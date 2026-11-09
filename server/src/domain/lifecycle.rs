use super::*;

// ---------- 消息生命周期 mutation：编辑 / 软删除 / reaction ----------
//
// 统一锁顺序：会话行锁 → 消息行锁（与 post_message、群管理、后续投票一致）。
// 所有 mutation 在会话行锁内重检成员资格；只读 DM（好友已解除）一律拒写。
// edit_version 是消息级单调变更版本：编辑、软删除、reaction 实际变更均 +1，
// 客户端按 (id, edit_version) 合并变更帧，旧版本不得覆盖新版本。

/// 编辑窗口：发送后 15 分钟（以数据库时间判定，测试可回拨 created_at 覆盖）
pub(super) const EDIT_WINDOW: &str = "15 minutes";
/// 撤回窗口：发送后 24 小时（以数据库时间判定）。群主撤他人消息不受此限。
pub(super) const RECALL_WINDOW: &str = "24 hours";
/// 同一用户对单条消息的不同 emoji 上限
pub(super) const MAX_REACTION_KINDS: i64 = 20;

pub(super) struct LockedMessage {
    pub(super) conversation_id: Uuid,
    pub(super) conv_type: String,
    pub(super) my_role: String,
    pub(super) sender_id: Uuid,
    pub(super) kind: String,
    pub(super) deleted_at: Option<DateTime<Utc>>,
    pub(super) edit_version: i32,
    pub(super) edit_op_id: Option<String>,
    pub(super) attachment_id: Option<Uuid>,
    pub(super) edit_expired: bool,
    pub(super) recall_expired: bool,
}

/// 定位消息 → 锁会话行 → 校验成员/可写 → 锁消息行。
/// 非成员对不存在与存在但无权的消息统一 403，不探测存在性。
pub(super) async fn lock_message_for_mutation(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    uid: Uuid,
) -> Result<LockedMessage, PostError> {
    let conv_id =
        sqlx::query_scalar::<_, Uuid>("SELECT conversation_id FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| PostError::deny("not_found", "message not found"))?;

    let conv = lock_conversation(tx, conv_id)
        .await?
        .ok_or_else(|| PostError::deny("forbidden", "not a member of this conversation"))?;
    let (conv_type, _owner, dissolved) = conv;
    let me = membership(&mut **tx, conv_id, uid)
        .await?
        .ok_or_else(|| PostError::deny("forbidden", "not a member of this conversation"))?;
    if dissolved {
        return Err(PostError::deny("forbidden", "not a member of this conversation").into());
    }
    if conv_type == "dm" && !dm_writable(tx, conv_id, uid).await? {
        return Err(PostError::deny("not_friends", "好友关系已解除，会话只读").into());
    }

    #[allow(clippy::type_complexity)]
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<DateTime<Utc>>,
            i32,
            Option<String>,
            Option<Uuid>,
            bool,
            bool,
        ),
    >(
        "SELECT sender_id, kind, deleted_at, edit_version, edit_op_id, attachment_id,
                created_at + $2::interval <= now() AS edit_expired,
                created_at + $3::interval <= now() AS recall_expired
           FROM messages WHERE id = $1 FOR UPDATE",
    )
    .bind(message_id)
    .bind(EDIT_WINDOW)
    .bind(RECALL_WINDOW)
    .fetch_one(&mut **tx)
    .await?;

    Ok(LockedMessage {
        conversation_id: conv_id,
        conv_type,
        my_role: me.role,
        sender_id: row.0,
        kind: row.1,
        deleted_at: row.2,
        edit_version: row.3,
        edit_op_id: row.4,
        attachment_id: row.5,
        edit_expired: row.6,
        recall_expired: row.7,
    })
}

pub struct Edited {
    pub conversation_id: Uuid,
    /// 变更后的读模型行；广播方须按成员逐一再 into_view
    pub row: MsgViewRow,
    pub member_ids: Vec<Uuid>,
    pub edit_version: i32,
}

/// 编辑：仅发送者、仅普通文本、15 分钟窗口内、expected_version 必须匹配当前版本。
/// 同一 client_op_id 重试返回现状（幂等），不二次递增版本。
pub async fn edit_message(
    pool: &PgPool,
    message_id: Uuid,
    editor: Uuid,
    content: &str,
    expected_version: i32,
    op_id: Option<Uuid>,
) -> Result<Edited, PostError> {
    let content = content.trim();
    if content.is_empty() || content.chars().count() > 4000 {
        return Err(PostError::deny(
            "bad_message",
            "message must be 1-4000 chars",
        ));
    }
    let mut tx = pool.begin().await.map_err(PostError::Db)?;
    let ed = edit_in_tx(
        &mut tx,
        message_id,
        editor,
        content,
        expected_version,
        op_id,
    )
    .await?;
    tx.commit().await?;
    Ok(ed)
}

pub(super) async fn edit_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    editor: Uuid,
    content: &str,
    expected_version: i32,
    op_id: Option<Uuid>,
) -> Result<Edited, PostError> {
    let m = lock_message_for_mutation(tx, message_id, editor).await?;
    let op_key = op_id.map(|u| u.to_string());

    // 幂等回放：该 op 已应用过 → 返回现状，不重复递增版本
    if op_key.is_some() && m.edit_op_id.as_deref() == op_key.as_deref() {
        return finish_edit(tx, m.conversation_id, message_id).await;
    }
    if m.sender_id != editor {
        return Err(PostError::deny("forbidden", "only the author can edit").into());
    }
    if m.deleted_at.is_some() {
        return Err(PostError::deny("conflict", "message already deleted").into());
    }
    if m.kind != "text" {
        return Err(PostError::deny("bad_request", "only text messages can be edited").into());
    }
    if m.edit_expired {
        return Err(PostError::deny("bad_request", "edit window expired").into());
    }
    if m.edit_version != expected_version {
        return Err(PostError::deny("conflict", "edit_version conflict").into());
    }

    sqlx::query(
        "UPDATE messages
            SET content = $2, edited_at = now(),
                edit_version = edit_version + 1, edit_op_id = $3
          WHERE id = $1",
    )
    .bind(message_id)
    .bind(content)
    .bind(&op_key)
    .execute(&mut **tx)
    .await?;

    // 群聊重新解析 mentions：提及关系跟随正文（seq 不变，不产生新 unread）
    if m.conv_type == "group" {
        sqlx::query("DELETE FROM message_mentions WHERE message_id = $1")
            .bind(message_id)
            .execute(&mut **tx)
            .await?;
        let members = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT m.user_id, u.username::text FROM memberships m
             JOIN users u ON u.id = m.user_id WHERE m.conversation_id = $1",
        )
        .bind(m.conversation_id)
        .fetch_all(&mut **tx)
        .await?;
        for uid in parse_mentions(content, &members, editor) {
            sqlx::query(
                "INSERT INTO message_mentions (message_id, user_id) VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
            )
            .bind(message_id)
            .bind(uid)
            .execute(&mut **tx)
            .await?;
        }
    }

    finish_edit(tx, m.conversation_id, message_id).await
}

pub(super) async fn finish_edit(
    tx: &mut Transaction<'_, Postgres>,
    conv: Uuid,
    message_id: Uuid,
) -> Result<Edited, PostError> {
    let row = sqlx::query_as::<_, MsgViewRow>("SELECT * FROM message_views WHERE id = $1")
        .bind(message_id)
        .fetch_one(&mut **tx)
        .await?;
    let edit_version = row.edit_version;
    let member_ids = member_ids(&mut **tx, conv).await?;
    Ok(Edited {
        conversation_id: conv,
        row,
        member_ids,
        edit_version,
    })
}

/// 用户已用附件存储（V2 P2-1，MF1 修订）：其上传的附件按 storage_path 去重求和，
/// 转发/记录克隆副本共享同一 storage_path 且不重复计。
/// 克隆行 uploader_id 现已记为转发者：同一 storage_path 只归「最早那行的上传者」
/// （created_at,id 最小的行）计费——克隆行更晚，转发不会推高转发者配额；
/// 最早行被清扫删除后，存活的最早引用自然接手计费。
/// bot 上传的算在主人名下：传主人或他的任一个 bot，都按「主人 + 他全部 bot」合计，
/// 共用主人那一份配额（不然每建一个 bot 就多一份配额）。
pub async fn storage_used(pool: &PgPool, uid: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(size), 0)::bigint
           FROM (SELECT DISTINCT a.storage_path, a.size
                   FROM attachments a
                  WHERE a.uploader_id IN (
                            SELECT u.id FROM users u, users me
                             WHERE me.id = $1
                               AND COALESCE(me.bot_owner_id, me.id) IN (u.id, u.bot_owner_id))
                    AND NOT EXISTS (SELECT 1 FROM attachments o
                                     WHERE o.storage_path = a.storage_path
                                       AND (o.created_at, o.id) < (a.created_at, a.id))) d",
    )
    .bind(uid)
    .fetch_one(pool)
    .await
}

pub struct Recalled {
    pub conversation_id: Uuid,
    /// 墓碑完整读模型行：广播/响应方按成员逐一再 into_view
    pub row: MsgViewRow,
    pub member_ids: Vec<Uuid>,
    /// 这次撤回摘掉了置顶行。软删除不删 messages 行，外键不会级联，调用方据此发 pins_updated。
    pub pins_dropped: bool,
    /// 发起者恰好是该附件的上传者时给出附件 id：客户端可凭它走
    /// GET /files/{id}/recalled 取回、或按 reuse_attachment_id 重发。
    /// 广播墓碑永远不带这个字段（只在 REST 响应、且仅上传者可见）。
    pub recalled_attachment_id: Option<Uuid>,
}

/// 撤回（原软删除改名，语义收紧）：
/// - 发送者可撤回自己的消息，限 created_at 起 24 小时内（RECALL_WINDOW）；
/// - 群主（撤回当下的 role）可撤回群内其他成员的任意消息，不限时；群主自己的消息仍按 24h；
/// - DM/self 会话只有发送者本人；站点 admin（users.role）无额外权力；
/// - 检查顺序：无权 → forbidden；超时 → recall_expired。
///
/// 重复撤回幂等返回现状。seq/last_seq/read cursor 不动；reactions、mentions、pins 随撤回清空。
/// 附件不删行不删文件：置 attachments.recalled_at 隐藏公开下载（视图/列表均掩去），
/// 上传者在 TTL 内可取回重发，过期由 sweep_recalled_attachments 清行并按引用计数删文件。
pub async fn recall_message(
    pool: &PgPool,
    message_id: Uuid,
    uid: Uuid,
    ttl_secs: u64,
) -> Result<Recalled, PostError> {
    let mut tx = pool.begin().await.map_err(PostError::Db)?;
    let d = recall_in_tx(&mut tx, message_id, uid, ttl_secs).await?;
    tx.commit().await?;
    Ok(d)
}

pub(super) async fn recall_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    uid: Uuid,
    ttl_secs: u64,
) -> Result<Recalled, PostError> {
    let m = lock_message_for_mutation(tx, message_id, uid).await?;
    let member_ids = member_ids(&mut **tx, m.conversation_id).await?;

    // 权限先于幂等：无权者（非发送者、非群主）对已撤回消息也只能得到 forbidden，
    // 否则任何人都能借重复撤回在别人墓碑上反复触发广播。
    let is_sender = m.sender_id == uid;
    let allowed = is_sender || (m.conv_type == "group" && m.my_role == "owner");
    if !allowed {
        return Err(PostError::deny("forbidden", "无权撤回这条消息"));
    }

    // 幂等：已撤回再撤回返回现状（重试/重复帧安全，不再校验 24h——撤回早已生效）。
    // 附件仍在保留期且发起者是上传者时照常给出 recalled_attachment_id——
    // 客户端可拿它补齐首次响应丢失的取回能力；已过期/被清扫的行不给（给了也 404）。
    if m.deleted_at.is_some() {
        let row = sqlx::query_as::<_, MsgViewRow>("SELECT * FROM message_views WHERE id = $1")
            .bind(message_id)
            .fetch_one(&mut **tx)
            .await?;
        let recalled_attachment_id = match m.attachment_id {
            Some(att) => sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM attachments
                  WHERE id = $1 AND uploader_id = $2 AND recalled_at IS NOT NULL
                    AND recalled_at + make_interval(secs => $3) > now()",
            )
            .bind(att)
            .bind(uid)
            .bind(ttl_secs as f64)
            .fetch_optional(&mut **tx)
            .await?,
            None => None,
        };
        return Ok(Recalled {
            conversation_id: m.conversation_id,
            row,
            member_ids,
            pins_dropped: false,
            recalled_attachment_id,
        });
    }

    // 仅发送者本人撤回受 24h 限制；群主撤他人消息不限时
    if is_sender && m.recall_expired {
        return Err(PostError::deny("recall_expired", "超过 24 小时的消息不能撤回"));
    }

    // content 物理清空为占位文本（视图同样掩去，双保险；CHECK 要求非空）。
    // 原文不留在任何地方：无 tsvector/编辑历史表，回复预览与会话摘要均实时计算自动掩去。
    sqlx::query(
        "UPDATE messages
            SET deleted_at = now(), deleted_by = $2, edit_version = edit_version + 1,
                content = '消息已撤回'
          WHERE id = $1",
    )
    .bind(message_id)
    .bind(uid)
    .execute(&mut **tx)
    .await?;

    // 墓碑级联：reactions 与由正文派生的 mentions 一并清空
    sqlx::query("DELETE FROM message_reactions WHERE message_id = $1")
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM message_mentions WHERE message_id = $1")
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
    // 软删除不删 messages 行，外键不会级联。摘掉 pin 行，列表查询再滤一次 deleted_at。
    let dropped = sqlx::query("DELETE FROM conversation_pins WHERE message_id = $1")
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
    let pins_dropped = dropped.rows_affected() > 0;

    // 附件保留：行与物理文件都不删，置 recalled_at 隐藏所有公开下载路径（墓碑视图不暴露附件）。
    // 上传者由此获知可重发的附件 id；非上传者发起（群主撤别人）不泄露。
    let mut recalled_attachment_id = None;
    if let Some(att) = m.attachment_id {
        let uploader = sqlx::query_scalar::<_, Uuid>(
            "UPDATE attachments SET recalled_at = now() WHERE id = $1 RETURNING uploader_id",
        )
        .bind(att)
        .fetch_optional(&mut **tx)
        .await?;
        if uploader == Some(uid) {
            recalled_attachment_id = Some(att);
        }
    }

    // 聊天记录级联（MF1）：记录内全部克隆附件（含嵌套，克隆行一律挂最外层记录 id）
    // 随消息同事务标记撤回——公开下载立即 404；行与文件由清扫按 TTL/引用计数处理。
    // 墓碑响应不带 recalled_attachment_id（记录消息 attachment_id 恒 NULL，天然满足）。
    if m.kind == "chat_record" {
        sqlx::query(
            "UPDATE attachments SET recalled_at = now()
              WHERE record_message_id = $1 AND recalled_at IS NULL",
        )
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
    }

    let row = sqlx::query_as::<_, MsgViewRow>("SELECT * FROM message_views WHERE id = $1")
        .bind(message_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(Recalled {
        conversation_id: m.conversation_id,
        row,
        member_ids,
        pins_dropped,
        recalled_attachment_id,
    })
}

/// 撤回附件清扫：删掉 recalled_at 超过 TTL 的附件行，物理文件（与缩略图分别判定）
/// 在提交后按最新引用计数删除——仍有其他 attachment 行引用同一 storage_path 就保留。
/// 单事务内 FOR UPDATE 锁候选行 → messages.attachment_id 显式置 NULL → 删行 → 提交。
/// 重发克隆出的行 recalled_at 为 NULL，永不进清扫集。
/// 返回删除的附件行数。
pub async fn sweep_recalled_attachments(
    pool: &PgPool,
    files_dir: &str,
    ttl: std::time::Duration,
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let expired = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "SELECT id, storage_path, thumb_path FROM attachments
          WHERE recalled_at IS NOT NULL AND recalled_at <= now() - make_interval(secs => $1)
          FOR UPDATE",
    )
    .bind(ttl.as_secs_f64())
    .fetch_all(&mut *tx)
    .await?;
    if expired.is_empty() {
        tx.rollback().await?;
        return Ok(0);
    }
    let ids: Vec<Uuid> = expired.iter().map(|r| r.0).collect();
    // 显式摘掉 messages 引用（不依赖 FK ON DELETE SET NULL 是否就位）
    sqlx::query("UPDATE messages SET attachment_id = NULL WHERE attachment_id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query("DELETE FROM attachments WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;

    // 提交后逐个路径重查引用计数再删文件：行删除与新克隆行插入可能交错。
    // storage_path 与 thumb_path 分开判定（克隆行共享两者，各自独立计数）。
    for (_, path, thumb) in &expired {
        for stored in [Some(path), thumb.as_ref()].into_iter().flatten() {
            let still_used = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM attachments
                  WHERE storage_path = $1 OR thumb_path = $1)",
            )
            .bind(stored)
            .fetch_one(pool)
            .await?;
            if !still_used {
                let fp = crate::api::stored_file_path(files_dir, stored);
                if let Err(e) = tokio::fs::remove_file(&fp).await {
                    // 文件不存在不算错（可能已被上一轮删过）
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %fp.display(), error = %e, "recalled attachment cleanup failed");
                    }
                }
            }
        }
    }
    Ok(n)
}

pub(super) const PIN_CAP: i64 = 20;

/// 置顶结果：无变化（重复置顶 / 取消不存在的行）不广播。
pub struct PinChange {
    pub conversation_id: Uuid,
    pub member_ids: Vec<Uuid>,
    pub changed: bool,
}

/// 置顶本会话未删除的消息。任何成员可操作；只读私聊拒绝。
/// 已置顶再置顶幂等，不改 pinned_at、不广播。
pub async fn pin_message(
    pool: &PgPool,
    conv: Uuid,
    message_id: Uuid,
    uid: Uuid,
) -> Result<PinChange, PostError> {
    let mut tx = pool.begin().await.map_err(PostError::Db)?;
    let changed = pin_in_tx(&mut tx, conv, message_id, uid).await?;
    finish_pin(tx, conv, changed).await
}

pub(super) async fn pin_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    conv: Uuid,
    message_id: Uuid,
    uid: Uuid,
) -> Result<bool, PostError> {
    ensure_pin_writable(tx, conv, uid).await?;
    let msg = sqlx::query_as::<_, (Uuid, Option<DateTime<Utc>>)>(
        "SELECT conversation_id, deleted_at FROM messages WHERE id = $1",
    )
    .bind(message_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| PostError::deny("not_found", "message not found"))?;
    if msg.0 != conv {
        return Err(PostError::deny("bad_request", "message is not in this conversation").into());
    }
    if msg.1.is_some() {
        return Err(PostError::deny("bad_request", "已删除的消息不能置顶").into());
    }
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM conversation_pins WHERE conversation_id = $1 AND message_id = $2)",
    )
    .bind(conv)
    .bind(message_id)
    .fetch_one(&mut **tx)
    .await?;
    if exists {
        return Ok(false);
    }
    let n = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM conversation_pins WHERE conversation_id = $1",
    )
    .bind(conv)
    .fetch_one(&mut **tx)
    .await?;
    if n >= PIN_CAP {
        return Err(PostError::deny("bad_request", "最多置顶 20 条").into());
    }
    sqlx::query(
        "INSERT INTO conversation_pins (conversation_id, message_id, pinned_by)
         VALUES ($1, $2, $3)",
    )
    .bind(conv)
    .bind(message_id)
    .bind(uid)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// 取消置顶。行不存在也成功，且不广播。
pub async fn unpin_message(
    pool: &PgPool,
    conv: Uuid,
    message_id: Uuid,
    uid: Uuid,
) -> Result<PinChange, PostError> {
    let mut tx = pool.begin().await.map_err(PostError::Db)?;
    let changed = unpin_in_tx(&mut tx, conv, message_id, uid).await?;
    finish_pin(tx, conv, changed).await
}

pub(super) async fn unpin_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    conv: Uuid,
    message_id: Uuid,
    uid: Uuid,
) -> Result<bool, PostError> {
    ensure_pin_writable(tx, conv, uid).await?;
    let res =
        sqlx::query("DELETE FROM conversation_pins WHERE conversation_id = $1 AND message_id = $2")
            .bind(conv)
            .bind(message_id)
            .execute(&mut **tx)
            .await?;
    Ok(res.rows_affected() > 0)
}

pub(super) async fn ensure_pin_writable(
    tx: &mut Transaction<'_, Postgres>,
    conv: Uuid,
    uid: Uuid,
) -> Result<(), PostError> {
    let Some((kind, _, dissolved)) = lock_conversation(tx, conv).await? else {
        return Err(PostError::deny("forbidden", "not a member of this conversation").into());
    };
    let member = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM memberships WHERE conversation_id = $1 AND user_id = $2)",
    )
    .bind(conv)
    .bind(uid)
    .fetch_one(&mut **tx)
    .await?;
    if !member || dissolved {
        return Err(PostError::deny("forbidden", "not a member of this conversation").into());
    }
    if kind == "dm" && !dm_writable(tx, conv, uid).await? {
        return Err(PostError::deny("not_friends", "好友关系已解除，会话只读").into());
    }
    Ok(())
}

/// 置顶列表的消息视图：过滤已删除，顺序由调用方按 pinned_at 保持。
pub async fn pinned_message_views(
    pool: &PgPool,
    conv: Uuid,
    viewer: Uuid,
    ids: &[Uuid],
) -> AppResult<Vec<MessageView>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, MsgViewRow>(
        "SELECT * FROM message_views
          WHERE conversation_id = $1 AND id = ANY($2) AND deleted_at IS NULL",
    )
    .bind(conv)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into_view(viewer)).collect())
}

pub(super) async fn finish_pin(
    mut tx: Transaction<'_, Postgres>,
    conv: Uuid,
    changed: bool,
) -> Result<PinChange, PostError> {
    let member_ids = member_ids(&mut *tx, conv).await?;
    tx.commit().await?;
    Ok(PinChange {
        conversation_id: conv,
        member_ids,
        changed,
    })
}

pub struct ReactionChange {
    pub conversation_id: Uuid,
    /// 变更后完整读模型行（含 reactions 聚合）；广播方按接收者 into_view 个性化
    pub row: MsgViewRow,
    pub member_ids: Vec<Uuid>,
}

/// reaction 增删：PK 天然幂等；单用户每消息最多 20 种 emoji；墓碑不可再反应。
/// 真实变更时递增消息 edit_version，驱动客户端 (id, version) 合并。
pub async fn set_reaction(
    pool: &PgPool,
    message_id: Uuid,
    uid: Uuid,
    emoji: &str,
    add: bool,
) -> Result<ReactionChange, PostError> {
    if emoji.is_empty() || emoji.len() > 32 {
        return Err(PostError::deny("bad_request", "invalid emoji"));
    }
    let mut tx = pool.begin().await.map_err(PostError::Db)?;
    let r = reaction_in_tx(&mut tx, message_id, uid, emoji, add).await?;
    tx.commit().await?;
    Ok(r)
}

pub(super) async fn reaction_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    uid: Uuid,
    emoji: &str,
    add: bool,
) -> Result<ReactionChange, PostError> {
    let m = lock_message_for_mutation(tx, message_id, uid).await?;
    if m.deleted_at.is_some() {
        return Err(PostError::deny("conflict", "message already deleted").into());
    }

    let changed = if add {
        let dup = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM message_reactions
              WHERE message_id = $1 AND user_id = $2 AND emoji = $3)",
        )
        .bind(message_id)
        .bind(uid)
        .bind(emoji)
        .fetch_one(&mut **tx)
        .await?;
        if dup {
            false
        } else {
            // 消息行锁串行化并发 reaction：锁内计数，上限判断准确
            let kinds = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM message_reactions
                  WHERE message_id = $1 AND user_id = $2",
            )
            .bind(message_id)
            .bind(uid)
            .fetch_one(&mut **tx)
            .await?;
            if kinds >= MAX_REACTION_KINDS {
                return Err(PostError::deny(
                    "bad_request",
                    "too many reactions on one message (max 20)",
                )
                .into());
            }
            sqlx::query(
                "INSERT INTO message_reactions (message_id, user_id, emoji)
                 VALUES ($1, $2, $3)",
            )
            .bind(message_id)
            .bind(uid)
            .bind(emoji)
            .execute(&mut **tx)
            .await?;
            true
        }
    } else {
        let res = sqlx::query(
            "DELETE FROM message_reactions
              WHERE message_id = $1 AND user_id = $2 AND emoji = $3",
        )
        .bind(message_id)
        .bind(uid)
        .bind(emoji)
        .execute(&mut **tx)
        .await?;
        res.rows_affected() > 0
    };

    // 实际增删时才递增版本；幂等重试保持原版本
    if changed {
        sqlx::query("UPDATE messages SET edit_version = edit_version + 1 WHERE id = $1")
            .bind(message_id)
            .execute(&mut **tx)
            .await?;
    }

    let row = sqlx::query_as::<_, MsgViewRow>("SELECT * FROM message_views WHERE id = $1")
        .bind(message_id)
        .fetch_one(&mut **tx)
        .await?;
    let member_ids = member_ids(&mut **tx, m.conversation_id).await?;

    Ok(ReactionChange {
        conversation_id: m.conversation_id,
        row,
        member_ids,
    })
}
