mod acp_client;
mod command;
mod dirs;
mod permission;

use acp_client::{PendingPerms, PromptStartMsg};
use futures_util::{SinkExt, StreamExt};
use permission::PermissionCache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustStore {
    #[serde(default)]
    allow_all_local: bool,
    #[serde(default)]
    approved: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeadlessAuthStore {
    server_url: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalPolicyView {
    pub allow_all_local: bool,
    pub approved_count: usize,
}

type ExecConfirmWait = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

#[derive(Clone)]
enum ExecGate {
    Desktop {
        app: AppHandle,
        waiters: ExecConfirmWait,
    },
    Headless {
        trust_agents: bool,
        allow_cwds: Vec<PathBuf>,
    },
}

pub struct AcpHostState {
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    ticket_wait: Arc<Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>>,
    exec_confirm_wait: ExecConfirmWait,
    trust: Arc<Mutex<TrustStore>>,
}

impl AcpHostState {
    pub fn new() -> Self {
        Self {
            stop_tx: Mutex::new(None),
            ticket_wait: Arc::new(Mutex::new(HashMap::new())),
            exec_confirm_wait: Arc::new(Mutex::new(HashMap::new())),
            trust: Arc::new(Mutex::new(load_trust_store())),
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::write(path, bytes).map_err(|e| format!("写入文件失败：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn trust_store_path() -> PathBuf {
    host_data_dir().join("trust.json")
}

fn load_trust_store() -> TrustStore {
    let path = trust_store_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<TrustStore>(&s).ok())
        .unwrap_or_default()
}

fn save_trust_store(store: &TrustStore) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(store).map_err(|e| format!("序列化信任策略失败：{e}"))?;
    write_private_file(&trust_store_path(), &data)
}

fn headless_auth_path() -> PathBuf {
    host_data_dir().join("host-auth.json")
}

fn load_headless_auth() -> Option<HeadlessAuthStore> {
    std::fs::read_to_string(headless_auth_path())
        .ok()
        .and_then(|s| serde_json::from_str::<HeadlessAuthStore>(&s).ok())
}

fn save_headless_auth(store: &HeadlessAuthStore) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(store).map_err(|e| format!("序列化凭据失败：{e}"))?;
    write_private_file(&headless_auth_path(), &data)
}

fn exec_fingerprint(canon_cwd: &Path, cmd: &str, env: &BTreeMap<String, String>) -> String {
    json!({
        "cwd": canon_cwd.to_string_lossy(),
        "cmd": cmd,
        "env": env,
    })
    .to_string()
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

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn http_base_url(server_url: &str) -> Result<Url, String> {
    let mut url =
        Url::parse(server_url.trim()).map_err(|_| format!("无效服务器地址：{server_url}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("无效服务器地址（缺少主机）：{server_url}"))?;
    let target_scheme = match url.scheme() {
        "https" | "wss" => "https",
        "http" | "ws" if is_loopback_host(host) => "http",
        "http" | "ws" => return Err("非本地服务器必须使用 HTTPS 连接".into()),
        other => return Err(format!("不支持的服务器协议：{other}")),
    };
    url.set_scheme(target_scheme)
        .map_err(|_| format!("无法设置协议：{server_url}"))?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn endpoint_url(base: &Url, suffix: &str) -> String {
    let mut u = base.clone();
    let prefix = u.path().trim_end_matches('/');
    let path = suffix.trim_start_matches('/');
    u.set_path(&format!("{prefix}/{path}"));
    u.to_string()
}

fn ws_base_url(server_url: &str, ticket: &str) -> Result<String, String> {
    let mut url =
        Url::parse(server_url.trim()).map_err(|_| format!("无效服务器地址：{server_url}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("无效服务器地址（缺少主机）：{server_url}"))?;
    let target_scheme = match url.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" if is_loopback_host(host) => "ws",
        "http" | "ws" => return Err("非本地服务器必须使用 WSS / HTTPS 加密连接".into()),
        other => return Err(format!("不支持的服务器协议：{other}")),
    };
    url.set_scheme(target_scheme)
        .map_err(|_| format!("无法设置协议：{server_url}"))?;
    let prefix = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{prefix}/v1/ws"));
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("ticket", ticket)
        .append_pair("role", "host");
    Ok(url.to_string())
}

enum TicketProvider {
    TauriFront {
        app: AppHandle,
        waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>>,
    },
    HeadlessToken {
        client: reqwest::Client,
        base_url: Url,
        token: String,
    },
    HeadlessPair {
        client: reqwest::Client,
        base_url: Url,
        auth: Arc<AsyncMutex<HeadlessAuthStore>>,
    },
}

impl TicketProvider {
    async fn fetch_ticket_with_token(
        client: &reqwest::Client,
        base_url: &Url,
        token: &str,
    ) -> Result<String, String> {
        let url = endpoint_url(base_url, "/v1/ws/ticket");
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
                base_url,
                token,
            } => Self::fetch_ticket_with_token(client, base_url, token).await,
            TicketProvider::HeadlessPair {
                client,
                base_url,
                auth,
            } => {
                let access = { auth.lock().await.access_token.clone() };
                match Self::fetch_ticket_with_token(client, base_url, &access).await {
                    Err(e) if e == "AUTH_EXPIRED" => {
                        let mut guard = auth.lock().await;
                        let refresh_url = endpoint_url(base_url, "/v1/auth/refresh");
                        let resp = client
                            .post(&refresh_url)
                            .json(&json!({ "refresh_token": guard.refresh_token }))
                            .send()
                            .await
                            .map_err(|e| format!("刷新凭据失败：{e}"))?;
                        if matches!(
                            resp.status(),
                            reqwest::StatusCode::UNAUTHORIZED
                                | reqwest::StatusCode::FORBIDDEN
                                | reqwest::StatusCode::BAD_REQUEST
                        ) {
                            return Err("AUTH_EXPIRED".into());
                        }
                        if !resp.status().is_success() {
                            return Err(format!("刷新凭据返回状态 {}", resp.status()));
                        }
                        let body: Value = resp
                            .json()
                            .await
                            .map_err(|e| format!("解析刷新响应失败：{e}"))?;
                        let new_access = body["access_token"]
                            .as_str()
                            .ok_or_else(|| "刷新响应缺少 access_token".to_string())?
                            .to_string();
                        let new_refresh = body["refresh_token"]
                            .as_str()
                            .ok_or_else(|| "刷新响应缺少 refresh_token".to_string())?
                            .to_string();
                        guard.access_token = new_access.clone();
                        guard.refresh_token = new_refresh;
                        let _ = save_headless_auth(&guard);
                        drop(guard);
                        Self::fetch_ticket_with_token(client, base_url, &new_access).await
                    }
                    other => other,
                }
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
    let trust = state.trust.clone();
    let gate = ExecGate::Desktop {
        app: app.clone(),
        waiters: state.exec_confirm_wait.clone(),
    };

    tauri::async_runtime::spawn(async move {
        let on_status = |online: bool, host_id: Option<String>| {
            let _ = app.emit(
                "acp-host:status",
                json!({ "online": online, "host_id": host_id }),
            );
        };
        run_host_loop(&server_url, &name, provider, trust, gate, stop_rx, on_status).await;
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
pub async fn acp_host_confirm_exec(
    state: State<'_, AcpHostState>,
    req_id: String,
    allow: bool,
) -> Result<(), String> {
    if let Some(tx) = state.exec_confirm_wait.lock().unwrap().remove(&req_id) {
        let _ = tx.send(allow);
    }
    Ok(())
}

#[tauri::command]
pub async fn acp_host_local_policy(
    state: State<'_, AcpHostState>,
) -> Result<LocalPolicyView, String> {
    let guard = state.trust.lock().unwrap();
    Ok(LocalPolicyView {
        allow_all_local: guard.allow_all_local,
        approved_count: guard.approved.len(),
    })
}

#[tauri::command]
pub async fn acp_host_set_local_allow_all(
    state: State<'_, AcpHostState>,
    enabled: bool,
) -> Result<LocalPolicyView, String> {
    let mut guard = state.trust.lock().unwrap();
    guard.allow_all_local = enabled;
    save_trust_store(&guard)?;
    Ok(LocalPolicyView {
        allow_all_local: guard.allow_all_local,
        approved_count: guard.approved.len(),
    })
}

#[tauri::command]
pub async fn acp_host_stop(state: State<'_, AcpHostState>) -> Result<(), String> {
    if let Some(prev) = state.stop_tx.lock().unwrap().take() {
        let _ = prev.send(true);
    }
    state.ticket_wait.lock().unwrap().clear();
    state.exec_confirm_wait.lock().unwrap().clear();
    Ok(())
}

async fn run_host_loop(
    server_url: &str,
    host_name: &str,
    provider: Arc<TicketProvider>,
    trust: Arc<Mutex<TrustStore>>,
    exec_gate: ExecGate,
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
                        let permanent_stop = run_one_connection(
                            &ws_url,
                            host_name,
                            trust.clone(),
                            exec_gate.clone(),
                            &mut stop_rx,
                            &on_status,
                        )
                        .await;
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
    trust: Arc<Mutex<TrustStore>>,
    exec_gate: ExecGate,
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
                                let turn_id_val = v["turn_id"].clone();
                                if let Ok(mut msg) = serde_json::from_value::<PromptStartMsg>(v) {
                                    let sid = msg.session_id.clone();
                                    let (ctx, mut crx) = watch::channel(false);
                                    if let Some(old) =
                                        running_cancels.lock().unwrap().insert(sid.clone(), ctx)
                                    {
                                        let _ = old.send(true);
                                    }
                                    let tx = out_tx.clone();
                                    let perms = pending_perms.clone();
                                    let cache = perm_cache.clone();
                                    let cancels = running_cancels.clone();
                                    let trust_clone = trust.clone();
                                    let gate_clone = exec_gate.clone();
                                    tokio::spawn(async move {
                                        let gate_res = authorize_prompt_exec(
                                            &mut msg,
                                            &trust_clone,
                                            &gate_clone,
                                            &mut crx,
                                        )
                                        .await;
                                        match gate_res {
                                            Ok(()) => {
                                                acp_client::run_prompt_task(
                                                    msg, tx, perms, cache, crx,
                                                )
                                                .await;
                                            }
                                            Err(err_msg) => {
                                                let _ = tx
                                                    .send(json!({
                                                        "type": "prompt_done",
                                                        "session_id": msg.session_id,
                                                        "turn_id": turn_id_val,
                                                        "status": "error",
                                                        "stop_reason": "refused",
                                                        "error": err_msg,
                                                    }))
                                                    .await;
                                            }
                                        }
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

async fn authorize_prompt_exec(
    msg: &mut PromptStartMsg,
    trust: &Arc<Mutex<TrustStore>>,
    exec_gate: &ExecGate,
    crx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let raw_cwd = Path::new(&msg.cwd);
    let canon_cwd = permission::resolve_path(raw_cwd)
        .ok_or_else(|| format!("工作区路径无效：{}", msg.cwd))?;
    if !canon_cwd.exists() || !canon_cwd.is_dir() {
        return Err(format!("工作区目录不存在：{}", msg.cwd));
    }
    if dirs::is_sensitive_path(&canon_cwd) {
        return Err("拒绝访问敏感系统目录".into());
    }

    if let ExecGate::Headless { allow_cwds, .. } = exec_gate {
        if !allow_cwds.is_empty() {
            let permitted = allow_cwds.iter().any(|allowed| {
                permission::resolve_path(allowed)
                    .map(|canon_allowed| canon_cwd.starts_with(&canon_allowed))
                    .unwrap_or(false)
            });
            if !permitted {
                return Err(format!(
                    "工作区目录 {} 未在 --allow-cwd 允许范围内",
                    canon_cwd.display()
                ));
            }
        }
    }

    let sorted_env: BTreeMap<String, String> = msg
        .agent
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let fp = exec_fingerprint(&canon_cwd, &msg.agent.cmd, &sorted_env);

    let (allow_all_local, already_approved) = {
        let guard = trust.lock().unwrap();
        (guard.allow_all_local, guard.approved.contains(&fp))
    };

    if !allow_all_local && msg.permission_policy == "allow_all" {
        msg.permission_policy = "auto_read".to_string();
    }

    if already_approved {
        return Ok(());
    }

    match exec_gate {
        ExecGate::Headless { trust_agents, .. } => {
            if *trust_agents {
                Ok(())
            } else {
                Err("无头宿主尚未信任此 Agent 启动配置，请显式传入 --trust-agents 后重试".into())
            }
        }
        ExecGate::Desktop { app, waiters } => {
            let req_id = Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel();
            waiters.lock().unwrap().insert(req_id.clone(), tx);
            let env_keys: Vec<String> = sorted_env.keys().cloned().collect();
            let _ = app.emit(
                "acp-host:confirm-exec",
                json!({
                    "req_id": req_id,
                    "session_id": msg.session_id,
                    "cwd": canon_cwd.to_string_lossy(),
                    "cmd": msg.agent.cmd,
                    "env_keys": env_keys,
                }),
            );
            let decision = tokio::select! {
                res = tokio::time::timeout(Duration::from_secs(60), rx) => {
                    match res {
                        Ok(Ok(allow)) => allow,
                        _ => false,
                    }
                }
                _ = crx.changed() => false,
            };
            waiters.lock().unwrap().remove(&req_id);
            if !decision {
                return Err("本地安全确认已拒绝或超时，未执行 Agent".into());
            }
            let mut guard = trust.lock().unwrap();
            guard.approved.insert(fp);
            let _ = save_trust_store(&guard);
            Ok(())
        }
    }
}

/// 供远端无头服务器（如 Ubuntu / GPU 节点）以 `chat-app --acp-host --server <URL> --pair <CODE>` 运行。
pub fn maybe_run_cli() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--acp-host") {
        return false;
    }

    ensure_rustls_ring();
    let mut server_url = std::env::var("CHAT_SERVER_URL").unwrap_or_default();
    let mut token = std::env::var("CHAT_ACCESS_TOKEN").unwrap_or_default();
    let mut pair_code = std::env::var("CHAT_PAIR_CODE").unwrap_or_default();
    let mut host_name = std::env::var("CHAT_HOST_NAME").unwrap_or_else(|_| default_host_name());
    let mut trust_agents = false;
    let mut allow_cwds: Vec<PathBuf> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--server" if i + 1 < args.len() => {
                server_url = args[i + 1].clone();
                i += 1;
            }
            "--pair" if i + 1 < args.len() => {
                pair_code = args[i + 1].clone();
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
            "--trust-agents" => {
                trust_agents = true;
            }
            "--allow-cwd" if i + 1 < args.len() => {
                allow_cwds.push(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let client = reqwest::Client::new();
        let trust = Arc::new(Mutex::new(load_trust_store()));
        let exec_gate = ExecGate::Headless {
            trust_agents,
            allow_cwds,
        };

        let (resolved_server, provider): (String, Arc<TicketProvider>) = if !pair_code.is_empty() {
            if server_url.is_empty() {
                eprintln!("Usage: --acp-host --server <URL> --pair <CODE> [--name <HOST_NAME>] [--trust-agents] [--allow-cwd <DIR>]");
                std::process::exit(2);
            }
            let base_url = match http_base_url(&server_url) {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("[acp-host] {e}");
                    std::process::exit(2);
                }
            };
            let pair_url = endpoint_url(&base_url, "/v1/acp/hosts/pair");
            let resp = match client
                .post(&pair_url)
                .json(&json!({ "code": pair_code, "host_name": host_name }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[acp-host] 配对请求失败：{e}");
                    std::process::exit(2);
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                eprintln!("[acp-host] 配对失败（{status}）：{txt}");
                std::process::exit(2);
            }
            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[acp-host] 解析配对响应失败：{e}");
                    std::process::exit(2);
                }
            };
            let Some(access_token) = body["access_token"].as_str().map(str::to_string) else {
                eprintln!("[acp-host] 配对响应缺少 access_token");
                std::process::exit(2);
            };
            let Some(refresh_token) = body["refresh_token"].as_str().map(str::to_string) else {
                eprintln!("[acp-host] 配对响应缺少 refresh_token");
                std::process::exit(2);
            };
            let store = HeadlessAuthStore {
                server_url: server_url.clone(),
                access_token,
                refresh_token,
            };
            if let Err(e) = save_headless_auth(&store) {
                eprintln!("[acp-host] 保存本地凭据失败：{e}");
            }
            (
                server_url.clone(),
                Arc::new(TicketProvider::HeadlessPair {
                    client,
                    base_url,
                    auth: Arc::new(AsyncMutex::new(store)),
                }),
            )
        } else if !token.is_empty() {
            if server_url.is_empty() {
                eprintln!("Usage: --acp-host --server <URL> --pair <CODE> [--name <HOST_NAME>] [--trust-agents] [--allow-cwd <DIR>]");
                std::process::exit(2);
            }
            let base_url = match http_base_url(&server_url) {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("[acp-host] {e}");
                    std::process::exit(2);
                }
            };
            (
                server_url.clone(),
                Arc::new(TicketProvider::HeadlessToken {
                    client,
                    base_url,
                    token,
                }),
            )
        } else if let Some(stored) = load_headless_auth() {
            let norm = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
            if !server_url.is_empty() && norm(&stored.server_url) != norm(&server_url) {
                eprintln!(
                    "[acp-host] 已保存凭据属于 {}，与 --server {} 不一致；请传入新的 --pair <CODE>",
                    stored.server_url, server_url
                );
                std::process::exit(2);
            }
            let target_server = if server_url.is_empty() {
                stored.server_url.clone()
            } else {
                server_url.clone()
            };
            let base_url = match http_base_url(&target_server) {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("[acp-host] {e}");
                    std::process::exit(2);
                }
            };
            (
                target_server,
                Arc::new(TicketProvider::HeadlessPair {
                    client,
                    base_url,
                    auth: Arc::new(AsyncMutex::new(stored)),
                }),
            )
        } else {
            eprintln!("Usage: --acp-host --server <URL> --pair <CODE> [--name <HOST_NAME>] [--trust-agents] [--allow-cwd <DIR>]");
            std::process::exit(2);
        };

        let (_stop_tx, stop_rx) = watch::channel(false);
        run_host_loop(
            &resolved_server,
            &host_name,
            provider,
            trust,
            exec_gate,
            stop_rx,
            |online, hid| {
                eprintln!("[acp-host] online={online} host_id={hid:?}");
            },
        )
        .await;
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_base_url_enforces_wss_on_non_loopback_and_encodes_ticket() {
        assert!(ws_base_url("http://example.com", "abc").is_err());
        assert!(ws_base_url("ws://192.168.1.10:3000", "abc").is_err());

        let local = ws_base_url("http://127.0.0.1:3000/", "t+1/2=3&x").unwrap();
        assert_eq!(
            local,
            "ws://127.0.0.1:3000/v1/ws?ticket=t%2B1%2F2%3D3%26x&role=host"
        );

        let remote = ws_base_url("https://chat.example.com/app/", "tok#1").unwrap();
        assert_eq!(
            remote,
            "wss://chat.example.com/app/v1/ws?ticket=tok%231&role=host"
        );
    }

    #[test]
    fn exec_fingerprint_changes_when_env_cmd_or_cwd_changes() {
        let mut e1 = BTreeMap::new();
        e1.insert("A".into(), "1".into());
        e1.insert("B".into(), "2".into());
        let fp1 = exec_fingerprint(Path::new("/tmp/w"), "claude", &e1);
        let fp2 = exec_fingerprint(Path::new("/tmp/w"), "claude", &e1);
        assert_eq!(fp1, fp2);

        let fp_diff_cmd = exec_fingerprint(Path::new("/tmp/w"), "evil", &e1);
        assert_ne!(fp1, fp_diff_cmd);

        let mut e2 = e1.clone();
        e2.insert("A".into(), "9".into());
        let fp_diff_env = exec_fingerprint(Path::new("/tmp/w"), "claude", &e2);
        assert_ne!(fp1, fp_diff_env);
    }
}
