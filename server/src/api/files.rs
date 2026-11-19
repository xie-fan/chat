use super::*;

// ---------- 文件上传/下载 ----------

/// 新附件只存 uuid 文件名；历史行是 "{FILES_DIR}/{uuid}"，文件始终平铺在 FILES_DIR 里。
/// 只取最后的文件名：库里的路径不可信（恢复的备份可以篡改），不能让它指到 FILES_DIR 外面去读或删。
pub(crate) fn stored_file_path(files_dir: &str, stored: &str) -> std::path::PathBuf {
    let name = std::path::Path::new(stored).file_name().unwrap_or_default();
    std::path::Path::new(files_dir).join(name)
}

#[cfg(test)]
mod stored_path_tests {
    use super::stored_file_path;
    use std::path::Path;

    #[test]
    fn stays_inside_files_dir() {
        let d = Path::new("/data/files");
        assert_eq!(stored_file_path("/data/files", "abc"), d.join("abc"));
        assert_eq!(
            stored_file_path("/data/files", "abc.thumb.jpg"),
            d.join("abc.thumb.jpg")
        );
        // 历史格式
        assert_eq!(
            stored_file_path("/data/files", "/data/files/abc"),
            d.join("abc")
        );
        assert_eq!(
            stored_file_path("/data/files", "./data/files/abc"),
            d.join("abc")
        );
        // 篡改的备份
        assert_eq!(
            stored_file_path("/data/files", "/proc/self/environ"),
            d.join("environ")
        );
        assert_eq!(stored_file_path("/data/files", ".."), d.to_path_buf());
    }
}

pub(crate) fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

/// 413 配额提示：已用按 0.1 MB 精度展示
pub(crate) fn quota_message(used_bytes: i64, quota_mb: i64) -> String {
    format!(
        "存储空间已满（已用 {:.1} MB / 上限 {} MB）",
        used_bytes as f64 / 1024.0 / 1024.0,
        quota_mb
    )
}

/// 存储配额三件套（V2 P2-1）：quota_mb、折算字节（0=不限）、当前已用。
pub(crate) async fn upload_quota(
    st: &SharedState,
    uploader: Uuid,
) -> AppResult<(i64, Option<i64>, i64)> {
    let quota_mb = settings::load(&st.pool).await?.storage_quota_mb;
    let quota_bytes = (quota_mb > 0).then(|| quota_mb * 1024 * 1024);
    // ponytail: 并发上传可略超配额（无锁读-改）；如需严格上限，按 uploader_id 加锁串行化上传。
    let used_bytes = match quota_bytes {
        Some(_) => domain::storage_used(&st.pool, uploader).await?,
        None => 0,
    };
    Ok((quota_mb, quota_bytes, used_bytes))
}

/// 上传中的临时文件（FILES_DIR/.upload-*.part）。没被 store_upload 收走就在 Drop 时删掉。
pub(crate) struct TempUpload {
    pub(crate) path: std::path::PathBuf,
    pub size: usize,
}

impl Drop for TempUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// multipart 读取结果：file 字段边收边写临时文件（超限/超配额排空打标），
/// text_fields 白名单里的普通文本字段收进 fields。
pub(crate) struct UploadRead {
    pub fields: std::collections::HashMap<String, String>,
    pub file_name: String,
    pub mime: String,
    pub file: Option<TempUpload>,
    pub over_quota: bool,
}

impl UploadRead {
    /// 收到了非空文件
    pub fn has_file(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.size > 0)
    }
}

/// 流式读 multipart：文件本体直接落盘（不进内存），卡 upload_max_bytes 与配额，错误响应仍能送达。
pub(crate) async fn read_upload(
    files_dir: &str,
    mp: &mut Multipart,
    max_bytes: usize,
    quota_bytes: Option<i64>,
    used_bytes: i64,
    text_fields: &[&'static str],
) -> AppResult<UploadRead> {
    use tokio::io::AsyncWriteExt;
    let size_error = || {
        AppError::payload_too_large(format!(
            "文件大小不能超过 {} MB",
            max_bytes / (1024 * 1024)
        ))
    };
    let multipart_error = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            size_error()
        } else {
            AppError::bad_request("无效的上传表单")
        }
    };
    let mut too_large = false;
    let mut rd = UploadRead {
        fields: std::collections::HashMap::new(),
        file_name: "file".to_string(),
        mime: "application/octet-stream".to_string(),
        file: None,
        over_quota: false,
    };
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(multipart_error)?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" && rd.file.is_none() {
            if let Some(n) = field.file_name() {
                rd.file_name = n.to_string();
            }
            if let Some(ct) = field.content_type() {
                rd.mime = ct.to_string();
            }
            // 守卫先建：中途断开 / 读失败 / 写失败都不留临时文件
            let mut tmp = TempUpload {
                path: std::path::Path::new(files_dir)
                    .join(format!(".upload-{}.part", Uuid::new_v4().simple())),
                size: 0,
            };
            let mut f = tokio::fs::File::create(&tmp.path)
                .await
                .map_err(|e| AppError::internal(format!("store file: {e}")))?;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(multipart_error)?
            {
                if too_large {
                    continue; // 排空剩余 body，保证错误响应能送达
                }
                tmp.size += chunk.len();
                if tmp.size > max_bytes {
                    too_large = true;
                } else if rd.over_quota || quota_bytes.is_some_and(|qb| used_bytes + tmp.size as i64 > qb) {
                    rd.over_quota = true;
                } else {
                    f.write_all(&chunk)
                        .await
                        .map_err(|e| AppError::internal(format!("store file: {e}")))?;
                }
            }
            f.flush()
                .await
                .map_err(|e| AppError::internal(format!("store file: {e}")))?;
            if !too_large && !rd.over_quota {
                rd.file = Some(tmp);
            }
        } else if text_fields.contains(&name.as_str()) {
            let v = field
                .text()
                .await
                .map_err(multipart_error)?;
            rd.fields.insert(name, v);
        }
    }
    if too_large {
        return Err(size_error());
    }
    Ok(rd)
}

/// 写盘 + 图片尺寸/缩略图 + attachments 行。
pub(crate) async fn store_upload(
    st: &SharedState,
    uploader: Uuid,
    conv: Uuid,
    file_name: String,
    mime: String,
    file: TempUpload,
    duration_ms: Option<i32>,
) -> AppResult<AttachmentView> {
    let id = Uuid::new_v4();
    // 只存文件名，文件固定在 FILES_DIR 里
    let stored = id.to_string();
    let path = stored_file_path(&st.cfg.files_dir, &stored);
    tokio::fs::rename(&file.path, &path)
        .await
        .map_err(|e| AppError::internal(format!("store file: {e}")))?;
    let size = file.size as i64;

    // 图片解码/缩略图（V2 P2-1）：只有图片才读回内存；CPU 密集放 blocking 池；解码失败只记日志、照常入库
    let meta = if mime.starts_with("image/") {
        let (mime2, path2) = (mime.clone(), path.clone());
        tokio::task::spawn_blocking(move || {
            let buf = std::fs::read(path2).ok()?;
            crate::media::inspect_image(&mime2, &buf)
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    if meta.is_none()
        && matches!(
            mime.as_str(),
            "image/jpeg" | "image/png" | "image/webp" | "image/gif"
        )
    {
        tracing::warn!(mime = %mime, "image decode failed, stored without dims/thumb");
    }
    let (width, height) = meta
        .as_ref()
        .map(|m| (Some(m.width), Some(m.height)))
        .unwrap_or((None, None));
    // 缩略图与原图同目录：存储名加 .thumb.jpg 后缀
    let mut thumb_path = None;
    if let Some(jpg) = meta.as_ref().and_then(|m| m.thumb_jpeg.as_ref()) {
        let tp = format!("{stored}.thumb.jpg");
        match tokio::fs::write(stored_file_path(&st.cfg.files_dir, &tp), jpg).await {
            Ok(()) => thumb_path = Some(tp),
            Err(e) => tracing::warn!(error = %e, "write thumb failed"),
        }
    }
    sqlx::query(
        "INSERT INTO attachments
             (id, uploader_id, conv_id, file_name, mime, size, storage_path, width, height, thumb_path,
              duration_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(uploader)
    .bind(conv)
    .bind(&file_name)
    .bind(&mime)
    .bind(size)
    .bind(&stored)
    .bind(width)
    .bind(height)
    .bind(&thumb_path)
    .bind(duration_ms)
    .execute(&st.pool)
    .await?;
    Ok(AttachmentView {
        id,
        file_name,
        mime,
        size,
        width,
        height,
        has_thumb: thumb_path.is_some(),
        duration_ms,
    })
}

/// multipart: conversation_id 字段 + file 字段。上传者必须是会话成员。
pub(super) async fn upload_file(
    State(st): State<SharedState>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> AppResult<Json<AttachmentView>> {
    let uid = uid_of(&headers, &st).await?;
    let (quota_mb, quota_bytes, used_bytes) = upload_quota(&st, uid).await?;
    let rd = read_upload(
        &st.cfg.files_dir,
        &mut mp,
        st.cfg.upload_max_bytes,
        quota_bytes,
        used_bytes,
        &["conversation_id", "duration_ms"],
    )
    .await?;
    if rd.over_quota {
        return Err(AppError::payload_too_large(quota_message(
            used_bytes, quota_mb,
        )));
    }
    let conv = match rd.fields.get("conversation_id") {
        None => return Err(AppError::bad_request("missing conversation_id")),
        Some(v) => {
            Uuid::parse_str(v.trim()).map_err(|_| AppError::bad_request("bad conversation_id"))?
        }
    };
    if !rd.has_file() {
        return Err(AppError::bad_request("missing file"));
    }
    require_member(&st, conv, uid).await?;
    let duration_ms = match rd.fields.get("duration_ms") {
        Some(raw) => {
            // 语音时长只接受音频附件；普通文件即使漏传字段也不写成语音。
            if !mime_base(&rd.mime).starts_with("audio/") {
                return Err(AppError::bad_request(
                    "duration_ms is only allowed for audio",
                ));
            }
            Some(parse_duration_ms(raw.trim())?)
        }
        None => None,
    };
    Ok(Json(
        store_upload(
            &st,
            uid,
            conv,
            rd.file_name,
            rd.mime,
            rd.file.expect("has_file"),
            duration_ms,
        )
        .await?,
    ))
}

/// 语音时长：仅整数，1–300000 毫秒（5 分钟）。空串、小数、越界都拒绝。
pub(super) fn parse_duration_ms(raw: &str) -> Result<i32, AppError> {
    let n: i32 = raw
        .parse()
        .map_err(|_| AppError::bad_request("bad duration_ms"))?;
    if !(1..=300_000).contains(&n) {
        return Err(AppError::bad_request("duration_ms must be 1-300000"));
    }
    Ok(n)
}

pub(super) fn mime_base(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// 按 UUID 直链下载（能力 URL：id 不可猜测，仅会话内可见）。
#[derive(Deserialize)]
pub(super) struct FileQuery {
    dl: Option<u8>,
    thumb: Option<u8>,
}

/// 允许 inline 预览的 MIME 白名单：仅渲染型媒体。SVG 可携带脚本，必须排除；
/// text/html、xhtml、xml 等一律走 attachment，避免在聊天 origin 上同源执行。
pub(crate) fn inline_safe(mime: &str) -> bool {
    let m = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (m.starts_with("image/") && !m.contains("svg"))
        || m.starts_with("audio/")
        || m.starts_with("video/")
        || m == "application/pdf"
}

/// 公开下载的缓存策略（能力 URL 可长期缓存）。
const PUBLIC_CACHE: &str = "private, max-age=31536000, immutable";
/// 撤回件强制不走缓存扩散（与公开下载刻意相反）。
const NO_STORE: &str = "private, no-store";

/// 按 ?thumb/?dl 选响应体后交给 serve_stored：两条下载路径共用，
/// 区别只在 Cache-Control——公开 immutable、撤回件 no-store。
async fn serve_attachment(
    st: &SharedState,
    q: &FileQuery,
    file_name: &str,
    mime: &str,
    stored: &str,
    thumb_stored: Option<&str>,
    cache_control: &'static str,
    req: axum::extract::Request,
) -> AppResult<axum::response::Response> {
    // ?thumb=1：有缩略图返回 JPEG 缩略图（缓存头与原图一致）；没有/读取失败回退原图。dl=1 忽略 thumb。
    if q.thumb == Some(1) && q.dl != Some(1) {
        if let Some(tp) = thumb_stored {
            let path = stored_file_path(&st.cfg.files_dir, tp);
            if tokio::fs::metadata(&path).await.is_ok() {
                let dispo = format!(
                    "inline; filename*=UTF-8''{}",
                    pct_encode(&format!("{file_name}.thumb.jpg"))
                );
                return serve_stored(path, req, "image/jpeg", &dispo, cache_control).await;
            }
        }
    }
    // dl=1 → attachment 触发下载；inline 只留给安全媒体白名单（防 Stored XSS）
    let mode = if q.dl != Some(1) && inline_safe(mime) {
        "inline"
    } else {
        "attachment"
    };
    let dispo = format!("{}; filename*=UTF-8''{}", mode, pct_encode(file_name));
    serve_stored(
        stored_file_path(&st.cfg.files_dir, stored),
        req,
        mime,
        &dispo,
        cache_control,
    )
    .await
}

/// 公开下载（能力 URL）。撤回中的附件对所有人一律 404：
/// 行还在、物理文件还在（上传者可在 TTL 内走 /files/{id}/recalled），但这条路径彻底封口。
pub(super) async fn download_file(
    State(st): State<SharedState>,
    Path(id): Path<Uuid>,
    Query(q): Query<FileQuery>,
    req: axum::extract::Request,
) -> AppResult<axum::response::Response> {
    let (file_name, mime, stored, thumb_stored) =
        sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT file_name, mime, storage_path, thumb_path FROM attachments
             WHERE id = $1 AND recalled_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or_else(|| AppError::not_found("file not found"))?;
    serve_attachment(
        &st,
        &q,
        &file_name,
        &mime,
        &stored,
        thumb_stored.as_deref(),
        PUBLIC_CACHE,
        req,
    )
    .await
}

/// 撤回附件取回：GET /files/{id}/recalled，仅 Bearer + 上传者本人 + 保留期内。
/// 不满足一律 404（不泄露行是否存在）；无 Bearer → 401。
/// 记录内克隆行（record_message_id 非空）恒 404——它们没有重发语义，
/// 记录撤回后走这条取回路径不该再拿到内容（MF1）。
/// ?thumb=1 / ?dl=1 语义与公开下载一致，但强制 Cache-Control: private, no-store——
/// 撤回件不应再被缓存扩散（与公开下载的 immutable 缓存策略刻意相反）。
pub(super) async fn download_recalled(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<FileQuery>,
    req: axum::extract::Request,
) -> AppResult<axum::response::Response> {
    let uid = uid_of(&headers, &st).await?;
    let ttl = st.cfg.recall_attachment_ttl_secs as f64;
    #[allow(clippy::type_complexity)]
    let row = sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, bool)>(
        "SELECT uploader_id, file_name, mime, storage_path, thumb_path,
                recalled_at IS NOT NULL AND recalled_at + make_interval(secs => $2) > now()
           FROM attachments WHERE id = $1 AND record_message_id IS NULL",
    )
    .bind(id)
    .bind(ttl)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("file not found"))?;
    let (uploader, file_name, mime, stored, thumb_stored, fresh) = row;
    if uploader != uid || !fresh {
        return Err(AppError::not_found("file not found"));
    }
    serve_attachment(
        &st,
        &q,
        &file_name,
        &mime,
        &stored,
        thumb_stored.as_deref(),
        NO_STORE,
        req,
    )
    .await
}

/// 流式发文件（不整个读进内存），支持 Range（音视频拖动、iOS 播放要用）和条件请求。
/// 类型、下载方式、缓存头用库里记的，不用按扩展名猜的；缓存策略由调用方给定
/// （公开下载 immutable；撤回取回 no-store）。
pub(crate) async fn serve_stored(
    path: std::path::PathBuf,
    req: axum::extract::Request,
    mime: &str,
    dispo: &str,
    cache_control: &'static str,
) -> AppResult<axum::response::Response> {
    use tower::ServiceExt;
    let res = tower_http::services::ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(AppError::internal)?;
    if res.status() == StatusCode::NOT_FOUND {
        return Err(AppError::not_found("file not found"));
    }
    let mut res = res.map(axum::body::Body::new);
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(mime)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(dispo) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache_control),
    );
    Ok(res)
}
