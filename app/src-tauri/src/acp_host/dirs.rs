use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

const MAX_ENTRIES: usize = 500;
const MAX_TEXT_PREVIEW_BYTES: usize = 512 * 1024;
const MAX_BINARY_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;
const SENSITIVE_DIR_NAMES: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube"];

pub fn is_sensitive_path(p: &Path) -> bool {
    let full_low = p.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    if full_low == "/etc/shadow" || full_low == "/etc/gshadow" || full_low == "/etc/master.passwd" {
        return true;
    }
    p.components().any(|c| {
        matches!(c, std::path::Component::Normal(name) if {
            let low = name.to_string_lossy().to_ascii_lowercase();
            SENSITIVE_DIR_NAMES.contains(&low.as_str())
        })
    })
}

pub fn roots() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(clean_path_str(&home));
    }
    if out.is_empty() && cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            if Path::new(&drive).exists() {
                out.push(drive);
            }
        }
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
    if is_sensitive_path(&canon) {
        return Err("拒绝浏览包含密钥或凭证的敏感目录".to_string());
    }
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
            if SENSITIVE_DIR_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
                return None;
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewMediaKind {
    BinaryMedia,
    Text,
    Unknown,
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = *chunk.get(1).unwrap_or(&0) as usize;
        let b2 = *chunk.get(2).unwrap_or(&0) as usize;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) & 0x3f] as char);
        out.push(TABLE[(n >> 12) & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_line_fragment(raw: &str) -> &str {
    if let Some(idx) = raw.rfind("#L") {
        let tail = &raw[idx + 2..];
        if !tail.is_empty()
            && tail
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == 'L' || c == 'C' || c == ':')
        {
            return &raw[..idx];
        }
    }
    raw
}

fn strip_trailing_line_col(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut end = bytes.len();
    for _ in 0..2 {
        let mut i = end;
        while i > 0 && bytes[i - 1].is_ascii_digit() {
            i -= 1;
        }
        if i == end || i == 0 || bytes[i - 1] != b':' {
            break;
        }
        let prefix = &raw[..i - 1];
        // 避免误剥 Windows 盘符前缀（如 "C:"）
        if prefix.len() <= 1 {
            break;
        }
        end = i - 1;
    }
    if end < bytes.len() {
        Some(&raw[..end])
    } else {
        None
    }
}

fn normalize_raw_file_input(raw: &str) -> String {
    let trimmed = strip_line_fragment(raw.trim());
    if trimmed
        .as_bytes()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"file://"))
    {
        if let Ok(u) = Url::parse(trimmed) {
            if let Ok(fp) = u.to_file_path() {
                return fp.to_string_lossy().into_owned();
            }
            let decoded = percent_decode(u.path());
            let bytes = decoded.as_bytes();
            if bytes.len() >= 3
                && bytes[0] == b'/'
                && bytes[1].is_ascii_alphabetic()
                && bytes[2] == b':'
            {
                return decoded[1..].to_string();
            }
            return decoded;
        }
    }
    trimmed.to_string()
}

fn resolve_candidate_path(normalized: &str, raw_cwd: Option<&str>) -> PathBuf {
    let expanded = expand_tilde(normalized);
    if expanded.is_absolute() {
        return expanded;
    }
    if let Some(cwd) = raw_cwd.map(str::trim).filter(|c| !c.is_empty()) {
        return expand_tilde(cwd).join(expanded);
    }
    expanded
}

fn guess_mime_and_kind(path: &Path) -> (&'static str, PreviewMediaKind) {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => ("image/png", PreviewMediaKind::BinaryMedia),
        "jpg" | "jpeg" => ("image/jpeg", PreviewMediaKind::BinaryMedia),
        "gif" => ("image/gif", PreviewMediaKind::BinaryMedia),
        "webp" => ("image/webp", PreviewMediaKind::BinaryMedia),
        "bmp" => ("image/bmp", PreviewMediaKind::BinaryMedia),
        "ico" => ("image/x-icon", PreviewMediaKind::BinaryMedia),
        "avif" => ("image/avif", PreviewMediaKind::BinaryMedia),
        "svg" => ("image/svg+xml", PreviewMediaKind::Text),
        "pdf" => ("application/pdf", PreviewMediaKind::BinaryMedia),
        "mp3" => ("audio/mpeg", PreviewMediaKind::BinaryMedia),
        "wav" => ("audio/wav", PreviewMediaKind::BinaryMedia),
        "ogg" => ("audio/ogg", PreviewMediaKind::BinaryMedia),
        "m4a" => ("audio/mp4", PreviewMediaKind::BinaryMedia),
        "aac" => ("audio/aac", PreviewMediaKind::BinaryMedia),
        "flac" => ("audio/flac", PreviewMediaKind::BinaryMedia),
        "mp4" => ("video/mp4", PreviewMediaKind::BinaryMedia),
        "webm" => ("video/webm", PreviewMediaKind::BinaryMedia),
        "mov" => ("video/quicktime", PreviewMediaKind::BinaryMedia),
        "md" | "markdown" | "mdx" => ("text/markdown", PreviewMediaKind::Text),
        "csv" => ("text/csv", PreviewMediaKind::Text),
        "tsv" => ("text/tab-separated-values", PreviewMediaKind::Text),
        "json" | "jsonc" | "json5" | "jsonl" => ("application/json", PreviewMediaKind::Text),
        "html" | "htm" => ("text/html", PreviewMediaKind::Text),
        "css" | "scss" | "sass" | "less" => ("text/css", PreviewMediaKind::Text),
        "xml" => ("application/xml", PreviewMediaKind::Text),
        "yaml" | "yml" => ("text/yaml", PreviewMediaKind::Text),
        "txt" | "log" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "toml" | "lock"
        | "py" | "pyi" | "go" | "mod" | "sum" | "java" | "kt" | "kts" | "swift" | "c" | "h"
        | "cc" | "cpp" | "hpp" | "cs" | "rb" | "php" | "lua" | "zig" | "sh" | "bash" | "zsh"
        | "fish" | "ps1" | "bat" | "cmd" | "sql" | "graphql" | "gql" | "proto" | "ini" | "cfg"
        | "conf" | "env" | "vue" | "svelte" | "astro" | "dockerfile" | "gradle" | "plist" => {
            ("text/plain", PreviewMediaKind::Text)
        }
        _ => ("application/octet-stream", PreviewMediaKind::Unknown),
    }
}

fn decode_text_bytes(buf: &[u8]) -> Option<String> {
    if buf.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    if buf.starts_with(&[0xFE, 0xFF]) {
        let u16s: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    let sample_len = buf.len().min(8192);
    if buf[..sample_len].contains(&0) {
        return None;
    }
    match std::str::from_utf8(buf) {
        Ok(s) => Some(s.to_string()),
        Err(e) if e.error_len().is_none() => {
            // 仅尾部因字节截断落在多字节 UTF-8 字符中间
            Some(std::str::from_utf8(&buf[..e.valid_up_to()]).unwrap_or("").to_string())
        }
        Err(_) => Some(String::from_utf8_lossy(buf).into_owned()),
    }
}

pub fn read_file(
    raw_path: &str,
    raw_cwd: Option<&str>,
    allow_cwds: &[PathBuf],
) -> Result<Value, String> {
    if raw_path.trim().is_empty() {
        return Err("文件路径不能为空".to_string());
    }
    let normalized = normalize_raw_file_input(raw_path);
    let mut target = resolve_candidate_path(&normalized, raw_cwd);
    if !target.exists() {
        if let Some(stripped) = strip_trailing_line_col(&normalized) {
            let fallback = resolve_candidate_path(stripped, raw_cwd);
            if fallback.exists() {
                target = fallback;
            }
        }
    }

    if is_sensitive_path(&target) {
        return Err("拒绝读取包含密钥或凭证的敏感路径".to_string());
    }
    if !target.exists() {
        return Err(format!("文件不存在：{}", target.display()));
    }
    if !target.is_file() {
        return Err(format!("不是普通文件：{}", target.display()));
    }

    let canon = super::permission::resolve_path(&target)
        .unwrap_or_else(|| target.canonicalize().unwrap_or_else(|_| target.clone()));
    if is_sensitive_path(&canon) {
        return Err("拒绝读取包含密钥或凭证的敏感路径".to_string());
    }

    if !allow_cwds.is_empty() {
        let permitted = allow_cwds.iter().any(|allowed| {
            super::permission::resolve_path(allowed)
                .map(|canon_allowed| canon.starts_with(&canon_allowed))
                .unwrap_or(false)
        });
        if !permitted {
            return Err(format!(
                "文件路径 {} 未在允许的工作区目录范围内",
                canon.display()
            ));
        }
    }

    let meta = std::fs::metadata(&canon).map_err(|e| format!("无法读取文件元数据：{e}"))?;
    let size = meta.len();
    let path_str = clean_path_str(&canon);
    let file_name = canon
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.clone());
    let (guessed_mime, kind) = guess_mime_and_kind(&canon);

    if kind == PreviewMediaKind::BinaryMedia {
        if size > MAX_BINARY_PREVIEW_BYTES {
            return Ok(json!({
                "path": path_str,
                "file_name": file_name,
                "mime": guessed_mime,
                "size": size,
                "encoding": "none",
                "content": Value::Null,
                "truncated": true,
                "reason": "too_large",
            }));
        }
        let bytes = std::fs::read(&canon).map_err(|e| format!("无法读取文件：{e}"))?;
        return Ok(json!({
            "path": path_str,
            "file_name": file_name,
            "mime": guessed_mime,
            "size": size,
            "encoding": "base64",
            "content": base64_encode(&bytes),
            "truncated": false,
            "reason": Value::Null,
        }));
    }

    let f = std::fs::File::open(&canon).map_err(|e| format!("无法打开文件：{e}"))?;
    let mut buf = Vec::with_capacity((size as usize).min(MAX_TEXT_PREVIEW_BYTES));
    f.take(MAX_TEXT_PREVIEW_BYTES as u64)
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取文件失败：{e}"))?;

    match decode_text_bytes(&buf) {
        Some(text) => {
            let final_mime = if guessed_mime == "application/octet-stream" {
                "text/plain"
            } else {
                guessed_mime
            };
            Ok(json!({
                "path": path_str,
                "file_name": file_name,
                "mime": final_mime,
                "size": size,
                "encoding": "utf8",
                "content": text,
                "truncated": size > buf.len() as u64,
                "reason": Value::Null,
            }))
        }
        None => Ok(json!({
            "path": path_str,
            "file_name": file_name,
            "mime": guessed_mime,
            "size": size,
            "encoding": "none",
            "content": Value::Null,
            "truncated": false,
            "reason": "binary",
        })),
    }
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

    #[test]
    fn read_file_handles_relative_line_suffix_file_uri_binary_and_security() {
        let base = std::env::temp_dir().join(format!("acp_read_file_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join(".ssh")).unwrap();
        let ts_file = base.join("src/app.ts");
        std::fs::write(&ts_file, "export const x = 42;\n").unwrap();
        std::fs::write(base.join(".ssh/id_rsa"), "secret").unwrap();
        let png_file = base.join("logo.png");
        std::fs::write(&png_file, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let bin_file = base.join("raw.bin");
        std::fs::write(&bin_file, [0x00, 0x01, 0x02, 0xFF]).unwrap();

        let cwd_str = base.to_string_lossy().to_string();

        // 1. 相对路径 + 行列后缀自动剥离
        let r1 = read_file("src/app.ts:1:8", Some(&cwd_str), &[]).unwrap();
        assert_eq!(r1["file_name"], "app.ts");
        assert_eq!(r1["encoding"], "utf8");
        assert_eq!(r1["content"], "export const x = 42;\n");
        assert_eq!(r1["truncated"], false);

        // 2. file:// URI + #L 行号锚点
        let file_uri = format!("{}#L1-L5", Url::from_file_path(&ts_file).unwrap());
        let r2 = read_file(&file_uri, None, &[]).unwrap();
        assert_eq!(r2["file_name"], "app.ts");
        assert_eq!(r2["content"], "export const x = 42;\n");

        // 3. 图片二进制返回 base64
        let r_png = read_file("logo.png", Some(&cwd_str), &[]).unwrap();
        assert_eq!(r_png["mime"], "image/png");
        assert_eq!(r_png["encoding"], "base64");
        assert!(r_png["content"].as_str().unwrap().len() >= 8);

        // 4. 未知二进制返回 encoding=none, reason=binary
        let r_bin = read_file("raw.bin", Some(&cwd_str), &[]).unwrap();
        assert_eq!(r_bin["encoding"], "none");
        assert_eq!(r_bin["reason"], "binary");

        // 5. 敏感目录拦截
        assert!(read_file(".ssh/id_rsa", Some(&cwd_str), &[]).is_err());

        // 6. allow_cwds 沙箱限制
        let other_dir = std::env::temp_dir().join(format!("acp_other_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&other_dir).unwrap();
        assert!(read_file("src/app.ts", Some(&cwd_str), std::slice::from_ref(&other_dir)).is_err());
        assert!(read_file("src/app.ts", Some(&cwd_str), std::slice::from_ref(&base)).is_ok());

        let _ = std::fs::remove_dir_all(base);
        let _ = std::fs::remove_dir_all(other_dir);
    }

    #[test]
    fn read_file_handles_chinese_relative_paths_without_panicking() {
        let base = std::env::temp_dir().join(format!("acp_read_cn_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("中文目录")).unwrap();
        std::fs::create_dir_all(base.join("文档")).unwrap();

        let root_doc = base.join("中文文档.md");
        let nested_doc = base.join("中文目录/说明.txt");
        let dot_doc = base.join("文档/备注.md");
        std::fs::write(&root_doc, "# 中文标题\n内容\n").unwrap();
        std::fs::write(&nested_doc, "目录中的文本\n").unwrap();
        std::fs::write(&dot_doc, "相对点前缀\n").unwrap();

        let cwd_str = base.to_string_lossy().to_string();

        for (raw, expected_name, expected_content) in [
            ("中文文档.md", "中文文档.md", "# 中文标题\n内容\n"),
            ("中文文档.md:1:2", "中文文档.md", "# 中文标题\n内容\n"),
            ("中文文档.md#L1-L2", "中文文档.md", "# 中文标题\n内容\n"),
            ("中文目录/说明.txt", "说明.txt", "目录中的文本\n"),
            ("./文档/备注.md", "备注.md", "相对点前缀\n"),
        ] {
            let res = read_file(raw, Some(&cwd_str), &[]).unwrap();
            assert_eq!(res["file_name"], expected_name, "raw={raw}");
            assert_eq!(res["encoding"], "utf8", "raw={raw}");
            assert_eq!(res["content"], expected_content, "raw={raw}");
        }

        let file_uri = Url::from_file_path(&root_doc).unwrap().to_string();
        let upper_uri = format!("FILE://{}", &file_uri[7..]);
        let res_uri = read_file(&upper_uri, None, &[]).unwrap();
        assert_eq!(res_uri["file_name"], "中文文档.md");
        assert_eq!(res_uri["content"], "# 中文标题\n内容\n");

        let _ = std::fs::remove_dir_all(base);
    }
}
