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

    // 3b. 远程文件预览读取 RPC (GET /v1/acp/hosts/{id}/file) 及越权拦截
    let forbidden_file = http()
        .get(format!(
            "{base}/v1/acp/hosts/{host_id}/file?path=src/main.rs&cwd=/home/dev/chat"
        ))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_file.status(), 404);

    let file_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .get(format!(
                    "{base}/v1/acp/hosts/{host_id}/file?path=src/main.rs&cwd=/home/dev/chat"
                ))
                .header("authorization", authz(&access))
                .send()
                .await
                .unwrap()
        }
    });

    let file_rpc = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(file_rpc["method"], "read_file");
    assert_eq!(file_rpc["params"]["path"], "src/main.rs");
    assert_eq!(file_rpc["params"]["cwd"], "/home/dev/chat");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": file_rpc["req_id"],
                "ok": true,
                "data": {
                    "path": "/home/dev/chat/src/main.rs",
                    "file_name": "main.rs",
                    "mime": "text/x-rust",
                    "size": 14,
                    "encoding": "utf8",
                    "content": "fn main() {}\n",
                    "truncated": false
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let file_res = file_req.await.unwrap();
    assert_eq!(file_res.status(), 200);
    let file_json: Value = file_res.json().await.unwrap();
    assert_eq!(file_json["file_name"], "main.rs");
    assert_eq!(file_json["content"], "fn main() {}\n");

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

    // 9. 设备登出应踢掉所属 ACP 宿主 WS 连接并使主机离线
    let u_watch: Value = http()
        .post(format!("{base}/v1/auth/login"))
        .json(&json!({ "username": u.username, "password": "secret123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let watch_access = u_watch["access_token"].as_str().unwrap().to_string();

    let logout_res = http()
        .post(format!("{base}/v1/auth/logout"))
        .json(&json!({ "refresh_token": u.refresh }))
        .send()
        .await
        .unwrap();
    assert_eq!(logout_res.status(), 204);
    let kicked = next_of(&mut host_ws, "force_logout").await;
    assert_eq!(kicked["type"], "force_logout");
    drop(host_ws);

    let mut offline_ok = false;
    for _ in 0..20 {
        let bs: Value = http()
            .get(format!("{base}/v1/acp/bootstrap"))
            .header("authorization", authz(&watch_access))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if bs["hosts"][0]["online"] == false {
            offline_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(offline_ok, "host should become offline after logout kick");
}

#[tokio::test]
async fn acp_security_concurrency_pairing_and_cleanup() {
    let base = spawn().await;
    let u = register(&base, &format!("acps_{}", uniq())).await;

    // 1. 一次性配对码生成、兑换及重放拒绝
    let pc_res = http()
        .post(format!("{base}/v1/acp/hosts/pair-code"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap();
    assert_eq!(pc_res.status(), 200);
    let pc_json: Value = pc_res.json().await.unwrap();
    let pair_code = pc_json["code"].as_str().unwrap().to_string();
    assert!(pair_code.starts_with("ACP-"));

    let pair_ok = http()
        .post(format!("{base}/v1/acp/hosts/pair"))
        .json(&json!({ "code": pair_code, "host_name": "PairedNode" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair_ok.status(), 200);
    let pair_tokens: Value = pair_ok.json().await.unwrap();
    let paired_access = pair_tokens["access_token"].as_str().unwrap().to_string();

    let pair_replay = http()
        .post(format!("{base}/v1/acp/hosts/pair"))
        .json(&json!({ "code": pair_code, "host_name": "ReplayNode" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair_replay.status(), 401);

    // 用配对兑换的 token 获取 WS ticket 建立模拟宿主
    let paired_user = TestUser {
        id: u.id.clone(),
        username: u.username.clone(),
        access: paired_access,
        refresh: pair_tokens["refresh_token"].as_str().unwrap().into(),
    };
    let mut host_ws = host_ws_connect(&base, &paired_user).await;
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": Uuid::new_v4(),
                "name": "PairedNode",
                "os": "Linux",
                "ip": "127.0.0.1",
                "roots": ["/tmp"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ready = next_of(&mut host_ws, "host_ready").await;
    let host_id = ready["host_id"].as_str().unwrap().to_string();

    // 2. 超长 Agent 名 + 工作区名默认标题截断 (<= 160 chars) 与 env 脱敏/保留
    let long_agent = "A".repeat(110);
    let long_ws = "W".repeat(110);
    let ag_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        let long_agent = long_agent.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/agents"))
                .header("authorization", authz(&access))
                .json(&json!({
                    "host_id": host_id,
                    "preset": "custom",
                    "name": long_agent,
                    "cmd": "echo",
                    "env": { "API_SECRET": "super-secret-val" }
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
    let ag_res = ag_req.await.unwrap();
    assert_eq!(ag_res.status(), 200);
    let ag_json: Value = ag_res.json().await.unwrap();
    let agent_id = ag_json["id"].as_str().unwrap().to_string();
    assert_eq!(ag_json["env"]["API_SECRET"], "••••••••");

    let bs: Value = http()
        .get(format!("{base}/v1/acp/bootstrap"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bs["agents"][0]["env"]["API_SECRET"], "••••••••");

    // 带占位符 "••••••••" 更新 env 应保留原密钥
    let ag_patch: Value = http()
        .patch(format!("{base}/v1/acp/agents/{agent_id}"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "env": { "API_SECRET": "••••••••", "PUBLIC_TAG": "v1" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ag_patch["env"]["API_SECRET"], "••••••••");
    assert_eq!(ag_patch["env"]["PUBLIC_TAG"], "••••••••");

    let ws_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        let long_ws = long_ws.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/workspaces"))
                .header("authorization", authz(&access))
                .json(&json!({
                    "host_id": host_id,
                    "cwd": "/tmp/w",
                    "name": long_ws
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
                "data": { "cwd": "/tmp/w", "branch": "main", "branches": ["main"] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ws_json: Value = ws_req.await.unwrap().json().await.unwrap();
    let ws_id = ws_json["id"].as_str().unwrap().to_string();

    // 留空标题新建会话：默认拼接截断至 160 字符内并创建成功
    let sess_res = http()
        .post(format!("{base}/v1/acp/sessions"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "workspace_id": ws_id,
            "agent_id": agent_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sess_res.status(), 200);
    let sess_json: Value = sess_res.json().await.unwrap();
    let sess_id = sess_json["id"].as_str().unwrap().to_string();
    assert_eq!(sess_json["title"].as_str().unwrap().chars().count(), 160);

    // 3. 上传附件 + 并发 send_prompt 互斥（一个 200 一个 409）+ 原始 env 下发
    let part = Part::bytes(b"cleanup-bytes".to_vec())
        .file_name("c.txt")
        .mime_str("text/plain")
        .unwrap();
    let att_json: Value = http()
        .post(format!("{base}/v1/acp/files"))
        .header("authorization", authz(&u.access))
        .multipart(Form::new().text("session_id", sess_id.clone()).part("file", part))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let att_id = att_json["id"].as_str().unwrap().to_string();
    let st_before: Value = http()
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(st_before["used_bytes"].as_i64().unwrap() >= 13);

    let req1 = http()
        .post(format!("{base}/v1/acp/sessions/{sess_id}/prompts"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "text": "one", "attachment_ids": [att_id] }))
        .send();
    let req2 = http()
        .post(format!("{base}/v1/acp/sessions/{sess_id}/prompts"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "text": "two" }))
        .send();
    let (r1, r2) = tokio::join!(req1, req2);
    let mut statuses = vec![r1.unwrap().status().as_u16(), r2.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, vec![200, 409]);

    let start = next_of(&mut host_ws, "prompt_start").await;
    assert_eq!(start["agent"]["env"]["API_SECRET"], "super-secret-val");
    assert_eq!(start["agent"]["env"]["PUBLIC_TAG"], "v1");

    // 4. 删除会话应清理关联附件物理文件与配额统计
    let del_res = http()
        .delete(format!("{base}/v1/acp/sessions/{sess_id}"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap();
    assert!(del_res.status().is_success());

    let att_after = http()
        .get(format!("{base}/v1/acp/files/{att_id}"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap();
    assert_eq!(att_after.status(), 404);

    let st_after: Value = http()
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st_after["used_bytes"], 0);
}

#[tokio::test]
async fn create_installable_agent_triggers_ensure_and_status_updates() {
    let base = spawn().await;
    let u = register(&base, &format!("acpens_{}", uniq())).await;

    let (mut client_ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();
    let mut host_ws = host_ws_connect(&base, &u).await;

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": Uuid::new_v4(),
                "name": "Win-Node",
                "os": "windows · x86_64",
                "ip": "127.0.0.1",
                "roots": ["C:\\work"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ready = next_of(&mut host_ws, "host_ready").await;
    let host_id = ready["host_id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "host_upserted").await;

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
                    "preset": "codex",
                    "name": "Codex CLI",
                    "cmd": "codex-acp"
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
                "data": {
                    "ok": false,
                    "installable": true,
                    "package": "@agentclientprotocol/codex-acp"
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let ag_res: Value = ag_req.await.unwrap().json().await.unwrap();
    let agent_id = ag_res["id"].as_str().unwrap().to_string();
    assert_eq!(ag_res["status"], "installing");

    let created_ev = next_acp_event(&mut client_ws, "agent_upserted").await;
    assert_eq!(created_ev["data"]["agent"]["status"], "installing");

    let ensure_msg = next_of(&mut host_ws, "ensure_agent").await;
    assert_eq!(ensure_msg["agent_id"], agent_id);
    assert_eq!(ensure_msg["cmd"], "codex-acp");

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "agent_status",
                "agent_id": agent_id,
                "status": "online"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let online_ev = next_acp_event(&mut client_ws, "agent_upserted").await;
    assert_eq!(online_ev["data"]["agent"]["id"], agent_id);
    assert_eq!(online_ev["data"]["agent"]["status"], "online");
}

#[tokio::test]
async fn agent_model_probe_on_attach_new_session_async_sync_and_option_delete_fallback() {
    let base = spawn().await;
    let u = register(&base, &format!("acpm_{}", uniq())).await;

    let (mut client_ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();
    let mut host_ws = host_ws_connect(&base, &u).await;

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": Uuid::new_v4(),
                "name": "DevHost",
                "os": "windows · x86_64",
                "ip": "127.0.0.1",
                "roots": ["C:\\work"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ready = next_of(&mut host_ws, "host_ready").await;
    let host_id = ready["host_id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "host_upserted").await;

    // 1. 接入时 probe_agent 直接返回真实模型列表
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
                    "preset": "codex",
                    "name": "Codex CLI",
                    "cmd": "codex-acp"
                }))
                .send()
                .await
                .unwrap()
        }
    });

    let probe_ag = next_of(&mut host_ws, "rpc_call").await;
    assert_eq!(probe_ag["method"], "probe_agent");
    assert_eq!(probe_ag["params"]["preset"], "codex");
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": probe_ag["req_id"],
                "ok": true,
                "data": {
                    "ok": true,
                    "installable": true,
                    "default_model": "xy-openai/gpt-6.2-astra",
                    "model_options": [
                        "xy-open/glm-5.3",
                        "xy-openai/gpt-6.2-astra"
                    ]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let ag_res: Value = ag_req.await.unwrap().json().await.unwrap();
    let agent_id = ag_res["id"].as_str().unwrap().to_string();
    assert_eq!(ag_res["default_model"], "xy-openai/gpt-6.2-astra");
    assert_eq!(
        ag_res["model_options"],
        json!(["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"])
    );
    let _ = next_acp_event(&mut client_ws, "agent_upserted").await;

    // 2. 删除当前默认模型选项时，default_model 自动回退到首项
    let after_del: Value = http()
        .patch(format!("{base}/v1/acp/agents/{agent_id}"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "model_options": ["xy-open/glm-5.3"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_del["default_model"], "xy-open/glm-5.3");
    assert_eq!(after_del["model_options"], json!(["xy-open/glm-5.3"]));
    let _ = next_acp_event(&mut client_ws, "agent_upserted").await;

    // 3. 创建工作区并新建会话：服务端立即返回会话，同时异步向宿主机发送 probe_agent_models
    let ws_req = tokio::spawn({
        let base = base.clone();
        let host_id = host_id.clone();
        let access = u.access.clone();
        async move {
            http()
                .post(format!("{base}/v1/acp/workspaces"))
                .header("authorization", authz(&access))
                .json(&json!({ "host_id": host_id, "cwd": "C:\\work\\repo" }))
                .send()
                .await
                .unwrap()
        }
    });
    let probe_ws = next_of(&mut host_ws, "rpc_call").await;
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "rpc_result",
                "req_id": probe_ws["req_id"],
                "ok": true,
                "data": { "cwd": "C:\\work\\repo", "branch": "main", "branches": ["main"] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ws_json: Value = ws_req.await.unwrap().json().await.unwrap();
    let ws_id = ws_json["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "workspace_upserted").await;

    // 模拟历史旧预设残留（如 GPT-5 Codex），验证新建会话后异步探测能同步刷新 Agent 与新会话模型
    let _ = http()
        .patch(format!("{base}/v1/acp/agents/{agent_id}"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "default_model": "GPT-5 Codex",
            "model_options": ["GPT-5 Codex", "o3-pro"]
        }))
        .send()
        .await
        .unwrap();
    let _ = next_acp_event(&mut client_ws, "agent_upserted").await;

    let sess: Value = http()
        .post(format!("{base}/v1/acp/sessions"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "workspace_id": ws_id, "agent_id": agent_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sess_id = sess["id"].as_str().unwrap().to_string();
    assert_eq!(sess["selected_model"], "GPT-5 Codex");
    let _ = next_acp_event(&mut client_ws, "session_upserted").await;

    let probe_msg = next_of(&mut host_ws, "probe_agent_models").await;
    assert_eq!(probe_msg["agent_id"], agent_id);
    assert_eq!(probe_msg["session_id"], sess_id);
    assert_eq!(probe_msg["cwd"], "C:\\work\\repo");

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "agent_models",
                "agent_id": agent_id,
                "session_id": sess_id,
                "default_model": "xy-openai/gpt-6.2-sol",
                "model_options": [
                    "xy-open/glm-5.3",
                    "xy-openai/gpt-6.2-sol",
                    "xy-openai/gpt-6.2-astra"
                ]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let ag_ev = next_acp_event(&mut client_ws, "agent_upserted").await;
    assert_eq!(ag_ev["data"]["agent"]["default_model"], "xy-openai/gpt-6.2-sol");
    assert_eq!(
        ag_ev["data"]["agent"]["model_options"],
        json!([
            "xy-open/glm-5.3",
            "xy-openai/gpt-6.2-sol",
            "xy-openai/gpt-6.2-astra"
        ])
    );

    let sess_ev = next_acp_event(&mut client_ws, "session_upserted").await;
    assert_eq!(sess_ev["data"]["session"]["id"], sess_id);
    assert_eq!(sess_ev["data"]["session"]["selected_model"], "xy-openai/gpt-6.2-sol");
}

#[tokio::test]
async fn acp_streams_thoughts_tool_calls_and_activity_events() {
    let base = spawn().await;
    let u = register(&base, &format!("acpact_{}", uniq())).await;

    let (mut client_ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();
    let mut host_ws = host_ws_connect(&base, &u).await;

    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": Uuid::new_v4(),
                "name": "StreamBox",
                "os": "Linux 6.8",
                "ip": "127.0.0.1",
                "roots": ["/tmp/repo"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ready = next_of(&mut host_ws, "host_ready").await;
    let host_id = ready["host_id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "host_upserted").await;

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
                    "preset": "pi",
                    "name": "Pi",
                    "cmd": "pi --mode rpc",
                    "local_probed": true,
                    "status": "online"
                }))
                .send()
                .await
                .unwrap()
        }
    });
    let ag_res: Value = ag_req.await.unwrap().json().await.unwrap();
    let agent_id = ag_res["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "agent_upserted").await;

    let ws_json: Value = http()
        .post(format!("{base}/v1/acp/workspaces"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "host_id": host_id,
            "cwd": "/tmp/repo",
            "local_probed": true,
            "branch": "main",
            "branches": ["main"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ws_id = ws_json["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "workspace_upserted").await;

    let sess: Value = http()
        .post(format!("{base}/v1/acp/sessions"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "workspace_id": ws_id,
            "agent_id": agent_id,
            "skip_model_probe": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sess_id = sess["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "session_upserted").await;

    let p_res = http()
        .post(format!("{base}/v1/acp/sessions/{sess_id}/prompts"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "text": "检查代码" }))
        .send()
        .await
        .unwrap();
    assert_eq!(p_res.status(), 200);
    let _ = next_of(&mut host_ws, "prompt_start").await;

    // 1. 实时状态广播（不落库）
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "activity",
                "phase": "thinking",
                "detail": "正在思考…",
                "usage": { "input_tokens": 120, "output_tokens": 15, "total_tokens": 135 }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let act_ev = next_acp_event(&mut client_ws, "session_activity").await;
    assert_eq!(act_ev["data"]["phase"], "thinking");
    assert_eq!(act_ev["data"]["usage"]["total_tokens"], 135);

    // 2. 思考流增量 + 完成耗时
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "thought_delta",
                "turn_key": "th-1",
                "delta": "先看下目录",
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
                "kind": "thought_delta",
                "turn_key": "th-1",
                "delta": "结构。",
                "done": true,
                "duration_ms": 950
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    // 3. 工具调用生命周期：pending -> in_progress + output_delta -> completed + duration_ms
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "tool_call",
                "tool_call_id": "tc-1",
                "name": "bash",
                "tool_kind": "execute",
                "title": "bash: cargo check",
                "input": "cargo check",
                "status": "pending"
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
                "kind": "tool_call",
                "tool_call_id": "tc-1",
                "status": "in_progress",
                "output_delta": "Checking crate...\n"
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
                "kind": "tool_call",
                "tool_call_id": "tc-1",
                "status": "completed",
                "output_delta": "Finished dev profile\n",
                "duration_ms": 620
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    // 4. 正文流 + prompt_done
    host_ws
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "text_delta",
                "turn_key": "txt-1",
                "delta": "检查通过。",
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
                "acp_session_key": "pi-k-1"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let mut verified = false;
    for _ in 0..20 {
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
        if s["running"] == false && s["acp_session_key"] == "pi-k-1" {
            let turns = s["turns"].as_array().unwrap();
            let th = turns.iter().find(|t| t["role"] == "agent_thought").unwrap();
            assert_eq!(th["status"], "done");
            assert_eq!(th["payload"]["text"], "先看下目录结构。");
            assert_eq!(th["payload"]["duration_ms"], 950);

            let tc = turns.iter().find(|t| t["role"] == "tool_call").unwrap();
            assert_eq!(tc["status"], "completed");
            assert_eq!(tc["payload"]["kind"], "execute");
            assert_eq!(tc["payload"]["title"], "bash: cargo check");
            assert_eq!(
                tc["payload"]["output"],
                "Checking crate...\nFinished dev profile\n"
            );
            assert_eq!(tc["payload"]["duration_ms"], 620);

            let txt = turns.iter().find(|t| t["role"] == "agent_text").unwrap();
            assert_eq!(txt["status"], "done");
            assert_eq!(txt["payload"]["text"], "检查通过。");
            verified = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(verified, "bootstrap should persist thought, tool_call, and text turns");
}

#[tokio::test]
async fn acp_host_reconnect_settles_orphaned_running_session_and_activity() {
    let base = spawn().await;
    let u = register(&base, &format!("acpr_{}", uniq())).await;
    let (mut client_ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();

    let install_id = Uuid::new_v4();
    let mut host_ws_1 = host_ws_connect(&base, &u).await;
    host_ws_1
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": install_id,
                "name": "DevBox",
                "os": "linux · x86_64",
                "ip": "Client-Daemon",
                "roots": ["/tmp"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ready1 = next_of(&mut host_ws_1, "host_ready").await;
    let host_id = ready1["host_id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "host_upserted").await;

    let ag_res: Value = http()
        .post(format!("{base}/v1/acp/agents"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "host_id": host_id,
            "preset": "codex",
            "name": "Codex",
            "cmd": "codex-acp",
            "local_probed": true,
            "status": "online"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = ag_res["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "agent_upserted").await;

    let ws_json: Value = http()
        .post(format!("{base}/v1/acp/workspaces"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "host_id": host_id,
            "cwd": "/tmp/repo",
            "local_probed": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ws_id = ws_json["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "workspace_upserted").await;

    let sess: Value = http()
        .post(format!("{base}/v1/acp/sessions"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "workspace_id": ws_id,
            "agent_id": agent_id,
            "skip_model_probe": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sess_id = sess["id"].as_str().unwrap().to_string();
    let _ = next_acp_event(&mut client_ws, "session_upserted").await;

    let p_res = http()
        .post(format!("{base}/v1/acp/sessions/{sess_id}/prompts"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "text": "长任务执行" }))
        .send()
        .await
        .unwrap();
    assert_eq!(p_res.status(), 200);
    let _ = next_of(&mut host_ws_1, "prompt_start").await;

    host_ws_1
        .send(WsMsg::Text(
            json!({
                "type": "session_update",
                "session_id": sess_id,
                "kind": "tool_call",
                "tool_call_id": "tc-hang",
                "name": "bash",
                "tool_kind": "execute",
                "title": "bash: long-task",
                "status": "in_progress"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let _ = next_acp_event(&mut client_ws, "turn_upserted").await;

    // 模拟宿主在旧 WS 未主动断开的情况下直接重连（顶替旧连接）
    let mut host_ws_2 = host_ws_connect(&base, &u).await;
    host_ws_2
        .send(WsMsg::Text(
            json!({
                "type": "host_hello",
                "install_id": install_id,
                "name": "DevBox",
                "os": "linux · x86_64",
                "ip": "Client-Daemon",
                "roots": ["/tmp"]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let superseded = next_of(&mut host_ws_1, "host_superseded").await;
    assert_eq!(superseded["type"], "host_superseded");
    let ready2 = next_of(&mut host_ws_2, "host_ready").await;
    assert_eq!(ready2["host_id"], host_id);

    let act_idle = next_acp_event(&mut client_ws, "session_activity").await;
    assert_eq!(act_idle["data"]["session_id"], sess_id);
    assert_eq!(act_idle["data"]["phase"], "idle");

    let bs: Value = http()
        .get(format!("{base}/v1/acp/bootstrap"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let settled_sess = bs["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == sess_id)
        .unwrap();
    assert_eq!(settled_sess["running"], false);
    let settled_tc = settled_sess["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["payload"]["tool_call_id"] == "tc-hang")
        .unwrap();
    assert_eq!(settled_tc["status"], "failed");
    assert_eq!(settled_tc["payload"]["is_error"], true);
}
