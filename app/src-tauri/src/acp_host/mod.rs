mod acp_client;
mod command;
mod dirs;
mod permission;

use acp_client::{PendingPerms, PromptStartMsg};
use futures_util::{SinkExt, StreamExt};
use permission::PermissionCache;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;

pub struct AcpHostState {
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    ticket_wait: Arc<Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>>,
}

impl AcpHostState {
    pub fn new() -> Self {
        Self {
            stop_tx: Mutex::new(None),
            ticket_wait: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn ensure_rustls_ring() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn host_data_dir() -> PathBuf {
    let base = ::dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    let dir = base.join("chat-acp-host");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn install_id() -> Uuid {
    let file = host_data_dir().join("install-id");
    if let Ok(raw) = std::fs::read_to_string(&file) {
        if let Ok(id) = Uuid::parse_str(raw.trim()) {
            return id;
        }
    }
    let id = Uuid::new_v4();
    let _ = std::fs::write(&file, id.to_string());
    id
}

fn default_host_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v.trim().chars().take(60).collect();
            }
        }
    }
    if cfg!(windows) {
        "Win-Desktop".to_string()
    } else if cfg!(target_os = "macos") {
        "Mac-Desktop".to_string()
    } else {
        "Linux-Host".to_string()
    }
}

fn host_os_label() -> String {
    format!("{} · {}", std::env::consts::OS, std::env::consts::ARCH)
}

fn ws_base_url(server_url: &str, ticket: &str) -> Result<String, String> {
    let base = server_url.trim().trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_string()
    } else {
        return Err(format!("无效服务器地址：{server_url}"));
    };
    Ok(format!("{ws}/v1/ws?ticket={ticket}&role=host"))
}

enum TicketProvider {
    TauriFront {
        app: AppHandle,
        waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>>,
    },
    HeadlessToken {
        client: reqwest::Client,
        server_url: String,
        token: String,
    },
}

impl TicketProvider {
    async fn next_ticket(&self) -> Result<String, String> {
        match self {
            TicketProvider::TauriFront { app, waiters } => {
                let req_id = Uuid::new_v4().to_string();
                let (tx, rx) = oneshot::channel();
                waiters.lock().unwrap().insert(req_id.clone(), tx);
                let _ = app.emit("acp-host:need-ticket", json!({ "req_id": req_id }));
                match tokio::time::timeout(Duration::from_secs(15), rx).await {
                    Ok(Ok(res)) => res,
                    Ok(Err(_)) => Err("取票通道关闭".into()),
                    Err(_) => {
                        waiters.lock().unwrap().remove(&req_id);
                        Err("前端取票超时".into())
                    }
                }
            }
            TicketProvider::HeadlessToken {
                client,
                server_url,
                token,
            } => {
                let url = format!("{}/v1/ws/ticket", server_url.trim_end_matches('/'));
                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| format!("获取 ws-ticket 失败：{e}"))?;
                if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                    return Err("AUTH_EXPIRED".into());
                }
                if !resp.status().is_success() {
                    return Err(format!("ws-ticket 返回状态 {}", resp.status()));
                }
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("解析 ws-ticket 失败：{e}"))?;
                body["ticket"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "响应缺少 ticket".to_string())
            }
        }
    }
}

#[tauri::command]
pub async fn acp_host_start(
    app: AppHandle,
    state: State<'_, AcpHostState>,
    server_url: String,
    host_name: Option<String>,
) -> Result<(), String> {
    ensure_rustls_ring();
    if let Some(prev) = state.stop_tx.lock().unwrap().take() {
        let _ = prev.send(true);
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    *state.stop_tx.lock().unwrap() = Some(stop_tx);

    let name = host_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_host_name);
    let provider = Arc::new(TicketProvider::TauriFront {
        app: app.clone(),
        waiters: state.ticket_wait.clone(),
    });

    tauri::async_runtime::spawn(async move {
        let on_status = |online: bool, host_id: Option<String>| {
            let _ = app.emit(
                "acp-host:status",
                json!({ "online": online, "host_id": host_id }),
            );
        };
        run_host_loop(&server_url, &name, provider, stop_rx, on_status).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn acp_host_ticket(
    state: State<'_, AcpHostState>,
    req_id: String,
    ticket: Option<String>,
    error: Option<String>,
) -> Result<(), String> {
    let tx = state.ticket_wait.lock().unwrap().remove(&req_id);
    if let Some(tx) = tx {
        let res = match ticket {
            Some(t) if !t.trim().is_empty() => Ok(t),
            _ => Err(error.unwrap_or_else(|| "无法获取 WS 票据".into())),
        };
        let _ = tx.send(res);
    }
    Ok(())
}

#[tauri::command]
pub async fn acp_host_stop(state: State<'_, AcpHostState>) -> Result<(), String> {
    if let Some(prev) = state.stop_tx.lock().unwrap().take() {
        let _ = prev.send(true);
    }
    state.ticket_wait.lock().unwrap().clear();
    Ok(())
}

async fn run_host_loop(
    server_url: &str,
    host_name: &str,
    provider: Arc<TicketProvider>,
    mut stop_rx: watch::Receiver<bool>,
    on_status: impl Fn(bool, Option<String>) + Send + Sync,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if *stop_rx.borrow() {
            break;
        }
        match provider.next_ticket().await {
            Ok(ticket) => {
                match ws_base_url(server_url, &ticket) {
                    Ok(ws_url) => {
                        let permanent_stop =
                            run_one_connection(&ws_url, host_name, &mut stop_rx, &on_status).await;
                        on_status(false, None);
                        if permanent_stop {
                            break;
                        }
                        backoff = Duration::from_secs(1);
                    }
                    Err(_) => break,
                }
            }
            Err(e) if e == "AUTH_EXPIRED" => {
                on_status(false, None);
                break;
            }
            Err(_) => {}
        }

        tokio::select! {
            _ = tokio::time::sleep(backoff) => {
                backoff = (backoff * 2).min(Duration::from_secs(20));
            }
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }
        }
    }
}

/// 返回 true 表示收到 force_logout / stop，应永久停止重连。
async fn run_one_connection(
    ws_url: &str,
    host_name: &str,
    stop_rx: &mut watch::Receiver<bool>,
    on_status: impl Fn(bool, Option<String>) + Send + Sync,
) -> bool {
    let Ok((ws_stream, _)) = tokio_tungstenite::connect_async(ws_url).await else {
        return false;
    };
    let (mut sink, mut stream) = ws_stream.split();

    let hello = json!({
        "type": "host_hello",
        "install_id": install_id(),
        "name": host_name,
        "os": host_os_label(),
        "ip": "Client-Daemon",
        "roots": dirs::roots(),
    })
    .to_string();
    if sink.send(WsMsg::Text(hello.into())).await.is_err() {
        return false;
    }

    let (out_tx, mut out_rx) = mpsc::channel::<Value>(128);
    let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
    let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
    let running_cancels: Arc<Mutex<HashMap<String, watch::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let mut permanent_stop = false;
    loop {
        if *stop_rx.borrow() {
            permanent_stop = true;
            break;
        }
        tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    permanent_stop = true;
                    break;
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(v) => {
                        if sink.send(WsMsg::Text(v.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(WsMsg::Text(txt))) => {
                        let Ok(v) = serde_json::from_str::<Value>(&txt) else {
                            continue;
                        };
                        match v["type"].as_str() {
                            Some("host_ready") => {
                                let hid = v["host_id"].as_str().map(str::to_string);
                                on_status(true, hid);
                            }
                            Some("force_logout") => {
                                permanent_stop = true;
                                break;
                            }
                            Some("host_superseded") => {
                                permanent_stop = true;
                                break;
                            }
                            Some("rpc_call") => {
                                let tx = out_tx.clone();
                                tokio::spawn(async move {
                                    handle_rpc_call(v, tx).await;
                                });
                            }
                            Some("prompt_start") => {
                                if let Ok(msg) = serde_json::from_value::<PromptStartMsg>(v) {
                                    let sid = msg.session_id.clone();
                                    let (ctx, crx) = watch::channel(false);
                                    if let Some(old) = running_cancels.lock().unwrap().insert(sid.clone(), ctx) {
                                        let _ = old.send(true);
                                    }
                                    let tx = out_tx.clone();
                                    let perms = pending_perms.clone();
                                    let cache = perm_cache.clone();
                                    let cancels = running_cancels.clone();
                                    tokio::spawn(async move {
                                        acp_client::run_prompt_task(msg, tx, perms, cache, crx).await;
                                        cancels.lock().unwrap().remove(&sid);
                                    });
                                }
                            }
                            Some("prompt_cancel") => {
                                if let Some(sid) = v["session_id"].as_str() {
                                    if let Some(ctx) = running_cancels.lock().unwrap().get(sid) {
                                        let _ = ctx.send(true);
                                    }
                                }
                            }
                            Some("permission_reply") => {
                                if let Some(req_id) = v["req_id"].as_str() {
                                    if let Some(opt) = v["option_id"].as_str() {
                                        if let Some(tx) = pending_perms.lock().unwrap().remove(req_id) {
                                            let _ = tx.send(opt.to_string());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(WsMsg::Ping(d))) => {
                        if sink.send(WsMsg::Pong(d)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMsg::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }

    // 取消仍在执行的子任务
    for (_, tx) in running_cancels.lock().unwrap().drain() {
        let _ = tx.send(true);
    }
    permanent_stop
}

async fn handle_rpc_call(v: Value, out_tx: mpsc::Sender<Value>) {
    let req_id = v["req_id"].as_str().unwrap_or("").to_string();
    let method = v["method"].as_str().unwrap_or("");
    let params = &v["params"];

    let res: Result<Value, String> = match method {
        "list_dir" => dirs::list_dir(params["path"].as_str().unwrap_or("")),
        "probe_workspace" => command::probe_workspace(params["cwd"].as_str().unwrap_or("")).await,
        "git_checkout" => {
            command::git_checkout(
                params["cwd"].as_str().unwrap_or(""),
                params["branch"].as_str().unwrap_or(""),
            )
            .await
        }
        "probe_agent" => Ok(command::probe_agent(params["cmd"].as_str().unwrap_or(""))),
        other => Err(format!("unknown rpc method: {other}")),
    };

    let frame = match res {
        Ok(data) => json!({ "type": "rpc_result", "req_id": req_id, "ok": true, "data": data }),
        Err(err) => json!({ "type": "rpc_result", "req_id": req_id, "ok": false, "error": err }),
    };
    let _ = out_tx.send(frame).await;
}

/// 供远端无头服务器（如 Ubuntu / GPU 节点）以 `chat-app --acp-host --server <URL> --token <JWT>` 运行。
pub fn maybe_run_cli() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--acp-host") {
        return false;
    }

    ensure_rustls_ring();
    let mut server_url = std::env::var("CHAT_SERVER_URL").unwrap_or_default();
    let mut token = std::env::var("CHAT_ACCESS_TOKEN").unwrap_or_default();
    let mut host_name = std::env::var("CHAT_HOST_NAME").unwrap_or_else(|_| default_host_name());

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--server" if i + 1 < args.len() => {
                server_url = args[i + 1].clone();
                i += 1;
            }
            "--token" if i + 1 < args.len() => {
                token = args[i + 1].clone();
                i += 1;
            }
            "--name" if i + 1 < args.len() => {
                host_name = args[i + 1].clone();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    if server_url.is_empty() || token.is_empty() {
        eprintln!("Usage: --acp-host --server <URL> --token <JWT> [--name <HOST_NAME>]");
        std::process::exit(2);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let client = reqwest::Client::new();
        let provider = Arc::new(TicketProvider::HeadlessToken {
            client,
            server_url: server_url.clone(),
            token,
        });
        let (_stop_tx, stop_rx) = watch::channel(false);
        run_host_loop(&server_url, &host_name, provider, stop_rx, |online, hid| {
            eprintln!("[acp-host] online={online} host_id={hid:?}");
        })
        .await;
    });
    true
}
