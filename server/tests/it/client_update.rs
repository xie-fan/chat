use super::*;
use axum::{
    extract::{Path as AxPath, State as AxState},
    http::{header, HeaderMap as AxHeaderMap, StatusCode as AxStatus},
    response::{IntoResponse, Response as AxResponse},
    routing::get,
    Json as AxJson, Router as AxRouter,
};
use std::sync::{Arc, Mutex as StdMutex};

// 环境变量是进程级的：碰 CLIENT_UPDATE_REPO / GITHUB_* 的测试必须串行
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 设置并保存环境变量，Drop 时还原
struct EnvRestore(Vec<(&'static str, Option<String>)>);

impl EnvRestore {
    fn new(vars: &[(&'static str, Option<&str>)]) -> Self {
        let saved = vars.iter().map(|(k, _)| (*k, std::env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn auth_of(h: &AxHeaderMap) -> String {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[derive(Clone)]
struct MockApi {
    api_base: String,
    cdn_base: String,
    have_release: bool,
    auth: Arc<StdMutex<Vec<String>>>,
    version: Arc<StdMutex<&'static str>>,
}

struct GhMock {
    api_base: String,
    api_auth: Arc<StdMutex<Vec<String>>>,
    cdn_auth: Arc<StdMutex<Vec<String>>>,
    version: Arc<StdMutex<&'static str>>,
}

async fn gh_latest(AxState(m): AxState<MockApi>, h: AxHeaderMap) -> AxResponse {
    m.auth.lock().unwrap().push(auth_of(&h));
    if !m.have_release {
        return AxStatus::NOT_FOUND.into_response();
    }
    let version = *m.version.lock().unwrap();
    let asset = |name: &str, size: i64| {
        let name = name.replace("9.9.9", version);
        json!({
            "name": name,
            "size": size,
            "browser_download_url": format!("{}/dl/{name}", m.api_base),
        })
    };
    AxJson(json!({
        "tag_name": format!("v{version}"),
        "body": "更新说明 v9",
        "published_at": "2026-01-02T03:04:05Z",
        "assets": [
            asset("Chat_9.9.9_x64-setup.exe", 5),
            asset("Chat_9.9.9_x64-setup.exe.sig", 13),
            asset("Chat_9.9.9_amd64.deb", 5),
            asset("Chat_9.9.9_amd64.deb.sig", 13),
            asset("Chat_9.9.9_amd64.AppImage", 5),
            asset("Chat_9.9.9_amd64.AppImage.sig", 13),
            asset("Chat_9.9.9_arm64.apk", 5),
        ],
    }))
    .into_response()
}

/// GitHub browser_download_url 的行为：302 到另一个 host（对象存储 CDN）
async fn gh_dl(AxState(m): AxState<MockApi>, AxPath(name): AxPath<String>) -> AxResponse {
    (
        AxStatus::FOUND,
        [(header::LOCATION, format!("{}/f/{name}", m.cdn_base))],
    )
        .into_response()
}

async fn cdn_file(
    AxState(auth): AxState<Arc<StdMutex<Vec<String>>>>,
    AxPath(name): AxPath<String>,
    h: AxHeaderMap,
) -> Vec<u8> {
    auth.lock().unwrap().push(auth_of(&h));
    if name.ends_with(".sig") {
        b"sig-bytes-xyz".to_vec()
    } else {
        vec![1, 2, 3, 4, 5]
    }
}

/// 起两个 axum：一个扮 GitHub/Gitea API（/repos/... 与 /api/v1/repos/... 两条路径都接），
/// 一个扮 CDN。资产的 browser_download_url 指向 API 的 /dl/{name}，由它 302 到 CDN。
async fn gh_mock(have_release: bool) -> GhMock {
    let api_auth = Arc::new(StdMutex::new(Vec::new()));
    let cdn_auth = Arc::new(StdMutex::new(Vec::new()));
    let version = Arc::new(StdMutex::new("9.9.9"));

    let cdn_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdn_base = format!("http://{}", cdn_listener.local_addr().unwrap());
    let cdn_app = AxRouter::new()
        .route("/f/{name}", get(cdn_file))
        .with_state(cdn_auth.clone());
    tokio::spawn(async move { axum::serve(cdn_listener, cdn_app).await.unwrap() });

    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_base = format!("http://{}", api_listener.local_addr().unwrap());
    let st = MockApi {
        api_base: api_base.clone(),
        cdn_base,
        have_release,
        auth: api_auth.clone(),
        version: version.clone(),
    };
    let api_app = AxRouter::new()
        .route("/repos/{o}/{r}/releases/latest", get(gh_latest))
        .route("/api/v1/repos/{o}/{r}/releases/latest", get(gh_latest))
        .route("/dl/{name}", get(gh_dl))
        .with_state(st);
    tokio::spawn(async move { axum::serve(api_listener, api_app).await.unwrap() });

    GhMock {
        api_base,
        api_auth,
        cdn_auth,
        version,
    }
}

/// 直接写 server_settings（管理员校验走另一个测试）
async fn set_repo(pool: &sqlx::PgPool, v: &str) {
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('update_repo_url', to_jsonb($1::text))
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(v)
    .execute(pool)
    .await
    .unwrap();
}

/// 直接写 server_settings.public_url。代码默认已是公网地址，要测
/// 「显式空 = 按请求 Host 拼下载地址」的用例需先置空。
async fn set_public_url(pool: &sqlx::PgPool, v: &str) {
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('public_url', to_jsonb($1::text))
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(v)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn github_release_flow() {
    let _lock = ENV_LOCK.lock().await;
    let mock = gh_mock(true).await;
    let _env = EnvRestore::new(&[
        ("GITHUB_API_BASE", Some(mock.api_base.as_str())),
        ("GITHUB_TOKEN", Some("tok-test")),
        ("CLIENT_UPDATE_REPO", None),
        ("CLIENT_UPDATE_PROXY", None),
    ]);
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_cu_{}", uniq())).await;
    set_repo(&st.pool, "octo/chat").await;
    // public_url 默认是公网地址；本用例断言下载 url 按请求 Host 拼，须显式置空
    set_public_url(&st.pool, "").await;

    // /client/latest：GitHub release 字段映射
    let r: Value = http()
        .get(format!("{base}/v1/client/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["version"], "9.9.9");
    assert_eq!(r["notes"], "更新说明 v9");
    assert_eq!(r["published_at"], "2026-01-02T03:04:05Z");
    let names: Vec<&str> = r["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Chat_9.9.9_x64-setup.exe"));
    *mock.version.lock().unwrap() = "9.9.10";

    // /client/update：签名经 /dl → 302 → CDN 取回
    let r: Value = http()
        .get(format!("{base}/v1/client/update/windows/x86_64/0.0.1?bundle=nsis"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["version"], "9.9.9");
    assert_eq!(r["notes"], "更新说明 v9");
    assert_eq!(r["pub_date"], "2026-01-02T03:04:05Z");
    assert_eq!(r["signature"], "sig-bytes-xyz");
    assert_eq!(
        r["url"].as_str().unwrap(),
        format!("{base}/v1/client/download/Chat_9.9.9_x64-setup.exe")
    );

    // /client/download：跨 host 302 流式回源
    let resp = http()
        .get(format!(
            "{base}/v1/client/download/Chat_9.9.9_x64-setup.exe"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), &[1, 2, 3, 4, 5]);

    // token 只到 API；版本检查、更新和下载复用同一份 Release。
    let api_auth = mock.api_auth.lock().unwrap();
    assert_eq!(api_auth.len(), 1);
    assert!(api_auth.iter().all(|a| a == "Bearer tok-test"));
    let cdn_auth = mock.cdn_auth.lock().unwrap();
    assert_eq!(cdn_auth.len(), 2);
    assert!(cdn_auth.iter().all(|a| a.is_empty()));
}

#[tokio::test]
async fn github_no_release_is_404() {
    let _lock = ENV_LOCK.lock().await;
    let mock = gh_mock(false).await;
    let _env = EnvRestore::new(&[
        ("GITHUB_API_BASE", Some(mock.api_base.as_str())),
        ("GITHUB_TOKEN", None),
        ("CLIENT_UPDATE_REPO", None),
        ("CLIENT_UPDATE_PROXY", None),
    ]);
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_cu_{}", uniq())).await;
    set_repo(&st.pool, "octo/chat").await;
    let resp = http()
        .get(format!("{base}/v1/client/latest"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = http()
        .get(format!("{base}/v1/client/update/windows/x86_64/0.0.1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn gitea_release_still_works() {
    let _lock = ENV_LOCK.lock().await;
    let mock = gh_mock(true).await;
    let _env = EnvRestore::new(&[
        ("GITHUB_API_BASE", None),
        ("GITHUB_TOKEN", None),
        ("CLIENT_UPDATE_REPO", None),
        ("CLIENT_UPDATE_PROXY", None),
    ]);
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_cu_{}", uniq())).await;
    // 非 github.com 的 http(s) 地址 → Gitea 兼容模式
    set_repo(&st.pool, &format!("{}/octo/chat", mock.api_base)).await;
    let r: Value = http()
        .get(format!("{base}/v1/client/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["version"], "9.9.9");
    let r: Value = http()
        .get(format!("{base}/v1/client/update/linux/x86_64/0.0.1?bundle=deb"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["signature"], "sig-bytes-xyz");
    // Gitea 模式不发 Authorization
    assert!(mock
        .api_auth
        .lock()
        .unwrap()
        .iter()
        .all(|a| a.is_empty()));
}

#[tokio::test]
async fn env_fallback_and_off() {
    let _lock = ENV_LOCK.lock().await;
    let mock = gh_mock(true).await;
    let (_guard, base, st) = spawn_named_state(&format!("chat_t_cu_{}", uniq())).await;
    {
        // DB 为空 → 用 CLIENT_UPDATE_REPO
        let _env = EnvRestore::new(&[
            ("GITHUB_API_BASE", Some(mock.api_base.as_str())),
            ("GITHUB_TOKEN", None),
            ("CLIENT_UPDATE_REPO", Some("octo/chat")),
            ("CLIENT_UPDATE_PROXY", None),
        ]);
        let r: Value = http()
            .get(format!("{base}/v1/client/latest"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r["version"], "9.9.9");
    }
    {
        // CLIENT_UPDATE_REPO=off → 关闭
        let _env = EnvRestore::new(&[
            ("GITHUB_API_BASE", Some(mock.api_base.as_str())),
            ("CLIENT_UPDATE_REPO", Some("off")),
            ("CLIENT_UPDATE_PROXY", None),
        ]);
        let resp = http()
            .get(format!("{base}/v1/client/latest"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        // DB 里的 off 同样关闭（优先于 env）
        set_repo(&st.pool, "off").await;
        let resp = http()
            .get(format!("{base}/v1/client/latest"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }
}

#[tokio::test]
async fn update_repo_url_validation() {
    let (_guard, base) = spawn_named_db(&format!("chat_t_cu_{}", uniq())).await;
    let a = register(&base, &format!("ad_{}", uniq())).await; // 首个用户 = admin
    for ok in [
        "xie-fan/chat",
        "https://github.com/xie-fan/chat",
        "https://github.com/xie-fan/chat/",
        "off",
        "http://192.168.6.210:3000/o/r",
        "",
    ] {
        let r = admin_put_settings(&base, &a, json!({"update_repo_url": ok})).await;
        assert_eq!(r.status(), 200, "应接受 {ok:?}");
    }
    for bad in ["justoneword", "a/b/c", "ftp://x/y", "ftp://h/o/r", "has space/r"] {
        let r = admin_put_settings(&base, &a, json!({"update_repo_url": bad})).await;
        assert_eq!(r.status(), 400, "应拒绝 {bad:?}");
    }
}
