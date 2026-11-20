//! 全局可配项（server_settings）与注册邀请码。
//! 配置量极小，每次从库读，不做内存缓存；缺省键按代码默认值处理（不预置行）。

use crate::auth;
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::state::SharedState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 注册模式：行不存在即 open（保证现有部署/测试行为不变）
pub const MODE_OPEN: &str = "open";
pub const MODE_INVITE: &str = "invite";
pub const MODE_CLOSED: &str = "closed";
const VALID_MODES: [&str; 3] = [MODE_OPEN, MODE_INVITE, MODE_CLOSED];

/// 邀请码：10 位无混淆字符（去掉 I/O/0/1）
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 10;

/// 每用户附件存储上限（MB）的合法区间；0 = 不限
pub const QUOTA_MAX_MB: i64 = 1_048_576;

/// 未配置会议地址时的默认服务；空字符串表示关闭会议按钮
pub const DEFAULT_MEETING_BASE_URL: &str = "https://meet.jit.si";
const MEETING_URL_MAX: usize = 200;

#[derive(Debug, Serialize)]
pub struct ServerSettings {
    pub registration_mode: String,
    /// 每用户附件总量上限（MB）；0 = 不限
    pub storage_quota_mb: i64,
    /// ntfy 服务器地址（http/https，V2 P3-3）；空 = 推送功能关闭
    pub push_ntfy_base: String,
    /// ntfy 访问令牌：只写不读——序列化跳过，GET 只暴露 push_ntfy_token_set
    #[serde(skip_serializing)]
    pub push_ntfy_token: String,
    pub push_ntfy_token_set: bool,
    /// 本服务对外地址：推送通知 Click 跳转 + 客户端更新下载地址前缀。
    /// 默认公网实例 https://chat.287152.xyz；显式置空 = 更新下载按请求 Host 拼、推送不带 Click
    pub public_url: String,
    /// 视频会议地址前缀；空字符串 = 关闭会议按钮
    pub meeting_base_url: String,
    /// 客户端安装包所在仓库：owner/repo、https://github.com/owner/repo（GitHub）或其他
    /// http(s) 仓库地址（Gitea）；"off" 关闭；空 = 用 CLIENT_UPDATE_REPO（默认 xie-fan/chat）
    pub update_repo_url: String,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            registration_mode: MODE_OPEN.into(),
            storage_quota_mb: 0,
            push_ntfy_base: String::new(),
            push_ntfy_token: String::new(),
            push_ntfy_token_set: false,
            public_url: "https://chat.287152.xyz".into(),
            meeting_base_url: DEFAULT_MEETING_BASE_URL.into(),
            update_repo_url: String::new(),
        }
    }
}

/// 一次查出全部行，缺省键用默认值；未知键与非法值忽略（前向兼容）
pub async fn load(executor: impl sqlx::PgExecutor<'_>) -> AppResult<ServerSettings> {
    let rows =
        sqlx::query_as::<_, (String, serde_json::Value)>("SELECT key, value FROM server_settings")
            .fetch_all(executor)
            .await?;
    let mut s = ServerSettings::default();
    for (k, v) in rows {
        if k == "registration_mode" {
            if let Some(m) = v.as_str() {
                if VALID_MODES.contains(&m) {
                    s.registration_mode = m.to_string();
                }
            }
        } else if k == "storage_quota_mb" {
            if let Some(q) = v.as_i64() {
                if (0..=QUOTA_MAX_MB).contains(&q) {
                    s.storage_quota_mb = q;
                }
            }
        } else if k == "push_ntfy_base" {
            if let Some(v) = v.as_str() {
                s.push_ntfy_base = v.trim_end_matches('/').to_string();
            }
        } else if k == "push_ntfy_token" {
            if let Some(v) = v.as_str() {
                s.push_ntfy_token_set = !v.is_empty();
                s.push_ntfy_token = v.to_string();
            }
        } else if k == "public_url" {
            if let Some(v) = v.as_str() {
                s.public_url = v.trim_end_matches('/').to_string();
            }
        } else if k == "meeting_base_url" {
            if let Some(url) = v.as_str() {
                if let Ok(url) = normalize_meeting_base_url(url) {
                    s.meeting_base_url = url;
                }
            }
        } else if k == "update_repo_url" {
            if let Some(v) = v.as_str() {
                s.update_repo_url = v.trim_end_matches('/').to_string();
            }
        }
    }
    Ok(s)
}

/// 空字符串表示关闭；否则必须是 http(s)、无空白、去掉路径末尾斜杠后 ≤ 200。
/// 只剥路径末尾的 `/`，保留 scheme 的 `//`（否则 `https://` 会被收成 `https:`）。
pub fn normalize_meeting_base_url(raw: &str) -> Result<String, &'static str> {
    let url = raw.trim();
    if url.is_empty() {
        return Ok(String::new());
    }
    if url.chars().any(char::is_whitespace) {
        return Err("meeting_base_url 不能含空白");
    }
    let scheme_len = if url.starts_with("https://") {
        8
    } else if url.starts_with("http://") {
        7
    } else {
        return Err("meeting_base_url 必须以 http:// 或 https:// 开头");
    };
    let mut end = url.len();
    let bytes = url.as_bytes();
    while end > scheme_len && bytes[end - 1] == b'/' {
        end -= 1;
    }
    let url = &url[..end];
    if url.chars().count() > MEETING_URL_MAX {
        return Err("meeting_base_url 不能超过 200 字符");
    }
    Ok(url.to_string())
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsReq {
    /// 部分更新：缺省字段不改
    pub registration_mode: Option<String>,
    /// 每用户附件存储上限（MB，0 = 不限）；带值时必须为合法整数
    pub storage_quota_mb: Option<serde_json::Value>,
    /// 视频会议地址；空字符串关闭。缺省不改
    pub meeting_base_url: Option<String>,
    /// ntfy 服务器地址（http/https、≤200 字符；V2 P3-3）；空 = 关闭推送
    pub push_ntfy_base: Option<String>,
    /// ntfy 访问令牌（只写不读）；空字符串 = 清除
    pub push_ntfy_token: Option<String>,
    /// 本服务对外地址（同 push_ntfy_base 的校验规则）；缺省不改，
    /// 显式空串 = 更新下载按请求 Host 拼、推送不带 Click
    pub public_url: Option<String>,
    /// 在线更新的安装包仓库（GitHub owner/repo 或 Gitea 地址）；"off" 关闭；
    /// 空 = 用 CLIENT_UPDATE_REPO（默认 xie-fan/chat）。缺省不改
    pub update_repo_url: Option<String>,
}

/// 管理员写入的地址类字段：http/https 且 ≤200 字符；空串合法（= 关闭/清除）。
/// 归一化去首尾空白与末尾斜杠（订阅地址按 "{base}/{topic}" 拼接）。
fn normalize_http_url(v: &str) -> Option<String> {
    let v = v.trim().trim_end_matches('/').to_string();
    (v.is_empty() || (v.len() <= 200 && (v.starts_with("http://") || v.starts_with("https://"))))
        .then_some(v)
}

/// 字符串值键的 UPSERT（设置仅存 jsonb 标量，文本经 to_jsonb 落库）
async fn upsert_text(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key: &str,
    value: &str,
    updated_by: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO server_settings (key, value, updated_by)
         VALUES ($1, to_jsonb($2::text), $3)
         ON CONFLICT (key) DO UPDATE
            SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by",
    )
    .bind(key)
    .bind(value)
    .bind(updated_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 按 patch 逐键 UPSERT（记录 updated_by），返回完整设置
pub async fn update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &UpdateSettingsReq,
    updated_by: Uuid,
) -> AppResult<ServerSettings> {
    if let Some(m) = &req.registration_mode {
        if !VALID_MODES.contains(&m.as_str()) {
            return Err(AppError::bad_request(
                "registration_mode 必须是 open / invite / closed",
            ));
        }
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_by)
             VALUES ('registration_mode', to_jsonb($1::text), $2)
             ON CONFLICT (key) DO UPDATE
                SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by",
        )
        .bind(m)
        .bind(updated_by)
        .execute(&mut **tx)
        .await?;
    }
    if let Some(v) = &req.storage_quota_mb {
        let q = v
            .as_i64()
            .filter(|q| (0..=QUOTA_MAX_MB).contains(q))
            .ok_or_else(|| AppError::bad_request("storage_quota_mb 必须是 0-1048576 的整数"))?;
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_by)
             VALUES ('storage_quota_mb', to_jsonb($1::bigint), $2)
             ON CONFLICT (key) DO UPDATE
                SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by",
        )
        .bind(q)
        .bind(updated_by)
        .execute(&mut **tx)
        .await?;
    }
    if let Some(v) = &req.push_ntfy_base {
        let v = normalize_http_url(v).ok_or_else(|| {
            AppError::bad_request("push_ntfy_base 必须是 http/https 地址（≤200 字符）")
        })?;
        upsert_text(tx, "push_ntfy_base", &v, updated_by).await?;
    }
    if let Some(v) = &req.push_ntfy_token {
        upsert_text(tx, "push_ntfy_token", v, updated_by).await?;
    }
    if let Some(v) = &req.public_url {
        let v = normalize_http_url(v).ok_or_else(|| {
            AppError::bad_request("public_url 必须是 http/https 地址（≤200 字符）")
        })?;
        upsert_text(tx, "public_url", &v, updated_by).await?;
    }
    if let Some(raw) = &req.meeting_base_url {
        let url = normalize_meeting_base_url(raw).map_err(AppError::bad_request)?;
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_by)
             VALUES ('meeting_base_url', to_jsonb($1::text), $2)
             ON CONFLICT (key) DO UPDATE
                SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by",
        )
        .bind(&url)
        .bind(updated_by)
        .execute(&mut **tx)
        .await?;
    }
    if let Some(v) = &req.update_repo_url {
        let v = v.trim().trim_end_matches('/').to_string();
        let ok = v.len() <= 200
            && (v.is_empty() || v == "off" || crate::client_update::parse_repo(&v).is_some());
        if !ok {
            return Err(AppError::bad_request(
                "update_repo_url 必须是 owner/repo、https://github.com/owner/repo、其他 http(s) 仓库地址或 off",
            ));
        }
        upsert_text(tx, "update_repo_url", &v, updated_by).await?;
    }
    load(&mut **tx).await
}

/// 邀请码规范化：比较大小写不敏感、去首尾空白（入库统一大写）
pub fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

fn gen_invite_code() -> String {
    let mut buf = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut buf);
    // 256 % 32 = 0，字节直接索引无模偏置
    buf.iter()
        .map(|b| CODE_ALPHABET[(*b & 31) as usize] as char)
        .collect()
}

/// invite 模式注册：事务内原子消费邀请码，返回 id 供写 users.invite_id；
/// 0 行（不存在/已撤销/过期/用完）→ 403
pub async fn consume_invite(executor: impl sqlx::PgExecutor<'_>, code: &str) -> AppResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "UPDATE invites SET uses = uses + 1
          WHERE code = $1 AND revoked_at IS NULL
            AND (expires_at IS NULL OR expires_at > now())
            AND (max_uses IS NULL OR uses < max_uses)
          RETURNING id",
    )
    .bind(code)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::forbidden("邀请码无效或已失效"))
}

// ---------- 路由处理 ----------

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub registration_mode: String,
    /// ntfy 推送已配置（V2 P3-3）：客户端据此决定是否展示手机推送入口
    pub push_available: bool,
    pub android_fcm_available: bool,
    /// 视频会议地址前缀；空字符串表示客户端不显示会议按钮
    pub meeting_base_url: String,
}

/// GET /server-info：公共端点，供登录页决定注册入口形态
pub async fn server_info(State(st): State<SharedState>) -> AppResult<Json<ServerInfo>> {
    let s = load(&st.pool).await?;
    Ok(Json(ServerInfo {
        registration_mode: s.registration_mode,
        push_available: !s.push_ntfy_base.is_empty(),
        android_fcm_available: crate::fcm::android_push_available(&st).await,
        meeting_base_url: s.meeting_base_url,
    }))
}

pub async fn get_settings(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<ServerSettings>> {
    auth::require_admin(&st, &headers).await?;
    Ok(Json(load(&st.pool).await?))
}

pub async fn put_settings(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<UpdateSettingsReq>,
) -> AppResult<Json<ServerSettings>> {
    let admin = auth::require_admin(&st, &headers).await?;
    let mut tx = st.pool.begin().await?;
    let s = update(&mut tx, &req, admin.id).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// 邀请码列表/详情共用读模型；usable 由库按「未撤销、未过期、未用完」算出
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct InviteView {
    pub id: Uuid,
    pub code: String,
    pub note: String,
    pub created_by_name: Option<String>,
    pub max_uses: Option<i32>,
    pub uses: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub usable: bool,
}

const INVITE_SELECT: &str = "SELECT i.id, i.code, i.note, u.display_name AS created_by_name,
            i.max_uses, i.uses, i.expires_at, i.revoked_at, i.created_at,
            (i.revoked_at IS NULL
             AND (i.expires_at IS NULL OR i.expires_at > now())
             AND (i.max_uses IS NULL OR i.uses < i.max_uses)) AS usable
       FROM invites i LEFT JOIN users u ON u.id = i.created_by";

pub async fn list_invites(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<InviteView>>> {
    auth::require_admin(&st, &headers).await?;
    let rows =
        sqlx::query_as::<_, InviteView>(&format!("{INVITE_SELECT} ORDER BY i.created_at DESC"))
            .fetch_all(&st.pool)
            .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct CreateInviteReq {
    /// 缺省 / null = 不限次数
    pub max_uses: Option<i32>,
    /// 缺省 / null = 永不过期
    pub expires_in_hours: Option<i64>,
    #[serde(default)]
    pub note: String,
}

pub async fn create_invite(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<CreateInviteReq>,
) -> AppResult<Json<InviteView>> {
    let admin = auth::require_admin(&st, &headers).await?;
    if let Some(n) = req.max_uses {
        if !(1..=1000).contains(&n) {
            return Err(AppError::bad_request("max_uses 需在 1-1000 之间"));
        }
    }
    if let Some(h) = req.expires_in_hours {
        if !(1..=8760).contains(&h) {
            return Err(AppError::bad_request("expires_in_hours 需在 1-8760 之间"));
        }
    }
    // 字符级截断，避免按字节切在 UTF-8 边界中间
    let note: String = req.note.trim().chars().take(100).collect();
    let expires_at = req
        .expires_in_hours
        .map(|h| Utc::now() + Duration::hours(h));

    // 码碰撞概率极低，唯一冲突时重试几次即可
    let mut id = None;
    for _ in 0..5 {
        let code = gen_invite_code();
        match sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO invites (code, note, created_by, max_uses, expires_at)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&code)
        .bind(&note)
        .bind(admin.id)
        .bind(req.max_uses)
        .bind(expires_at)
        .fetch_one(&st.pool)
        .await
        {
            Ok(v) => {
                id = Some(v);
                break;
            }
            Err(e) if is_unique_violation(&e) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let id = id.ok_or_else(|| AppError::internal("invite code collision"))?;
    let row = sqlx::query_as::<_, InviteView>(&format!("{INVITE_SELECT} WHERE i.id = $1"))
        .bind(id)
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(row))
}

/// 撤销（置 revoked_at，幂等）：已撤销/不存在的 id 同样返回 204
pub async fn revoke_invite(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    sqlx::query("UPDATE invites SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1")
        .bind(id)
        .execute(&st.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_public_instance() {
        let s = ServerSettings::default();
        assert_eq!(s.public_url, "https://chat.287152.xyz");
        assert_eq!(s.update_repo_url, "");
        assert_eq!(s.registration_mode, MODE_OPEN);
    }
}
