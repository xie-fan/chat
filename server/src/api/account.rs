use super::*;

// ---------- auth ----------

pub(super) fn valid_username(s: &str) -> bool {
    (3..=20).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) async fn register(
    State(st): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterReq>,
) -> AppResult<Json<AuthResponse>> {
    rate_limit(&st, format!("reg:{}", client_ip(&st, &headers, addr)))?;
    let username = req.username.trim();
    if !valid_username(username) {
        return Err(AppError::bad_request(
            "username must be 3-20 chars of [a-zA-Z0-9_]",
        ));
    }
    if !(6..=72).contains(&req.password.len()) {
        return Err(AppError::bad_request("password must be 6-72 chars"));
    }
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 50)
        .unwrap_or(username)
        .to_string();

    let hash = auth::hash_password(&req.password).await?;
    let mut tx = st.pool.begin().await?;
    // 注册全局串行：READ COMMITTED 下两个并发注册都会看到空库/同一闸门状态（双管理员）。
    // ponytail: 全局事务锁，注册低频够用。
    sqlx::query("SELECT pg_advisory_xact_lock(7100001)")
        .execute(&mut *tx)
        .await?;

    // 注册闸门：判断与插入在同一事务内（不留竞态窗口）。
    // 空库放行（首个用户仍是管理员引导路径）；非空库按 registration_mode 校验。
    let mut invite_id = None;
    if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await?
    {
        match settings::load(&mut *tx).await?.registration_mode.as_str() {
            settings::MODE_CLOSED => {
                return Err(AppError::forbidden("管理员已关闭注册"));
            }
            settings::MODE_INVITE => {
                let code = req
                    .invite_code
                    .as_deref()
                    .map(settings::normalize_code)
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| AppError::forbidden("邀请码无效或已失效"))?;
                invite_id = Some(settings::consume_invite(&mut *tx, &code).await?);
            }
            // open（含未知值兜底）：忽略 invite_code，不校验不计数
            _ => {}
        }
    }

    let row = sqlx::query_as::<_, UserRow>(
        "INSERT INTO users (username, display_name, password_hash, role, invite_id)
         SELECT $1, $2, $3,
                CASE WHEN NOT EXISTS(SELECT 1 FROM users) THEN 'admin' ELSE 'user' END,
                $4
         RETURNING *",
    )
    .bind(username)
    .bind(&display_name)
    .bind(&hash)
    .bind(invite_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::conflict("username already taken")
        } else {
            e.into()
        }
    })?;

    // 文件传输助手：每用户一个 self 会话，与用户同事务创建（失败整体回滚）
    domain::ensure_self_conversation(&mut *tx, row.id).await?;

    // 注册即一条登录会话（一台设备）
    let sid = auth::create_session(
        &mut *tx,
        row.id,
        &crate::sessions::device_label_of(req.device_label.as_deref()),
        &crate::sessions::user_agent_of(&headers),
    )
    .await?;
    let res = auth::issue_tokens(&mut tx, &st.cfg, &row, sid).await?;
    tx.commit().await?;
    Ok(Json(res))
}

pub(super) async fn login(
    State(st): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> AppResult<Json<AuthResponse>> {
    rate_limit(&st, format!("login:{}", client_ip(&st, &headers, addr)))?;
    // 先不加锁验密码（Argon2 几十毫秒），再锁行；锁住后哈希变了（并发改密/重置）按密码错处理
    let hash =
        sqlx::query_scalar::<_, String>("SELECT password_hash FROM users WHERE username = $1")
            .bind(req.username.trim())
            .fetch_optional(&st.pool)
            .await?
            .ok_or_else(|| AppError::unauthorized("invalid credentials"))?;
    if !auth::verify_password(&hash, &req.password).await {
        return Err(AppError::unauthorized("invalid credentials"));
    }
    let mut tx = st.pool.begin().await?;
    let row = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE username = $1 FOR UPDATE")
        .bind(req.username.trim())
        .fetch_optional(&mut *tx)
        .await?
        .filter(|r| r.password_hash == hash)
        .ok_or_else(|| AppError::unauthorized("invalid credentials"))?;
    if row.disabled_at.is_some() {
        return Err(AppError::forbidden("账号已被禁用"));
    }
    // 登录即一条登录会话（一台设备）
    let sid = auth::create_session(
        &mut *tx,
        row.id,
        &crate::sessions::device_label_of(req.device_label.as_deref()),
        &crate::sessions::user_agent_of(&headers),
    )
    .await?;
    let res = auth::issue_tokens(&mut tx, &st.cfg, &row, sid).await?;
    tx.commit().await?;
    Ok(Json(res))
}

pub(super) async fn refresh(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<RefreshReq>,
) -> AppResult<Json<AuthResponse>> {
    let hash = auth::refresh_hash(&req.refresh_token);
    let mut tx = st.pool.begin().await?;
    // 先定位用户，再按 user → refresh 顺序加锁，与改密/禁用串行化。
    let user_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM refresh_tokens
          WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(user_id) = user_id else {
        // 重放检测：已轮换掉超过宽限期的令牌又被拿来刷新，说明令牌被别人拿到过。
        // 整个登录会话作废（正主和窃取者手里的新令牌一起失效），设备连接踢下线。
        // 宽限期留给「刷新响应丢了、客户端拿旧令牌重试」的正常情况。
        let reused = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT s.user_id, s.id FROM refresh_tokens t
               JOIN sessions s ON s.id = t.session_id
              WHERE t.token_hash = $1 AND t.revoked_at < now() - interval '60 seconds'
                AND s.revoked_at IS NULL",
        )
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((uid, sid)) = reused {
            crate::sessions::revoke_session_row(&mut tx, sid).await?;
            crate::sessions::revoke_session_tokens(&mut tx, sid).await?;
            tx.commit().await?;
            tracing::warn!(user_id = %uid, session_id = %sid, "refresh token 重放，会话已撤销");
            if let Some(epoch) = st.kick_session(uid, sid, "session_revoked") {
                ws::publish_offline(&st, uid, epoch).await;
            }
        }
        // 统一 401：不区分「从未签发」「已撤销/过期」「重放」，避免令牌状态探测。
        // 失败路径不再向 pool 借第二个连接（本函数只持有这一个事务连接），
        // 并发非法 refresh 不会把连接池拖死。
        return Err(AppError::unauthorized("invalid refresh token"));
    };

    let row = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    // 等待用户锁期间可能已被撤销；锁内重检并轮换，失败回滚旧 token。
    let old_sid = sqlx::query_scalar::<_, Option<Uuid>>(
        "UPDATE refresh_tokens SET revoked_at = now()
          WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
          RETURNING session_id",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(old_sid) = old_sid else {
        return Err(AppError::unauthorized("invalid refresh token"));
    };
    if row.disabled_at.is_some() {
        tx.commit().await?;
        return Err(AppError::forbidden("账号已被禁用"));
    }
    let sid = match old_sid {
        Some(sid) => {
            // 沿用同一登录会话；session 已撤销则整次登录作废（顺带烧掉已轮换的旧 token）
            let alive = sqlx::query_scalar::<_, bool>(
                "UPDATE sessions SET last_used_at = now()
                  WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL
                  RETURNING true",
            )
            .bind(sid)
            .bind(row.id)
            .fetch_optional(&mut *tx)
            .await?;
            if alive.is_none() {
                tx.commit().await?;
                return Err(AppError::unauthorized(
                    "session expired, please login again",
                ));
            }
            sid
        }
        // 升级前签发的老 token（session_id 为 NULL）：补建一条 session
        None => {
            auth::create_session(
                &mut *tx,
                row.id,
                "旧设备",
                &crate::sessions::user_agent_of(&headers),
            )
            .await?
        }
    };
    let res = auth::issue_tokens(&mut tx, &st.cfg, &row, sid).await?;
    tx.commit().await?;
    Ok(Json(res))
}

pub(super) async fn logout(
    State(st): State<SharedState>,
    Json(req): Json<LogoutReq>,
) -> AppResult<StatusCode> {
    let hash = auth::refresh_hash(&req.refresh_token);
    let mut tx = st.pool.begin().await?;
    let found = sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "SELECT user_id, session_id FROM refresh_tokens WHERE token_hash = $1",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((uid, sid)) = found {
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(uid)
            .execute(&mut *tx)
            .await?;
        match sid {
            // 登出 = 撤销该 refresh token 所属的整个登录会话（连带其全部 refresh token）
            Some(sid) => {
                sqlx::query(
                    "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
                )
                .bind(sid)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM android_fcm_devices WHERE session_id = $1")
                    .bind(sid)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "UPDATE refresh_tokens SET revoked_at = now()
                     WHERE session_id = $1 AND revoked_at IS NULL",
                )
                .bind(sid)
                .execute(&mut *tx)
                .await?;
            }
            // 无 session 的老 token：只撤销自己
            None => {
                sqlx::query(
                    "UPDATE refresh_tokens SET revoked_at = now()
                     WHERE token_hash = $1 AND revoked_at IS NULL",
                )
                .bind(&hash)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    // 会话已撤销：它名下的 WS 也要断（WS 只在入站帧时校验会话，静默连接否则会一直收推送）
    if let Some((uid, Some(sid))) = found {
        if let Some(epoch) = st.kick_session(uid, sid, "logout") {
            ws::publish_offline(&st, uid, epoch).await;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- ws ticket ----------

pub(super) async fn ws_ticket(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<TicketRes>> {
    let authn = auth::require_auth(&st, &headers).await?;
    let ticket = st.issue_ticket(authn.user.id, authn.user.session_version as i64, authn.sid);
    Ok(Json(TicketRes {
        ticket,
        expires_in: 30,
    }))
}
