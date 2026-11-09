use super::*;

// ---------- 后台管理（仅 admin，账号级角色，与会话内 owner 无关） ----------

/// 撤销某用户全部有效 refresh token（禁用/改密时用）
pub(super) async fn revoke_all_refresh(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    uid: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now()
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(uid)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 撤销某用户全部登录会话（禁用/管理员重置密码时与 refresh 一并撤销）
pub(super) async fn revoke_all_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    uid: Uuid,
) -> AppResult<()> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(uid)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn admin_overview(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminOverview>> {
    auth::require_admin(&st, &headers).await?;
    let (user_count, admin_count, disabled_count) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT count(*),
                count(*) FILTER (WHERE role = 'admin'),
                count(*) FILTER (WHERE disabled_at IS NOT NULL)
           FROM users",
    )
    .fetch_one(&st.pool)
    .await?;
    let (dm_count, group_count) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*) FILTER (WHERE type = 'dm'),
                count(*) FILTER (WHERE type = 'group')
           FROM conversations",
    )
    .fetch_one(&st.pool)
    .await?;
    let message_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages")
        .fetch_one(&st.pool)
        .await?;
    let (attachment_count, attachment_bytes) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*), COALESCE(sum(size), 0)::bigint FROM attachments",
    )
    .fetch_one(&st.pool)
    .await?;
    let online_users = st.conns.iter().filter(|e| !e.value().is_empty()).count() as i64;
    let online_conns = st.conns.iter().map(|e| e.value().len() as i64).sum();
    Ok(Json(AdminOverview {
        user_count,
        admin_count,
        disabled_count,
        message_count,
        dm_count,
        group_count,
        attachment_count,
        attachment_bytes,
        online_users,
        online_conns,
    }))
}

pub(super) const ADMIN_PAGE: i64 = 50;

pub(super) async fn admin_users(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Query(q): Query<AdminUsersQuery>,
) -> AppResult<Json<Vec<AdminUserView>>> {
    auth::require_admin(&st, &headers).await?;
    let rows = if let Some(needle) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let esc = needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sqlx::query_as::<_, UserRow>(
            "SELECT * FROM users
              WHERE username ILIKE '%' || $1 || '%' ESCAPE '\\'
                 OR display_name ILIKE '%' || $1 || '%' ESCAPE '\\'
              ORDER BY created_at LIMIT $2 OFFSET $3",
        )
        .bind(esc)
        .bind(ADMIN_PAGE)
        .bind(q.offset.max(0))
        .fetch_all(&st.pool)
        .await?
    } else {
        sqlx::query_as::<_, UserRow>("SELECT * FROM users ORDER BY created_at LIMIT $1 OFFSET $2")
            .bind(ADMIN_PAGE)
            .bind(q.offset.max(0))
            .fetch_all(&st.pool)
            .await?
    };
    // 每人附件已用存储（V2 P2-1，MF1 修订）：同一 storage_path 只归最早一行的
    // 上传者计费——转发/记录克隆行 uploader=转发者但更晚，不会被计入其配额。
    // bot 上传的记在主人那一行（和配额口径一致），bot 自己那行是 0。
    let ids: Vec<Uuid> = rows.iter().map(|u| u.id).collect();
    let usage: std::collections::HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT owner_id, SUM(size)::bigint
           FROM (SELECT DISTINCT COALESCE(u.bot_owner_id, u.id) AS owner_id, a.storage_path, a.size
                   FROM attachments a JOIN users u ON u.id = a.uploader_id
                  WHERE COALESCE(u.bot_owner_id, u.id) = ANY($1)
                    AND NOT EXISTS (SELECT 1 FROM attachments o
                                     WHERE o.storage_path = a.storage_path
                                       AND (o.created_at, o.id) < (a.created_at, a.id))) d
          GROUP BY owner_id",
    )
    .bind(&ids)
    .fetch_all(&st.pool)
    .await?
    .into_iter()
    .collect();
    Ok(Json(
        rows.iter()
            .map(|u| AdminUserView {
                id: u.id,
                username: u.username.clone(),
                display_name: u.display_name.clone(),
                role: u.role.clone(),
                disabled_at: u.disabled_at,
                last_seen_at: u.last_seen_at,
                created_at: u.created_at,
                online: st.is_online(u.id),
                storage_bytes: usage.get(&u.id).copied().unwrap_or(0),
                avatar_version: u.avatar_version,
                has_avatar: u.avatar_path.is_some(),
            })
            .collect(),
    ))
}

/// 禁用：顾问锁 + 行锁串行化 → 置标记并 bump 凭据版本 → 撤 refresh → 踢下线（帧 + 主动断连）
pub(super) async fn admin_disable(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(target): Path<Uuid>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMIN_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let (role, already) = sqlx::query_as::<_, (String, bool)>(
        "SELECT role, disabled_at IS NOT NULL FROM users WHERE id = $1 FOR UPDATE",
    )
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("user not found"))?;
    if role == "admin" {
        return Err(AppError::bad_request("管理员账号不可禁用"));
    }
    if !already {
        sqlx::query(
            "UPDATE users SET disabled_at = now(), session_version = session_version + 1
             WHERE id = $1",
        )
        .bind(target)
        .execute(&mut *tx)
        .await?;
    }
    revoke_all_refresh(&mut tx, target).await?;
    revoke_all_sessions(&mut tx, target).await?;
    tx.commit().await?;
    notify(&st, &[target], "me");
    if let Some(epoch) = st.kick_user(target, "disabled") {
        ws::publish_offline(&st, target, epoch).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_enable(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(target): Path<Uuid>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    // 与 disable/set_role 共用顾问锁：disabled 变更串行化，恢复管理员可用性的时点可预期
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMIN_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let res = sqlx::query("UPDATE users SET disabled_at = NULL WHERE id = $1")
        .bind(target)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::not_found("user not found"));
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 任命/撤销管理员。顾问锁串行化并发降权；末位保护按「可用（未禁用）管理员」计数；
/// 被禁用的账号不能被任命为管理员。
pub(super) async fn admin_set_role(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(target): Path<Uuid>,
    Json(req): Json<SetRoleReq>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    if req.role != "admin" && req.role != "user" {
        return Err(AppError::bad_request("role must be admin or user"));
    }
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMIN_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let (cur, disabled) = sqlx::query_as::<_, (String, bool)>(
        "SELECT role, disabled_at IS NOT NULL FROM users WHERE id = $1 FOR UPDATE",
    )
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("user not found"))?;
    if req.role == "admin" {
        if disabled {
            return Err(AppError::bad_request("禁用账号不能设为管理员"));
        }
    } else if cur == "admin" {
        // 撤管理员：要求目标之外仍存在至少一名可用管理员，防止并发双降权把系统锁死
        let usable = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM users
              WHERE role = 'admin' AND disabled_at IS NULL AND id <> $1",
        )
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
        if usable == 0 {
            return Err(AppError::bad_request("至少保留一名可用管理员"));
        }
    }
    sqlx::query("UPDATE users SET role = $2 WHERE id = $1")
        .bind(target)
        .bind(&req.role)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    notify(&st, &[target], "me");
    Ok(StatusCode::NO_CONTENT)
}

/// 重置他人密码：改哈希 + bump 凭据版本（旧 access token/WS ticket 即失效）
/// → 撤 refresh → 踢下线强制重登
pub(super) async fn admin_reset_password(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(target): Path<Uuid>,
    Json(req): Json<ResetPasswordReq>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    if !(6..=72).contains(&req.password.len()) {
        return Err(AppError::bad_request("password must be 6-72 chars"));
    }
    let hash = auth::hash_password(&req.password).await?;
    let mut tx = st.pool.begin().await?;
    let res = sqlx::query(
        "UPDATE users SET password_hash = $2, session_version = session_version + 1
         WHERE id = $1 AND bot_owner_id IS NULL",
    )
    .bind(target)
    .bind(&hash)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::not_found("user not found"));
    }
    revoke_all_refresh(&mut tx, target).await?;
    revoke_all_sessions(&mut tx, target).await?;
    tx.commit().await?;
    if let Some(epoch) = st.kick_user(target, "password_reset") {
        ws::publish_offline(&st, target, epoch).await;
    }
    Ok(StatusCode::NO_CONTENT)
}
