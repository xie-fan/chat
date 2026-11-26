use super::*;

// ===== V2 P2-1 =====
// 图片尺寸/缩略图 + 每用户存储配额

/// spawn() 共用的 files_dir（与 spawn() 中的路径一致）
fn it_files_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("chat-it-files")
}

#[tokio::test]
async fn attachments_follow_membership_even_for_issued_links() {
    let base = spawn().await;
    let owner = register(&base, &format!("fo_{}", uniq())).await;
    let member = register(&base, &format!("fm_{}", uniq())).await;
    befriend(&base, &owner, &member).await;
    let group = mk_group(&base, &owner, std::slice::from_ref(&member.id)).await;
    let (status, attachment) = upload_att(
        &base,
        &group,
        &owner.access,
        "photo.png",
        "image/png",
        png_fixture(900, 700),
    )
    .await;
    assert_eq!(status, 200);
    let id = attachment["id"].as_str().unwrap();
    let url = format!("{base}/v1/files/{id}");
    let c = http();
    assert_eq!(c.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        c.get(&url)
            .header("authorization", authz(&member.access))
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "unsent uploads remain visible only to the uploader"
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &owner).await)
        .await
        .unwrap();
    next_of(&mut ws, "presence_init").await;
    ws_send(&mut ws, &group, json!({"content": "", "attachment_id": id})).await;

    let ticket_response = c
        .get(format!("{url}/ticket"))
        .header("authorization", authz(&member.access))
        .send()
        .await
        .unwrap();
    assert_eq!(ticket_response.status(), 200);
    assert_eq!(ticket_response.headers()["cache-control"], "private, no-store");
    let ticket: Value = ticket_response.json().await.unwrap();
    let ticket = ticket["ticket"].as_str().unwrap();
    let thumb = c
        .get(format!("{url}?thumb=1&ticket={ticket}"))
        .header("range", "bytes=0-1")
        .send()
        .await
        .unwrap();
    assert_eq!(thumb.status(), 206);
    assert_eq!(thumb.headers()["cache-control"], "private, no-store");
    assert_eq!(thumb.headers()["content-type"], "image/jpeg");
    assert_eq!(thumb.bytes().await.unwrap().as_ref(), &[0xff, 0xd8]);

    let other_url = format!("{base}/v1/files/{}?ticket={ticket}", uuid::Uuid::new_v4());
    assert_eq!(c.get(other_url).send().await.unwrap().status(), 401);
    assert_eq!(
        c.get(format!("{base}/v1/users/me"))
            .header("authorization", authz(ticket))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "a scoped ticket is not an account access token"
    );

    let r = c
        .delete(format!("{base}/v1/conversations/{group}/members/{}", member.id))
        .header("authorization", authz(&owner.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    for suffix in ["", "/ticket", "?dl=1", "?thumb=1"] {
        let r = c
            .get(format!("{url}{suffix}"))
            .header("authorization", authz(&member.access))
            .header("range", "bytes=0-1")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "membership removal applies to {suffix}");
        assert_eq!(r.headers()["cache-control"], "private, no-store");
    }
    let old_link = c
        .get(format!("{url}?dl=1&ticket={ticket}"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_link.status(), 404, "a ticket cannot keep membership alive");
    assert_eq!(old_link.headers()["cache-control"], "private, no-store");
    assert_eq!(
        c.get(url)
            .header("authorization", authz(&owner.access))
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "remaining members keep attachment access"
    );
}

#[tokio::test]
async fn repeated_upload_key_replays_the_same_attachment() {
    let (_guard, base, state) = spawn_named_state(&format!("chat_t_upload_retry_{}", uniq())).await;
    let user = register(&base, &format!("ur_{}", uniq())).await;
    let conv = self_conv(&base, &user).await["id"].as_str().unwrap().to_string();
    let key = uuid::Uuid::new_v4().to_string();
    let upload = |body: &'static [u8]| {
        let base = base.clone();
        let access = user.access.clone();
        let conv = conv.clone();
        let key = key.clone();
        async move {
            http()
                .post(format!("{base}/v1/files"))
                .header("authorization", authz(&access))
                .multipart(
                    reqwest::multipart::Form::new()
                        .text("conversation_id", conv)
                        .text("client_upload_id", key)
                        .part(
                            "file",
                            reqwest::multipart::Part::bytes(body.to_vec())
                                .file_name("retry.txt")
                                .mime_str("text/plain")
                                .unwrap(),
                        ),
                )
                .send()
                .await
                .unwrap()
        }
    };
    let first: Value = upload(b"first").await.json().await.unwrap();
    let second: Value = upload(b"second").await.json().await.unwrap();
    assert_eq!(first["id"], second["id"]);
    assert_eq!(first["size"], 5);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachments WHERE client_upload_id = $1")
            .bind(uuid::Uuid::parse_str(&key).unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn unsent_attachment_delete_and_gc_release_storage_but_sent_files_survive() {
    let (_guard, base, state) =
        spawn_named_state(&format!("chat_t_upload_gc_{}", uniq())).await;
    let user = register(&base, &format!("ugc_{}", uniq())).await;
    let conv = self_conv(&base, &user).await["id"].as_str().unwrap().to_string();
    let files_dir = std::path::PathBuf::from(&state.cfg.files_dir);
    let c = http();

    let (_, manual) = upload_att(&base, &conv, &user.access, "manual.txt", "text/plain", b"manual".to_vec()).await;
    let manual_id = manual["id"].as_str().unwrap().to_string();
    let manual_path = files_dir.join(&manual_id);
    assert!(manual_path.exists());
    assert_eq!(
        c.delete(format!("{base}/v1/files/{manual_id}"))
            .header("authorization", authz(&user.access))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(!manual_path.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachments WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&manual_id).unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );

    let (_, old) = upload_att(&base, &conv, &user.access, "old.txt", "text/plain", b"old".to_vec()).await;
    let old_id = old["id"].as_str().unwrap().to_string();
    let old_path = files_dir.join(&old_id);
    sqlx::query("UPDATE attachments SET created_at = now() - interval '2 days' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&old_id).unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        chat_server::domain::sweep_unattached_attachments(
            &state.pool,
            &state.cfg.files_dir,
            std::time::Duration::from_secs(86400),
        )
        .await
        .unwrap(),
        1
    );
    assert!(!old_path.exists());

    let (_, sent) = upload_att(&base, &conv, &user.access, "sent.txt", "text/plain", b"sent".to_vec()).await;
    let sent_id = sent["id"].as_str().unwrap().to_string();
    let sent_path = files_dir.join(&sent_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &user).await)
        .await
        .unwrap();
    next_of(&mut ws, "presence_init").await;
    ws_send(&mut ws, &conv, json!({"client_msg_id": "gc-sent", "content": "", "attachment_id": sent_id})).await;
    assert_eq!(
        c.delete(format!("{base}/v1/files/{sent_id}"))
            .header("authorization", authz(&user.access))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(sent_path.exists());
    assert_eq!(
        chat_server::domain::sweep_unattached_attachments(
            &state.pool,
            &state.cfg.files_dir,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap(),
        0
    );
    assert!(sent_path.exists());
}

/// 复制一行 attachments（新 id，共享 storage_path/thumb_path），模拟 P2-4 转发副本
async fn copy_attachment(pool: &sqlx::PgPool, src_id: &str) -> String {
    let copy_id = uuid::Uuid::new_v4();
    let n = sqlx::query(
        "INSERT INTO attachments
             (id, uploader_id, conv_id, file_name, mime, size, storage_path, width, height, thumb_path)
         SELECT $1, uploader_id, conv_id, file_name, mime, size, storage_path, width, height, thumb_path
           FROM attachments WHERE id = $2",
    )
    .bind(copy_id)
    .bind(uuid::Uuid::parse_str(src_id).unwrap())
    .execute(pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(n, 1);
    copy_id.to_string()
}

/// WS 入站限速：短时间刷几百帧的连接被断开
#[tokio::test]
async fn ws_inbound_flood_disconnects() {
    let base = spawn().await;
    let a = register(&base, &format!("flood_{}", uniq())).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    for _ in 0..310 {
        if ws.send(WsMsg::Text("{}".into())).await.is_err() {
            break;
        }
    }
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(m) = ws.next().await {
            if matches!(m, Ok(WsMsg::Close(_)) | Err(_)) {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "刷帧的连接应被断开");
}

/// 下载走流式：Range 回 206 片段（音视频拖动要用），类型和下载方式仍按库里记的
#[tokio::test]
async fn download_supports_range_and_keeps_headers() {
    let base = spawn().await;
    let a = register(&base, &format!("rng_{}", uniq())).await;
    let b = register(&base, &format!("rngb_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let body: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let (st, att) = upload_att(
        &base,
        &conv,
        &a.access,
        "clip.mp4",
        "video/mp4",
        body.clone(),
    )
    .await;
    assert_eq!(st, 200, "{att}");
    let url = format!("{base}/v1/files/{}", att["id"].as_str().unwrap());

    let full = http().get(&url).header("authorization", authz(&a.access)).send().await.unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["content-type"], "video/mp4");
    assert!(full.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("inline;"));
    assert_eq!(full.headers()["x-content-type-options"], "nosniff");
    assert_eq!(full.bytes().await.unwrap().to_vec(), body);

    let part = http()
        .get(&url)
        .header("authorization", authz(&a.access))
        .header("range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.headers()["content-range"], "bytes 100-199/10000");
    assert_eq!(part.headers()["content-type"], "video/mp4");
    assert_eq!(
        part.bytes().await.unwrap().to_vec(),
        body[100..200].to_vec()
    );

    let dl = http().get(format!("{url}?dl=1")).header("authorization", authz(&a.access)).send().await.unwrap();
    assert!(dl.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment;"));
    assert_eq!(
        http()
            .get(format!("{base}/v1/files/{}", uuid::Uuid::new_v4()))
            .header("authorization", authz(&a.access))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

/// 大图：上传记录宽高 + 生成缩略图；?thumb=1 拿 JPEG；消息视图字段一致
#[tokio::test]
async fn p2_1_image_dims_thumb() {
    let base = spawn().await;
    let a = register(&base, &format!("p2a_{}", uniq())).await;
    let b = register(&base, &format!("p2b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let (st, att) = upload_att(
        &base,
        &conv,
        &a.access,
        "big.png",
        "image/png",
        png_fixture(1200, 800),
    )
    .await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap();
    assert_eq!(att["width"], 1200);
    assert_eq!(att["height"], 800);
    assert_eq!(att["has_thumb"], true);

    // ?thumb=1 → JPEG，最长边 480
    let dl = c
        .get(format!("{base}/v1/files/{att_id}?thumb=1"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.headers()["content-type"], "image/jpeg");
    let t = image::load_from_memory(&dl.bytes().await.unwrap()).unwrap();
    assert_eq!((t.width(), t.height()), (480, 320));

    // dl=1 忽略 thumb：返回原图
    let dl = c
        .get(format!("{base}/v1/files/{att_id}?thumb=1&dl=1"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.headers()["content-type"], "image/png");

    // 消息视图字段与上传响应一致
    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;
    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "i1", "content": "", "attachment_id": att_id}),
    )
    .await;
    assert_eq!(m["kind"], "image");
    assert_eq!(m["attachment"]["width"], 1200);
    assert_eq!(m["attachment"]["height"], 800);
    assert_eq!(m["attachment"]["has_thumb"], true);
}

/// 小图有宽高无缩略图（?thumb=1 回退原图）；坏图照常入库、字段为 null
#[tokio::test]
async fn p2_1_small_and_broken_image() {
    let base = spawn().await;
    let a = register(&base, &format!("s2a_{}", uniq())).await;
    let b = register(&base, &format!("s2b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();

    let png = png_fixture(300, 200);
    let (st, att) = upload_att(&base, &conv, &a.access, "s.png", "image/png", png.clone()).await;
    assert_eq!(st, 200);
    assert_eq!(att["width"], 300);
    assert_eq!(att["height"], 200);
    assert_eq!(att["has_thumb"], false);
    let dl = c
        .get(format!(
            "{base}/v1/files/{}?thumb=1",
            att["id"].as_str().unwrap()
        ))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(dl.headers()["content-type"], "image/png");
    assert_eq!(dl.bytes().await.unwrap().as_ref(), png.as_slice());

    // 坏图：mime 声明 image/png 但内容不可解码 → 照常入库，尺寸/缩略图缺省
    let junk: Vec<u8> = (0..8192).map(|i| (i * 31 % 256) as u8).collect();
    let (st, att) = upload_att(&base, &conv, &a.access, "b.png", "image/png", junk).await;
    assert_eq!(st, 200);
    assert!(att["width"].is_null());
    assert!(att["height"].is_null());
    assert_eq!(att["has_thumb"], false);
}

/// 撤回消息：附件行保留并隐藏（公开下载 404），物理文件由过期清扫按引用计数删——
/// 仍有副本共享 storage_path 时原图与缩略图都保留
#[tokio::test]
async fn p2_1_delete_cleans_thumb() {
    let base = spawn().await;
    let a = register(&base, &format!("d2a_{}", uniq())).await;
    let b = register(&base, &format!("d2b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let conv = dm_conv(&base, &a).await;
    let c = http();
    let (mut sa, _) = tokio_tungstenite::connect_async(ws_url(&base, &a).await)
        .await
        .unwrap();
    next_of(&mut sa, "presence_init").await;

    let (_, att) = upload_att(
        &base,
        &conv,
        &a.access,
        "d.png",
        "image/png",
        png_fixture(900, 700),
    )
    .await;
    let id = att["id"].as_str().unwrap();
    let file = it_files_dir().join(id);
    let thumb = it_files_dir().join(format!("{id}.thumb.jpg"));
    assert!(file.exists());
    assert!(thumb.exists());

    let m = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "d1", "content": "", "attachment_id": id}),
    )
    .await;
    let r = c
        .delete(format!("{base}/v1/messages/{}", m["id"].as_str().unwrap()))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // 撤回不再删文件：公开下载立即 404，物理文件与缩略图保留（待过期清扫）
    let dl = c
        .get(format!("{base}/v1/files/{id}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 404);
    assert!(file.exists());
    assert!(thumb.exists());

    // 拨过保留期后清扫：独立 storage_path 的原图与缩略图一起删
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let att_uuid = uuid::Uuid::parse_str(id).unwrap();
    assert!(sweep_recalled(&pool, it_files_dir().to_str().unwrap(), &[att_uuid]).await >= 1);
    assert!(!file.exists());
    assert!(!thumb.exists());

    // 共享 storage_path 的副本（模拟转发）存在时，撤回一条不清理物理文件
    let (_, att2) = upload_att(
        &base,
        &conv,
        &a.access,
        "d2.png",
        "image/png",
        png_fixture(900, 700),
    )
    .await;
    let id2 = att2["id"].as_str().unwrap();
    let file2 = it_files_dir().join(id2);
    let thumb2 = it_files_dir().join(format!("{id2}.thumb.jpg"));
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let copy = copy_attachment(&pool, id2).await;
    let m2 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "d2", "content": "", "attachment_id": id2}),
    )
    .await;
    let m3 = ws_send(
        &mut sa,
        &conv,
        json!({"client_msg_id": "d3", "content": "", "attachment_id": copy}),
    )
    .await;

    let r = c
        .delete(format!("{base}/v1/messages/{}", m2["id"].as_str().unwrap()))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // 撤回行过期清扫：副本行仍引用同一 storage_path，物理文件保留
    let id2_uuid = uuid::Uuid::parse_str(id2).unwrap();
    assert!(
        sweep_recalled(&pool, it_files_dir().to_str().unwrap(), &[id2_uuid]).await >= 1
    );
    assert!(file2.exists());
    assert!(thumb2.exists());

    let r = c
        .delete(format!("{base}/v1/messages/{}", m3["id"].as_str().unwrap()))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // 最后一个引用过期：原图与缩略图一起删
    let copy_uuid = uuid::Uuid::parse_str(&copy).unwrap();
    assert!(
        sweep_recalled(&pool, it_files_dir().to_str().unwrap(), &[copy_uuid]).await >= 1
    );
    assert!(!file2.exists());
    assert!(!thumb2.exists());
}

/// 配额：1MB 上限 → 600KB 成功、再 600KB 拒绝；共享行不重复计；0 恢复不限
#[tokio::test]
async fn p2_1_storage_quota() {
    let (_guard, base, state) = spawn_named_state(&format!("chat_t_p21q_{}", uniq())).await;
    let admin = register(&base, &format!("qad_{}", uniq())).await;
    let u = register(&base, &format!("qu_{}", uniq())).await;
    befriend(&base, &admin, &u).await;
    let conv = dm_conv(&base, &u).await;
    let c = http();

    // 缺省不限额
    let s: Value = c
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["used_bytes"], 0);
    assert!(s["quota_bytes"].is_null());

    let r = c
        .put(format!("{base}/v1/admin/settings"))
        .header("authorization", authz(&admin.access))
        .json(&json!({"storage_quota_mb": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["storage_quota_mb"], 1);

    // 600KB 成功；再 600KB → 413 带中文提示
    let blob = vec![7u8; 600 * 1024];
    let (st, att) = upload_att(
        &base,
        &conv,
        &u.access,
        "a.bin",
        "application/octet-stream",
        blob.clone(),
    )
    .await;
    assert_eq!(st, 200);
    let att_id = att["id"].as_str().unwrap().to_string();
    let (st, err) = upload_att(
        &base,
        &conv,
        &u.access,
        "b.bin",
        "application/octet-stream",
        blob,
    )
    .await;
    assert_eq!(st, 413);
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("存储空间已满"),
        "unexpected error body: {err}"
    );

    // me/storage：用量与上限
    let s: Value = c
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["used_bytes"], 614400);
    assert_eq!(s["quota_bytes"], 1048576);

    // 共享 storage_path 的副本不重复计
    copy_attachment(&state.pool, &att_id).await;
    let s: Value = c
        .get(format!("{base}/v1/users/me/storage"))
        .header("authorization", authz(&u.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["used_bytes"], 614400);

    // admin/users 行带 storage_bytes
    let users: Value = c
        .get(format!("{base}/v1/admin/users"))
        .header("authorization", authz(&admin.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = users
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == u.id)
        .unwrap();
    assert_eq!(row["storage_bytes"], 614400);

    // 调 0 → 恢复不限
    let r = c
        .put(format!("{base}/v1/admin/settings"))
        .header("authorization", authz(&admin.access))
        .json(&json!({"storage_quota_mb": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (st, _) = upload_att(
        &base,
        &conv,
        &u.access,
        "c.bin",
        "application/octet-stream",
        vec![9u8; 600 * 1024],
    )
    .await;
    assert_eq!(st, 200);
}

/// 非管理员设配额 403；非法值 400；server-info 不暴露配额
#[tokio::test]
async fn p2_1_quota_admin_and_validation() {
    let (_guard, base) = spawn_named_db(&format!("chat_t_p21v_{}", uniq())).await;
    let admin = register(&base, &format!("vad_{}", uniq())).await;
    let u = register(&base, &format!("vu_{}", uniq())).await;
    let c = http();

    let r = c
        .put(format!("{base}/v1/admin/settings"))
        .header("authorization", authz(&u.access))
        .json(&json!({"storage_quota_mb": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    for v in [json!(-1), json!(1048577), json!("5"), json!(1.5)] {
        let r = c
            .put(format!("{base}/v1/admin/settings"))
            .header("authorization", authz(&admin.access))
            .json(&json!({"storage_quota_mb": v}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{v} 应 400");
    }

    let r = c
        .put(format!("{base}/v1/admin/settings"))
        .header("authorization", authz(&admin.access))
        .json(&json!({"storage_quota_mb": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let info: Value = c
        .get(format!("{base}/v1/server-info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.get("storage_quota_mb").is_none());
}
