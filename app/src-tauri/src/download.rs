//! MM4：附件直存系统下载目录（无另存为对话框）。
//! - 桌面（Linux/Windows/macOS）：reqwest 流式 GET → `<下载目录>/.<名>.part` → 落定改名。
//! - Android：URL 校验与文件名净化仍在 Rust 侧完成，随后委托 Kotlin DownloadPlugin
//!   （MediaStore.Downloads，IS_PENDING 行，API 29+；旧版本由 Kotlin 拒绝）。
//!
//! origin 必须在前端传入的服务器列表内，路径为 /v1/files/<uuid>。
//! 短时附件票据通过 URL 提供，原生下载器不跟重定向，也不接收账号凭据。

#[cfg(target_os = "android")]
use tauri::Manager; // manage/try_state

/// 给前端的结果：path = 落盘路径（Android 为 content:// URI），name = 实际文件名
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Downloaded {
    pub path: String,
    pub name: String,
}

/// Android 插件句柄的受管状态。包一层 newtype：裸 PluginHandle<R> 是泛型，
/// 别的插件若也 manage 同型会撞 state（try_state 只能按类型存取）。
#[cfg(target_os = "android")]
struct DownloadHandle<R: tauri::Runtime>(tauri::plugin::PluginHandle<R>);

/// Tauri 插件壳：仅在 Android 上注册 Kotlin DownloadPlugin（com.chat.app.DownloadPlugin），
/// 桌面平台没有原生侧，setup 空转。
pub fn init<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("download")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let handle = api.register_android_plugin("com.chat.app", "DownloadPlugin")?;
                app.manage(DownloadHandle(handle));
            }
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}

#[tauri::command]
pub async fn download_to_downloads(
    app: tauri::AppHandle,
    url: String,
    file_name: String,
    allowed_origins: Vec<String>,
) -> Result<Downloaded, String> {
    let url = validate_download_url(&url, &allowed_origins)?;
    let name = sanitize_file_name(&file_name);
    platform_download(&app, &url, &name).await
}

// ---------- 文件名净化（C3） ----------

const MAX_NAME_BYTES: usize = 200;
const FALLBACK_NAME: &str = "download";
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn forbidden_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        // bidi 控制符：U+200E/U+200F、U+202A–U+202E、U+2066–U+2069
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// 净化文件名：剔除分隔符/控制符/bidi 控制符；剪首点与尾点、尾空格；
/// Windows 保留名（首个 . 之前命中）加 "_" 前缀；总长压到 200 字节内且保扩展名。
pub(crate) fn sanitize_file_name(raw: &str) -> String {
    let filtered: String = raw.chars().filter(|&c| !forbidden_char(c)).collect();
    let trimmed = filtered.trim_start_matches('.').trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        // 空串、"."、".." 以及纯点/空格组合
        return FALLBACK_NAME.into();
    }
    let mut name = trimmed.to_string();
    let stem = name.split('.').next().unwrap_or("");
    if WINDOWS_RESERVED.contains(&stem.to_ascii_uppercase().as_str()) {
        name.insert(0, '_');
    }
    truncate_keep_ext(&name)
}

/// 超字节上限时砍掉 stem 中段（UTF-8 边界），保住扩展名；砍空了就回退默认名
fn truncate_keep_ext(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        // 扩展名自身不能超过预算，否则当作无扩展名整体截断
        Some(i) if i > 0 && name.len() - i < MAX_NAME_BYTES => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let budget = MAX_NAME_BYTES - ext.len();
    let mut end = stem.len().min(budget);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let stem = stem[..end].trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        return FALLBACK_NAME.into();
    }
    format!("{stem}{ext}")
}

// ---------- 下载地址校验（C1） ----------

/// 仅放行服务器附件 URL：http/https、路径恰为 [<服务器路径前缀>/]v1/files/<uuid>、
/// query 只接受 dl=1 和单个附件票据，origin 须在允许列表内。
fn validate_download_url(raw: &str, allowed_origins: &[String]) -> Result<url::Url, String> {
    const BAD: &str = "下载地址无效";
    let mut u = url::Url::parse(raw.trim()).map_err(|_| BAD.to_string())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(BAD.into());
    }
    u.set_fragment(None);
    let mut saw_dl = false;
    let mut saw_ticket = false;
    for (k, v) in u.query_pairs() {
        if k == "dl" && v == "1" && !saw_dl {
            saw_dl = true;
        } else if k == "ticket" && !saw_ticket && valid_ticket(&v) {
            saw_ticket = true;
        } else {
            return Err(BAD.into());
        }
    }
    for allowed in allowed_origins {
        let Ok(a) = url::Url::parse(allowed) else {
            continue;
        };
        if a.origin() != u.origin() {
            continue;
        }
        let base = a.path().trim_end_matches('/');
        let Some(rest) = u.path().strip_prefix(&format!("{base}/v1/files/")) else {
            continue;
        };
        if uuid::Uuid::parse_str(rest).is_err() {
            return Err(BAD.into());
        }
        return Ok(u);
    }
    Err("下载地址不在允许的服务器列表".into())
}

fn valid_ticket(ticket: &str) -> bool {
    ticket.len() <= 512
        && ticket.split('.').count() == 3
        && ticket.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
        })
}

// ---------- 平台实现 ----------

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn platform_download(
    app: &tauri::AppHandle,
    url: &url::Url,
    name: &str,
) -> Result<Downloaded, String> {
    let dir = imp::downloads_dir(app)?;
    imp::download_to_dir(url, &dir, name).await
}

/// Android：校验过的 URL + 净化名交给 Kotlin 插件走 MediaStore
#[cfg(target_os = "android")]
async fn platform_download(
    app: &tauri::AppHandle,
    url: &url::Url,
    name: &str,
) -> Result<Downloaded, String> {
    use tauri::plugin::mobile::PluginInvokeError;
    let handle = app
        .try_state::<DownloadHandle<tauri::Wry>>()
        .ok_or_else(|| "下载组件未就绪".to_string())?;
    handle
        .0
        .run_mobile_plugin_async::<Downloaded>(
            "download",
            serde_json::json!({ "url": url.as_str(), "fileName": name }),
        )
        .await
        .map_err(|e| match e {
            PluginInvokeError::InvokeRejected(r) => {
                r.message.unwrap_or_else(|| "下载失败".to_string())
            }
            other => format!("下载失败：{other}"),
        })
}

#[cfg(target_os = "ios")]
async fn platform_download(
    _app: &tauri::AppHandle,
    _url: &url::Url,
    _name: &str,
) -> Result<Downloaded, String> {
    Err("当前系统不支持直接保存".into())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod imp {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tauri::Manager;

    /// 最后一个 . 且不在首字符才算扩展名（".zshrc" 没有扩展名）
    pub(super) fn split_ext(name: &str) -> (&str, &str) {
        match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        }
    }

    // ---------- 避让重名（C2 后缀规则） ----------

    /// create_new 原子占名：name.ext 已存在则依次试 name (1).ext、name (2).ext …
    pub(super) fn create_unique(
        dir: &Path,
        file_name: &str,
    ) -> std::io::Result<(PathBuf, String, File)> {
        let (stem, ext) = split_ext(file_name);
        for n in 0..10_000u32 {
            let candidate = if n == 0 {
                file_name.to_string()
            } else {
                format!("{stem} ({n}){ext}")
            };
            let path = dir.join(&candidate);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => return Ok((path, candidate, f)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "下载目录同名文件过多",
        ))
    }

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

    /// XDG_DOWNLOAD_DIR 可能指向根本不存在的目录（下载会报 No such file or
    /// directory）——无论系统返回还是 home 回退，拿到路径先补建。
    pub(super) fn ensure_dir(dir: PathBuf) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&dir).map_err(|e| format!("保存失败：{e}"))?;
        Ok(dir)
    }

    /// 下载目录（C7）：系统 Downloads；拿不到退回 ~/Downloads（没有就建）
    pub(super) fn downloads_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
        if let Ok(d) = app.path().download_dir() {
            return ensure_dir(d);
        }
        let home = app
            .path()
            .home_dir()
            .map_err(|e| format!("无法确定下载目录：{e}"))?;
        ensure_dir(home.join("Downloads"))
    }

    fn client() -> Result<reqwest::Client, String> {
        // rustls-no-provider 不带加密提供者；幂等安装（已装则返回 Err，忽略）
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // 不跟重定向（C1）；不设总超时——大文件时长不可预估，读空闲按块判定
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("下载组件初始化失败：{e}"))
    }

    fn map_net_err(e: reqwest::Error) -> String {
        if e.is_timeout() {
            "下载超时".into()
        } else {
            format!("下载失败：{}", e.without_url())
        }
    }

    /// .part 残骸清理：任何失败路径都删掉半成品
    struct PartGuard(PathBuf);
    impl Drop for PartGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// 状态码先行（404/非 2xx），然后逐块流式写 `.<名>.part`；
    /// 落定用 create_new 占最终名 + 拷入（永不覆盖既有文件），成功后删 .part。
    pub(super) async fn download_to_dir(
        url: &url::Url,
        dir: &Path,
        name: &str,
    ) -> Result<Downloaded, String> {
        let mut res = client()?
            .get(url.clone())
            .send()
            .await
            .map_err(map_net_err)?;
        let status = res.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err("文件已撤回或不存在".into());
        }
        if !status.is_success() {
            return Err(format!("下载失败（HTTP {}）", status.as_u16()));
        }
        let (tmp_path, _tmp_name, tmp) =
            create_unique(dir, &format!(".{name}.part")).map_err(|e| format!("保存失败：{e}"))?;
        let guard = PartGuard(tmp_path.clone());
        let mut w = std::io::BufWriter::new(tmp);
        loop {
            let chunk = match tokio::time::timeout(READ_IDLE_TIMEOUT, res.chunk()).await {
                Err(_) => return Err("下载超时".into()),
                Ok(Err(e)) => return Err(map_net_err(e)),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(c))) => c,
            };
            w.write_all(&chunk).map_err(|e| format!("保存失败：{e}"))?;
        }
        w.flush().map_err(|e| format!("保存失败：{e}"))?;
        drop(w);
        let (dst_path, dst_name, mut dst) =
            create_unique(dir, name).map_err(|e| format!("保存失败：{e}"))?;
        {
            let mut src = File::open(&tmp_path).map_err(|e| format!("保存失败：{e}"))?;
            std::io::copy(&mut src, &mut dst).map_err(|e| format!("保存失败：{e}"))?;
            dst.flush().map_err(|e| format!("保存失败：{e}"))?;
        }
        drop(guard);
        Ok(Downloaded {
            path: dst_path.to_string_lossy().into_owned(),
            name: dst_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 净化器（C3） ----------
    #[test]
    fn sanitize_strips_forbidden_chars() {
        assert_eq!(sanitize_file_name("a.txt"), "a.txt");
        assert_eq!(sanitize_file_name("a/b\\c:d*e?f\"g<h>i|j.txt"), "abcdefghij.txt");
        assert_eq!(sanitize_file_name("a\u{0}\nb\u{7f}.txt"), "ab.txt");
        assert_eq!(sanitize_file_name("trick\u{202E}txt.exe"), "tricktxt.exe");
        assert_eq!(sanitize_file_name("a\u{2066}b\u{2069}.png"), "ab.png");
        assert_eq!(sanitize_file_name("a\u{200E}b\u{200F}.png"), "ab.png");
    }

    #[test]
    fn sanitize_trims_and_fallbacks() {
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        assert_eq!(sanitize_file_name("a.txt."), "a.txt");
        assert_eq!(sanitize_file_name("a.txt   "), "a.txt");
        for s in ["", ".", "..", "...", "   "] {
            assert_eq!(sanitize_file_name(s), FALLBACK_NAME, "{s:?}");
        }
    }

    #[test]
    fn sanitize_windows_reserved() {
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("con"), "_con");
        assert_eq!(sanitize_file_name("aux.log"), "_aux.log");
        assert_eq!(sanitize_file_name("LPT9"), "_LPT9");
        assert_eq!(sanitize_file_name("content.txt"), "content.txt");
        assert_eq!(sanitize_file_name("con.me.txt"), "_con.me.txt");
    }

    #[test]
    fn sanitize_byte_limit_keeps_ext() {
        let long = format!("{}.txt", "x".repeat(300));
        let got = sanitize_file_name(&long);
        assert_eq!(got.len(), MAX_NAME_BYTES);
        assert!(got.ends_with(".txt"));
        // 多字节字符在边界截断
        let mb = format!("{}.bin", "汉".repeat(150));
        let got = sanitize_file_name(&mb);
        assert!(got.len() <= MAX_NAME_BYTES && got.ends_with(".bin"));
        // 无扩展名整体截断
        assert_eq!(sanitize_file_name(&"y".repeat(300)).len(), MAX_NAME_BYTES);
    }

    // ---------- URL 白名单校验（C1） ----------
    const UUID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const ORIGIN: &str = "http://127.0.0.1:28080";

    fn ok_url(path_query: &str) -> String {
        format!("{ORIGIN}{path_query}")
    }

    #[test]
    fn validate_url_accepts_attachment_ticket() {
        for q in [
            format!("/v1/files/{UUID}?dl=1&ticket=header.payload.signature"),
            format!("/v1/files/{UUID}?dl=1"),
            format!("/v1/files/{UUID}"),
        ] {
            validate_download_url(&ok_url(&q), &[ORIGIN.to_string()]).unwrap();
        }
        // 服务器带路径前缀也认
        validate_download_url(
            &format!("http://h:9/sub/v1/files/{UUID}?dl=1"),
            &["http://h:9/sub".to_string()],
        )
        .unwrap();
    }

    #[test]
    fn validate_url_rejects_bad_input() {
        let allowed = vec![ORIGIN.to_string()];
        for u in [
            format!("ftp://127.0.0.1:28080/v1/files/{UUID}"),
            ok_url(&format!("/v1/files/{UUID}/x")),
            ok_url(&format!("/files/{UUID}")),
            ok_url("/v1/files/not-a-uuid?dl=1"),
            ok_url(&format!("/v1/files/{UUID}?dl=1&x=1")),
            ok_url(&format!("/v1/files/{UUID}?thumb=1")),
            ok_url(&format!("/v1/files/{UUID}?dl=1&dl=1")),
            ok_url(&format!("/v1/files/{UUID}?ticket=a.b.c&ticket=d.e.f")),
            ok_url(&format!("/v1/files/{UUID}?ticket=a..c")),
            ok_url(&format!("/v1/files/{UUID}?ticket=a.b.c%2F")),
            format!("http://evil.example/v1/files/{UUID}?dl=1"),
            format!("http://u:p@127.0.0.1:28080/v1/files/{UUID}"),
            "not a url".to_string(),
        ] {
            assert!(validate_download_url(&u, &allowed).is_err(), "{u}");
        }
        // 路径前缀不匹配 / 前缀服务器 origin 不同
        assert!(validate_download_url(
            &format!("http://h:9/v1/files/{UUID}"),
            &["http://h:9/sub".to_string()],
        )
        .is_err());
        assert!(validate_download_url(
            &format!("https://h:9/sub/v1/files/{UUID}"),
            &["http://h:9/sub".to_string()],
        )
        .is_err());
    }

    // ---------- 落盘与流式下载（桌面实现；mock HTTP server） ----------
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    mod host {
        use super::super::imp;
        use super::*;
        use std::fs::File;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::path::{Path, PathBuf};

        fn tmp_dir(tag: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "chat-dl-test-{}-{tag}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        // ---------- 后缀避让 ----------
        #[test]
        fn suffix_picker_avoids_existing() {
            let dir = tmp_dir("suffix");
            File::create(dir.join("a.txt")).unwrap();
            File::create(dir.join("a (1).txt")).unwrap();
            File::create(dir.join("b")).unwrap();
            let (_p, name, f) = imp::create_unique(&dir, "a.txt").unwrap();
            drop(f);
            assert_eq!(name, "a (2).txt");
            let (_p, name, f) = imp::create_unique(&dir, "b").unwrap();
            drop(f);
            assert_eq!(name, "b (1)");
            let (_p, name, f) = imp::create_unique(&dir, "c.png").unwrap();
            drop(f);
            assert_eq!(name, "c.png");
            std::fs::remove_dir_all(&dir).ok();
        }

        fn serve_once(handler: impl FnOnce(TcpStream) + Send + 'static) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                if let Ok((mut s, _)) = listener.accept() {
                    // 读掉请求头再回
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf);
                    handler(s);
                }
            });
            format!("http://{addr}")
        }

        fn entries(dir: &Path) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }

        #[tokio::test(flavor = "current_thread")]
        async fn streams_to_downloads_dir() {
            let dir = tmp_dir("ok");
            File::create(dir.join("a.bin")).unwrap(); // 已存在 → 落 (1)
            let origin = serve_once(|mut s| {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
                    .unwrap();
            });
            let url = validate_download_url(
                &format!("{origin}/v1/files/{UUID}?dl=1"),
                &[origin.clone()],
            )
            .unwrap();
            let r = imp::download_to_dir(&url, &dir, "a.bin").await.unwrap();
            assert_eq!(r.name, "a (1).bin");
            assert_eq!(std::fs::read(dir.join("a (1).bin")).unwrap(), b"hello");
            assert_eq!(entries(&dir), vec!["a (1).bin", "a.bin"]);
            std::fs::remove_dir_all(&dir).ok();
        }

        #[tokio::test(flavor = "current_thread")]
        async fn not_found_maps_to_chinese_error() {
            let dir = tmp_dir("404");
            let origin = serve_once(|mut s| {
                s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            });
            let url =
                validate_download_url(&format!("{origin}/v1/files/{UUID}"), &[origin]).unwrap();
            let err = imp::download_to_dir(&url, &dir, "a.bin").await.unwrap_err();
            assert_eq!(err, "文件已撤回或不存在");
            assert!(entries(&dir).is_empty());
            std::fs::remove_dir_all(&dir).ok();
        }

        #[tokio::test(flavor = "current_thread")]
        async fn other_status_maps_to_http_error() {
            let dir = tmp_dir("500");
            let origin = serve_once(|mut s| {
                s.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            });
            let url =
                validate_download_url(&format!("{origin}/v1/files/{UUID}"), &[origin]).unwrap();
            let err = imp::download_to_dir(&url, &dir, "a.bin").await.unwrap_err();
            assert_eq!(err, "下载失败（HTTP 500）");
            assert!(entries(&dir).is_empty());
            std::fs::remove_dir_all(&dir).ok();
        }

        #[tokio::test(flavor = "current_thread")]
        async fn failed_stream_cleans_up_part_file() {
            let dir = tmp_dir("part");
            let origin = serve_once(|mut s| {
                // 宣称 64 字节只发 5 个就断 —— 流中途失败
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\nhello")
                    .unwrap();
            });
            let url =
                validate_download_url(&format!("{origin}/v1/files/{UUID}"), &[origin]).unwrap();
            let err = imp::download_to_dir(&url, &dir, "a.bin").await.unwrap_err();
            assert!(!err.is_empty());
            assert!(entries(&dir).is_empty(), ".part 必须被清理");
            std::fs::remove_dir_all(&dir).ok();
        }

        // ---------- 下载目录补建（fix3） ----------
        #[test]
        fn ensure_dir_creates_missing_dir() {
            // XDG_DOWNLOAD_DIR 返回了不存在的路径：必须就地建好而不是直接失败
            let dir = tmp_dir("ensure").join("XDG-Downloads");
            assert!(!dir.exists());
            let got = imp::ensure_dir(dir.clone()).unwrap();
            assert_eq!(got, dir);
            assert!(dir.is_dir());
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
