use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 500;

pub fn roots() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(clean_path_str(&home));
    }
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            if Path::new(&drive).exists() {
                out.push(drive);
            }
        }
    } else if !out.iter().any(|p| p == "/") {
        out.push("/".to_string());
    }
    out
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

pub fn clean_path_str(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    #[cfg(windows)]
    if let Some(stripped) = s.strip_prefix("\\\\?\\") {
        return stripped.to_string();
    }
    s
}

pub fn list_dir(raw_path: &str) -> Result<Value, String> {
    let root_list = roots();
    let dir = if raw_path.trim().is_empty() {
        dirs::home_dir()
            .or_else(|| root_list.first().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"))
    } else {
        expand_tilde(raw_path)
    };

    if !dir.exists() {
        return Err(format!("目录不存在：{}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("不是文件夹：{}", dir.display()));
    }

    let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let path_str = clean_path_str(&canon);
    let parent = canon
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(clean_path_str);

    let read = std::fs::read_dir(&canon).map_err(|e| format!("无法读取目录：{e}"))?;
    let mut dirs_vec: Vec<(String, String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let ft = entry.file_type().ok()?;
            if !ft.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let full = clean_path_str(&entry.path());
            let has_children = std::fs::read_dir(entry.path())
                .map(|mut rd| {
                    rd.any(|sub| {
                        sub.ok()
                            .is_some_and(|s| s.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    })
                })
                .unwrap_or(false);
            Some((name, full, has_children))
        })
        .collect();

    dirs_vec.sort_by_key(|(name, _, _)| name.to_lowercase());
    dirs_vec.truncate(MAX_ENTRIES);

    let entries: Vec<Value> = dirs_vec
        .into_iter()
        .map(|(name, path, has_children)| {
            json!({
                "name": name,
                "path": path,
                "has_children": has_children,
            })
        })
        .collect();

    Ok(json!({
        "path": path_str,
        "parent": parent,
        "roots": root_list,
        "entries": entries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_dir_filters_files_and_sorts() {
        let base = std::env::temp_dir().join(format!("acp_dirs_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("z-folder")).unwrap();
        std::fs::create_dir_all(base.join("A-folder/nested")).unwrap();
        std::fs::write(base.join("ignored.txt"), "hello").unwrap();

        let res = list_dir(&base.to_string_lossy()).unwrap();
        let entries = res["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "A-folder");
        assert_eq!(entries[0]["has_children"], true);
        assert_eq!(entries[1]["name"], "z-folder");
        assert_eq!(entries[1]["has_children"], false);

        let _ = std::fs::remove_dir_all(base);
    }
}
