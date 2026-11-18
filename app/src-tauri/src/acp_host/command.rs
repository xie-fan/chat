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

pub fn prepare(command: &str, args: &[String]) -> Command {
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| default_pathext());
    prepare_in(command, args, &path, &pathext)
}

pub fn prepare_in(command: &str, args: &[String], path_env: &str, pathext: &str) -> Command {
    let mut cmd = match resolve_in(command, path_env, pathext) {
        Some(path) if is_batch(&path) => batch_command(&path, args),
        Some(path) => {
            let mut c = Command::new(path);
            c.args(args);
            c
        }
        None => {
            let mut c = Command::new(command);
            c.args(args);
            c
        }
    };
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
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

fn is_batch(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("cmd" | "bat")
    )
}

fn batch_command(script: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(if cfg!(windows) { "cmd.exe" } else { "sh" });
    #[cfg(windows)]
    {
        let mut line = String::from("/d /s /c \"");
        line.push('"');
        line.push_str(&script.display().to_string());
        line.push('"');
        for a in args {
            line.push(' ');
            line.push_str(&quote_cmd_arg(a));
        }
        line.push('"');
        cmd.raw_arg(line);
    }
    #[cfg(not(windows))]
    {
        cmd.arg(script);
        cmd.args(args);
    }
    cmd
}

#[cfg(windows)]
fn quote_cmd_arg(a: &str) -> String {
    if a.is_empty() || a.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

pub fn probe_agent(cmd: &str) -> Value {
    let Ok((prog, _)) = split_cmdline(cmd) else {
        return json!({ "ok": false });
    };
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| default_pathext());
    let ok = resolve_in(&prog, &path, &pathext).is_some();
    json!({ "ok": ok })
}

async fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = prepare("git", &args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
    if b.is_empty() {
        return Err("分支名不能为空".to_string());
    }
    let mut cmd = prepare("git", &["checkout".into(), b.into()]);
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
}
