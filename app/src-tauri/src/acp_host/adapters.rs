use super::command;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::{watch, Mutex as AsyncMutex};

const SENTINEL_FILE: &str = ".acp-installed";
const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const PIPE_TAIL_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpmAdapterSpec {
    pub bin: &'static str,
    pub package: &'static str,
}

const ADAPTER_SPECS: &[NpmAdapterSpec] = &[
    NpmAdapterSpec {
        bin: "codex-acp",
        package: "@agentclientprotocol/codex-acp",
    },
    NpmAdapterSpec {
        bin: "claude-agent-acp",
        package: "@agentclientprotocol/claude-agent-acp",
    },
];

/// 按白名单映射需要通过 npm 自动安装的非原生 ACP 适配器；显式路径或未知命令返回 None。
pub fn lookup_npm_adapter(prog: &str) -> Option<NpmAdapterSpec> {
    let trimmed = prog.trim();
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('\\') {
        return None;
    }
    let normalized = normalize_prog_name(trimmed);
    ADAPTER_SPECS
        .iter()
        .copied()
        .find(|spec| spec.bin == normalized.as_str())
}

fn normalize_prog_name(prog: &str) -> String {
    if cfg!(windows) {
        let low = prog.to_ascii_lowercase();
        for ext in [".cmd", ".exe", ".bat"] {
            if let Some(stem) = low.strip_suffix(ext) {
                return stem.to_string();
            }
        }
        low
    } else {
        prog.to_string()
    }
}

pub fn managed_adapter_prefix(data_dir: &Path, bin: &str) -> PathBuf {
    data_dir.join("adapters").join(bin)
}

pub fn resolve_managed_in(data_dir: &Path, prog: &str, pathext: &str) -> Option<PathBuf> {
    let spec = lookup_npm_adapter(prog)?;
    let prefix = managed_adapter_prefix(data_dir, spec.bin);
    if !prefix.join(SENTINEL_FILE).is_file() {
        return None;
    }
    let bin_dir = prefix.join("node_modules").join(".bin");
    command::resolve_in(spec.bin, &bin_dir.to_string_lossy(), pathext)
}

pub fn resolve_agent_program(prog: &str) -> Option<PathBuf> {
    let path_env = command::effective_path_env();
    let pathext = command::default_pathext_env();
    resolve_agent_program_in(&super::host_data_dir(), prog, &path_env, &pathext)
}

pub fn resolve_agent_program_in(
    data_dir: &Path,
    prog: &str,
    path_env: &str,
    pathext: &str,
) -> Option<PathBuf> {
    command::resolve_in(prog, path_env, pathext)
        .or_else(|| resolve_managed_in(data_dir, prog, pathext))
}

fn adapter_lock_for(bin: &'static str) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<StdMutex<HashMap<&'static str, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    guard
        .entry(bin)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

async fn wait_cancelled(cancel: Option<&mut watch::Receiver<bool>>) {
    let Some(rx) = cancel else {
        std::future::pending::<()>().await;
        return;
    };
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn is_cancelled(cancel: Option<&watch::Receiver<bool>>) -> bool {
    cancel.is_some_and(|rx| *rx.borrow())
}

pub async fn ensure_npm_adapter(
    spec: NpmAdapterSpec,
    data_dir: &Path,
    cancel: Option<&mut watch::Receiver<bool>>,
) -> Result<PathBuf, String> {
    let path_env = command::effective_path_env();
    let pathext = command::default_pathext_env();
    ensure_npm_adapter_with(
        spec,
        data_dir,
        &path_env,
        &pathext,
        DEFAULT_INSTALL_TIMEOUT,
        cancel,
    )
    .await
}

pub async fn ensure_npm_adapter_with(
    spec: NpmAdapterSpec,
    data_dir: &Path,
    path_env: &str,
    pathext: &str,
    timeout: Duration,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> Result<PathBuf, String> {
    if is_cancelled(cancel.as_deref()) {
        return Err("已取消安装 ACP 适配器".into());
    }

    let lock = adapter_lock_for(spec.bin);
    let _guard = tokio::select! {
        biased;
        _ = wait_cancelled(cancel.as_deref_mut()) => {
            return Err("已取消安装 ACP 适配器".into());
        }
        g = lock.lock() => g,
    };

    if let Some(hit) = resolve_agent_program_in(data_dir, spec.bin, path_env, pathext) {
        return Ok(hit);
    }

    let Some(npm_path) = command::resolve_in("npm", path_env, pathext) else {
        return Err(format!(
            "未检测到 npm（Node.js），无法自动安装 {}。请先安装 Node.js 或手动安装 {}。",
            spec.package, spec.bin
        ));
    };

    let prefix = managed_adapter_prefix(data_dir, spec.bin);
    std::fs::create_dir_all(&prefix).map_err(|e| format!("创建适配器目录失败：{e}"))?;
    let sentinel = prefix.join(SENTINEL_FILE);
    let _ = std::fs::remove_file(&sentinel);

    let args = vec![
        "install".to_string(),
        "--prefix".to_string(),
        prefix.display().to_string(),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
        "--loglevel=error".to_string(),
        spec.package.to_string(),
    ];
    let mut cmd = command::prepare_resolved(&npm_path, &args, path_env);
    cmd.current_dir(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command::configure_child_group(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 npm 安装 {} 失败：{e}", spec.package))?;

    let stdout_task = tokio::spawn(drain_pipe_tail(child.stdout.take()));
    let stderr_task = tokio::spawn(drain_pipe_tail(child.stderr.take()));

    let status = tokio::select! {
        biased;
        _ = wait_cancelled(cancel) => {
            command::kill_child_tree(&mut child).await;
            let _ = std::fs::remove_dir_all(&prefix);
            return Err("已取消安装 ACP 适配器".into());
        }
        _ = tokio::time::sleep(timeout) => {
            command::kill_child_tree(&mut child).await;
            let _ = std::fs::remove_dir_all(&prefix);
            return Err(format!("通过 npm 自动安装 {} 超时", spec.package));
        }
        res = child.wait() => {
            res.map_err(|e| format!("等待 npm 安装 {} 失败：{e}", spec.package))?
        }
    };

    let out_bytes = stdout_task.await.unwrap_or_default();
    let err_bytes = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let _ = std::fs::remove_dir_all(&prefix);
        let err_str = String::from_utf8_lossy(&err_bytes).trim().to_string();
        let raw_msg = if err_str.is_empty() {
            String::from_utf8_lossy(&out_bytes).trim().to_string()
        } else {
            err_str
        };
        let tail = last_n_chars(&raw_msg, 400);
        let msg = if tail.is_empty() {
            format!("通过 npm 自动安装 {} 失败（退出码 {:?}）", spec.package, status.code())
        } else {
            format!("通过 npm 自动安装 {} 失败：{}", spec.package, tail)
        };
        return Err(msg);
    }

    let bin_dir = prefix.join("node_modules").join(".bin");
    let Some(bin_path) = command::resolve_in(spec.bin, &bin_dir.to_string_lossy(), pathext) else {
        let _ = std::fs::remove_dir_all(&prefix);
        return Err(format!(
            "npm 安装 {} 完成，但未找到可执行文件 {}",
            spec.package, spec.bin
        ));
    };

    let marker = json!({
        "package": spec.package,
        "bin": spec.bin,
    })
    .to_string();
    std::fs::write(&sentinel, marker).map_err(|e| format!("写入适配器安装标记失败：{e}"))?;
    Ok(bin_path)
}

async fn drain_pipe_tail<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let Some(mut reader) = pipe else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > PIPE_TAIL_BYTES {
                    let drop_n = buf.len() - PIPE_TAIL_BYTES;
                    buf.drain(0..drop_n);
                }
            }
        }
    }
    buf
}

fn last_n_chars(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "acp_adapters_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_mock_bin(dir: &Path, name: &str, body_win: &str, body_unix: &str) -> PathBuf {
        #[cfg(windows)]
        {
            let _ = body_unix;
            let p = dir.join(format!("{name}.cmd"));
            std::fs::write(&p, body_win).unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = body_win;
            let p = dir.join(name);
            std::fs::write(&p, body_unix).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    fn write_mock_npm_success(bin_dir: &Path, counter_file: &Path, adapter_bin: &str) {
        let c_win = counter_file.display().to_string();
        let c_unix = counter_file.display().to_string();
        let win = format!(
            "@echo off\r\n\
            setlocal\r\n\
            set \"PREFIX=%~3\"\r\n\
            echo hit>>\"{c_win}\"\r\n\
            if not exist \"%PREFIX%\\node_modules\\.bin\" mkdir \"%PREFIX%\\node_modules\\.bin\"\r\n\
            >\"%PREFIX%\\node_modules\\.bin\\{adapter_bin}.cmd\" echo @echo off\r\n\
            >>\"%PREFIX%\\node_modules\\.bin\\{adapter_bin}.cmd\" echo echo ok\r\n\
            exit /b 0\r\n"
        );
        let unix = format!(
            "#!/bin/sh\n\
            PREFIX=\"$3\"\n\
            echo hit >> \"{c_unix}\"\n\
            mkdir -p \"$PREFIX/node_modules/.bin\"\n\
            printf '#!/bin/sh\\necho ok\\n' > \"$PREFIX/node_modules/.bin/{adapter_bin}\"\n\
            chmod +x \"$PREFIX/node_modules/.bin/{adapter_bin}\"\n"
        );
        write_mock_bin(bin_dir, "npm", &win, &unix);
    }

    fn test_pathext() -> &'static str {
        if cfg!(windows) {
            ".COM;.EXE;.BAT;.CMD"
        } else {
            ""
        }
    }

    #[test]
    fn lookup_matches_allowlist_and_rejects_paths_and_unknowns() {
        assert_eq!(
            lookup_npm_adapter("codex-acp").map(|s| s.package),
            Some("@agentclientprotocol/codex-acp")
        );
        assert_eq!(
            lookup_npm_adapter(" claude-agent-acp ").map(|s| s.package),
            Some("@agentclientprotocol/claude-agent-acp")
        );
        if cfg!(windows) {
            assert_eq!(
                lookup_npm_adapter("CODEX-ACP.CMD").map(|s| s.bin),
                Some("codex-acp")
            );
        }
        assert!(lookup_npm_adapter("/usr/local/bin/codex-acp").is_none());
        assert!(lookup_npm_adapter(r"C:\tools\codex-acp.cmd").is_none());
        assert!(lookup_npm_adapter("gemini").is_none());
        assert!(lookup_npm_adapter("opencode").is_none());
        assert!(lookup_npm_adapter("evil-pkg").is_none());
        assert!(lookup_npm_adapter("").is_none());
    }

    #[test]
    fn resolve_managed_requires_sentinel() {
        let data = scratch_dir("sentinel");
        let prefix = managed_adapter_prefix(&data, "codex-acp");
        let bin_dir = prefix.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        write_mock_bin(&bin_dir, "codex-acp", "@echo off\r\n", "#!/bin/sh\n");

        // 没有 .acp-installed 标记时应视为未安装
        assert!(resolve_managed_in(&data, "codex-acp", test_pathext()).is_none());

        std::fs::write(prefix.join(SENTINEL_FILE), "{}").unwrap();
        assert!(resolve_managed_in(&data, "codex-acp", test_pathext()).is_some());

        let _ = std::fs::remove_dir_all(data);
    }

    #[tokio::test]
    async fn ensure_npm_adapter_installs_caches_and_serializes_concurrent_calls() {
        let root = scratch_dir("install");
        let data = root.join("data");
        let mock_bin = root.join("mock_bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&mock_bin).unwrap();
        let counter = root.join("npm_hits.txt");
        write_mock_npm_success(&mock_bin, &counter, "codex-acp");

        let spec = lookup_npm_adapter("codex-acp").unwrap();
        let path_env = mock_bin.display().to_string();
        let pathext = test_pathext();

        // 并发请求同一适配器时，只应执行一次 npm install
        let (r1, r2) = tokio::join!(
            ensure_npm_adapter_with(spec, &data, &path_env, pathext, Duration::from_secs(30), None),
            ensure_npm_adapter_with(spec, &data, &path_env, pathext, Duration::from_secs(30), None),
        );
        let p1 = r1.unwrap();
        let p2 = r2.unwrap();
        assert_eq!(p1, p2);
        assert!(p1.is_file());
        assert!(managed_adapter_prefix(&data, "codex-acp")
            .join(SENTINEL_FILE)
            .is_file());

        // 再次调用应直接命中缓存
        let p3 = ensure_npm_adapter_with(
            spec,
            &data,
            &path_env,
            pathext,
            Duration::from_secs(30),
            None,
        )
        .await
        .unwrap();
        assert_eq!(p3, p1);

        let hits = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(hits.lines().count(), 1, "expected 1 npm invocation, got: {hits}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_npm_adapter_reports_missing_npm_and_nonzero_failure() {
        let root = scratch_dir("errors");
        let data = root.join("data");
        let empty_path = root.join("empty_bin");
        let fail_bin = root.join("fail_bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&empty_path).unwrap();
        std::fs::create_dir_all(&fail_bin).unwrap();

        let spec = lookup_npm_adapter("claude-agent-acp").unwrap();
        let pathext = test_pathext();

        let missing_err = ensure_npm_adapter_with(
            spec,
            &data,
            &empty_path.display().to_string(),
            pathext,
            Duration::from_secs(10),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            missing_err.contains("npm") && missing_err.contains("Node.js"),
            "unexpected missing npm error: {missing_err}"
        );

        write_mock_bin(
            &fail_bin,
            "npm",
            "@echo off\r\necho mock npm boom 1>&2\r\nexit /b 1\r\n",
            "#!/bin/sh\necho mock npm boom >&2\nexit 1\n",
        );
        let fail_err = ensure_npm_adapter_with(
            spec,
            &data,
            &fail_bin.display().to_string(),
            pathext,
            Duration::from_secs(10),
            None,
        )
        .await
        .unwrap_err();
        assert!(fail_err.contains("mock npm boom"), "got: {fail_err}");
        assert!(!managed_adapter_prefix(&data, "claude-agent-acp")
            .join(SENTINEL_FILE)
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_npm_adapter_cancels_slow_install() {
        let root = scratch_dir("cancel");
        let data = root.join("data");
        let slow_bin = root.join("slow_bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&slow_bin).unwrap();

        write_mock_bin(
            &slow_bin,
            "npm",
            "@echo off\r\nping -n 15 127.0.0.1 >nul\r\nexit /b 0\r\n",
            "#!/bin/sh\nsleep 15\n",
        );

        let spec = lookup_npm_adapter("claude-agent-acp").unwrap();
        let (tx, mut rx) = watch::channel(false);
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let _ = tx.send(true);
        });

        let sep = if cfg!(windows) { ';' } else { ':' };
        let sys_bin = if cfg!(windows) {
            std::env::var("SystemRoot")
                .map(|r| format!(r"{r}\System32"))
                .unwrap_or_else(|_| r"C:\Windows\System32".to_string())
        } else {
            "/bin:/usr/bin".to_string()
        };
        let path_env = format!("{}{sep}{sys_bin}", slow_bin.display());

        let start = std::time::Instant::now();
        let err = ensure_npm_adapter_with(
            spec,
            &data,
            &path_env,
            test_pathext(),
            Duration::from_secs(30),
            Some(&mut rx),
        )
        .await
        .unwrap_err();
        stopper.await.unwrap();

        assert!(err.contains("取消"), "got: {err}");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!managed_adapter_prefix(&data, "claude-agent-acp")
            .join(SENTINEL_FILE)
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }
}
