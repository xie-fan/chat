use super::*;

// ==================== 消息生命周期（Chat V1 / Devin A） ====================

/// 回复：同会话校验 + 预览 + 目标删除后的墓碑预览 + 跨会话拒绝
#[tokio::test]
async fn lifecycle_reply() {
    let base = spawn().await;
    let a = register(&base, &format!("ra_{}", uniq())).await;
    let b = register(&base, &format!("rb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"t1","content":"原消息 abc"}),
    )
    .await;
    let m1_id = m1["id"].as_str().unwrap();

    // B 回复 m1：预览含 id/seq/sender/excerpt
    let r1 = ws_send(
        &mut sb,
        &conv,
        json!({"client_msg_id":"t2","content":"回复来了","reply_to_id":m1_id}),
    )
    .await;
    let pv = &r1["reply_to_preview"];
    assert_eq!(pv["message_id"], m1_id);
    assert_eq!(pv["seq"].as_i64().unwrap(), 1);
    assert_eq!(pv["sender_name"], a.username);
    assert_eq!(pv["excerpt"], "原消息 abc");
    assert_eq!(pv["deleted"], false);
    assert_eq!(r1["reply_to_id"], m1_id);

    // 跨会话回复必须拒绝：a 建群后在群里引用 DM 消息
    let g: Value = c
        .post(format!("{base}/v1/conversations/group"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "g", "member_ids": [b.id]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = g["id"].as_str().unwrap();
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":gid,"client_msg_id":"t3",
               "content":"x","reply_to_id":m1_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "bad_message");
    assert_eq!(err["client_msg_id"], "t3");

    // 删除原消息 → 预览 deleted=true、excerpt 清空、id/seq 仍可定位
    let d = c
        .delete(format!("{base}/v1/messages/{m1_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(d.status(), 200);
    let page = history(&base, &b.access, &conv).await;
    let reply = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == r1["id"])
        .unwrap();
    assert_eq!(reply["reply_to_preview"]["deleted"], true);
    assert_eq!(reply["reply_to_preview"]["excerpt"], "");
    assert_eq!(reply["reply_to_preview"]["seq"].as_i64().unwrap(), 1);
}

/// 编辑：权限/类型/窗口/版本契约
#[tokio::test]
async fn lifecycle_edit() {
    let base = spawn().await;
    let a = register(&base, &format!("ea_{}", uniq())).await;
    let b = register(&base, &format!("eb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m1 = ws_send(&mut sa, &conv, json!({"client_msg_id":"e1","content":"v0"})).await;
    let mid = m1["id"].as_str().unwrap();
    assert_eq!(m1["edit_version"].as_i64().unwrap(), 0);
    assert_eq!(m1["seq"].as_i64().unwrap(), 1);

    // 他人编辑 → 403
    let r = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&b.access))
        .json(&json!({"content":"x","expected_version":0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 版本落后 → 409
    let r = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"v1","expected_version":9}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);

    // 正常编辑：edited_at 出现、edit_version+1、seq 不变
    let op_id = uuid::Uuid::new_v4();
    let r: Value = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"v1","expected_version":0,"client_op_id":op_id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "v1");
    assert_eq!(r["edit_version"].as_i64().unwrap(), 1);
    assert_eq!(r["seq"].as_i64().unwrap(), 1);
    assert!(r["edited_at"].is_string());

    // 同一 client_op_id 幂等回放：不二次递增版本，返回现状
    let r: Value = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"v1-again","expected_version":0,"client_op_id":op_id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "v1");
    assert_eq!(r["edit_version"].as_i64().unwrap(), 1);

    // 附件消息不可编辑：先上传再发附件消息
    let part = reqwest::multipart::Part::bytes(b"hello".to_vec())
        .file_name("f.txt".to_string())
        .mime_str("text/plain")
        .unwrap();
    let att: Value = c
        .post(format!("{base}/v1/files"))
        .header("authorization", authz(&a.access))
        .multipart(
            reqwest::multipart::Form::new()
                .part(
                    "conversation_id",
                    reqwest::multipart::Part::text(conv.clone()),
                )
                .part("file", part),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let m2 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"e2","content":"","attachment_id":att["id"]}),
    )
    .await;
    let r = c
        .patch(format!("{base}/v1/messages/{}", m2["id"].as_str().unwrap()))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"x","expected_version":0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // 15 分钟窗口（回拨 created_at）→ 400
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    sqlx::query("UPDATE messages SET created_at = now() - interval '20 minutes' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(mid).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let r = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"v2","expected_version":1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    sqlx::query("UPDATE messages SET created_at = now() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(mid).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    // 已删除 → 409
    c.delete(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    let r = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"v3","expected_version":2}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
}

/// 删除权限矩阵 + 幂等 + 内容掩蔽 + 附件失效
#[tokio::test]
async fn lifecycle_delete() {
    let base = spawn().await;
    let a = register(&base, &format!("xa_{}", uniq())).await;
    let b = register(&base, &format!("xb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;

    // DM：b 不能删 a 的消息
    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"d1","content":"mine"}),
    )
    .await;
    let m1_id = m1["id"].as_str().unwrap();
    let r = c
        .delete(format!("{base}/v1/messages/{m1_id}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 附件消息删除后文件 404（行删除即失效，不等物理清理）
    let part = reqwest::multipart::Part::bytes(b"img".to_vec())
        .file_name("p.png".to_string())
        .mime_str("image/png")
        .unwrap();
    let att: Value = c
        .post(format!("{base}/v1/files"))
        .header("authorization", authz(&a.access))
        .multipart(
            reqwest::multipart::Form::new()
                .part(
                    "conversation_id",
                    reqwest::multipart::Part::text(conv.clone()),
                )
                .part("file", part),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let att_id = att["id"].as_str().unwrap().to_string();
    let dl = c
        .get(format!("{base}/v1/files/{att_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    let m2 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"d2","content":"","attachment_id":att_id}),
    )
    .await;
    let m2_id = m2["id"].as_str().unwrap();
    let r = c
        .delete(format!("{base}/v1/messages/{m2_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let tomb: Value = r.json().await.unwrap();
    assert_eq!(tomb["id"], m2_id);
    assert_eq!(tomb["content"], "消息已撤回");
    assert!(tomb["deleted_at"].is_string());
    assert!(tomb["edit_version"].as_i64().unwrap() >= 1);
    let dl = c
        .get(format!("{base}/v1/files/{att_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 404);

    // 本人删除：幂等（第二次仍 200，版本不再递增）
    let d1: Value = c
        .delete(format!("{base}/v1/messages/{m1_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let d2: Value = c
        .delete(format!("{base}/v1/messages/{m1_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(d1["edit_version"], d2["edit_version"]);

    // 历史/搜索里正文被掩蔽
    let page = history(&base, &b.access, &conv).await;
    let t1 = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == Value::from(m1_id))
        .unwrap();
    assert_eq!(t1["content"], "消息已撤回");
    assert!(t1["deleted_at"].is_string());
    assert_eq!(t1["seq"].as_i64().unwrap(), 1);
    let found: Value = c
        .get(format!(
            "{base}/v1/conversations/{conv}/messages/search?q=mine"
        ))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        found.as_array().unwrap().is_empty(),
        "deleted content leaked"
    );

    // 群聊：群主可删他人消息，普通成员不行
    let cx = register(&base, &format!("xc_{}", uniq())).await;
    befriend(&base, &a, &cx).await;
    let g: Value = c
        .post(format!("{base}/v1/conversations/group"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "gd", "member_ids": [b.id, cx.id]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = g["id"].as_str().unwrap().to_string();
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;
    let gm = ws_send(&mut sb, &gid, json!({"client_msg_id":"gd1","content":"gm"})).await;
    let gm_id = gm["id"].as_str().unwrap();
    let r = c
        .delete(format!("{base}/v1/messages/{gm_id}"))
        .header("authorization", authz(&cx.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = c
        .delete(format!("{base}/v1/messages/{gm_id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// reactions：幂等、个性化、上限、墓碑拒绝
#[tokio::test]
async fn lifecycle_reactions() {
    let base = spawn().await;
    let a = register(&base, &format!("xa2_{}", uniq())).await;
    let b = register(&base, &format!("xb2_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"rx1","content":"hi"}),
    )
    .await;
    let mid = m1["id"].as_str().unwrap();
    let url = |e: &str| format!("{base}/v1/messages/{mid}/reactions/{}", urlencoded(e));

    // a 添加 👍 → count 1 / reacted_by_me=true / version+1
    let r: Value = c
        .put(url("👍"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["reactions"][0]["count"].as_i64().unwrap(), 1);
    assert_eq!(r["reactions"][0]["reacted_by_me"], true);
    let v1 = r["edit_version"].as_i64().unwrap();
    assert_eq!(v1, 1);

    // 重复 add 幂等：count 不变、版本不涨
    let r: Value = c
        .put(url("👍"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["reactions"][0]["count"].as_i64().unwrap(), 1);
    assert_eq!(r["edit_version"].as_i64().unwrap(), v1);

    // b 也添加同一 emoji → count 2；b 视角 reacted_by_me=true
    let r: Value = c
        .put(url("👍"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["reactions"][0]["count"].as_i64().unwrap(), 2);
    assert_eq!(r["reactions"][0]["reacted_by_me"], true);
    assert_eq!(r["edit_version"].as_i64().unwrap(), v1 + 1);

    // a 视角历史：reaction_update 之后 reacted_by_me 仍 true；b 删除自己 → a 视角变回 1
    let r: Value = c
        .delete(url("👍"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["reactions"][0]["count"].as_i64().unwrap(), 1);
    // 删除不存在的 reaction 幂等（再删一次 → count 仍 1、版本不涨）
    let r2: Value = c
        .delete(url("👍"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r2["edit_version"], r["edit_version"]);

    // 每用户每消息最多 20 种 emoji（用 ASCII key 测边界，服务端不校验 emoji 字形）
    for i in 0..19 {
        let e = format!("e{i}");
        let r = c
            .put(url(&e))
            .header("authorization", authz(&a.access))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "emoji {e} should succeed");
    }
    let r = c
        .put(url("e19"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "21st emoji kind must be rejected");

    // 墓碑不可再表态
    c.delete(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    let r = c
        .put(url("😂"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
}

pub(crate) fn urlencoded(s: &str) -> String {
    s.bytes().map(|b| format!("%{b:02X}")).collect()
}

/// REST mutation 的 WS 广播：双端都收到，reaction 按接收者个性化
#[tokio::test]
async fn lifecycle_ws_frames() {
    let base = spawn().await;
    let a = register(&base, &format!("fa_{}", uniq())).await;
    let b = register(&base, &format!("fb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"w1","content":"ws msg"}),
    )
    .await;
    let mid = m1["id"].as_str().unwrap();
    next_of(&mut sb, "message").await; // b 收到原始消息

    // 编辑 → 双端 message_updated
    c.patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"edited","expected_version":0}))
        .send()
        .await
        .unwrap();
    let upd_a = next_of(&mut sa, "message_updated").await;
    let upd_b = next_of(&mut sb, "message_updated").await;
    assert_eq!(upd_a["message"]["content"], "edited");
    assert_eq!(upd_a["edit_version"].as_i64().unwrap(), 1);
    assert_eq!(upd_b["message"]["content"], "edited");

    // a 加 reaction → 双端 reaction_update，reacted_by_me 逐接收者个性化
    c.put(format!(
        "{base}/v1/messages/{mid}/reactions/{}",
        urlencoded("👍")
    ))
    .header("authorization", authz(&a.access))
    .send()
    .await
    .unwrap();
    let rx_a = next_of(&mut sa, "reaction_update").await;
    let rx_b = next_of(&mut sb, "reaction_update").await;
    assert_eq!(rx_a["message"]["reactions"][0]["reacted_by_me"], true);
    assert_eq!(rx_b["message"]["reactions"][0]["reacted_by_me"], false);
    assert_eq!(
        rx_a["message"]["reactions"][0]["count"].as_i64().unwrap(),
        1
    );

    // 删除 → 双端墓碑帧（完整视图，seq/id 保留、正文已掩蔽）
    c.delete(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    let del_a = next_of(&mut sa, "message_deleted").await;
    let del_b = next_of(&mut sb, "message_deleted").await;
    assert_eq!(del_a["message"]["id"], mid);
    assert_eq!(del_b["message"]["id"], mid);
    assert_eq!(del_a["message"]["content"], "消息已撤回");
    assert_eq!(del_a["message"]["seq"].as_i64().unwrap(), 1);
    assert!(del_a["message"]["deleted_at"].is_string());
    assert!(del_a["edit_version"].as_i64().unwrap() >= 2);
}

/// §8 反序安全回归：每个 mutation 帧都携带完整视图——
/// reaction_update(v2) 已含编辑后的正文，即使客户端随后才收到 message_updated(v1)
/// 并按版本丢弃它，正文与 reaction 也不会丢；message_deleted(v3) 同理自带墓碑态。
#[tokio::test]
async fn lifecycle_frames_carry_full_view() {
    let base = spawn().await;
    let a = register(&base, &format!("fa2_{}", uniq())).await;
    let b = register(&base, &format!("fb2_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"fv1","content":"old"}),
    )
    .await;
    let mid = m1["id"].as_str().unwrap();
    next_of(&mut sb, "message").await;

    // edit(v1) → reaction(v2)：v2 帧必须同时含新正文与新 reaction
    c.patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"edited body","expected_version":0}))
        .send()
        .await
        .unwrap();
    c.put(format!(
        "{base}/v1/messages/{mid}/reactions/{}",
        urlencoded("👍")
    ))
    .header("authorization", authz(&b.access))
    .send()
    .await
    .unwrap();

    let upd = next_of(&mut sb, "message_updated").await;
    let rx = next_of(&mut sb, "reaction_update").await;
    assert_eq!(upd["edit_version"].as_i64().unwrap(), 1);
    // 关键断言：reaction_update(v2) 携带完整视图，含 v1 的编辑结果
    let m2 = &rx["message"];
    assert_eq!(m2["edit_version"].as_i64().unwrap(), 2);
    assert_eq!(m2["content"], "edited body");
    assert_eq!(m2["reactions"][0]["emoji"], "👍");
    assert_eq!(m2["reactions"][0]["count"].as_i64().unwrap(), 1);
    assert!(m2["deleted_at"].is_null());
    // b 视角 reacted_by_me=true（b 自己加的）
    assert_eq!(m2["reactions"][0]["reacted_by_me"], true);

    // delete(v3)：帧内完整墓碑视图（正文体掩蔽、reactions 清空、seq 保留）
    c.delete(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    let del = next_of(&mut sb, "message_deleted").await;
    let m3 = &del["message"];
    assert_eq!(m3["edit_version"].as_i64().unwrap(), 3);
    assert_eq!(m3["id"], mid);
    assert_eq!(m3["seq"].as_i64().unwrap(), 1);
    assert_eq!(m3["content"], "消息已撤回");
    assert!(m3["deleted_at"].is_string());
    assert_eq!(m3["reactions"].as_array().unwrap().len(), 0);
}

/// 重连批量刷新端点：按 id 回读、≤100、无权限会话 403
#[tokio::test]
async fn lifecycle_refresh() {
    let base = spawn().await;
    let a = register(&base, &format!("ra2_{}", uniq())).await;
    let b = register(&base, &format!("rb2_{}", uniq())).await;
    let outsider = register(&base, &format!("ro_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m1 = ws_send(&mut sa, &conv, json!({"client_msg_id":"rf1","content":"x"})).await;
    let mid = m1["id"].as_str().unwrap();

    // 编辑后刷新应回最新正文与版本
    c.patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content":"new","expected_version":0}))
        .send()
        .await
        .unwrap();
    let r: Value = c
        .post(format!("{base}/v1/conversations/{conv}/messages/refresh"))
        .header("authorization", authz(&b.access))
        .json(&json!({"ids":[mid]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let m = &r["messages"][0];
    assert_eq!(m["content"], "new");
    assert_eq!(m["edit_version"].as_i64().unwrap(), 1);
    assert!(m["edited_at"].is_string());

    // >100 ids → 400
    let ids: Vec<String> = (0..=100)
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let r = c
        .post(format!("{base}/v1/conversations/{conv}/messages/refresh"))
        .header("authorization", authz(&b.access))
        .json(&json!({"ids":ids}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // 非成员 → 403（客户端据此清空本地缓存）
    let r = c
        .post(format!("{base}/v1/conversations/{conv}/messages/refresh"))
        .header("authorization", authz(&outsider.access))
        .json(&json!({"ids":[mid]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

/// 只读 DM（好友解除后）：回复/编辑/删除/reaction 全部拒绝
#[tokio::test]
async fn lifecycle_readonly_dm() {
    let base = spawn().await;
    let a = register(&base, &format!("za_{}", uniq())).await;
    let b = register(&base, &format!("zb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m1 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id":"ro1","content":"keep"}),
    )
    .await;
    let mid = m1["id"].as_str().unwrap();

    // 解除好友 → DM 只读
    let r = c
        .delete(format!("{base}/v1/friends/{}", b.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // 回复（send_message + reply_to_id）被拒
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"ro2",
               "content":"reply","reply_to_id":mid})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "not_friends");
    assert_eq!(err["client_msg_id"], "ro2");

    // 编辑/删除/reaction 均 403
    for resp in [
        c.patch(format!("{base}/v1/messages/{mid}"))
            .header("authorization", authz(&a.access))
            .json(&json!({"content":"x","expected_version":0}))
            .send()
            .await
            .unwrap(),
        c.delete(format!("{base}/v1/messages/{mid}"))
            .header("authorization", authz(&a.access))
            .send()
            .await
            .unwrap(),
        c.put(format!(
            "{base}/v1/messages/{mid}/reactions/{}",
            urlencoded("👍")
        ))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap(),
    ] {
        assert_eq!(resp.status(), 403);
    }
}
