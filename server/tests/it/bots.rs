use super::*;

async fn create_bot(base: &str, u: &TestUser, name: &str, webhook: &str) -> Value {
    let r = http()
        .post(format!("{base}/v1/bots"))
        .header("authorization", authz(&u.access))
        .json(&json!({"username": name, "webhook_url": webhook}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    r.json().await.unwrap()
}

/// 加一个入站令牌，返回 (令牌 id, 令牌)
async fn add_token(base: &str, u: &TestUser, bot: &str, name: &str) -> (String, String) {
    let r = http()
        .post(format!("{base}/v1/bots/{bot}/tokens"))
        .header("authorization", authz(&u.access))
        .json(&json!({ "name": name }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    let t = v["tokens"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(t["name"], name);
    (
        t["id"].as_str().unwrap().to_string(),
        t["token"].as_str().unwrap().to_string(),
    )
}

async fn hook(base: &str, token: &str, body: Value) -> u16 {
    http()
        .post(format!("{base}/v1/hooks"))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 带 bot 令牌调 Bot API，返回 (状态码, JSON)
async fn bot_api(
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = http()
        .request(method, format!("{base}/v1{path}"))
        .header("authorization", format!("Bearer {token}"));
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// GET /bot/updates 的 events
async fn updates(base: &str, token: &str, query: &str) -> Vec<Value> {
    let (s, v) = bot_api(
        base,
        token,
        reqwest::Method::GET,
        &format!("/bot/updates{query}"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v["events"].as_array().unwrap().clone()
}

/// 带 bot 令牌传附件到 POST /bot/files，返回 (状态码, JSON)
async fn bot_upload(
    base: &str,
    token: &str,
    conv: &str,
    name: &str,
    mime: &str,
    body: Vec<u8>,
) -> (u16, Value) {
    let part = reqwest::multipart::Part::bytes(body)
        .file_name(name.to_string())
        .mime_str(mime)
        .unwrap();
    let r = http()
        .post(format!("{base}/v1/bot/files"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(
            reqwest::multipart::Form::new()
                .text("conversation_id", conv.to_string())
                .part("file", part),
        )
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn dm_with(convs: &Value, peer: &str) -> String {
    convs
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "dm" && c["peer"]["id"] == peer)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// 等会话里出现满足条件的消息（bot 回调是异步的）
async fn wait_message(
    base: &str,
    u: &TestUser,
    conv: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..50 {
        if let Some(m) = conv_messages(base, u, conv)
            .await
            .into_iter()
            .find(|m| pred(m))
        {
            return m;
        }
        tokio::time::sleep(Dur::from_millis(100)).await;
    }
    panic!("message never arrived");
}

/// 假 bot：记下收到的 (签名头, body)，回 {"content": "pong: <原文>"}
struct FakeBot {
    url: String,
    hits: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
}

async fn fake_bot() -> FakeBot {
    let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let h = hits.clone();
    let app = axum::Router::new().fallback(axum::routing::post(
        move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let h = h.clone();
            async move {
                let v: Value = serde_json::from_slice(&body).unwrap();
                let sig = headers
                    .get("x-chat-signature")
                    .and_then(|x| x.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let reply = format!("pong: {}", v["message"]["content"].as_str().unwrap());
                h.lock().unwrap().push((sig, v));
                axum::Json(json!({ "content": reply }))
            }
        },
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FakeBot { url, hits }
}

#[tokio::test]
async fn bot_lifecycle_and_inbound_hook() {
    let base = spawn().await;
    let owner = register(&base, &format!("bo{}", uniq())).await;
    let other = register(&base, &format!("bx{}", uniq())).await;
    let name = format!("bot{}", uniq());
    let bot = create_bot(&base, &owner, &name, "").await;
    let bot_id = bot["id"].as_str().unwrap();
    assert_eq!(bot["tokens"].as_array().unwrap().len(), 0);
    let (token_id, token) = add_token(&base, &owner, bot_id, "ci").await;

    // 同名再建 → 409
    let dup = http()
        .post(format!("{base}/v1/bots"))
        .header("authorization", authz(&owner.access))
        .json(&json!({"username": name}))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status().as_u16(), 409);

    // 主人的好友和私聊里都有它，带 is_bot
    let friends: Value = http()
        .get(format!("{base}/v1/friends"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let f = friends
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["user"]["id"] == bot_id)
        .unwrap();
    assert_eq!(f["user"]["is_bot"], true);
    let dm = dm_with(&conv_list(&base, &owner).await, bot_id);

    // 入站：默认发到和主人的私聊
    assert_eq!(
        hook(&base, &token, json!({"content": "build ok"})).await,
        200
    );
    let m = wait_message(&base, &owner, &dm, |m| m["content"] == "build ok").await;
    assert_eq!(m["sender_id"], bot_id);

    // 带 client_msg_id 重试：返回同一条消息，不重复发
    let send_idem = || async {
        http()
            .post(format!("{base}/v1/hooks"))
            .header("authorization", format!("Bearer {token}"))
            .json(&json!({"content": "deploy done", "client_msg_id": "deploy-1"}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };
    let first = send_idem().await;
    let again = send_idem().await;
    assert_eq!(first["id"], again["id"]);
    assert_eq!(first["seq"], again["seq"]);

    assert_eq!(hook(&base, "nope", json!({"content": "x"})).await, 401);
    let no_auth = http()
        .post(format!("{base}/v1/hooks"))
        .json(&json!({"content": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_auth.status().as_u16(), 401);

    // 别人搜不到、加不了；bot 也登录不了
    let found: Value = http()
        .get(format!("{base}/v1/users/search?q={name}"))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found.as_array().unwrap().len(), 0);
    let req = http()
        .post(format!("{base}/v1/friends/requests"))
        .header("authorization", authz(&other.access))
        .json(&json!({"username": name, "message": ""}))
        .send()
        .await
        .unwrap();
    assert_eq!(req.status().as_u16(), 404);
    let login = http()
        .post(format!("{base}/v1/auth/login"))
        .json(&json!({"username": name, "password": "!"}))
        .send()
        .await
        .unwrap();
    assert!(login.status().is_client_error());

    // 不是成员的会话发不进去
    let foreign = self_conv(&base, &other).await["id"].clone();
    assert_eq!(
        hook(
            &base,
            &token,
            json!({"content": "x", "conversation_id": foreign})
        )
        .await,
        403
    );

    // 多个令牌各自可用；删掉一个只影响它自己
    let (_, token2) = add_token(&base, &owner, bot_id, "cron").await;
    assert_eq!(
        hook(&base, &token2, json!({"content": "from cron"})).await,
        200
    );
    let r = http()
        .delete(format!("{base}/v1/bots/{bot_id}/tokens/{token_id}"))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404, "only the owner can delete tokens");
    let after: Value = http()
        .delete(format!("{base}/v1/bots/{bot_id}/tokens/{token_id}"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["tokens"].as_array().unwrap().len(), 1);
    assert_eq!(hook(&base, &token, json!({"content": "x"})).await, 401);
    assert_eq!(
        hook(&base, &token2, json!({"content": "still ok"})).await,
        200
    );

    // 换签名密钥
    let reset: Value = http()
        .post(format!("{base}/v1/bots/{bot_id}/secret"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(reset["secret"], bot["secret"]);

    // 别人删不了；主人删掉后入站失效、私聊只读
    let r = http()
        .delete(format!("{base}/v1/bots/{bot_id}"))
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let r = http()
        .delete(format!("{base}/v1/bots/{bot_id}"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    assert_eq!(hook(&base, &token2, json!({"content": "x"})).await, 401);
    let list: Value = http()
        .get(format!("{base}/v1/bots"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn bot_webhook_replies_in_dm_and_when_mentioned_in_group() {
    let base = spawn().await;
    let fake = fake_bot().await;
    let owner = register(&base, &format!("wo{}", uniq())).await;
    let b = register(&base, &format!("wb{}", uniq())).await;
    befriend(&base, &owner, &b).await;
    let name = format!("echo{}", uniq());
    let bot = create_bot(&base, &owner, &name, &fake.url).await;
    let bot_id = bot["id"].as_str().unwrap().to_string();
    let dm = dm_with(&conv_list(&base, &owner).await, &bot_id);

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &owner).await)
        .await
        .unwrap();
    ws_send(
        &mut ws,
        &dm,
        json!({"client_msg_id": "c1", "content": "ping"}),
    )
    .await;
    let reply = wait_message(&base, &owner, &dm, |m| m["content"] == "pong: ping").await;
    assert_eq!(reply["sender_id"], bot_id.as_str());
    {
        let hits = fake.hits.lock().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.starts_with("sha256=") && hits[0].0.len() == 7 + 64);
        assert_eq!(hits[0].1["conversation_type"], "dm");
        assert_eq!(hits[0].1["message"]["sender_id"], owner.id.as_str());
    }

    // 群里：没 @ 不回调，@ 了才回调；bot 自己的回复不再触发回调
    let group = mk_group(&base, &owner, &[b.id.clone(), bot_id.clone()]).await;
    let (mut wb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g1", "content": "hi all"}),
    )
    .await;
    let mention = format!("@{name} status?");
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g2", "content": mention}),
    )
    .await;
    let want = format!("pong: {mention}");
    wait_message(&base, &b, &group, |m| m["content"] == want.as_str()).await;
    tokio::time::sleep(Dur::from_millis(300)).await;
    let hits = fake.hits.lock().unwrap();
    assert_eq!(hits.len(), 2, "only the DM and the @ trigger the webhook");
    assert_eq!(hits[1].1["conversation_type"], "group");
}

#[tokio::test]
async fn group_with_only_bots_dissolves_when_last_human_leaves() {
    let base = spawn().await;
    let owner = register(&base, &format!("lo{}", uniq())).await;
    let bot = create_bot(&base, &owner, &format!("lb{}", uniq()), "").await;
    let bot_id = bot["id"].as_str().unwrap().to_string();
    let group = mk_group(&base, &owner, &[bot_id.clone()]).await;

    // 不能把群主转给 bot
    let r = http()
        .post(format!("{base}/v1/conversations/{group}/transfer"))
        .header("authorization", authz(&owner.access))
        .json(&json!({"user_id": bot_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    let r = http()
        .delete(format!("{base}/v1/conversations/{group}/members/me"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    let (_, token) = add_token(&base, &owner, &bot_id, "t").await;
    assert_eq!(
        hook(
            &base,
            &token,
            json!({"content": "x", "conversation_id": group})
        )
        .await,
        403,
        "group is gone, not handed to the bot"
    );
}

#[tokio::test]
async fn bot_api_pulls_updates_and_reads_history() {
    use reqwest::Method;
    let base = spawn().await;
    let owner = register(&base, &format!("uo{}", uniq())).await;
    let b = register(&base, &format!("ub{}", uniq())).await;
    befriend(&base, &owner, &b).await;
    let name = format!("ag{}", uniq());
    let bot = create_bot(&base, &owner, &name, "").await;
    let bot_id = bot["id"].as_str().unwrap().to_string();
    let (_, token) = add_token(&base, &owner, &bot_id, "agent").await;
    let dm = dm_with(&conv_list(&base, &owner).await, &bot_id);

    let (s, me) = bot_api(&base, &token, Method::GET, "/bot/me", None).await;
    assert_eq!(s, 200);
    assert_eq!(me["id"], bot_id.as_str());
    assert_eq!(me["is_bot"], true);
    // bot 令牌只认 Bot API，用户接口不认
    let r = http()
        .get(format!("{base}/v1/conversations"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);

    // 第一次拉取之前发的也拉得到；取到即交付，第二次是空的
    let (mut wo, _) = tokio_tungstenite::connect_async(ws_url(&base, &owner).await)
        .await
        .unwrap();
    ws_send(
        &mut wo,
        &dm,
        json!({"client_msg_id": "u1", "content": "first"}),
    )
    .await;
    let ev = updates(&base, &token, "").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["bot_id"], bot_id.as_str());
    assert_eq!(ev[0]["conversation_type"], "dm");
    assert_eq!(ev[0]["message"]["content"], "first");
    assert!(updates(&base, &token, "").await.is_empty());

    // bot 发言不会把还没拉的消息跳过（交付游标和已读位置分开）
    ws_send(
        &mut wo,
        &dm,
        json!({"client_msg_id": "u2", "content": "second"}),
    )
    .await;
    let (s, _) = bot_api(
        &base,
        &token,
        Method::POST,
        "/hooks",
        Some(json!({"content": "on it"})),
    )
    .await;
    assert_eq!(s, 200);
    let ev = updates(&base, &token, "").await;
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["message"]["content"], "second");

    // 长轮询：等着的时候来了消息立刻返回
    let waiting = tokio::spawn({
        let (base, token) = (base.clone(), token.clone());
        async move { updates(&base, &token, "?timeout=20").await }
    });
    tokio::time::sleep(Dur::from_millis(300)).await;
    let sent_at = std::time::Instant::now();
    ws_send(
        &mut wo,
        &dm,
        json!({"client_msg_id": "u3", "content": "third"}),
    )
    .await;
    let ev = waiting.await.unwrap();
    assert!(
        sent_at.elapsed() < Dur::from_secs(5),
        "long poll should wake up"
    );
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["message"]["content"], "third");

    // 群里：没 @ 它的不算，@ 了才算
    let group = mk_group(&base, &owner, &[b.id.clone(), bot_id.clone()]).await;
    let (mut wb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    let mention = format!("@{name} status?");
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g1", "content": "hi all"}),
    )
    .await;
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g2", "content": mention}),
    )
    .await;
    let ev = updates(&base, &token, "").await;
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["conversation_type"], "group");
    assert_eq!(ev[0]["message"]["content"], mention.as_str());

    // 会话、成员、历史：群里没 @ 它的也读得到；不是成员的会话读不到
    let (s, convs) = bot_api(&base, &token, Method::GET, "/bot/conversations", None).await;
    assert_eq!(s, 200);
    let ids: Vec<&str> = convs
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&dm.as_str()) && ids.contains(&group.as_str()),
        "{convs}"
    );
    let (s, detail) = bot_api(
        &base,
        &token,
        Method::GET,
        &format!("/bot/conversations/{group}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(detail["members"].as_array().unwrap().len(), 3);
    let (s, page) = bot_api(
        &base,
        &token,
        Method::GET,
        &format!("/bot/conversations/{group}/messages"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let contents: Vec<&str> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert!(
        contents.contains(&"hi all") && contents.contains(&mention.as_str()),
        "{page}"
    );
    let foreign = self_conv(&base, &b).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = bot_api(
        &base,
        &token,
        Method::GET,
        &format!("/bot/conversations/{foreign}/messages"),
        None,
    )
    .await;
    assert_eq!(s, 403);

    // 踢出再拉回群：离开前没拉的 @ 不再交付，只交付回来之后的
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g3", "content": format!("@{name} old")}),
    )
    .await;
    let r = http()
        .delete(format!("{base}/v1/conversations/{group}/members/{bot_id}"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    let r = http()
        .post(format!("{base}/v1/conversations/{group}/members"))
        .header("authorization", authz(&owner.access))
        .json(&json!({ "user_ids": [bot_id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "g4", "content": format!("@{name} new")}),
    )
    .await;
    let ev = updates(&base, &token, "").await;
    let got: Vec<&str> = ev
        .iter()
        .map(|e| e["message"]["content"].as_str().unwrap())
        .collect();
    assert_eq!(got, [format!("@{name} new")], "{ev:?}");
}

#[tokio::test]
async fn bot_api_sends_files_edits_and_recalls() {
    use reqwest::Method;
    let base = spawn().await;
    let owner = register(&base, &format!("fo{}", uniq())).await;
    let bot = create_bot(&base, &owner, &format!("fb{}", uniq()), "").await;
    let bot_id = bot["id"].as_str().unwrap().to_string();
    let (_, token) = add_token(&base, &owner, &bot_id, "agent").await;
    let dm = dm_with(&conv_list(&base, &owner).await, &bot_id);

    // 先上传到会话，再带 attachment_id 发出（不写正文也行）
    let (s, att) = bot_upload(
        &base,
        &token,
        &dm,
        "chart.png",
        "image/png",
        png_fixture(4, 4),
    )
    .await;
    assert_eq!(s, 200);
    let (s, m) = bot_api(
        &base,
        &token,
        Method::POST,
        "/hooks",
        Some(json!({"attachment_id": att["id"]})),
    )
    .await;
    assert_eq!(s, 200, "{m}");
    assert_eq!(m["kind"], "image");
    assert_eq!(m["attachment"]["id"], att["id"]);
    assert_eq!(m["sender_id"], bot_id.as_str());

    // 先发「处理中」再改成结果；版本对不上 409
    let (_, m) = bot_api(
        &base,
        &token,
        Method::POST,
        "/hooks",
        Some(json!({"content": "处理中…"})),
    )
    .await;
    let id = m["id"].as_str().unwrap().to_string();
    assert_eq!(m["edit_version"], 0);
    let path = format!("/bot/messages/{id}");
    let stale = json!({"content": "x", "expected_version": 5});
    assert_eq!(
        bot_api(&base, &token, Method::PATCH, &path, Some(stale))
            .await
            .0,
        409
    );
    let done = json!({"content": "完成：3 个文件", "expected_version": 0});
    let (s, edited) = bot_api(&base, &token, Method::PATCH, &path, Some(done)).await;
    assert_eq!(s, 200);
    assert_eq!(edited["content"], "完成：3 个文件");
    assert_eq!(edited["edit_version"], 1);
    let (s, recalled) = bot_api(
        &base,
        &token,
        Method::POST,
        &format!("/bot/messages/{id}/recall"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(recalled["recalled"], true);

    // 回复主人的某条消息；主人的消息改不了、撤不了
    let (mut wo, _) = tokio_tungstenite::connect_async(ws_url(&base, &owner).await)
        .await
        .unwrap();
    let mine = ws_send(
        &mut wo,
        &dm,
        json!({"client_msg_id": "o1", "content": "owner says"}),
    )
    .await;
    let oid = mine["id"].as_str().unwrap();
    let (s, reply) = bot_api(
        &base,
        &token,
        Method::POST,
        "/hooks",
        Some(json!({"content": "收到", "reply_to_id": oid})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(reply["reply_to_id"], oid);
    let (s, _) = bot_api(
        &base,
        &token,
        Method::PATCH,
        &format!("/bot/messages/{oid}"),
        Some(json!({"content": "hacked", "expected_version": 0})),
    )
    .await;
    assert_eq!(s, 403);
    let (s, _) = bot_api(
        &base,
        &token,
        Method::POST,
        &format!("/bot/messages/{oid}/recall"),
        None,
    )
    .await;
    assert_eq!(s, 403);
}

/// GET /bot/openapi.json（不带令牌）
async fn openapi(base: &str) -> Value {
    let r = http()
        .get(format!("{base}/v1/bot/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    r.json().await.unwrap()
}

/// OpenAPI 描述里的每个接口都真实存在，而且只认 bot 令牌（乱写的令牌、用户的 access token 一律 401）；
/// servers 用后台配的 public_url，清空后按请求的 Host
#[tokio::test]
async fn bot_openapi_lists_guarded_routes() {
    let (_guard, base) = spawn_named_db(&format!("chat_it_botoas_{}", uniq())).await;
    // 空库第一个注册的自动成为管理员
    let user = register(&base, &format!("oa{}", uniq())).await;
    let set = |url: &str| admin_put_settings(&base, &user, json!({ "public_url": url }));
    assert_eq!(set("https://chat.example.com/").await.status(), 200);
    assert_eq!(
        openapi(&base).await["servers"][0]["url"],
        "https://chat.example.com/v1"
    );
    assert_eq!(set("").await.status(), 200);
    let spec = openapi(&base).await;
    assert_eq!(spec["servers"][0]["url"], format!("{base}/v1"));
    let mut checked = 0;
    for (path, ops) in spec["paths"].as_object().unwrap() {
        for (method, op) in ops.as_object().unwrap() {
            if op["security"] == json!([]) {
                continue; // 公开的附件下载
            }
            let url = format!(
                "{base}/v1{}",
                path.replace("{id}", &uuid::Uuid::new_v4().to_string())
            );
            for token in ["nope", user.access.as_str()] {
                let r = http()
                    .request(method.to_uppercase().parse().unwrap(), &url)
                    .header("authorization", authz(token))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(r.status().as_u16(), 401, "{method} {path}");
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
}

/// bot 上传的算进主人的配额（主人和他的 bot 共用一份）；
/// 主人被禁用期间 bot 令牌 403、@ 它也不回调，启用后恢复
#[tokio::test]
async fn bot_shares_owner_quota_and_pauses_while_owner_disabled() {
    let (_guard, base) = spawn_named_db(&format!("chat_it_botown_{}", uniq())).await;
    // 空库第一个注册的自动成为管理员
    let admin = register(&base, &format!("ba{}", uniq())).await;
    let owner = register(&base, &format!("bo{}", uniq())).await;
    let b = register(&base, &format!("bb{}", uniq())).await;
    befriend(&base, &owner, &b).await;
    let fake = fake_bot().await;
    let name = format!("bq{}", uniq());
    let bot = create_bot(&base, &owner, &name, &fake.url).await;
    let bot_id = bot["id"].as_str().unwrap().to_string();
    let (_, token) = add_token(&base, &owner, &bot_id, "agent").await;
    let dm = dm_with(&conv_list(&base, &owner).await, &bot_id);
    let group = mk_group(&base, &owner, &[b.id.clone(), bot_id.clone()]).await;

    // 配额 1 MB：主人传 600 KB、bot 传 300 KB 都行，bot 再传 300 KB 就超了
    let r = admin_put_settings(&base, &admin, json!({"storage_quota_mb": 1})).await;
    assert_eq!(r.status(), 200);
    let bin = "application/octet-stream";
    let kb = |n: usize| vec![0u8; n * 1024];
    let (s, _) = upload_att(&base, &dm, &owner.access, "a.bin", bin, kb(600)).await;
    assert_eq!(s, 200);
    let (s, _) = bot_upload(&base, &token, &dm, "b.bin", bin, kb(300)).await;
    assert_eq!(s, 200);
    let (s, _) = bot_upload(&base, &token, &dm, "c.bin", bin, kb(300)).await;
    assert_eq!(s, 413, "bot uploads share the owner's quota");

    // 主人「我」页和后台用户列表的已用空间都算上 bot 传的；bot 那行是 0
    let mine: Value = http()
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["used_bytes"], 900 * 1024);
    let users: Value = http()
        .get(format!("{base}/v1/admin/users"))
        .header("authorization", authz(&admin.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let used = |id: &str| {
        users
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["id"] == id)
            .unwrap()["storage_bytes"]
            .clone()
    };
    assert_eq!(used(&owner.id), 900 * 1024);
    assert_eq!(used(&bot_id), 0);

    // 管理员禁用主人：bot 令牌 403，群里 @ 它也不回调
    let admin_post = |action: String| {
        http()
            .post(format!("{base}/v1/admin/users/{}/{action}", owner.id))
            .header("authorization", authz(&admin.access))
            .send()
    };
    assert_eq!(admin_post("disable".into()).await.unwrap().status(), 204);
    assert_eq!(hook(&base, &token, json!({"content": "x"})).await, 403);
    let (s, _) = bot_api(&base, &token, reqwest::Method::GET, "/bot/updates", None).await;
    assert_eq!(s, 403);
    let (mut wb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    let paused = format!("@{name} while disabled");
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "d1", "content": paused}),
    )
    .await;
    tokio::time::sleep(Dur::from_millis(300)).await;

    // 启用后恢复：令牌能用，@ 它又回调
    assert_eq!(admin_post("enable".into()).await.unwrap().status(), 204);
    assert_eq!(hook(&base, &token, json!({"content": "back"})).await, 200);
    let again = format!("@{name} after enable");
    ws_send(
        &mut wb,
        &group,
        json!({"client_msg_id": "d2", "content": again}),
    )
    .await;
    let want = format!("pong: {again}");
    wait_message(&base, &b, &group, |m| m["content"] == want.as_str()).await;
    let hits = fake.hits.lock().unwrap();
    assert_eq!(hits.len(), 1, "no callback while the owner was disabled");
    assert_eq!(hits[0].1["message"]["content"], again.as_str());
}
