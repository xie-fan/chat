pub mod api;
pub mod auth;
pub mod backup;
pub mod client_update;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod media;
pub mod model;
pub mod push;
pub mod sessions;
pub mod settings;
pub mod state;
pub mod ws;

use axum::{response::IntoResponse, Router};
use config::Config;
use state::{AppState, SharedState};
use std::net::SocketAddr;
use axum::http::{header, HeaderValue};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

/// 构建完整路由（API + 可选静态托管）。测试复用。
pub fn build_router(state: SharedState, cfg: &Config) -> Router {
    // V2 P4-1：维护模式闸。恢复进行中除当前恢复请求外所有 /v1 请求（含 WS 升级）503。
    // 未知的 /v1 路径回 JSON 404（带 CORS）：客户端比服务端新时能说清楚是版本问题。
    // 不设这个会落到下面的静态托管兜底，POST 回一个不带 CORS 的 405，客户端只看到 Failed to fetch。
    let v1 = api::router(cfg, &state)
        .fallback(|| async { error::AppError::not_found("unknown_api") })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            backup::maintenance_gate,
        ));
    let mut app = Router::new()
        .nest("/v1", v1)
        // panic 也回 JSON 500：放在 CORS 内侧，响应穿过 CORS 层照常补头。
        // （500 不在 CF Error Pages 替换列表内，body 能透传给客户端。）
        .layer(CatchPanicLayer::custom(|err: Box<dyn std::any::Any + Send>| {
            let msg = err
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| err.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown");
            error::AppError::internal(format!("handler panic: {msg}")).into_response()
        }))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    if let Some(dist) = &cfg.app_dist {
        let index = format!("{dist}/index.html");
        let pages = ServeDir::new(dist).not_found_service(ServeFile::new(index));
        // 网页端的 CSP 与 Tauri 壳一致（tauri.conf.json）。登录凭据存在 localStorage，
        // 一旦有 XSS 就能拿走：CSP 只允许本站脚本，是最后一道闸。
        let headers = tower::ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::if_not_present(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(WEB_CSP),
            ))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ));
        app = app.fallback_service(headers.service(pages));
    }
    app
}

/// 和 app/src-tauri/tauri.conf.json 的 csp 保持一致，另加不许被别的站点嵌进 iframe（点击劫持）。
const WEB_CSP: &str = "default-src 'self'; connect-src 'self' http: https: ws: wss:; img-src 'self' data: blob: http: https:; media-src 'self' http: https: blob:; frame-src 'self' http: https:; object-src 'none'; style-src 'self' 'unsafe-inline'; script-src 'self'; base-uri 'self'; frame-ancestors 'self'";

pub async fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env();
    std::fs::create_dir_all(&cfg.files_dir).expect("create files dir failed");
    let pool = db::connect(&cfg.database_url).await;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations failed");

    let state = AppState::new(pool.clone(), cfg.clone());
    let app = build_router(state, &cfg);

    // 撤回附件定时清扫（默认每小时）：过保留期的附件行删除、无引用的物理文件清掉。
    // 首轮立即执行——停机期间过期的行不该等到下一个整点。
    {
        let files_dir = cfg.files_dir.clone();
        let ttl = std::time::Duration::from_secs(cfg.recall_attachment_ttl_secs);
        let every = std::time::Duration::from_secs(cfg.recall_sweep_interval_secs);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                match domain::sweep_recalled_attachments(&pool, &files_dir, ttl).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(swept = n, "recalled attachments swept"),
                    Err(e) => tracing::error!(error = %e, "recalled attachment sweep failed"),
                }
            }
        });
    }

    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port).parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind failed");
    tracing::info!(%addr, "chat-server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .expect("server error");
}
