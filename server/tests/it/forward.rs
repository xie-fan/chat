use super::*;

// ===== V2 P2-4 =====
// 文件传输助手（type='self' 单人会话）+ 消息转发/收藏。

/// POST /v1/messages/{id}/forward
async fn post_forward(
    base: &str,
    u: &TestUser,
    msg_id: &str,
    targets: &[String],
    op: &str,
) -> reqwest::Response {
    http()
        .post(format!("{base}/v1/messages/{msg_id}/forward"))
        .header("authorization", authz(&u.access))
        .json(&json!({"conversation_ids": targets, "client_op_id": op}))
        .send()
        .await
        .unwrap()
}

/// 新注册：会话列表恰有一个 self 会话；后台统计不把 self 计入 dm/group。
/// 独立库：首个注册者即管理员，顺手校验 admin_overview 口径。
#[tokio::test]
async fn p24_self_conv_on_register() {
    let (_guard, base) = spawn_named_db(&format!("chat_it_p24a_{}", uniq())).await;
    let a = register(&base, &format!("p24a_{}", uniq())).await;
    let convs = conv_list(&base, &a).await;
    let selves: Vec<_> = convs
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "self")
        .collect();
    assert_eq!(selves.len(), 1, "每用户恰一个 self 会话");
    let sc = selves[0];
    assert!(sc["name"].is_null(), "self 会话 name 为 NULL");
    assert_eq!(sc["member_count"], 1);
    assert!(sc["peer"].is_null());

    // 后台统计：self 不计入 dm_count / group_count
    let ov: Value = http()
        .get(format!("{base}/v1/admin/overview"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ov["dm_count"], 0);
    assert_eq!(ov["group_count"], 0);
}

/// self 会话发消息：本人两条 WS 连接都实时收到（多设备同步）；
/// 他人无法访问该会话历史（403）。
#[tokio::test]
async fn p24_self_conv_sync_devices() {
    let base = spawn().await;
    let a = register(&base, &format!("p24b_{}", uniq())).await;
    let b = register(&base, &format!("p24c_{}", uniq())).await;
    let conv = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut s1, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    let (mut s2, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut s1, "presence_init").await;
    next_of(&mut s2, "presence_init").await;

    ws_send(
        &mut s1,
        &conv,
        json!({"client_msg_id": "self1", "content": "memo to self"}),
    )
    .await;
    // 本人第二条连接实时收到同一消息
    let m2 = next_of(&mut s2, "message").await;
    assert_eq!(m2["message"]["conversation_id"], conv);
    assert_eq!(m2["message"]["content"], "memo to self");

    // 非成员访问历史 → 403
    let r = http()
        .get(format!("{base}/v1/conversations/{conv}/messages"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

/// self 会话不可做：拉人/踢人/退出/改名/转让/解散/建投票 → 400
#[tokio::test]
async fn p24_self_conv_restrictions() {
    let base = spawn().await;
    let a = register(&base, &format!("p24d_{}", uniq())).await;
    let b = register(&base, &format!("p24e_{}", uniq())).await;
    // 先成为好友：拉人入口的好友校验通过后才能命中类型守卫 400
    befriend(&base, &a, &b).await;
    let conv = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = http();
    let az = authz(&a.access);

    let cases: Vec<(reqwest::Response, &str)> = vec![
        (
            c.post(format!("{base}/v1/conversations/{conv}/members"))
                .header("authorization", az.clone())
                .json(&json!({"user_ids": [b.id]}))
                .send()
                .await
                .unwrap(),
            "拉人",
        ),
        (
            c.delete(format!("{base}/v1/conversations/{conv}/members/{}", b.id))
                .header("authorization", az.clone())
                .send()
                .await
                .unwrap(),
            "踢人",
        ),
        (
            c.delete(format!("{base}/v1/conversations/{conv}/members/me"))
                .header("authorization", az.clone())
                .send()
                .await
                .unwrap(),
            "退出",
        ),
        (
            c.patch(format!("{base}/v1/conversations/{conv}"))
                .header("authorization", az.clone())
                .json(&json!({"name": "改名"}))
                .send()
                .await
                .unwrap(),
            "改名",
        ),
        (
            c.post(format!("{base}/v1/conversations/{conv}/transfer"))
                .header("authorization", az.clone())
                .json(&json!({"user_id": b.id}))
                .send()
                .await
                .unwrap(),
            "转让",
        ),
        (
            c.delete(format!("{base}/v1/conversations/{conv}"))
                .header("authorization", az.clone())
                .send()
                .await
                .unwrap(),
            "解散",
        ),
        (
            c.post(format!("{base}/v1/conversations/{conv}/polls"))
                .header("authorization", az.clone())
                .json(&json!({"question": "q?", "options": ["a", "b"]}))
                .send()
                .await
                .unwrap(),
            "建投票",
        ),
    ];
    for (r, name) in cases {
        assert_eq!(r.status(), 400, "self 会话{name}应 400");
    }
}

/// 转发文字到一个 DM 和一个群：forwarded_from 正确、目标 seq 各自递增、
/// 目标成员实时收到 message 帧。
#[tokio::test]
async fn p24_forward_text_to_dm_and_group() {
    let base = spawn().await;
    let a = register(&base, &format!("p24f_{}", uniq())).await;
    let b = register(&base, &format!("p24g_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let gid = mk_group(&base, &a, &[b.id.clone()]).await;

    // b 在 DM 发源消息（a 是成员可转发）。
    // 注意：ws_send 等待 message_ack 时会把先到的 message 广播帧一并消费，
    // 因此此处 sb 帧流是干净的，后续 next_of 只可能收到转发帧。
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;
    let view = ws_send(
        &mut sb,
        &dm,
        json!({"client_msg_id": "src1", "content": "link https://x"}),
    )
    .await;
    let src = view["id"].as_str().unwrap().to_string();
    let src_seq = view["seq"].as_i64().unwrap();

    // a 转发到 [群, DM]
    let r = post_forward(
        &base,
        &a,
        &src,
        &[gid.clone(), dm.clone()],
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    let g_res = results
        .iter()
        .find(|x| x["conversation_id"] == gid)
        .unwrap();
    let dm_res = results.iter().find(|x| x["conversation_id"] == dm).unwrap();
    assert_eq!(g_res["ok"], true, "{g_res}");
    assert_eq!(dm_res["ok"], true, "{dm_res}");
    // 转发来源 = 源发送者展示名
    assert_eq!(g_res["message"]["forwarded_from"], b.username);
    assert_eq!(dm_res["message"]["forwarded_from"], b.username);
    assert_eq!(g_res["message"]["content"], "link https://x");
    // seq 各自递增：DM 已有源消息；群在 UI3 group_created（seq 1）之后，转发为 seq 2
    assert_eq!(dm_res["message"]["seq"], src_seq + 1);
    assert_eq!(g_res["message"]["seq"], 2);

    // 目标成员 b 实时收到两条广播
    let f1 = next_of(&mut sb, "message").await;
    let f2 = next_of(&mut sb, "message").await;
    let got: std::collections::HashSet<String> = [f1, f2]
        .iter()
        .map(|f| {
            f["message"]["conversation_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(got.contains(&dm) && got.contains(&gid), "{got:?}");
}

/// 转发附件：目标会话克隆 attachment 行（新 id、共享物理文件）。
/// 源删除后目标仍可下载；副本删除后物理文件被清理（异步，轮询等待）。
#[tokio::test]
async fn p24_forward_attachment_lifecycle() {
    let base = spawn().await;
    let a = register(&base, &format!("p24h_{}", uniq())).await;
    let b = register(&base, &format!("p24i_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = http();

    // a 传图到 self 会话并发送
    let part = reqwest::multipart::Part::bytes(b"png-bytes-p24".to_vec())
        .file_name("pic.png".to_string())
        .mime_str("image/png")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("conversation_id", selfc.clone())
        .part("file", part);
    let r = c
        .post(format!("{base}/v1/files"))
        .header("authorization", authz(&a.access))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let att = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let view = ws_send(
        &mut sa,
        &selfc,
        json!({"client_msg_id": "img1", "content": "", "attachment_id": att}),
    )
    .await;
    let src = view["id"].as_str().unwrap().to_string();

    // 转发到 DM：新 attachment id，kind=image，uploader 保持原上传者
    let r = post_forward(
        &base,
        &a,
        &src,
        &[dm.clone()],
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let res = &body["results"][0];
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["message"]["kind"], "image");
    let fwd_id = res["message"]["id"].as_str().unwrap().to_string();
    let fwd_att = res["message"]["attachment"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(fwd_att, att, "克隆附件行得到新 id");

    // 目标附件可下载且内容一致
    let dl = c
        .get(format!("{base}/v1/files/{fwd_att}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.bytes().await.unwrap().as_ref(), b"png-bytes-p24");

    // 撤回源消息 → 目标副本仍被引用，下载不受影响（撤回只藏自己的附件行）
    let r = c
        .delete(format!("{base}/v1/messages/{src}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let dl = c
        .get(format!("{base}/v1/files/{fwd_att}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    // 源附件公开下载已 404（recalled_at 隐藏），物理文件因副本引用仍在
    let dl = c
        .get(format!("{base}/v1/files/{att}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 404);
    let path = std::env::temp_dir().join("chat-it-files").join(&att);
    assert!(path.exists(), "撤回保留期内物理文件不得删除");

    // 撤回目标副本 → 两行都过期清扫后，最后一个 storage_path 引用消失，物理文件删除
    let r = c
        .delete(format!("{base}/v1/messages/{fwd_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let swept = sweep_recalled(
        &pool,
        std::env::temp_dir().join("chat-it-files").to_str().unwrap(),
        &[
            uuid::Uuid::parse_str(&att).unwrap(),
            uuid::Uuid::parse_str(&fwd_att).unwrap(),
        ],
    )
    .await;
    assert!(swept >= 2, "两行撤回附件都应被清扫，got {swept}");
    assert!(!path.exists(), "孤儿物理文件必须被清理");
}

/// 逐目标独立：已删好友的 DM 返回 ok=false/error="not_friends"，其他目标照常成功。
#[tokio::test]
async fn p24_forward_partial_failure() {
    let base = spawn().await;
    let a = register(&base, &format!("p24j_{}", uniq())).await;
    let b = register(&base, &format!("p24k_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = http();

    // 删好友 → DM 只读；a 在 self 发源消息
    let r = c
        .delete(format!("{base}/v1/friends/{}", b.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let view = ws_send(
        &mut sa,
        &selfc,
        json!({"client_msg_id": "src2", "content": "partial test"}),
    )
    .await;
    let src = view["id"].as_str().unwrap().to_string();

    // 转发到 [只读 DM, 自己的 self]：一败一成，互不影响
    let r = post_forward(
        &base,
        &a,
        &src,
        &[dm.clone(), selfc.clone()],
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let dm_res = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["conversation_id"] == dm)
        .unwrap();
    let self_res = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["conversation_id"] == selfc)
        .unwrap();
    assert_eq!(dm_res["ok"], false);
    assert_eq!(dm_res["error"], "not_friends");
    assert_eq!(self_res["ok"], true, "{self_res}");
}

/// 同一 client_op_id 重试幂等：不产生重复消息，返回同一条；目标列表去重。
#[tokio::test]
async fn p24_forward_idempotent_retry() {
    let base = spawn().await;
    let a = register(&base, &format!("p24l_{}", uniq())).await;
    let b = register(&base, &format!("p24m_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let view = ws_send(
        &mut sa,
        &selfc,
        json!({"client_msg_id": "src3", "content": "idem body"}),
    )
    .await;
    let src = view["id"].as_str().unwrap().to_string();

    let op = uuid::Uuid::new_v4().to_string();
    // 目标列表带重复 id：服务端去重
    let r = post_forward(&base, &a, &src, &[dm.clone(), dm.clone()], &op).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "重复目标去重");
    let first_id = results[0]["message"]["id"].as_str().unwrap().to_string();

    // 同一 op 重试：返回同一条消息，不重复发送
    let r = post_forward(&base, &a, &src, &[dm.clone()], &op).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["results"][0]["ok"], true);
    assert_eq!(body["results"][0]["message"]["id"], first_id);

    let page: Value = c
        .get(format!("{base}/v1/conversations/{dm}/messages"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fwd: Vec<_> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["forwarded_from"].is_string())
        .collect();
    assert_eq!(fwd.len(), 1, "幂等重试不产生重复消息");
}

/// 拒绝路径：投票 400 / 已删除 400 / 非成员源消息 403 / 目标数量越界 400；
/// 普通 WS send_message 仍不能借用他人 attachment_id。
#[tokio::test]
async fn p24_forward_rejections() {
    let base = spawn().await;
    let a = register(&base, &format!("p24n_{}", uniq())).await;
    let b = register(&base, &format!("p24o_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let gid = mk_group(&base, &a, &[b.id.clone()]).await;
    let a_self = self_conv(&base, &a).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b_self = self_conv(&base, &b).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = http();
    let op = || uuid::Uuid::new_v4().to_string();

    // 1) 投票消息 → 400 投票不能转发
    let poll: Value = c
        .post(format!("{base}/v1/conversations/{gid}/polls"))
        .header("authorization", authz(&a.access))
        .json(&json!({"question": "q?", "options": ["x", "y"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = post_forward(
        &base,
        &a,
        poll["id"].as_str().unwrap(),
        &[dm.clone()],
        &op(),
    )
    .await;
    assert_eq!(r.status(), 400);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("投票"), "{err}");

    // 2) 已撤回消息 → 400 消息已撤回
    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let view = ws_send(
        &mut sa,
        &a_self,
        json!({"client_msg_id": "del1", "content": "soon deleted"}),
    )
    .await;
    let gone = view["id"].as_str().unwrap().to_string();
    let r = c
        .delete(format!("{base}/v1/messages/{gone}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = post_forward(&base, &a, &gone, &[dm.clone()], &op()).await;
    assert_eq!(r.status(), 400);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("已撤回"), "{err}");

    // 3) 转发他人 self 会话里的消息（a 非成员）→ 403
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;
    let view = ws_send(
        &mut sb,
        &b_self,
        json!({"client_msg_id": "bs1", "content": "private"}),
    )
    .await;
    let foreign = view["id"].as_str().unwrap().to_string();
    let r = post_forward(&base, &a, &foreign, &[dm.clone()], &op()).await;
    assert_eq!(r.status(), 403);

    // 4) 目标数量边界：空数组与 >10 都 400
    let r = post_forward(&base, &a, &foreign, &[], &op()).await;
    assert_eq!(r.status(), 400);
    let many: Vec<String> = (0..11).map(|_| dm.clone()).collect();
    let r = post_forward(&base, &a, &foreign, &many, &op()).await;
    assert_eq!(r.status(), 400);

    // 5) 普通 WS send_message 仍不能借用他人 attachment_id：
    //    b 传附件到自己的 self，a 拿去发到自己 self → not_found
    let part = reqwest::multipart::Part::bytes(b"other-bytes".to_vec())
        .file_name("other.txt".to_string())
        .mime_str("text/plain")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("conversation_id", b_self.clone())
        .part("file", part);
    let r = c
        .post(format!("{base}/v1/files"))
        .header("authorization", authz(&b.access))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let foreign_att = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":a_self,"client_msg_id":"steal1",
               "content":"","attachment_id":foreign_att})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "not_found");
}

/// UI2 / ADR-forward-keep-quote：转发保留 reply_to（可跨会话），预览仍展开一层。
#[tokio::test]
async fn ui2_forward_keeps_reply_to() {
    let base = spawn().await;
    let a = register(&base, &format!("ui2a_{}", uniq())).await;
    let b = register(&base, &format!("ui2b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let gid = mk_group(&base, &a, &[b.id.clone()]).await;

    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    // b 在 DM：原消息 → 回复；a 转发回复到群
    let orig = ws_send(
        &mut sb,
        &dm,
        json!({"client_msg_id": "ui2-orig", "content": "被引用的原文"}),
    )
    .await;
    let orig_id = orig["id"].as_str().unwrap().to_string();
    let reply = ws_send(
        &mut sb,
        &dm,
        json!({
            "client_msg_id": "ui2-reply",
            "content": "这是回复",
            "reply_to_id": orig_id,
        }),
    )
    .await;
    let reply_id = reply["id"].as_str().unwrap().to_string();
    assert_eq!(reply["reply_to_id"], orig_id);
    assert_eq!(reply["reply_to_preview"]["excerpt"], "被引用的原文");

    let r = post_forward(
        &base,
        &a,
        &reply_id,
        &[gid.clone()],
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let body: Value = r.json().await.unwrap();
    let res = body["results"].as_array().unwrap()[0].clone();
    assert_eq!(res["ok"], true, "{res}");
    let msg = &res["message"];
    assert_eq!(msg["reply_to_id"], orig_id, "转发须保留 reply_to_id");
    assert_eq!(msg["reply_to_preview"]["message_id"], orig_id);
    assert_eq!(msg["reply_to_preview"]["excerpt"], "被引用的原文");
    assert_eq!(msg["reply_to_preview"]["deleted"], false);
    assert_eq!(msg["content"], "这是回复");
    assert_eq!(msg["forwarded_from"], b.username);
}
