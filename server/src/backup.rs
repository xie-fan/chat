//! V2 P4-1 管理后台备份/恢复。
//!
//! 备份 = Postgres 全部业务表（REPEATABLE READ 只读事务里 COPY 出 CSV）
//!      + FILES_DIR 全量文件，打成 tar.gz 存到 FILES_DIR/.backups/；
//!      先写 *.part 再原子改名，列表只出成品。
//! 恢复 = 上传留档 → 只读校验 → 维护模式踢掉在线连接（restoring，发起者会话除外）
//!      → 单事务 TRUNCATE+COPY 回灌 → 记录并换位文件 → 提交回灌和恢复标记。
//! 同一把 backup_busy 锁保证备份生成与恢复互不重叠。

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Read as _, Write as _},
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

// 这些迁移只新增数据可从空表开始的功能。对应旧备份可以不携带这些表。
const ADDITIVE_RESTORE_TABLES: &[(&str, i64)] = &[("android_fcm_devices", 44)];

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
const ACTIVE_RESTORE: &str = ".restore-transaction";
const STAGED_RESTORE: &str = ".restore-tmp-";
const FINISHED_RESTORE: &str = ".restore-cleanup-";

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
        return maintenance_response();
    }
    // 恢复请求本身会获取独占锁，不能持有读锁等待自己的后台任务。
    let _storage = if req.uri().path() == "/admin/restore" {
        None
    } else {
        match storage_operation(&st) {
            Some(guard) => Some(guard),
            None => return maintenance_response(),
        }
    };
    next.run(req).await
}

fn maintenance_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "服务维护中，请稍后" })),
    )
        .into_response()
}

pub(crate) fn storage_operation(st: &SharedState) -> Option<tokio::sync::RwLockReadGuard<'_, ()>> {
    let guard = st.storage_barrier.try_read().ok()?;
    (!st.maintenance.load(Ordering::SeqCst)).then_some(guard)
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
    let manifest = validate_archive(st.clone(), upload_path.clone()).await?;
    check_legacy_restore(&files_dir)?;
    let archived_tables = manifest.tables.into_iter().map(|table| table.name).collect();

    // 第二步：恢复挪进独立任务执行——客户端中断连接（浏览器登出/断网）后恢复必须继续
    // 跑完并收敛文件和数据库状态。锁、维护闸和上传暂存文件均由守卫管理。
    // 踢下线原因 restoring（区别于「维护中」：前端提示恢复完成后重新登录）；
    // 发起者的会话连接保留——它的恢复请求还在等结果，踢掉会先弹出踢下线文案。
    let keep_sid = ctx.sid;
    let st2 = st.clone();
    let upload2 = upload_path.clone();
    let task = tokio::spawn(async move {
        let _busy = busy;
        let _cleanup_upload = cleanup_upload;
        // 从这一步起拒绝新操作，排空已通过维护闸的请求后才换位文件。
        let _maint = MaintGuard::enter(&st2);
        st2.kick_all_except_session(keep_sid, "restoring");
        let _storage = st2.storage_barrier.write().await;
        do_restore(&st2, &upload2, &archived_tables).await
    });
    match task.await {
        Ok(res) => Ok(Json(res?)),
        Err(e) => Err(AppError::internal(format!("restore task failed: {e}"))),
    }
}

/// 仅在没有未完成文件换位时退出维护模式。恢复日志会在重启时继续处理。
struct MaintGuard(SharedState);
impl MaintGuard {
    fn enter(st: &SharedState) -> Self {
        st.maintenance.store(true, Ordering::SeqCst);
        st.restore_epoch.fetch_add(1, Ordering::AcqRel);
        st.maintenance_started.notify_waiters();
        Self(st.clone())
    }
}
impl Drop for MaintGuard {
    fn drop(&mut self) {
        let active = Path::new(&self.0.cfg.files_dir).join(ACTIVE_RESTORE);
        if matches!(active.try_exists(), Ok(false)) {
            self.0.maintenance.store(false, Ordering::SeqCst);
        } else {
            tracing::error!(path = %active.display(), "restore recovery pending; API stays in maintenance");
        }
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
            if !safe_rel_path(Path::new(&s)) {
                return Err(AppError::bad_request("备份包含非法路径"));
            }
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
            } else if let Some(rel) = s.strip_prefix("files/").filter(|rel| !rel.is_empty()) {
                let rel = Path::new(rel);
                if !safe_rel_path(rel) {
                    return Err(AppError::bad_request("备份包含非法路径"));
                }
                if let Some(Component::Normal(name)) = rel.components().next() {
                    if !restore_data_entry(name) {
                        return Err(AppError::internal("备份包含保留的附件目录"));
                    }
                }
            }
        }
        let m = manifest.ok_or_else(|| AppError::bad_request("备份缺少 manifest.json"))?;
        if m.format != 1 {
            return Err(AppError::bad_request("备份格式版本不支持"));
        }
        // 只允许「不比当前新」的备份，已知新增表可以在对应旧备份中缺省。
        // 逐表按 CSV 表头列清单回灌，备份里缺的新列（如 attachments.recalled_at）
        // 落默认值；更新版本生成的备份拒绝，防止降级灌库。
        if m.migration_version > mv {
            return Err(AppError::bad_request(
                "该备份来自更新版本的服务端，请先升级服务端后再恢复",
            ));
        }
        let mset: BTreeSet<String> = m.tables.iter().map(|t| t.name.clone()).collect();
        if !mset.is_subset(&expected)
            || expected.difference(&mset).any(|missing| {
                !ADDITIVE_RESTORE_TABLES.iter().any(|&(table, introduced)| {
                    missing == table && m.migration_version < introduced
                })
            })
        {
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

/// 解包：db/*.csv → staging/db/；files/* → staging/new/。
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
        if !safe_rel_path(Path::new(&s)) {
            return Err("备份包含非法路径".to_string());
        }
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
            if !safe_rel_path(Path::new(rel)) {
                return Err("备份包含非法路径".to_string());
            }
            let dst = new_dir.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut o = std::fs::File::create(&dst).map_err(|e| e.to_string())?;
            std::io::copy(&mut e, &mut o).map_err(|e| e.to_string())?;
            o.sync_all().map_err(|e| e.to_string())?;
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
    archived_tables: &BTreeSet<String>,
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
    // 仍然清空所有当前业务表。备份时代尚未创建的表保持为空。
    for t in order.iter().filter(|table| archived_tables.contains(*table)) {
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
    let f = tokio::fs::File::open(csv)
        .await
        .map_err(AppError::internal)?;
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
            return Err(AppError::bad_request(format!(
                "备份表 {table} 含未知列 {c}"
            )));
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

fn entry_names(dir: &Path) -> io::Result<Vec<OsString>> {
    fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect()
}

fn restore_data_entry(name: &std::ffi::OsStr) -> bool {
    !EXCLUDE_PREFIXES
        .iter()
        .any(|prefix| name.to_string_lossy().starts_with(prefix))
}

#[cfg(unix)]
fn sync_directory(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> io::Result<()> {
    // Windows 的目录句柄不能用 File::sync_all。文件内容仍会同步，目录换位均为同卷改名。
    Ok(())
}

fn sync_directory_tree(root: &Path) -> io::Result<()> {
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            }
        }
        sync_directory(&dir)?;
    }
    Ok(())
}

fn move_restore_entries(
    names: impl IntoIterator<Item = OsString>,
    from: &Path,
    to: &Path,
) -> io::Result<()> {
    for name in names {
        let dst = to.join(&name);
        match fs::symlink_metadata(&dst) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("恢复目标已存在：{}", dst.display()),
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        fs::rename(from.join(name), &dst)?;
    }
    sync_directory(from)?;
    sync_directory(to)
}

fn mark_restore_phase(journal: &Path, phase: &str) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(journal.join(phase))?
        .sync_all()?;
    sync_directory(journal)
}

fn install_restore_files(files_dir: &Path, staging: &Path, id: Uuid) -> io::Result<()> {
    let new = entry_names(&staging.join("new"))?;
    if new.iter().any(|name| !restore_data_entry(name)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "备份包含保留的附件目录",
        ));
    }

    // 日志与两个同卷目录准备完成后，才开始移动任何现有文件。
    fs::create_dir(staging.join("old"))?;
    {
        let mut marker = fs::File::create(staging.join("id"))?;
        marker.write_all(id.to_string().as_bytes())?;
        marker.sync_all()?;
    }
    sync_directory_tree(&staging.join("new"))?;
    sync_directory(&staging.join("old"))?;
    sync_directory(staging)?;
    let journal = files_dir.join(ACTIVE_RESTORE);
    if journal.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "已有未完成的恢复日志",
        ));
    }
    fs::rename(staging, &journal)?;
    sync_directory(files_dir)?;

    let old = entry_names(files_dir)?
        .into_iter()
        .filter(|name| restore_data_entry(name));
    move_restore_entries(old, files_dir, &journal.join("old"))?;
    mark_restore_phase(&journal, "old_saved")?;
    move_restore_entries(new, &journal.join("new"), files_dir)
}

fn complete_restore_files(files_dir: &Path, id: Uuid, committed: bool) -> io::Result<()> {
    let journal = files_dir.join(ACTIVE_RESTORE);
    let old_saved = journal.join("old_saved").try_exists()?;
    if committed {
        // 数据库提交前已经完成全部换位，旧目录只剩清理。
        if !old_saved || !entry_names(&journal.join("new"))?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "已提交的恢复日志缺少文件换位记录",
            ));
        }
    } else {
        // 新文件只在全部旧文件搬完后才开始换入。回退时先清空新文件，再搬回旧文件。
        // 单独记录回退进度，避免第二次中断后把已经搬回的旧文件当成新文件处理。
        if old_saved && !journal.join("new_removed").try_exists()? {
            let installed = entry_names(files_dir)?
                .into_iter()
                .filter(|name| restore_data_entry(name));
            move_restore_entries(installed, files_dir, &journal.join("new"))?;
            mark_restore_phase(&journal, "new_removed")?;
        }
        let old_dir = journal.join("old");
        move_restore_entries(entry_names(&old_dir)?, &old_dir, files_dir)?;
    }

    // 换位或回退完成后，先原子移走日志，再清理已完成的目录。中断清理不影响恢复判断。
    let finished = files_dir.join(format!("{FINISHED_RESTORE}{}", id.simple()));
    fs::rename(journal, &finished)?;
    sync_directory(files_dir)?;
    remove_restore_dir(&finished);
    Ok(())
}

fn remove_restore_dir(path: &Path) {
    if let Err(e) = fs::remove_dir_all(path) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), error = %e, "restore temporary directory cleanup failed");
        }
    }
}

fn check_legacy_restore(files_dir: &Path) -> AppResult<()> {
    let old = files_dir.join(".restore-old");
    let mut entries = match fs::read_dir(&old) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::internal(e)),
    };
    if entries.next().is_some()
        || files_dir
            .join(".restore-new")
            .try_exists()
            .map_err(AppError::internal)?
    {
        return Err(AppError::internal(format!(
            "{} 留有旧版本恢复数据，请核对数据库与附件后再继续",
            old.display()
        )));
    }
    Ok(())
}

/// 等待 PostgreSQL 确定事务结果，再判断附件应该保留新版本还是回退。
async fn reconcile_restore(pool: &sqlx::PgPool, files_dir: &Path) -> AppResult<Option<bool>> {
    let journal = files_dir.join(ACTIVE_RESTORE);
    if !journal.try_exists().map_err(AppError::internal)? {
        return Ok(None);
    }
    let id = tokio::fs::read_to_string(journal.join("id"))
        .await
        .map_err(AppError::internal)?;
    let id = Uuid::parse_str(&id).map_err(AppError::internal)?;
    // 连接中断可能发生在 COMMIT 已送达、尚未收到回执时。行锁等待该事务真正结束。
    let committed_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT operation_id FROM chat_internal.backup_restore WHERE singleton FOR UPDATE",
    )
    .fetch_one(pool)
    .await?;
    let committed = committed_id == Some(id);
    let files_dir = files_dir.to_path_buf();
    tokio::task::spawn_blocking(move || complete_restore_files(&files_dir, id, committed))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;
    tracing::info!(%id, committed, "restore journal resolved");
    Ok(Some(committed))
}

/// 正式启动在接受请求及清扫附件之前调用。发现有未完成的换位时先收敛到数据库的版本。
pub(crate) async fn recover_interrupted_restore(
    pool: &sqlx::PgPool,
    files_dir: &Path,
) -> AppResult<()> {
    check_legacy_restore(files_dir)?;
    reconcile_restore(pool, files_dir).await?;
    let files_dir = files_dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        for entry in fs::read_dir(files_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let id = name
                .strip_prefix(STAGED_RESTORE)
                .or_else(|| name.strip_prefix(FINISHED_RESTORE));
            if id.is_some_and(|id| Uuid::parse_str(id).is_ok()) {
                remove_restore_dir(&entry.path());
            }
        }
        Ok(())
    })
    .await
    .map_err(AppError::internal)?
    .map_err(AppError::internal)
}

async fn do_restore(
    st: &SharedState,
    upload_path: &Path,
    archived_tables: &BTreeSet<String>,
) -> AppResult<Value> {
    let files_dir = PathBuf::from(&st.cfg.files_dir);
    let id = Uuid::new_v4();
    let staging = files_dir.join(format!("{STAGED_RESTORE}{}", id.simple()));
    let upload = upload_path.to_path_buf();
    let db_dir = staging.join("db");
    let new_dir = staging.join("new");

    let attempt = async {
        let files =
            tokio::task::spawn_blocking(move || extract_archive(&upload, &db_dir, &new_dir))
                .await
                .map_err(AppError::internal)?
                .map_err(|e| AppError::bad_request(format!("备份解包失败: {e}")))?;
        let (mut tx, tables) = restore_db(st, &staging.join("db"), archived_tables).await?;
        sqlx::query("SET LOCAL synchronous_commit = on")
            .execute(&mut *tx)
            .await?;
        let marker = sqlx::query(
            "UPDATE chat_internal.backup_restore SET operation_id = $1 WHERE singleton",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if marker.rows_affected() != 1 {
            return Err(AppError::internal("数据库缺少恢复提交标记"));
        }
        let root = files_dir.clone();
        let stage = staging.clone();
        tokio::task::spawn_blocking(move || install_restore_files(&root, &stage, id))
            .await
            .map_err(AppError::internal)?
            .map_err(|e| AppError::internal(format!("文件替换失败: {e}")))?;
        Ok::<_, AppError>((files, tables, tx.commit().await))
    }
    .await;

    let outcome = reconcile_restore(&st.pool, &files_dir).await;
    tokio::task::spawn_blocking(move || remove_restore_dir(&staging))
        .await
        .map_err(AppError::internal)?;
    let committed = outcome? == Some(true);
    let (files, tables, commit) = attempt?;
    if !committed {
        return Err(AppError::internal(format!(
            "数据库恢复未提交，附件已回退: {commit:?}"
        )));
    }
    if let Err(e) = commit {
        tracing::warn!(error = %e, %id, "restore commit verified after connection error");
    }
    Ok(json!({ "tables": tables, "files": files }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RestoreTestDir(PathBuf);

    impl RestoreTestDir {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("chat-restore-test-{}", Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for RestoreTestDir {
        fn drop(&mut self) {
            remove_restore_dir(&self.0);
        }
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn startup_recovers_partial_file_swap(pool: sqlx::PgPool) {
        let dir = RestoreTestDir::new();
        let root = &dir.0;
        let journal = root.join(ACTIVE_RESTORE);
        fs::create_dir_all(journal.join("old")).unwrap();
        fs::create_dir(journal.join("new")).unwrap();
        fs::write(journal.join("id"), Uuid::new_v4().to_string()).unwrap();
        // 第一批旧文件刚移走一条，此时根目录仍有旧内容，新文件尚未安装。
        fs::write(journal.join("old/already-moved"), b"old").unwrap();
        fs::write(root.join("not-yet-moved"), b"still here").unwrap();
        fs::write(journal.join("new/replacement"), b"incoming").unwrap();

        recover_interrupted_restore(&pool, root).await.unwrap();

        assert_eq!(fs::read(root.join("already-moved")).unwrap(), b"old");
        assert_eq!(fs::read(root.join("not-yet-moved")).unwrap(), b"still here");
        assert!(!root.join("replacement").exists());
        assert!(!journal.exists());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn startup_rolls_back_files_after_uncommitted_restore(pool: sqlx::PgPool) {
        let dir = RestoreTestDir::new();
        let root = &dir.0;
        let id = Uuid::new_v4();
        let stage = root.join(format!("{STAGED_RESTORE}{}", id.simple()));
        fs::create_dir_all(stage.join("new")).unwrap();
        fs::write(stage.join("new/shared"), b"new").unwrap();
        fs::write(stage.join("new/added"), b"new only").unwrap();
        fs::write(root.join("shared"), b"old").unwrap();
        fs::write(root.join("removed"), b"old only").unwrap();
        install_restore_files(root, &stage, id).unwrap();

        recover_interrupted_restore(&pool, root).await.unwrap();

        assert_eq!(fs::read(root.join("shared")).unwrap(), b"old");
        assert_eq!(fs::read(root.join("removed")).unwrap(), b"old only");
        assert!(!root.join("added").exists());
        assert!(!root.join(ACTIVE_RESTORE).exists());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn startup_waits_for_commit_and_keeps_committed_files(pool: sqlx::PgPool) {
        let dir = RestoreTestDir::new();
        let root = &dir.0;
        let id = Uuid::new_v4();
        let stage = root.join(format!("{STAGED_RESTORE}{}", id.simple()));
        fs::create_dir_all(stage.join("new")).unwrap();
        fs::write(stage.join("new/shared"), b"new").unwrap();
        fs::write(root.join("shared"), b"old").unwrap();

        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE chat_internal.backup_restore SET operation_id = $1 WHERE singleton")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        install_restore_files(root, &stage, id).unwrap();

        // 连接断开时 PostgreSQL 可能仍在提交。恢复要等待事务决定，而不是读取旧行。
        let recovery = recover_interrupted_restore(&pool, root);
        tokio::pin!(recovery);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut recovery)
                .await
                .is_err()
        );
        tx.commit().await.unwrap();
        recovery.await.unwrap();

        assert_eq!(fs::read(root.join("shared")).unwrap(), b"new");
        assert!(!root.join(ACTIVE_RESTORE).exists());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn startup_resumes_an_interrupted_rollback(pool: sqlx::PgPool) {
        let dir = RestoreTestDir::new();
        let root = &dir.0;
        let journal = root.join(ACTIVE_RESTORE);
        fs::create_dir_all(journal.join("old")).unwrap();
        fs::create_dir(journal.join("new")).unwrap();
        fs::write(journal.join("id"), Uuid::new_v4().to_string()).unwrap();
        mark_restore_phase(&journal, "old_saved").unwrap();
        mark_restore_phase(&journal, "new_removed").unwrap();
        fs::write(journal.join("new/shared"), b"new").unwrap();
        fs::write(journal.join("old/remaining"), b"old only").unwrap();
        fs::write(root.join("shared"), b"old").unwrap();

        recover_interrupted_restore(&pool, root).await.unwrap();

        assert_eq!(fs::read(root.join("shared")).unwrap(), b"old");
        assert_eq!(fs::read(root.join("remaining")).unwrap(), b"old only");
        assert!(!journal.exists());
    }
}
