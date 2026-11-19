//! 群生命周期系统消息（UI3）：kind=system，content 为机器可读 JSON。
//!
//! content 形状：`{"code":"group_left","text":"Alice 退出了群聊","actor_name":"Alice",...}`
//! - `code`：稳定机器码（客户端映射文案的主键）
//! - `text`：中文兜底（列表预览 / 旧客户端）
//! - 其余字段为展示参数（actor_name / group_name / invitee_names）
//!
//! **不回填历史**：只在本票上线后的新事件写入。
//!
//! 解散路径（可测）：同事务内先插入 `group_dissolved` 再标 dissolved_at、清空 memberships；
//! 提交后按捕获的 member_ids 广播系统消息，再 `invalidate conversations`；
//! 客户端本人解散成功时 `applyGroupLeft` 立刻摘列表。

use super::*;
use serde_json::{json, Map, Value};

pub const CODE_CREATED: &str = "group_created";
pub const CODE_RENAMED: &str = "group_renamed";
pub const CODE_ANNOUNCEMENT: &str = "group_announcement";
pub const CODE_INVITED: &str = "group_invited";
pub const CODE_LEFT: &str = "group_left";
pub const CODE_REMOVED: &str = "group_removed";
pub const CODE_DISSOLVED: &str = "group_dissolved";

/// 组装 content JSON；`text` 必填作兜底。
pub fn system_content(code: &str, text: &str, mut params: Map<String, Value>) -> String {
    params.insert("code".into(), Value::String(code.into()));
    params.insert("text".into(), Value::String(text.into()));
    Value::Object(params).to_string()
}

pub fn params_actor(actor_name: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("actor_name".into(), json!(actor_name));
    m
}

pub fn params_actor_group(actor_name: &str, group_name: &str) -> Map<String, Value> {
    let mut m = params_actor(actor_name);
    m.insert("group_name".into(), json!(group_name));
    m
}

pub fn params_invite(actor_name: &str, invitee_names: &[String]) -> Map<String, Value> {
    let mut m = params_actor(actor_name);
    m.insert("invitee_names".into(), json!(invitee_names));
    m
}

pub fn params_remove(actor_name: &str, target_name: &str) -> Map<String, Value> {
    let mut m = params_actor(actor_name);
    m.insert("target_name".into(), json!(target_name));
    m
}

/// 当前用户展示名（缺省回落 username）。
pub async fn display_name(
    executor: impl PgExecutor<'_>,
    uid: Uuid,
) -> Result<String, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT display_name, username::text FROM users WHERE id = $1",
    )
    .bind(uid)
    .fetch_one(executor)
    .await?;
    let (dn, un) = row;
    let t = dn.trim();
    Ok(if t.is_empty() { un } else { dn })
}

/// 批量展示名，顺序与 ids 一致；缺行跳过。
pub async fn display_names(
    executor: impl PgExecutor<'_>,
    ids: &[Uuid],
) -> Result<Vec<String>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, display_name, username::text FROM users WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(executor)
    .await?;
    let mut by_id: std::collections::HashMap<Uuid, String> = rows
        .into_iter()
        .map(|(id, dn, un)| {
            let t = dn.trim();
            (id, if t.is_empty() { un } else { dn })
        })
        .collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// 在已持有会话行锁的事务内写入一条系统消息（不走普通 send 的解散/成员前置校验）。
/// 调用方保证：会话仍存在；解散场景须在标 dissolved_at **之前**调用。
pub async fn insert_system_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    conv: Uuid,
    actor_id: Uuid,
    content: &str,
) -> Result<MessageView, sqlx::Error> {
    let seq = sqlx::query_scalar::<_, i64>(
        "UPDATE conversations SET last_seq = last_seq + 1 WHERE id = $1 RETURNING last_seq",
    )
    .bind(conv)
    .fetch_one(&mut **tx)
    .await?;

    let message_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages
            (id, conversation_id, seq, sender_id, content, kind)
         VALUES ($1, $2, $3, $4, $5, 'system')",
    )
    .bind(message_id)
    .bind(conv)
    .bind(seq)
    .bind(actor_id)
    .bind(content)
    .execute(&mut **tx)
    .await?;

    // 发起者已读推到本条，避免自己的系统条算未读
    sqlx::query(
        "UPDATE memberships SET last_read_seq = $3
          WHERE conversation_id = $1 AND user_id = $2 AND last_read_seq < $3",
    )
    .bind(conv)
    .bind(actor_id)
    .bind(seq)
    .execute(&mut **tx)
    .await?;

    view_by_id(&mut **tx, actor_id, message_id).await
}

/// 独立事务写入并返回 Posted（member_ids = 当前成员）。用于改名/公告/建群后等已提交路径。
pub async fn post_system(
    pool: &PgPool,
    conv: Uuid,
    actor_id: Uuid,
    content: &str,
) -> Result<Posted, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // 与普通发消息同锁，避免与解散交错
    let locked = sqlx::query_scalar::<_, bool>(
        "SELECT dissolved_at IS NOT NULL FROM conversations WHERE id = $1 FOR UPDATE",
    )
    .bind(conv)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(dissolved) = locked else {
        return Err(sqlx::Error::RowNotFound);
    };
    if dissolved {
        return Err(sqlx::Error::RowNotFound);
    }
    let message = insert_system_in_tx(&mut tx, conv, actor_id, content).await?;
    let member_ids =
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM memberships WHERE conversation_id = $1")
            .bind(conv)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Posted {
        message,
        inserted: true,
        member_ids,
        unarchived: Vec::new(),
    })
}
