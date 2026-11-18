use super::command;
use super::dirs::expand_tilde;
use super::permission::{self, PermissionCache};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

pub type PendingPerms = Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>;

#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentPayload {
    pub file_name: String,
    pub mime: String,
    pub data_base64: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub cmd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptStartMsg {
    pub session_id: String,
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
    child: tokio::process::Child,
    command: String,
    output: Arc<Mutex<String>>,
    exit_code: Option<i32>,
}

struct Driver<W> {
    session_id: String,
    cwd: PathBuf,
    policy: String,
    writer: W,
    rx: mpsc::UnboundedReceiver<Option<Value>>,
    next_id: u64,
    suppress_chunks: bool,
    startup_text: Option<String>,
    text_turn_key: String,
    plan_run_key: String,
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

    let mut cmd = command::prepare(&prog, &args);
    cmd.current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command::configure_child_group(&mut cmd);
    for (k, v) in &msg.agent.env {
        cmd.env(k, v);
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

    let upload_dir = session_upload_dir(&msg.session_id);
    let res = drive_stdio(
        stdout,
        stdin,
        &msg,
        &dir,
        sink.clone(),
        pending_perms,
        perm_cache,
        cancel,
    )
    .await;

    command::kill_child_tree(&mut child).await;
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
        next_id: 1,
        suppress_chunks: true,
        startup_text: None,
        text_turn_key: Uuid::new_v4().to_string(),
        plan_run_key: Uuid::new_v4().to_string(),
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

    let mut acp_sid: Option<String> = None;
    let mut avail_models: Vec<String> = Vec::new();

    if let Some(prev) = msg.acp_session_key.as_deref().filter(|s| !s.is_empty()) {
        let resume_params = json!({ "sessionId": prev, "cwd": dir, "mcpServers": [] });
        if caps.pointer("/sessionCapabilities/resume").is_some_and(|v| !v.is_null() && *v != json!(false)) {
            if let Ok(r) = drv.call("session/resume", resume_params.clone()).await {
                acp_sid = r["sessionId"].as_str().map(str::to_string).or_else(|| Some(prev.to_string()));
            }
        } else if caps.get("loadSession").and_then(Value::as_bool) == Some(true) {
            if let Ok(r) = drv.call("session/load", resume_params).await {
                acp_sid = r["sessionId"].as_str().map(str::to_string).or_else(|| Some(prev.to_string()));
            }
        }
    }

    let resolved_sid = if let Some(sid) = acp_sid {
        sid
    } else {
        let new_res = drv
            .call("session/new", json!({ "cwd": dir, "mcpServers": [] }))
            .await?;
        drv.log_rpc("out", "session/new", &format!("cwd: {}", dir.display())).await;
        drv.startup_text = new_res
            .get("_meta")
            .and_then(Value::as_object)
            .and_then(|o| o.values().find_map(|v| v.get("startupInfo")?.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(arr) = new_res.pointer("/models/availableModels").and_then(Value::as_array) {
            for item in arr {
                if let Some(m) = item["modelId"].as_str().or_else(|| item["id"].as_str()) {
                    avail_models.push(m.to_string());
                }
            }
        }
        new_res["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Agent 未返回 sessionId".to_string())?
            .to_string()
    };

    if !msg.model.is_empty() && (avail_models.is_empty() || avail_models.contains(&msg.model)) {
        let _ = drv
            .call(
                "session/set_model",
                json!({ "sessionId": resolved_sid, "modelId": msg.model }),
            )
            .await;
    }

    let blocks = build_prompt_blocks(msg, dir, image_ok);
    drv.suppress_chunks = false;
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
        command::kill_child_tree(&mut term.child).await;
    }
    let cancelled = cancelled?;
    if cancelled {
        let _ = drv
            .notify("session/cancel", json!({ "sessionId": resolved_sid }))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(4), drv.wait_resp(prompt_req_id)).await;
    }

    // 把最后一条文本收尾标成 done
    Ok(Some(resolved_sid))
}

fn session_upload_dir(session_id: &str) -> Option<PathBuf> {
    let parsed = Uuid::parse_str(session_id.trim()).ok()?;
    Some(super::host_data_dir().join("uploads").join(parsed.to_string()))
}

fn restrict_dir_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn restrict_file_owner(path: &Path) {
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
    let upload_dir = session_upload_dir(&msg.session_id);
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

    async fn wait_resp(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let msg = match self.rx.recv().await {
                Some(Some(v)) => v,
                _ => return Err("Agent 进程意外退出".to_string()),
            };
            if let Some(res) = self.take_msg(id, msg).await? {
                return res;
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
            tokio::select! {
                biased;
                changed = cancel.changed() => {
                    if changed.is_ok() && *cancel.borrow() {
                        return Ok(true);
                    }
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
                    let _ = self
                        .sink
                        .send(json!({
                            "type": "session_update",
                            "session_id": self.session_id,
                            "kind": "text_delta",
                            "turn_key": self.text_turn_key,
                            "delta": t,
                            "done": false,
                        }))
                        .await;
                }
            }
            Some("plan") => {
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
            _ => {}
        }
    }

    async fn answer_request(&mut self, msg: &Value) -> Result<(), String> {
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("");
        let params = &msg["params"];

        let resp = match method {
            "session/request_permission" => self.handle_permission_req(id, params).await?,
            "fs/read_text_file" => self.handle_fs_read(id, params).await?,
            "fs/write_text_file" => self.handle_fs_write(id, params).await?,
            "terminal/create" => self.handle_term_create(id, params).await?,
            "terminal/output" => self.handle_term_output(id, params).await?,
            "terminal/wait_for_exit" => self.handle_term_wait(id, params).await?,
            "terminal/kill" => self.handle_term_kill(id, params),
            "terminal/release" => {
                if let Some(tid) = params["terminalId"].as_str() {
                    if let Some(mut t) = self.terminals.remove(tid) {
                        let _ = t.child.kill().await;
                    }
                }
                json!({ "jsonrpc": "2.0", "id": id, "result": {} })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unsupported method: {method}") }
            }),
        };
        self.write_line(&resp).await
    }

    async fn handle_permission_req(&self, id: Value, params: &Value) -> Result<Value, String> {
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
        if paths_in_scope {
            self.perm_cache.lock().unwrap().record(key, &choice);
        }
        let opt = permission::pick_option_id(&options, &choice);
        Ok(permission_result(id, opt.as_deref()))
    }

    async fn ask_user_permission(
        &self,
        tool_kind: &str,
        cmd: &str,
        reason: &str,
    ) -> Result<String, String> {
        if self.policy == "allow_all" {
            return Ok("allow_once".to_string());
        }
        let req_id = Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel::<String>();
        self.pending_perms.lock().unwrap().insert(req_id.clone(), tx);

        let _ = self
            .sink
            .send(json!({
                "type": "session_update",
                "session_id": self.session_id,
                "kind": "permission_request",
                "req_id": req_id,
                "tool_kind": tool_kind,
                "command": cmd,
                "cwd": self.cwd.to_string_lossy(),
                "reason": reason,
            }))
            .await;

        let mut cancel_rx = self.cancel.clone();
        loop {
            if *cancel_rx.borrow() {
                self.pending_perms.lock().unwrap().remove(&req_id);
                return Ok("reject_once".to_string());
            }
            tokio::select! {
                biased;
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        self.pending_perms.lock().unwrap().remove(&req_id);
                        return Ok("reject_once".to_string());
                    }
                }
                res = &mut rx => {
                    self.pending_perms.lock().unwrap().remove(&req_id);
                    return Ok(res.unwrap_or_else(|_| "reject_once".to_string()));
                }
            }
        }
    }

    async fn handle_fs_read(&self, id: Value, params: &Value) -> Result<Value, String> {
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

    async fn handle_fs_write(&self, id: Value, params: &Value) -> Result<Value, String> {
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
        let mut cmd = command::prepare(prog, &args);
        cmd.current_dir(&self.cwd)
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
        self.terminals.insert(
            term_id.clone(),
            ManagedTerminal {
                child,
                command: display_cmd,
                output: out_buf,
                exit_code: None,
            },
        );
        Ok(json!({ "jsonrpc": "2.0", "id": id, "result": { "terminalId": term_id } }))
    }

    async fn handle_term_output(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let tid = params["terminalId"].as_str().unwrap_or("");
        let Some(term) = self.terminals.get_mut(tid) else {
            return Ok(json_err(id, -32002, "unknown terminalId"));
        };
        if term.exit_code.is_none() {
            if let Ok(Some(st)) = term.child.try_wait() {
                term.exit_code = Some(st.code().unwrap_or(-1));
            }
        }
        let out = term.output.lock().unwrap().clone();
        Ok(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "output": out,
                "truncated": false,
                "exitStatus": term.exit_code.map(|c| json!({ "exitCode": c }))
            }
        }))
    }

    async fn handle_term_wait(&mut self, id: Value, params: &Value) -> Result<Value, String> {
        let tid = params["terminalId"].as_str().unwrap_or("");
        let Some(term) = self.terminals.get_mut(tid) else {
            return Ok(json_err(id, -32002, "unknown terminalId"));
        };
        let code = if let Some(c) = term.exit_code {
            c
        } else {
            let st = term.child.wait().await.ok();
            let c = st.and_then(|s| s.code()).unwrap_or(-1);
            term.exit_code = Some(c);
            c
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let out = term.output.lock().unwrap().clone();
        let cmd_str = term.command.clone();
        let _ = self
            .sink
            .send(json!({
                "type": "session_update",
                "session_id": self.session_id,
                "kind": "terminal_out",
                "command": cmd_str,
                "output": out,
                "exit_code": code,
            }))
            .await;
        Ok(json!({ "jsonrpc": "2.0", "id": id, "result": { "exitCode": code } }))
    }

    fn handle_term_kill(&mut self, id: Value, params: &Value) -> Value {
        let tid = params["terminalId"].as_str().unwrap_or("");
        if let Some(mut t) = self.terminals.remove(tid) {
            tokio::spawn(async move {
                command::kill_child_tree(&mut t.child).await;
            });
        }
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
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
            session_id: "s-1".into(),
            acp_session_key: None,
            cwd: dir.to_string_lossy().into(),
            agent: AgentConfig { cmd: "mock".into(), env: HashMap::new() },
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
}
