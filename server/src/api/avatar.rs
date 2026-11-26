use super::*;

// ---------- 头像 ----------

pub(crate) struct AvatarImage {
    pub ext: &'static str,
    pub mime: &'static str,
}

/// 只认文件头魔数，不信任客户端 MIME，也不解码图片。
pub(crate) fn sniff_avatar(buf: &[u8]) -> Option<AvatarImage> {
    if buf.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(AvatarImage {
            ext: "png",
            mime: "image/png",
        })
    } else if buf.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(AvatarImage {
            ext: "jpg",
            mime: "image/jpeg",
        })
    } else if buf.len() >= 12 && buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        Some(AvatarImage {
            ext: "webp",
            mime: "image/webp",
        })
    } else {
        None
    }
}

pub(crate) fn avatar_file_path(files_dir: &str, stored: &str) -> std::path::PathBuf {
    std::path::Path::new(files_dir).join("avatars").join(stored)
}

/// 在请求释放恢复屏障前删旧头像。只接受本目录文件名。
async fn cleanup_old_avatar(st: &SharedState, stored: Option<String>) {
    let Some(stored) = stored.filter(|s| {
        !s.is_empty()
            && !s.contains('/')
            && !s.contains('\\')
            && !s.contains("..")
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    }) else {
        return;
    };
    let path = avatar_file_path(&st.cfg.files_dir, &stored);
    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::warn!(path = %path.display(), error = %e, "old avatar cleanup failed");
    }
}

pub(super) async fn load_user_row(st: &SharedState, uid: Uuid) -> AppResult<UserRow> {
    sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = $1")
        .bind(uid)
        .fetch_optional(&st.pool)
        .await?
        .ok_or_else(|| AppError::not_found("user not found"))
}

/// 头像变更只影响目录展示：好友与共群成员刷新会话，本人其他连接也要跟上。
pub(super) async fn notify_avatar_changed(st: &SharedState, uid: Uuid) {
    let mut uids = st.watchers(uid).await.unwrap_or_default();
    uids.push(uid);
    notify(st, &uids, "conversations");
}

/// PUT /users/me/avatar：multipart 字段 `file`。只改自己。
pub(super) async fn put_avatar(
    State(st): State<SharedState>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> AppResult<Json<UserPublic>> {
    let uid = uid_of(&headers, &st).await?;
    let mut buf: Vec<u8> = Vec::new();
    let mut got_file = false;
    let mut too_large = false;
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("bad multipart: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        got_file = true;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| AppError::bad_request(format!("read error: {e}")))?
        {
            if too_large {
                continue;
            }
            buf.extend_from_slice(&chunk);
            if buf.len() > AVATAR_MAX_BYTES {
                too_large = true;
                buf.clear();
            }
        }
    }
    if too_large {
        return Err(AppError::bad_request("avatar too large"));
    }
    if !got_file || buf.is_empty() {
        return Err(AppError::bad_request("missing file"));
    }
    let img = sniff_avatar(&buf).ok_or_else(|| AppError::bad_request("unsupported image"))?;

    let me = load_user_row(&st, uid).await?;
    let version = me.avatar_version + 1;
    // 文件名带随机段：并发上传各写各的文件，乐观锁输掉的一方只删自己的，不会删掉赢家的
    let stored = format!(
        "{uid}-{version}-{}.{}",
        &Uuid::new_v4().simple().to_string()[..8],
        img.ext
    );
    let dir = std::path::Path::new(&st.cfg.files_dir).join("avatars");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::internal(format!("create avatar dir: {e}")))?;
    let path = dir.join(&stored);
    if let Err(e) = tokio::fs::write(&path, &buf).await {
        return Err(AppError::internal(format!("store avatar: {e}")));
    }
    let updated = sqlx::query(
        "UPDATE users SET avatar_path = $2, avatar_version = avatar_version + 1
          WHERE id = $1 AND avatar_version = $3",
    )
    .bind(uid)
    .bind(&stored)
    .bind(me.avatar_version)
    .execute(&st.pool)
    .await?;
    if updated.rows_affected() == 0 {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(AppError::conflict("avatar changed, retry"));
    }
    cleanup_old_avatar(&st, me.avatar_path).await;
    notify_avatar_changed(&st, uid).await;
    let row = load_user_row(&st, uid).await?;
    Ok(Json(self_public(&st, &row).await?))
}

pub(super) async fn delete_avatar(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<UserPublic>> {
    let uid = uid_of(&headers, &st).await?;
    let me = load_user_row(&st, uid).await?;
    if me.avatar_path.is_some() {
        sqlx::query(
            "UPDATE users SET avatar_path = NULL, avatar_version = avatar_version + 1
              WHERE id = $1 AND avatar_path IS NOT NULL",
        )
        .bind(uid)
        .execute(&st.pool)
        .await?;
        cleanup_old_avatar(&st, me.avatar_path).await;
        notify_avatar_changed(&st, uid).await;
    }
    let row = load_user_row(&st, uid).await?;
    Ok(Json(self_public(&st, &row).await?))
}

#[derive(Deserialize)]
pub(super) struct AvatarQuery {
    v: Option<i32>,
}

/// GET /users/{id}/avatar：无需鉴权。v 等于当前版本才允许长期缓存。
pub(super) async fn get_avatar(
    State(st): State<SharedState>,
    Path(id): Path<Uuid>,
    Query(q): Query<AvatarQuery>,
) -> AppResult<impl IntoResponse> {
    let row = sqlx::query_as::<_, (Option<String>, i32)>(
        "SELECT avatar_path, avatar_version FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("user not found"))?;
    let (path, version) = row;
    let stored = path.ok_or_else(|| AppError::not_found("no avatar"))?;
    if stored.contains('/') || stored.contains('\\') || stored.contains("..") {
        return Err(AppError::not_found("no avatar"));
    }
    let data = tokio::fs::read(avatar_file_path(&st.cfg.files_dir, &stored))
        .await
        .map_err(|_| AppError::not_found("no avatar"))?;
    let mime = sniff_avatar(&data)
        .map(|img| img.mime)
        .unwrap_or("application/octet-stream");
    let cache = if q.v == Some(version) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        data,
    ))
}
