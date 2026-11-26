use super::*;

#[tokio::test]
async fn client_heartbeat_and_replayed_message_are_idempotent() {
    let base = spawn().await;
    let a = register(&base, &format!("hb_{}", uniq())).await;
    let b = register(&base, &format!("hb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut socket, "presence_init").await;
    socket
        .send(WsMsg::Text(json!({"type":"ping"}).to_string().into()))
        .await
        .unwrap();
    next_of(&mut socket, "pong").await;
    let (mut peer, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut peer, "presence_init").await;

    let frame = json!({"client_msg_id":"heartbeat-replay", "content":"retry"});
    let mut request = frame.clone();
    request["type"] = json!("send_message");
    request["conversation_id"] = json!(conv);
    socket
        .send(WsMsg::Text(request.to_string().into()))
        .await
        .unwrap();
    let echo = next_of(&mut socket, "message").await;
    assert_eq!(echo["client_msg_id"], "heartbeat-replay");
    let delivered = next_of(&mut peer, "message").await;
    assert!(delivered.get("client_msg_id").is_none());
    let first = next_of(&mut socket, "message_ack").await["message"].clone();
    let replay = ws_send(&mut socket, &conv, frame).await;
    assert_eq!(first["id"], replay["id"]);
    assert_eq!(first["seq"], replay["seq"]);
    let separate = ws_send(
        &mut socket,
        &conv,
        json!({"client_msg_id":"heartbeat-separate", "content":"retry"}),
    )
    .await;
    assert_ne!(first["id"], separate["id"]);
}

// ---------- review 回归 ----------

/// P1-1：附件下载安全头。HTML/SVG 一律 attachment + nosniff；图片/PDF 仍可 inline。
#[tokio::test]
async fn attachment_download_hardened() {
    let base = spawn().await;
    let a = register(&base, &format!("fa_{}", uniq())).await;
    let b = register(&base, &format!("fb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    async fn upload(
        base: &str,
        conv: &str,
        at: &str,
        name: &str,
        mime: &str,
        body: &[u8],
    ) -> String {
        let part = reqwest::multipart::Part::bytes(body.to_vec())
            .file_name(name.to_string())
            .mime_str(mime)
            .unwrap();
        let form = reqwest::multipart::Form::new()
            .text("conversation_id", conv.to_string())
            .part("file", part);
        let r = http()
            .post(format!("{base}/v1/files"))
            .header("authorization", authz(at))
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    // text/html 上传 → 下载必须 attachment + nosniff（不允许同源 inline 执行）
    let id = upload(
        &base,
        &conv,
        &a.access,
        "evil.html",
        "text/html",
        b"<script>x</script>",
    )
    .await;
    let dl = c.get(format!("{base}/v1/files/{id}")).header("authorization", authz(&a.access)).send().await.unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.headers()["x-content-type-options"], "nosniff");
    assert!(dl.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));

    // svg 带脚本 → 同样 attachment
    let id = upload(
        &base,
        &conv,
        &a.access,
        "i.svg",
        "image/svg+xml",
        b"<svg><script>x</script></svg>",
    )
    .await;
    let dl = c.get(format!("{base}/v1/files/{id}")).header("authorization", authz(&a.access)).send().await.unwrap();
    assert!(dl.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));

    // png → inline 预览保留，nosniff 同样带上
    let png: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let id = upload(&base, &conv, &a.access, "p.png", "image/png", png).await;
    let dl = c.get(format!("{base}/v1/files/{id}")).header("authorization", authz(&a.access)).send().await.unwrap();
    assert!(dl.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("inline"));
    assert_eq!(dl.headers()["x-content-type-options"], "nosniff");

    // dl=1 时即便安全 MIME 也强制 attachment
    let dl = c
        .get(format!("{base}/v1/files/{id}?dl=1"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert!(dl.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
}

/// 文件和 multipart 请求各有大小上限，但错误提示相同。
#[tokio::test]
async fn upload_body_limit_consistent() {
    let base = spawn().await;
    let a = register(&base, &format!("ua_{}", uniq())).await;
    let b = register(&base, &format!("ub_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;

    let up = |bytes: usize| {
        let base = base.clone();
        let conv = conv.clone();
        let at = a.access.clone();
        async move {
            let part = reqwest::multipart::Part::bytes(vec![0u8; bytes])
                .file_name("big.bin".to_string())
                .mime_str("application/octet-stream")
                .unwrap();
            let form = reqwest::multipart::Form::new()
                .text("conversation_id", conv)
                .part("file", part);
            let r = http()
                .post(format!("{base}/v1/files"))
                .header("authorization", authz(&at))
                .multipart(form)
                .send()
                .await
                .unwrap();
            (r.status().as_u16(), r.json::<serde_json::Value>().await.unwrap())
        }
    };

    for size in [25 * 1024 * 1024 + 1, 26 * 1024 * 1024] {
        let (st, body) = up(size).await;
        assert_eq!(st, 413);
        assert_eq!(body["error"], "文件大小不能超过 25 MB");
    }
}

/// P1-4：并发非法 refresh 不得拖垮连接池。全部 401，/users/me 仍即时响应。
#[tokio::test]
async fn refresh_flood_no_pool_starvation() {
    let base = spawn().await;
    let u = register(&base, &format!("rf_{}", uniq())).await;

    let mut hs = Vec::new();
    for i in 0..40 {
        let base = base.clone();
        hs.push(tokio::spawn(async move {
            http()
                .post(format!("{base}/v1/auth/refresh"))
                .json(&json!({ "refresh_token": format!("bad-{i}") }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    for h in hs {
        assert_eq!(h.await.unwrap(), 401);
    }

    // 池没有被拖死：正常请求立刻成功
    let r = http()
        .get(format!("{base}/v1/users/me"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

// Observe the lock barrier, not elapsed time, before advancing a concurrent operation.
pub(crate) async fn wait_for_blocker<T>(
    pool: &sqlx::PgPool,
    pid: i32,
    task: &tokio::task::JoinHandle<T>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
            ).bind(pid).fetch_one(pool).await.unwrap();
            if waiting || task.is_finished() { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("operation did not reach lock barrier");
}

#[tokio::test]
async fn token_issuance_waits_for_user_authorization() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_issue_{}", uniq())).await;
    let _admin = register(&base, &format!("ia_{}", uniq())).await;
    for refresh in [false, true] {
        let u = register(&base, &format!("iu_{}", uniq())).await;
        let uid = uuid::Uuid::parse_str(&u.id).unwrap();
        let mut reset = st.pool.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *reset)
            .await
            .unwrap();
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(uid)
            .execute(&mut *reset)
            .await
            .unwrap();
        let request = if refresh {
            http()
                .post(format!("{base}/v1/auth/refresh"))
                .json(&json!({"refresh_token":u.refresh}))
        } else {
            http()
                .post(format!("{base}/v1/auth/login"))
                .json(&json!({"username":u.username,"password":"secret123"}))
        };
        let task = tokio::spawn(async move { request.send().await.unwrap() });
        wait_for_blocker(&st.pool, pid, &task).await;
        // Commit the revocation while the issuance request waits for authorization.
        sqlx::query("UPDATE users SET password_hash = $2, session_version = session_version + 1 WHERE id = $1")
            .bind(uid).bind(chat_server::auth::hash_password("new-password").await.unwrap())
            .execute(&mut *reset).await.unwrap();
        sqlx::query("UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1")
            .bind(uid)
            .execute(&mut *reset)
            .await
            .unwrap();
        reset.commit().await.unwrap();
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            r.status(),
            401,
            "stale login/refresh authorization must not mint tokens"
        );
    }
}

#[tokio::test]
async fn registration_is_not_visible_before_token_issuance_commits() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_register_{}", uniq())).await;
    let admin = register(&base, &format!("rga_{}", uniq())).await;
    let name = format!("rgu_{}", uniq());
    let mut barrier = st.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    sqlx::query("LOCK TABLE refresh_tokens IN SHARE MODE")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let request = http()
        .post(format!("{base}/v1/auth/register"))
        .json(&json!({"username":name,"password":"secret123"}));
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    wait_for_blocker(&st.pool, pid, &task).await;
    assert!(!task.is_finished());
    let visible: Value = http()
        .get(format!("{base}/v1/admin/users?q={name}"))
        .header("authorization", authz(&admin.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        visible,
        json!([]),
        "an admin must not reset/disable a half-issued registration"
    );
    barrier.commit().await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn reset_and_disable_revoke_tokens_atomically() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_atomic_{}", uniq())).await;
    let admin = register(&base, &format!("ata_{}", uniq())).await;
    for operation in ["password", "disable"] {
        let u = register(&base, &format!("atu_{}", uniq())).await;
        let uid = uuid::Uuid::parse_str(&u.id).unwrap();
        let mut barrier = st.pool.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *barrier)
            .await
            .unwrap();
        sqlx::query("SELECT id FROM refresh_tokens WHERE user_id = $1 FOR UPDATE")
            .bind(uid)
            .execute(&mut *barrier)
            .await
            .unwrap();
        let request = http()
            .post(format!("{base}/v1/admin/users/{}/{operation}", u.id))
            .header("authorization", authz(&admin.access))
            .json(&json!({"password":"new-password"}));
        let task = tokio::spawn(async move { request.send().await.unwrap() });
        wait_for_blocker(&st.pool, pid, &task).await;
        assert!(
            !task.is_finished(),
            "revocation must wait for the token barrier"
        );
        let during = http()
            .get(format!("{base}/v1/users/me"))
            .header("authorization", authz(&u.access))
            .send()
            .await
            .unwrap();
        // Neither new credentials nor disabled/version state may commit before token revocation.
        assert_eq!(
            during.status(),
            200,
            "partially committed session revocation"
        );
        barrier.commit().await.unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .status(),
            204
        );
        let r = http()
            .post(format!("{base}/v1/auth/refresh"))
            .json(&json!({"refresh_token":u.refresh}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
    }
}

/// P1-3：管理员重置密码后，旧 access token 与 WS ticket 立即失效。
#[tokio::test]
async fn reset_password_revokes_access() {
    let (_guard, base) = spawn_named_db(&format!("chat_t_rp_{}", uniq())).await;
    let c = http();
    let a = register(&base, &format!("ra_{}", uniq())).await; // 首个 = admin
    let b = register(&base, &format!("rb_{}", uniq())).await;

    // 重置前 token 可用
    let r = c
        .get(format!("{base}/v1/users/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let r = c
        .post(format!("{base}/v1/admin/users/{}/password", b.id))
        .header("authorization", authz(&a.access))
        .json(&json!({"password": "rotated999"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // 旧 access token → 401（session_version 已 +1）
    let r = c
        .get(format!("{base}/v1/users/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // WS ticket 同样失效
    let r = c
        .get(format!("{base}/v1/ws/ticket"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // 旧 refresh 已撤销
    let r = c
        .post(format!("{base}/v1/auth/refresh"))
        .json(&json!({"refresh_token": b.refresh}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // 新密码可登录
    let r = c
        .post(format!("{base}/v1/auth/login"))
        .json(&json!({"username": b.username, "password": "rotated999"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// P1-2：禁用用户时旧 WS 收到 force_logout 后被服务端主动关闭；旧连接无法再写。
#[tokio::test]
async fn force_logout_actually_closes_socket() {
    let (_guard, base) = spawn_named_db(&format!("chat_t_fk_{}", uniq())).await;
    let c = http();
    let a = register(&base, &format!("ka_{}", uniq())).await;
    let b = register(&base, &format!("kb_{}", uniq())).await;

    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    let r = c
        .post(format!("{base}/v1/admin/users/{}/disable", b.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // 先收 force_logout 帧，随后服务端主动发 Close
    let f = next_of(&mut sb, "force_logout").await;
    assert_eq!(f["reason"], "disabled");
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match sb.next().await {
                Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => break true,
                _ => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "server did not close the kicked socket");

    // 旧 access token 一并失效
    let r = c
        .get(format!("{base}/v1/users/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert!(r.status() == 401 || r.status() == 403);
}

pub(crate) async fn closed_without_application_frames<S>(socket: &mut S)
where
    S: StreamExt<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMsg::Close(_))) | None => break,
                Some(Ok(WsMsg::Text(text))) => {
                    let f: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(
                        f["type"], "force_logout",
                        "revoked connection accepted application frames"
                    );
                }
                Some(Err(e)) => panic!("expected a clean server close: {e}"),
                _ => {}
            }
        }
    })
    .await
    .expect("revoked connection was not closed");
}

#[tokio::test]
async fn ws_reset_between_upgrade_and_registration_closes_without_presence() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_upgrade_{}", uniq())).await;
    let admin = register(&base, &format!("uga_{}", uniq())).await;
    let u = register(&base, &format!("ugu_{}", uniq())).await;
    let grant = chat_server::auth::verify_access(&u.access, &st.cfg.jwt_secret).unwrap();
    let uid = grant.uid;
    let version = grant.session_version;
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let resume = release.clone();
    let session_state = st.clone();
    // Pause only the test's upgrade continuation; production needs no timing/debug hooks.
    let router = axum::Router::new().route(
        "/",
        axum::routing::get(move |upgrade: axum::extract::WebSocketUpgrade| {
            let resume = resume.clone();
            let state = session_state.clone();
            async move {
                upgrade.on_upgrade(move |socket| async move {
                    resume.notified().await;
                    chat_server::ws::handle_session(
                        socket,
                        state,
                        chat_server::auth::AccessGrant {
                            uid,
                            session_version: version,
                            sid: None,
                        },
                    )
                    .await;
                })
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut delayed, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
        .await
        .unwrap();
    let reset = http()
        .post(format!("{base}/v1/admin/users/{}/password", u.id))
        .header("authorization", authz(&admin.access))
        .json(&json!({"password":"new-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), 204);
    assert_eq!(st.conn_count(uid), 0);
    release.notify_one();
    closed_without_application_frames(&mut delayed).await;
    assert_eq!(
        st.conn_count(uid),
        0,
        "late revoked registration leaked presence"
    );
    assert!(!st.is_online(uid));
    server.abort();
}

#[tokio::test]
async fn ws_rechecks_session_version_before_frames_and_preserves_other_connections() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_frame_{}", uniq())).await;
    let u = register(&base, &format!("fvu_{}", uniq())).await;
    let peer = register(&base, &format!("fvp_{}", uniq())).await;
    befriend(&base, &u, &peer).await;
    let uid = uuid::Uuid::parse_str(&u.id).unwrap();
    let conv = mk_group(&base, &u, &[peer.id.clone()]).await;
    let (mut old, _) = tokio_tungstenite::connect_async(ws_url(&base, &u).await)
        .await
        .unwrap();
    next_of(&mut old, "presence_init").await;
    // Model the committed version change before the in-memory kick is delivered.
    sqlx::query("UPDATE users SET session_version = session_version + 1 WHERE id = $1")
        .bind(uid)
        .execute(&st.pool)
        .await
        .unwrap();
    let r = http()
        .post(format!("{base}/v1/auth/login"))
        .json(&json!({"username":u.username,"password":"secret123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let tokens: Value = r.json().await.unwrap();
    let fresh = TestUser {
        id: u.id,
        username: u.username,
        access: tokens["access_token"].as_str().unwrap().into(),
        refresh: tokens["refresh_token"].as_str().unwrap().into(),
    };
    let (mut current, _) = tokio_tungstenite::connect_async(ws_url(&base, &fresh).await)
        .await
        .unwrap();
    next_of(&mut current, "presence_init").await;
    assert_eq!(st.conn_count(uid), 2);
    old.send(WsMsg::Text(
        json!({"type":"sync","conversation_id":conv,"last_seq":0})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    closed_without_application_frames(&mut old).await;
    // Fence the remaining connection and check that stale cleanup did not remove its epoch/bucket.
    current
        .send(WsMsg::Text(
            json!({"type":"sync","conversation_id":conv,"last_seq":0})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        next_of(&mut current, "sync_messages"),
    )
    .await
    .unwrap();
    assert_eq!(st.conn_count(uid), 1);
    assert!(st.is_online(uid));
}

/// P1-6：群主转让与退群并发——最终必须恰好剩一个群主，且仍是成员。
#[tokio::test]
async fn owner_transfer_leave_race() {
    let base = spawn().await;
    let a = register(&base, &format!("oa_{}", uniq())).await;
    let b = register(&base, &format!("ob_{}", uniq())).await;
    let cc = register(&base, &format!("oc_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    befriend(&base, &a, &cc).await;
    let c = http();
    let detail: Value = c
        .post(format!("{base}/v1/conversations/group"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "race", "member_ids": [b.id, cc.id]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = detail["id"].as_str().unwrap().to_string();

    // 并发：a 转让群主给 b + a 退群
    let (r1, r2) = tokio::join!(
        c.post(format!("{base}/v1/conversations/{gid}/transfer"))
            .header("authorization", authz(&a.access))
            .json(&json!({"user_id": b.id}))
            .send(),
        c.delete(format!("{base}/v1/conversations/{gid}/members/me"))
            .header("authorization", authz(&a.access))
            .send(),
    );
    let transfer = r1.unwrap().status().as_u16();
    assert!(
        matches!(transfer, 204 | 403),
        "unexpected transfer status: {transfer}"
    );
    assert_eq!(r2.unwrap().status(), 204);

    // 终态不变量（不依赖竞态赢家）：恰好一个群主、群主属于剩余成员、a 已离群
    let d: Value = c
        .get(format!("{base}/v1/conversations/{gid}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let members = d["members"].as_array().unwrap();
    let owners: Vec<&Value> = members.iter().filter(|m| m["role"] == "owner").collect();
    assert_eq!(owners.len(), 1, "exactly one owner must remain");
    let owner_id = owners[0]["user"]["id"].as_str().unwrap();
    assert_eq!(
        d["owner_id"].as_str().unwrap(),
        owner_id,
        "conversations.owner_id must match the owner member"
    );
    assert!(members.iter().any(|m| m["user"]["id"] == b.id));
    assert!(members.iter().any(|m| m["user"]["id"] == cc.id));
    assert!(!members.iter().any(|m| m["user"]["id"] == a.id));
}

/// P1-7：解散与拉人并发——成员不得被插回已解散会话。
#[tokio::test]
async fn dissolve_invite_race() {
    let base = spawn().await;
    let a = register(&base, &format!("xa_{}", uniq())).await;
    let b = register(&base, &format!("xb_{}", uniq())).await;
    let cc = register(&base, &format!("xc_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    befriend(&base, &b, &cc).await; // b 与 c 是好友，b 才能拉 c
    let c = http();
    let detail: Value = c
        .post(format!("{base}/v1/conversations/group"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "race2", "member_ids": [b.id]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = detail["id"].as_str().unwrap().to_string();

    let (r1, r2) = tokio::join!(
        c.delete(format!("{base}/v1/conversations/{gid}"))
            .header("authorization", authz(&a.access))
            .send(),
        c.post(format!("{base}/v1/conversations/{gid}/members"))
            .header("authorization", authz(&b.access))
            .json(&json!({"user_ids": [cc.id]}))
            .send(),
    );
    assert_eq!(r1.unwrap().status(), 204);
    let invite = r2.unwrap().status().as_u16();
    assert!(
        matches!(invite, 204 | 400 | 403),
        "unexpected invite status: {invite}"
    );

    // 终态：会话已解散，且没有任何成员残留（含刚被拉进来的 c）
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let gid_uuid = uuid::Uuid::parse_str(&gid).unwrap();
    let dissolved: bool =
        sqlx::query_scalar("SELECT dissolved_at IS NOT NULL FROM conversations WHERE id = $1")
            .bind(gid_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(dissolved);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships WHERE conversation_id = $1")
        .bind(gid_uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// P1-8：禁用账号不能被任命为管理员；并发互相降权后系统仍剩可用管理员。
#[tokio::test]
async fn admin_role_safety() {
    let db_name = format!("chat_t_ar_{}", uniq());
    let (_guard, base) = spawn_named_db(&db_name).await;
    let c = http();
    let a = register(&base, &format!("aa_{}", uniq())).await; // admin
    let b = register(&base, &format!("bb_{}", uniq())).await;
    let cc = register(&base, &format!("cc_{}", uniq())).await;

    // 禁用 cc → 任命为 admin 必须被拒
    let r = c
        .post(format!("{base}/v1/admin/users/{}/disable", cc.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = c
        .post(format!("{base}/v1/admin/users/{}/role", cc.id))
        .header("authorization", authz(&a.access))
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = c
        .post(format!("{base}/v1/admin/users/{}/enable", cc.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // 提升 b 为管理员，然后 a、b 并发互相降权：必须恰好一个成功
    let r = c
        .post(format!("{base}/v1/admin/users/{}/role", b.id))
        .header("authorization", authz(&a.access))
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    let (r1, r2) = tokio::join!(
        c.post(format!("{base}/v1/admin/users/{}/role", a.id))
            .header("authorization", authz(&b.access))
            .json(&json!({"role": "user"}))
            .send(),
        c.post(format!("{base}/v1/admin/users/{}/role", b.id))
            .header("authorization", authz(&a.access))
            .json(&json!({"role": "user"}))
            .send(),
    );
    let codes = [r1.unwrap().status().as_u16(), r2.unwrap().status().as_u16()];
    assert!(
        codes.iter().all(|s| matches!(s, 204 | 400 | 403)),
        "unexpected demotion status: {codes:?}"
    );
    assert!(
        codes.iter().filter(|s| **s == 204).count() == 1,
        "double demote both succeeded: {codes:?}"
    );

    // 终态：恰好一名可用管理员（查该测试库，避免误数到共享库）
    let named_url = match base_db_url().rfind('/') {
        Some(i) => format!("{}/{db_name}", &base_db_url()[..i]),
        None => format!("postgres://chat:chat@localhost:15432/{db_name}"),
    };
    let pool = PgPoolOptions::new().connect(&named_url).await.unwrap();
    let admins: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE role = 'admin' AND disabled_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(admins, 1);
}

/// P2-1：中文验证消息按字符截断（>200 字 → 200 字），不能按字节 panic。
#[tokio::test]
async fn request_message_char_truncate() {
    let base = spawn().await;
    let a = register(&base, &format!("ta_{}", uniq())).await;
    let b = register(&base, &format!("tb_{}", uniq())).await;
    let c = http();

    let msg = "验证消息很长很长".repeat(30); // 210 个汉字
    let r = c
        .post(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&a.access))
        .json(&json!({"username": b.username, "message": msg}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let reqs: Value = c
        .get(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stored = reqs["incoming"][0]["message"].as_str().unwrap();
    assert_eq!(stored.chars().count(), 200);
}

/// P2-6：被拒后同方向不能再发申请；反方向申请仍然可以建立。
#[tokio::test]
async fn declined_request_blocks_same_direction() {
    let base = spawn().await;
    let a = register(&base, &format!("pa_{}", uniq())).await;
    let b = register(&base, &format!("pb_{}", uniq())).await;
    let c = http();

    let rid: String = c
        .post(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&a.access))
        .json(&json!({"username": b.username}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .post(format!("{base}/v1/friends/requests/{rid}/decline"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // 同方向重申 → 409
    let r = c
        .post(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&a.access))
        .json(&json!({"username": b.username}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);

    // 反方向（b → a）正常发起，a 接受 → 成为好友
    let r = c
        .post(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&b.access))
        .json(&json!({"username": a.username}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let reqs: Value = c
        .get(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rid2 = reqs["incoming"][0]["id"].as_str().unwrap().to_string();
    let r = c
        .post(format!("{base}/v1/friends/requests/{rid2}/accept"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// P2-5：删除好友后保留的只读 DM 不再暴露对方 last_seen_at。
#[tokio::test]
async fn unfriended_dm_hides_presence() {
    let base = spawn().await;
    let a = register(&base, &format!("ya_{}", uniq())).await;
    let b = register(&base, &format!("yb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let c = http();

    // b 先上线再下线，让 last_seen_at 有值
    {
        let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
            .await
            .unwrap();
        next_of(&mut sb, "presence_init").await;
        sb.close(None).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // 好友期间：a 的会话列表里 b 的 last_seen_at 可见
    let convs: Value = c
        .get(format!("{base}/v1/conversations"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dm = convs
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "dm")
        .unwrap();
    assert!(dm["peer"]["last_seen_at"].is_string() || dm["peer"]["last_seen_at"].is_null());

    // 删除好友 → 保留的 DM 里 last_seen_at 必须为 null
    let r = c
        .delete(format!("{base}/v1/friends/{}", b.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let convs: Value = c
        .get(format!("{base}/v1/conversations"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dm = convs
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "dm")
        .unwrap();
    assert!(dm["peer"]["last_seen_at"].is_null(), "last_seen leaked");
}

/// P2-3：WS sync 单页 500 条封顶，has_more=true 驱动客户端续页。
#[tokio::test]
async fn sync_paginates_beyond_500() {
    let base = spawn().await;
    let a = register(&base, &format!("sa_{}", uniq())).await;
    let b = register(&base, &format!("sb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;

    // 直接灌 505 条历史（绕过逐条 WS 发送的开销），保持 seq 连续
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let conv_uuid = uuid::Uuid::parse_str(&conv).unwrap();
    let a_uuid = uuid::Uuid::parse_str(&a.id).unwrap();
    sqlx::query(
        "INSERT INTO messages (conversation_id, seq, sender_id, content, kind)
         SELECT $1, g, $2, 'm' || g, 'text' FROM generate_series(1, 505) g",
    )
    .bind(conv_uuid)
    .bind(a_uuid)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE conversations SET last_seq = 505 WHERE id = $1")
        .bind(conv_uuid)
        .execute(&pool)
        .await
        .unwrap();

    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    sb.send(WsMsg::Text(
        json!({"type":"sync","conversation_id":conv,"last_seq":0})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let page1 = next_of(&mut sb, "sync_messages").await;
    assert_eq!(page1["messages"].as_array().unwrap().len(), 500);
    assert_eq!(page1["has_more"], true);

    // 续页：按上一页末尾 seq 继续
    sb.send(WsMsg::Text(
        json!({"type":"sync","conversation_id":conv,"last_seq":500})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let page2 = next_of(&mut sb, "sync_messages").await;
    assert_eq!(page2["messages"].as_array().unwrap().len(), 5);
    assert_eq!(page2["has_more"], false);
}

// ===== V2 审查修复（gpt-6-astar） =====

/// 1. 文件替换失败（新档里有 .backups 条目，挪不到位）：数据库不提交、原文件全部挪回，
///    .restore-old / .restore-new 不留；.restore-old 有遗留原文件时拒绝恢复且不碰它。
#[tokio::test]
async fn review_restore_swap_failure_keeps_files_and_db() {
    let (_guard, base, _st, files_dir) = spawn_backup_env("sf").await;
    let c = http();
    let admin = register(&base, &format!("rvsf_{}", uniq())).await;
    std::fs::write(files_dir.join("keep.bin"), b"before").unwrap();
    let name = create_and_wait_backup(&base, &admin.access).await;
    let bytes = download_backup(&base, &admin.access, &name).await;

    // 备份后的改动：恢复失败时必须原样保留
    let r = c
        .patch(format!("{base}/v1/users/me"))
        .header("authorization", authz(&admin.access))
        .json(&json!({"display_name": "changed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    std::fs::write(files_dir.join("after.bin"), b"after").unwrap();

    let mut entries = read_backup(&bytes);
    entries.insert("files/.backups/evil".into(), b"x".to_vec());
    let r = restore_upload(
        &base,
        &admin.access,
        write_backup(&entries),
        Some("RESTORE"),
    )
    .await;
    assert_eq!(r.status(), 500, "{}", r.text().await.unwrap());

    assert_eq!(
        std::fs::read(files_dir.join("keep.bin")).unwrap(),
        b"before"
    );
    assert_eq!(
        std::fs::read(files_dir.join("after.bin")).unwrap(),
        b"after"
    );
    assert!(!files_dir.join(".restore-old").exists());
    assert!(!files_dir.join(".restore-new").exists());
    assert!(files_dir.join(".backups").join(&name).exists());
    // 库没回滚到备份时刻、凭据没作废
    let me: Value = c
        .get(format!("{base}/v1/users/me"))
        .header("authorization", authz(&admin.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["display_name"], "changed");

    // 遗留的 .restore-old（上次撤回失败的原文件）：拒绝恢复，文件不动
    let old = files_dir.join(".restore-old");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("orphan.bin"), b"orig").unwrap();
    let r = restore_upload(&base, &admin.access, bytes, Some("RESTORE")).await;
    assert_eq!(r.status(), 500);
    assert_eq!(std::fs::read(old.join("orphan.bin")).unwrap(), b"orig");
    assert_eq!(
        std::fs::read(files_dir.join("after.bin")).unwrap(),
        b"after"
    );
}

/// 4. 空库并发注册只能产生一个管理员
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_register_race_single_admin() {
    let (_guard, base, st) = spawn_named_state(&format!("chat_it_rvreg_{}", uniq())).await;
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let base = base.clone();
            tokio::spawn(async move {
                http()
                    .post(format!("{base}/v1/auth/register"))
                    .json(
                        &json!({"username": format!("rv{i}_{}", uniq()), "password": "secret123"}),
                    )
                    .send()
                    .await
                    .unwrap()
                    .status()
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap(), 200);
    }
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(&st.pool)
        .await
        .unwrap();
    assert_eq!(admins, 1);
}

/// 5. 登出撤销会话后，该会话名下的 WS 立即收到 force_logout
#[tokio::test]
async fn review_logout_kicks_session_ws() {
    let base = spawn().await;
    let u = register(&base, &format!("rvlo_{}", uniq())).await;
    let dev = login_device(&base, &u.username, "secret123", "A").await;
    let (mut s, _) = tokio_tungstenite::connect_async(ws_url(&base, &dev).await)
        .await
        .unwrap();
    next_of(&mut s, "presence_init").await;
    let r = http()
        .post(format!("{base}/v1/auth/logout"))
        .json(&json!({"refresh_token": dev.refresh}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let v = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        next_of(&mut s, "force_logout"),
    )
    .await
    .expect("登出后该会话的 WS 应被踢");
    assert_eq!(v["reason"], "logout");
}

/// 6. 永不读取的连接：发送缓冲塞满后写超时/出站队列满即断开（心跳检查远在窗口外，不能靠它）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_ws_stuck_reader_is_dropped() {
    use std::sync::atomic::Ordering::Relaxed;
    let (_guard, base, st) = spawn_named_state(&format!("chat_it_rvws_{}", uniq())).await;
    st.ws_ping_interval_ms.store(60_000, Relaxed);
    st.ws_idle_timeout_ms.store(1000, Relaxed);
    let z = register(&base, &format!("rvws_{}", uniq())).await;
    let zid = uuid::Uuid::parse_str(&z.id).unwrap();
    let _zombie = tokio_tungstenite::connect_async(ws_url(&base, &z).await)
        .await
        .unwrap();
    for _ in 0..100 {
        if st.is_online(zid) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(st.is_online(zid));

    let big = "x".repeat(64 * 1024);
    let t0 = std::time::Instant::now();
    let mut dropped = false;
    while t0.elapsed() < std::time::Duration::from_secs(8) {
        st.send_to_user(zid, &big);
        if !st.is_online(zid) {
            dropped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert!(dropped, "读不动的连接应被断开");
}

/// 8. 并发换头像：输家只删自己的文件，赢家的头像始终可取
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_avatar_concurrent_upload_keeps_winner_file() {
    let (base, _files_dir) = spawn_avatar().await;
    let u = register(&base, &format!("rvav_{}", uniq())).await;
    for round in 0..8 {
        let (r1, r2) = tokio::join!(
            put_avatar(&base, &u.access, png_bytes(), "image/png"),
            put_avatar(&base, &u.access, png_bytes(), "image/png"),
        );
        for r in [r1.status(), r2.status()] {
            assert!(r == 200 || r == 409, "round {round}: {r}");
        }
        let me: Value = http()
            .get(format!("{base}/v1/users/me"))
            .header("authorization", authz(&u.access))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let r = http()
            .get(format!(
                "{base}/v1/users/{}/avatar?v={}",
                u.id, me["avatar_version"]
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "round {round}: 赢家的头像文件被删了");
    }
}

/// 自己发的消息不算未读：发言即把本人已读推进到这条，并给本人其他设备推 read_sync
#[tokio::test]
async fn review_own_messages_not_unread() {
    let base = spawn().await;
    let a = register(&base, &format!("rvun_{}", uniq())).await;
    let b = register(&base, &format!("rvuo_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;
    // A 的第二台设备
    let a2 = login_device(&base, &a.username, "secret123", "A2").await;
    let (mut sa2, _) = tokio_tungstenite::connect_async(ws_url(&base, &a2).await)
        .await
        .unwrap();
    next_of(&mut sa2, "presence_init").await;

    ws_send(
        &mut sb,
        &conv,
        json!({"client_msg_id": "b1", "content": "from b"}),
    )
    .await;
    for i in 0..3 {
        ws_send(
            &mut sa,
            &conv,
            json!({"client_msg_id": format!("a{i}"), "content": "from a"}),
        )
        .await;
    }
    let unread = |tok: String| {
        let base = base.clone();
        let conv = conv.clone();
        async move {
            let list: Value = http()
                .get(format!("{base}/v1/conversations"))
                .header("authorization", authz(&tok))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            list.as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == conv.as_str())
                .unwrap()["unread_count"]
                .as_i64()
                .unwrap()
        }
    };
    assert_eq!(
        unread(a.access.clone()).await,
        0,
        "自己发言后本会话不应有未读"
    );
    assert_eq!(unread(b.access.clone()).await, 3, "对方的未读不受影响");
    let rs = next_of(&mut sa2, "read_sync").await;
    assert_eq!(rs["conversation_id"], conv.as_str());
}

// ===== 未知 /v1 路径 =====
// 客户端比服务端新时调用了不存在的接口：回带 CORS 的 JSON 404，而不是落到静态托管的 405
#[tokio::test]
async fn unknown_api_is_json_404_with_cors() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let r = c
        .post(format!(
            "{base}/v1/conversations/{}/no-such-thing",
            uuid::Uuid::nil()
        ))
        .header("Origin", "http://tauri.localhost")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    assert!(r.headers().contains_key("access-control-allow-origin"));
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["error"], "unknown_api");

    let pre = c
        .request(reqwest::Method::OPTIONS, format!("{base}/v1/no-such-thing"))
        .header("Origin", "http://tauri.localhost")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .unwrap();
    assert!(pre.status().is_success(), "预检应放行: {}", pre.status());
    assert!(pre.headers().contains_key("access-control-allow-origin"));
}
