//! 客户端在线更新。安装包由 GitHub Actions 打好、挂在 GitHub Release 上（兼容自建 Gitea
//! Release）；服务端读仓库的最新 Release 转给客户端，下载也经服务端中转（客户端不一定连得
//! 到 GitHub）。
//! 仓库由管理员在后台配置（server_settings.update_repo_url）：
//!   - "owner/repo" 或 "https://github.com/owner/repo" → GitHub Releases
//!   - 其他 http(s) 地址 → Gitea（向后兼容）
//!   - "off" → 关闭；留空 → 环境变量 CLIENT_UPDATE_REPO（默认 "xie-fan/chat"，"off" 关闭）
//!
//! GitHub 相关环境变量：GITHUB_API_BASE（默认 https://api.github.com，测试用 mock）、
//!   GITHUB_TOKEN（只发给 API host，不跟重定向进 CDN）、CLIENT_UPDATE_PROXY（更新请求走代
//!   理，不设则不走系统代理）。

use crate::error::{AppError, AppResult};
use crate::settings;
use crate::state::SharedState;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// update_repo_url / CLIENT_UPDATE_REPO 都为空时的默认仓库
const DEFAULT_REPO: &str = "xie-fan/chat";

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Gitea 仓库地址 "http://h:3000/owner/repo" → "http://h:3000/api/v1/repos/owner/repo"；
/// 不是 http(s) 仓库地址 → None
fn gitea_api_base(repo_url: &str) -> Option<String> {
    if !repo_url.starts_with("http://") && !repo_url.starts_with("https://") {
        return None;
    }
    let (scheme, rest) = repo_url.split_once("://")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next().filter(|h| !h.is_empty())?;
    let path: Vec<&str> = parts.collect();
    // Gitea 可能挂在子路径下：最后两段是 owner/repo，前面的都是前缀
    let [prefix @ .., owner, repo] = path.as_slice() else {
        return None;
    };
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let prefix: String = prefix.iter().map(|p| format!("/{p}")).collect();
    Some(format!(
        "{scheme}://{host}{prefix}/api/v1/repos/{owner}/{repo}"
    ))
}

/// 更新源：GitHub Releases 或 Gitea（api 为 .../api/v1/repos/o/r）
pub enum UpdateRepo {
    GitHub { owner: String, repo: String },
    Gitea { api: String },
}

/// 单个配置值 → 更新源。"off" / 空 / 无法识别 → None
pub fn parse_repo(v: &str) -> Option<UpdateRepo> {
    let v = v.trim().trim_end_matches('/');
    if v.is_empty() || v == "off" || v.chars().any(char::is_whitespace) {
        return None;
    }
    let gh_path = v
        .strip_prefix("https://github.com/")
        .or_else(|| v.strip_prefix("http://github.com/"))
        .or_else(|| (!v.contains("://")).then_some(v));
    match gh_path {
        Some(path) => {
            let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() != 2 {
                return None;
            }
            Some(UpdateRepo::GitHub {
                owner: parts[0].into(),
                repo: parts[1].into(),
            })
        }
        None => gitea_api_base(v).map(|api| UpdateRepo::Gitea { api }),
    }
}

/// 生效的更新源：DB 设置优先；空 → CLIENT_UPDATE_REPO → 默认 DEFAULT_REPO
fn resolve_repo(setting: &str) -> Option<UpdateRepo> {
    let setting = setting.trim();
    let eff = if setting.is_empty() {
        env_opt("CLIENT_UPDATE_REPO").unwrap_or_else(|| DEFAULT_REPO.into())
    } else {
        setting.to_string()
    };
    parse_repo(&eff)
}

fn github_api_base() -> String {
    env_opt("GITHUB_API_BASE")
        .unwrap_or_else(|| "https://api.github.com".into())
        .trim_end_matches('/')
        .to_string()
}

/// 更新请求专用 client：跟随 302（GitHub 资产 → objects.githubusercontent.com），
/// CLIENT_UPDATE_PROXY 设了才走代理（不设保持原有的 no_proxy 行为，env 每次重读便于测试）。
fn update_client() -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10));
    b = match env_opt("CLIENT_UPDATE_PROXY").and_then(|p| reqwest::Proxy::all(&p).ok()) {
        Some(proxy) => b.proxy(proxy),
        None => b.no_proxy(),
    };
    b.build().expect("build reqwest client")
}

/// GitHub/GitHub 兼容（Gitea 同名字段）的 Release 读模型
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    size: i64,
    browser_download_url: String,
}

async fn latest_release(st: &SharedState) -> AppResult<Release> {
    let s = settings::load(&st.pool).await?;
    let repo = resolve_repo(&s.update_repo_url).ok_or_else(|| AppError::not_found("update_off"))?;
    let req = match &repo {
        UpdateRepo::GitHub { owner, repo } => {
            let r = update_client()
                .get(format!(
                    "{}/repos/{owner}/{repo}/releases/latest",
                    github_api_base()
                ))
                .header(header::ACCEPT, "application/vnd.github+json");
            // token 只发 API host；资产走 browser_download_url，不带凭据
            match env_opt("GITHUB_TOKEN") {
                Some(t) => r.bearer_auth(t),
                None => r,
            }
        }
        UpdateRepo::Gitea { api } => update_client().get(format!("{api}/releases/latest")),
    };
    let resp = req
        .header(header::USER_AGENT, "chat-server")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("更新源不可达：{e}")))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::not_found("no_release"));
    }
    if !resp.status().is_success() {
        return Err(AppError::bad_gateway(format!("更新源返回 {}", resp.status())));
    }
    resp.json()
        .await
        .map_err(|e| AppError::bad_gateway(format!("更新源响应无法解析：{e}")))
}

fn version_of(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

#[derive(Serialize)]
pub struct AssetView {
    pub name: String,
    pub size: i64,
}

#[derive(Serialize)]
pub struct ClientLatest {
    pub version: String,
    pub notes: String,
    pub published_at: String,
    /// 下载地址：/v1/client/download/{name}
    pub assets: Vec<AssetView>,
}

/// GET /client/latest：公共端点。功能关闭 / 还没有 Release → 404
pub async fn latest(State(st): State<SharedState>) -> AppResult<Json<ClientLatest>> {
    let r = latest_release(&st).await?;
    Ok(Json(ClientLatest {
        version: version_of(&r.tag_name).to_string(),
        notes: r.body,
        published_at: r.published_at,
        assets: r
            .assets
            .into_iter()
            .map(|a| AssetView {
                name: a.name,
                size: a.size,
            })
            .collect(),
    }))
}

/// 本服务对外地址：优先后台配置的 public_url，否则按请求的 Host 拼
pub(crate) fn origin(public_url: &str, headers: &HeaderMap) -> String {
    if !public_url.is_empty() {
        return public_url.to_string();
    }
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let proto = h("x-forwarded-proto").unwrap_or("http");
    let host = h("x-forwarded-host").or(h("host")).unwrap_or("localhost");
    format!("{proto}://{host}")
}

#[derive(Deserialize)]
pub struct UpdateQuery {
    /// 插件的 {{bundle_type}}：nsis / deb / appimage
    #[serde(default)]
    bundle: String,
}

/// 按平台和安装方式挑安装包的文件名后缀；不支持的组合 → None
fn package_suffix(target: &str, arch: &str, bundle: &str) -> Option<&'static str> {
    match (target, arch, bundle) {
        ("windows", "x86_64", _) => Some("-setup.exe"),
        ("linux", "x86_64", "deb") => Some("_amd64.deb"),
        ("linux", "x86_64", _) => Some("_amd64.AppImage"),
        _ => None,
    }
}

/// GET /client/update/{target}/{arch}/{current}?bundle=：tauri-plugin-updater 的动态更新格式。
/// 安装包要带同名 .sig；没有对应安装包 → 204（无更新）。
/// 版本比较交给客户端插件（不比当前新就不装）。
pub async fn tauri_update(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path((target, arch, _current)): Path<(String, String, String)>,
    Query(q): Query<UpdateQuery>,
) -> AppResult<Response> {
    let Some(suffix) = package_suffix(&target, &arch, &q.bundle) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let r = latest_release(&st).await?;
    let Some(exe) = r.assets.iter().find(|a| a.name.ends_with(suffix)) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let sig_name = format!("{}.sig", exe.name);
    let Some(sig) = r.assets.iter().find(|a| a.name == sig_name) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    // browser_download_url 会 302 到 CDN；不带任何凭据（token 只发 API host）
    let signature = update_client()
        .get(&sig.browser_download_url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::bad_gateway(format!("取签名失败：{e}")))?
        .text()
        .await
        .map_err(|e| AppError::bad_gateway(format!("取签名失败：{e}")))?;
    let s = settings::load(&st.pool).await?;
    let base = origin(&s.public_url, &headers);
    Ok(Json(serde_json::json!({
        "version": version_of(&r.tag_name),
        "notes": r.body,
        "pub_date": r.published_at,
        "url": format!("{base}/v1/client/download/{}", exe.name),
        "signature": signature.trim(),
    }))
    .into_response())
}

/// GET /client/download/{name}：从最新 Release 流式转发一个安装包（跟 302 到 CDN，不带凭据）
pub async fn download(
    State(st): State<SharedState>,
    Path(name): Path<String>,
) -> AppResult<Response> {
    let r = latest_release(&st).await?;
    let a = r
        .assets
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| AppError::not_found("no_asset"))?;
    let resp = update_client()
        .get(&a.browser_download_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::bad_gateway(format!("下载失败：{e}")))?;
    let mut out = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", a.name.replace('"', "")),
        );
    if let Some(len) = resp.content_length() {
        out = out.header(header::CONTENT_LENGTH, len);
    }
    out.body(Body::from_stream(resp.bytes_stream()))
        .map_err(AppError::internal)
}

#[cfg(test)]
mod tests {
    use super::{gitea_api_base, parse_repo, UpdateRepo};

    #[test]
    fn repo_url_to_api() {
        assert_eq!(
            gitea_api_base("http://192.168.6.210:3000/xiefan/chat").as_deref(),
            Some("http://192.168.6.210:3000/api/v1/repos/xiefan/chat")
        );
        assert_eq!(
            gitea_api_base("https://g.example.com/git/o/r/").as_deref(),
            Some("https://g.example.com/git/api/v1/repos/o/r")
        );
        assert_eq!(gitea_api_base("http://g.example.com/onlyowner"), None);
        assert_eq!(gitea_api_base(""), None);
    }

    fn gh(v: &str) -> Option<(String, String)> {
        match parse_repo(v) {
            Some(UpdateRepo::GitHub { owner, repo }) => Some((owner, repo)),
            _ => None,
        }
    }

    #[test]
    fn parse_repo_github() {
        assert_eq!(
            gh("xie-fan/chat"),
            Some(("xie-fan".into(), "chat".into()))
        );
        assert_eq!(
            gh("https://github.com/xie-fan/chat"),
            Some(("xie-fan".into(), "chat".into()))
        );
        assert_eq!(
            gh("https://github.com/xie-fan/chat/"),
            Some(("xie-fan".into(), "chat".into()))
        );
        assert_eq!(gh("http://github.com/o/r"), Some(("o".into(), "r".into())));
    }

    #[test]
    fn parse_repo_gitea() {
        match parse_repo("http://192.168.6.210:3000/xiefan/chat") {
            Some(UpdateRepo::Gitea { api }) => assert_eq!(
                api,
                "http://192.168.6.210:3000/api/v1/repos/xiefan/chat"
            ),
            _ => panic!("expected gitea"),
        }
        match parse_repo("https://gitea.example.com/o/r") {
            Some(UpdateRepo::Gitea { api }) => {
                assert_eq!(api, "https://gitea.example.com/api/v1/repos/o/r")
            }
            _ => panic!("expected gitea"),
        }
    }

    #[test]
    fn parse_repo_off_and_invalid() {
        for bad in [
            "",
            "off",
            "justoneword",
            "a/b/c",
            "ftp://x/y",
            "ftp://h/o/r",
            "has space/r",
        ] {
            assert!(parse_repo(bad).is_none(), "{bad:?}");
        }
    }
}
