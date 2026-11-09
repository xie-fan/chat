//! Webhook bot：主人名下的特殊账号（users.bot_owner_id）。不能登录、搜不到、收不了好友申请。
//! 入站：POST /v1/hooks 带 Authorization: Bearer <令牌>，以 bot 身份发消息（默认发到和主人的私聊）。
//! 一个 bot 可以有多个令牌，分给不同程序、单独删除。
//! 出站：bot 在私聊里收到消息、或在群里被 @ 时，服务端把消息 POST 到 webhook_url，
//! 用 secret 签名；响应 JSON 里的 content 作为 bot 的回复发回同一会话。
//! Bot API：同一个令牌还能读会话和历史、拉取新消息（GET /v1/bot/updates，和回调同样的事件）、
//! 传文件、改/撤自己的消息。给 agent 用的描述在 GET /v1/bot/openapi.json（bot_openapi.json）。

use super::*;
use sha2::{Digest, Sha256};

/// 每人最多这么多 bot（防手滑刷号）
const MAX_BOTS: i64 = 20;
/// 每个 bot 最多这么多入站令牌
const MAX_TOKENS: i64 = 20;
const WEBHOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 回调响应只读这么多字节；超出当作无回复
const REPLY_MAX_BYTES: usize = 64 * 1024;
/// GET /bot/updates 长轮询最多等这么多秒：低于常见反向代理 60 秒的读超时
const UPDATES_MAX_WAIT_SECS: u64 = 50;

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct BotView {
    id: Uuid,
    username: String,
    display_name: String,
    webhook_url: String,
    /// 只有主人看得到。回调请求用它签名
    secret: String,
    /// 入站令牌 [{id, name, token, created_at}]，按创建时间排
    tokens: sqlx::types::Json<Value>,
    created_at: DateTime<Utc>,
}

const BOT_SELECT: &str = "SELECT u.id, u.username::text AS username, u.display_name,
        b.webhook_url, b.secret, b.created_at,
        COALESCE((SELECT json_agg(json_build_object(
                    'id', t.id, 'name', t.name, 'token', t.token, 'created_at', t.created_at)
                  ORDER BY t.created_at)
                    FROM bot_tokens t WHERE t.bot_id = b.user_id), '[]') AS tokens
   FROM bots b JOIN users u ON u.id = b.user_id";

#[derive(Deserialize)]
pub(super) struct CreateBotReq {
    username: String,
    display_name: Option<String>,
    #[serde(default)]
    webhook_url: String,
}

#[derive(Deserialize)]
pub(super) struct PatchBotReq {
    display_name: Option<String>,
    webhook_url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateTokenReq {
    name: String,
}

#[derive(Deserialize)]
pub(super) struct HookReq {
    /// 带附件时可以为空
    #[serde(default)]
    content: String,
    /// 缺省 = bot 和主人的私聊
    conversation_id: Option<Uuid>,
    /// 幂等键（1–80 字符）：同一 bot 同一会话重发同一个值，返回第一次那条消息、不重复发
    client_msg_id: Option<String>,
    /// POST /bot/files 上传到同一会话拿到的附件 id
    attachment_id: Option<Uuid>,
    /// 回复同一会话里的哪条消息
    reply_to_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub(super) struct UpdatesQuery {
    /// 没有新消息时最多等几秒；缺省 0 = 立即返回
    #[serde(default)]
    timeout: u64,
    limit: Option<usize>,
}

fn new_token() -> String {
    let mut buf = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut buf);
    hex::encode(buf)
}

/// 空串 = 不回调；否则 http/https、≤500 字符
fn normalize_webhook(raw: &str) -> AppResult<String> {
    let url = raw.trim();
    let ok = url.is_empty()
        || (url.len() <= 500
            && (url.starts_with("http://") || url.starts_with("https://"))
            && !url.chars().any(char::is_whitespace));
    if !ok {
        return Err(AppError::bad_request("webhook_url 必须是 http/https 地址"));
    }
    Ok(url.to_string())
}

fn normalize_display(raw: &str) -> AppResult<String> {
    let name = raw.trim();
    if name.is_empty() || name.len() > 50 {
        return Err(AppError::bad_request("display_name must be 1-50 chars"));
    }
    Ok(name.to_string())
}

async fn load_bot(st: &SharedState, owner: Uuid, bot: Uuid) -> AppResult<BotView> {
    sqlx::query_as::<_, BotView>(&format!(
        "{BOT_SELECT} WHERE b.user_id = $1 AND b.owner_id = $2"
    ))
    .bind(bot)
    .bind(owner)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("bot not found"))
}

/// GET /bots：本人的 bot
pub(super) async fn list_bots(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<BotView>>> {
    let uid = uid_of(&headers, &st).await?;
    let rows = sqlx::query_as::<_, BotView>(&format!(
        "{BOT_SELECT} WHERE b.owner_id = $1 ORDER BY b.created_at"
    ))
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

/// POST /bots：建账号 + 和主人成为好友 + 建私聊
pub(super) async fn create_bot(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<CreateBotReq>,
) -> AppResult<Json<BotView>> {
    let uid = uid_of(&headers, &st).await?;
    let username = req.username.trim();
    if !account::valid_username(username) {
        return Err(AppError::bad_request(
            "username must be 3-20 chars of [a-zA-Z0-9_]",
        ));
    }
    let display = normalize_display(req.display_name.as_deref().unwrap_or(username))?;
    let webhook = normalize_webhook(&req.webhook_url)?;

    let mut tx = st.pool.begin().await?;
    // 主人行锁：并发创建也数得准上限
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(uid)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM bots WHERE owner_id = $1")
        .bind(uid)
        .fetch_one(&mut *tx)
        .await?;
    if n >= MAX_BOTS {
        return Err(AppError::bad_request("bot 数量已达上限"));
    }
    // 密码哈希填不可解析的占位：verify_password 恒 false，bot 无法登录
    let bot = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (username, display_name, password_hash, bot_owner_id)
         VALUES ($1, $2, '!', $3) RETURNING id",
    )
    .bind(username)
    .bind(&display)
    .bind(uid)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::conflict("username taken")
        } else {
            e.into()
        }
    })?;
    let (lo, hi) = domain::canonical_pair(uid, bot);
    sqlx::query("INSERT INTO friendships (user_a, user_b) VALUES ($1, $2)")
        .bind(lo)
        .bind(hi)
        .execute(&mut *tx)
        .await?;
    domain::ensure_dm(&mut tx, uid, bot).await?;
    sqlx::query(
        "INSERT INTO bots (user_id, owner_id, webhook_url, secret) VALUES ($1, $2, $3, $4)",
    )
    .bind(bot)
    .bind(uid)
    .bind(&webhook)
    .bind(new_token())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    notify(&st, &[uid], "friends");
    notify(&st, &[uid], "conversations");
    Ok(Json(load_bot(&st, uid, bot).await?))
}

/// PATCH /bots/{id}：改昵称 / 回调地址
pub(super) async fn patch_bot(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(bot): Path<Uuid>,
    Json(req): Json<PatchBotReq>,
) -> AppResult<Json<BotView>> {
    let uid = uid_of(&headers, &st).await?;
    load_bot(&st, uid, bot).await?;
    if let Some(name) = &req.display_name {
        sqlx::query("UPDATE users SET display_name = $2 WHERE id = $1")
            .bind(bot)
            .bind(normalize_display(name)?)
            .execute(&st.pool)
            .await?;
    }
    if let Some(url) = &req.webhook_url {
        sqlx::query("UPDATE bots SET webhook_url = $2 WHERE user_id = $1")
            .bind(bot)
            .bind(normalize_webhook(url)?)
            .execute(&st.pool)
            .await?;
    }
    notify(&st, &[uid], "friends");
    Ok(Json(load_bot(&st, uid, bot).await?))
}

/// POST /bots/{id}/secret：换签名密钥
pub(super) async fn reset_bot_secret(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(bot): Path<Uuid>,
) -> AppResult<Json<BotView>> {
    let uid = uid_of(&headers, &st).await?;
    load_bot(&st, uid, bot).await?;
    sqlx::query("UPDATE bots SET secret = $2 WHERE user_id = $1")
        .bind(bot)
        .bind(new_token())
        .execute(&st.pool)
        .await?;
    Ok(Json(load_bot(&st, uid, bot).await?))
}

/// POST /bots/{id}/tokens：加一个入站令牌
pub(super) async fn create_bot_token(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(bot): Path<Uuid>,
    Json(req): Json<CreateTokenReq>,
) -> AppResult<Json<BotView>> {
    let uid = uid_of(&headers, &st).await?;
    load_bot(&st, uid, bot).await?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 50 {
        return Err(AppError::bad_request("令牌名称 1-50 个字"));
    }
    let mut tx = st.pool.begin().await?;
    // bot 行锁：并发添加也数得准上限
    sqlx::query("SELECT 1 FROM bots WHERE user_id = $1 FOR UPDATE")
        .bind(bot)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM bot_tokens WHERE bot_id = $1")
        .bind(bot)
        .fetch_one(&mut *tx)
        .await?;
    if n >= MAX_TOKENS {
        return Err(AppError::bad_request("令牌数量已达上限"));
    }
    sqlx::query("INSERT INTO bot_tokens (bot_id, name, token) VALUES ($1, $2, $3)")
        .bind(bot)
        .bind(name)
        .bind(new_token())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(load_bot(&st, uid, bot).await?))
}

/// DELETE /bots/{id}/tokens/{token_id}：删令牌，立即失效
pub(super) async fn delete_bot_token(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path((bot, token_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<BotView>> {
    let uid = uid_of(&headers, &st).await?;
    load_bot(&st, uid, bot).await?;
    let n = sqlx::query("DELETE FROM bot_tokens WHERE id = $1 AND bot_id = $2")
        .bind(token_id)
        .bind(bot)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found("token not found"));
    }
    Ok(Json(load_bot(&st, uid, bot).await?))
}

/// DELETE /bots/{id}：账号禁用、退出所有群、和主人解除好友（私聊变只读）。
/// 发过的消息保留（messages.sender_id 不级联），所以账号行不删。
pub(super) async fn delete_bot(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(bot): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    load_bot(&st, uid, bot).await?;
    let mut tx = st.pool.begin().await?;
    let groups = sqlx::query_scalar::<_, Uuid>(
        "DELETE FROM memberships m USING conversations c
          WHERE m.conversation_id = c.id AND m.user_id = $1 AND c.type = 'group'
          RETURNING c.id",
    )
    .bind(bot)
    .fetch_all(&mut *tx)
    .await?;
    let (lo, hi) = domain::canonical_pair(uid, bot);
    sqlx::query("DELETE FROM friendships WHERE user_a = $1 AND user_b = $2")
        .bind(lo)
        .bind(hi)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(bot)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM bots WHERE user_id = $1")
        .bind(bot)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut affected = vec![uid];
    for g in groups {
        affected.extend(domain::member_ids(&st.pool, g).await?);
    }
    notify(&st, &[uid], "friends");
    notify(&st, &affected, "conversations");
    Ok(StatusCode::NO_CONTENT)
}

/// Bot API 的鉴权层：bot 令牌换成这个 bot 的 access token 再交给处理函数，处理函数照常按用户鉴权
/// （账号禁用、凭据版本都在那里复核）。只挂在 api::router 里 Bot API 那几条路由上：
/// bot 能调什么由那张表决定，那几条路由也只认 bot 令牌，用户的 access token 进不来。
/// 主人被禁用期间令牌停用（403，和被禁用的人一样），启用后恢复：不然被禁用的人还能借 bot 发消息、读群。
pub(super) async fn bot_auth(
    State(st): State<SharedState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> AppResult<axum::response::Response> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::unauthorized("expected bearer token"))?;
    let (bot, version, owner_disabled) = sqlx::query_as::<_, (Uuid, i32, bool)>(
        "SELECT u.id, u.session_version, o.disabled_at IS NOT NULL
           FROM bot_tokens t
           JOIN users u ON u.id = t.bot_id
           JOIN users o ON o.id = u.bot_owner_id
          WHERE t.token = $1",
    )
    .bind(token)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::unauthorized("invalid bot token"))?;
    if owner_disabled {
        return Err(AppError::forbidden("bot 的主人账号已被禁用"));
    }
    let (access, _) = auth::mint_access(bot, version.into(), None, &st.cfg.jwt_secret)?;
    let value =
        header::HeaderValue::from_str(&format!("Bearer {access}")).map_err(AppError::internal)?;
    req.headers_mut().insert(header::AUTHORIZATION, value);
    Ok(next.run(req).await)
}

/// POST /hooks：以 bot 身份发一条消息（文字、附件或带附件的文字），返回这条消息。
pub(super) async fn post_hook(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<HookReq>,
) -> AppResult<Json<MessageView>> {
    let bot = auth::require_user(&st, &headers).await?;
    let conv = match req.conversation_id {
        Some(c) => c,
        None => {
            // bot_auth 只放 bot 进来，bot_owner_id 必有
            let (lo, hi) = domain::canonical_pair(bot.id, bot.bot_owner_id.unwrap_or_default());
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM conversations WHERE dm_key = $1")
                .bind(format!("{lo}:{hi}"))
                .fetch_optional(&st.pool)
                .await?
                .ok_or_else(|| AppError::not_found("conversation not found"))?
        }
    };
    let posted = ws::post_and_broadcast(
        &st,
        domain::PostMessage {
            client_msg_id: req.client_msg_id.as_deref(),
            attachment_id: req.attachment_id,
            reply_to_id: req.reply_to_id,
            ..domain::PostMessage::text(conv, bot.id, &req.content)
        },
    )
    .await
    .map_err(post_err)?;
    // 完整消息视图：以前返回的 id / conversation_id / seq 都在里面，edit_version 改消息时要用
    Ok(Json(posted.message))
}

/// GET /bot/updates：发给 bot 的新消息（私聊里的、群里 @ 它的），事件形状和回调一样。
/// 取到即交付：游标随之前移，同一条不会给第二次（漏处理的用历史接口补）。
/// 没有新消息时长轮询，最多等 timeout 秒。
pub(super) async fn bot_updates(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Query(q): Query<UpdatesQuery>,
) -> AppResult<Json<Value>> {
    let bot = uid_of(&headers, &st).await?;
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(q.timeout.min(UPDATES_MAX_WAIT_SECS));
    let wake = st.bot_wake.entry(bot).or_default().clone();
    loop {
        // 先登记再查：查完到开始等的空档里来的消息也叫得醒
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let events = take_updates(&st, bot, limit).await?;
        if !events.is_empty() || tokio::time::timeout_at(deadline, notified).await.is_err() {
            return Ok(Json(json!({ "events": events })));
        }
    }
}

/// 取出待交付的事件并前移交付游标（memberships.delivered_seq）。同一 bot 的拉取用 bots 行锁串行，
/// 同一条不会交付两次。待交付消息和各会话的 last_seq 在同一条 SELECT 里读（同一个快照）：
/// 某会话待交付的全取走了，游标直接推到 last_seq，群里没 @ 它的消息以后不用再扫。
async fn take_updates(st: &SharedState, bot: Uuid, limit: usize) -> AppResult<Vec<Value>> {
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT 1 FROM bots WHERE user_id = $1 FOR UPDATE")
        .bind(bot)
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query_as::<_, (Uuid, String, i64, Vec<Uuid>, Vec<i64>)>(
        "SELECT m.conversation_id, c.type, c.last_seq,
                COALESCE(array_agg(p.id ORDER BY p.seq) FILTER (WHERE p.id IS NOT NULL), '{}'),
                COALESCE(array_agg(p.seq ORDER BY p.seq) FILTER (WHERE p.id IS NOT NULL), '{}')
           FROM memberships m
           JOIN conversations c ON c.id = m.conversation_id
           LEFT JOIN LATERAL (
               SELECT msg.id, msg.seq FROM messages msg JOIN users s ON s.id = msg.sender_id
                WHERE msg.conversation_id = m.conversation_id AND msg.seq > m.delivered_seq
                  AND msg.deleted_at IS NULL AND s.bot_owner_id IS NULL
                  AND (c.type = 'dm' OR EXISTS(SELECT 1 FROM message_mentions mm
                                                WHERE mm.message_id = msg.id AND mm.user_id = $1))
                ORDER BY msg.seq
                LIMIT $2
           ) p ON true
          WHERE m.user_id = $1 AND m.delivered_seq < c.last_seq
          GROUP BY m.conversation_id, c.type, c.last_seq",
    )
    .bind(bot)
    .bind(limit as i64)
    .fetch_all(&mut *tx)
    .await?;
    let mut ids: Vec<Uuid> = Vec::new();
    let mut kinds = std::collections::HashMap::new();
    let (mut convs, mut cursors): (Vec<Uuid>, Vec<i64>) = (Vec::new(), Vec::new());
    for (conv, kind, last_seq, msg_ids, msg_seqs) in rows {
        let n = msg_ids.len().min(limit - ids.len());
        // 全取走了（也没被 LATERAL 的 LIMIT 截断）→ 推到 last_seq；只取走一部分 → 推到取走的最后一条
        let cursor = if n == msg_ids.len() && n < limit {
            last_seq
        } else if n > 0 {
            msg_seqs[n - 1]
        } else {
            continue;
        };
        ids.extend_from_slice(&msg_ids[..n]);
        kinds.insert(conv, kind);
        convs.push(conv);
        cursors.push(cursor);
    }
    sqlx::query(
        "UPDATE memberships m SET delivered_seq = v.seq
           FROM unnest($2::uuid[], $3::bigint[]) AS v(conv, seq)
          WHERE m.user_id = $1 AND m.conversation_id = v.conv",
    )
    .bind(bot)
    .bind(&convs)
    .bind(&cursors)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, domain::MsgViewRow>(
        "SELECT * FROM message_views WHERE id = ANY($1) ORDER BY created_at, seq",
    )
    .bind(&ids)
    .fetch_all(&st.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let conv = r.conversation_id;
            bot_event(bot, conv, &kinds[&conv], &r.into_view(bot))
        })
        .collect())
}

/// GET /bot/openapi.json：Bot API 的 OpenAPI 描述。不要令牌，agent 平台可以直接按这个地址导入；
/// servers 填本服务对外地址（后台的 public_url，没填就按请求的 Host）。
pub(super) async fn bot_openapi(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let mut spec: Value =
        serde_json::from_str(include_str!("bot_openapi.json")).map_err(AppError::internal)?;
    let public_url = settings::load(&st.pool).await?.public_url;
    let origin = crate::client_update::origin(&public_url, &headers);
    spec["servers"] = json!([{ "url": format!("{origin}/v1") }]);
    Ok(Json(spec))
}

/// 发给 bot 的一条消息：回调的请求体和 GET /bot/updates 里的事件是同一个形状
fn bot_event(bot: Uuid, conv: Uuid, kind: &str, msg: &MessageView) -> Value {
    json!({
        "bot_id": bot,
        "conversation_id": conv,
        "conversation_type": kind,
        "message": msg,
    })
}

/// 新消息落库后调用：叫醒在 GET /bot/updates 上等着的 bot；有回调地址的再异步 POST 过去，不拖慢广播。
/// 私聊里 bot 是对方就算发给它；群里只算被 @ 的 bot。bot 发的消息不算给任何 bot（防互相 @ 死循环）。
/// 主人被禁用、或 bot 自己被管理员禁用时都不回调（令牌那边由 bot_auth / require_user 挡）。
pub(crate) fn dispatch(st: &SharedState, conv: Uuid, msg: &MessageView) {
    let st = st.clone();
    let msg = msg.clone();
    tokio::spawn(async move {
        let targets = match sqlx::query_as::<_, (Uuid, String, String, String)>(
            "SELECT b.user_id, b.webhook_url, b.secret, c.type
               FROM bots b
               JOIN memberships m ON m.user_id = b.user_id AND m.conversation_id = $1
               JOIN conversations c ON c.id = $1
               JOIN users s ON s.id = $2
               JOIN users bu ON bu.id = b.user_id
               JOIN users o ON o.id = b.owner_id
              WHERE b.user_id <> $2 AND s.bot_owner_id IS NULL
                AND bu.disabled_at IS NULL AND o.disabled_at IS NULL
                AND (c.type = 'dm' OR b.user_id = ANY($3))",
        )
        .bind(conv)
        .bind(msg.sender_id)
        .bind(&msg.mentions)
        .fetch_all(&st.pool)
        .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "bot dispatch lookup failed");
                return;
            }
        };
        for (bot, url, secret, kind) in targets {
            if let Some(w) = st.bot_wake.get(&bot) {
                w.notify_waiters();
            }
            if url.is_empty() {
                continue;
            }
            let st = st.clone();
            let msg = msg.clone();
            tokio::spawn(async move {
                match call_webhook(&url, &secret, bot, conv, &kind, &msg).await {
                    Ok(Some(reply)) => {
                        let r = ws::post_and_broadcast(
                            &st,
                            domain::PostMessage::text(conv, bot, &reply),
                        )
                        .await;
                        if let Err(e) = r {
                            tracing::warn!(bot = %bot, error = ?e, "bot reply rejected");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(bot = %bot, url = %url, error = %e, "bot webhook failed")
                    }
                }
            });
        }
    });
}

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(WEBHOOK_TIMEOUT)
            // bot 常在局域网里，不走系统代理
            .no_proxy()
            .build()
            .expect("build reqwest client")
    })
}

/// POST 消息给 bot；返回响应 JSON 里非空的 content
async fn call_webhook(
    url: &str,
    secret: &str,
    bot: Uuid,
    conv: Uuid,
    kind: &str,
    msg: &MessageView,
) -> Result<Option<String>, String> {
    let body = serde_json::to_vec(&bot_event(bot, conv, kind, msg)).map_err(|e| e.to_string())?;
    let sig = hex::encode(hmac_sha256(secret.as_bytes(), &body));
    let mut resp = client()
        .post(url)
        .header("content-type", "application/json")
        .header("x-chat-signature", format!("sha256={sig}"))
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        buf.extend_from_slice(&chunk);
        if buf.len() > REPLY_MAX_BYTES {
            return Err("reply too large".into());
        }
    }
    Ok(serde_json::from_slice::<Value>(&buf)
        .ok()
        .and_then(|v| v["content"].as_str().map(str::to_owned))
        .filter(|s| !s.trim().is_empty()))
}

/// HMAC-SHA256（RFC 2104）。只此一处用，不为它加依赖。
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hmac_matches_rfc4231_case_2() {
        assert_eq!(
            hex::encode(super::hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
