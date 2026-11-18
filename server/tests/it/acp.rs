use super::*;
use reqwest::multipart::{Form, Part};
use uuid::Uuid;

async fn host_ws_connect(
    base: &str,
    u: &TestUser,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("{}&role=host", ws_url(base, u).await);
    let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws
}

async fn next_acp_event<S>(s: &mut S, want_event: &str) -> Value
where
    S: StreamExt<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    for _ in 0..20 {
        let m = s.next().await.unwrap().unwrap();
        if let WsMsg::Text(t) = m {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "acp_event" && v["event"] == want_event {
                return v;
            }
        }
    }
    panic!("did not receive acp_event={want_event}");
}

#[tokio::test]
async fn acp_host_lifecycle_rpc_prompt_and_permissions() {
    let base = spawn().await;
    let u = register(&base, &format!("acpu_{}", uniq())).await;
    let other = register(&base, &format!("acpo_{}", uniq())).await;

    // 1. 建立客户端 WS 与模拟宿主 WS
    let (mut client_ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();
    let mut host_ws = host_ws_connect(&base, &u).await;

    let install_id = Uuid::new_v4();
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": install_id,
                "name": "DevBox",
                "os": "Linux 6.8",
                "ip": "127.0.0.1",
                "roots": ["/home/dev/go"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let ready = next_of(&mut host_ws, "host_ready").await;
    let host_id = ready["host_id"].as_str().unwrap().to_string();

    let ev = next_acp_event(&mut client_ws, "host_upserted").await;
    assert_eq!(ev["data"]["host"]["id"], host_id);
    assert_eq!(ev["data"]["host"]["online"], true);

    // 2. 其他用户不能访问别人的宿主/目录
    let forbidden = http()
        .get(format!("{base}/v1/acp/hosts/{host_id}/dirs"))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 404);

    // 3. 远程目录浏览 RPC (GET /v1/acp/hosts/{id}/dirs)
    let dirs_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .get(format!("{base}/v1/acp/hosts/{host_id}/dirs?path=/home/dev"))
                .header("authorization", authz(&access))
                .send()
                .await
                .unwrap()
        }
    });

    let rpc_call = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(rpc_call["method"], "list_dir");
    assert_eq!(rpc_call["params"]["path"], "/home/dev");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": rpc_call["req_id"],
                "ok": true,
                "data": {
                    "path": "/home/dev",
                    "parent": "/home",
                    "roots": ["/", "/home/dev"],
                    "entries": [
                        { "name": "chat", "path": "/home/dev/chat", "has_children": true }
                    ]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let dirs_res = dirs_req.await.unwrap();
    assert_eq!(dirs_res.status(), 200);
    let dirs_json: Value = dirs_res.json().await.unwrap();
    assert_eq!(dirs_json["entries"][0]["name"], "chat");

    // 4. 配置全局权限策略 + 在该宿主接入 Claude Agent + 修改默认模型/权限覆盖
    let s_res = http()
        .put(format!("{base}/v1/acp/settings"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "default_permission": "ask" }))
        .send()
        .await
        .unwrap();
    assert_eq!(s_res.status(), 200);

    let ag_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/agents"))
                .header("authorization", authz(&access))
                .json(&json!({
                    "host_id": host_id,
                    "preset": "claude",
                    "name": "Claude Code",
                    "cmd": "npx @zed-industries/claude-agent-acp"
                }))
                .send()
                .await
                .unwrap()
        }
    });
    let probe_ag = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(probe_ag["method"], "probe_agent");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": probe_ag["req_id"],
                "ok": true,
                "data": { "ok": true }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ag_res: Value = ag_req.await.unwrap().json().await.unwrap();
    let agent_id = ag_res["id"].as_str().unwrap().to_string();
    assert_eq!(ag_res["status"], "online");
    assert_eq!(ag_res["short_tag"], "CC");

    // 自定义模型写入 model_options 与 default_model
    let ag_upd: Value = http()
        .patch(format!("{base}/v1/acp/agents/{agent_id}"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "default_model": "claude-sonnet-4-5-custom" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ag_upd["default_model"], "claude-sonnet-4-5-custom");
    assert!(ag_upd["model_options"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "claude-sonnet-4-5-custom"));

    // 5. 创建工作目录（触发 probe_workspace RPC）+ Git checkout RPC
    let ws_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/workspaces"))
                .header("authorization", authz(&access))
                .json(&json!({
                    "host_id": host_id,
                    "cwd": "/home/dev/chat"
                }))
                .send()
                .await
                .unwrap()
        }
    });
    let probe_ws = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(probe_ws["method"], "probe_workspace");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": probe_ws["req_id"],
                "ok": true,
                "data": {
                    "cwd": "/home/dev/chat",
                    "branch": "main",
                    "branches": ["main", "feat/ui"]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ws_json: Value = ws_req.await.unwrap().json().await.unwrap();
    let ws_id = ws_json["id"].as_str().unwrap().to_string();
    assert_eq!(ws_json["name"], "chat");
    assert_eq!(ws_json["branch"], "main");

    let co_req = tokio::spawn({
        let base = base.clone();
        let ws_id = ws_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/workspaces/{ws_id}/git-checkout"))
                .header("authorization", authz(&access))
                .json(&json!({ "branch": "feat/ui" }))
                .send()
                .await
                .unwrap()
        }
    });
    let rpc_co = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(rpc_co["method"], "git_checkout");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": rpc_co["req_id"],
                "ok": true,
                "data": { "branch": "feat/ui", "branches": ["main", "feat/ui"] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let co_json: Value = co_req.await.unwrap().json().await.unwrap();
    assert_eq!(co_json["branch"], "feat/ui");

    // 6. 新建会话：权限继承用户全局 'ask'，选中模型继承 Agent default_model
    let sess: Value = http()
        .post(format!("{base}/v1/acp/sessions"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "workspace_id": ws_id,
            "agent_id": agent_id,
            "title": "修 bug"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sess_id = sess["id"].as_str().unwrap().to_string();
    assert_eq!(sess["permission_policy"], "ask");
    assert_eq!(sess["selected_model"], "claude-sonnet-4-5-custom");

    // 7. 上传 ACP 附件并下发 prompt
    let part = Part::bytes(b"hello attachment".to_vec())
        .file_name("notes.txt")
        .mime_str("text/plain")
        .unwrap();
    let form = Form::new().text("session_id", sess_id.clone()).part("file", part);
    let att_res = http()
        .post(format!("{base}/v1/acp/files"))
        .header("authorization", authz(&u.access))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(att_res.status(), 200);
    let att_json: Value = att_res.json().await.unwrap();
    let att_id = att_json["id"].as_str().unwrap().to_string();

    // 他人无法下载该私有 ACP 文件，本人可下载
    let dl_other = http()
        .get(format!("{base}/v1/acp/files/{att_id}"))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl_other.status(), 404);
    let dl_self = http()
        .get(format!("{base}/v1/acp/files/{att_id}"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl_self.status(), 200);
    assert_eq!(dl_self.text().await.unwrap(), "hello attachment");

    let prompt_res = http()
        .post(format!("{base}/v1/acp/sessions/{sess_id}/prompts"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "text": "跑一下单测",
            "files": ["src/main.rs"],
            "attachment_ids": [att_id],
            "thinking_level": "高"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(prompt_res.status(), 200);

    let start = next_of(&mut host_ws, "prompt_start").await;
    assert_eq!(start["session_id"], sess_id);
    assert_eq!(start["thinking_level"], "高");
    assert_eq!(start["attachments"][0]["file_name"], "notes.txt");
    assert!(start["attachments"][0]["data_base64"].is_string());

    // 8. 宿主推 plan + permission_request + 终端输出 + 文本流 + prompt_done
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "permission_request",
                "req_id": "perm-1",
                "tool_kind": "terminal/run",
                "command": "cargo test",
                "cwd": "/home/dev/chat",
                "reason": "运行 cargo test"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let perm_ev = loop {
        let ev = next_acp_event(&mut client_ws, "turn_upserted").await;
        if ev["data"]["turn"]["role"] == "permission" {
            break ev;
        }
    };
    let perm_turn_id = perm_ev["data"]["turn"]["id"].as_str().unwrap().to_string();

    let perm_reply_res = http()
        .post(format!("{base}/v1/acp/permissions/{perm_turn_id}"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "option_id": "allow_once" }))
        .send()
        .await
        .unwrap();
    assert_eq!(perm_reply_res.status(), 200);
    let reply_frame = next_of(&mut host_ws, "permission_reply").await;
    assert_eq!(reply_frame["req_id"], "perm-1");
    assert_eq!(reply_frame["option_id"], "allow_once");

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "text_delta",
                "turn_key": "t-ans",
                "delta": "已完成：",
                "done": false
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "text_delta",
                "turn_key": "t-ans",
                "delta": "全部通过。",
                "done": true
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "prompt_done",
                "acp_session_key": "acp-k-1"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    // 等 prompt_done 收敛到数据库后拉 bootstrap 校验持久化与存储配额统计
    let mut done_ok = false;
    for _ in 0..10 {
        let bs: Value = http()
            .get(format!("{base}/v1/acp/bootstrap"))
            .header("authorization", authz(&u.access))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let s = &bs["sessions"][0];
        if s["running"] == false && s["acp_session_key"] == "acp-k-1" {
            let turns = s["turns"].as_array().unwrap();
            assert!(turns.iter().any(|t| {
                t["role"] == "agent_text" && t["payload"]["text"] == "已完成：全部通过。"
            }));
            assert!(turns.iter().any(|t| {
                t["role"] == "permission"
                    && t["status"] == "approved"
                    && t["payload"]["chosen_option"] == "allow_once"
            }));
            done_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(done_ok, "bootstrap should persist completed session and turns");

    // 9. 设备登出应踢掉所属 ACP 宿主 WS 连接
    let logout_res = http()
        .post(format!("{base}/v1/auth/logout"))
        .json(&json!({ "refresh_token": u.refresh }))
        .send()
        .await
        .unwrap();
    assert_eq!(logout_res.status(), 204);
    let kicked = next_of(&mut host_ws, "force_logout").await;
    assert_eq!(kicked["type"], "force_logout");
}
