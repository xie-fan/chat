use super::dirs::{clean_path_str, expand_tilde};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 拆分命令行（支持单引号/双引号），返回 (program, args)。
pub fn split_cmdline(raw: &str) -> Result<(String, Vec<String>), String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;

    let mut chars = raw.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' && chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    in_token = true;
                } else if c.is_whitespace() {
                    if in_token {
                        parts.push(std::mem::take(&mut cur));
                        in_token = false;
                    }
                } else {
                    cur.push(c);
                    in_token = true;
                }
            }
        }
    }
    if quote.is_some() {
        return Err("命令行存在未闭合的引号".to_string());
    }
    if in_token {
        parts.push(cur);
    }
    if parts.is_empty() {
        return Err("启动命令不能为空".to_string());
    }
    let prog = parts.remove(0);
    Ok((prog, parts))
}

pub fn resolve_in(command: &str, path_env: &str, pathext: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let exts = pathext_list(pathext);
    if command.contains('/') || command.contains('\\') {
        return try_file(Path::new(command), &exts);
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path_env.split(sep) {
        if dir.is_empty() {
            continue;
        }
        if let Some(hit) = try_file(&Path::new(dir).join(command), &exts) {
            return Some(hit);
        }
    }
    None
}

pub fn effective_path_env() -> String {
    let raw = std::env::var("PATH").unwrap_or_default();
    #[cfg(unix)]
    {
        let mut parts: Vec<String> = raw
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
            if !parts.iter().any(|p| p == extra) {
                parts.push(extra.to_string());
            }
        }
        parts.join(":")
    }
    #[cfg(not(unix))]
    {
        raw
    }
}

pub fn default_pathext_env() -> String {
    std::env::var("PATHEXT").unwrap_or_else(|_| default_pathext())
}

pub fn prepare(command: &str, args: &[String]) -> Command {
    let path = effective_path_env();
    let pathext = default_pathext_env();
    prepare_in(command, args, &path, &pathext)
}

pub fn prepare_resolved(path: &Path, args: &[String], path_env: &str) -> Command {
    let mut cmd = Command::new(path);
    cmd.args(args);
    if !path_env.is_empty() {
        cmd.env("PATH", path_env);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

pub fn prepare_in(command: &str, args: &[String], path_env: &str, pathext: &str) -> Command {
    match resolve_in(command, path_env, pathext) {
        Some(path) => prepare_resolved(&path, args, path_env),
        None => {
            let mut c = Command::new(command);
            c.args(args);
            if !path_env.is_empty() {
                c.env("PATH", path_env);
            }
            #[cfg(windows)]
            c.creation_flags(CREATE_NO_WINDOW);
            c
        }
    }
}

pub fn configure_child_group(cmd: &mut Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

pub async fn kill_child_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            let mut tk = Command::new("taskkill");
            tk.args(["/T", "/F", "/PID", &pid.to_string()]);
            tk.creation_flags(CREATE_NO_WINDOW);
            let _ = tk.status().await;
        }
        #[cfg(unix)]
        {
            let mut k = Command::new("kill");
            k.args(["-KILL", &format!("-{pid}")]);
            let _ = k.status().await;
        }
    }
    let _ = child.kill().await;
}

fn default_pathext() -> String {
    if cfg!(windows) {
        ".COM;.EXE;.BAT;.CMD".into()
    } else {
        String::new()
    }
}

fn pathext_list(pathext: &str) -> Vec<String> {
    pathext
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('.') {
                s.to_string()
            } else {
                format!(".{s}")
            }
        })
        .collect()
}

fn try_file(base: &Path, exts: &[String]) -> Option<PathBuf> {
    let name = base.file_name()?.to_string_lossy();
    let parent = base.parent().unwrap_or(Path::new(""));
    if Path::new(name.as_ref()).extension().is_some() {
        let exact = parent.join(name.as_ref());
        return exact.is_file().then_some(exact);
    }
    for ext in exts {
        let cand = parent.join(format!("{name}{ext}"));
        if cand.is_file() {
            return Some(cand);
        }
    }
    let exact = parent.join(name.as_ref());
    exact.is_file().then_some(exact)
}

pub fn probe_agent(cmd: &str) -> Value {
    let path = effective_path_env();
    let pathext = default_pathext_env();
    probe_agent_in(cmd, &super::host_data_dir(), &path, &pathext)
}

pub fn probe_agent_in(cmd: &str, data_dir: &Path, path_env: &str, pathext: &str) -> Value {
    let Ok((prog, _)) = split_cmdline(cmd) else {
        return json!({ "ok": false, "installable": false });
    };
    let ok = super::adapters::resolve_agent_program_in(data_dir, &prog, path_env, pathext).is_some();
    let adapter = super::adapters::lookup_npm_adapter(&prog);
    json!({
        "ok": ok,
        "installable": adapter.is_some(),
        "package": adapter.map(|a| a.package),
    })
}

fn empty_hooks_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("chat-acp-empty-hooks");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn hardened_git_args(args: &[&str]) -> Vec<String> {
    let hooks_arg = format!("core.hooksPath={}", empty_hooks_dir().display());
    let mut out = vec![
        "-c".to_string(),
        hooks_arg,
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
    ];
    for a in args {
        out.push((*a).to_string());
    }
    out
}

async fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let full_args = hardened_git_args(args);
    let mut cmd = prepare("git", &full_args);
    cmd.current_dir(dir);
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn git_info(dir: &Path) -> (String, Vec<String>) {
    let branch = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let branches = run_git(dir, &["branch", "--format=%(refname:short)"])
        .await
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    (branch, branches)
}

pub async fn probe_workspace(raw_cwd: &str) -> Result<Value, String> {
    let dir = expand_tilde(raw_cwd);
    if !dir.is_dir() {
        return Err(format!("工作目录不存在或不是文件夹：{}", dir.display()));
    }
    let canon = dir.canonicalize().unwrap_or(dir);
    if super::dirs::is_sensitive_path(&canon) {
        return Err("不允许将敏感凭证目录设为工作区".to_string());
    }
    let cwd = clean_path_str(&canon);
    let (branch, branches) = git_info(&canon).await;
    Ok(json!({
        "cwd": cwd,
        "branch": branch,
        "branches": branches,
    }))
}

pub async fn git_checkout(raw_cwd: &str, branch: &str) -> Result<Value, String> {
    let dir = expand_tilde(raw_cwd);
    if !dir.is_dir() {
        return Err(format!("工作目录不存在：{}", dir.display()));
    }
    let b = branch.trim();
    if b.is_empty() || b.starts_with('-') {
        return Err("分支名不合法".to_string());
    }
    let check_args = hardened_git_args(&["check-ref-format", "--branch", b]);
    let mut check_cmd = prepare("git", &check_args);
    check_cmd.current_dir(&dir);
    let check_out = check_cmd
        .output()
        .await
        .map_err(|e| format!("校验分支名失败：{e}"))?;
    if !check_out.status.success() {
        return Err("非法 Git 分支名".to_string());
    }

    let co_args = hardened_git_args(&["checkout", b, "--"]);
    let mut cmd = prepare("git", &co_args);
    cmd.current_dir(&dir);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("执行 git checkout 失败：{e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git checkout 失败".to_string()
        } else {
            stderr
        });
    }
    let (cur, branches) = git_info(&dir).await;
    Ok(json!({
        "branch": cur,
        "branches": branches,
    }))
}

pub async fn collect_git_diffs(dir: &Path) -> Value {
    let Some(numstat) = run_git(dir, &["diff", "--numstat", "HEAD"]).await else {
        return json!([]);
    };
    let mut out = Vec::new();
    for line in numstat.lines().take(10) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        let added: i64 = cols[0].parse().unwrap_or(0);
        let removed: i64 = cols[1].parse().unwrap_or(0);
        let file = cols[2].to_string();
        let patch = run_git(dir, &["diff", "-U2", "HEAD", "--", &file])
            .await
            .unwrap_or_default();
        let diff_lines: Vec<String> = patch
            .lines()
            .skip_while(|l| !l.starts_with("@@"))
            .take(80)
            .map(str::to_string)
            .collect();
        out.push(json!({
            "file": file,
            "added": added,
            "removed": removed,
            "status": "modified",
            "diff_lines": diff_lines,
        }));
    }
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_commands() {
        let (prog, args) = split_cmdline(r#""C:\Program Files\node\npx.cmd" @scope/pkg --acp"#).unwrap();
        assert_eq!(prog, r"C:\Program Files\node\npx.cmd");
        assert_eq!(args, vec!["@scope/pkg", "--acp"]);
    }

    #[tokio::test]
    async fn probes_temp_workspace_dir() {
        let base = std::env::temp_dir().join(format!("acp_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let res = probe_workspace(&base.to_string_lossy()).await.unwrap();
        assert!(!res["cwd"].as_str().unwrap().is_empty());
        assert!(res["branches"].is_array());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn probe_agent_in_reports_installable_and_installed_states() {
        let data = std::env::temp_dir().join(format!("acp_probe_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();

        let missing = probe_agent_in("codex-acp", &data, "", ".CMD");
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["installable"], true);
        assert_eq!(missing["package"], "@agentclientprotocol/codex-acp");

        let native = probe_agent_in("gemini --acp", &data, "", ".CMD");
        assert_eq!(native["ok"], false);
        assert_eq!(native["installable"], false);
        assert!(native["package"].is_null());

        let prefix = super::super::adapters::managed_adapter_prefix(&data, "codex-acp");
        let bin_dir = prefix.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin_name = if cfg!(windows) { "codex-acp.cmd" } else { "codex-acp" };
        std::fs::write(bin_dir.join(bin_name), "echo ok\n").unwrap();
        std::fs::write(prefix.join(".acp-installed"), "{}").unwrap();

        let ready = probe_agent_in("codex-acp", &data, "", ".CMD");
        assert_eq!(ready["ok"], true);
        assert_eq!(ready["installable"], true);

        let _ = std::fs::remove_dir_all(data);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pathext_resolves_cmd_and_runs_it() {
        let root = std::env::temp_dir().join(format!("acp_cmd_{}", uuid::Uuid::new_v4()));
        let dir = root.join("bin dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hello.cmd"),
            "@echo off\r\necho pong:%~1\r\n",
        )
        .unwrap();
        let path_env = dir.display().to_string();
        let resolved = resolve_in("hello", &path_env, ".COM;.EXE;.BAT;.CMD").unwrap();
        assert!(resolved.extension().unwrap().eq_ignore_ascii_case("cmd"));

        let mut cmd = prepare_resolved(&resolved, &["arg1".to_string()], &path_env);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let out = cmd.output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("pong:arg1"),
            "status={:?} stdout={text} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_shim_forwards_stdin() {
        use tokio::io::AsyncWriteExt;

        let root = std::env::temp_dir().join(format!("acp_stdin_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("read.cmd");
        std::fs::write(&script, "@echo off\r\nset /p LINE=\r\necho got:%LINE%\r\n").unwrap();
        let mut cmd = prepare_resolved(&script, &[], "");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(b"ping\r\n").await.unwrap();
        }
        let out = child.wait_with_output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("got:ping"),
            "status={:?} stdout={text} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
