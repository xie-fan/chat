use super::*;

// ===== MM1 消息撤回 =====
// 撤回语义、权限矩阵、附件保留/取回/重发/清扫。
// 时间判定一律用数据库 now()：测试用 UPDATE ... created_at/recalled_at 回拨。

fn uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap()
}

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new().connect(&test_db_url()).await.unwrap()
}

async fn age_message(pool: &sqlx::PgPool, id: &str, older_than: &str) {
    sqlx::query(&format!(
        "UPDATE messages SET created_at = now() - interval '{older_than}' WHERE id = $1"
    ))
    .bind(uuid(id))
    .execute(pool)
    .await
    .unwrap();
}

async fn age_attachment(pool: &sqlx::PgPool, id: &str, older_than: &str) {
    sqlx::query(&format!(
        "UPDATE attachments SET recalled_at = now() - interval '{older_than}' WHERE id = $1"
    ))
    .bind(uuid(id))
    .execute(pool)
    .await
    .unwrap();
}

async fn recall_post(base: &str, token: &str, mid: &str) -> reqwest::Response {
    http()
        .post(format!("{base}/v1/messages/{mid}/recall"))
        .header("authorization", authz(token))
        .send()
        .await
        .unwrap()
}

async fn recall_delete(base: &str, token: &str, mid: &str) -> reqwest::Response {
    http()
        .delete(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(token))
        .send()
        .await
        .unwrap()
}

/// 本人撤回（POST /recall 与 DELETE 别名）：墓碑字段齐全，原文从库里消失，
/// WS message_deleted 帧带撤回者信息，重复撤回幂等。
#[tokio::test]
async fn recall_own_within_24h() {
    let base = spawn().await;
    let a = register(&base, &format!("rca_{}", uniq())).await;
    let b = register(&base, &format!("rcb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let pool = pool().await;

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;

    let secret = format!("绝密-{}", uniq());
    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "r1", "content": secret}),
    )
    .await;
    let mid = m["id"].as_str().unwrap();
    next_of(&mut sb, "message").await;

    // POST /recall：撤回自己的消息
    let r = recall_post(&base, &a.access, mid).await;
    assert_eq!(r.status(), 200);
    let tomb: Value = r.json().await.unwrap();
    assert_eq!(tomb["id"], mid);
    assert_eq!(tomb["content"], "消息已撤回");
    assert_eq!(tomb["recalled"], true);
    assert_eq!(tomb["recalled_by"], a.id);
    assert_eq!(tomb["recalled_by_name"], a.username);
    assert!(tomb["deleted_at"].is_string());
    // 无附件消息不携带 recalled_attachment_id（skip_serializing_if）
    assert!(tomb.get("recalled_attachment_id").is_none());

    // WS 广播帧：b 收到同形墓碑（recalled_by 指 a），绝不含 recalled_attachment_id
    let del = next_of(&mut sb, "message_deleted").await;
    assert_eq!(del["message"]["id"], mid);
    assert_eq!(del["message"]["recalled"], true);
    assert_eq!(del["message"]["recalled_by"], a.id);
    assert_eq!(del["message"]["recalled_by_name"], a.username);
    assert!(del["message"].get("recalled_attachment_id").is_none());
    let del_a = next_of(&mut sa, "message_deleted").await;
    assert!(del_a["message"].get("recalled_attachment_id").is_none());

    // 原文在库里物理消失：messages.content 被占位文本覆写
    let stored: String = sqlx::query_scalar("SELECT content FROM messages WHERE id = $1")
        .bind(uuid(mid))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "消息已撤回");
    let leaked = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM messages WHERE content LIKE '%' || $1 || '%'",
    )
    .bind(&secret)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0, "原文不得留在任何 messages.content");

    // 幂等：DELETE 别名重复撤回返回同一墓碑，版本不再递增
    let r = recall_delete(&base, &a.access, mid).await;
    assert_eq!(r.status(), 200);
    let t2: Value = r.json().await.unwrap();
    assert_eq!(t2["edit_version"], tomb["edit_version"]);
    assert_eq!(t2["recalled_by"], a.id);

    // 会话列表预览带撤回者（渲染「X 撤回了一条消息」）
    let convs = conv_list(&base, &b).await;
    let lm = &convs
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == conv)
        .unwrap()["last_message"];
    assert_eq!(lm["recalled"], true);
    assert_eq!(lm["recalled_by_name"], a.username);
    assert_eq!(lm["content"], "消息已撤回");

    // seq/last_seq 不动
    assert_eq!(lm["seq"].as_i64().unwrap(), m["seq"].as_i64().unwrap());
}

/// 24 小时窗口：发送者超时 → 403 + code=recall_expired；群主撤他人消息不限时，
/// 群主自己的超时消息同样 recall_expired；普通成员撤他人 → forbidden；
/// 站点 admin（非群主非发送者）无特权 → forbidden。
#[tokio::test]
async fn recall_permission_matrix() {
    let base = spawn().await;
    let owner = register(&base, &format!("gown_{}", uniq())).await;
    let member = register(&base, &format!("gmem_{}", uniq())).await;
    let third = register(&base, &format!("g3_{}", uniq())).await;
    let admin = register(&base, &format!("gadm_{}", uniq())).await;
    let pool = pool().await;

    // 站点 admin（不入群，仅 users.role=admin）
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(uuid(&admin.id))
        .execute(&pool)
        .await
        .unwrap();

    befriend(&base, &owner, &member).await;
    befriend(&base, &owner, &third).await;
    let gid = mk_group(&base, &owner, &[member.id.clone(), third.id.clone()]).await;

    let (mut sm, _) = tokio_tungstenite::connect_async(ws_url(&base, &member).await)
        .await
        .unwrap();
    next_of(&mut sm, "presence_init").await;

    // member 的旧消息（拨回 25h）——群主可撤；member 自己/外人不可
    let old = ws_send(
        &mut sm,
        &gid,
        json!({"client_msg_id": "old1", "content": "older than 24h"}),
    )
    .await;
    let old_id = old["id"].as_str().unwrap().to_string();
    age_message(&pool, &old_id, "25 hours").await;

    // 发送者自己撤回超时消息 → 403 recall_expired
    let r = recall_post(&base, &member.access, &old_id).await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["code"], "recall_expired", "{body}");

    // 非群主成员撤他人 → 403 forbidden（先于时间判定）
    let r = recall_post(&base, &third.access, &old_id).await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_ne!(body["code"], "recall_expired", "{body}");

    // 站点 admin 非成员 → 403
    let r = recall_post(&base, &admin.access, &old_id).await;
    assert_eq!(r.status(), 403);

    // 群主撤成员的旧消息 → 200（群主豁免时限），recalled_by = 群主
    let r = recall_post(&base, &owner.access, &old_id).await;
    assert_eq!(r.status(), 200);
    let tomb: Value = r.json().await.unwrap();
    assert_eq!(tomb["recalled_by"], owner.id);
    assert_eq!(tomb["recalled_by_name"], owner.username);
    // 群主不是附件上传者/且消息无附件 → 无 recalled_attachment_id
    assert!(tomb.get("recalled_attachment_id").is_none());

    // 已撤回消息上的权限依旧生效：非群主成员重复撤回 → 403 forbidden（不能借幂等短路）
    let r = recall_post(&base, &third.access, &old_id).await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_ne!(body["code"], "recall_expired", "{body}");
    // 发送者本人重复撤回 → 幂等 200（撤回早已生效，不再触发 24h 判定）
    let r = recall_post(&base, &member.access, &old_id).await;
    assert_eq!(r.status(), 200, "已撤回消息的重发撤回须幂等，{r:?}");
    let again: Value = r.json().await.unwrap();
    assert_eq!(again["recalled"], true);
    assert_eq!(again["edit_version"], tomb["edit_version"]);

    // 群主自己的超时消息 → recall_expired（群主不豁免自己的窗口）
    let (mut so, _) = tokio_tungstenite::connect_async(ws_url(&base, &owner).await)
        .await
        .unwrap();
    next_of(&mut so, "presence_init").await;
    let own = ws_send(
        &mut so,
        &gid,
        json!({"client_msg_id": "own1", "content": "owner old"}),
    )
    .await;
    let own_id = own["id"].as_str().unwrap().to_string();
    age_message(&pool, &own_id, "25 hours").await;
    let r = recall_post(&base, &owner.access, &own_id).await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["code"], "recall_expired", "{body}");

    // DM 里 24h 边界
    let dm = dm_conv(&base, &member).await;
    let fresh = ws_send(
        &mut sm,
        &dm,
        json!({"client_msg_id": "fresh", "content": "within window"}),
    )
    .await;
    let r = recall_post(&base, &member.access, fresh["id"].as_str().unwrap()).await;
    assert_eq!(r.status(), 200, "窗口内本人撤回应成功");
}

/// 撤回后的消息拒绝一切 mutation：编辑 409、reaction 409、置顶 400。
#[tokio::test]
async fn recalled_message_rejects_mutations() {
    let base = spawn().await;
    let a = register(&base, &format!("rna_{}", uniq())).await;
    let b = register(&base, &format!("rnb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "rm1", "content": "bye"}),
    )
    .await;
    let mid = m["id"].as_str().unwrap();

    let r = recall_post(&base, &a.access, mid).await;
    assert_eq!(r.status(), 200);

    let r = c
        .patch(format!("{base}/v1/messages/{mid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"content": "x", "expected_version": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "撤回后编辑应 409");
    let r = c
        .put(format!(
            "{base}/v1/messages/{mid}/reactions/{}",
            urlencoded("👍")
        ))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "撤回后 reaction 应 409");
    let r = c
        .put(format!("{base}/v1/conversations/{conv}/pins/{mid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "撤回后置顶应 400");
}

/// 附件消息撤回：公开下载/缩略图/下载参数一律 404，视图不暴露附件；
/// /files/{id}/recalled 仅上传者 Bearer 可取回；过期后同样 404。
#[tokio::test]
async fn recall_attachment_hidden_and_uploader_fetch() {
    let base = spawn().await;
    let a = register(&base, &format!("ra_{}", uniq())).await;
    let b = register(&base, &format!("rb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();
    let pool = pool().await;

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;

    let body = png_fixture(120, 80);
    let (st, att) = upload_att(&base, &conv, &a.access, "pic.png", "image/png", body.clone()).await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "att1", "content": "", "attachment_id": att_id}),
    )
    .await;
    let mid = m["id"].as_str().unwrap().to_string();

    // 撤回前：签发短时 ticket 访问原图/缩略图均返回 200，且强制 private, no-store 防止浏览器缓存
    let ticket_resp = c
        .get(format!("{base}/v1/files/{att_id}/ticket"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(ticket_resp.status(), 200);
    assert_eq!(ticket_resp.headers()["cache-control"], "private, no-store");
    let ticket = ticket_resp.json::<Value>().await.unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    for pre_url in [
        format!("{base}/v1/files/{att_id}?ticket={ticket}"),
        format!("{base}/v1/files/{att_id}?thumb=1&ticket={ticket}"),
    ] {
        let r = c.get(&pre_url).send().await.unwrap();
        assert_eq!(r.status(), 200, "撤回前 {pre_url} 应 200");
        assert_eq!(r.headers()["cache-control"], "private, no-store");
    }

    // 撤回：响应是上传者视角，带 recalled_attachment_id
    let r = recall_post(&base, &a.access, &mid).await;
    assert_eq!(r.status(), 200);
    let tomb: Value = r.json().await.unwrap();
    assert!(tomb["attachment"].is_null());
    assert_eq!(tomb["recalled_attachment_id"], att_id);

    // 公开路径全部 404（含 Bearer 与撤回前签发的旧 ticket）：原图 / ?dl=1 / ?thumb=1，均带 no-store
    for url in [
        format!("{base}/v1/files/{att_id}"),
        format!("{base}/v1/files/{att_id}?dl=1"),
        format!("{base}/v1/files/{att_id}?thumb=1"),
    ] {
        let dl = c.get(&url).header("authorization", authz(&a.access)).send().await.unwrap();
        assert_eq!(dl.status(), 404, "{url} 应 404");
        assert_eq!(dl.headers()["cache-control"], "private, no-store");
    }
    for old_url in [
        format!("{base}/v1/files/{att_id}?ticket={ticket}"),
        format!("{base}/v1/files/{att_id}?thumb=1&ticket={ticket}"),
    ] {
        let dl = c.get(&old_url).send().await.unwrap();
        assert_eq!(dl.status(), 404, "撤回后旧票据 {old_url} 应 404");
        assert_eq!(dl.headers()["cache-control"], "private, no-store");
    }

    // 墓碑视图不暴露附件
    let page = history(&base, &b.access, &conv).await;
    let t = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == mid)
        .unwrap();
    assert!(t["attachment"].is_null());
    assert_eq!(t["recalled"], true);

    // /recalled 取回：无认证 → 401；他人 → 404；上传者 → 200 且字节一致、no-store
    let r = c
        .get(format!("{base}/v1/files/{att_id}/recalled"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = c
        .get(format!("{base}/v1/files/{att_id}/recalled"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = c
        .get(format!("{base}/v1/files/{att_id}/recalled"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cc = r
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cc.contains("no-store"), "撤回取回必须 no-store: {cc}");
    assert_eq!(r.bytes().await.unwrap().as_ref(), body.as_slice());

    // 缩略图同样可取
    let r = c
        .get(format!("{base}/v1/files/{att_id}/recalled?thumb=1"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("no-store"));

    // 拨过保留期 → 上传者本人也 404
    age_attachment(&pool, &att_id, "2 days").await;
    let r = c
        .get(format!("{base}/v1/files/{att_id}/recalled"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// 重发复用：WS send 带 reuse_attachment_id → 克隆新行新 id、公开可下、
/// kind/宽高沿用源行；互斥校验、会话限制、归属、过期、幂等序。
#[tokio::test]
async fn reuse_recalled_attachment() {
    let base = spawn().await;
    let a = register(&base, &format!("rua_{}", uniq())).await;
    let b = register(&base, &format!("rub_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();
    let pool = pool().await;

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;

    let body = png_fixture(64, 48);
    let (st, att) = upload_att(&base, &conv, &a.access, "p.png", "image/png", body.clone()).await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "ru1", "content": "", "attachment_id": att_id}),
    )
    .await;
    let r = recall_post(&base, &a.access, m["id"].as_str().unwrap()).await;
    assert_eq!(r.status(), 200);
    let tomb: Value = r.json().await.unwrap();
    assert_eq!(tomb["recalled_attachment_id"], att_id);

    // WS 重发复用：新消息拿新附件 id，公开 URL 可用，kind/dims 来自源行
    let re = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "ru2", "content": "", "reuse_attachment_id": att_id}),
    )
    .await;
    let new_att = re["attachment"]["id"].as_str().unwrap().to_string();
    assert_ne!(new_att, att_id, "复用必须克隆出新附件行");
    assert_eq!(re["kind"], "image");
    assert_eq!(re["attachment"]["width"], 64);
    assert_eq!(re["attachment"]["height"], 48);
    let dl = c
        .get(format!("{base}/v1/files/{new_att}"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.bytes().await.unwrap().as_ref(), body.as_slice());
    // 克隆行不进清扫集
    let recalled_flag: bool = sqlx::query_scalar(
        "SELECT recalled_at IS NOT NULL FROM attachments WHERE id = $1",
    )
    .bind(uuid(&new_att))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!recalled_flag, "克隆行 recalled_at 必须为 NULL");
    // 源行仍 recalled（TTL 内可再复用）
    let src_still: bool = sqlx::query_scalar(
        "SELECT recalled_at IS NOT NULL FROM attachments WHERE id = $1",
    )
    .bind(uuid(&att_id))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(src_still);

    // 同一源 TTL 内可再复用
    let re2 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "ru3", "content": "", "reuse_attachment_id": att_id}),
    )
    .await;
    assert!(re2["attachment"]["id"].as_str().unwrap() != new_att);

    // attachment_id 与 reuse_attachment_id 互斥 → error 帧（bad_request）
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"ru4",
               "content":"","attachment_id":new_att,"reuse_attachment_id":att_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "bad_request");

    // 非上传者复用 → forbidden
    let (mut sb, _) = tokio_tungstenite::connect_async(ws_url(&base, &b).await)
        .await
        .unwrap();
    next_of(&mut sb, "presence_init").await;
    sb.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"rux",
               "content":"","reuse_attachment_id":att_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sb, "error").await;
    assert_eq!(err["code"], "forbidden");

    // 其他会话复用 → forbidden（reuse_conv_allowed 仅同源会话）
    let selfc = self_conv(&base, &a).await["id"].as_str().unwrap().to_string();
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":selfc,"client_msg_id":"ru5",
               "content":"","reuse_attachment_id":att_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "forbidden");

    // 幂等序：client_msg_id 命中先于 reuse 校验——源行过期后重放仍返回原消息
    let re4 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "ru6", "content": "", "reuse_attachment_id": att_id}),
    )
    .await;
    age_attachment(&pool, &att_id, "2 days").await;
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"ru6",
               "content":"","reuse_attachment_id":att_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let ack = next_of(&mut sa, "message_ack").await;
    assert_eq!(
        ack["message"]["id"].as_str().unwrap(),
        re4["id"].as_str().unwrap(),
        "幂等命中必须先于过期校验返回原消息"
    );

    // 过期源 → attachment_expired
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"ru7",
               "content":"","reuse_attachment_id":att_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "attachment_expired");

    // 未撤回的正常附件当 reuse 源 → attachment_expired
    let (st, live) = upload_att(&base, &conv, &a.access, "l.png", "image/png", body.clone()).await;
    assert_eq!(st, 200);
    let live_id = live["id"].as_str().unwrap().to_string();
    sa.send(WsMsg::Text(
        json!({"type":"send_message","conversation_id":conv,"client_msg_id":"ru8",
               "content":"","reuse_attachment_id":live_id})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let err = next_of(&mut sa, "error").await;
    assert_eq!(err["code"], "attachment_expired");
}

/// 清扫：过期撤回行删除、messages.attachment_id 置空、物理文件按引用计数删；
/// 未过期行不动。
#[tokio::test]
async fn sweep_removes_only_expired() {
    let base = spawn().await;
    let a = register(&base, &format!("swa_{}", uniq())).await;
    let b = register(&base, &format!("swb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();
    let pool = pool().await;
    let files_dir = std::env::temp_dir().join("chat-it-files");

    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;

    // 两条附件消息都撤回；只过期第一条
    let mut att_ids = Vec::new();
    let mut msg_ids = Vec::new();
    for (i, name) in [(1, "s1.png"), (2, "s2.png")] {
        let (st, att) = upload_att(
            &base,
            &conv,
            &a.access,
            name,
            "image/png",
            png_fixture(32, 32),
        )
        .await;
        assert_eq!(st, 200);
        let aid = att["id"].as_str().unwrap().to_string();
        let m = ws_send(
            &mut sa,
            &conv,
            json!({"client_msg_id": format!("sw{i}"), "content": "", "attachment_id": aid}),
        )
        .await;
        att_ids.push(aid);
        msg_ids.push(m["id"].as_str().unwrap().to_string());
    }
    for mid in &msg_ids {
        let r = recall_post(&base, &a.access, mid).await;
        assert_eq!(r.status(), 200);
    }
    let f1 = files_dir.join(&att_ids[0]);
    let f2 = files_dir.join(&att_ids[1]);
    assert!(f1.exists() && f2.exists());

    // 只过期第一条 → 清行、文件删、消息引用置空；第二条原样
    // （并行用例可能同时有过期行，swept 只断言下限，逐行断言保证确定性）
    let swept = sweep_recalled(&pool, files_dir.to_str().unwrap(), &[uuid(&att_ids[0])]).await;
    assert!(swept >= 1, "过期行必须被清扫");
    assert!(!f1.exists());
    assert!(f2.exists());
    let (att_ref, row_gone): (Option<uuid::Uuid>, i64) = sqlx::query_as(
        "SELECT m.attachment_id, (SELECT count(*) FROM attachments WHERE id = $1)
           FROM messages m WHERE m.id = $2",
    )
    .bind(uuid(&att_ids[0]))
    .bind(uuid(&msg_ids[0]))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(att_ref.is_none(), "清扫后 messages.attachment_id 必须为 NULL");
    assert_eq!(row_gone, 0);
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE id = $1")
        .bind(uuid(&att_ids[1]))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1, "未过期行不得被清扫");
    // /recalled 在源行被清扫后也是 404
    let r = c
        .get(format!("{base}/v1/files/{}/recalled", att_ids[0]))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}
