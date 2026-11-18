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
    if kind == "think" {
        return Some(true);
    }
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
    let p = Path::new(raw.trim());
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let Some(norm_p) = normalize(&joined) else {
        return false;
    };
    let Some(norm_root) = normalize(root) else {
        return false;
    };
    norm_p.starts_with(norm_root)
}

fn normalize(p: &Path) -> Option<PathBuf> {
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
    if let Ok(canon) = stack.canonicalize() {
        let s = canon.to_string_lossy();
        #[cfg(windows)]
        if let Some(stripped) = s.strip_prefix("\\\\?\\") {
            return Some(PathBuf::from(stripped));
        }
        return Some(canon);
    }
    Some(stack)
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
}
