use super::*;

// ===== MF1 =====
// 批量转发（merged/separate）、chat_record 消息、记录内附件转发、撤回级联、
// 转发不解析 @、克隆附件归转发者。

/// POST /v1/messages/forward-batch
pub(crate) async fn post_batch(
    base: &str,
    u: &TestUser,
    conv: &str,
    ids: &[String],
    targets: &[String],
    mode: &str,
    op: &str,
) -> reqwest::Response {
    http()
        .post(format!("{base}/v1/messages/forward-batch"))
        .header("authorization", authz(&u.access))
        .json(&json!({
            "conversation_id": conv,
            "message_ids": ids,
            "targets": targets,
            "mode": mode,
            "client_op_id": op,
        }))
        .send()
        .await
        .unwrap()
}

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

async fn get_record(base: &str, u: &TestUser, msg_id: &str) -> reqwest::Response {
    http()
        .get(format!("{base}/v1/messages/{msg_id}/record"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
}

/// ws 里发一条文本消息，返回消息 id
async fn send_text<S>(s: &mut S, conv: &str, key: &str, content: &str) -> String
where
    S: StreamExt<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<WsMsg, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    ws_send(s, conv, json!({"client_msg_id": key, "content": content}))
        .await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

type WsConn = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn ws_conn(base: &str, u: &TestUser) -> WsConn {
    let (mut s, _) = tokio_tungstenite::connect_async(ws_url(base, u).await)
        .await
        .unwrap();
    next_of(&mut s, "presence_init").await;
    s
}

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new().connect(&test_db_url()).await.unwrap()
}

/// 合并转发的三种标题：DM 「A 和 B 的聊天记录」、self 「A 的聊天记录」、
/// 群聊恒为「群聊的聊天记录」（不含群名）。快照 source_kind/count/items 齐全。
#[tokio::test]
async fn mf1_merged_titles() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1ta_{}", uniq())).await;
    let b = register(&base, &format!("mf1tb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let gid = mk_group(&base, &a, std::slice::from_ref(&b.id)).await;

    let mut sa = ws_conn(&base, &a).await;
    let dm_msg = send_text(&mut sa, &dm, "t1", "dm hello").await;
    let self_msg = send_text(&mut sa, &selfc, "t2", "self note").await;
    let g_msg = send_text(&mut sa, &gid, "t3", "group hi").await;

    // DM → self：标题 = 双方展示名
    let r = post_batch(
        &base,
        &a,
        &dm,
        &[dm_msg],
        std::slice::from_ref(&selfc),
        "merged",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let rec = &body["results"][0]["message_ids"][0];
    let rec = get_record(&base, &a, rec.as_str().unwrap())
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(rec["v"], 1);
    assert_eq!(rec["title"], format!("{} 和 {} 的聊天记录", a.username, b.username));
    assert_eq!(rec["source_kind"], "dm");
    assert_eq!(rec["count"], 1);
    assert_eq!(rec["items"][0]["kind"], "text");
    assert_eq!(rec["items"][0]["content"], "dm hello");
    assert_eq!(rec["items"][0]["sender_name"], a.username);

    // self → self：「A 的聊天记录」
    let r = post_batch(
        &base,
        &a,
        &selfc,
        &[self_msg],
        std::slice::from_ref(&selfc),
        "merged",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let body: Value = r.json().await.unwrap();
    let rec = get_record(
        &base,
        &a,
        body["results"][0]["message_ids"][0].as_str().unwrap(),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(rec["title"], format!("{} 的聊天记录", a.username));
    assert_eq!(rec["source_kind"], "self");

    // 群 → self：固定「群聊的聊天记录」，不带群名
    let r = post_batch(
        &base,
        &a,
        &gid,
        &[g_msg],
        std::slice::from_ref(&selfc),
        "merged",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let body: Value = r.json().await.unwrap();
    let rec = get_record(
        &base,
        &a,
        body["results"][0]["message_ids"][0].as_str().unwrap(),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(rec["title"], "群聊的聊天记录");
    assert_eq!(rec["source_kind"], "group");
    // 目标消息的 kind / content 兜底行
    let page = conv_messages(&base, &a, &selfc).await;
    let m = page
        .iter()
        .find(|m| m["kind"] == "chat_record" && m["content"].as_str().unwrap_or("").contains("群聊"))
        .expect("record message in self conv");
    assert_eq!(
        m["content"].as_str().unwrap().lines().next().unwrap(),
        "[聊天记录] 群聊的聊天记录"
    );
    assert!(m["forwarded_from"].is_null(), "记录消息不带转发署名");
}

/// seq 排序：请求乱序给出 id，结果与源会话 seq 升序一致；
/// 跳过分类：不存在/别的会话的 id 计 unavailable，撤回计 recalled，投票计 poll。
#[tokio::test]
async fn mf1_batch_seq_order_and_skips() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1sa_{}", uniq())).await;
    let b = register(&base, &format!("mf1sb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let gid = mk_group(&base, &a, std::slice::from_ref(&b.id)).await;
    let c = http();

    let mut sa = ws_conn(&base, &a).await;
    let m1 = send_text(&mut sa, &dm, "s1", "first").await;
    let m2 = send_text(&mut sa, &dm, "s2", "second").await;
    let m3 = send_text(&mut sa, &dm, "s3", "third").await;
    // 撤回 m3 → recalled；建一个投票 → poll；随机 uuid + 别的会话消息 → unavailable
    let r = c
        .delete(format!("{base}/v1/messages/{m3}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
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
    let foreign = send_text(&mut sa, &selfc, "s4", "not in dm").await;
    let random = uuid::Uuid::new_v4().to_string();

    // 乱序请求：separate 结果按 seq 升序
    let r = post_batch(
        &base,
        &a,
        &dm,
        &[m3.clone(), m2.clone(), random.clone(), m1.clone(), foreign.clone()],
        std::slice::from_ref(&gid),
        "separate",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let t = &body["results"][0];
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["forwarded"], 2);
    // skipped：recalled 1（m3）+ unavailable 2（random + foreign；foreign 属本会话外）
    let sk = &body["skipped"];
    assert_eq!(sk["recalled"], 1, "{sk}");
    assert_eq!(sk["unavailable"], 2, "{sk}");
    assert!(sk.get("poll").is_none(), "0 计数的键不出现: {sk}");
    // 逐目标结果里带同一份 skipped
    assert_eq!(t["skipped"], *sk);
    // 落库消息按 seq 序：first 在前 second 在后
    let ids = t["message_ids"].as_array().unwrap();
    let page = conv_messages(&base, &a, &gid).await;
    let texts: Vec<&str> = page
        .iter()
        .filter(|m| ids.iter().any(|i| *i == m["id"]))
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["first", "second"]);

    // merged 同样计入 skipped；投票在群里 → poll（源里加一条活消息保住请求）
    let g_ok = send_text(&mut sa, &gid, "s5", "group ok").await;
    let r = post_batch(
        &base,
        &a,
        &gid,
        &[poll["id"].as_str().unwrap().to_string(), m1.clone(), g_ok],
        std::slice::from_ref(&selfc),
        "merged",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    // m1 属于 dm 不属于 gid → unavailable；poll 属 gid → poll；g_ok 入选
    assert_eq!(body["skipped"]["unavailable"], 1, "{body}");
    assert_eq!(body["skipped"]["poll"], 1, "{body}");
    assert_eq!(body["results"][0]["forwarded"], 1);
    let rec = get_record(
        &base,
        &a,
        body["results"][0]["message_ids"][0].as_str().unwrap(),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(rec["count"], 1);
    assert_eq!(rec["items"][0]["content"], "group ok");
}

/// 上限与形状：101 ids / 11 targets / 空 ids / 非法 mode / 全部跳过 → 400。
/// 403 同形：非成员源会话与不存在的会话返回逐字节相同的响应体。
#[tokio::test]
async fn mf1_batch_limits_and_403_shape() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1la_{}", uniq())).await;
    let b = register(&base, &format!("mf1lb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let b_self = self_conv(&base, &b).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let m1 = send_text(&mut sa, &dm, "l1", "x").await;
    let op = || uuid::Uuid::new_v4().to_string();

    // 101 个 id → 400；11 个目标 → 400；空 ids → 400；空 targets → 400；非法 mode → 400
    let ids101: Vec<String> = (0..101).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let r = post_batch(&base, &a, &dm, &ids101, std::slice::from_ref(&selfc), "merged", &op()).await;
    assert_eq!(r.status(), 400, "101 ids");
    let t11: Vec<String> = (0..11).map(|_| selfc.clone()).collect();
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m1), &t11, "merged", &op()).await;
    assert_eq!(r.status(), 400, "11 targets");
    let r = post_batch(&base, &a, &dm, &[], std::slice::from_ref(&selfc), "merged", &op()).await;
    assert_eq!(r.status(), 400, "empty ids");
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m1), &[], "merged", &op()).await;
    assert_eq!(r.status(), 400, "empty targets");
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m1), std::slice::from_ref(&selfc), "weird", &op()).await;
    assert_eq!(r.status(), 400, "bad mode");

    // 全部不可用 → 400 没有可转发的消息
    let r = post_batch(
        &base,
        &a,
        &dm,
        &[uuid::Uuid::new_v4().to_string()],
        std::slice::from_ref(&selfc),
        "merged",
        &op(),
    )
    .await;
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
        "没有可转发的消息"
    );

    // 403 同形：不存在的会话 vs 非成员会话（b 的 self）
    let gone = post_batch(
        &base,
        &a,
        &uuid::Uuid::new_v4().to_string(),
        std::slice::from_ref(&m1),
        std::slice::from_ref(&selfc),
        "merged",
        &op(),
    )
    .await;
    let foreign = post_batch(
        &base,
        &a,
        &b_self,
        std::slice::from_ref(&m1),
        std::slice::from_ref(&selfc),
        "merged",
        &op(),
    )
    .await;
    assert_eq!(gone.status(), 403);
    assert_eq!(foreign.status(), 403);
    let g = gone.text().await.unwrap();
    let f = foreign.text().await.unwrap();
    assert_eq!(g, f, "不存在与非成员会话必须逐字节同形");

    // GET record 的 403 同形：不存在的消息 vs 无权消息（b 的 self 里的一条）
    let mut sb = ws_conn(&base, &b).await;
    let msg = send_text(&mut sb, &b_self, "l2", "b private").await;
    let r1 = get_record(&base, &a, &uuid::Uuid::new_v4().to_string()).await;
    let r2 = get_record(&base, &a, &msg).await;
    assert_eq!(r1.status(), 403);
    assert_eq!(r2.status(), 403);
    assert_eq!(
        r1.text().await.unwrap(),
        r2.text().await.unwrap(),
        "GET record：不存在与非成员必须同形"
    );
}

/// merged 幂等：同一 op 重试返回同一条记录，不产生重复消息；并发两发 → 每目标一条。
#[tokio::test]
async fn mf1_merged_idempotent() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1ia_{}", uniq())).await;
    let b = register(&base, &format!("mf1ib_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let m = send_text(&mut sa, &dm, "i1", "idem").await;
    let op = uuid::Uuid::new_v4().to_string();

    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m), std::slice::from_ref(&selfc), "merged", &op).await;
    assert_eq!(r.status(), 200);
    let first: Value = r.json().await.unwrap();
    let mid = first["results"][0]["message_ids"][0].as_str().unwrap().to_string();

    // 重试：同一消息 id，无重复行
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m), std::slice::from_ref(&selfc), "merged", &op).await;
    assert_eq!(r.status(), 200);
    let again: Value = r.json().await.unwrap();
    assert_eq!(again["results"][0]["message_ids"][0], mid);

    let count = conv_messages(&base, &a, &selfc)
        .await
        .iter()
        .filter(|m| m["kind"] == "chat_record")
        .count();
    assert_eq!(count, 1, "幂等重试不得重复落库");

    // 并发两个相同请求 → 仍是一条记录 + 一套克隆
    let op2 = uuid::Uuid::new_v4().to_string();
    let ids = [m.clone()];
    let tgts = [selfc.clone()];
    let (r1, r2) = tokio::join!(
        post_batch(&base, &a, &dm, &ids, &tgts, "merged", &op2),
        post_batch(&base, &a, &dm, &ids, &tgts, "merged", &op2),
    );
    assert_eq!(r1.status(), 200);
    assert_eq!(r2.status(), 200);
    let b1: Value = r1.json().await.unwrap();
    let b2: Value = r2.json().await.unwrap();
    assert_eq!(
        b1["results"][0]["message_ids"][0], b2["results"][0]["message_ids"][0],
        "并发同 op 收敛同一条记录"
    );
    let count = conv_messages(&base, &a, &selfc)
        .await
        .iter()
        .filter(|m| m["kind"] == "chat_record")
        .count();
    assert_eq!(count, 2, "并发重试也只得一条新记录");
}

/// 独立限流桶：fwdb 20 次/5 分钟，一请求只计一次；耗尽后 /forward 仍可用（fwd 桶独立）。
#[tokio::test]
async fn mf1_fwdb_bucket_independent() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1ra_{}", uniq())).await;
    let b = register(&base, &format!("mf1rb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let m = send_text(&mut sa, &dm, "r1", "rl").await;

    for i in 0..20 {
        let r = post_batch(
            &base,
            &a,
            &dm,
            std::slice::from_ref(&m),
            std::slice::from_ref(&selfc),
            "merged",
            &uuid::Uuid::new_v4().to_string(),
        )
        .await;
        assert_eq!(r.status(), 200, "第 {} 次批量应成功", i + 1);
    }
    let r = post_batch(
        &base,
        &a,
        &dm,
        std::slice::from_ref(&m),
        std::slice::from_ref(&selfc),
        "merged",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
        "too many attempts, try later"
    );

    // fwd 桶独立：单条转发仍然可用
    let r = post_forward(
        &base,
        &a,
        &m,
        std::slice::from_ref(&selfc),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(r.status(), 200, "fwdb 耗尽不影响 fwd 桶");
}

/// 嵌套：高度 1、2 的记录可进合并（外层到 2、3）；高度 3 的记 too_deep；
/// 全是高度 3 → 400 聊天记录嵌套过深；高度 3 的记录单条转发仍 200（高度不增）。
#[tokio::test]
async fn mf1_nesting_depth() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1na_{}", uniq())).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    // 嵌套素材都建在 self 会话里：批量只认单一源会话，记录必须和文本同会话
    // 才能被选进下一次合并。
    let mut sa = ws_conn(&base, &a).await;
    let leaf = send_text(&mut sa, &selfc, "n1", "leaf").await;

    // h1：含一条纯文本的记录（高度 1）
    let h1 = {
        let r = post_batch(&base, &a, &selfc, std::slice::from_ref(&leaf), std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
            .as_str()
            .unwrap()
            .to_string()
    };
    // h2：合并 [文本, h1记录] → 高度 2
    let h2 = {
        let r = post_batch(&base, &a, &selfc, &[leaf.clone(), h1.clone()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
            .as_str()
            .unwrap()
            .to_string()
    };
    // h3：合并 [文本, h2] → 高度 3
    let h3 = {
        let r = post_batch(&base, &a, &selfc, &[leaf.clone(), h2.clone()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
        assert_eq!(r.status(), 200, "外层高度 3 合法");
        r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
            .as_str()
            .unwrap()
            .to_string()
    };

    // 合并 [leaf, h1, h2] → 接受；[h3] → too_deep 跳过 → 400
    let r = post_batch(&base, &a, &selfc, &[leaf.clone(), h1.clone(), h2.clone()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200, "高度 1/2 可进合并: {}", r.status());
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["results"][0]["ok"], true);

    let r = post_batch(&base, &a, &selfc, std::slice::from_ref(&h3), std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
        "聊天记录嵌套过深"
    );

    // 混合：h3 被跳过但还有别的 → 正常完成，skipped.too_deep=1
    let r = post_batch(&base, &a, &selfc, &[leaf.clone(), h3.clone()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["skipped"]["too_deep"], 1, "{body}");

    // separate 模式复制不变 → h3 也能逐条过
    let r = post_batch(&base, &a, &selfc, std::slice::from_ref(&h3), std::slice::from_ref(&selfc), "separate", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200, "separate 原样复制高度不增");

    // 单条转发 h3 记录 → 200；且快照高度仍 ≤3（不能伪造更高的）
    let r = post_forward(&base, &a, &h3, std::slice::from_ref(&selfc), &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let rid = body["results"][0]["message"]["id"].as_str().unwrap().to_string();
    let rec = get_record(&base, &a, &rid).await.json::<Value>().await.unwrap();
    assert_eq!(rec["items"][1]["kind"], "chat_record");
    let depth = |v: &Value| -> u32 {
        fn h(x: &Value) -> u32 {
            x.get("items")
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|it| it.get("record").filter(|r| !r.is_null()).map(h))
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0)
                + 1
        }
        h(v)
    };
    assert_eq!(depth(&rec), 3, "拷贝的记录高度不增");
}

/// merged 附件克隆：record_message_id=新消息、uploader=转发者、共享物理文件；
/// 附件按目标会话校验；GET record 里的附件 id 是克隆 id。
#[tokio::test]
async fn mf1_merged_attachment_clones() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1aa_{}", uniq())).await;
    let b = register(&base, &format!("mf1ab_{}", uniq())).await;
    let cuser = register(&base, &format!("mf1ac_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let c = http();

    // b 在 DM 里发图片（a 是成员可见）
    let (st, att) = upload_att(&base, &dm, &b.access, "p.png", "image/png", png_bytes()).await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let mut sb = ws_conn(&base, &b).await;
    let img_msg = ws_send(&mut sb, &dm, json!({"client_msg_id": "ia1", "content": "", "attachment_id": att_id})).await;
    let img_id = img_msg["id"].as_str().unwrap().to_string();

    // a 合并转发到 self
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&img_id), std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let rid = body["results"][0]["message_ids"][0].as_str().unwrap().to_string();
    let rec = get_record(&base, &a, &rid).await.json::<Value>().await.unwrap();
    let item = &rec["items"][0];
    assert_eq!(item["kind"], "image");
    let clone_id = item["attachment"]["id"].as_str().unwrap().to_string();
    assert_ne!(clone_id, att_id, "快照附件 id 必须是克隆行 id");

    // 库级断言：克隆行 record_message_id=记录消息、uploader=转发者 a、storage_path 与源相同
    let p = pool().await;
    let (rec_mid, up, sp, rsp): (Option<uuid::Uuid>, uuid::Uuid, String, String) = sqlx::query_as(
        "SELECT a.record_message_id, a.uploader_id, a.storage_path, s.storage_path
           FROM attachments a JOIN attachments s ON s.id = $2
          WHERE a.id = $1",
    )
    .bind(uuid::Uuid::parse_str(&clone_id).unwrap())
    .bind(uuid::Uuid::parse_str(&att_id).unwrap())
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(rec_mid, Some(uuid::Uuid::parse_str(&rid).unwrap()));
    assert_eq!(up, uuid::Uuid::parse_str(&a.id).unwrap(), "克隆 uploader = 转发者");
    assert_eq!(sp, rsp, "共享物理路径");

    let path = format!("/v1/files/{clone_id}");
    let dl = c_get(&base, &a, &path).await;
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.bytes().await.unwrap().as_ref(), png_bytes().as_slice());
    assert_eq!(c_get(&base, &b, &path).await.status(), 404, "源会话成员无目标会话权限");
    assert_eq!(c_get(&base, &cuser, &path).await.status(), 404);

    // 配额不变：a 的 used_bytes 与转发前一致（克隆共享路径不重复计）
    let used: Value = c
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(used["used_bytes"], 0, "转发克隆不占转发者配额");

    // record-item 转发这条克隆 → 普通图片消息、附件再次克隆、uploader=转发者
    let r = http()
        .post(format!("{base}/v1/messages/{rid}/forward"))
        .header("authorization", authz(&a.access))
        .json(&json!({
            "conversation_ids": [selfc.clone()],
            "client_op_id": uuid::Uuid::new_v4().to_string(),
            "record_attachment_id": clone_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let m = &body["results"][0]["message"];
    assert_eq!(m["kind"], "image", "{m}");
    assert_eq!(m["forwarded_from"], b.username, "署名 = 快照条目发送者");
    let reclone = m["attachment"]["id"].as_str().unwrap().to_string();
    assert_ne!(reclone, clone_id);
    let up2: uuid::Uuid = sqlx::query_scalar("SELECT uploader_id FROM attachments WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&reclone).unwrap())
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(up2, uuid::Uuid::parse_str(&a.id).unwrap());
    let dl = c_get(&base, &a, &format!("/v1/files/{reclone}")).await;
    assert_eq!(dl.status(), 200);
}

/// GET record：成员 200 拿全量；非成员/不存在同形 403；非记录/已撤回 → 404。
/// 列表与 WS 只带 record_preview（前 4 条、无附件 id），绝不外泄完整快照。
#[tokio::test]
async fn mf1_record_view_and_get() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1va_{}", uniq())).await;
    let b = register(&base, &format!("mf1vb_{}", uniq())).await;
    let outsider = register(&base, &format!("mf1vc_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(send_text(&mut sa, &dm, &format!("v{i}"), &format!("m{i}")).await);
    }
    let r = post_batch(&base, &a, &dm, &ids, std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    let rid = r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();

    // 成员拿到完整快照（6 条全在）
    let rec = get_record(&base, &a, &rid).await.json::<Value>().await.unwrap();
    assert_eq!(rec["items"].as_array().unwrap().len(), 6);

    // 非成员 403；不存在 403 同形
    let r = get_record(&base, &outsider, &rid).await;
    assert_eq!(r.status(), 403);
    // 非记录消息 → 404
    let r = get_record(&base, &a, &ids[0]).await;
    assert_eq!(r.status(), 404);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
        "聊天记录不可用"
    );

    // 列表视图：record_preview 摘要（title/count/前 4 条），无 record 字段
    let page = conv_messages(&base, &a, &selfc).await;
    let m = page.iter().find(|m| m["id"] == rid).unwrap();
    assert!(m.get("record").is_none(), "列表不得暴露完整快照");
    let pv = &m["record_preview"];
    assert_eq!(pv["title"], format!("{} 和 {} 的聊天记录", a.username, b.username));
    assert_eq!(pv["count"], 6);
    assert_eq!(pv["items"].as_array().unwrap().len(), 4, "预览只带前 4 条");
    assert_eq!(pv["items"][0]["text"], "m0");

    // WS 帧同样只带 preview
    let r = post_batch(&base, &a, &dm, &[ids[0].clone()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let frame = next_of(&mut sa, "message").await;
    assert_eq!(frame["message"]["kind"], "chat_record");
    assert!(frame["message"]["record_preview"]["title"].is_string());
    assert!(frame["message"].get("record").is_none());
}

/// 撤回级联：撤回记录 → 响应无 recalled_attachment_id、GET record 404、
/// 快照克隆立即 404、record-item 转发 400；别处拷贝不受影响；清扫后引用计数正确。
#[tokio::test]
async fn mf1_recall_cascade() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1xa_{}", uniq())).await;
    let b = register(&base, &format!("mf1xb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let c = http();

    // 源：b 发的图
    let (st, att) = upload_att(&base, &dm, &b.access, "x.png", "image/png", png_bytes()).await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let mut sb = ws_conn(&base, &b).await;
    let img = ws_send(&mut sb, &dm, json!({"client_msg_id": "rc1", "content": "", "attachment_id": att_id})).await;
    let img_id = img["id"].as_str().unwrap().to_string();

    // 合并转发两条：rid 留下撤回，rid2 单条转发到 b 的 self 做对照
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&img_id), std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    let rid = r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
        .as_str().unwrap().to_string();
    let rec = get_record(&base, &a, &rid).await.json::<Value>().await.unwrap();
    let clone_id = rec["items"][0]["attachment"]["id"].as_str().unwrap().to_string();

    // 备份一份转发到别处（撤回后它仍可用）
    let r = post_forward(&base, &a, &rid, std::slice::from_ref(&dm), &uuid::Uuid::new_v4().to_string()).await;
    let copy_id = r.json::<Value>().await.unwrap()["results"][0]["message"]["id"]
        .as_str().unwrap().to_string();
    let copy_rec = get_record(&base, &a, &copy_id).await.json::<Value>().await.unwrap();
    let copy_att = copy_rec["items"][0]["attachment"]["id"].as_str().unwrap().to_string();
    assert_ne!(copy_att, clone_id, "拷贝的记录有独立克隆行");

    // a 在 WS 上监听墓碑帧（record_preview=null）
    let mut sa = ws_conn(&base, &a).await;
    let r = c
        .delete(format!("{base}/v1/messages/{rid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert!(
        body.get("recalled_attachment_id").is_none(),
        "记录撤回响应不得带 recalled_attachment_id: {body}"
    );
    let tomb = next_of(&mut sa, "message_deleted").await;
    assert!(tomb["message"]["record_preview"].is_null(), "{tomb}");
    assert_eq!(tomb["message"]["recalled"], true);

    // GET record → 404；克隆公开下载立即 404；record-item → 400
    assert_eq!(get_record(&base, &a, &rid).await.status(), 404);
    let dl = c_get(&base, &a, &format!("/v1/files/{clone_id}")).await;
    assert_eq!(dl.status(), 404);
    let r = http()
        .post(format!("{base}/v1/messages/{rid}/forward"))
        .header("authorization", authz(&a.access))
        .json(&json!({
            "conversation_ids": [selfc.clone()],
            "client_op_id": uuid::Uuid::new_v4().to_string(),
            "record_attachment_id": clone_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
        "消息已撤回"
    );

    // 别处拷贝仍完整可用
    assert_eq!(get_record(&base, &a, &copy_id).await.status(), 200);
    let dl = c_get(&base, &b, &format!("/v1/files/{copy_att}")).await;
    assert_eq!(dl.status(), 200);

    // 清扫语义：撤回记录 + 撤回源附件 → 两行过期清扫；物理文件只被别处拷贝引用而保留
    let r = c
        .delete(format!("{base}/v1/messages/{img_id}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let p = pool().await;
    let swept = sweep_recalled(
        &p,
        std::env::temp_dir().join("chat-it-files").to_str().unwrap(),
        &[
            uuid::Uuid::parse_str(&att_id).unwrap(),
            uuid::Uuid::parse_str(&clone_id).unwrap(),
        ],
    )
    .await;
    assert!(swept >= 2);
    let path = std::env::temp_dir().join("chat-it-files").join(&att_id);
    assert!(path.exists(), "别处拷贝仍引用 storage_path，文件不得删");
    let dl = c_get(&base, &b, &format!("/v1/files/{copy_att}")).await;
    assert_eq!(dl.status(), 200, "清扫后别处拷贝仍可下载");
}

/// 转发不解析 @：群里文本含 @成员 的用户名，单条/separate/merged/record-item
/// 全部不产生 message_mentions 行；record 撤回附件 → GET /files/{id}/recalled 404。
#[tokio::test]
async fn mf1_forward_never_mentions() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1ma_{}", uniq())).await;
    let b = register(&base, &format!("mf1mb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let gid = mk_group(&base, &a, std::slice::from_ref(&b.id)).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let p = pool().await;

    let mut sa = ws_conn(&base, &a).await;
    // 源消息含 @b（在源群里会产生一条真实 mention 行，不影响断言口径——查的是新消息 id）
    let src = send_text(&mut sa, &gid, "m1", &format!("@{} wake", b.username)).await;

    for (label, msg_id) in [
        ("single", {
            let r = post_forward(&base, &a, &src, std::slice::from_ref(&gid), &uuid::Uuid::new_v4().to_string()).await;
            r.json::<Value>().await.unwrap()["results"][0]["message"]["id"].as_str().unwrap().to_string()
        }),
        ("separate", {
            let r = post_batch(&base, &a, &gid, std::slice::from_ref(&src), std::slice::from_ref(&selfc), "separate", &uuid::Uuid::new_v4().to_string()).await;
            r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0].as_str().unwrap().to_string()
        }),
        ("merged", {
            let r = post_batch(&base, &a, &gid, std::slice::from_ref(&src), std::slice::from_ref(&gid), "merged", &uuid::Uuid::new_v4().to_string()).await;
            r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0].as_str().unwrap().to_string()
        }),
    ] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM message_mentions WHERE message_id = $1")
            .bind(uuid::Uuid::parse_str(&msg_id).unwrap())
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(n, 0, "{label} 转发不得产生 mention 行");
        // 视图 mentions 也为空
        let page = conv_messages(&base, &a, if label == "separate" { &selfc } else { &gid }).await;
        let m = page.iter().find(|m| m["id"] == msg_id).unwrap();
        assert_eq!(m["mentions"], json!([]), "{label} 视图不带 mentions");
    }

    // record-item：先造一个含附件的记录
    let (st, att) = upload_att(&base, &gid, &b.access, "a.png", "image/png", png_bytes()).await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let mut sb = ws_conn(&base, &b).await;
    let img = ws_send(&mut sb, &gid, json!({"client_msg_id": "m2", "content": format!("@{} x", a.username), "attachment_id": att_id})).await;
    let r = post_batch(&base, &a, &gid, &[img["id"].as_str().unwrap().to_string()], std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    let rid = r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0].as_str().unwrap().to_string();
    let rec = get_record(&base, &a, &rid).await.json::<Value>().await.unwrap();
    let clone_id = rec["items"][0]["attachment"]["id"].as_str().unwrap().to_string();

    let r = http()
        .post(format!("{base}/v1/messages/{rid}/forward"))
        .header("authorization", authz(&a.access))
        .json(&json!({
            "conversation_ids": [gid.clone()],
            "client_op_id": uuid::Uuid::new_v4().to_string(),
            "record_attachment_id": clone_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let mid = r.json::<Value>().await.unwrap()["results"][0]["message"]["id"].as_str().unwrap().to_string();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM message_mentions WHERE message_id = $1")
        .bind(uuid::Uuid::parse_str(&mid).unwrap())
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(n, 0, "record-item 转发不得产生 mention 行");

    // 记录内克隆行走 /files/{id}/recalled 恒 404（哪怕调用者是克隆行的 uploader）
    let r = c_get(&base, &a, &format!("/v1/files/{clone_id}/recalled")).await;
    assert_eq!(r.status(), 404);

    // @b 转发不解除归档：b 归档 gid，a 把 "@b ..." 单条转发进 gid，
    // b 的 archived_at 必须原样保留（解除归档只由 message_mentions 驱动）。
    assert_eq!(put_pref(&base, &b, &gid, json!({"archived": true})).await.status(), 200);
    let r = post_forward(&base, &a, &src, std::slice::from_ref(&gid), &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let pref = conv_list(&base, &b)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == gid)
        .cloned()
        .unwrap();
    assert!(
        pref["preference"]["archived_at"].is_string(),
        "转发不产生 mention → 不得解除归档: {pref}"
    );
}

/// 站点管理员没有任何转发特权：非成员源会话 403（同形）、GET record 403（同形）、
/// 撤回他人记录恒 403（MM1：管理员不能撤记录）。
#[tokio::test]
async fn mf1_admin_no_special_access() {
    let (_guard, base) = spawn_named_db(&format!("chat_it_mf1adm_{}", uniq())).await;
    let admin = register(&base, &format!("mf1adm_{}", uniq())).await; // 首个 = 管理员
    assert_eq!(me(&base, &admin).await["role"], "admin");
    let a = register(&base, &format!("mf1ga_{}", uniq())).await;
    let b = register(&base, &format!("mf1gb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let m = send_text(&mut sa, &dm, "ad1", "hi").await;
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m), std::slice::from_ref(&dm), "merged", &uuid::Uuid::new_v4().to_string()).await;
    let rid = r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
        .as_str().unwrap().to_string();

    // admin 批量转发 a↔b 的 dm → 403，与不存在会话逐字节同形
    let targets = [selfc.clone()];
    let r1 = post_batch(&base, &admin, &dm, std::slice::from_ref(&m), &targets, "merged", &uuid::Uuid::new_v4().to_string()).await;
    let r2 = post_batch(&base, &admin, &uuid::Uuid::new_v4().to_string(), std::slice::from_ref(&m), &targets, "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r1.status(), 403);
    assert_eq!(r2.status(), 403);
    assert_eq!(r1.text().await.unwrap(), r2.text().await.unwrap());

    // admin GET 他人会话里的记录 → 403，与不存在消息同形
    let r1 = get_record(&base, &admin, &rid).await;
    let r2 = get_record(&base, &admin, &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r1.status(), 403);
    assert_eq!(r2.status(), 403);
    assert_eq!(r1.text().await.unwrap(), r2.text().await.unwrap());

    // admin 撤回别人的记录 → 403 且重试仍 403（MM1：管理员无撤回特权）
    for _ in 0..2 {
        let r = http()
            .post(format!("{base}/v1/messages/{rid}/recall"))
            .header("authorization", authz(&admin.access))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "站点管理员不得撤回他人消息");
    }
}

/// 兜底正文截断：100 × 4000 字条目合并后 content ≤4000 字符、
/// 首行是标题、末行恒为「…（共 N 条，请升级客户端查看）」。
#[tokio::test]
async fn mf1_fallback_truncation() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1fa_{}", uniq())).await;
    let b = register(&base, &format!("mf1fb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    // 单字节填充：100 × ~4KB 留在 1MB 快照上限内，只压兜底正文的 4000 字符闸
    let long = "x".repeat(4000);
    let mut ids = Vec::new();
    for i in 0..100 {
        ids.push(send_text(&mut sa, &dm, &format!("f{i}"), &long).await);
    }
    let r = post_batch(&base, &a, &dm, &ids, std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let rid = body["results"][0]["message_ids"][0].as_str().unwrap().to_string();

    let page = conv_messages(&base, &a, &selfc).await;
    let m = page.iter().find(|m| m["id"] == rid).unwrap();
    let content = m["content"].as_str().unwrap();
    assert!(
        content.chars().count() <= 4000,
        "兜底正文不得超 4000 字符: {}",
        content.chars().count()
    );
    let mut lines = content.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("[聊天记录] {} 和 {} 的聊天记录", a.username, b.username)
    );
    assert_eq!(
        lines.last().unwrap(),
        "…（共 100 条，请升级客户端查看）",
        "截断后末行必须是计数行"
    );
}

/// 记录内附件转发：ok 情况前文已覆盖；这里补拒绝路径——
/// 不存在的附件/别的记录里的附件/普通消息附件 → 同一 404「附件不存在」；
/// 非成员 → 403 同形。
#[tokio::test]
async fn mf1_record_item_forward_rejects() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1ja_{}", uniq())).await;
    let b = register(&base, &format!("mf1jb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();

    let mut sa = ws_conn(&base, &a).await;
    let m = send_text(&mut sa, &dm, "j1", "x").await;
    let r = post_batch(&base, &a, &dm, std::slice::from_ref(&m), std::slice::from_ref(&selfc), "merged", &uuid::Uuid::new_v4().to_string()).await;
    let rid = r.json::<Value>().await.unwrap()["results"][0]["message_ids"][0]
        .as_str().unwrap().to_string();

    let fwd = |att: &str| {
        let base = base.clone();
        let az = authz(&a.access);
        let tgt = selfc.clone();
        let rid = rid.clone();
        let att = att.to_string();
        async move {
            http()
                .post(format!("{base}/v1/messages/{rid}/forward"))
                .header("authorization", az)
                .json(&json!({
                    "conversation_ids": [tgt],
                    "client_op_id": uuid::Uuid::new_v4().to_string(),
                    "record_attachment_id": att,
                }))
                .send()
                .await
                .unwrap()
        }
    };

    // 随机 uuid / 普通消息 id → 同一 404 附件不存在
    for bad in [uuid::Uuid::new_v4().to_string(), m.clone(), rid.clone()] {
        let r = fwd(&bad).await;
        assert_eq!(r.status(), 404, "bad={bad}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"].as_str().unwrap(),
            "附件不存在"
        );
    }

    // 非成员（b 不是 a 的 self 成员）→ 403 与不存在同形
    let r = http()
        .post(format!("{base}/v1/messages/{rid}/forward"))
        .header("authorization", authz(&b.access))
        .json(&json!({
            "conversation_ids": [dm.clone()],
            "client_op_id": uuid::Uuid::new_v4().to_string(),
            "record_attachment_id": uuid::Uuid::new_v4().to_string(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r2 = http()
        .post(format!("{base}/v1/messages/{}/forward", uuid::Uuid::new_v4()))
        .header("authorization", authz(&b.access))
        .json(&json!({
            "conversation_ids": [dm.clone()],
            "client_op_id": uuid::Uuid::new_v4().to_string(),
            "record_attachment_id": uuid::Uuid::new_v4().to_string(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 403);
    assert_eq!(r.text().await.unwrap(), r2.text().await.unwrap());
}

/// separate 幂等：撤回一条源消息后同 op 重试 → 不重复、不广播旧行。
#[tokio::test]
async fn mf1_separate_idempotent_recall_between() {
    let base = spawn().await;
    let a = register(&base, &format!("mf1qa_{}", uniq())).await;
    let b = register(&base, &format!("mf1qb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let dm = dm_conv(&base, &a).await;
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    let c = http();

    let mut sa = ws_conn(&base, &a).await;
    let m1 = send_text(&mut sa, &dm, "q1", "one").await;
    let m2 = send_text(&mut sa, &dm, "q2", "two").await;
    let op = uuid::Uuid::new_v4().to_string();

    let r = post_batch(&base, &a, &dm, &[m1.clone(), m2.clone()], std::slice::from_ref(&selfc), "separate", &op).await;
    assert_eq!(r.status(), 200);
    let first: Value = r.json().await.unwrap();
    let ids1: Vec<String> = first["results"][0]["message_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids1.len(), 2);

    // 撤回源 m2 后同 op 重试：m1 幂等回放、m2 也回放（键含源消息 id，撤回不影响）
    let r = c
        .delete(format!("{base}/v1/messages/{m2}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = post_batch(&base, &a, &dm, &[m1.clone(), m2.clone()], std::slice::from_ref(&selfc), "separate", &op).await;
    assert_eq!(r.status(), 200);
    let again: Value = r.json().await.unwrap();
    let ids2: Vec<String> = again["results"][0]["message_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids1, ids2, "幂等重放返回同一批消息 id");
    let page = conv_messages(&base, &a, &selfc).await;
    let forwarded: Vec<_> = page
        .iter()
        .filter(|m| m["forwarded_from"].is_string() && m["content"] != "[聊天记录]")
        .collect();
    assert_eq!(forwarded.len(), 2, "不得出现重复转发行");
}

async fn c_get(base: &str, u: &TestUser, path: &str) -> reqwest::Response {
    http()
        .get(format!("{base}{path}"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
}
