//! V2 P4-1 管理后台备份/恢复。
//!
//! 备份 = Postgres 全部业务表（REPEATABLE READ 只读事务里 COPY 出 CSV）
//!      + FILES_DIR 全量文件，打成 tar.gz 存到 FILES_DIR/.backups/；
//!      先写 *.part 再原子改名，列表只出成品。
//! 恢复 = 上传留档 → 只读校验 → 维护模式踢掉在线连接（restoring，发起者会话除外）
//!      → 单事务 TRUNCATE+COPY 回灌 → 原子换文件目录 → 全量吊销凭据。
//! 同一把 backup_busy 锁保证备份生成与恢复互不重叠。

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    path::{Component, Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Multipart, Path as AxPath, Query, State},
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::auth;
use crate::error::{AppError, AppResult};
use crate::state::SharedState;

/// 业务表清单（public 下全部 BASE TABLE，排除迁移账本；视图天然不含）
const TABLES_SQL: &str = "SELECT table_name FROM information_schema.tables
       WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
         AND table_name <> '_sqlx_migrations'
       ORDER BY table_name";

/// 当前成功迁移版本号（_sqlx_migrations 最大 version）
const MIGRATION_VERSION_SQL: &str =
    "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success";

/// 备份/恢复过程中 FILES_DIR 里需要排除的顶层前缀
/// 快照里数据库引用的文件（相对 FILES_DIR）：附件原文件、缩略图、头像
const REFERENCED_FILES_SQL: &str = "SELECT storage_path FROM attachments
    UNION SELECT thumb_path FROM attachments WHERE thumb_path IS NOT NULL
    UNION SELECT 'avatars/' || avatar_path FROM users WHERE avatar_path IS NOT NULL
    UNION SELECT storage_path FROM acp_attachments";

/// 备份旁的缺失清单：`<备份名>.missing`，每行一个相对路径；完整的备份没有这个文件
fn missing_list_path(bdir: &Path, name: &str) -> PathBuf {
    bdir.join(format!("{name}.missing"))
}

const EXCLUDE_PREFIXES: &[&str] = &[".backups", ".restore-", ".spool-", ".upload-"];

fn qi(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 备份文件名严格校验：chat-backup-<8位日期>T<6位时间>Z.tar.gz，共 35 字符。
/// 任何其他形态一律 400，杜绝路径穿越。
fn valid_backup_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() != 35 || !name.starts_with("chat-backup-") || !name.ends_with(".tar.gz") {
        return false;
    }
    b[12..20].iter().all(|c| c.is_ascii_digit())
        && b[20] == b'T'
        && b[21..27].iter().all(|c| c.is_ascii_digit())
        && b[27] == b'Z'
}

/// tar 条目路径安全检查：必须全部由普通组件构成
/// （隐含排除绝对路径、盘符前缀、.. 上跳、当前目录点）。
fn safe_rel_path(p: &Path) -> bool {
    let mut n = 0;
    for c in p.components() {
        if !matches!(c, Component::Normal(_)) {
            return false;
        }
        n += 1;
    }
    n > 0
}

/// tar 归档字节数统计：append 无法回写实际长度，包一层计数器，
/// 写完与 header 声明长度对账，不一致即整包作废（宁可失败也不产出坏包）。
struct CountingRead<R> {
    inner: R,
    n: u64,
}
impl<R: std::io::Read> std::io::Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.n += n as u64;
        Ok(n)
    }
}

#[derive(Serialize)]
struct ManifestTable {
    name: String,
    rows: i64,
}

/// manifest.json 的读入形态（created_at/files/files_bytes 仅存档，不读）
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Manifest {
    format: i64,
    created_at: String,
    migration_version: i64,
    tables: Vec<ManifestTableIn>,
    files: u64,
    files_bytes: u64,
}
#[derive(Debug, Deserialize)]
struct ManifestTableIn {
    name: String,
    #[allow(dead_code)]
    rows: i64,
}

fn backups_dir(st: &SharedState) -> PathBuf {
    Path::new(&st.cfg.files_dir).join(".backups")
}

/// drop 时释放备份/恢复互斥锁
struct BusyGuard(SharedState);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.backup_busy.store(false, Ordering::SeqCst);
    }
}

fn acquire_busy(st: &SharedState) -> AppResult<BusyGuard> {
    if st
        .backup_busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(AppError::conflict("已有备份或恢复在进行中"));
    }
    Ok(BusyGuard(st.clone()))
}

// ---------- 维护模式中间件（/v1 路由层） ----------

/// 维护模式闸：开启期间除已在执行中的恢复请求外，所有 /v1 请求（含 WS 升级）一律 503。
pub async fn maintenance_gate(
    State(st): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if st.maintenance.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "服务维护中，请稍后" })),
        )
            .into_response();
    }
    next.run(req).await
}

// ---------- REST handlers ----------

/// POST /admin/backups → 202 {name}；生成异步进行
pub async fn create_backup(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    auth::require_admin(&st, &headers).await?;
    let _guard = acquire_busy(&st)?;
    let name = format!("chat-backup-{}.tar.gz", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let st2 = st.clone();
    let n2 = name.clone();
    tokio::spawn(async move {
        let _guard = _guard; // 任务持有锁直到完成
        if let Err(e) = run_backup(&st2, &n2).await {
            tracing::error!(error = %e, name = %n2, "backup failed");
            let _ = tokio::fs::remove_file(backups_dir(&st2).join(format!("{n2}.part"))).await;
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "name": name }))))
}

/// GET /admin/backups → {in_progress, items[]}
pub async fn list_backups(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    auth::require_admin(&st, &headers).await?;
    let dir = backups_dir(&st);
    let mut items: Vec<Value> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            if !valid_backup_name(&name) {
                continue; // .part 与杂项一律不出现在列表
            }
            let size = e.metadata().await.map(|m| m.len()).unwrap_or(0);
            let missing = tokio::fs::read_to_string(missing_list_path(&dir, &name))
                .await
                .map(|t| t.lines().count())
                .unwrap_or(0);
            items.push(json!({
                "name": name,
                "size": size,
                "created_at": backup_ts_to_rfc3339(&name),
                "missing_files": missing,
            }));
        }
    }
    // 文件名内嵌 UTC 时间戳，名字倒序 = 时间倒序
    items.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    Ok(Json(json!({
        "in_progress": st.backup_busy.load(Ordering::SeqCst),
        "items": items,
    })))
}

fn backup_ts_to_rfc3339(name: &str) -> String {
    NaiveDateTime::parse_from_str(&name[12..27], "%Y%m%dT%H%M%SZ")
        .map(|t| {
            DateTime::<Utc>::from_naive_utc_and_offset(t, Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .unwrap_or_default()
}

/// POST /admin/backups/{name}/download-ticket → {url}
/// 浏览器 <a href> 没法带 Authorization，发一次性票据换下载 URL。
pub async fn download_ticket(
    State(st): State<SharedState>,
    headers: HeaderMap,
    AxPath(name): AxPath<String>,
) -> AppResult<impl IntoResponse> {
    let ctx = auth::require_auth(&st, &headers).await?;
    if ctx.user.role != "admin" {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    if !valid_backup_name(&name) {
        return Err(AppError::bad_request("invalid backup name"));
    }
    let path = backups_dir(&st).join(&name);
    if !path.is_file() {
        return Err(AppError::not_found("backup not found"));
    }
    let t = st.issue_ticket(ctx.user.id, ctx.user.session_version as i64, ctx.sid);
    Ok(Json(json!({
        "url": format!("/v1/admin/backups/{name}/download?t={t}"),
    })))
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    t: String,
}

/// GET /admin/backups/{name}/download?t=<ticket> → 流式下载 tar.gz
/// ticket 一次性、60 秒有效；消费时复核用户仍有效且仍是管理员。
pub async fn download_backup(
    State(st): State<SharedState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<DownloadQuery>,
) -> AppResult<Response> {
    if !valid_backup_name(&name) {
        return Err(AppError::bad_request("invalid backup name"));
    }
    let grant = st
        .tickets
        .remove(&q.t)
        .map(|(_, v)| v)
        .ok_or_else(|| AppError::unauthorized("invalid ticket"))?;
    if grant.issued.elapsed() > Duration::from_secs(60) {
        return Err(AppError::unauthorized("ticket expired"));
    }
    let ok = sqlx::query_scalar::<_, bool>(
        "SELECT disabled_at IS NULL AND session_version = $2 AND role = 'admin'
            AND ($3::uuid IS NULL OR EXISTS(
                 SELECT 1 FROM sessions s
                  WHERE s.id = $3 AND s.user_id = users.id AND s.revoked_at IS NULL))
           FROM users WHERE id = $1",
    )
    .bind(grant.uid)
    .bind(grant.session_version)
    .bind(grant.sid)
    .fetch_one(&st.pool)
    .await
    .unwrap_or(false);
    if !ok {
        return Err(AppError::unauthorized(
            "session expired, please login again",
        ));
    }
    let path = backups_dir(&st).join(&name);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::not_found("backup not found"))?;
    // 流式：ReaderStream 分块读盘，不整包进内存
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let dispo = format!("attachment; filename=\"{name}\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (header::CONTENT_DISPOSITION, dispo),
        ],
        body,
    )
        .into_response())
}

/// DELETE /admin/backups/{name}
pub async fn delete_backup(
    State(st): State<SharedState>,
    headers: HeaderMap,
    AxPath(name): AxPath<String>,
) -> AppResult<StatusCode> {
    auth::require_admin(&st, &headers).await?;
    if !valid_backup_name(&name) {
        return Err(AppError::bad_request("invalid backup name"));
    }
    let dir = backups_dir(&st);
    let _ = tokio::fs::remove_file(missing_list_path(&dir, &name)).await;
    match tokio::fs::remove_file(dir.join(&name)).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found("backup not found"))
        }
        Err(e) => Err(AppError::internal(e)),
    }
}

// ---------- 备份生成 ----------

async fn current_migration_version(st: &SharedState) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(MIGRATION_VERSION_SQL)
        .fetch_one(&st.pool)
        .await?)
}

async fn current_tables(st: &SharedState) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(TABLES_SQL)
        .fetch_all(&st.pool)
        .await?)
}

/// 导出阶段：只读 REPEATABLE READ 事务里逐表 COUNT + COPY 出 CSV 到 spool 目录。
/// 每张表一个 .csv 文件，随后由 blocking 打包线程读入 tar。
async fn export_db(
    st: &SharedState,
    spool_dir: &Path,
) -> Result<(Vec<ManifestTable>, i64, Vec<String>), String> {
    let mut conn = st.pool.acquire().await.map_err(|e| e.to_string())?;
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    let r = async {
        let migration_version: i64 = sqlx::query_scalar(MIGRATION_VERSION_SQL)
            .fetch_one(&mut *conn)
            .await
            .map_err(AppError::internal)?;
        let tables: Vec<String> = sqlx::query_scalar(TABLES_SQL)
            .fetch_all(&mut *conn)
            .await
            .map_err(AppError::internal)?;
        let mut out = Vec::with_capacity(tables.len());
        for t in &tables {
            let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", qi(t)))
                .fetch_one(&mut *conn)
                .await
                .map_err(AppError::internal)?;
            let spool = spool_dir.join(format!("{t}.csv"));
            let mut f = tokio::fs::File::create(&spool)
                .await
                .map_err(AppError::internal)?;
            {
                let mut stream = conn
                    .copy_out_raw(&format!(
                        "COPY {} TO STDOUT WITH (FORMAT csv, HEADER true)",
                        qi(t)
                    ))
                    .await
                    .map_err(AppError::internal)?;
                while let Some(chunk) = stream.next().await {
                    let b = chunk.map_err(AppError::internal)?;
                    f.write_all(&b).await.map_err(AppError::internal)?;
                }
            }
            f.flush().await.map_err(AppError::internal)?;
            out.push(ManifestTable {
                name: t.clone(),
                rows,
            });
        }
        let referenced: Vec<String> = sqlx::query_scalar(REFERENCED_FILES_SQL)
            .fetch_all(&mut *conn)
            .await
            .map_err(AppError::internal)?;
        Ok::<_, AppError>((out, migration_version, referenced))
    }
    .await;
    let _ = sqlx::query("COMMIT").execute(&mut *conn).await;
    r.map_err(|e| match e {
        AppError::Internal(m) => m,
        other => format!("{other:?}"),
    })
}

/// 打包阶段（blocking）：manifest + db/*.csv + files/* → *.part。
/// 返回快照引用了、却没能入包的文件（备份期间被删的头像/附件），由调用方记成缺失清单。
fn pack_archive(
    files_dir: &Path,
    spool_dir: &Path,
    part_path: &Path,
    tables: &[ManifestTable],
    migration_version: i64,
    created_at: DateTime<Utc>,
    referenced: &[String],
) -> Result<Vec<String>, String> {
    // 先枚举文件清单（manifest 需要 files/files_bytes）
    let files = enumerate_files(files_dir)?;

    let out = std::fs::File::create(part_path).map_err(|e| e.to_string())?;
    let gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut ar = tar::Builder::new(gz);

    // 表 CSV
    for t in tables {
        let spool = spool_dir.join(format!("{}.csv", t.name));
        let meta = std::fs::metadata(&spool).map_err(|e| e.to_string())?;
        let f = std::fs::File::open(&spool).map_err(|e| e.to_string())?;
        let mut h = tar::Header::new_gnu();
        h.set_size(meta.len());
        h.set_mode(0o644);
        h.set_cksum();
        let mut cr = CountingRead { inner: f, n: 0 };
        ar.append_data(&mut h, format!("db/{}.csv", t.name), &mut cr)
            .map_err(|e| e.to_string())?;
        if cr.n != meta.len() {
            return Err(format!("spool {} changed during backup", t.name));
        }
    }

    // 文件：枚举后消失的跳过（备份期间被删），最后与快照引用对账；
    // 读不了（权限/IO）直接失败，不产出悄悄缺东西的包。
    // ponytail: 导出期间文件仍在被读写，半途变化的文件宁可失败重试，也不产出含坏条目的包。
    let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    let mut packed = std::collections::HashSet::new();
    let mut appended_bytes = 0u64;
    for (rel, _size, abs) in &files {
        let meta = match std::fs::metadata(abs) {
            Ok(m) => m,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(format!("read {rel}: {e}")),
        };
        if !meta.is_file() {
            continue;
        }
        let f = match std::fs::File::open(abs) {
            Ok(f) => f,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(format!("open {rel}: {e}")),
        };
        let mut h = tar::Header::new_gnu();
        h.set_size(meta.len());
        h.set_mode(0o644);
        h.set_cksum();
        let mut cr = CountingRead { inner: f, n: 0 };
        ar.append_data(&mut h, format!("files/{rel}"), &mut cr)
            .map_err(|e| e.to_string())?;
        if cr.n != meta.len() {
            return Err(format!("file {rel} changed during backup"));
        }
        appended_bytes += cr.n;
        packed.insert(rel.as_str());
    }
    let mut missing: Vec<String> = referenced
        .iter()
        .filter(|r| !packed.contains(r.as_str()))
        .cloned()
        .collect();
    missing.sort();

    // manifest 最后写：files/files_bytes 以实际入包为准
    let manifest = json!({
        "format": 1,
        "created_at": created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "migration_version": migration_version,
        "tables": tables,
        "files": packed.len(),
        "files_bytes": appended_bytes,
        "missing_files": missing,
    });
    let mbytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let mut h = tar::Header::new_gnu();
    h.set_size(mbytes.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    ar.append_data(&mut h, "manifest.json", &mbytes[..])
        .map_err(|e| e.to_string())?;

    let gz = ar.into_inner().map_err(|e| e.to_string())?;
    let f = gz.finish().map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(missing)
}

/// 递归枚举 FILES_DIR 下所有普通文件（跳过 .backups/.restore-*/.spool-* 顶层前缀），
/// 返回 (归档内相对路径, 字节数, 绝对路径)。符号链接与目录不进清单。
fn enumerate_files(files_dir: &Path) -> Result<Vec<(String, u64, PathBuf)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![files_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // 读不了的目录不能静默跳过：那样整个目录都不会进备份
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for e in rd {
            let e = e.map_err(|e| format!("read {}: {e}", dir.display()))?;
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if dir == files_dir && EXCLUDE_PREFIXES.iter().any(|x| name.starts_with(x)) {
                continue;
            }
            let ft = e
                .file_type()
                .map_err(|e| format!("stat {}: {e}", p.display()))?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let rel = p
                .strip_prefix(files_dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((rel, size, p));
        }
    }
    Ok(out)
}

async fn run_backup(st: &SharedState, name: &str) -> Result<(), String> {
    let bdir = backups_dir(st);
    tokio::fs::create_dir_all(&bdir)
        .await
        .map_err(|e| e.to_string())?;
    let part = bdir.join(format!("{name}.part"));
    let spool = bdir.join(format!(".spool-{}", Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&spool)
        .await
        .map_err(|e| e.to_string())?;

    let res = async {
        let (tables, mv, referenced) = export_db(st, &spool).await?;
        let files_dir = PathBuf::from(&st.cfg.files_dir);
        let spool2 = spool.clone();
        let part2 = part.clone();
        let created = Utc::now();
        let missing = tokio::task::spawn_blocking(move || {
            pack_archive(
                &files_dir,
                &spool2,
                &part2,
                &tables,
                mv,
                created,
                &referenced,
            )
        })
        .await
        .map_err(|e| e.to_string())??;
        if !missing.is_empty() {
            tracing::warn!(
                name,
                missing = missing.len(),
                "backup is missing referenced files"
            );
            tokio::fs::write(missing_list_path(&bdir, name), missing.join("\n"))
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::rename(&part, bdir.join(name))
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&spool).await;
    res
}

// ---------- 恢复 ----------

/// POST /admin/restore（multipart: file + confirm=RESTORE）
/// 大文件上传按 chunk 流进临时文件，不整包进内存。
pub async fn restore(
    State(st): State<SharedState>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> AppResult<impl IntoResponse> {
    let ctx = auth::require_auth(&st, &headers).await?;
    if ctx.user.role != "admin" {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    let busy = acquire_busy(&st)?; // 恢复与备份共用一把锁，挪进后台任务持有直到恢复真正结束

    // 收 multipart：file → FILES_DIR/.restore-upload-*.part；confirm → 串
    let files_dir = PathBuf::from(&st.cfg.files_dir);
    let upload_path = files_dir.join(format!(".restore-upload-{}.part", Uuid::new_v4().simple()));
    // 路径一定下来就挂上清理：上传中途断开 / 读失败 / 写盘失败都不留 .part。
    // 恢复开跑后守卫随文件一起移进后台任务，请求被取消也不会提前删掉任务要读的归档。
    let cleanup_upload = scopeguard(upload_path.clone());
    let mut got_file = false;
    let mut confirm = String::new();
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("bad multipart: {e}")))?
    {
        match field.name() {
            Some("confirm") => {
                confirm = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(format!("bad confirm: {e}")))?;
            }
            Some("file") if !got_file => {
                let mut f = tokio::fs::File::create(&upload_path)
                    .await
                    .map_err(AppError::internal)?;
                got_file = true;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| AppError::bad_request(format!("read error: {e}")))?
                {
                    f.write_all(&chunk).await.map_err(AppError::internal)?;
                }
                f.flush().await.map_err(AppError::internal)?;
            }
            _ => {}
        }
    }
    if !got_file {
        return Err(AppError::bad_request("missing file"));
    }
    if confirm != "RESTORE" {
        return Err(AppError::bad_request("请键入 RESTORE 确认恢复"));
    }

    // 第一步：只读校验（不解包落盘、不动任何状态）
    let _manifest = validate_archive(st.clone(), upload_path.clone()).await?;

    // 第二步：恢复挪进独立任务执行——客户端中断连接（浏览器登出/断网）后恢复必须继续
    // 跑完并把维护闸放下，否则服务会永久卡在 503。锁/旗标/清理都由任务的 Drop 守卫兜底。
    // 踢下线原因 restoring（区别于「维护中」：前端提示恢复完成后重新登录）；
    // 发起者的会话连接保留——它的恢复请求还在等结果，踢掉会先弹出踢下线文案。
    let keep_sid = ctx.sid;
    let st2 = st.clone();
    let upload2 = upload_path.clone();
    let task = tokio::spawn(async move {
        let _busy = busy;
        let _cleanup_upload = cleanup_upload;
        let _maint = MaintGuard::enter(&st2); // 进维护态；Drop 兜底退出（含 panic/取消路径）
        st2.kick_all_except_session(keep_sid, "restoring");
        do_restore(&st2, &upload2).await
    });
    match task.await {
        Ok(res) => Ok(Json(res?)),
        Err(e) => Err(AppError::internal(format!("restore task failed: {e}"))),
    }
}

/// 维护态进入/退出守卫：Drop 时强制落下闸，panic 与 future 被取消都不会卡死服务。
struct MaintGuard(SharedState);
impl MaintGuard {
    fn enter(st: &SharedState) -> Self {
        st.maintenance.store(true, Ordering::SeqCst);
        Self(st.clone())
    }
}
impl Drop for MaintGuard {
    fn drop(&mut self) {
        self.0.maintenance.store(false, Ordering::SeqCst);
    }
}

/// 出口自动删除的临时文件守卫
fn scopeguard(path: PathBuf) -> impl Drop + Send + 'static {
    struct G(PathBuf);
    impl Drop for G {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    G(path)
}

/// 校验包（全程只读）：能解开、manifest 齐、版本一致、表集合一致、路径全安全。
async fn validate_archive(st: SharedState, upload_path: PathBuf) -> AppResult<Manifest> {
    let mv = current_migration_version(&st).await?;
    let tables = current_tables(&st).await?;
    let expected: BTreeSet<String> = tables.into_iter().collect();
    tokio::task::spawn_blocking(move || -> AppResult<Manifest> {
        let f = std::fs::File::open(&upload_path)
            .map_err(|e| AppError::bad_request(format!("备份文件无法读取: {e}")))?;
        let gz = flate2::read::GzDecoder::new(f);
        let mut ar = tar::Archive::new(gz);
        let mut manifest: Option<Manifest> = None;
        let mut db_csvs: BTreeSet<String> = BTreeSet::new();
        let entries = ar
            .entries()
            .map_err(|e| AppError::bad_request(format!("备份文件无法解包: {e}")))?;
        for e in entries {
            let mut e = e.map_err(|e| AppError::bad_request(format!("备份文件无法解包: {e}")))?;
            let et = e.header().entry_type();
            // 只允许普通文件与目录；符号链接/硬链接/设备节点一律拒绝
            if !matches!(et, tar::EntryType::Regular | tar::EntryType::Directory) {
                return Err(AppError::bad_request("备份包含不安全条目类型"));
            }
            let p = e
                .path()
                .map_err(|_| AppError::bad_request("备份包含非法路径"))?;
            if !safe_rel_path(&p) {
                return Err(AppError::bad_request("备份包含非法路径"));
            }
            let s = p.to_string_lossy().replace('\\', "/");
            if s == "manifest.json" {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)
                    .map_err(|e2| AppError::bad_request(format!("manifest 读取失败: {e2}")))?;
                manifest = Some(
                    serde_json::from_slice(&buf)
                        .map_err(|e2| AppError::bad_request(format!("manifest 解析失败: {e2}")))?,
                );
            } else if let Some(t) = s.strip_prefix("db/") {
                if let Some(tn) = t.strip_suffix(".csv") {
                    db_csvs.insert(tn.to_string());
                }
            }
        }
        let m = manifest.ok_or_else(|| AppError::bad_request("备份缺少 manifest.json"))?;
        if m.format != 1 {
            return Err(AppError::bad_request("备份格式版本不支持"));
        }
        // 只允许「不比当前新」的备份：更老版本的备份可恢复——表集合仍须一致，
        // 逐表按 CSV 表头列清单回灌，备份里缺的新列（如 attachments.recalled_at）
        // 落默认值；更新版本生成的备份拒绝，防止降级灌库。
        if m.migration_version > mv {
            return Err(AppError::bad_request(
                "该备份来自更新版本的服务端，请先升级服务端后再恢复",
            ));
        }
        let mset: BTreeSet<String> = m.tables.iter().map(|t| t.name.clone()).collect();
        if mset != expected {
            return Err(AppError::bad_request("备份表结构与当前服务端不一致"));
        }
        for t in &mset {
            if !db_csvs.contains(t) {
                return Err(AppError::bad_request(format!("备份缺少表数据 db/{t}.csv")));
            }
        }
        Ok(m)
    })
    .await
    .map_err(AppError::internal)?
}

/// 解包：db/*.csv → staging/db/；files/* → FILES_DIR/.restore-new/。
/// 返回解出的文件数。
fn extract_archive(upload: &Path, db_dir: &Path, new_dir: &Path) -> Result<u64, String> {
    std::fs::create_dir_all(db_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(new_dir).map_err(|e| e.to_string())?;
    let f = std::fs::File::open(upload).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(gz);
    let mut n = 0u64;
    let entries = ar.entries().map_err(|e| e.to_string())?;
    for e in entries {
        let mut e = e.map_err(|e| e.to_string())?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let p = e.path().map_err(|e| e.to_string())?;
        if !safe_rel_path(&p) {
            return Err("备份包含非法路径".to_string());
        }
        let s = p.to_string_lossy().replace('\\', "/");
        if let Some(t) = s.strip_prefix("db/") {
            // db 条目只允许单层文件名
            if t.contains('/') || t.contains('\\') || t.is_empty() {
                continue;
            }
            let dst = db_dir.join(t);
            let mut o = std::fs::File::create(&dst).map_err(|e| e.to_string())?;
            std::io::copy(&mut e, &mut o).map_err(|e| e.to_string())?;
        } else if let Some(rel) = s.strip_prefix("files/") {
            if rel.is_empty() {
                continue;
            }
            let dst = new_dir.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut o = std::fs::File::create(&dst).map_err(|e| e.to_string())?;
            std::io::copy(&mut e, &mut o).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    Ok(n)
}

/// 恢复时外键依赖序：referenced 先、dependent 后；自引用忽略。
/// 返回 (COPY 顺序, 需先卸除的循环/自引用约束 (表, 约束名, 定义))。
fn restore_order(
    tables: &[String],
    edges: &[(String, String, String, String)],
) -> (Vec<String>, Vec<(String, String, String)>) {
    // dep[t] = t 依赖的其他表
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for t in tables {
        deps.insert(t.clone(), BTreeSet::new());
    }
    for (dep, refd, _, _) in edges {
        if dep != refd {
            deps.entry(dep.clone()).or_default().insert(refd.clone());
        }
    }
    // Kahn：反复摘出「依赖已全部排完」的表（名字序保证确定性）
    let mut emitted: Vec<String> = Vec::new();
    let mut remaining: BTreeSet<String> = tables.iter().cloned().collect();
    loop {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|t| deps[*t].iter().all(|d| !remaining.contains(d)))
            .cloned()
            .collect();
        if ready.is_empty() {
            break;
        }
        for t in ready {
            emitted.push(t.clone());
            remaining.remove(&t);
        }
    }
    // remaining = 循环残留（如 users ↔ invites）：卸除内部互指外键后任意序
    let mut dropped = Vec::new();
    for (dep, refd, cname, def) in edges {
        let cyclic = remaining.contains(dep) && remaining.contains(refd);
        let self_ref = dep == refd;
        if cyclic || self_ref {
            dropped.push((dep.clone(), cname.clone(), def.clone()));
        }
    }
    let mut order = emitted;
    order.extend(remaining.iter().cloned());
    (order, dropped)
}

/// 数据库回灌：单事务内 卸环约束 → TRUNCATE 全表 CASCADE → 拓扑序 COPY → 回装约束。
/// 任一步失败事务整体回滚。
/// 回灌数据库并作废全部凭据，返回**未提交**的事务：文件替换成功后才提交，失败直接丢弃回滚。
async fn restore_db(
    st: &SharedState,
    db_dir: &Path,
) -> Result<(sqlx::Transaction<'static, sqlx::Postgres>, Vec<String>), AppError> {
    let tables = current_tables(st).await?;
    let edges_raw: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT pgc.conrelid::regclass::text AS dep,
                pgc.confrelid::regclass::text AS refd,
                pgc.conname AS cname,
                pg_get_constraintdef(pgc.oid) AS def
           FROM pg_constraint pgc
           JOIN pg_namespace n ON n.oid = pgc.connamespace
          WHERE pgc.contype = 'f' AND n.nspname = 'public'",
    )
    .fetch_all(&st.pool)
    .await?;
    let edges: Vec<(String, String, String, String)> = edges_raw
        .into_iter()
        .map(|(a, b, c, d)| (unqualify(&a), unqualify(&b), c, d))
        .collect();
    let (order, dropped) = restore_order(&tables, &edges);

    let mut tx = st.pool.begin().await?;
    tracing::info!(
        "restore_db: tx began, dropping {} constraints",
        dropped.len()
    );
    // 卸除循环/自引用约束（恢复后原样装回并触发整表校验）
    for (t, c, _) in &dropped {
        sqlx::query(&format!("ALTER TABLE {} DROP CONSTRAINT {}", qi(t), qi(c)))
            .execute(&mut *tx)
            .await?;
    }
    tracing::info!("restore_db: truncating {} tables", tables.len());
    sqlx::query(&format!(
        "TRUNCATE {} CASCADE",
        tables.iter().map(|t| qi(t)).collect::<Vec<_>>().join(", ")
    ))
    .execute(&mut *tx)
    .await?;
    tracing::info!("restore_db: truncate ok, copying {} tables", order.len());
    for t in &order {
        let csv = db_dir.join(format!("{t}.csv"));
        if !csv.is_file() {
            return Err(AppError::internal(format!("missing csv for table {t}")));
        }
        let cols = copy_column_list(&mut tx, t, &csv).await?;
        let mut ci = tx
            .copy_in_raw(&format!(
                "COPY {} ({cols}) FROM STDIN WITH (FORMAT csv, HEADER true)",
                qi(t)
            ))
            .await?;
        let f = tokio::fs::File::open(&csv)
            .await
            .map_err(AppError::internal)?;
        ci.read_from(f).await?;
        ci.finish().await?;
        tracing::info!(table = %t, "restore_db: copied");
    }
    for (t, c, def) in &dropped {
        sqlx::query(&format!(
            "ALTER TABLE {} ADD CONSTRAINT {} {}",
            qi(t),
            qi(c),
            def
        ))
        .execute(&mut *tx)
        .await?;
    }
    // 备份里的旧令牌与恢复前签发的令牌一律作废（与回灌同一事务）
    sqlx::query("UPDATE users SET session_version = session_version + 1")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE revoked_at IS NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() WHERE revoked_at IS NULL")
        .execute(&mut *tx)
        .await?;
    Ok((tx, order))
}

/// 从 CSV 表头构造 COPY 列清单：按备份自身的列序灌入——老备份缺少的新列
/// （如 0039 的 attachments.recalled_at）由库默认值/NULL 补齐。
/// CSV 里出现当前库没有的列直接拒绝：跨版本恢复不许悄悄丢数据。
async fn copy_column_list(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    csv: &Path,
) -> Result<String, AppError> {
    use tokio::io::AsyncBufReadExt;
    let f = tokio::fs::File::open(csv).await.map_err(AppError::internal)?;
    let header = tokio::io::BufReader::new(f)
        .lines()
        .next_line()
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::bad_request(format!("备份表 {table} 缺少表头")))?;
    let known: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT column_name FROM information_schema.columns
          WHERE table_schema = 'public' AND table_name = $1",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let mut cols = Vec::new();
    for raw in header.trim_end_matches('\r').split(',') {
        let c = raw.trim().trim_matches('"');
        if c.is_empty() || !known.contains(c) {
            return Err(AppError::bad_request(format!("备份表 {table} 含未知列 {c}")));
        }
        cols.push(qi(c));
    }
    if cols.is_empty() {
        return Err(AppError::bad_request(format!("备份表 {table} 缺少表头")));
    }
    Ok(cols.join(", "))
}

/// regclass 文本 → 纯表名（剥掉 schema 与引号；我们的对象名都是简单小写标识符）
fn unqualify(reg: &str) -> String {
    reg.rsplit('.')
        .next()
        .unwrap_or(reg)
        .trim_matches('"')
        .to_string()
}

/// files_dir 顶层里参与替换的条目（.backups/.restore-*/.spool-* 两边都不动）
fn swap_entries(dir: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let name = e.map_err(|e| e.to_string())?.file_name();
        if !EXCLUDE_PREFIXES
            .iter()
            .any(|x| name.to_string_lossy().starts_with(x))
        {
            out.push(name);
        }
    }
    Ok(out)
}

/// 逐个 rename from → to；中途失败把已挪的挪回去再报错（尽力而为，挪回失败只记日志）
fn move_all(names: &[std::ffi::OsString], from: &Path, to: &Path) -> Result<(), String> {
    for (i, n) in names.iter().enumerate() {
        if let Err(e) = std::fs::rename(from.join(n), to.join(n)) {
            for n in &names[..i] {
                if let Err(e2) = std::fs::rename(to.join(n), from.join(n)) {
                    tracing::error!(name = ?n, error = %e2, "restore: move back failed");
                }
            }
            return Err(format!("{}: {e}", n.to_string_lossy()));
        }
    }
    Ok(())
}

/// 文件替换：当前文件 → .restore-old，.restore-new → 原位。失败时撤回到原状再报错。
/// 返回 (旧条目, 新条目)，数据库提交失败时交给 unswap_files 撤回。
type Swapped = (Vec<std::ffi::OsString>, Vec<std::ffi::OsString>);
fn swap_files(files_dir: &Path, new_dir: &Path, old_dir: &Path) -> Result<Swapped, String> {
    std::fs::create_dir_all(old_dir).map_err(|e| e.to_string())?;
    let old = swap_entries(files_dir)?;
    let new: Vec<_> = std::fs::read_dir(new_dir)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.file_name()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    move_all(&old, files_dir, old_dir)?;
    if let Err(e) = move_all(&new, new_dir, files_dir) {
        move_all(&old, old_dir, files_dir)
            .map_err(|e2| format!("{e}; 旧文件挪回失败（保留在 .restore-old）: {e2}"))?;
        return Err(e);
    }
    Ok((old, new))
}

fn unswap_files(
    files_dir: &Path,
    new_dir: &Path,
    old_dir: &Path,
    sw: &Swapped,
) -> Result<(), String> {
    move_all(&sw.1, files_dir, new_dir)?;
    move_all(&sw.0, old_dir, files_dir)
}

async fn do_restore(st: &SharedState, upload_path: &Path) -> AppResult<Value> {
    tracing::info!("restore: begin");
    let files_dir = PathBuf::from(&st.cfg.files_dir);
    let staging = files_dir.join(format!(".restore-tmp-{}", Uuid::new_v4().simple()));
    let new_dir = files_dir.join(".restore-new");
    let old_dir = files_dir.join(".restore-old");
    // 上次恢复撤回失败时原文件留在 .restore-old：不能再往里挪、更不能在成功后连它一起删
    if std::fs::read_dir(&old_dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(AppError::internal(format!(
            "{} 非空（上次恢复遗留的原文件），请人工处理后再恢复",
            old_dir.display()
        )));
    }
    // .restore-new 只会是解包出的备份副本，残留直接清掉
    let _ = tokio::fs::remove_dir_all(&new_dir).await;
    let upload2 = upload_path.to_path_buf();
    let db_dir = staging.join("db");
    let new2 = new_dir.clone();
    let staging2 = staging.clone();
    let files = match tokio::task::spawn_blocking(move || extract_archive(&upload2, &db_dir, &new2))
        .await
        .map_err(AppError::internal)?
    {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&staging2).await;
            let _ = tokio::fs::remove_dir_all(&new_dir).await;
            return Err(AppError::bad_request(format!("备份解包失败: {e}")));
        }
    };

    // 顺序：库回灌（不提交）→ 换文件 → 提交。换文件失败丢弃事务；提交失败把文件换回去。
    let r = async {
        tracing::info!("restore: extracted files={files}, restore_db start");
        let (tx, tables) = restore_db(st, &staging.join("db")).await?;
        let (f2, n2, o2) = (files_dir.clone(), new_dir.clone(), old_dir.clone());
        let swapped = tokio::task::spawn_blocking(move || swap_files(&f2, &n2, &o2))
            .await
            .map_err(AppError::internal)?
            .map_err(|e| AppError::internal(format!("文件替换失败（数据库未改动）: {e}")))?;
        if let Err(e) = tx.commit().await {
            let (f2, n2, o2) = (files_dir.clone(), new_dir.clone(), old_dir.clone());
            let back = tokio::task::spawn_blocking(move || unswap_files(&f2, &n2, &o2, &swapped))
                .await
                .map_err(AppError::internal)?;
            return Err(AppError::internal(match back {
                Ok(()) => format!("数据库提交失败，文件已换回: {e}"),
                Err(b) => {
                    format!("数据库提交失败，文件换回失败（原文件在 .restore-old）: {e}; {b}")
                }
            }));
        }
        tracing::info!("restore: committed");
        Ok::<_, AppError>(json!({
            "tables": tables,
            "files": files,
        }))
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = tokio::fs::remove_dir_all(&new_dir).await;
    if r.is_ok() {
        let _ = tokio::fs::remove_dir_all(&old_dir).await;
    } else if tokio::fs::remove_dir(&old_dir).await.is_err() && old_dir.exists() {
        // 只删空目录：非空说明原文件没能挪回，留给人工
        tracing::error!(dir = %old_dir.display(), "restore failed; original files kept here");
    }
    r
}
