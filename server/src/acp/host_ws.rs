use super::{
    apply_agent_probed_models, broadcast_acp, load_agent, load_host, load_session_meta,
    load_workspace, turn_from_row,
};
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
    let superseded = st.register_acp_host(host_id, uid, conn_id, sid, tx.clone());

    if !crate::auth::grant_valid(&st.pool, uid, grant.session_version, sid).await {
        st.unregister_acp_host(host_id, conn_id);
        let _ = socket.send(Message::Close(None)).await;
        return Ok(());
    }

    if superseded {
        st.fail_acp_host_rpcs(host_id);
    }
    // 新建立的宿主 WS 连接不继承旧连接的在途任务；若重连顶替了旧连接或曾非正常断连，在此立即收敛遗留运行态
    settle_host_running_sessions(&st, uid, host_id).await;

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
        st.fail_acp_host_rpcs(host_id);
        let _ = sqlx::query(
            "UPDATE acp_hosts SET online = false, latency_ms = 0, last_seen = now() WHERE id = $1",
        )
        .bind(host_id)
        .execute(&st.pool)
        .await;

        settle_host_running_sessions(&st, uid, host_id).await;

        if let Ok(h) = load_host(&st, uid, host_id).await {
            broadcast_acp(&st, uid, "host_upserted", json!({ "host": h }));
        }
    }

    Ok(())
}

/// 停止该宿主所有正在进行的会话、拒绝挂起的权限卡片并收敛流式与进行中工具块，防止断连/重连顶替后前端卡在运行态。
async fn settle_host_running_sessions(st: &SharedState, uid: Uuid, host_id: Uuid) {
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

    for sess_id in &paused_sessions {
        if let Ok(s) = load_session_meta(&st.pool, uid, *sess_id).await {
            broadcast_acp(st, uid, "session_upserted", json!({ "session": s }));
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
            st,
            uid,
            "turn_upserted",
            json!({ "session_id": tv.session_id, "turn": tv }),
        );
    }

    let settled_streams = sqlx::query(
        "UPDATE acp_turns t
            SET status = 'done', updated_at = now()
           FROM acp_sessions s
           JOIN acp_workspaces w ON w.id = s.acp_workspace_id
          WHERE t.acp_session_id = s.id AND w.acp_host_id = $1
            AND t.role IN ('agent_text', 'agent_thought') AND t.status = 'streaming'
          RETURNING t.id, t.acp_session_id, t.seq, t.role, t.status, t.payload, t.created_at, t.updated_at",
    )
    .bind(host_id)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    for r in &settled_streams {
        let tv = turn_from_row(r);
        broadcast_acp(
            st,
            uid,
            "turn_upserted",
            json!({ "session_id": tv.session_id, "turn": tv }),
        );
    }

    let failed_tools = sqlx::query(
        "UPDATE acp_turns t
            SET status = 'failed',
                payload = jsonb_set(t.payload, '{is_error}', 'true'::jsonb),
                updated_at = now()
           FROM acp_sessions s
           JOIN acp_workspaces w ON w.id = s.acp_workspace_id
          WHERE t.acp_session_id = s.id AND w.acp_host_id = $1
            AND t.role = 'tool_call' AND t.status IN ('pending', 'in_progress')
          RETURNING t.id, t.acp_session_id, t.seq, t.role, t.status, t.payload, t.created_at, t.updated_at",
    )
    .bind(host_id)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    for r in &failed_tools {
        let tv = turn_from_row(r);
        broadcast_acp(
            st,
            uid,
            "turn_upserted",
            json!({ "session_id": tv.session_id, "turn": tv }),
        );
    }

    for sess_id in paused_sessions {
        broadcast_acp(
            st,
            uid,
            "session_activity",
            json!({ "session_id": sess_id, "phase": "idle", "detail": "" }),
        );
    }
}

async fn handle_host_frame(st: &SharedState, uid: Uuid, host_id: Uuid, raw: &str) {
    if raw.len() > 8 * 1024 * 1024 {
        tracing::warn!(host_id = %host_id, bytes = raw.len(), "dropping oversized acp host frame");
        return;
    }
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
            if let Some((_, (_, tx))) = st
                .acp_rpc_wait
                .remove_if(&req_id, |_, (expected_host, _)| *expected_host == host_id)
            {
                if v["ok"].as_bool() == Some(true) {
                    let _ = tx.send(Ok(v["data"].clone()));
                } else {
                    let msg = v["error"]
                        .as_str()
                        .unwrap_or("rpc execution failed")
                        .chars()
                        .take(500)
                        .collect();
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
                Some("installing") => "installing",
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
        "agent_models" => {
            let Some(agent_id) = v["agent_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                return;
            };
            let session_id = v["session_id"].as_str().and_then(|s| Uuid::parse_str(s).ok());
            let probed_default = v["default_model"].as_str();
            let probed_options: Vec<String> =
                serde_json::from_value(v["model_options"].clone()).unwrap_or_default();
            let _ = apply_agent_probed_models(
                st,
                uid,
                agent_id,
                Some(host_id),
                session_id,
                probed_default,
                &probed_options,
            )
            .await;
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
            upsert_stream_turn(st, sess_id, uid, "agent_text", kind, v).await;
        }
        "thought_delta" | "thought" => {
            upsert_stream_turn(st, sess_id, uid, "agent_thought", kind, v).await;
        }
        "tool_call" => {
            handle_tool_call_update(st, sess_id, uid, v).await;
        }
        "activity" => {
            let phase = truncate_chars(v["phase"].as_str().unwrap_or("").trim(), 40);
            let detail = truncate_chars(v["detail"].as_str().unwrap_or("").trim(), 240);
            let usage = if v["usage"].is_object() {
                v["usage"].clone()
            } else {
                Value::Null
            };
            if !phase.is_empty() || !usage.is_null() {
                broadcast_acp(
                    st,
                    uid,
                    "session_activity",
                    json!({
                        "session_id": sess_id,
                        "phase": phase,
                        "detail": detail,
                        "usage": usage,
                    }),
                );
            }
        }
        "terminal_out" => {
            let cmd_str: String = v["command"].as_str().unwrap_or("").chars().take(1000).collect();
            let output_str: String = v["output"].as_str().unwrap_or("").chars().take(65536).collect();
            let payload = json!({
                "command": cmd_str,
                "output": output_str,
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

            // 将残留的流式正文与思考块收敛为 done
            let settled_streams = sqlx::query(
                "UPDATE acp_turns SET status = 'done', updated_at = now()
                  WHERE acp_session_id = $1
                    AND role IN ('agent_text', 'agent_thought')
                    AND status = 'streaming'
                  RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
            )
            .bind(sess_id)
            .fetch_all(&st.pool)
            .await
            .unwrap_or_default();
            for r in &settled_streams {
                let tv = turn_from_row(r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }

            let err_msg_opt = v["error"].as_str().map(str::trim).filter(|s| !s.is_empty());
            let settled_tools = if err_msg_opt.is_some() {
                sqlx::query(
                    "UPDATE acp_turns
                        SET status = 'failed',
                            payload = jsonb_set(payload, '{is_error}', 'true'::jsonb),
                            updated_at = now()
                      WHERE acp_session_id = $1
                        AND role = 'tool_call'
                        AND status IN ('pending', 'in_progress')
                      RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
                )
                .bind(sess_id)
                .fetch_all(&st.pool)
                .await
                .unwrap_or_default()
            } else {
                sqlx::query(
                    "UPDATE acp_turns
                        SET status = 'completed',
                            updated_at = now()
                      WHERE acp_session_id = $1
                        AND role = 'tool_call'
                        AND status IN ('pending', 'in_progress')
                      RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
                )
                .bind(sess_id)
                .fetch_all(&st.pool)
                .await
                .unwrap_or_default()
            };
            for r in &settled_tools {
                let tv = turn_from_row(r);
                broadcast_acp(
                    st,
                    uid,
                    "turn_upserted",
                    json!({ "session_id": sess_id, "turn": tv }),
                );
            }

            if let Some(err_msg) = err_msg_opt {
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
            broadcast_acp(
                st,
                uid,
                "session_activity",
                json!({ "session_id": sess_id, "phase": "idle", "detail": "" }),
            );
        }
        _ => {}
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn normalize_tool_status(raw: Option<&str>) -> Option<&'static str> {
    match raw.map(str::trim) {
        Some("pending") => Some("pending"),
        Some("in_progress") => Some("in_progress"),
        Some("completed") => Some("completed"),
        Some("failed") => Some("failed"),
        _ => None,
    }
}

pub(crate) fn merge_tool_call_payload(
    existing: Option<&Value>,
    incoming: &Value,
    tool_call_id: &str,
) -> Value {
    let name = incoming["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, 120))
        .unwrap_or_else(|| {
            existing
                .and_then(|e| e["name"].as_str())
                .unwrap_or("")
                .to_string()
        });

    let kind = incoming["tool_kind"]
        .as_str()
        .or_else(|| incoming["kind"].as_str().filter(|s| *s != "tool_call"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, 40))
        .unwrap_or_else(|| {
            existing
                .and_then(|e| e["kind"].as_str())
                .unwrap_or("other")
                .to_string()
        });

    let title = incoming["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, 300))
        .unwrap_or_else(|| {
            existing
                .and_then(|e| e["title"].as_str())
                .unwrap_or("")
                .to_string()
        });

    let input = match &incoming["input"] {
        Value::String(s) if !s.is_empty() => truncate_chars(s, 16_384),
        Value::Object(_) | Value::Array(_) => {
            let s = serde_json::to_string_pretty(&incoming["input"]).unwrap_or_default();
            if s.is_empty() {
                existing
                    .and_then(|e| e["input"].as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                truncate_chars(&s, 16_384)
            }
        }
        _ => existing
            .and_then(|e| e["input"].as_str())
            .unwrap_or("")
            .to_string(),
    };

    let prev_output = existing.and_then(|e| e["output"].as_str()).unwrap_or("");
    let output = if let Some(s) = incoming["output"].as_str().filter(|s| !s.is_empty()) {
        truncate_chars(s, 65_536)
    } else if let Some(delta) = incoming["output_delta"].as_str().filter(|d| !d.is_empty()) {
        let mut combined = String::with_capacity(
            prev_output
                .len()
                .saturating_add(delta.len())
                .min(65_536 * 4),
        );
        combined.push_str(prev_output);
        combined.push_str(delta);
        truncate_chars(&combined, 65_536)
    } else {
        prev_output.to_string()
    };

    let locations: Vec<String> = incoming["locations"]
        .as_array()
        .filter(|arr| !arr.is_empty())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty()))
                .take(20)
                .map(|s| truncate_chars(s, 300))
                .collect()
        })
        .unwrap_or_else(|| {
            existing
                .and_then(|e| serde_json::from_value::<Vec<String>>(e["locations"].clone()).ok())
                .unwrap_or_default()
        });

    let diffs: Vec<Value> = incoming["diffs"]
        .as_array()
        .filter(|arr| !arr.is_empty())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    let path = d["path"].as_str()?.trim();
                    if path.is_empty() {
                        return None;
                    }
                    let old_text = d["old_text"].as_str().map(|s| truncate_chars(s, 32_768));
                    let new_text = truncate_chars(d["new_text"].as_str().unwrap_or(""), 32_768);
                    Some(json!({
                        "path": truncate_chars(path, 300),
                        "old_text": old_text,
                        "new_text": new_text,
                    }))
                })
                .take(10)
                .collect()
        })
        .unwrap_or_else(|| {
            existing
                .and_then(|e| e["diffs"].as_array().cloned())
                .unwrap_or_default()
        });

    let duration_ms = incoming["duration_ms"]
        .as_u64()
        .or_else(|| existing.and_then(|e| e["duration_ms"].as_u64()));

    let status_norm = normalize_tool_status(incoming["status"].as_str());
    let is_error = incoming["is_error"].as_bool().unwrap_or_else(|| {
        if status_norm == Some("failed") {
            true
        } else if status_norm == Some("completed") {
            false
        } else {
            existing
                .and_then(|e| e["is_error"].as_bool())
                .unwrap_or(false)
        }
    });

    let mut out = json!({
        "tool_call_id": tool_call_id,
        "name": name,
        "kind": kind,
        "title": title,
        "input": input,
        "output": output,
        "locations": locations,
        "diffs": diffs,
        "is_error": is_error,
    });
    if let Some(ms) = duration_ms {
        out["duration_ms"] = json!(ms);
    }
    out
}

async fn upsert_stream_turn(
    st: &SharedState,
    sess_id: Uuid,
    uid: Uuid,
    role: &str,
    kind: &str,
    v: &Value,
) {
    let turn_key = truncate_chars(v["turn_key"].as_str().unwrap_or(""), 120);
    let delta = truncate_chars(v["delta"].as_str().unwrap_or(""), 65_536);
    let full_text: Option<String> = v["text"].as_str().map(|s| truncate_chars(s, 262_144));
    let done = v["done"]
        .as_bool()
        .unwrap_or(kind == "text" || kind == "thought");
    let duration_ms: Option<i64> = v["duration_ms"]
        .as_u64()
        .and_then(|n| i64::try_from(n).ok());
    let status = if done { "done" } else { "streaming" };

    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(session_id = %sess_id, role, error = %e, "stream_turn begin tx failed");
            return;
        }
    };
    let locked = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM acp_sessions WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(sess_id)
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    if locked.is_none() {
        return;
    }

    let updated_existing = if !turn_key.is_empty() {
        sqlx::query(
            "UPDATE acp_turns
                SET payload = jsonb_strip_nulls(jsonb_build_object(
                        'turn_key', $4::text,
                        'text', CASE WHEN $5::text IS NOT NULL
                                     THEN left($5, 262144)
                                     ELSE left(COALESCE(payload->>'text', '') || $6::text, 262144)
                                END,
                        'duration_ms', COALESCE(to_jsonb($7::bigint), payload->'duration_ms')
                    )),
                    status = $8,
                    updated_at = now()
              WHERE id = (
                  SELECT id FROM acp_turns
                   WHERE acp_session_id = $1 AND role = $2
                     AND payload->>'turn_key' = $3
                   LIMIT 1
              )
              RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
        )
        .bind(sess_id)
        .bind(role)
        .bind(&turn_key)
        .bind(&turn_key)
        .bind(full_text.as_deref())
        .bind(&delta)
        .bind(duration_ms)
        .bind(status)
        .fetch_optional(&mut *tx)
        .await
        .unwrap_or(None)
    } else {
        None
    };

    let row = match updated_existing {
        Some(r) => Some(r),
        None => {
            let initial = full_text.unwrap_or(delta);
            let mut initial_payload = json!({ "turn_key": turn_key, "text": initial });
            if let Some(ms) = duration_ms {
                initial_payload["duration_ms"] = json!(ms);
            }
            match insert_turn_tx(&mut *tx, sess_id, uid, role, status, &initial_payload).await {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(session_id = %sess_id, role, error = %e, "insert stream turn failed");
                    None
                }
            }
        }
    };

    if row.is_some() && tx.commit().await.is_ok() {
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
}

async fn handle_tool_call_update(st: &SharedState, sess_id: Uuid, uid: Uuid, v: &Value) {
    let tool_call_id = v["tool_call_id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, 128))
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(session_id = %sess_id, error = %e, "tool_call begin tx failed");
            return;
        }
    };
    let locked = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM acp_sessions WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(sess_id)
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    if locked.is_none() {
        return;
    }

    let existing = sqlx::query_as::<_, (Uuid, String, Value)>(
        "SELECT id, status, payload FROM acp_turns
          WHERE acp_session_id = $1 AND role = 'tool_call'
            AND payload->>'tool_call_id' = $2
          LIMIT 1",
    )
    .bind(sess_id)
    .bind(&tool_call_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();

    let next_status = normalize_tool_status(v["status"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|(_, st_s, _)| st_s.clone())
                .unwrap_or_else(|| "in_progress".to_string())
        });

    let merged_payload = merge_tool_call_payload(
        existing.as_ref().map(|(_, _, p)| p),
        v,
        &tool_call_id,
    );

    let row = match existing {
        Some((turn_id, _, _)) => sqlx::query(
            "UPDATE acp_turns
                SET status = $2, payload = $3, updated_at = now()
              WHERE id = $1
              RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
        )
        .bind(turn_id)
        .bind(&next_status)
        .bind(&merged_payload)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten(),
        None => {
            match insert_turn_tx(
                &mut *tx,
                sess_id,
                uid,
                "tool_call",
                &next_status,
                &merged_payload,
            )
            .await
            {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(session_id = %sess_id, error = %e, "insert tool_call turn failed");
                    None
                }
            }
        }
    };

    if row.is_some() && tx.commit().await.is_ok() {
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
}

pub(crate) async fn insert_turn_tx<'e, E>(
    exec: E,
    sess_id: Uuid,
    uid: Uuid,
    role: &str,
    status: &str,
    payload: &Value,
) -> Result<sqlx::postgres::PgRow, sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
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
    .bind(payload)
    .fetch_one(exec)
    .await
}

pub(crate) async fn insert_turn(
    st: &SharedState,
    sess_id: Uuid,
    uid: Uuid,
    role: &str,
    status: &str,
    payload: Value,
) -> Option<sqlx::postgres::PgRow> {
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(session_id = %sess_id, error = %e, "insert_turn begin tx failed");
            return None;
        }
    };
    let locked = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM acp_sessions WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(sess_id)
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await;
    if !matches!(locked, Ok(Some(_))) {
        return None;
    }
    match insert_turn_tx(&mut *tx, sess_id, uid, role, status, &payload).await {
        Ok(row) => {
            if let Err(e) = tx.commit().await {
                tracing::warn!(session_id = %sess_id, error = %e, "insert_turn commit failed");
                return None;
            }
            Some(row)
        }
        Err(e) => {
            tracing::warn!(session_id = %sess_id, error = %e, "insert_turn failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_tool_call_payload;
    use serde_json::json;

    #[test]
    fn merge_tool_call_payload_supports_sparse_patch_delta_and_diffs() {
        let initial = merge_tool_call_payload(
            None,
            &json!({
                "kind": "tool_call",
                "tool_call_id": "call-1",
                "name": "edit",
                "tool_kind": "edit",
                "title": "修改 main.rs",
                "input": "{\"path\":\"src/main.rs\"}",
                "locations": ["src/main.rs:12"],
                "diffs": [{ "path": "src/main.rs", "old_text": "old", "new_text": "new" }],
                "status": "in_progress"
            }),
            "call-1",
        );
        assert_eq!(initial["tool_call_id"], "call-1");
        assert_eq!(initial["kind"], "edit");
        assert_eq!(initial["title"], "修改 main.rs");
        assert_eq!(initial["locations"], json!(["src/main.rs:12"]));
        assert_eq!(initial["diffs"][0]["new_text"], "new");
        assert_eq!(initial["is_error"], false);

        // 增量追加输出且保留既有 name / title / diffs
        let chunk1 = merge_tool_call_payload(
            Some(&initial),
            &json!({
                "kind": "tool_call",
                "tool_call_id": "call-1",
                "output_delta": "line 1\n"
            }),
            "call-1",
        );
        assert_eq!(chunk1["name"], "edit");
        assert_eq!(chunk1["kind"], "edit");
        assert_eq!(chunk1["output"], "line 1\n");
        assert_eq!(chunk1["diffs"][0]["path"], "src/main.rs");

        let chunk2 = merge_tool_call_payload(
            Some(&chunk1),
            &json!({
                "output_delta": "line 2\n",
                "duration_ms": 420,
                "status": "completed"
            }),
            "call-1",
        );
        assert_eq!(chunk2["output"], "line 1\nline 2\n");
        assert_eq!(chunk2["duration_ms"], 420);
        assert_eq!(chunk2["is_error"], false);

        // 全量 output 覆盖旧值且 failed 自动置 is_error=true
        let failed = merge_tool_call_payload(
            Some(&chunk2),
            &json!({
                "output": "final error output",
                "status": "failed"
            }),
            "call-1",
        );
        assert_eq!(failed["output"], "final error output");
        assert_eq!(failed["is_error"], true);

        // 超长输出截断到 65536 字符
        let huge = "x".repeat(70_000);
        let capped = merge_tool_call_payload(None, &json!({ "output": huge }), "call-2");
        assert_eq!(capped["output"].as_str().unwrap().chars().count(), 65_536);
    }
}
