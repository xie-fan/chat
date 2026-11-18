use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

const READ_KINDS: &[&str] = &["read", "search", "glob", "grep", "ls", "fs/read_text_file", "fs.read"];

#[derive(Default)]
pub struct PermissionCache {
    decisions: HashMap<String, bool>,
}

impl PermissionCache {
    pub fn get(&self, key: &str) -> Option<bool> {
        self.decisions.get(key).copied()
    }

    pub fn record(&mut self, key: String, choice: &str) {
        if choice == "allow_always" {
            self.decisions.insert(key, true);
        } else if choice == "reject_always" {
            self.decisions.insert(key, false);
        }
    }
}

pub fn cache_key(session_id: &str, cwd: &Path, tool_kind: &str, cmd_or_title: &str) -> String {
    let canon_cwd = resolve_path(cwd)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string_lossy().to_string());
    let kind = tool_kind.to_ascii_lowercase();
    let extra = if matches!(
        kind.as_str(),
        "execute" | "terminal/run" | "bash" | "shell" | "command"
    ) {
        cmd_or_title.split_whitespace().next().unwrap_or("")
    } else {
        ""
    };
    format!("{session_id}|{canon_cwd}|{kind}|{extra}")
}

pub fn evaluate_policy(
    policy: &str,
    tool_kind: &str,
    locations: &[String],
    cwd: &Path,
) -> Option<bool> {
    if policy == "allow_all" {
        return Some(true);
    }
    let kind = tool_kind.to_ascii_lowercase();
    if policy == "auto_read"
        && READ_KINDS.contains(&kind.as_str())
        && !locations.is_empty()
        && locations.iter().all(|loc| path_inside_root(loc, cwd))
    {
        return Some(true);
    }
    None
}

pub fn path_inside_root(raw: &str, root: &Path) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let p = Path::new(trimmed);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let Some(norm_p) = resolve_path(&joined) else {
        return false;
    };
    let Some(norm_root) = resolve_path(root) else {
        return false;
    };
    if super::dirs::is_sensitive_path(&norm_p) {
        return false;
    }
    norm_p.starts_with(norm_root)
}

fn strip_win_prefix(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(stripped) = s.strip_prefix("\\\\?\\") {
            return PathBuf::from(stripped);
        }
    }
    p
}

fn lexical_clean(p: &Path) -> Option<PathBuf> {
    let mut stack = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(x) => {
                let s = x.as_os_str().to_string_lossy();
                if let Some(stripped) = s.strip_prefix("\\\\?\\") {
                    stack.push(stripped);
                } else {
                    stack.push(x.as_os_str());
                }
            }
            Component::RootDir => stack.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !stack.pop() {
                    return None;
                }
            }
            Component::Normal(x) => stack.push(x),
        }
    }
    Some(stack)
}

/// 沿父目录向上寻找已存在祖先并 canonicalize，再拼接不存在的尾部相对路径，
/// 防止通过工作区内指向外部的符号链接逃逸写新文件。
pub fn resolve_path(p: &Path) -> Option<PathBuf> {
    let clean = lexical_clean(p)?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = clean.as_path();
    loop {
        if probe.exists() {
            let mut base = strip_win_prefix(probe.canonicalize().ok()?);
            for seg in tail.into_iter().rev() {
                base.push(seg);
            }
            return Some(base);
        }
        let name = probe.file_name()?;
        tail.push(name.to_os_string());
        probe = probe.parent()?;
    }
}

pub fn location_strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let s = item
                        .as_str()
                        .or_else(|| item.get("path").and_then(Value::as_str))?;
                    let trimmed = s.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn pick_option_id(options: &Value, choice: &str) -> Option<String> {
    let arr = options.as_array()?;
    if let Some(exact) = arr.iter().find_map(|o| {
        let oid = o["optionId"].as_str().unwrap_or("");
        let kind = o["kind"].as_str().unwrap_or("");
        (oid == choice || kind == choice).then(|| oid.to_string())
    }) {
        return Some(exact);
    }
    let want_allow = choice.starts_with("allow");
    arr.iter().find_map(|o| {
        let kind = o["kind"].as_str().unwrap_or("");
        let oid = o["optionId"].as_str()?;
        let is_allow = kind.contains("allow");
        (is_allow == want_allow).then(|| oid.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_read_allows_inside_cwd_and_blocks_writes_or_traversal() {
        let root = std::env::temp_dir().join("acp_perm_root");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            evaluate_policy("auto_read", "read", &["src/main.rs".into()], &root),
            Some(true)
        );
        assert_eq!(
            evaluate_policy("auto_read", "read", &["../../etc/passwd".into()], &root),
            None
        );
        assert_eq!(
            evaluate_policy("auto_read", "edit", &["src/main.rs".into()], &root),
            None
        );
        assert_eq!(
            evaluate_policy("allow_all", "terminal/run", &[], &root),
            Some(true)
        );
        assert_eq!(evaluate_policy("ask", "read", &["a.rs".into()], &root), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn picks_matching_option_id() {
        let opts = json!([
            {"optionId": "opt-allow", "kind": "allow_once"},
            {"optionId": "opt-deny", "kind": "reject_once"}
        ]);
        assert_eq!(pick_option_id(&opts, "allow_once").as_deref(), Some("opt-allow"));
        assert_eq!(pick_option_id(&opts, "reject_always").as_deref(), Some("opt-deny"));
    }

    #[test]
    fn think_tool_is_not_auto_allowed_under_ask_or_auto_read() {
        let root = std::env::temp_dir().join(format!("acp_think_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(evaluate_policy("ask", "think", &[], &root), None);
        assert_eq!(evaluate_policy("auto_read", "think", &[], &root), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cache_key_is_scoped_by_session_cwd_and_command() {
        let base = cache_key("s1", Path::new("/tmp/a"), "terminal/run", "cargo test");
        assert_ne!(
            base,
            cache_key("s2", Path::new("/tmp/a"), "terminal/run", "cargo test")
        );
        assert_ne!(
            base,
            cache_key("s1", Path::new("/tmp/b"), "terminal/run", "cargo test")
        );
        assert_ne!(
            base,
            cache_key("s1", Path::new("/tmp/a"), "terminal/run", "rm -rf /")
        );
    }

    #[test]
    fn path_inside_root_blocks_symlink_escape_for_nonexistent_file() {
        let uid = uuid::Uuid::new_v4();
        let inside = std::env::temp_dir().join(format!("acp_inside_{uid}"));
        let outside = std::env::temp_dir().join(format!("acp_outside_{uid}"));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let link = inside.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J should succeed");
        }

        assert!(!path_inside_root("escape/new.txt", &inside));
        assert!(path_inside_root("sub/new.txt", &inside));

        let _ = std::fs::remove_dir(&link).or_else(|_| std::fs::remove_file(&link));
        let _ = std::fs::remove_dir_all(&inside);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
