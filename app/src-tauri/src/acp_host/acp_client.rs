use super::adapters;
use super::command;
use super::dirs::expand_tilde;
use super::permission::{self, PermissionCache};
use super::pi_rpc_client;
use super::stream_buf::StreamCoalescer;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

pub type PendingPerms = Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbedModels {
    pub default_model: Option<String>,
    pub model_options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentPayload {
    pub file_name: String,
    pub mime: String,
    pub data_base64: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub preset: String,
    pub cmd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptStartMsg {
    pub session_id: String,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    pub acp_session_key: Option<String>,
    pub cwd: String,
    pub agent: AgentConfig,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub thinking_level: String,
    #[serde(default)]
    pub permission_policy: String,
    pub text: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentPayload>,
}

struct ManagedTerminal {
    command: String,
    output: Arc<Mutex<String>>,
    exit_rx: watch::Receiver<Option<i32>>,
    kill_tx: Option<oneshot::Sender<()>>,
    done_tx: watch::Sender<bool>,
}

struct Driver<W> {
    session_id: String,
    cwd: PathBuf,
    policy: String,
    writer: W,
    rx: mpsc::UnboundedReceiver<Option<Value>>,
    outbound_tx: mpsc::UnboundedSender<Value>,
    outbound_rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    suppress_chunks: bool,
    startup_text: Option<String>,
    stream: StreamCoalescer,
    plan_run_key: String,
    tool_starts: HashMap<String, Instant>,
    tool_titles: HashMap<String, String>,
    tool_terminals: Arc<Mutex<HashMap<String, String>>>,
    last_activity: (String, String),
    sink: mpsc::Sender<Value>,
    pending_perms: PendingPerms,
    perm_cache: Arc<Mutex<PermissionCache>>,
    cancel: watch::Receiver<bool>,
    terminals: HashMap<String, ManagedTerminal>,
}

pub async fn run_prompt_task(
    msg: PromptStartMsg,
    sink: mpsc::Sender<Value>,
    pending_perms: PendingPerms,
    perm_cache: Arc<Mutex<PermissionCache>>,
    cancel: watch::Receiver<bool>,
) {
    let dir = expand_tilde(&msg.cwd);
    if !dir.is_dir() {
        emit_done(&sink, &msg.session_id, None, &dir, Some(format!("工作目录不存在：{}", dir.display()))).await;
        return;
    }

    let (prog, args) = match command::split_cmdline(&msg.agent.cmd) {
        Ok(x) => x,
        Err(e) => {
            emit_done(&sink, &msg.session_id, None, &dir, Some(e)).await;
            return;
        }
    };

    let is_pi = pi_rpc_client::is_pi_rpc_invocation(&msg.agent.preset, &prog, &args);
    let (launch_args, gate_token) = if is_pi {
        match pi_rpc_client::prepare_launch_args(&args, &msg, &super::host_data_dir()) {
            Ok(x) => x,
            Err(err_msg) => {
                emit_done(&sink, &msg.session_id, None, &dir, Some(err_msg)).await;
                return;
            }
        }
    } else {
        (args, None)
    };

    let mut cmd = match prepare_agent_command(&msg, &prog, &launch_args, &sink, &cancel).await {
        Ok(c) => c,
        Err(err_msg) => {
            emit_done(&sink, &msg.session_id, None, &dir, Some(err_msg)).await;
            return;
        }
    };
    cmd.current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command::configure_child_group(&mut cmd);
    for (k, v) in &msg.agent.env {
        cmd.env(k, v);
    }
    if let Some(ref tok) = gate_token {
        cmd.env("CHAT_PI_GATE_TOKEN", tok);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emit_done(
                &sink,
                &msg.session_id,
                None,
                &dir,
                Some(format!("启动 Agent 进程失败：{e}")),
            )
            .await;
            return;
        }
    };

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut se) = child.stderr.take() {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 1024];
            loop {
                match se.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut g = tail.lock().unwrap();
                        if g.len() < 2048 {
                            g.push_str(&String::from_utf8_lossy(&buf[..n]));
                        }
                    }
                }
            }
        });
    }

    let child_pid = child.id();
    let upload_dir = session_upload_dir(&msg);
    let res = {
        let drive_fut = async {
            if is_pi {
                pi_rpc_client::drive_stdio(
                    stdout,
                    stdin,
                    &msg,
                    &dir,
                    sink.clone(),
                    pending_perms,
                    perm_cache,
                    cancel,
                    gate_token.as_deref(),
                )
                .await
            } else {
                drive_stdio(
                    stdout,
                    stdin,
                    &msg,
                    &dir,
                    sink.clone(),
                    pending_perms,
                    perm_cache,
                    cancel,
                )
                .await
            }
        };
        tokio::pin!(drive_fut);
        tokio::select! {
            r = &mut drive_fut => r,
            exit_res = child.wait() => {
                match tokio::time::timeout(Duration::from_secs(2), &mut drive_fut).await {
                    Ok(r) => r,
                    Err(_) => {
                        let code_desc = match exit_res {
                            Ok(st) => st
                                .code()
                                .map(|c| format!("exit {c}"))
                                .unwrap_or_else(|| "信号终止".to_string()),
                            Err(e) => format!("{e}"),
                        };
                        Err(format!("Agent 进程已提前退出（{code_desc}）"))
                    }
                }
            }
        }
    };

    if is_pi {
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
    command::kill_child_tree(&mut child).await;
    if child.id().is_none() {
        if let Some(pid) = child_pid {
            command::kill_pid_tree(pid).await;
        }
    }
    if let Some(udir) = upload_dir {
        let _ = std::fs::remove_dir_all(udir);
    }
    match res {
        Ok(acp_key) => {
            let diffs = command::collect_git_diffs(&dir).await;
            if diffs.as_array().is_some_and(|a| !a.is_empty()) {
                let _ = sink
                    .send(json!({
                        "type": "session_update",
                        "session_id": msg.session_id,
                        "kind": "diffs",
                        "diffs": diffs,
                    }))
                    .await;
            }
            emit_done(&sink, &msg.session_id, acp_key.as_deref(), &dir, None).await;
        }
        Err(e) => {
            let extra = stderr_tail.lock().unwrap().trim().to_string();
            let err_msg = if extra.is_empty() {
                e
            } else {
                format!("{e} ({})", extra.chars().take(240).collect::<String>())
            };
            emit_done(&sink, &msg.session_id, None, &dir, Some(err_msg)).await;
        }
    }
}

pub(super) async fn ask_user_permission_via_sink(
    sink: &mpsc::Sender<Value>,
    pending_perms: &PendingPerms,
    cancel: &watch::Receiver<bool>,
    session_id: &str,
    cwd: &Path,
    policy: &str,
    tool_kind: &str,
    cmd: &str,
    reason: &str,
) -> String {
    if policy == "allow_all" {
        return "allow_once".to_string();
    }
    let req_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = oneshot::channel::<String>();
    pending_perms.lock().unwrap().insert(req_id.clone(), tx);

    let _ = sink
        .send(json!({
            "type": "session_update",
            "session_id": session_id,
            "kind": "permission_request",
            "req_id": req_id,
            "tool_kind": tool_kind,
            "command": cmd,
            "cwd": cwd.to_string_lossy(),
            "reason": reason,
        }))
        .await;

    let mut cancel_rx = cancel.clone();
    loop {
        if *cancel_rx.borrow() {
            pending_perms.lock().unwrap().remove(&req_id);
            return "reject_once".to_string();
        }
        tokio::select! {
            biased;
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    pending_perms.lock().unwrap().remove(&req_id);
                    return "reject_once".to_string();
                }
            }
            res = &mut rx => {
                pending_perms.lock().unwrap().remove(&req_id);
                return res.unwrap_or_else(|_| "reject_once".to_string());
            }
        }
    }
}

async fn emit_install_plan(sink: &mpsc::Sender<Value>, session_id: &str, run_key: &str, label: &str, state: &str) {
    let _ = sink
        .send(json!({
            "type": "session_update",
            "session_id": session_id,
            "kind": "plan",
            "run_key": run_key,
            "steps": [{ "label": label, "state": state }],
        }))
        .await;
}

async fn emit_agent_status(sink: &mpsc::Sender<Value>, agent_id: Option<&str>, status: &str) {
    let Some(aid) = agent_id.filter(|s| !s.is_empty()) else {
        return;
    };
    let _ = sink
        .send(json!({
            "type": "agent_status",
            "agent_id": aid,
            "status": status,
        }))
        .await;
}

pub(super) async fn emit_agent_models(
    sink: &mpsc::Sender<Value>,
    agent_id: Option<&str>,
    session_id: Option<&str>,
    models: &ProbedModels,
) {
    if models.model_options.is_empty() {
        return;
    }
    let Some(aid) = agent_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    let _ = sink
        .send(json!({
            "type": "agent_models",
            "agent_id": aid,
            "session_id": session_id.filter(|s| !s.is_empty()),
            "default_model": models.default_model,
            "model_options": models.model_options,
        }))
        .await;
}

pub fn extract_acp_model_catalog(session_res: &Value) -> ProbedModels {
    if let Some(arr) = session_res["configOptions"].as_array() {
        if let Some(opt) = arr.iter().find(|o| {
            o["category"].as_str() == Some("model") || o["id"].as_str() == Some("model")
        }) {
            let mut options = collect_config_option_values(&opt["options"]);
            let cur = opt["currentValue"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if let Some(ref c) = cur {
                if !options.iter().any(|x| x == c) {
                    options.insert(0, c.clone());
                }
            }
            if !options.is_empty() {
                let default_model = cur.or_else(|| options.first().cloned());
                return ProbedModels {
                    default_model,
                    model_options: options,
                };
            }
        }
    }

    let mut legacy_opts = Vec::new();
    if let Some(arr) = session_res
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
    {
        for item in arr {
            if let Some(m) = item["modelId"]
                .as_str()
                .or_else(|| item["id"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !legacy_opts.iter().any(|x| x == m) {
                    legacy_opts.push(m.to_string());
                }
            }
        }
    }
    let cur = session_res
        .pointer("/models/currentModelId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(ref c) = cur {
        if !legacy_opts.iter().any(|x| x == c) {
            legacy_opts.insert(0, c.clone());
        }
    }
    let default_model = if legacy_opts.is_empty() {
        None
    } else {
        cur.or_else(|| legacy_opts.first().cloned())
    };
    ProbedModels {
        default_model,
        model_options: legacy_opts,
    }
}

fn collect_config_option_values(raw: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(arr) = raw.as_array() else {
        return out;
    };
    for item in arr {
        if let Some(v) = item["value"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !out.iter().any(|x| x == v) {
                out.push(v.to_string());
            }
        } else if let Some(sub) = item["options"].as_array() {
            for sub_item in sub {
                if let Some(v) = sub_item["value"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if !out.iter().any(|x| x == v) {
                        out.push(v.to_string());
                    }
                }
            }
        }
    }
    out
}

fn collect_config_option_pairs(raw: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(arr) = raw.as_array() else {
        return out;
    };
    for item in arr {
        if let Some(v) = item["value"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let name = item["name"].as_str().unwrap_or("").trim().to_string();
            out.push((v.to_string(), name));
        } else if let Some(sub) = item["options"].as_array() {
            for sub_item in sub {
                if let Some(v) = sub_item["value"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let name = sub_item["name"].as_str().unwrap_or("").trim().to_string();
                    out.push((v.to_string(), name));
                }
            }
        }
    }
    out
}

pub fn probe_scratch_dir() -> PathBuf {
    let dir = super::host_data_dir().join("probe-cwd");
    let _ = std::fs::create_dir_all(&dir);
    restrict_dir_owner(&dir);
    dir
}

pub async fn probe_models(
    preset: &str,
    cmd_str: &str,
    env: &HashMap<String, String>,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<ProbedModels, String> {
    let (prog, args) = command::split_cmdline(cmd_str)?;
    let path_env = command::effective_path_env();
    let pathext = command::default_pathext_env();
    let data_dir = super::host_data_dir();
    let Some(bin_path) = adapters::resolve_agent_program_in(&data_dir, &prog, &path_env, &pathext)
    else {
        return Err(format!("未检测到可执行文件：{prog}"));
    };

    let is_pi = pi_rpc_client::is_pi_rpc_invocation(preset, &prog, &args);
    let launch_args = if is_pi {
        pi_rpc_client::prepare_probe_args(&args)
    } else {
        args
    };

    let probe_dir = cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(expand_tilde)
        .filter(|p| p.is_dir() && !super::dirs::is_sensitive_path(p))
        .unwrap_or_else(probe_scratch_dir);

    let mut cmd = command::prepare_resolved(&bin_path, &launch_args, &path_env);
    cmd.current_dir(&probe_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command::configure_child_group(&mut cmd);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 Agent 探测进程失败：{e}"))?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let res = tokio::time::timeout(timeout, async {
        if is_pi {
            pi_rpc_client::probe_stdio(stdout, stdin).await
        } else {
            probe_acp_stdio(stdout, stdin, &probe_dir).await
        }
    })
    .await;

    command::kill_child_tree(&mut child).await;
    match res {
        Ok(inner) => inner,
        Err(_) => Err("探测 Agent 模型列表超时".into()),
    }
}

pub(crate) async fn probe_acp_stdio<R, W>(
    reader: R,
    writer: W,
    dir: &Path,
) -> Result<ProbedModels, String>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send,
{
    let (tx, rx) = mpsc::unbounded_channel::<Option<Value>>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if tx.send(Some(v)).is_err() {
                            return;
                        }
                    }
                }
                _ => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
    });

    let (dummy_sink, _dummy_rx) = mpsc::channel::<Value>(4);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<Value>();
    let mut drv = Driver {
        session_id: String::new(),
        cwd: dir.to_path_buf(),
        policy: "ask".to_string(),
        writer,
        rx,
        outbound_tx,
        outbound_rx,
        next_id: 1,
        suppress_chunks: true,
        startup_text: None,
        stream: StreamCoalescer::new(),
        plan_run_key: String::new(),
        tool_starts: HashMap::new(),
        tool_titles: HashMap::new(),
        tool_terminals: Arc::new(Mutex::new(HashMap::new())),
        last_activity: (String::new(), String::new()),
        sink: dummy_sink,
        pending_perms: Arc::new(Mutex::new(HashMap::new())),
        perm_cache: Arc::new(Mutex::new(PermissionCache::default())),
        cancel: cancel_rx,
        terminals: HashMap::new(),
    };

    let init = drv
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": true
                }
            }),
        )
        .await?;
    let caps = init.get("agentCapabilities").cloned().unwrap_or(Value::Null);
    let new_res = drv
        .call("session/new", json!({ "cwd": dir, "mcpServers": [] }))
        .await?;
    let catalog = extract_acp_model_catalog(&new_res);

    if let Some(sid) = new_res["sessionId"].as_str().filter(|s| !s.is_empty()) {
        let cleanup_method = if caps
            .pointer("/sessionCapabilities/delete")
            .is_some_and(|v| !v.is_null() && *v != json!(false))
        {
            Some("session/delete")
        } else if caps
            .pointer("/sessionCapabilities/close")
            .is_some_and(|v| !v.is_null() && *v != json!(false))
        {
            Some("session/close")
        } else {
            None
        };
        if let Some(m) = cleanup_method {
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                drv.call(m, json!({ "sessionId": sid })),
            )
            .await;
        }
    }

    Ok(catalog)
}

async fn prepare_agent_command(
    msg: &PromptStartMsg,
    prog: &str,
    args: &[String],
    sink: &mpsc::Sender<Value>,
    cancel: &watch::Receiver<bool>,
) -> Result<tokio::process::Command, String> {
    let path_env = command::effective_path_env();
    let pathext = command::default_pathext_env();
    let data_dir = super::host_data_dir();

    if let Some(bin_path) = adapters::resolve_agent_program_in(&data_dir, prog, &path_env, &pathext) {
        return Ok(command::prepare_resolved(&bin_path, args, &path_env));
    }

    let Some(spec) = adapters::lookup_npm_adapter(prog) else {
        return Ok(command::prepare(prog, args));
    };

    let install_key = format!("install-{}", Uuid::new_v4());
    emit_install_plan(
        sink,
        &msg.session_id,
        &install_key,
        &format!("未检测到本地 {}，正在通过 npm 自动安装 {}…", spec.bin, spec.package),
        "active",
    )
    .await;
    let _ = sink
        .send(json!({
            "type": "session_update",
            "session_id": msg.session_id,
            "kind": "rpc_log",
            "entry": {
                "dir": "sys",
                "method": "adapter/install",
                "summary": format!("npm install {}", spec.package),
            },
        }))
        .await;
    emit_agent_status(sink, msg.agent.id.as_deref(), "installing").await;

    let mut install_cancel = cancel.clone();
    match adapters::ensure_npm_adapter(spec, &data_dir, Some(&mut install_cancel)).await {
        Ok(bin_path) => {
            emit_install_plan(
                sink,
                &msg.session_id,
                &install_key,
                &format!("已通过 npm 自动安装 {}", spec.package),
                "done",
            )
            .await;
            emit_agent_status(sink, msg.agent.id.as_deref(), "online").await;
            Ok(command::prepare_resolved(&bin_path, args, &path_env))
        }
        Err(err_msg) => {
            emit_install_plan(
                sink,
                &msg.session_id,
                &install_key,
                &format!("自动安装 {} 失败", spec.package),
                "pending",
            )
            .await;
            emit_agent_status(sink, msg.agent.id.as_deref(), "standby").await;
            Err(err_msg)
        }
    }
}

async fn emit_done(
    sink: &mpsc::Sender<Value>,
    session_id: &str,
    acp_session_key: Option<&str>,
    dir: &Path,
    error: Option<String>,
) {
    let (branch, branches) = if dir.is_dir() {
        command::git_info(dir).await
    } else {
        (String::new(), Vec::new())
    };
    let _ = sink
        .send(json!({
            "type": "session_update",
            "session_id": session_id,
            "kind": "prompt_done",
            "acp_session_key": acp_session_key,
            "branch": branch,
            "branches": branches,
            "error": error,
        }))
        .await;
}

pub(crate) async fn drive_stdio<R, W>(
    reader: R,
    writer: W,
    msg: &PromptStartMsg,
    dir: &Path,
    sink: mpsc::Sender<Value>,
    pending_perms: PendingPerms,
    perm_cache: Arc<Mutex<PermissionCache>>,
    cancel: watch::Receiver<bool>,
) -> Result<Option<String>, String>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send,
{
    let (tx, rx) = mpsc::unbounded_channel::<Option<Value>>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if tx.send(Some(v)).is_err() {
                            return;
                        }
                    }
                }
                _ => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
    });

    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<Value>();
    let mut drv = Driver {
        session_id: msg.session_id.clone(),
        cwd: dir.to_path_buf(),
        policy: if msg.permission_policy.is_empty() {
            "auto_read".to_string()
        } else {
            msg.permission_policy.clone()
        },
        writer,
        rx,
        outbound_tx,
        outbound_rx,
        next_id: 1,
        suppress_chunks: true,
        startup_text: None,
        stream: StreamCoalescer::new(),
        plan_run_key: Uuid::new_v4().to_string(),
        tool_starts: HashMap::new(),
        tool_titles: HashMap::new(),
        tool_terminals: Arc::new(Mutex::new(HashMap::new())),
        last_activity: (String::new(), String::new()),
        sink,
        pending_perms,
        perm_cache,
        cancel,
        terminals: HashMap::new(),
    };

    let init = drv
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": true
                }
            }),
        )
        .await?;
    drv.log_rpc("out", "initialize", "ACP v1 handshake").await;

    let caps = init.get("agentCapabilities").cloned().unwrap_or(Value::Null);
    let image_ok = caps
        .pointer("/promptCapabilities/image")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut resumed: Option<(String, Value)> = None;

    if let Some(prev) = msg.acp_session_key.as_deref().filter(|s| !s.is_empty()) {
        let resume_params = json!({ "sessionId": prev, "cwd": dir, "mcpServers": [] });
        if caps
            .pointer("/sessionCapabilities/resume")
            .is_some_and(|v| !v.is_null() && *v != json!(false))
        {
            if let Ok(r) = drv.call("session/resume", resume_params.clone()).await {
                let sid = r["sessionId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| prev.to_string());
                resumed = Some((sid, r));
            }
        } else if caps.get("loadSession").and_then(Value::as_bool) == Some(true) {
            if let Ok(r) = drv.call("session/load", resume_params).await {
                let sid = r["sessionId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| prev.to_string());
                resumed = Some((sid, r));
            }
        }
    }

    let (resolved_sid, session_res) = if let Some(pair) = resumed {
        pair
    } else {
        let new_res = drv
            .call("session/new", json!({ "cwd": dir, "mcpServers": [] }))
            .await?;
        drv.log_rpc("out", "session/new", &format!("cwd: {}", dir.display()))
            .await;
        drv.startup_text = new_res
            .get("_meta")
            .and_then(Value::as_object)
            .and_then(|o| o.values().find_map(|v| v.get("startupInfo")?.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let sid = new_res["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Agent 未返回 sessionId".to_string())?
            .to_string();
        (sid, new_res)
    };

    let catalog = extract_acp_model_catalog(&session_res);
    emit_agent_models(
        &drv.sink,
        msg.agent.id.as_deref(),
        Some(&msg.session_id),
        &catalog,
    )
    .await;

    apply_acp_session_preferences(
        &mut drv,
        &resolved_sid,
        &session_res,
        &msg.model,
        &msg.thinking_level,
    )
    .await;

    let blocks = build_prompt_blocks(msg, dir, image_ok);
    drv.suppress_chunks = false;
    drv.emit_activity("connecting", "等待 Agent 响应…").await;
    drv.log_rpc("out", "session/prompt", &format!("blocks: {}", blocks.len())).await;
    let prompt_req_id = drv
        .send_req(
            "session/prompt",
            json!({
                "sessionId": resolved_sid,
                "prompt": blocks,
                "_meta": {
                    "thinkingLevel": msg.thinking_level
                }
            }),
        )
        .await?;

    let mut cancel_rx = drv.cancel.clone();
    let cancelled = drv.wait_resp_or_cancel(prompt_req_id, &mut cancel_rx).await;
    for (_, mut term) in drv.terminals.drain() {
        let _ = term.done_tx.send(true);
        if let Some(ktx) = term.kill_tx.take() {
            let _ = ktx.send(());
        }
    }
    let cancelled = cancelled?;
    if cancelled {
        let _ = drv
            .notify("session/cancel", json!({ "sessionId": resolved_sid }))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(4), drv.wait_resp(prompt_req_id)).await;
    }

    // 把最后一条活动流式块（思考或正文）收尾标成 done
    drv.stream.finish_active(&drv.sink, &drv.session_id).await;
    drv.emit_activity("idle", "").await;
    Ok(Some(resolved_sid))
}

async fn apply_acp_session_preferences<W: AsyncWrite + Unpin + Send>(
    drv: &mut Driver<W>,
    session_id: &str,
    session_res: &Value,
    raw_model: &str,
    raw_thinking: &str,
) {
    let mut config_opts = session_res.get("configOptions").cloned();
    let want_model = raw_model.trim();

    if !want_model.is_empty() && !pi_rpc_client::is_default_model_placeholder(want_model) {
        let model_cfg = config_opts
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o["category"].as_str() == Some("model") || o["id"].as_str() == Some("model")
                })
            });

        if let Some(opt) = model_cfg {
            let cfg_id = opt["id"].as_str().unwrap_or("model").to_string();
            let cur_val = opt["currentValue"].as_str().map(str::to_string);
            let pairs = collect_config_option_pairs(&opt["options"]);
            let matched = match_option_pair(&pairs, want_model).or_else(|| {
                if pairs.is_empty() {
                    Some(want_model.to_string())
                } else {
                    None
                }
            });
            if let Some(target_val) = matched {
                if cur_val.as_deref() != Some(target_val.as_str()) {
                    if let Ok(resp) = drv
                        .call(
                            "session/set_config_option",
                            json!({
                                "sessionId": session_id,
                                "configId": cfg_id,
                                "value": target_val,
                            }),
                        )
                        .await
                    {
                        drv.log_rpc(
                            "out",
                            "session/set_config_option",
                            &format!("{cfg_id}={target_val}"),
                        )
                        .await;
                        if resp.get("configOptions").is_some() {
                            config_opts = resp.get("configOptions").cloned();
                        }
                    }
                }
            }
        } else {
            let legacy_models: Vec<(String, String)> = session_res
                .pointer("/models/availableModels")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let id = item["modelId"]
                                .as_str()
                                .or_else(|| item["id"].as_str())?
                                .trim();
                            if id.is_empty() {
                                return None;
                            }
                            let name = item["name"].as_str().unwrap_or("").trim();
                            Some((id.to_string(), name.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let matched = match_legacy_acp_model(&legacy_models, want_model).or_else(|| {
                if legacy_models.is_empty() {
                    Some(want_model.to_string())
                } else {
                    None
                }
            });
            let cur_legacy = session_res
                .pointer("/models/currentModelId")
                .and_then(Value::as_str);
            if let Some(target_id) = matched {
                if cur_legacy != Some(target_id.as_str()) {
                    if drv
                        .call(
                            "session/set_model",
                            json!({ "sessionId": session_id, "modelId": target_id }),
                        )
                        .await
                        .is_ok()
                    {
                        drv.log_rpc("out", "session/set_model", &target_id).await;
                    }
                }
            }
        }
    }

    let effort_candidates = acp_thinking_candidates(raw_thinking);
    if !effort_candidates.is_empty() {
        let effort_cfg = config_opts
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o["category"].as_str() == Some("thought_level")
                        || matches!(
                            o["id"].as_str(),
                            Some("reasoning_effort" | "thinking_level")
                        )
                })
            });

        if let Some(opt) = effort_cfg {
            let cfg_id = opt["id"].as_str().unwrap_or("reasoning_effort").to_string();
            let cur_val = opt["currentValue"].as_str().map(str::to_string);
            let avail = collect_config_option_values(&opt["options"]);
            let chosen = effort_candidates.iter().find_map(|cand| {
                avail
                    .iter()
                    .find(|v| v.eq_ignore_ascii_case(cand))
                    .cloned()
            });
            if let Some(target_level) = chosen {
                if cur_val.as_deref() != Some(target_level.as_str()) {
                    if drv
                        .call(
                            "session/set_config_option",
                            json!({
                                "sessionId": session_id,
                                "configId": cfg_id,
                                "value": target_level,
                            }),
                        )
                        .await
                        .is_ok()
                    {
                        drv.log_rpc(
                            "out",
                            "session/set_config_option",
                            &format!("{cfg_id}={target_level}"),
                        )
                        .await;
                    }
                }
            }
        }
    }
}

fn match_option_pair(pairs: &[(String, String)], want: &str) -> Option<String> {
    if let Some((v, _)) = pairs.iter().find(|(v, _)| v == want) {
        return Some(v.clone());
    }
    if let Some((v, _)) = pairs.iter().find(|(v, _)| v.eq_ignore_ascii_case(want)) {
        return Some(v.clone());
    }
    pairs
        .iter()
        .find(|(_, name)| !name.is_empty() && name.eq_ignore_ascii_case(want))
        .map(|(v, _)| v.clone())
}

fn match_legacy_acp_model(models: &[(String, String)], want: &str) -> Option<String> {
    if let Some(hit) = match_option_pair(models, want) {
        return Some(hit);
    }
    models
        .iter()
        .find(|(id, _)| {
            id.split('[')
                .next()
                .is_some_and(|base| base.eq_ignore_ascii_case(want))
        })
        .map(|(id, _)| id.clone())
}

fn acp_thinking_candidates(raw: &str) -> &'static [&'static str] {
    match raw.trim().to_ascii_lowercase().as_str() {
        "关" | "off" | "none" => &["none", "off", "minimal", "low"],
        "低" | "low" => &["low", "minimal", "medium"],
        "中" | "medium" => &["medium", "high", "low"],
        "高" | "high" => &["high", "xhigh", "max", "ultra", "medium"],
        _ => &[],
    }
}

pub(super) fn session_upload_dir(msg: &PromptStartMsg) -> Option<PathBuf> {
    let session_id = Uuid::parse_str(msg.session_id.trim()).ok()?;
    let directory_id = msg.run_id.unwrap_or(session_id);
    Some(super::host_data_dir().join("uploads").join(directory_id.to_string()))
}

pub(super) fn restrict_dir_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub(super) fn restrict_file_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn build_prompt_blocks(msg: &PromptStartMsg, dir: &Path, image_ok: bool) -> Vec<Value> {
    let mut blocks = Vec::new();
    let upload_dir = session_upload_dir(msg);
    for att in &msg.attachments {
        if let Some(b64) = &att.data_base64 {
            if image_ok && att.mime.starts_with("image/") {
                blocks.push(json!({
                    "type": "image",
                    "mimeType": att.mime,
                    "data": b64,
                }));
                continue;
            }
            let Some(ref udir) = upload_dir else {
                continue;
            };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                let _ = std::fs::create_dir_all(udir);
                restrict_dir_owner(udir);
                let safe_name: String = att
                    .file_name
                    .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
                    .chars()
                    .take(120)
                    .collect();
                let dest = udir.join(format!("{}_{safe_name}", Uuid::new_v4().simple()));
                if std::fs::write(&dest, bytes).is_ok() {
                    restrict_file_owner(&dest);
                    blocks.push(json!({
                        "type": "resource_link",
                        "uri": file_uri(&dest),
                        "name": att.file_name,
                        "mimeType": att.mime,
                    }));
                }
            }
        }
    }
    for f in &msg.files {
        let clean = f.trim().trim_start_matches('@');
        if !clean.is_empty() && permission::path_inside_root(clean, dir) {
            let path = if Path::new(clean).is_absolute() {
                PathBuf::from(clean)
            } else {
                dir.join(clean)
            };
            blocks.push(json!({
                "type": "resource_link",
                "uri": file_uri(&path),
                "name": clean,
            }));
        }
    }
    if !msg.text.is_empty() {
        blocks.push(json!({
            "type": "text",
            "text": msg.text,
        }));
    }
    blocks
}

fn file_uri(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        s = format!("/{s}");
    }
    format!("file://{s}")
}

impl<W: AsyncWrite + Unpin + Send> Driver<W> {
    async fn log_rpc(&self, dir: &str, method: &str, summary: &str) {
        let _ = self
            .sink
            .send(json!({
                "type": "session_update",
                "session_id": self.session_id,
                "kind": "rpc_log",
                "entry": {
                    "dir": dir,
                    "method": method,
                    "summary": summary,
                }
            }))
            .await;
    }

    async fn write_line(&mut self, v: &Value) -> Result<(), String> {
        let mut s = v.to_string();
        s.push('\n');
        self.writer
            .write_all(s.as_bytes())
            .await
            .map_err(|e| format!("写入 Agent stdin 失败：{e}"))?;
        let _ = self.writer.flush().await;
        Ok(())
    }

    async fn send_req(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        Ok(id)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.send_req(method, params).await?;
        self.wait_resp(id).await
    }

    async fn emit_activity(&mut self, phase: &str, detail: &str) {
        if self.last_activity.0 == phase && self.last_activity.1 == detail {
            return;
        }
        self.last_activity = (phase.to_string(), detail.to_string());
        let _ = self
            .sink
            .send(json!({
                "type": "session_update",
                "session_id": self.session_id,
                "kind": "activity",
                "phase": phase,
                "detail": detail,
            }))
            .await;
    }

    async fn emit_usage(&self, usage: Value) {
        let _ = self
            .sink
            .send(json!({
                "type": "session_update",
                "session_id": self.session_id,
                "kind": "activity",
                "phase": if self.last_activity.0.is_empty() { "responding" } else { &self.last_activity.0 },
                "detail": self.last_activity.1,
                "usage": usage,
            }))
            .await;
    }

    async fn wait_resp(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let deadline = self.stream.next_flush_deadline();
            tokio::select! {
                biased;
                _ = async {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    self.stream.flush_due(&self.sink, &self.session_id).await;
                }
                Some(resp) = self.outbound_rx.recv() => {
                    self.write_line(&resp).await?;
                }
                msg = self.rx.recv() => {
                    let msg = match msg {
                        Some(Some(v)) => v,
                        _ => return Err("Agent 进程意外退出".to_string()),
                    };
                    if let Some(res) = self.take_msg(id, msg).await? {
                        return res;
                    }
                }
            }
        }
    }

    async fn wait_resp_or_cancel(
        &mut self,
        id: u64,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<bool, String> {
        loop {
            if *cancel.borrow() {
                return Ok(true);
            }
            let deadline = self.stream.next_flush_deadline();
            tokio::select! {
                biased;
                changed = cancel.changed() => {
                    if changed.is_ok() && *cancel.borrow() {
                        return Ok(true);
                    }
                }
                _ = async {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    self.stream.flush_due(&self.sink, &self.session_id).await;
                }
                Some(resp) = self.outbound_rx.recv() => {
                    self.write_line(&resp).await?;
                }
                msg = self.rx.recv() => {
                    let msg = match msg {
                        Some(Some(v)) => v,
                        _ => return Err("Agent 进程意外退出".to_string()),
                    };
                    if let Some(res) = self.take_msg(id, msg).await? {
                        res?;
                        return Ok(false);
                    }
                }
            }
        }
    }

    async fn take_msg(&mut self, id: u64, msg: Value) -> Result<Option<Result<Value, String>>, String> {
        if msg.get("id").and_then(Value::as_u64) == Some(id)
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            return Ok(Some(match msg.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("agent error").to_string()),
                None => Ok(msg["result"].clone()),
            }));
        }
        if msg.get("id").is_some() && msg.get("method").is_some() {
            self.answer_request(&msg).await?;
        } else if let Some(method) = msg.get("method").and_then(Value::as_str) {
            self.on_notify(method, &msg["params"]).await;
        }
        Ok(None)
    }

    async fn on_notify(&mut self, method: &str, params: &Value) {
        if method != "session/update" || self.suppress_chunks {
            return;
        }
        let upd = &params["update"];
        match upd["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let text = upd
                    .pointer("/content/text")
                    .and_then(Value::as_str)
                    .or_else(|| upd["content"].as_str());
                if let Some(t) = text {
                    if self.startup_text.as_deref() == Some(t) {
                        self.startup_text = None;
                        return;
                    }
                    let msg_id = upd["messageId"].as_str();
                    self.emit_activity("responding", "正在生成回复…").await;
                    self.stream
                        .push_text(t, msg_id, None, &self.sink, &self.session_id)
                        .await;
                }
            }
            Some("agent_thought_chunk") => {
                let text = upd
                    .pointer("/content/text")
                    .and_then(Value::as_str)
                    .or_else(|| upd["content"].as_str());
                if let Some(t) = text {
                    let msg_id = upd["messageId"].as_str();
                    self.emit_activity("thinking", "正在思考…").await;
                    self.stream
                        .push_thought(t, msg_id, None, &self.sink, &self.session_id)
                        .await;
                }
            }
            Some(kind_str @ ("tool_call" | "tool_call_update")) => {
                let is_create = kind_str == "tool_call";
                let tool_call_id = upd["toolCallId"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| Uuid::new_v4().to_string());

                if is_create || !self.tool_starts.contains_key(&tool_call_id) {
                    self.stream.finish_active(&self.sink, &self.session_id).await;
                }

                let raw_status = upd["status"].as_str().map(str::trim).filter(|s| !s.is_empty());
                let effective_status = raw_status.or(if is_create { Some("pending") } else { None });
                if matches!(effective_status, Some("pending" | "in_progress")) {
                    self.tool_starts
                        .entry(tool_call_id.clone())
                        .or_insert_with(Instant::now);
                }
                let is_terminal = matches!(effective_status, Some("completed" | "failed"));
                let duration_ms = if is_terminal {
                    self.tool_starts
                        .remove(&tool_call_id)
                        .map(|t| t.elapsed().as_millis() as u64)
                } else {
                    None
                };

                let title = upd["title"].as_str().map(str::trim).filter(|s| !s.is_empty());
                let name = upd["name"].as_str().map(str::trim).filter(|s| !s.is_empty());
                if let Some(label) = title.or(name) {
                    if !is_terminal {
                        self.tool_titles.insert(tool_call_id.clone(), label.to_string());
                    }
                }
                if is_terminal {
                    self.tool_titles.remove(&tool_call_id);
                }
                let tool_kind = upd["kind"].as_str().map(str::trim).filter(|s| !s.is_empty());
                let input = extract_acp_raw_input(&upd["rawInput"]);
                let locations = extract_acp_locations(&upd["locations"]);

                let mut content_texts = Vec::new();
                let mut diffs = Vec::new();
                let mut terminal_snap: Option<String> = None;
                if let Some(arr) = upd["content"].as_array() {
                    for item in arr {
                        match item["type"].as_str() {
                            Some("content") => {
                                if let Some(t) = item
                                    .pointer("/content/text")
                                    .and_then(Value::as_str)
                                    .or_else(|| item["text"].as_str())
                                    .filter(|s| !s.is_empty())
                                {
                                    content_texts.push(t.to_string());
                                }
                            }
                            Some("diff") => {
                                if let Some(path) =
                                    item["path"].as_str().map(str::trim).filter(|s| !s.is_empty())
                                {
                                    diffs.push(json!({
                                        "path": path,
                                        "old_text": item["oldText"].as_str(),
                                        "new_text": item["newText"].as_str().unwrap_or(""),
                                    }));
                                }
                            }
                            Some("terminal") => {
                                if let Some(tid) =
                                    item["terminalId"].as_str().map(str::trim).filter(|s| !s.is_empty())
                                {
                                    self.tool_terminals
                                        .lock()
                                        .unwrap()
                                        .insert(tid.to_string(), tool_call_id.clone());
                                    if let Some(term) = self.terminals.get(tid) {
                                        let buf = term.output.lock().unwrap().clone();
                                        if !buf.is_empty() {
                                            terminal_snap = Some(buf);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                let output = if !content_texts.is_empty() {
                    Some(content_texts.join("\n"))
                } else {
                    extract_acp_raw_output(&upd["rawOutput"]).or(terminal_snap)
                };

                let mut ev = json!({
                    "type": "session_update",
                    "session_id": self.session_id,
                    "kind": "tool_call",
                    "tool_call_id": tool_call_id,
                });
                if let Some(n) = name {
                    ev["name"] = json!(n);
                }
                if let Some(k) = tool_kind {
                    ev["tool_kind"] = json!(k);
                }
                if let Some(t) = title.or(if is_create { name } else { None }) {
                    ev["title"] = json!(t);
                }
                if let Some(st) = effective_status {
                    ev["status"] = json!(st);
                }
                if let Some(inp) = input {
                    ev["input"] = json!(inp);
                }
                if let Some(out) = output {
                    ev["output"] = json!(out);
                }
                if !locations.is_empty() {
                    ev["locations"] = json!(locations);
                }
                if !diffs.is_empty() {
                    ev["diffs"] = json!(diffs);
                }
                if let Some(ms) = duration_ms {
                    ev["duration_ms"] = json!(ms);
                }
                let _ = self.sink.send(ev).await;

                if is_terminal {
                    if let Some((next_id, _)) = self.tool_starts.iter().next() {
                        let next_detail = self
                            .tool_titles
                            .get(next_id)
                            .cloned()
                            .unwrap_or_else(|| "执行工具中…".to_string());
                        self.emit_activity("tooling", &next_detail).await;
                    } else {
                        self.emit_activity("connecting", "等待 Agent 响应…").await;
                    }
                } else if matches!(effective_status, Some("pending" | "in_progress"))
                    || (effective_status.is_none() && self.tool_starts.contains_key(&tool_call_id))
                {
                    let detail = title
                        .or(name)
                        .map(str::to_string)
                        .or_else(|| self.tool_titles.get(&tool_call_id).cloned())
                        .unwrap_or_else(|| "执行工具中…".to_string());
                    self.emit_activity("tooling", &detail).await;
                }
            }
            Some("plan") => {
                self.stream.finish_active(&self.sink, &self.session_id).await;
                let steps: Vec<Value> = upd["entries"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|e| {
                                let state = match e["status"].as_str() {
                                    Some("completed") => "done",
                                    Some("in_progress") => "active",
                                    _ => "pending",
                                };
                                json!({
                                    "label": e["content"].as_str().unwrap_or(""),
                                    "state": state,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = self
                    .sink
                    .send(json!({
                        "type": "session_update",
                        "session_id": self.session_id,
                        "kind": "plan",
                        "run_key": self.plan_run_key,
                        "steps": steps,
                    }))
                    .await;
            }
            Some("usage_update") => {
                if let Some(u) = extract_acp_usage(upd) {
                    self.emit_usage(u).await;
                }
            }
            Some("compaction_update") => {
                let st = upd["status"].as_str().unwrap_or("in_progress");
                let detail = match st {
                    "in_progress" => "正在压缩上下文…",
                    "failed" => upd["error"].as_str().unwrap_or("上下文压缩失败"),
                    _ => "上下文已压缩",
                };
                if st == "in_progress" {
                    self.emit_activity("compacting", detail).await;
                }
                self.log_rpc("in", "session/compaction", detail).await;
            }
            Some("notice") => {
                let title = upd["title"].as_str().unwrap_or("");
                let desc = upd["description"].as_str().unwrap_or("");
                let summary = if desc.is_empty() {
                    title.to_string()
                } else {
                    format!("{title}: {desc}")
                };
                if !summary.is_empty() {
                    self.log_rpc("in", "session/notice", &summary).await;
                }
            }
            _ => {}
        }
    }

    async fn answer_request(&mut self, msg: &Value) -> Result<(), String> {
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("");
        let params = &msg["params"];

        let resp_opt = match method {
            "session/request_permission" => Some(self.handle_permission_req(id, params).await?),
            "fs/read_text_file" => Some(self.handle_fs_read(id, params).await?),
            "fs/write_text_file" => Some(self.handle_fs_write(id, params).await?),
            "terminal/create" => Some(self.handle_term_create(id, params).await?),
            "terminal/output" => Some(self.handle_term_output(id, params).await?),
            "terminal/wait_for_exit" => self.handle_term_wait(id, params).await?,
            "terminal/kill" => Some(self.handle_term_kill(id, params)),
            "terminal/release" => {
                if let Some(tid) = params["terminalId"].as_str() {
                    if let Some(mut t) = self.terminals.remove(tid) {
                        let _ = t.done_tx.send(true);
                        if let Some(ktx) = t.kill_tx.take() {
                            let _ = ktx.send(());
                        }
                    }
                }
                Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
            }
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unsupported method: {method}") }
            })),
        };
        if let Some(resp) = resp_opt {
            self.write_line(&resp).await?;
        }
        Ok(())
    }

    async fn handle_permission_req(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let tool = &params["toolCall"];
        let kind = tool["kind"].as_str().unwrap_or("other");
        let title = tool["title"].as_str().unwrap_or("");
        let locations = permission::location_strings(&tool["locations"]);
        let options = params.get("options").cloned().unwrap_or(Value::Null);

        let paths_in_scope = locations
            .iter()
            .all(|loc| permission::path_inside_root(loc, &self.cwd));

        let cmd_str = if !title.is_empty() {
            title.to_string()
        } else {
            format!("{kind} {}", locations.join(" "))
        };
        let key = permission::cache_key(&self.session_id, &self.cwd, kind, &cmd_str);

        if paths_in_scope {
            if let Some(cached) = self.perm_cache.lock().unwrap().get(&key) {
                let opt = permission::pick_option_id(
                    &options,
                    if cached { "allow_always" } else { "reject_always" },
                );
                return Ok(permission_result(id, opt.as_deref()));
            }

            if permission::evaluate_policy(&self.policy, kind, &locations, &self.cwd) == Some(true)
            {
                let opt = permission::pick_option_id(&options, "allow_once");
                return Ok(permission_result(id, opt.as_deref()));
            }
        }

        let reason = if paths_in_scope {
            format!("Agent 请求执行 {kind}")
        } else {
            format!("警告：目标路径超出工作目录！Agent 请求执行 {kind}")
        };
        let choice = self.ask_user_permission(kind, &cmd_str, &reason).await?;
        if choice.starts_with("allow") {
            self.emit_activity("tooling", &cmd_str).await;
        }
        if paths_in_scope {
            self.perm_cache.lock().unwrap().record(key, &choice);
        }
        let opt = permission::pick_option_id(&options, &choice);
        Ok(permission_result(id, opt.as_deref()))
    }

    async fn ask_user_permission(
        &mut self,
        tool_kind: &str,
        cmd: &str,
        reason: &str,
    ) -> Result<String, String> {
        if self.policy != "allow_all" {
            self.stream.finish_active(&self.sink, &self.session_id).await;
            self.emit_activity("waiting_permission", cmd).await;
        }
        Ok(ask_user_permission_via_sink(
            &self.sink,
            &self.pending_perms,
            &self.cancel,
            &self.session_id,
            &self.cwd,
            &self.policy,
            tool_kind,
            cmd,
            reason,
        )
        .await)
    }

    async fn handle_fs_read(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let raw_path = params["path"].as_str().unwrap_or("");
        if !permission::path_inside_root(raw_path, &self.cwd) {
            return Ok(json_err(id, -32602, "路径不在当前工作目录内"));
        }
        if permission::evaluate_policy(&self.policy, "fs/read_text_file", &[raw_path.to_string()], &self.cwd) != Some(true) {
            let choice = self
                .ask_user_permission("fs/read_text_file", raw_path, &format!("读取工作目录文件 {raw_path}"))
                .await?;
            if !choice.starts_with("allow") {
                return Ok(json_err(id, -32001, "用户拒绝读取"));
            }
            self.emit_activity("tooling", raw_path).await;
        }
        let target = if Path::new(raw_path).is_absolute() {
            PathBuf::from(raw_path)
        } else {
            self.cwd.join(raw_path)
        };
        if let Ok(meta) = tokio::fs::metadata(&target).await {
            if meta.len() > 2 * 1024 * 1024 {
                return Ok(json_err(id, -32002, "文件超过 2MB 上限，拒绝整文件读取"));
            }
        }
        match tokio::fs::read_to_string(&target).await {
            Ok(content) => Ok(json!({ "jsonrpc": "2.0", "id": id, "result": { "content": content } })),
            Err(e) => Ok(json_err(id, -32002, &format!("读取失败：{e}"))),
        }
    }

    async fn handle_fs_write(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let raw_path = params["path"].as_str().unwrap_or("");
        let content = params["content"].as_str().unwrap_or("");
        if !permission::path_inside_root(raw_path, &self.cwd) {
            return Ok(json_err(id, -32602, "路径不在当前工作目录内"));
        }
        if self.policy != "allow_all" {
            let choice = self
                .ask_user_permission("fs/write_text_file", raw_path, &format!("修改文件 {raw_path}"))
                .await?;
            if !choice.starts_with("allow") {
                return Ok(json_err(id, -32001, "用户拒绝写入"));
            }
            self.emit_activity("tooling", raw_path).await;
        }
        let target = if Path::new(raw_path).is_absolute() {
            PathBuf::from(raw_path)
        } else {
            self.cwd.join(raw_path)
        };
        if let Some(parent) = target.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&target, content.as_bytes()).await {
            Ok(()) => Ok(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            Err(e) => Ok(json_err(id, -32002, &format!("写入失败：{e}"))),
        }
    }

    async fn handle_term_create(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let prog = params["command"].as_str().unwrap_or("");
        let args: Vec<String> = params["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let display_cmd = if args.is_empty() {
            prog.to_string()
        } else {
            format!("{prog} {}", args.join(" "))
        };
        if self.policy != "allow_all" {
            let choice = self
                .ask_user_permission("terminal/run", &display_cmd, &format!("请求在 {} 执行命令", self.cwd.display()))
                .await?;
            if !choice.starts_with("allow") {
                return Ok(json_err(id, -32001, "用户拒绝执行终端命令"));
            }
        }
        self.emit_activity("tooling", &display_cmd).await;
        let mut cmd = command::prepare(prog, &args);
        cmd.current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command::configure_child_group(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Ok(json_err(id, -32002, &format!("启动命令失败：{e}"))),
        };
        let out_buf = Arc::new(Mutex::new(String::new()));
        if let Some(mut so) = child.stdout.take() {
            let b = out_buf.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut tmp = [0u8; 2048];
                while let Ok(n) = so.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    let mut g = b.lock().unwrap();
                    if g.len() < 32768 {
                        g.push_str(&String::from_utf8_lossy(&tmp[..n]));
                    }
                }
            });
        }
        if let Some(mut se) = child.stderr.take() {
            let b = out_buf.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut tmp = [0u8; 2048];
                while let Ok(n) = se.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    let mut g = b.lock().unwrap();
                    if g.len() < 32768 {
                        g.push_str(&String::from_utf8_lossy(&tmp[..n]));
                    }
                }
            });
        }
        let term_id = Uuid::new_v4().to_string();
        let (done_tx, mut done_rx) = watch::channel(false);
        let (exit_tx, exit_rx) = watch::channel::<Option<i32>>(None);
        let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
        let done_tx_reaper = done_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                st = child.wait() => {
                    let c = st.ok().and_then(|s| s.code()).unwrap_or(-1);
                    let _ = exit_tx.send(Some(c));
                    let _ = done_tx_reaper.send(true);
                }
                _ = &mut kill_rx => {
                    command::kill_child_tree(&mut child).await;
                    let c = child.try_wait().ok().flatten().and_then(|s| s.code()).unwrap_or(-1);
                    let _ = exit_tx.send(Some(c));
                    let _ = done_tx_reaper.send(true);
                }
            }
        });
        let tid_watch = term_id.clone();
        let out_watch = out_buf.clone();
        let sink_watch = self.sink.clone();
        let sess_watch = self.session_id.clone();
        let tt_watch = self.tool_terminals.clone();
        tokio::spawn(async move {
            let mut last_len = 0usize;
            let mut interval = tokio::time::interval(Duration::from_millis(150));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let tool_call_id = tt_watch.lock().unwrap().get(&tid_watch).cloned();
                        if let Some(tc_id) = tool_call_id {
                            let snap = {
                                let g = out_watch.lock().unwrap();
                                if g.len() > last_len {
                                    last_len = g.len();
                                    Some(g.clone())
                                } else {
                                    None
                                }
                            };
                            if let Some(out) = snap {
                                let _ = sink_watch.send(json!({
                                    "type": "session_update",
                                    "session_id": sess_watch,
                                    "kind": "tool_call",
                                    "tool_call_id": tc_id,
                                    "output": out,
                                })).await;
                            }
                        }
                    }
                    _ = done_rx.changed() => {
                        if *done_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        self.terminals.insert(
            term_id.clone(),
            ManagedTerminal {
                command: display_cmd,
                output: out_buf,
                exit_rx,
                kill_tx: Some(kill_tx),
                done_tx,
            },
        );
        Ok(json!({ "jsonrpc": "2.0", "id": id, "result": { "terminalId": term_id } }))
    }

    async fn handle_term_output(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let tid = params["terminalId"].as_str().unwrap_or("");
        let Some(term) = self.terminals.get(tid) else {
            return Ok(json_err(id, -32002, "unknown terminalId"));
        };
        let exit_code = *term.exit_rx.borrow();
        let out = term.output.lock().unwrap().clone();
        Ok(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "output": out,
                "truncated": false,
                "exitStatus": exit_code.map(|c| json!({ "exitCode": c }))
            }
        }))
    }

    async fn handle_term_wait(&mut self, id: Value, params: &Value) -> Result<Option<Value>, String> {
        let tid = params["terminalId"].as_str().unwrap_or("").to_string();
        let Some(term) = self.terminals.get(&tid) else {
            return Ok(Some(json_err(id, -32002, "unknown terminalId")));
        };
        let already = *term.exit_rx.borrow();
        if let Some(code) = already {
            let _ = term.done_tx.send(true);
            tokio::time::sleep(Duration::from_millis(20)).await;
            let out = term.output.lock().unwrap().clone();
            let cmd_str = term.command.clone();
            emit_terminal_final_via(
                &self.sink,
                &self.session_id,
                &self.tool_terminals,
                &tid,
                &cmd_str,
                &out,
                code,
            )
            .await;
            return Ok(Some(json!({ "jsonrpc": "2.0", "id": id, "result": { "exitCode": code } })));
        }

        let mut exit_rx = term.exit_rx.clone();
        let out_arc = term.output.clone();
        let cmd_str = term.command.clone();
        let tool_terminals = self.tool_terminals.clone();
        let sink = self.sink.clone();
        let session_id = self.session_id.clone();
        let outbound_tx = self.outbound_tx.clone();
        let mut cancel_rx = self.cancel.clone();
        tokio::spawn(async move {
            let code = loop {
                if let Some(c) = *exit_rx.borrow() {
                    break c;
                }
                if *cancel_rx.borrow() {
                    let _ = outbound_tx.send(json_err(id, -32000, "session cancelled"));
                    return;
                }
                tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            let _ = outbound_tx.send(json_err(id, -32000, "session cancelled"));
                            return;
                        }
                    }
                    res = exit_rx.changed() => {
                        if res.is_err() {
                            break (*exit_rx.borrow()).unwrap_or(-1);
                        }
                    }
                }
            };
            tokio::time::sleep(Duration::from_millis(20)).await;
            let out = out_arc.lock().unwrap().clone();
            emit_terminal_final_via(&sink, &session_id, &tool_terminals, &tid, &cmd_str, &out, code).await;
            let _ = outbound_tx.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "exitCode": code }
            }));
        });
        Ok(None)
    }

    fn handle_term_kill(&mut self, id: Value, params: &Value) -> Value {
        let tid = params["terminalId"].as_str().unwrap_or("");
        if let Some(mut t) = self.terminals.remove(tid) {
            let _ = t.done_tx.send(true);
            if let Some(ktx) = t.kill_tx.take() {
                let _ = ktx.send(());
            }
        }
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    }
}

async fn emit_terminal_final_via(
    sink: &mpsc::Sender<Value>,
    session_id: &str,
    tool_terminals: &Arc<Mutex<HashMap<String, String>>>,
    tid: &str,
    cmd_str: &str,
    out: &str,
    code: i32,
) {
    let bound_tool_id = tool_terminals.lock().unwrap().get(tid).cloned();
    if let Some(tc_id) = bound_tool_id {
        let _ = sink
            .send(json!({
                "type": "session_update",
                "session_id": session_id,
                "kind": "tool_call",
                "tool_call_id": tc_id,
                "output": out,
            }))
            .await;
    } else {
        let _ = sink
            .send(json!({
                "type": "session_update",
                "session_id": session_id,
                "kind": "terminal_out",
                "command": cmd_str,
                "output": out,
                "exit_code": code,
            }))
            .await;
    }
}

fn extract_acp_raw_input(raw: &Value) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    if raw.as_object().is_some_and(|o| o.is_empty())
        || raw.as_array().is_some_and(|a| a.is_empty())
    {
        return None;
    }
    serde_json::to_string_pretty(raw).ok().filter(|s| !s.is_empty())
}

fn extract_acp_raw_output(raw: &Value) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    if let Some(s) = raw.as_str() {
        return Some(s.to_string());
    }
    if let Some(s) = raw["content"].as_str().or_else(|| raw["output"].as_str()).or_else(|| raw["text"].as_str()) {
        return Some(s.to_string());
    }
    if raw["stdout"].is_string() || raw["stderr"].is_string() {
        let out = raw["stdout"].as_str().unwrap_or("");
        let err = raw["stderr"].as_str().unwrap_or("");
        let combined = match (out.is_empty(), err.is_empty()) {
            (false, false) => format!("{out}\n{err}"),
            (false, true) => out.to_string(),
            (true, false) => err.to_string(),
            (true, true) => String::new(),
        };
        return (!combined.is_empty()).then_some(combined);
    }
    serde_json::to_string_pretty(raw).ok().filter(|s| !s.is_empty())
}

fn extract_acp_locations(raw: &Value) -> Vec<String> {
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                return Some(s.to_string());
            }
            let path = item["path"].as_str()?.trim();
            if path.is_empty() {
                return None;
            }
            if let Some(line) = item["line"].as_u64() {
                Some(format!("{path}:{line}"))
            } else {
                Some(path.to_string())
            }
        })
        .collect()
}

fn extract_acp_usage(upd: &Value) -> Option<Value> {
    let mut obj = serde_json::Map::new();
    if let Some(v) = upd["used"].as_u64() {
        obj.insert("context_used".into(), json!(v));
    }
    if let Some(v) = upd["size"].as_u64() {
        obj.insert("context_size".into(), json!(v));
    }
    if let Some(v) = upd["inputTokens"].as_u64().or_else(|| upd["input_tokens"].as_u64()) {
        obj.insert("input_tokens".into(), json!(v));
    }
    if let Some(v) = upd["outputTokens"].as_u64().or_else(|| upd["output_tokens"].as_u64()) {
        obj.insert("output_tokens".into(), json!(v));
    }
    if let Some(v) = upd["totalTokens"].as_u64().or_else(|| upd["total_tokens"].as_u64()) {
        obj.insert("total_tokens".into(), json!(v));
    } else if let Some(v) = upd["used"].as_u64() {
        if !obj.contains_key("total_tokens") {
            obj.insert("total_tokens".into(), json!(v));
        }
    }
    if let Some(v) = upd["cachedReadTokens"]
        .as_u64()
        .or_else(|| upd["cacheRead"].as_u64())
        .or_else(|| upd["cache_read"].as_u64())
    {
        obj.insert("cache_read".into(), json!(v));
    }
    if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    }
}

fn permission_result(id: Value, option_id: Option<&str>) -> Value {
    match option_id {
        Some(oid) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "outcome": { "outcome": "selected", "optionId": oid } }
        }),
        None => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "outcome": { "outcome": "cancelled" } }
        }),
    }
}

fn json_err(id: Value, code: i32, msg: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": msg }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acp_client_drives_initialize_prompt_permission_and_plan() {
        let dir = std::env::temp_dir().join(format!("acp_drv_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(64 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(64 * 1024);

        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(32);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let pending_clone = pending_perms.clone();
        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            // 1. initialize
            let req1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req1["method"], "initialize");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":req1["id"],"result":{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":true}}}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 2. session/new
            let req2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req2["method"], "session/new");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":req2["id"],"result":{"sessionId":"acp-test-1"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 3. session/prompt
            let req3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req3["method"], "session/prompt");

            // Emit plan + ask permission + approve + chunk + done
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-test-1","update":{"sessionUpdate":"plan","entries":[{"content":"Run check","status":"in_progress"}]}}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"toolCall":{"title":"cargo check","kind":"execute","locations":[]},"options":[{"optionId":"allow_once","kind":"allow_once"},{"optionId":"reject_once","kind":"reject_once"}]}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let perm_resp: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(perm_resp["id"], 99);
            assert_eq!(perm_resp["result"]["outcome"]["optionId"], "allow_once");

            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-test-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"All green!"}}}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":req3["id"],"result":{"stopReason":"end_turn"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        // Auto-answer permission_request arriving on sink
        let responder = tokio::spawn(async move {
            let mut saw_plan = false;
            let mut saw_text = false;
            while let Some(ev) = sink_rx.recv().await {
                if ev["kind"] == "plan" {
                    saw_plan = true;
                } else if ev["kind"] == "text_delta" && ev["delta"] == "All green!" {
                    saw_text = true;
                } else if ev["kind"] == "permission_request" {
                    let req_id = ev["req_id"].as_str().unwrap().to_string();
                    let tx = pending_clone.lock().unwrap().remove(&req_id).unwrap();
                    let _ = tx.send("allow_once".to_string());
                }
            }
            (saw_plan, saw_text)
        });

        let msg = PromptStartMsg {
            run_id: None,
            session_id: "s-1".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig { id: None, preset: String::new(), cmd: "mock".into(), env: HashMap::new() },
            model: String::new(),
            thinking_level: "中".into(),
            permission_policy: "ask".into(),
            text: "hi".into(),
            files: vec![],
            attachments: vec![],
        };

        let out_key = drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
        )
        .await
        .unwrap();
        assert_eq!(out_key.as_deref(), Some("acp-test-1"));
        agent_task.await.unwrap();
        let (saw_plan, saw_text) = responder.await.unwrap();
        assert!(saw_plan && saw_text);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn probe_acp_stdio_prefers_config_options_and_deletes_temp_session() {
        let dir = std::env::temp_dir().join(format!("acp_probe_m_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(32 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(32 * 1024);

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            let req1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req1["method"], "initialize");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "jsonrpc": "2.0",
                            "id": req1["id"],
                            "result": {
                                "protocolVersion": 1,
                                "agentCapabilities": { "sessionCapabilities": { "delete": {} } }
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let req2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req2["method"], "session/new");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "jsonrpc": "2.0",
                            "id": req2["id"],
                            "result": {
                                "sessionId": "probe-sid-1",
                                "models": {
                                    "currentModelId": "xy-openai/gpt-6.2-astra[max]",
                                    "availableModels": [
                                        { "modelId": "xy-openai/gpt-6.2-astra[low]" },
                                        { "modelId": "xy-openai/gpt-6.2-astra[max]" }
                                    ]
                                },
                                "configOptions": [
                                    {
                                        "id": "model",
                                        "category": "model",
                                        "type": "select",
                                        "currentValue": "xy-openai/gpt-6.2-astra",
                                        "options": [
                                            { "value": "xy-open/glm-5.3", "name": "GLM-5.3" },
                                            { "value": "xy-openai/gpt-6.2-astra", "name": "6.2 Astra" }
                                        ]
                                    }
                                ]
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let req3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req3["method"], "session/delete");
            assert_eq!(req3["params"]["sessionId"], "probe-sid-1");
            agent_write
                .write_all(
                    format!("{}\n", json!({ "jsonrpc": "2.0", "id": req3["id"], "result": {} }))
                        .as_bytes(),
                )
                .await
                .unwrap();
        });

        let probed = probe_acp_stdio(client_read, client_write, &dir).await.unwrap();
        assert_eq!(
            probed.default_model.as_deref(),
            Some("xy-openai/gpt-6.2-astra")
        );
        assert_eq!(
            probed.model_options,
            vec!["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"]
        );
        agent_task.await.unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn acp_client_syncs_models_and_sets_config_options_for_model_and_thinking() {
        let dir = std::env::temp_dir().join(format!("acp_cfg_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(32 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(32 * 1024);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(32);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            let r1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r1["method"], "initialize");
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r1["id"],"result":{"protocolVersion":1}}))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r2["method"], "session/new");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "jsonrpc": "2.0",
                            "id": r2["id"],
                            "result": {
                                "sessionId": "acp-cfg-1",
                                "configOptions": [
                                    {
                                        "id": "model",
                                        "category": "model",
                                        "currentValue": "xy-openai/gpt-6.2-astra",
                                        "options": [
                                            { "value": "xy-open/glm-5.3", "name": "GLM-5.3" },
                                            { "value": "xy-openai/gpt-6.2-astra", "name": "6.2 Astra" }
                                        ]
                                    },
                                    {
                                        "id": "reasoning_effort",
                                        "category": "thought_level",
                                        "currentValue": "max",
                                        "options": [
                                            { "value": "low" },
                                            { "value": "high" },
                                            { "value": "max" }
                                        ]
                                    }
                                ]
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r3["method"], "session/set_config_option");
            assert_eq!(r3["params"]["configId"], "model");
            assert_eq!(r3["params"]["value"], "xy-open/glm-5.3");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "jsonrpc": "2.0",
                            "id": r3["id"],
                            "result": {
                                "configOptions": [
                                    {
                                        "id": "reasoning_effort",
                                        "category": "thought_level",
                                        "currentValue": "max",
                                        "options": [
                                            { "value": "low" },
                                            { "value": "high" }
                                        ]
                                    }
                                ]
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r4: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r4["method"], "session/set_config_option");
            assert_eq!(r4["params"]["configId"], "reasoning_effort");
            assert_eq!(r4["params"]["value"], "low");
            agent_write
                .write_all(
                    format!("{}\n", json!({ "jsonrpc": "2.0", "id": r4["id"], "result": {} }))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r5: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r5["method"], "session/prompt");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":r5["id"],"result":{"stopReason":"end_turn"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let msg = PromptStartMsg {
            run_id: None,
            session_id: "s-cfg".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig {
                id: Some("ag-123".into()),
                preset: "codex".into(),
                cmd: "codex-acp".into(),
                env: HashMap::new(),
            },
            model: "xy-open/glm-5.3".into(),
            thinking_level: "低".into(),
            permission_policy: "ask".into(),
            text: "ping".into(),
            files: vec![],
            attachments: vec![],
        };

        let out_key = drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
        )
        .await
        .unwrap();
        assert_eq!(out_key.as_deref(), Some("acp-cfg-1"));
        agent_task.await.unwrap();

        let mut synced_models: Option<Value> = None;
        while let Ok(ev) = sink_rx.try_recv() {
            if ev["type"] == "agent_models" {
                synced_models = Some(ev);
            }
        }
        let m_ev = synced_models.expect("should emit agent_models event");
        assert_eq!(m_ev["agent_id"], "ag-123");
        assert_eq!(m_ev["default_model"], "xy-openai/gpt-6.2-astra");
        assert_eq!(
            m_ev["model_options"],
            json!(["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"])
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn acp_client_streams_thoughts_tool_calls_diffs_usage_and_rotates_text_keys() {
        let dir = std::env::temp_dir().join(format!("acp_act_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(64 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(64 * 1024);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(64);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            let r1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":r1["id"],"result":{"protocolVersion":1}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":r2["id"],"result":{"sessionId":"acp-act-1"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r3["method"], "session/prompt");

            let updates = vec![
                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Analyzing..."}}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Let me edit."}}),
                json!({
                    "sessionUpdate":"tool_call",
                    "toolCallId":"tc-edit-1",
                    "title":"Edit lib.rs",
                    "name":"edit_file",
                    "kind":"edit",
                    "status":"in_progress",
                    "rawInput":{"path":"src/lib.rs"},
                    "locations":[{"path":"src/lib.rs","line":10}]
                }),
                json!({
                    "sessionUpdate":"tool_call_update",
                    "toolCallId":"tc-edit-1",
                    "status":"completed",
                    "content":[{"type":"diff","path":"src/lib.rs","oldText":"fn a() {}","newText":"fn b() {}"}],
                    "rawOutput":{"ok":true}
                }),
                json!({"sessionUpdate":"usage_update","used":400,"size":200000}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Updated."}}),
            ];
            for upd in updates {
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-act-1","update":upd}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }

            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":r3["id"],"result":{"stopReason":"end_turn"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let msg = PromptStartMsg {
            run_id: None,
            session_id: "s-act".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig {
                id: None,
                preset: "claude".into(),
                cmd: "claude-agent-acp".into(),
                env: HashMap::new(),
            },
            model: String::new(),
            thinking_level: "中".into(),
            permission_policy: "allow_all".into(),
            text: "edit".into(),
            files: vec![],
            attachments: vec![],
        };

        drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
        )
        .await
        .unwrap();
        agent_task.await.unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = sink_rx.try_recv() {
            events.push(ev);
        }

        assert!(events
            .iter()
            .any(|e| e["kind"] == "thought_delta" && e["delta"] == "Analyzing..."));
        assert!(events
            .iter()
            .any(|e| e["kind"] == "thought_delta" && e["done"] == true && e["duration_ms"].is_u64()));

        let text_starts: Vec<String> = events
            .iter()
            .filter(|e| e["kind"] == "text_delta" && e["done"] == false)
            .filter_map(|e| e["turn_key"].as_str().map(str::to_string))
            .collect();
        assert_eq!(text_starts.len(), 2);
        assert_ne!(text_starts[0], text_starts[1]);

        let tc_done = events
            .iter()
            .find(|e| e["kind"] == "tool_call" && e["status"] == "completed")
            .unwrap();
        assert_eq!(tc_done["tool_call_id"], "tc-edit-1");
        assert_eq!(tc_done["diffs"][0]["path"], "src/lib.rs");
        assert!(tc_done["duration_ms"].is_u64());

        assert!(events
            .iter()
            .any(|e| e["kind"] == "activity" && e["usage"]["context_used"] == 400));
        assert_eq!(
            events.last().unwrap()["phase"],
            "idle"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn acp_client_preserves_tool_title_on_sparse_update_and_leaves_tooling_when_done() {
        let dir = std::env::temp_dir().join(format!("acp_sp_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(32 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(32 * 1024);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(32);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            let r1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r1["id"],"result":{"protocolVersion":1}}))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r2["id"],"result":{"sessionId":"acp-sp-1"}}))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            for upd in [
                json!({
                    "sessionUpdate":"tool_call",
                    "toolCallId":"tc-sp-1",
                    "title":"bash: cargo test",
                    "name":"bash",
                    "kind":"execute",
                    "status":"in_progress"
                }),
                // 稀疏更新：不携带 title/name/status，只补 output
                json!({
                    "sessionUpdate":"tool_call_update",
                    "toolCallId":"tc-sp-1",
                    "content":[{"type":"content","text":"running..."}]
                }),
                json!({
                    "sessionUpdate":"tool_call_update",
                    "toolCallId":"tc-sp-1",
                    "status":"completed"
                }),
            ] {
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-sp-1","update":upd}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r3["id"],"result":{"stopReason":"end_turn"}}))
                        .as_bytes(),
                )
                .await
                .unwrap();
        });

        let msg = PromptStartMsg {
            run_id: None,
            session_id: "s-sp".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig {
                id: None,
                preset: "codex".into(),
                cmd: "codex-acp".into(),
                env: HashMap::new(),
            },
            model: String::new(),
            thinking_level: "中".into(),
            permission_policy: "allow_all".into(),
            text: "test".into(),
            files: vec![],
            attachments: vec![],
        };

        drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
        )
        .await
        .unwrap();
        agent_task.await.unwrap();

        let mut acts = Vec::new();
        while let Ok(ev) = sink_rx.try_recv() {
            if ev["kind"] == "activity" {
                acts.push((
                    ev["phase"].as_str().unwrap_or("").to_string(),
                    ev["detail"].as_str().unwrap_or("").to_string(),
                ));
            }
        }
        assert!(
            !acts.iter().any(|(_, d)| d == "执行工具中…"),
            "sparse update should not overwrite title with generic placeholder: {acts:?}"
        );
        let tooling_idx = acts
            .iter()
            .position(|(p, d)| p == "tooling" && d == "bash: cargo test")
            .expect("should report tooling with tool title");
        assert!(
            acts[tooling_idx + 1..]
                .iter()
                .any(|(p, _)| p == "connecting"),
            "should leave tooling phase once tool_call completes: {acts:?}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn acp_client_term_wait_does_not_block_cancel_or_kill() {
        let dir = std::env::temp_dir().join(format!("acp_tw_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let (client_read, mut agent_write) = tokio::io::duplex(32 * 1024);
        let (mut agent_read, client_write) = tokio::io::duplex(32 * 1024);
        let (sink_tx, _sink_rx) = mpsc::channel::<Value>(32);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let (cmd_prog, cmd_args) = if cfg!(windows) {
            ("cmd", vec!["/C", "ping", "-n", "15", "127.0.0.1"])
        } else {
            ("sh", vec!["-c", "sleep 15"])
        };

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut agent_read).lines();

            let r1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r1["id"],"result":{"protocolVersion":1}}))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r2["id"],"result":{"sessionId":"acp-tw-1"}}))
                        .as_bytes(),
                )
                .await
                .unwrap();

            let r3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":101,"method":"terminal/create","params":{"command":cmd_prog,"args":cmd_args}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let create_resp: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let tid = create_resp["result"]["terminalId"]
                .as_str()
                .unwrap()
                .to_string();

            // 发起阻塞性 terminal/wait_for_exit 后，立即紧接着发 terminal/kill，验证不会被 wait 卡死
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":102,"method":"terminal/wait_for_exit","params":{"terminalId":tid}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":103,"method":"terminal/kill","params":{"terminalId":tid}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let mut saw_kill = false;
            let mut saw_wait = false;
            while !(saw_kill && saw_wait) {
                let line = tokio::time::timeout(Duration::from_secs(4), lines.next_line())
                    .await
                    .expect("wait/kill responses should arrive promptly without deadlock")
                    .unwrap()
                    .unwrap();
                let resp: Value = serde_json::from_str(&line).unwrap();
                if resp["id"] == 103 {
                    saw_kill = true;
                } else if resp["id"] == 102 {
                    saw_wait = true;
                }
            }

            agent_write
                .write_all(
                    format!("{}\n", json!({"jsonrpc":"2.0","id":r3["id"],"result":{"stopReason":"end_turn"}}))
                        .as_bytes(),
                )
                .await
                .unwrap();
        });

        let msg = PromptStartMsg {
            run_id: None,
            session_id: "s-tw".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig {
                id: None,
                preset: "codex".into(),
                cmd: "codex-acp".into(),
                env: HashMap::new(),
            },
            model: String::new(),
            thinking_level: "中".into(),
            permission_policy: "allow_all".into(),
            text: "wait".into(),
            files: vec![],
            attachments: vec![],
        };

        tokio::time::timeout(
            Duration::from_secs(6),
            drive_stdio(
                client_read,
                client_write,
                &msg,
                &dir,
                sink_tx,
                pending_perms,
                perm_cache,
                cancel_rx,
            ),
        )
        .await
        .expect("drive_stdio should finish promptly without term wait deadlock")
        .unwrap();
        agent_task.await.unwrap();

        let _ = std::fs::remove_dir_all(dir);
    }
}
