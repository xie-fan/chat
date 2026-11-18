use super::{broadcast_acp, load_agent, load_host, load_session_meta, load_workspace, turn_from_row};
use crate::error::AppResult;
use crate::state::{ConnMsg, SharedState};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

pub async fn host_session(
    mut socket: WebSocket,
    st: SharedState,
    grant: crate::auth::AccessGrant,
) -> AppResult<()> {
    let uid = grant.uid;
    let sid = grant.sid;

    // 10s 内必须收到合法 host_hello，否则关闭连接
    let hello = match tokio::time::timeout(Duration::from_secs(10), socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|v| v["type"] == "host_hello") else {
        let _ = socket.send(Message::Close(None)).await;
        return Ok(());
    };
    let Some(install_id) = hello["install_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        let _ = socket.send(Message::Close(None)).await;
        return Ok(());
    };

    let raw_name: String = hello["name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect();
    let name = if raw_name.is_empty() {
        "Desktop".to_string()
    } else {
        raw_name
    };
    let os: String = hello["os"].as_str().unwrap_or("").chars().take(120).collect();
    let ip: String = hello["ip"].as_str().unwrap_or("").chars().take(120).collect();
    let roots = if hello["roots"].is_array() {
        hello["roots"].clone()
    } else {
        json!([])
    };

    let host_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO acp_hosts (user_id, install_id, name, os, ip, roots_json, online, latency_ms, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, true, 0, now())
         ON CONFLICT (user_id, install_id)
         DO UPDATE SET
             os = EXCLUDED.os,
             ip = EXCLUDED.ip,
             roots_json = EXCLUDED.roots_json,
             online = true,
             last_seen = now()
         RETURNING id",
    )
    .bind(uid)
    .bind(install_id)
    .bind(&name)
    .bind(&os)
    .bind(&ip)
    .bind(&roots)
    .fetch_one(&st.pool)
    .await;
    let Ok(host_id) = host_id else {
        let _ = socket.send(Message::Close(None)).await;
        return Ok(());
    };

    let conn_id = Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel::<ConnMsg>(crate::state::CONN_QUEUE);
    st.register_acp_host(host_id, uid, conn_id, sid, tx.clone());

    if !crate::auth::grant_valid(&st.pool, uid, grant.session_version, sid).await {
        st.unregister_acp_host(host_id, conn_id);
        let _ = socket.send(Message::Close(None)).await;
        return Ok(());
    }

    let _ = tx.try_send(ConnMsg::Frame(
        json!({ "type": "host_ready", "host_id": host_id }).to_string(),
    ));
    if let Ok(h) = load_host(&st, uid, host_id).await {
        broadcast_acp(&st, uid, "host_upserted", json!({ "host": h }));
    }

    let (mut sink, mut stream) = socket.split();
    let ping_every = Duration::from_millis(st.ws_ping_interval_ms.load(Ordering::Relaxed).max(10));
    let idle_timeout = Duration::from_millis(st.ws_idle_timeout_ms.load(Ordering::Relaxed).max(1));
    let mut ping = tokio::time::interval(ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_inbound = Instant::now();
    let mut last_ping_sent: Option<Instant> = None;
    let mut last_latency_reported: Option<i32> = None;

    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                match msg {
                    Some(ConnMsg::Frame(f)) => {
                        if sink.send(Message::Text(f.into())).await.is_err() {
                            break;
                        }
                    }
                    Some(ConnMsg::Shutdown(f)) => {
                        let _ = sink.send(Message::Text(f.into())).await;
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                    None => break,
                }
            }
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(txt))) => {
                        last_inbound = Instant::now();
                        handle_host_frame(&st, uid, host_id, &txt).await;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_inbound = Instant::now();
                        if let Some(sent) = last_ping_sent.take() {
                            let ms = sent.elapsed().as_millis().min(9999) as i32;
                            let changed = last_latency_reported
                                .map(|prev| (prev - ms).abs() >= 5)
                                .unwrap_or(true);
                            let _ = sqlx::query(
                                "UPDATE acp_hosts SET latency_ms = $2, last_seen = now() WHERE id = $1",
                            )
                            .bind(host_id)
                            .bind(ms)
                            .execute(&st.pool)
                            .await;
                            if changed {
                                last_latency_reported = Some(ms);
                                if let Ok(h) = load_host(&st, uid, host_id).await {
                                    broadcast_acp(&st, uid, "host_upserted", json!({ "host": h }));
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(d))) => {
                        last_inbound = Instant::now();
                        if sink.send(Message::Pong(d)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {
                        last_inbound = Instant::now();
                    }
                }
            }
            _ = ping.tick() => {
                if last_inbound.elapsed() > idle_timeout {
                    break;
                }
                if !crate::auth::grant_valid(&st.pool, uid, grant.session_version, sid).await {
                    break;
                }
                last_ping_sent = Some(Instant::now());
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    if st.unregister_acp_host(host_id, conn_id) {
        let _ = sqlx::query(
            "UPDATE acp_hosts SET online = false, latency_ms = 0, last_seen = now() WHERE id = $1",
        )
        .bind(host_id)
        .execute(&st.pool)
        .await;

        // 停止该宿主所有正在进行的会话并拒绝挂起的权限卡片，防止前端卡在运行态
        let paused_sessions = sqlx::query_scalar::<_, Uuid>(
            "UPDATE acp_sessions s SET running = false, updated_at = now()
              FROM acp_workspaces w
             WHERE s.acp_workspace_id = w.id AND w.acp_host_id = $1 AND s.running = true
             RETURNING s.id",
        )
        .bind(host_id)
        .fetch_all(&st.pool)
        .await
        .unwrap_or_default();

        for sess_id in paused_sessions {
            if let Ok(s) = load_session_meta(&st.pool, uid, sess_id).await {
                broadcast_acp(&st, uid, "session_upserted", json!({ "session": s }));
            }
        }

        let aborted_turns = sqlx::query(
            "UPDATE acp_turns t
                SET status = 'rejected',
                    payload = jsonb_set(t.payload, '{chosen_option}', '\"host_disconnected\"'::jsonb),
                    updated_at = now()
               FROM acp_sessions s
               JOIN acp_workspaces w ON w.id = s.acp_workspace_id
              WHERE t.acp_session_id = s.id AND w.acp_host_id = $1
                AND t.role = 'permission' AND t.status = 'pending'
              RETURNING t.id, t.acp_session_id, t.seq, t.role, t.status, t.payload, t.created_at, t.updated_at",
        )
        .bind(host_id)
        .fetch_all(&st.pool)
        .await
        .unwrap_or_default();

        for r in &aborted_turns {
            let tv = turn_from_row(r);
            broadcast_acp(
                &st,
                uid,
                "turn_upserted",
                json!({ "session_id": tv.session_id, "turn": tv }),
            );
        }

        if let Ok(h) = load_host(&st, uid, host_id).await {
            broadcast_acp(&st, uid, "host_upserted", json!({ "host": h }));
        }
    }

    Ok(())
}

async fn handle_host_frame(st: &SharedState, uid: Uuid, host_id: Uuid, raw: &str) {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let Some(kind) = v["type"].as_str() else {
        return;
    };

    match kind {
        "rpc_result" => {
            let Some(req_id) = v["req_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                return;
            };
            if let Some((_, tx)) = st.acp_rpc_wait.remove(&req_id) {
                if v["ok"].as_bool() == Some(true) {
                    let _ = tx.send(Ok(v["data"].clone()));
                } else {
                    let msg = v["error"]
                        .as_str()
                        .unwrap_or("rpc execution failed")
                        .to_string();
                    let _ = tx.send(Err(msg));
                }
            }
        }
        "agent_status" => {
            let Some(agent_id) = v["agent_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                return;
            };
            let status = match v["status"].as_str() {
                Some("online") => "online",
                Some("offline") => "offline",
                _ => "standby",
            };
            let updated = sqlx::query(
                "UPDATE acp_agents SET status = $3, updated_at = now()
                  WHERE id = $1 AND acp_host_id = $2 AND user_id = $4",
            )
            .bind(agent_id)
            .bind(host_id)
            .bind(status)
            .bind(uid)
            .execute(&st.pool)
            .await;
            if updated.is_ok() {
                if let Ok(ag) = load_agent(&st.pool, uid, agent_id).await {
                    broadcast_acp(&st, uid, "agent_upserted", json!({ "agent": ag }));
                }
            }
        }
        "session_update" => {
            handle_session_update(st, uid, host_id, &v).await;
        }
        _ => {}
    }
}

async fn handle_session_update(st: &SharedState, uid: Uuid, host_id: Uuid, v: &Value) {
    let Some(sess_id) = v["session_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
        return;
    };
    let Some(ws_id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT s.acp_workspace_id
           FROM acp_sessions s
           JOIN acp_workspaces w ON w.id = s.acp_workspace_id
          WHERE s.id = $1 AND s.user_id = $2 AND w.acp_host_id = $3",
    )
    .bind(sess_id)
    .bind(uid)
    .bind(host_id)
    .fetch_optional(&st.pool)
    .await
    .ok()
    .flatten() else {
        return;
    };

    let Some(kind) = v["kind"].as_str() else {
        return;
    };

    match kind {
        "plan" => {
            let run_key = v["run_key"].as_str().unwrap_or("");
            let steps = v["steps"].clone();
            let payload = json!({ "run_key": run_key, "steps": steps });
            let row = if !run_key.is_empty() {
                sqlx::query(
                    "UPDATE acp_turns SET payload = $3, status = 'done', updated_at = now()
                      WHERE acp_session_id = $1 AND role = 'agent_plan'
                        AND payload->>'run_key' = $2
                      RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
                )
                .bind(sess_id)
                .bind(run_key)
                .bind(&payload)
                .fetch_optional(&st.pool)
                .await
                .ok()
                .flatten()
            } else {
                None
            };
            let row = match row {
                Some(r) => Some(r),
                None => insert_turn(st, sess_id, uid, "agent_plan", "done", payload).await,
            };
            if let Some(r) = row {
                let tv = turn_from_row(&r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }
        }
        "text_delta" | "text" => {
            let turn_key = v["turn_key"].as_str().unwrap_or("");
            let delta = v["delta"].as_str().unwrap_or("");
            let full_text = v["text"].as_str();
            let done = v["done"].as_bool().unwrap_or(kind == "text");
            let status = if done { "done" } else { "streaming" };

            let existing = if !turn_key.is_empty() {
                sqlx::query(
                    "SELECT id, payload FROM acp_turns
                      WHERE acp_session_id = $1 AND role = 'agent_text'
                        AND payload->>'turn_key' = $2
                      LIMIT 1",
                )
                .bind(sess_id)
                .bind(turn_key)
                .fetch_optional(&st.pool)
                .await
                .ok()
                .flatten()
            } else {
                None
            };

            let row = if let Some(ex) = existing {
                use sqlx::Row;
                let tid: Uuid = ex.get("id");
                let prev: Value = ex.get("payload");
                let next_text = if let Some(ft) = full_text {
                    ft.to_string()
                } else {
                    format!("{}{}", prev["text"].as_str().unwrap_or(""), delta)
                };
                sqlx::query(
                    "UPDATE acp_turns SET payload = $2, status = $3, updated_at = now()
                      WHERE id = $1
                      RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
                )
                .bind(tid)
                .bind(json!({ "turn_key": turn_key, "text": next_text }))
                .bind(status)
                .fetch_optional(&st.pool)
                .await
                .ok()
                .flatten()
            } else {
                let txt = full_text.unwrap_or(delta);
                insert_turn(
                    st,
                    sess_id,
                    uid,
                    "agent_text",
                    status,
                    json!({ "turn_key": turn_key, "text": txt }),
                )
                .await
            };

            if let Some(r) = row {
                let tv = turn_from_row(&r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }
        }
        "terminal_out" => {
            let payload = json!({
                "command": v["command"].as_str().unwrap_or(""),
                "output": v["output"].as_str().unwrap_or(""),
                "exit_code": v["exit_code"].as_i64().unwrap_or(0),
            });
            if let Some(r) = insert_turn(st, sess_id, uid, "terminal_out", "done", payload).await {
                let tv = turn_from_row(&r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }
        }
        "permission_request" => {
            let payload = json!({
                "req_id": v["req_id"].as_str().unwrap_or(""),
                "tool_kind": v["tool_kind"].as_str().unwrap_or("tool"),
                "command": v["command"].as_str().unwrap_or(""),
                "cwd": v["cwd"].as_str().unwrap_or(""),
                "reason": v["reason"].as_str().unwrap_or("请求执行需要确认的工具操作"),
                "chosen_option": "",
            });
            if let Some(r) = insert_turn(st, sess_id, uid, "permission", "pending", payload).await {
                let tv = turn_from_row(&r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }
        }
        "diffs" => {
            let diffs = v["diffs"].clone();
            let _ = sqlx::query(
                "UPDATE acp_sessions SET diffs_json = $2, updated_at = now() WHERE id = $1",
            )
            .bind(sess_id)
            .bind(&diffs)
            .execute(&st.pool)
            .await;
            if let Ok(s) = load_session_meta(&st.pool, uid, sess_id).await {
                broadcast_acp(st, uid, "session_upserted", json!({ "session": s }));
            }
        }
        "rpc_log" => {
            if let Ok(mut current) = sqlx::query_scalar::<_, Value>(
                "SELECT rpc_logs_json FROM acp_sessions WHERE id = $1",
            )
            .bind(sess_id)
            .fetch_one(&st.pool)
            .await
            {
                let arr = current.as_array_mut();
                if let Some(list) = arr {
                    list.push(v["entry"].clone());
                    if list.len() > 50 {
                        let drop_n = list.len() - 50;
                        list.drain(0..drop_n);
                    }
                    let _ = sqlx::query(
                        "UPDATE acp_sessions SET rpc_logs_json = $2, updated_at = now() WHERE id = $1",
                    )
                    .bind(sess_id)
                    .bind(&current)
                    .execute(&st.pool)
                    .await;
                }
                if let Ok(s) = load_session_meta(&st.pool, uid, sess_id).await {
                    broadcast_acp(st, uid, "session_upserted", json!({ "session": s }));
                }
            }
        }
        "prompt_done" => {
            let acp_session_key = v["acp_session_key"].as_str().map(str::to_string);
            let _ = sqlx::query(
                "UPDATE acp_sessions
                    SET running = false,
                        acp_session_key = COALESCE($2, acp_session_key),
                        updated_at = now()
                  WHERE id = $1",
            )
            .bind(sess_id)
            .bind(&acp_session_key)
            .execute(&st.pool)
            .await;

            // 标记未完成计划项 & 流式消息收敛为 done
            let _ = sqlx::query(
                "UPDATE acp_turns SET status = 'done', updated_at = now()
                  WHERE acp_session_id = $1 AND role = 'agent_text' AND status = 'streaming'",
            )
            .bind(sess_id)
            .execute(&st.pool)
            .await;

            if let Some(err_msg) = v["error"].as_str().filter(|s| !s.is_empty()) {
                if let Some(r) = insert_turn(
                    st,
                    sess_id,
                    uid,
                    "agent_text",
                    "error",
                    json!({ "text": format!("执行中断：{err_msg}") }),
                )
                .await
                {
                    let tv = turn_from_row(&r);
                    broadcast_acp(
                        st,
                        uid,
                        "turn_upserted",
                        json!({ "session_id": sess_id, "turn": tv }),
                    );
                }
            }

            if v["branch"].is_string() || v["branches"].is_array() {
                let branch = v["branch"].as_str().unwrap_or("");
                let branches = if v["branches"].is_array() {
                    v["branches"].clone()
                } else {
                    json!([])
                };
                let _ = sqlx::query(
                    "UPDATE acp_workspaces
                        SET branch = CASE WHEN $2 <> '' THEN $2 ELSE branch END,
                            branches = CASE WHEN jsonb_array_length($3) > 0 THEN $3 ELSE branches END,
                            updated_at = now()
                      WHERE id = $1",
                )
                .bind(ws_id)
                .bind(branch)
                .bind(&branches)
                .execute(&st.pool)
                .await;
                if let Ok(ws) = load_workspace(&st.pool, uid, ws_id).await {
                    broadcast_acp(st, uid, "workspace_upserted", json!({ "workspace": ws }));
                }
            }

            if let Ok(s) = load_session_meta(&st.pool, uid, sess_id).await {
                broadcast_acp(st, uid, "session_upserted", json!({ "session": s }));
            }
        }
        _ => {}
    }
}

pub(crate) async fn insert_turn(
    st: &SharedState,
    sess_id: Uuid,
    uid: Uuid,
    role: &str,
    status: &str,
    payload: Value,
) -> Option<sqlx::postgres::PgRow> {
    sqlx::query(
        "INSERT INTO acp_turns (acp_session_id, user_id, seq, role, status, payload)
         VALUES (
             $1,
             $2,
             (SELECT COALESCE(MAX(seq), 0) + 1 FROM acp_turns WHERE acp_session_id = $1),
             $3,
             $4,
             $5
         )
         RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
    )
    .bind(sess_id)
    .bind(uid)
    .bind(role)
    .bind(status)
    .bind(&payload)
    .fetch_optional(&st.pool)
    .await
    .ok()
    .flatten()
}
