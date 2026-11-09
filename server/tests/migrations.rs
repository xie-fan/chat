use chat_server::domain;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use uuid::Uuid;

// MM5：所有测试库经 TempDb 守卫创建（作用域结束含 panic 即删库）。
#[path = "common/testdb.rs"]
mod testdb;

#[tokio::test]
async fn upgrade_preserves_shared_attachment_references_and_cleanup() {
    let db = testdb::TempDb::create(&format!("chat_t_mig_{}", Uuid::new_v4().simple())).await;
    let options = PgConnectOptions::from_str(&testdb::admin_db_url()).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(db.name()))
        .await
        .unwrap();
    for script in [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_attachments.sql"),
        include_str!("../migrations/0003_mentions.sql"),
        include_str!("../migrations/0004_admin.sql"),
    ] {
        sqlx::raw_sql(script).execute(&pool).await.unwrap();
    }

    let user = Uuid::new_v4();
    let conv = Uuid::new_v4();
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash) VALUES ($1,'migration_user','Migration','unused')")
        .bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO conversations (id,type,name,owner_id,last_seq) VALUES ($1,'group','Migration',$2,3)")
        .bind(conv).bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO memberships (conversation_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(conv)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachments (id,uploader_id,conv_id,file_name,mime,size,storage_path,created_at) VALUES ($1,$2,$3,'shared.txt','text/plain',7,'migration-shared-path','2026-01-01T00:00:00Z')")
        .bind(attachment).bind(user).bind(conv).execute(&pool).await.unwrap();
    let mut ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    ids.sort();
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO messages (id,conversation_id,seq,sender_id,content,kind,attachment_id,created_at) VALUES ($1,$2,$3,$4,'[file] shared.txt','file',$5,'2026-01-02T00:00:00Z')")
            .bind(id).bind(conv).bind((index + 1) as i64).bind(user).bind(attachment)
            .execute(&pool).await.unwrap();
    }
    for script in [
        include_str!("../migrations/0005_sync_invariants.sql"),
        include_str!("../migrations/0006_session_version.sql"),
        include_str!("../migrations/0007_message_lifecycle.sql"),
        include_str!("../migrations/0008_conversation_preferences.sql"),
        include_str!("../migrations/0009_polls.sql"),
        // 新迁移必须追加到这里：夹具之后调用的领域函数读取最新 message_views 列
        include_str!("../migrations/0010_server_settings_invites.sql"),
        include_str!("../migrations/0011_sessions.sql"),
        include_str!("../migrations/0012_v2_media.sql"),
        include_str!("../migrations/0015_self_conversations.sql"),
        include_str!("../migrations/0016_search_trgm.sql"),
        include_str!("../migrations/0017_announcements_pins.sql"),
        include_str!("../migrations/0018_push.sql"),
        include_str!("../migrations/0019_agents.sql"),
        include_str!("../migrations/0020_agent_settings.sql"),
        include_str!("../migrations/0021_agent_queue.sql"),
        include_str!("../migrations/0022_agent_contexts.sql"),
        include_str!("../migrations/0023_agent_config.sql"),
        include_str!("../migrations/0024_agent_steps.sql"),
        include_str!("../migrations/0025_agent_access.sql"),
        include_str!("../migrations/0026_agent_attachments.sql"),
        include_str!("../migrations/0027_agent_schedules.sql"),
        include_str!("../migrations/0028_agent_approvals.sql"),
        include_str!("../migrations/0029_agent_calls.sql"),
        include_str!("../migrations/0030_invocation_root_index.sql"),
        include_str!("../migrations/0031_agent_context_epoch.sql"),
        include_str!("../migrations/0032_host_offline_grace.sql"),
        include_str!("../migrations/0033_agent_workspaces.sql"),
        include_str!("../migrations/0034_agent_scratch_workspace.sql"),
        include_str!("../migrations/0035_drop_agents.sql"),
        include_str!("../migrations/0036_bots.sql"),
        include_str!("../migrations/0037_bot_tokens.sql"),
        include_str!("../migrations/0038_push_devices.sql"),
        include_str!("../migrations/0039_message_recall.sql"),
        include_str!("../migrations/0040_chat_record.sql"),
        include_str!("../migrations/0041_bot_delivery.sql"),
    ] {
        sqlx::raw_sql(script).execute(&pool).await.unwrap();
    }
    // 0039 起发送者撤回限 24h（库 now() 判定）：把夹具消息拨到当下使其可撤回
    sqlx::query("UPDATE messages SET created_at = now()")
        .execute(&pool)
        .await
        .unwrap();

    let references = sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "SELECT id,attachment_id FROM messages ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(references.len(), 3);
    assert!(
        references.iter().all(|(_, att)| att.is_some()),
        "upgrade must preserve every historical attachment association"
    );
    assert_eq!(references[0], (ids[0], Some(attachment)));
    let distinct =
        sqlx::query_scalar::<_, i64>("SELECT count(DISTINCT attachment_id) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(distinct, 3);
    let preserved = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM attachments WHERE uploader_id=$1 AND conv_id=$2 AND file_name='shared.txt' AND mime='text/plain' AND size=7 AND storage_path='migration-shared-path' AND created_at='2026-01-01T00:00:00Z'",
    )
    .bind(user)
    .bind(conv)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preserved, 3);
    let visible =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM message_views WHERE att_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(visible, 3);

    // 0039 撤回语义：附件行不删，标 recalled_at 并在读模型掩去；物理文件由清扫按引用计数删
    let att_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT attachment_id FROM messages ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(att_ids.len(), 3);
    let files_dir =
        std::env::temp_dir().join(format!("chat-migration-files-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("migration-shared-path"), b"shared").unwrap();

    for (index, id) in ids.iter().enumerate() {
        let recalled = domain::recall_message(&pool, *id, user, 86400)
            .await
            .unwrap();
        // 发起者是上传者 → REST 响应带撤回附件 id
        assert_eq!(recalled.recalled_attachment_id, Some(att_ids[index]));
        let remaining = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM message_views WHERE att_id IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 2 - index as i64);
    }
    let marked = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM attachments WHERE recalled_at IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(marked, 3, "撤回只打 recalled_at，不删附件行");

    // 前两行先过期清扫：行删、messages 引用摘掉，但物理文件被第三行引用而保留
    sqlx::query(
        "UPDATE attachments SET recalled_at = now() - interval '2 days' WHERE id = ANY($1)",
    )
    .bind(&att_ids[..2])
    .execute(&pool)
    .await
    .unwrap();
    let n = domain::sweep_recalled_attachments(
        &pool,
        files_dir.to_str().unwrap(),
        std::time::Duration::from_secs(86400),
    )
    .await
    .unwrap();
    assert_eq!(n, 2);
    assert!(
        files_dir.join("migration-shared-path").exists(),
        "仍有行引用 storage_path 时文件不得删除"
    );

    // 第三行过期：最后一个引用消失，物理文件删除
    sqlx::query(
        "UPDATE attachments SET recalled_at = now() - interval '2 days' WHERE id = $1",
    )
    .bind(att_ids[2])
    .execute(&pool)
    .await
    .unwrap();
    let n = domain::sweep_recalled_attachments(
        &pool,
        files_dir.to_str().unwrap(),
        std::time::Duration::from_secs(86400),
    )
    .await
    .unwrap();
    assert_eq!(n, 1);
    assert!(!files_dir.join("migration-shared-path").exists());
    let dangling =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE attachment_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dangling, 0, "清扫后消息上的附件引用必须置空");
    std::fs::remove_dir_all(&files_dir).unwrap();
    // 0035 把 Agent 的表、列和设置全部拿掉
    let leftovers = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT count(*) FROM information_schema.tables WHERE table_name LIKE 'agent%')
              + (SELECT count(*) FROM information_schema.columns
                  WHERE column_name IN ('agent_id', 'agent_access', 'agent_steps', 'workspace_id'))
              + (SELECT count(*) FROM server_settings WHERE key LIKE 'agent%')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        leftovers, 0,
        "0035 drops every agent table, column and setting"
    );

    pool.close().await;
}

/// V2 P2-4：0015 为 0015 之前已存在的用户回填 type='self' 会话与 owner 成员；
/// 重复执行幂等（uq_self_conversation 部分唯一索引 + memberships PK 兜底）。
#[tokio::test]
async fn upgrade_backfills_self_conversations() {
    let db = testdb::TempDb::create(&format!("chat_t_mig_{}", Uuid::new_v4().simple())).await;
    let options = PgConnectOptions::from_str(&testdb::admin_db_url()).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(db.name()))
        .await
        .unwrap();
    for script in [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_attachments.sql"),
        include_str!("../migrations/0003_mentions.sql"),
        include_str!("../migrations/0004_admin.sql"),
        include_str!("../migrations/0005_sync_invariants.sql"),
        include_str!("../migrations/0006_session_version.sql"),
        include_str!("../migrations/0007_message_lifecycle.sql"),
        include_str!("../migrations/0008_conversation_preferences.sql"),
        include_str!("../migrations/0009_polls.sql"),
        include_str!("../migrations/0010_server_settings_invites.sql"),
        include_str!("../migrations/0011_sessions.sql"),
        include_str!("../migrations/0012_v2_media.sql"),
    ] {
        sqlx::raw_sql(script).execute(&pool).await.unwrap();
    }

    // 旧 schema 上的两个既有用户
    let u1 = Uuid::new_v4();
    let u2 = Uuid::new_v4();
    for (u, name) in [(u1, "old_user_a"), (u2, "old_user_b")] {
        sqlx::query(
            "INSERT INTO users (id,username,display_name,password_hash) VALUES ($1,$2,$3,'unused')",
        )
        .bind(u)
        .bind(name)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }

    // 0015 跑两遍：回填必须幂等（不重复建会话、不重复建成员）
    for _ in 0..2 {
        sqlx::raw_sql(include_str!("../migrations/0015_self_conversations.sql"))
            .execute(&pool)
            .await
            .unwrap();
    }

    let total =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversations WHERE type = 'self'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 2, "每个用户恰一个 self 会话");

    for u in [u1, u2] {
        let (name_null, owners): (bool, i64) = sqlx::query_as(
            "SELECT c.name IS NULL,
                    (SELECT count(*) FROM memberships mb
                      WHERE mb.conversation_id = c.id AND mb.user_id = $1 AND mb.role = 'owner')
               FROM conversations c
              WHERE c.type = 'self' AND c.owner_id = $1",
        )
        .bind(u)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(name_null, "self 会话 name 必须为 NULL");
        assert_eq!(owners, 1, "self 会话恰一个 owner 成员");
    }

    // 注册/兜底共用的领域函数在已迁移库上同样幂等
    let again = domain::ensure_self_conversation(&pool, u1).await.unwrap();
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM conversations WHERE type = 'self' AND owner_id = $1",
    )
    .bind(u1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(total, 1);
    let member_count =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM memberships WHERE conversation_id = $1")
            .bind(again)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(member_count, 1);

    // 0018：push_settings 建表
    sqlx::raw_sql(include_str!("../migrations/0018_push.sql"))
        .execute(&pool)
        .await
        .unwrap();

    pool.close().await;
}

/// V2 P3-1：0016 在已有数据上创建 pg_trgm 扩展与 messages.content 部分 GIN 索引。
#[tokio::test]
async fn upgrade_creates_search_trgm_index() {
    let db = testdb::TempDb::create(&format!("chat_t_mig_{}", Uuid::new_v4().simple())).await;
    let options = PgConnectOptions::from_str(&testdb::admin_db_url()).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(db.name()))
        .await
        .unwrap();
    for script in [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_attachments.sql"),
        include_str!("../migrations/0003_mentions.sql"),
        include_str!("../migrations/0004_admin.sql"),
        include_str!("../migrations/0005_sync_invariants.sql"),
        include_str!("../migrations/0006_session_version.sql"),
        include_str!("../migrations/0007_message_lifecycle.sql"),
        include_str!("../migrations/0008_conversation_preferences.sql"),
        include_str!("../migrations/0009_polls.sql"),
        include_str!("../migrations/0010_server_settings_invites.sql"),
        include_str!("../migrations/0011_sessions.sql"),
        include_str!("../migrations/0012_v2_media.sql"),
        include_str!("../migrations/0015_self_conversations.sql"),
    ] {
        sqlx::raw_sql(script).execute(&pool).await.unwrap();
    }

    // 0016 之前已存在的数据：一条活消息 + 一条墓碑（content 已被占位文本覆写）
    let u = Uuid::new_v4();
    let conv = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash) VALUES ($1,'trgm_user','Trgm','unused')")
        .bind(u).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO conversations (id,type,name,owner_id) VALUES ($1,'group','Trgm',$2)")
        .bind(conv)
        .bind(u)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memberships (conversation_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(conv)
        .bind(u)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO messages (conversation_id,seq,sender_id,content) VALUES ($1,1,$2,'live keyword')")
        .bind(conv).bind(u).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO messages (conversation_id,seq,sender_id,content,deleted_at) VALUES ($1,2,$2,'消息已删除',now())")
        .bind(conv).bind(u).execute(&pool).await.unwrap();

    sqlx::raw_sql(include_str!("../migrations/0016_search_trgm.sql"))
        .execute(&pool)
        .await
        .unwrap();

    let ext = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(ext, "pg_trgm extension must exist");
    let idx = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_indexes
          WHERE tablename = 'messages' AND indexname = 'idx_messages_content_trgm')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(idx, "trgm index must exist on existing data");

    // 部分索引只覆盖未删除行：活消息可命中，墓碑行被 deleted_at 条件排除
    let hits = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM messages
          WHERE deleted_at IS NULL AND content ILIKE '%keyword%' ESCAPE '\\'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(hits, 1);

    pool.close().await;
}

/// MF1 0040：messages.record jsonb、attachments.record_message_id + 部分索引、
/// client_msg_id 上限放宽到 120、message_views 追加末尾列 record（墓碑掩 NULL）。
#[tokio::test]
async fn upgrade_0040_chat_record() {
    let db = testdb::TempDb::create(&format!("chat_t_mig_{}", Uuid::new_v4().simple())).await;
    let options = PgConnectOptions::from_str(&testdb::admin_db_url()).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(db.name()))
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    // 列与索引
    let cols: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name, data_type FROM information_schema.columns
          WHERE table_name IN ('messages','attachments')
            AND column_name IN ('record','record_message_id')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(cols.contains(&("record".into(), "jsonb".into())), "{cols:?}");
    assert!(cols.contains(&("record_message_id".into(), "uuid".into())));
    let idx = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_indexes
          WHERE tablename = 'attachments' AND indexname = 'idx_attachments_record_message'
            AND indexdef LIKE '%WHERE%record_message_id%')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(idx, "record_message_id 需要部分索引");

    // message_views 最后一列必须是 record（只允许追加）
    let last_col: String = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
          WHERE table_name = 'message_views'
          ORDER BY ordinal_position DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(last_col, "record");

    // client_msg_id 上限 120：120 通过、121 被拒
    let u = Uuid::new_v4();
    let conv = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash) VALUES ($1,'mig40','Mig40','x')")
        .bind(u).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO conversations (id,type,name,owner_id,last_seq) VALUES ($1,'group','G',$2,0)")
        .bind(conv).bind(u).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO memberships (conversation_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(conv).bind(u).execute(&pool).await.unwrap();
    let k120 = "k".repeat(120);
    sqlx::query("INSERT INTO messages (conversation_id,seq,sender_id,content,client_msg_id) VALUES ($1,1,$2,'x',$3)")
        .bind(conv).bind(u).bind(&k120).execute(&pool).await.unwrap();
    let too_long = "k".repeat(121);
    let e = sqlx::query("INSERT INTO messages (conversation_id,seq,sender_id,content,client_msg_id) VALUES ($1,2,$2,'x',$3)")
        .bind(conv).bind(u).bind(&too_long).execute(&pool).await;
    assert!(e.is_err(), "121 字符的 client_msg_id 必须被 CHECK 拒掉");

    // 墓碑掩 record：deleted_at 非空时视图里 record 为 NULL
    sqlx::query(
        "UPDATE messages SET record = '{\"v\":1}'::jsonb, deleted_at = now() WHERE seq = 1",
    )
    .execute(&pool)
    .await
    .unwrap();
    let rec: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT record FROM message_views WHERE seq = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(rec.is_none(), "墓碑消息的 record 必须掩去");

    pool.close().await;
}

/// 0035：Agent 专属会话整个删掉；Agent 在群里发的消息删掉，人发的留下。
#[tokio::test]
async fn drop_agents_removes_agent_messages_and_conversations() {
    let db = testdb::TempDb::create(&format!("chat_t_mig_{}", Uuid::new_v4().simple())).await;
    let options = PgConnectOptions::from_str(&testdb::admin_db_url()).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(db.name()))
        .await
        .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let (before, after): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|p| p.file_name().unwrap().to_str().unwrap() < "0035");
    for p in &before {
        sqlx::raw_sql(&std::fs::read_to_string(p).unwrap())
            .execute(&pool)
            .await
            .unwrap();
    }

    let owner = Uuid::new_v4();
    let group = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash) VALUES ($1,'owner','Owner','x')")
        .bind(owner).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO conversations (id,type,name,owner_id,last_seq) VALUES ($1,'group','G',$2,2)",
    )
    .bind(group)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    let host: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_hosts (user_id,install_id) VALUES ($1,gen_random_uuid()) RETURNING id",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    let agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agents (owner_id,host_id,name,runtime) VALUES ($1,$2,'bot','{}') RETURNING id",
    )
    .bind(owner)
    .bind(host)
    .fetch_one(&pool)
    .await
    .unwrap();
    let agent_conv: Uuid = sqlx::query_scalar("INSERT INTO conversations (type,owner_id,agent_id,last_seq) VALUES ('agent',$1,$2,1) RETURNING id")
        .bind(owner).bind(agent).fetch_one(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO messages (conversation_id,seq,sender_id,content) VALUES ($1,1,$2,'hi agent')",
    )
    .bind(agent_conv)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (conversation_id,seq,sender_id,content) VALUES ($1,1,$2,'@bot hi')",
    )
    .bind(group)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO messages (conversation_id,seq,sender_id,content,agent_id) VALUES ($1,2,$2,'reply',$3)")
        .bind(group).bind(owner).bind(agent).execute(&pool).await.unwrap();

    for p in &after {
        sqlx::raw_sql(&std::fs::read_to_string(p).unwrap())
            .execute(&pool)
            .await
            .unwrap();
    }

    let left: Vec<String> =
        sqlx::query_scalar("SELECT content FROM message_views WHERE conversation_id = $1")
            .bind(group)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(left, vec!["@bot hi".to_string()]);
    let agent_convs: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations WHERE id = $1")
        .bind(agent_conv)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(agent_convs, 0);

    pool.close().await;
}
