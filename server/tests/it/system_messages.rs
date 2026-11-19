//! UI3：群生命周期系统消息（六种 code）+ 不回填历史。
use super::*;

fn code_of(msg: &Value) -> String {
    let content = msg["content"].as_str().unwrap_or("");
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
        .unwrap_or_default()
}

async fn msgs_of(base: &str, u: &TestUser, gid: &str) -> Vec<Value> {
    conv_messages(base, u, gid).await
}

#[tokio::test]
async fn ui3_six_lifecycle_system_codes() {
    let base = spawn().await;
    let a = register(&base, &format!("sys_a_{}", uniq())).await;
    let b = register(&base, &format!("sys_b_{}", uniq())).await;
    let c3 = register(&base, &format!("sys_c_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    befriend(&base, &a, &c3).await;
    let c = http();

    // 1) create → group_created
    let detail: Value = c
        .post(format!("{base}/v1/conversations/group"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "系统群", "member_ids": [b.id]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = detail["id"].as_str().unwrap().to_string();
    let created = msgs_of(&base, &a, &gid).await;
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["kind"], "system");
    assert_eq!(code_of(&created[0]), "group_created");

    // 2) rename → group_renamed
    let r = c
        .patch(format!("{base}/v1/conversations/{gid}"))
        .header("authorization", authz(&a.access))
        .json(&json!({"name": "新名"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let after_rename = msgs_of(&base, &a, &gid).await;
    assert!(after_rename.iter().any(|m| code_of(m) == "group_renamed"));

    // 3) announcement → group_announcement
    let r = c
        .put(format!("{base}/v1/conversations/{gid}/announcement"))
        .header("authorization", authz(&a.access))
        .json(&json!({"text": "今晚八点"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let after_ann = msgs_of(&base, &a, &gid).await;
    assert!(after_ann.iter().any(|m| code_of(m) == "group_announcement"));

    // 4) invite → group_invited
    let r = c
        .post(format!("{base}/v1/conversations/{gid}/members"))
        .header("authorization", authz(&a.access))
        .json(&json!({"user_ids": [c3.id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let after_inv = msgs_of(&base, &a, &gid).await;
    assert!(after_inv.iter().any(|m| code_of(m) == "group_invited"));

    // 5) leave → group_left（B 退出；会话仍在，A 可见）
    let r = c
        .delete(format!("{base}/v1/conversations/{gid}/members/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let after_leave = msgs_of(&base, &a, &gid).await;
    assert!(after_leave.iter().any(|m| code_of(m) == "group_left"));
    // B 列表不应再有该群
    let blist = conv_list(&base, &b).await;
    let bids: Vec<&str> = blist
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["id"].as_str())
        .collect();
    assert!(!bids.contains(&gid.as_str()));

    // 6) dissolve → group_dissolved（消息保留；成员清空）
    let before_dissolve_n = after_leave.len();
    let r = c
        .delete(format!("{base}/v1/conversations/{gid}"))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let gid_uuid = uuid::Uuid::parse_str(&gid).unwrap();
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT kind, content FROM messages WHERE conversation_id = $1 ORDER BY seq",
    )
    .bind(gid_uuid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(rows.iter().any(|(k, content)| {
        k == "system"
            && serde_json::from_str::<Value>(content)
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(|s| s == "group_dissolved"))
                .unwrap_or(false)
    }));
    assert!(rows.len() > before_dissolve_n);
    let members: i64 =
        sqlx::query_scalar("SELECT count(*) FROM memberships WHERE conversation_id = $1")
            .bind(gid_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(members, 0);
}

#[tokio::test]
async fn ui3_no_backfill_preexisting_group_stays_silent() {
    // 直接插库模拟「升级前已存在、无 system 行」的群；跑完生命周期外的读不应凭空补条。
    let base = spawn().await;
    let a = register(&base, &format!("nobf_a_{}", uniq())).await;
    let b = register(&base, &format!("nobf_b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let pool = PgPoolOptions::new().connect(&test_db_url()).await.unwrap();
    let gid = uuid::Uuid::new_v4();
    let a_id = uuid::Uuid::parse_str(&a.id).unwrap();
    let b_id = uuid::Uuid::parse_str(&b.id).unwrap();
    sqlx::query(
        "INSERT INTO conversations (id, type, name, owner_id, last_seq) VALUES ($1, 'group', '旧群', $2, 0)",
    )
    .bind(gid)
    .bind(a_id)
    .execute(&pool)
    .await
    .unwrap();
    for (uid, role) in [(a_id, "owner"), (b_id, "member")] {
        sqlx::query(
            "INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, $3)",
        )
        .bind(gid)
        .bind(uid)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
    }
    // 仅读历史：不得凭空出现 system
    let hist = msgs_of(&base, &a, &gid.to_string()).await;
    assert!(hist.is_empty());
    let sys: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM messages WHERE conversation_id = $1 AND kind = 'system'",
    )
    .bind(gid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sys, 0);
}

#[tokio::test]
async fn ui3_releave_returns_not_a_member() {
    let base = spawn().await;
    let a = register(&base, &format!("rel_a_{}", uniq())).await;
    let b = register(&base, &format!("rel_b_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    let gid = mk_group(&base, &a, &[b.id.clone()]).await;
    let c = http();
    let r = c
        .delete(format!("{base}/v1/conversations/{gid}/members/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = c
        .delete(format!("{base}/v1/conversations/{gid}/members/me"))
        .header("authorization", authz(&b.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"], "not a member of this conversation");
}

#[tokio::test]
async fn kick_posts_group_removed_to_remaining_members() {
    let base = spawn().await;
    let a = register(&base, &format!("kick_a_{}", uniq())).await;
    let b = register(&base, &format!("kick_b_{}", uniq())).await;
    let c3 = register(&base, &format!("kick_c_{}", uniq())).await;
    befriend(&base, &a, &b).await;
    befriend(&base, &a, &c3).await;
    let gid = mk_group(&base, &a, &[b.id.clone(), c3.id.clone()]).await;
    let r = http()
        .delete(format!("{base}/v1/conversations/{gid}/members/{}", b.id))
        .header("authorization", authz(&a.access))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    for u in [&a, &c3] {
        let msgs = msgs_of(&base, u, &gid).await;
        let removed = msgs
            .iter()
            .find(|m| code_of(m) == "group_removed")
            .expect("group_removed system message");
        assert_eq!(removed["kind"], "system");
        let payload: Value = serde_json::from_str(removed["content"].as_str().unwrap()).unwrap();
        assert_eq!(payload["target_name"], b.username.as_str());
        assert!(payload["text"].as_str().unwrap().contains("移出了群聊"));
    }
}
