use super::acp_client::{
    ask_user_permission_via_sink, emit_agent_models, restrict_dir_owner, restrict_file_owner,
    session_upload_dir, PendingPerms, ProbedModels, PromptStartMsg,
};
use super::permission::{self, PermissionCache};
use super::stream_buf::{BlockKind, StreamCoalescer};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use uuid::Uuid;

const PERMISSION_GATE_FILENAME: &str = "pi-permission-gate.mjs";
const PERMISSION_GATE_SCRIPT: &str = r#"export default function (pi) {
  const token = process.env.CHAT_PI_GATE_TOKEN || "";
  if (!token) return;
  const marker = `__CHAT_PERM_GATE__${token}`;
  pi.on("tool_call", async (event, ctx) => {
    if (!ctx.hasUI) {
      return { block: true, reason: "权限校验未连接前端交互通道" };
    }
    const payload = JSON.stringify({
      toolName: event.toolName ?? "",
      input: event.input ?? {},
    });
    const ok = await ctx.ui.confirm(marker, payload);
    if (!ok) {
      return { block: true, reason: "用户拒绝执行此工具操作" };
    }
    return undefined;
  });
}
"#;

pub fn is_pi_rpc_invocation(preset: &str, prog: &str, args: &[String]) -> bool {
    let Some(stem) = normalized_prog_stem(prog) else {
        return false;
    };
    if stem != "pi" {
        return false;
    }
    if has_mode_rpc(args) {
        return true;
    }
    if preset == "pi" {
        return !has_non_rpc_mode(args);
    }
    false
}

fn normalized_prog_stem(prog: &str) -> Option<String> {
    let name = Path::new(prog.trim()).file_name()?.to_string_lossy();
    if cfg!(windows) {
        let low = name.to_ascii_lowercase();
        for ext in [".cmd", ".exe", ".bat"] {
            if let Some(stem) = low.strip_suffix(ext) {
                return Some(stem.to_string());
            }
        }
        Some(low)
    } else {
        Some(name.to_string())
    }
}

fn has_mode_rpc(args: &[String]) -> bool {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--mode=rpc" {
            return true;
        }
        if arg == "--mode" && iter.peek().is_some_and(|next| next.as_str() == "rpc") {
            return true;
        }
    }
    false
}

fn has_non_rpc_mode(args: &[String]) -> bool {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--acp" {
            return true;
        }
        if let Some(val) = arg.strip_prefix("--mode=") {
            if val != "rpc" {
                return true;
            }
        } else if arg == "--mode" {
            if let Some(next) = iter.peek() {
                if next.as_str() != "rpc" {
                    return true;
                }
            }
        }
    }
    false
}

fn has_session_flag(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--session" || a.starts_with("--session=") || a == "--no-session")
}

pub fn ensure_permission_gate_script(data_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
    let path = data_dir.join(PERMISSION_GATE_FILENAME);
    let unchanged = std::fs::read_to_string(&path)
        .ok()
        .is_some_and(|cur| cur == PERMISSION_GATE_SCRIPT);
    if !unchanged {
        std::fs::write(&path, PERMISSION_GATE_SCRIPT)
            .map_err(|e| format!("写入 Pi 权限扩展脚本失败：{e}"))?;
        restrict_file_owner(&path);
    }
    Ok(path)
}

pub fn prepare_launch_args(
    base_args: &[String],
    msg: &PromptStartMsg,
    data_dir: &Path,
) -> Result<(Vec<String>, Option<String>), String> {
    let mut out = base_args.to_vec();
    if !has_mode_rpc(&out) {
        out.push("--mode".to_string());
        out.push("rpc".to_string());
    }

    if !has_session_flag(&out) {
        if let Some(prev) = msg
            .acp_session_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let looks_like_path = prev.contains('/') || prev.contains('\\');
            if !looks_like_path || Path::new(prev).is_file() {
                out.push("--session".to_string());
                out.push(prev.to_string());
            }
        }
    }

    let policy = effective_policy(&msg.permission_policy);
    let gate_token = if policy != "allow_all" {
        let gate_path = ensure_permission_gate_script(data_dir)?;
        out.push("-e".to_string());
        out.push(gate_path.to_string_lossy().to_string());
        Some(Uuid::new_v4().to_string())
    } else {
        None
    };

    Ok((out, gate_token))
}

pub fn prepare_probe_args(base_args: &[String]) -> Vec<String> {
    let mut out = base_args.to_vec();
    if !has_mode_rpc(&out) {
        out.push("--mode".to_string());
        out.push("rpc".to_string());
    }
    if !has_session_flag(&out) {
        out.push("--no-session".to_string());
    }
    out
}

fn effective_policy(raw: &str) -> &str {
    if raw.is_empty() {
        "auto_read"
    } else {
        raw
    }
}

struct PiToolMeta {
    name: String,
    kind: String,
    title: String,
    last_partial_emit: Option<Instant>,
}

struct PiDriver<W> {
    session_id: String,
    cwd: PathBuf,
    policy: String,
    writer: W,
    rx: mpsc::UnboundedReceiver<Option<Value>>,
    next_id: u64,
    stream: StreamCoalescer,
    tool_starts: HashMap<String, Instant>,
    tool_meta: HashMap<String, PiToolMeta>,
    emitted_thought_in_msg: bool,
    emitted_text_in_msg: bool,
    emitted_any_text: bool,
    last_error: Option<String>,
    last_activity: (String, String),
    last_usage_emit: Option<Instant>,
    gate_marker: Option<String>,
    sink: mpsc::Sender<Value>,
    pending_perms: PendingPerms,
    perm_cache: Arc<Mutex<PermissionCache>>,
    cancel: watch::Receiver<bool>,
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
    gate_token: Option<&str>,
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

    let mut drv = PiDriver {
        session_id: msg.session_id.clone(),
        cwd: dir.to_path_buf(),
        policy: effective_policy(&msg.permission_policy).to_string(),
        writer,
        rx,
        next_id: 1,
        stream: StreamCoalescer::new(),
        tool_starts: HashMap::new(),
        tool_meta: HashMap::new(),
        emitted_thought_in_msg: false,
        emitted_text_in_msg: false,
        emitted_any_text: false,
        last_error: None,
        last_activity: (String::new(), String::new()),
        last_usage_emit: None,
        gate_marker: gate_token.map(|t| format!("__CHAT_PERM_GATE__{t}")),
        sink,
        pending_perms,
        perm_cache,
        cancel,
    };

    // A. 读取当前会话状态
    let state_resp = drv.call(json!({ "type": "get_state" })).await?;
    drv.log_rpc("out", "pi/get_state", "Pi RPC handshake").await;
    let mut session_key = extract_session_key(&state_resp["data"])
        .or_else(|| msg.acp_session_key.clone().filter(|s| !s.is_empty()));
    let current_thinking = state_resp["data"]["thinkingLevel"]
        .as_str()
        .map(str::to_string);

    // B. 按用户选择切换模型
    let want_model = msg.model.trim();
    if !want_model.is_empty() && !is_default_model_placeholder(want_model) {
        let models_resp = drv
            .call(json!({ "type": "get_available_models" }))
            .await
            .ok()
            .filter(|r| r["success"].as_bool() == Some(true));
        if let Some(ref mr) = models_resp {
            let catalog = extract_pi_model_catalog(&state_resp["data"], &mr["data"]["models"]);
            emit_agent_models(
                &drv.sink,
                msg.agent.id.as_deref(),
                Some(&msg.session_id),
                &catalog,
            )
            .await;
        }
        let matched = models_resp
            .as_ref()
            .and_then(|r| match_pi_model(&r["data"]["models"], want_model));
        let target = matched.or_else(|| {
            want_model
                .split_once('/')
                .filter(|(p, m)| !p.is_empty() && !m.is_empty())
                .map(|(p, m)| (p.to_string(), m.to_string()))
        });
        if let Some((provider, model_id)) = target {
            if let Ok(r) = drv
                .call(json!({
                    "type": "set_model",
                    "provider": provider,
                    "modelId": model_id,
                }))
                .await
            {
                if r["success"].as_bool() == Some(true) {
                    drv.log_rpc("out", "pi/set_model", &format!("{provider}/{model_id}"))
                        .await;
                }
            }
        }
    }

    // C. 按用户选择切换思考深度
    let candidates = thinking_candidates(&msg.thinking_level);
    if !candidates.is_empty() {
        let avail_levels: Vec<String> = drv
            .call(json!({ "type": "get_available_thinking_levels" }))
            .await
            .ok()
            .filter(|r| r["success"].as_bool() == Some(true))
            .and_then(|r| r["data"]["levels"].as_array().cloned())
            .map(|arr| {
                arr.into_iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let chosen = if avail_levels.is_empty() {
            candidates.first().copied()
        } else {
            candidates
                .iter()
                .copied()
                .find(|c| avail_levels.iter().any(|l| l.eq_ignore_ascii_case(c)))
        };
        if let Some(level) = chosen {
            if current_thinking.as_deref() != Some(level) {
                if let Ok(r) = drv
                    .call(json!({ "type": "set_thinking_level", "level": level }))
                    .await
                {
                    if r["success"].as_bool() == Some(true) {
                        drv.log_rpc("out", "pi/set_thinking_level", level).await;
                    }
                }
            }
        }
    }

    // D. 构造并发送 prompt
    let (message, images) = build_prompt_payload(msg, dir);
    let image_count = images.len();
    let mut prompt_cmd = json!({
        "type": "prompt",
        "message": message,
    });
    if !images.is_empty() {
        prompt_cmd["images"] = Value::Array(images);
    }
    drv.log_rpc("out", "pi/prompt", &format!("images: {image_count}"))
        .await;
    let prompt_resp = drv.call(prompt_cmd).await?;
    if prompt_resp["success"].as_bool() != Some(true) {
        let err = prompt_resp["error"]
            .as_str()
            .unwrap_or("Pi 执行失败")
            .to_string();
        return Err(err);
    }
    if prompt_resp["data"]["disposition"].as_str() == Some("handled") {
        drv.emit_activity("idle", "", None).await;
        return Ok(session_key);
    }

    drv.emit_activity("connecting", "等待 Agent 响应…", None).await;

    // E. 消费流式事件直至 agent_settled 或取消
    let mut cancel_rx = drv.cancel.clone();
    let mut cancelled = false;
    loop {
        if *cancel_rx.borrow() {
            cancelled = true;
            break;
        }
        let deadline = drv.stream.next_flush_deadline();
        tokio::select! {
            biased;
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    cancelled = true;
                    break;
                }
            }
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                drv.stream.flush_due(&drv.sink, &drv.session_id).await;
            }
            ev = drv.rx.recv() => {
                match ev {
                    Some(Some(v)) => {
                        if v["type"].as_str() == Some("agent_settled") {
                            break;
                        }
                        drv.handle_event(&v).await?;
                    }
                    Some(None) | None => {
                        drv.stream.finish_active(&drv.sink, &drv.session_id).await;
                        drv.emit_activity("idle", "", None).await;
                        return Err("Pi 进程意外退出".to_string());
                    }
                }
            }
        }
    }

    if cancelled {
        let abort_id = drv.alloc_id();
        let _ = drv
            .write_line(&json!({ "id": abort_id, "type": "abort" }))
            .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            match tokio::time::timeout_at(deadline, drv.rx.recv()).await {
                Ok(Some(Some(v))) => {
                    if v["type"].as_str() == Some("agent_settled") {
                        break;
                    }
                    let _ = drv.handle_event(&v).await;
                }
                _ => break,
            }
        }
    } else if !session_key
        .as_deref()
        .is_some_and(|s| s.ends_with(".jsonl") || Path::new(s).is_file())
    {
        if let Ok(Ok(st)) =
            tokio::time::timeout(Duration::from_secs(1), drv.call(json!({ "type": "get_state" })))
                .await
        {
            if st["success"].as_bool() == Some(true) {
                session_key = extract_session_key(&st["data"]).or(session_key);
            }
        }
    }

    drv.stream.finish_active(&drv.sink, &drv.session_id).await;
    drv.emit_activity("idle", "", None).await;

    if let Some(err) = drv.last_error.take() {
        if !drv.emitted_any_text && !cancelled {
            return Err(err);
        }
    }

    // 显式关闭 stdin 通知 Pi 有序刷盘并退出
    drop(drv.writer);
    Ok(session_key)
}

fn extract_session_key(data: &Value) -> Option<String> {
    data["sessionFile"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            data["sessionId"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

pub(super) async fn probe_stdio<R, W>(reader: R, writer: W) -> Result<ProbedModels, String>
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
    let mut drv = PiDriver {
        session_id: String::new(),
        cwd: PathBuf::new(),
        policy: "ask".to_string(),
        writer,
        rx,
        next_id: 1,
        stream: StreamCoalescer::new(),
        tool_starts: HashMap::new(),
        tool_meta: HashMap::new(),
        emitted_thought_in_msg: false,
        emitted_text_in_msg: false,
        emitted_any_text: false,
        last_error: None,
        last_activity: (String::new(), String::new()),
        last_usage_emit: None,
        gate_marker: None,
        sink: dummy_sink,
        pending_perms: Arc::new(Mutex::new(HashMap::new())),
        perm_cache: Arc::new(Mutex::new(PermissionCache::default())),
        cancel: cancel_rx,
    };

    let state_resp = drv
        .call(json!({ "type": "get_state" }))
        .await
        .unwrap_or(Value::Null);
    let models_resp = drv.call(json!({ "type": "get_available_models" })).await?;
    drop(drv.writer);
    if models_resp["success"].as_bool() != Some(true) {
        let err = models_resp["error"]
            .as_str()
            .unwrap_or("Pi 返回模型列表失败")
            .to_string();
        return Err(err);
    }
    Ok(extract_pi_model_catalog(
        &state_resp["data"],
        &models_resp["data"]["models"],
    ))
}

pub(super) fn extract_pi_model_catalog(state_data: &Value, models: &Value) -> ProbedModels {
    let mut opts = Vec::new();
    if let Some(arr) = models.as_array() {
        for m in arr {
            let provider = m["provider"].as_str().unwrap_or("").trim();
            let id = m["id"].as_str().unwrap_or("").trim();
            if id.is_empty() {
                continue;
            }
            let full = if provider.is_empty() {
                id.to_string()
            } else {
                format!("{provider}/{id}")
            };
            if !opts.iter().any(|x| x == &full) {
                opts.push(full);
            }
        }
    }

    let cur = state_data["model"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let obj = state_data["model"].as_object()?;
            let p = obj.get("provider")?.as_str()?.trim();
            let id = obj.get("id")?.as_str()?.trim();
            if id.is_empty() {
                None
            } else if p.is_empty() {
                Some(id.to_string())
            } else {
                Some(format!("{p}/{id}"))
            }
        });

    if let Some(ref c) = cur {
        if !opts.iter().any(|x| x == c) {
            opts.insert(0, c.clone());
        }
    }
    let default_model = if opts.is_empty() {
        None
    } else {
        cur.or_else(|| opts.first().cloned())
    };
    ProbedModels {
        default_model,
        model_options: opts,
    }
}

pub(super) fn is_default_model_placeholder(m: &str) -> bool {
    matches!(
        m.trim().to_ascii_lowercase().as_str(),
        "default model"
            | "default"
            | "默认"
            | "默认模型"
            | "gpt-5 codex"
            | "o3-pro"
            | "gpt-4.1"
            | "claude sonnet 4.5"
            | "claude opus 4.1"
            | "claude haiku 4.5"
            | "gemini 2.5 pro"
            | "gemini 2.5 flash"
    )
}

fn match_pi_model(models: &Value, want: &str) -> Option<(String, String)> {
    let arr = models.as_array()?;
    let entries: Vec<(&str, &str, &str)> = arr
        .iter()
        .filter_map(|m| {
            let provider = m["provider"].as_str()?;
            let id = m["id"].as_str()?;
            let name = m["name"].as_str().unwrap_or("");
            Some((provider, id, name))
        })
        .collect();

    if let Some(&(p, id, _)) = entries
        .iter()
        .find(|&&(p, id, _)| format!("{p}/{id}").eq_ignore_ascii_case(want))
    {
        return Some((p.to_string(), id.to_string()));
    }
    if let Some(&(p, id, _)) = entries.iter().find(|&&(_, id, _)| id.eq_ignore_ascii_case(want)) {
        return Some((p.to_string(), id.to_string()));
    }
    entries
        .iter()
        .find(|&&(_, _, name)| !name.is_empty() && name.eq_ignore_ascii_case(want))
        .map(|&(p, id, _)| (p.to_string(), id.to_string()))
}

fn thinking_candidates(raw: &str) -> Vec<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "关" | "off" => vec!["off", "minimal"],
        "低" | "low" => vec!["low", "minimal", "medium"],
        "中" | "medium" => vec!["medium", "high", "low"],
        "高" | "high" => vec!["high", "xhigh", "max", "medium"],
        "minimal" => vec!["minimal", "low"],
        "xhigh" => vec!["xhigh", "high", "max"],
        "max" => vec!["max", "xhigh", "high"],
        _ => Vec::new(),
    }
}

fn build_prompt_payload(msg: &PromptStartMsg, dir: &Path) -> (String, Vec<Value>) {
    let mut message = msg.text.clone();
    let mut file_refs = Vec::new();
    for f in &msg.files {
        let clean = f.trim().trim_start_matches('@');
        if !clean.is_empty() && permission::path_inside_root(clean, dir) {
            let path = if Path::new(clean).is_absolute() {
                PathBuf::from(clean)
            } else {
                dir.join(clean)
            };
            file_refs.push(path.display().to_string());
        }
    }
    if !file_refs.is_empty() {
        if !message.is_empty() {
            message.push_str("\n\n");
        }
        message.push_str("引用文件：");
        for r in file_refs {
            message.push_str("\n- ");
            message.push_str(&r);
        }
    }

    let mut images = Vec::new();
    let upload_dir = session_upload_dir(msg);
    for att in &msg.attachments {
        let Some(b64) = &att.data_base64 else {
            continue;
        };
        if att.mime.starts_with("image/") {
            images.push(json!({
                "type": "image",
                "data": b64,
                "mimeType": att.mime,
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
                if !message.is_empty() {
                    message.push('\n');
                }
                message.push_str(&format!("[用户上传附件：{}]", dest.display()));
            }
        }
    }

    (message, images)
}

impl<W: AsyncWrite + Unpin + Send> PiDriver<W> {
    fn alloc_id(&mut self) -> String {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        id
    }

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
            .map_err(|e| format!("写入 Pi stdin 失败：{e}"))?;
        let _ = self.writer.flush().await;
        Ok(())
    }

    async fn emit_activity(&mut self, phase: &str, detail: &str, usage: Option<Value>) {
        let phase_s = phase.trim().to_string();
        let detail_s = detail.trim().to_string();
        if usage.is_none()
            && self.last_activity.0 == phase_s
            && self.last_activity.1 == detail_s
        {
            return;
        }
        self.last_activity = (phase_s.clone(), detail_s.clone());
        let mut ev = json!({
            "type": "session_update",
            "session_id": self.session_id,
            "kind": "activity",
            "phase": phase_s,
            "detail": detail_s,
        });
        if let Some(u) = usage {
            ev["usage"] = u;
        }
        let _ = self.sink.send(ev).await;
    }

    async fn emit_pi_usage(&mut self, usage: &Value, force: bool) {
        if !force
            && self
                .last_usage_emit
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        let Some(u) = extract_pi_usage(usage) else {
            return;
        };
        self.last_usage_emit = Some(Instant::now());
        let phase = if self.last_activity.0.is_empty() {
            "responding".to_string()
        } else {
            self.last_activity.0.clone()
        };
        let detail = self.last_activity.1.clone();
        self.emit_activity(&phase, &detail, Some(u)).await;
    }

    async fn call(&mut self, mut cmd: Value) -> Result<Value, String> {
        let id = self.alloc_id();
        cmd["id"] = Value::String(id.clone());
        self.write_line(&cmd).await?;

        let mut cancel_rx = self.cancel.clone();
        loop {
            if *cancel_rx.borrow() {
                return Err("已取消执行".to_string());
            }
            let deadline = self.stream.next_flush_deadline();
            tokio::select! {
                biased;
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        return Err("已取消执行".to_string());
                    }
                }
                _ = async {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.stream.flush_due(&self.sink, &self.session_id).await;
                }
                ev = self.rx.recv() => {
                    match ev {
                        Some(Some(v)) => {
                            if v["type"].as_str() == Some("response")
                                && v["id"].as_str() == Some(id.as_str())
                            {
                                return Ok(v);
                            }
                            self.handle_event(&v).await?;
                        }
                        Some(None) | None => return Err("Pi 进程意外退出".to_string()),
                    }
                }
            }
        }
    }

    async fn handle_event(&mut self, v: &Value) -> Result<(), String> {
        match v["type"].as_str() {
            Some("message_start") => {
                if v["message"]["role"].as_str() == Some("assistant") {
                    self.emitted_thought_in_msg = false;
                    self.emitted_text_in_msg = false;
                }
            }
            Some("message_update") => {
                let ame = &v["assistantMessageEvent"];
                let msg_id = v["message"]["id"].as_str();
                let c_idx = ame["contentIndex"].as_u64();
                match ame["type"].as_str() {
                    Some("thinking_start") => {
                        self.emit_activity("thinking", "正在思考…", None).await;
                    }
                    Some("thinking_delta") => {
                        if let Some(delta) = ame["delta"].as_str().filter(|s| !s.is_empty()) {
                            self.emitted_thought_in_msg = true;
                            self.emit_activity("thinking", "正在思考…", None).await;
                            self.stream
                                .push_thought(delta, msg_id, c_idx, &self.sink, &self.session_id)
                                .await;
                        }
                    }
                    Some("thinking_end") => {
                        let content = ame["content"].as_str().filter(|s| !s.is_empty());
                        if content.is_some() {
                            self.emitted_thought_in_msg = true;
                        }
                        if content.is_some() || self.emitted_thought_in_msg {
                            self.stream
                                .finish_kind(
                                    BlockKind::Thought,
                                    content,
                                    &self.sink,
                                    &self.session_id,
                                )
                                .await;
                        }
                    }
                    Some("text_start") => {
                        self.emit_activity("responding", "正在生成回复…", None).await;
                    }
                    Some("text_delta") => {
                        if let Some(delta) = ame["delta"].as_str().filter(|s| !s.is_empty()) {
                            self.emitted_text_in_msg = true;
                            self.emitted_any_text = true;
                            self.emit_activity("responding", "正在生成回复…", None).await;
                            self.stream
                                .push_text(delta, msg_id, c_idx, &self.sink, &self.session_id)
                                .await;
                        }
                    }
                    Some("text_end") => {
                        let content = ame["content"].as_str().filter(|s| !s.is_empty());
                        if content.is_some() {
                            self.emitted_text_in_msg = true;
                            self.emitted_any_text = true;
                        }
                        if content.is_some() || self.emitted_text_in_msg {
                            self.stream
                                .finish_kind(
                                    BlockKind::Text,
                                    content,
                                    &self.sink,
                                    &self.session_id,
                                )
                                .await;
                        }
                    }
                    Some("toolcall_start") => {
                        let partial_item = c_idx
                            .and_then(|idx| ame["partial"]["content"].get(idx as usize))
                            .unwrap_or(&Value::Null);
                        let tc_id = ame["toolCall"]["id"]
                            .as_str()
                            .or_else(|| ame["id"].as_str())
                            .or_else(|| partial_item["id"].as_str())
                            .map(str::trim)
                            .unwrap_or("");
                        let tc_name = ame["toolCall"]["name"]
                            .as_str()
                            .or_else(|| ame["toolName"].as_str())
                            .or_else(|| ame["name"].as_str())
                            .or_else(|| partial_item["name"].as_str())
                            .unwrap_or("tool")
                            .trim();
                        if !tc_id.is_empty() {
                            self.stream.finish_active(&self.sink, &self.session_id).await;
                            let kind = normalize_pi_tool_kind(tc_name).to_string();
                            let args = if !ame["toolCall"]["arguments"].is_null() {
                                &ame["toolCall"]["arguments"]
                            } else {
                                &partial_item["arguments"]
                            };
                            let title = format_pi_tool_title(tc_name, args);
                            self.tool_meta.insert(
                                tc_id.to_string(),
                                PiToolMeta {
                                    name: tc_name.to_string(),
                                    kind: kind.clone(),
                                    title: title.clone(),
                                    last_partial_emit: None,
                                },
                            );
                            let mut ev = json!({
                                "type": "session_update",
                                "session_id": self.session_id,
                                "kind": "tool_call",
                                "tool_call_id": tc_id,
                                "name": tc_name,
                                "tool_kind": kind,
                                "title": title,
                                "status": "pending",
                            });
                            let input = format_pi_tool_input(tc_name, args);
                            if !input.is_empty() {
                                ev["input"] = json!(input);
                            }
                            let locs = extract_pi_tool_locations(tc_name, args, &self.cwd);
                            if !locs.is_empty() {
                                ev["locations"] = json!(locs);
                            }
                            let diffs = extract_pi_tool_diffs(tc_name, args);
                            if !diffs.is_empty() {
                                ev["diffs"] = json!(diffs);
                            }
                            let _ = self.sink.send(ev).await;
                            self.emit_activity("tooling", &title, None).await;
                        }
                    }
                    Some("toolcall_end") => {
                        let tc = &ame["toolCall"];
                        let tc_id = tc["id"].as_str().map(str::trim).unwrap_or("");
                        if !tc_id.is_empty() {
                            let fallback_name = self
                                .tool_meta
                                .get(tc_id)
                                .map(|m| m.name.clone())
                                .unwrap_or_else(|| "tool".to_string());
                            let tc_name = tc["name"]
                                .as_str()
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .unwrap_or(&fallback_name);
                            let kind = normalize_pi_tool_kind(tc_name).to_string();
                            let args = &tc["arguments"];
                            let title = format_pi_tool_title(tc_name, args);
                            let meta = PiToolMeta {
                                name: tc_name.to_string(),
                                kind: kind.clone(),
                                title: title.clone(),
                                last_partial_emit: self
                                    .tool_meta
                                    .get(tc_id)
                                    .and_then(|m| m.last_partial_emit),
                            };
                            self.tool_meta.insert(tc_id.to_string(), meta);
                            let mut ev = json!({
                                "type": "session_update",
                                "session_id": self.session_id,
                                "kind": "tool_call",
                                "tool_call_id": tc_id,
                                "name": tc_name,
                                "tool_kind": kind,
                                "title": title,
                            });
                            let input = format_pi_tool_input(tc_name, args);
                            if !input.is_empty() {
                                ev["input"] = json!(input);
                            }
                            let locs = extract_pi_tool_locations(tc_name, args, &self.cwd);
                            if !locs.is_empty() {
                                ev["locations"] = json!(locs);
                            }
                            let diffs = extract_pi_tool_diffs(tc_name, args);
                            if !diffs.is_empty() {
                                ev["diffs"] = json!(diffs);
                            }
                            let _ = self.sink.send(ev).await;
                        }
                    }
                    _ => {}
                }
                let usage = if ame["partial"]["usage"].is_object() {
                    &ame["partial"]["usage"]
                } else {
                    &v["message"]["usage"]
                };
                if usage.is_object() {
                    self.emit_pi_usage(usage, false).await;
                }
            }
            Some("message_end") => {
                let message = &v["message"];
                if message["role"].as_str() == Some("assistant") {
                    if message["stopReason"].as_str() == Some("error") {
                        if let Some(err) = message["errorMessage"].as_str().filter(|s| !s.is_empty())
                        {
                            self.last_error = Some(err.to_string());
                        }
                    }
                    if !self.emitted_thought_in_msg {
                        let thinking_fallback = extract_assistant_thinking(&message["content"]);
                        if !thinking_fallback.is_empty() {
                            self.stream
                                .finish_kind(
                                    BlockKind::Thought,
                                    Some(&thinking_fallback),
                                    &self.sink,
                                    &self.session_id,
                                )
                                .await;
                        }
                    } else {
                        self.stream
                            .finish_kind(BlockKind::Thought, None, &self.sink, &self.session_id)
                            .await;
                    }

                    if !self.emitted_text_in_msg {
                        let fallback = extract_assistant_text(&message["content"]);
                        if !fallback.is_empty() {
                            self.emitted_any_text = true;
                            self.stream
                                .finish_kind(
                                    BlockKind::Text,
                                    Some(&fallback),
                                    &self.sink,
                                    &self.session_id,
                                )
                                .await;
                        }
                    } else {
                        self.stream
                            .finish_kind(BlockKind::Text, None, &self.sink, &self.session_id)
                            .await;
                    }

                    if message["usage"].is_object() {
                        self.emit_pi_usage(&message["usage"], true).await;
                    }
                    self.emitted_thought_in_msg = false;
                    self.emitted_text_in_msg = false;
                }
            }
            Some("tool_execution_start") => {
                self.stream.finish_active(&self.sink, &self.session_id).await;
                let call_id = v["toolCallId"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let tool_name = v["toolName"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("tool")
                    .to_string();
                let args = &v["args"];
                let kind = normalize_pi_tool_kind(&tool_name).to_string();
                let title = format_pi_tool_title(&tool_name, args);
                let input = format_pi_tool_input(&tool_name, args);
                let locs = extract_pi_tool_locations(&tool_name, args, &self.cwd);
                let diffs = extract_pi_tool_diffs(&tool_name, args);

                self.tool_starts.insert(call_id.clone(), Instant::now());
                self.tool_meta.insert(
                    call_id.clone(),
                    PiToolMeta {
                        name: tool_name.clone(),
                        kind: kind.clone(),
                        title: title.clone(),
                        last_partial_emit: None,
                    },
                );

                let mut ev = json!({
                    "type": "session_update",
                    "session_id": self.session_id,
                    "kind": "tool_call",
                    "tool_call_id": call_id,
                    "name": tool_name,
                    "tool_kind": kind,
                    "title": title,
                    "status": "in_progress",
                });
                if !input.is_empty() {
                    ev["input"] = json!(input);
                }
                if !locs.is_empty() {
                    ev["locations"] = json!(locs);
                }
                if !diffs.is_empty() {
                    ev["diffs"] = json!(diffs);
                }
                let _ = self.sink.send(ev).await;
                self.emit_activity("tooling", &title, None).await;
            }
            Some("tool_execution_update") => {
                let call_id = v["toolCallId"].as_str().map(str::trim).unwrap_or("");
                if !call_id.is_empty() {
                    let throttled = self
                        .tool_meta
                        .get(call_id)
                        .and_then(|m| m.last_partial_emit)
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(150));
                    if !throttled {
                        let output = extract_tool_result_text(&v["partialResult"]);
                        let output = truncate_chars(&output, 60_000);
                        if !output.is_empty() {
                            if let Some(meta) = self.tool_meta.get_mut(call_id) {
                                meta.last_partial_emit = Some(Instant::now());
                            }
                            let _ = self
                                .sink
                                .send(json!({
                                    "type": "session_update",
                                    "session_id": self.session_id,
                                    "kind": "tool_call",
                                    "tool_call_id": call_id,
                                    "status": "in_progress",
                                    "output": output,
                                }))
                                .await;
                        }
                    }
                }
            }
            Some("tool_execution_end") => {
                let call_id = v["toolCallId"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let meta = self.tool_meta.get(&call_id);
                let tool_name = v["toolName"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| meta.map(|m| m.name.clone()))
                    .unwrap_or_else(|| "tool".to_string());
                let kind = meta
                    .map(|m| m.kind.clone())
                    .unwrap_or_else(|| normalize_pi_tool_kind(&tool_name).to_string());
                let title = meta
                    .map(|m| m.title.clone())
                    .unwrap_or_else(|| tool_name.clone());
                let is_error = v["isError"].as_bool().unwrap_or(false);
                let duration_ms = v["durationMs"]
                    .as_u64()
                    .or_else(|| v["result"]["details"]["durationMs"].as_u64())
                    .or_else(|| {
                        self.tool_starts
                            .remove(&call_id)
                            .map(|t| t.elapsed().as_millis() as u64)
                    });
                let output = truncate_chars(&extract_tool_result_text(&v["result"]), 60_000);

                let mut ev = json!({
                    "type": "session_update",
                    "session_id": self.session_id,
                    "kind": "tool_call",
                    "tool_call_id": call_id,
                    "name": tool_name,
                    "tool_kind": kind,
                    "title": title,
                    "status": if is_error { "failed" } else { "completed" },
                    "is_error": is_error,
                });
                if !output.is_empty() {
                    ev["output"] = json!(output);
                }
                if let Some(ms) = duration_ms {
                    ev["duration_ms"] = json!(ms);
                }
                let _ = self.sink.send(ev).await;
            }
            Some("compaction_start") => {
                self.stream.finish_active(&self.sink, &self.session_id).await;
                self.emit_activity("compacting", "正在压缩上下文…", None)
                    .await;
            }
            Some("compaction_end") => {
                self.emit_activity("responding", "", None).await;
            }
            Some("auto_retry_start") => {
                let attempt = v["attempt"].as_u64().unwrap_or(1);
                let detail = format!("请求重试中（第 {attempt} 次）…");
                self.emit_activity("retrying", &detail, None).await;
            }
            Some("auto_retry_end") => {
                self.emit_activity("responding", "", None).await;
            }
            Some("extension_ui_request") => {
                self.handle_extension_ui(v).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_extension_ui(&mut self, v: &Value) -> Result<(), String> {
        let req_id = v["id"].as_str().unwrap_or("");
        let method = v["method"].as_str().unwrap_or("");
        if req_id.is_empty() {
            return Ok(());
        }

        match method {
            "confirm" => {
                let title = v["title"].as_str().unwrap_or("");
                let message = v["message"].as_str().unwrap_or("");
                let allow = if self.gate_marker.as_deref() == Some(title) {
                    self.evaluate_and_confirm_tool(message).await
                } else {
                    self.stream.finish_active(&self.sink, &self.session_id).await;
                    self.emit_activity("waiting_permission", title, None).await;
                    let choice = ask_user_permission_via_sink(
                        &self.sink,
                        &self.pending_perms,
                        &self.cancel,
                        &self.session_id,
                        &self.cwd,
                        &self.policy,
                        "extension_confirm",
                        title,
                        message,
                    )
                    .await;
                    let allowed = choice.starts_with("allow");
                    if allowed {
                        self.emit_activity("tooling", title, None).await;
                    }
                    allowed
                };
                self.write_line(&json!({
                    "type": "extension_ui_response",
                    "id": req_id,
                    "confirmed": allow,
                }))
                .await?;
            }
            "select" => {
                let title = v["title"].as_str().unwrap_or("");
                let options: Vec<String> = v["options"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let reason = if options.is_empty() {
                    title.to_string()
                } else {
                    format!("{title}\n可选项：{}", options.join(" / "))
                };
                self.stream.finish_active(&self.sink, &self.session_id).await;
                self.emit_activity("waiting_permission", title, None).await;
                let choice = ask_user_permission_via_sink(
                    &self.sink,
                    &self.pending_perms,
                    &self.cancel,
                    &self.session_id,
                    &self.cwd,
                    &self.policy,
                    "extension_select",
                    title,
                    &reason,
                )
                .await;
                if choice.starts_with("allow") {
                    self.emit_activity("tooling", title, None).await;
                    if let Some(first) = options.first() {
                        self.write_line(&json!({
                            "type": "extension_ui_response",
                            "id": req_id,
                            "value": first,
                        }))
                        .await?;
                        return Ok(());
                    }
                }
                self.write_line(&json!({
                    "type": "extension_ui_response",
                    "id": req_id,
                    "cancelled": true,
                }))
                .await?;
            }
            "input" | "editor" => {
                self.write_line(&json!({
                    "type": "extension_ui_response",
                    "id": req_id,
                    "cancelled": true,
                }))
                .await?;
            }
            "notify" => {
                let msg_text = v["message"].as_str().unwrap_or("");
                if !msg_text.is_empty() {
                    self.log_rpc("in", "pi/notify", msg_text).await;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn evaluate_and_confirm_tool(&mut self, payload_json: &str) -> bool {
        let parsed: Value = serde_json::from_str(payload_json).unwrap_or(Value::Null);
        let tool_name = parsed["toolName"].as_str().unwrap_or("tool");
        let input = &parsed["input"];

        let mut locations = Vec::new();
        if let Some(p) = input["path"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            locations.push(p.to_string());
        } else if matches!(tool_name, "grep" | "find" | "ls") {
            locations.push(self.cwd.to_string_lossy().to_string());
        }

        let paths_in_scope = locations
            .iter()
            .all(|loc| permission::path_inside_root(loc, &self.cwd));

        let display_cmd = tool_display_cmd(tool_name, input);
        let cmd_str = if !display_cmd.is_empty() {
            display_cmd
        } else {
            format!("{tool_name} {}", locations.join(" "))
        };
        let key = permission::cache_key(&self.session_id, &self.cwd, tool_name, &cmd_str);

        if paths_in_scope {
            if let Some(cached) = self.perm_cache.lock().unwrap().get(&key) {
                return cached;
            }
            if permission::evaluate_policy(&self.policy, tool_name, &locations, &self.cwd)
                == Some(true)
            {
                return true;
            }
        }

        let reason = if paths_in_scope {
            format!("Agent 请求执行 {tool_name}")
        } else {
            format!("警告：目标路径超出工作目录！Agent 请求执行 {tool_name}")
        };
        self.stream.finish_active(&self.sink, &self.session_id).await;
        self.emit_activity("waiting_permission", &cmd_str, None).await;
        let choice = ask_user_permission_via_sink(
            &self.sink,
            &self.pending_perms,
            &self.cancel,
            &self.session_id,
            &self.cwd,
            &self.policy,
            tool_name,
            &cmd_str,
            &reason,
        )
        .await;
        if paths_in_scope {
            self.perm_cache.lock().unwrap().record(key, &choice);
        }
        let allowed = choice.starts_with("allow");
        if allowed {
            self.emit_activity("tooling", &cmd_str, None).await;
        }
        allowed
    }
}

fn extract_pi_usage(usage: &Value) -> Option<Value> {
    let input = usage["input"]
        .as_u64()
        .or_else(|| usage["input_tokens"].as_u64());
    let output = usage["output"]
        .as_u64()
        .or_else(|| usage["output_tokens"].as_u64());
    let cache_read = usage["cacheRead"]
        .as_u64()
        .or_else(|| usage["cache_read"].as_u64());
    let total = usage["totalTokens"]
        .as_u64()
        .or_else(|| usage["total_tokens"].as_u64());
    if input.is_none() && output.is_none() && cache_read.is_none() && total.is_none() {
        return None;
    }
    let mut out = json!({});
    if let Some(n) = input {
        out["input_tokens"] = json!(n);
    }
    if let Some(n) = output {
        out["output_tokens"] = json!(n);
    }
    if let Some(n) = cache_read {
        out["cache_read"] = json!(n);
    }
    if let Some(n) = total {
        out["total_tokens"] = json!(n);
    }
    Some(out)
}

fn normalize_pi_tool_kind(tool_name: &str) -> &'static str {
    match tool_name.trim().to_ascii_lowercase().as_str() {
        "read" | "read_file" => "read",
        "edit" | "write" | "write_file" => "edit",
        "grep" | "find" | "ls" | "glob" | "search" => "search",
        "bash" | "powershell" | "sh" | "cmd" | "terminal" => "execute",
        _ => "other",
    }
}

fn format_pi_tool_title(tool_name: &str, args: &Value) -> String {
    let disp = tool_display_cmd(tool_name, args);
    if disp.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name}: {}", truncate_chars(&disp, 120))
    }
}

fn format_pi_tool_input(tool_name: &str, args: &Value) -> String {
    if matches!(tool_name, "bash" | "powershell") {
        if let Some(cmd) = args["command"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            return truncate_chars(cmd, 8000);
        }
    }
    if args.is_object() {
        let cloned = if matches!(tool_name, "edit" | "write") {
            let mut stripped = serde_json::Map::new();
            if let Some(p) = args.get("path") {
                stripped.insert("path".to_string(), p.clone());
            }
            if !stripped.is_empty() {
                Value::Object(stripped)
            } else {
                args.clone()
            }
        } else {
            args.clone()
        };
        let s = serde_json::to_string_pretty(&cloned).unwrap_or_default();
        if !s.is_empty() && s != "{}" {
            return truncate_chars(&s, 8000);
        }
    } else if let Some(s) = args.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        return truncate_chars(s, 8000);
    }
    String::new()
}

fn extract_pi_tool_locations(tool_name: &str, args: &Value, cwd: &Path) -> Vec<String> {
    if let Some(p) = args["path"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        return vec![p.to_string()];
    }
    if matches!(tool_name, "grep" | "find" | "ls") {
        return vec![cwd.to_string_lossy().to_string()];
    }
    Vec::new()
}

fn extract_pi_tool_diffs(tool_name: &str, args: &Value) -> Vec<Value> {
    let path = args["path"].as_str().map(str::trim).unwrap_or("");
    if path.is_empty() {
        return Vec::new();
    }
    match tool_name {
        "edit" => {
            if let Some(edits) = args["edits"].as_array() {
                let mut out = Vec::new();
                for e in edits.iter().take(10) {
                    let old_t = e["oldText"].as_str().or_else(|| e["old_text"].as_str());
                    let new_t = e["newText"]
                        .as_str()
                        .or_else(|| e["new_text"].as_str())
                        .unwrap_or("");
                    if old_t.is_some() || !new_t.is_empty() {
                        out.push(json!({
                            "path": path,
                            "old_text": old_t.map(|s| truncate_chars(s, 32_768)),
                            "new_text": truncate_chars(new_t, 32_768),
                        }));
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
            let old_t = args["oldText"]
                .as_str()
                .or_else(|| args["old_text"].as_str());
            let new_t = args["newText"]
                .as_str()
                .or_else(|| args["new_text"].as_str())
                .unwrap_or("");
            if old_t.is_some() || !new_t.is_empty() {
                return vec![json!({
                    "path": path,
                    "old_text": old_t.map(|s| truncate_chars(s, 32_768)),
                    "new_text": truncate_chars(new_t, 32_768),
                })];
            }
            Vec::new()
        }
        "write" => {
            if let Some(content) = args["content"].as_str() {
                vec![json!({
                    "path": path,
                    "old_text": Value::Null,
                    "new_text": truncate_chars(content, 32_768),
                })]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn tool_display_cmd(tool_name: &str, input: &Value) -> String {
    if matches!(tool_name, "bash" | "powershell") {
        if let Some(cmd) = input["command"].as_str().map(str::trim) {
            return cmd.to_string();
        }
    }
    if let Some(path) = input["path"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(pat) = input["pattern"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return format!("{pat} ({path})");
        }
        return path.to_string();
    }
    if let Some(pat) = input["pattern"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return pat.to_string();
    }
    String::new()
}

fn extract_assistant_thinking(content: &Value) -> String {
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter_map(|item| {
            if item["type"].as_str() == Some("thinking") {
                item["thinking"].as_str().or_else(|| item["text"].as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn extract_assistant_text(content: &Value) -> String {
    let Some(arr) = content.as_array() else {
        return content.as_str().unwrap_or("").to_string();
    };
    arr.iter()
        .filter_map(|item| {
            if item["type"].as_str() == Some("text") {
                item["text"].as_str()
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn extract_tool_result_text(result: &Value) -> String {
    if let Some(arr) = result["content"].as_array() {
        let joined = arr
            .iter()
            .filter_map(|item| {
                if item["type"].as_str() == Some("text") {
                    item["text"].as_str()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return joined;
        }
    }
    result["text"]
        .as_str()
        .or_else(|| result.as_str())
        .unwrap_or("")
        .to_string()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::super::acp_client::{AgentConfig, AttachmentPayload};
    use super::*;

    fn sample_msg(policy: &str, session_key: Option<&str>) -> PromptStartMsg {
        PromptStartMsg {
            run_id: None,
            session_id: "s-1".into(),
            acp_session_key: session_key.map(str::to_string),
            cwd: ".".into(),
            agent: AgentConfig {
                id: Some("a-1".into()),
                preset: "pi".into(),
                cmd: "pi --mode rpc".into(),
                env: HashMap::new(),
            },
            model: String::new(),
            thinking_level: "中".into(),
            permission_policy: policy.into(),
            text: "hi".into(),
            files: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn detects_pi_rpc_invocations_and_builds_launch_args() {
        assert!(is_pi_rpc_invocation("pi", "pi", &[]));
        assert!(is_pi_rpc_invocation(
            "custom",
            r"C:\Users\me\AppData\Roaming\npm\pi.cmd",
            &["--mode".into(), "rpc".into()]
        ));
        assert!(is_pi_rpc_invocation(
            "custom",
            "pi",
            &["--mode=rpc".into()]
        ));
        assert!(!is_pi_rpc_invocation("custom", "pi-acp", &[]));
        assert!(!is_pi_rpc_invocation(
            "pi",
            "pi",
            &["--mode".into(), "json".into()]
        ));
        assert!(!is_pi_rpc_invocation("pi", "pi", &["--acp".into()]));

        let data = std::env::temp_dir().join(format!("pi_gate_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let sess_file = data.join("session.jsonl");
        std::fs::write(&sess_file, "{}\n").unwrap();

        let (args, tok) = prepare_launch_args(
            &["--no-tools".into()],
            &sample_msg("auto_read", Some(&sess_file.to_string_lossy())),
            &data,
        )
        .unwrap();
        assert!(tok.is_some());
        assert!(args.windows(2).any(|w| w == ["--mode", "rpc"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--session", &sess_file.to_string_lossy()]));
        assert!(args.iter().any(|a| a == "-e"));
        assert!(data.join(PERMISSION_GATE_FILENAME).is_file());

        // 缺失的 session 文件路径应优雅降级为不传 --session，allow_all 不注入 gate
        let (args_allow, tok_allow) = prepare_launch_args(
            &["--mode".into(), "rpc".into()],
            &sample_msg("allow_all", Some("/nonexistent/missing.jsonl")),
            &data,
        )
        .unwrap();
        assert!(tok_allow.is_none());
        assert!(!args_allow.iter().any(|a| a == "--session"));
        assert!(!args_allow.iter().any(|a| a == "-e"));

        let _ = std::fs::remove_dir_all(data);
    }

    #[tokio::test]
    async fn pi_rpc_drives_model_thinking_permission_tools_and_streaming() {
        let dir = std::env::temp_dir().join(format!("pi_rpc_drive_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (agent_read, client_write) = tokio::io::duplex(16384);
        let (client_read, mut agent_write) = tokio::io::duplex(16384);

        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(64);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending_perms.clone();
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let gate_token = "tok-123";
        let expected_marker = format!("__CHAT_PERM_GATE__{gate_token}");

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();

            // 1. get_state
            let r1: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r1["type"], "get_state");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "id": r1["id"],
                            "type": "response",
                            "command": "get_state",
                            "success": true,
                            "data": {
                                "sessionId": "pi-s1",
                                "sessionFile": "/tmp/pi-s1.jsonl",
                                "thinkingLevel": "low"
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 2. get_available_models + set_model (id contains '/')
            let r2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r2["type"], "get_available_models");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "id": r2["id"],
                            "type": "response",
                            "command": "get_available_models",
                            "success": true,
                            "data": {
                                "models": [
                                    {"provider": "magpie", "id": "xy-open/glm-5.3", "name": "GLM-5.3"}
                                ]
                            }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r3: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r3["type"], "set_model");
            assert_eq!(r3["provider"], "magpie");
            assert_eq!(r3["modelId"], "xy-open/glm-5.3");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r3["id"], "type": "response", "command": "set_model", "success": true, "data": {}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 3. get_available_thinking_levels ("高" falls back to "xhigh" when "high" missing)
            let r4: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r4["type"], "get_available_thinking_levels");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "id": r4["id"],
                            "type": "response",
                            "command": "get_available_thinking_levels",
                            "success": true,
                            "data": { "levels": ["off", "low", "xhigh"] }
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let r5: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r5["type"], "set_thinking_level");
            assert_eq!(r5["level"], "xhigh");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r5["id"], "type": "response", "command": "set_thinking_level", "success": true})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 4. prompt with image
            let r6: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(r6["type"], "prompt");
            assert_eq!(r6["images"][0]["mimeType"], "image/png");
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r6["id"], "type": "response", "command": "prompt", "success": true, "data": {"disposition": "started"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            // 4a. read inside cwd -> auto-allowed by auto_read without UI prompt
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "type": "extension_ui_request",
                            "id": "ui-read",
                            "method": "confirm",
                            "title": expected_marker,
                            "message": json!({"toolName": "read", "input": {"path": "README.md"}}).to_string()
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let ui_read_resp: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(ui_read_resp["id"], "ui-read");
            assert_eq!(ui_read_resp["confirmed"], true);

            // 4b. bash -> asks user, user chooses allow_always; second similar bash hits cache
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "type": "extension_ui_request",
                            "id": "ui-bash-1",
                            "method": "confirm",
                            "title": expected_marker,
                            "message": json!({"toolName": "bash", "input": {"command": "cargo test"}}).to_string()
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let ui_bash_resp1: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(ui_bash_resp1["confirmed"], true);

            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({
                            "type": "extension_ui_request",
                            "id": "ui-bash-2",
                            "method": "confirm",
                            "title": expected_marker,
                            "message": json!({"toolName": "bash", "input": {"command": "cargo check"}}).to_string()
                        })
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let ui_bash_resp2: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(ui_bash_resp2["confirmed"], true);

            // 4c. tool_execution_start/end + text_delta + agent_settled
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{"command":"cargo test"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[{"type":"text","text":"19 passed"}]},"isError":false})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Done!"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"type":"message_end","message":{"role":"assistant","content":[]}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            agent_write
                .write_all(format!("{}\n", json!({"type":"agent_settled","aborted":false})).as_bytes())
                .await
                .unwrap();
        });

        let responder = tokio::spawn(async move {
            let mut perm_count = 0usize;
            let mut saw_tool_done = false;
            let mut text = String::new();
            while let Some(ev) = sink_rx.recv().await {
                match ev["kind"].as_str() {
                    Some("permission_request") => {
                        perm_count += 1;
                        let req_id = ev["req_id"].as_str().unwrap().to_string();
                        let tx = pending_clone.lock().unwrap().remove(&req_id).unwrap();
                        let _ = tx.send("allow_always".to_string());
                    }
                    Some("tool_call") => {
                        if ev["status"] == "completed"
                            && ev["output"] == "19 passed"
                            && ev["tool_kind"] == "execute"
                        {
                            saw_tool_done = true;
                        }
                    }
                    Some("text_delta") => {
                        if let Some(d) = ev["delta"].as_str() {
                            text.push_str(d);
                        }
                    }
                    _ => {}
                }
            }
            (perm_count, saw_tool_done, text)
        });

        let mut msg = sample_msg("auto_read", None);
        msg.cwd = dir.to_string_lossy().into();
        msg.model = "xy-open/glm-5.3".into();
        msg.thinking_level = "高".into();
        msg.attachments = vec![AttachmentPayload {
            file_name: "shot.png".into(),
            mime: "image/png".into(),
            data_base64: Some("AAAA".into()),
        }];

        let out_key = drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
            Some(gate_token),
        )
        .await
        .unwrap();
        assert_eq!(out_key.as_deref(), Some("/tmp/pi-s1.jsonl"));
        agent_task.await.unwrap();
        let (perm_count, saw_tool_done, text) = responder.await.unwrap();
        // read 自动放行，第 2 次 bash 命中 allow_always 缓存，所以前端只弹 1 次权限请求
        assert_eq!(perm_count, 1);
        assert!(saw_tool_done);
        assert_eq!(text, "Done!");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pi_rpc_handles_disposition_handled_and_abort_cancel() {
        // Case 1: disposition == "handled" returns immediately without waiting for agent_settled
        let dir = std::env::temp_dir().join(format!("pi_rpc_cancel_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let (agent_read, client_write) = tokio::io::duplex(8192);
            let (client_read, mut agent_write) = tokio::io::duplex(8192);
            let (sink_tx, _sink_rx) = mpsc::channel::<Value>(16);
            let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
            let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
            let (_cancel_tx, cancel_rx) = watch::channel(false);

            let agent_task = tokio::spawn(async move {
                let mut lines = BufReader::new(agent_read).lines();
                let r1: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r1["id"], "type": "response", "command": "get_state", "success": true, "data": {"sessionId": "pi-ext"}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let r2: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(r2["type"], "get_available_thinking_levels");
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r2["id"], "type": "response", "command": "get_available_thinking_levels", "success": true, "data": {"levels": ["medium"]}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let r3: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(r3["type"], "set_thinking_level");
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r3["id"], "type": "response", "command": "set_thinking_level", "success": true})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let r4: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(r4["type"], "prompt");
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r4["id"], "type": "response", "command": "prompt", "success": true, "data": {"disposition": "handled"}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });

            let mut msg = sample_msg("allow_all", None);
            msg.cwd = dir.to_string_lossy().into();
            let out_key = drive_stdio(
                client_read,
                client_write,
                &msg,
                &dir,
                sink_tx,
                pending_perms,
                perm_cache,
                cancel_rx,
                None,
            )
            .await
            .unwrap();
            assert_eq!(out_key.as_deref(), Some("pi-ext"));
            agent_task.await.unwrap();
        }

        // Case 2: cancel sends abort and waits for agent_settled
        {
            let (agent_read, client_write) = tokio::io::duplex(8192);
            let (client_read, mut agent_write) = tokio::io::duplex(8192);
            let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(16);
            let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
            let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
            let (cancel_tx, cancel_rx) = watch::channel(false);

            let canceller = tokio::spawn(async move {
                while let Some(ev) = sink_rx.recv().await {
                    if ev["kind"] == "text_delta" {
                        let _ = cancel_tx.send(true);
                        break;
                    }
                }
            });

            let agent_task = tokio::spawn(async move {
                let mut lines = BufReader::new(agent_read).lines();
                let r1: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r1["id"], "type": "response", "command": "get_state", "success": true, "data": {"sessionId": "pi-abort", "thinkingLevel": "medium"}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let r2: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r2["id"], "type": "response", "command": "get_available_thinking_levels", "success": true, "data": {"levels": ["medium"]}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let r3: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id": r3["id"], "type": "response", "command": "prompt", "success": true, "data": {"disposition": "started"}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Working..."}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();

                let abort_cmd: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(abort_cmd["type"], "abort");
                agent_write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"type": "agent_settled", "aborted": true})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });

            let mut msg = sample_msg("allow_all", None);
            msg.cwd = dir.to_string_lossy().into();
            let out_key = drive_stdio(
                client_read,
                client_write,
                &msg,
                &dir,
                sink_tx,
                pending_perms,
                perm_cache,
                cancel_rx,
                None,
            )
            .await
            .unwrap();
            assert_eq!(out_key.as_deref(), Some("pi-abort"));
            canceller.await.unwrap();
            agent_task.await.unwrap();
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pi_rpc_streams_thinking_toolcall_partial_retry_and_usage() {
        let dir = std::env::temp_dir().join(format!("pi_rpc_rich_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (agent_read, client_write) = tokio::io::duplex(16384);
        let (client_read, mut agent_write) = tokio::io::duplex(16384);

        let (sink_tx, mut sink_rx) = mpsc::channel::<Value>(64);
        let pending_perms: PendingPerms = Arc::new(Mutex::new(HashMap::new()));
        let perm_cache = Arc::new(Mutex::new(PermissionCache::default()));
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let agent_task = tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let r1: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r1["id"], "type": "response", "command": "get_state", "success": true, "data": {"sessionId": "pi-rich", "thinkingLevel": "medium"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let r2: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r2["id"], "type": "response", "command": "get_available_thinking_levels", "success": true, "data": {"levels": ["medium"]}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let r3: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            agent_write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id": r3["id"], "type": "response", "command": "prompt", "success": true, "data": {"disposition": "started"}})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            for ev in [
                json!({"type":"message_start","message":{"id":"m1","role":"assistant"}}),
                json!({"type":"message_update","message":{"id":"m1"},"assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"Analyze "}}),
                json!({"type":"message_update","message":{"id":"m1"},"assistantMessageEvent":{"type":"thinking_end","contentIndex":0,"content":"Analyze problem"}}),
                json!({"type":"message_update","message":{"id":"m1"},"assistantMessageEvent":{"type":"toolcall_start","contentIndex":1,"toolCall":{"id":"tc-edit","name":"edit"}}}),
                json!({"type":"message_update","message":{"id":"m1"},"assistantMessageEvent":{"type":"toolcall_end","contentIndex":1,"toolCall":{"id":"tc-edit","name":"edit","arguments":{"path":"src/lib.rs","oldText":"a","newText":"b"}}}}),
                json!({"type":"tool_execution_start","toolCallId":"tc-edit","toolName":"edit","args":{"path":"src/lib.rs","oldText":"a","newText":"b"}}),
                json!({"type":"tool_execution_update","toolCallId":"tc-edit","toolName":"edit","partialResult":{"content":[{"type":"text","text":"applying diff..."}]}}),
                json!({"type":"tool_execution_end","toolCallId":"tc-edit","toolName":"edit","result":{"content":[{"type":"text","text":"ok"}]},"isError":false,"durationMs":42}),
                json!({"type":"auto_retry_start","attempt":2}),
                json!({"type":"auto_retry_end"}),
                json!({"type":"compaction_start"}),
                json!({"type":"compaction_end"}),
                json!({"type":"message_update","message":{"id":"m1"},"assistantMessageEvent":{"type":"text_delta","contentIndex":2,"delta":"Patched!"}}),
                json!({"type":"message_end","message":{"id":"m1","role":"assistant","content":[],"usage":{"input":120,"output":45,"cacheRead":30,"totalTokens":195}}}),
                json!({"type":"agent_settled","aborted":false}),
            ] {
                agent_write
                    .write_all(format!("{ev}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });

        let collector = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(ev) = sink_rx.recv().await {
                events.push(ev);
            }
            events
        });

        let mut msg = sample_msg("allow_all", None);
        msg.cwd = dir.to_string_lossy().into();
        let out_key = drive_stdio(
            client_read,
            client_write,
            &msg,
            &dir,
            sink_tx,
            pending_perms,
            perm_cache,
            cancel_rx,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out_key.as_deref(), Some("pi-rich"));
        agent_task.await.unwrap();
        let events = collector.await.unwrap();

        assert!(events.iter().any(|e| {
            e["kind"] == "thought_delta" && e["text"] == "Analyze problem" && e["done"] == true
        }));
        assert!(events.iter().any(|e| {
            e["kind"] == "tool_call"
                && e["tool_call_id"] == "tc-edit"
                && e["status"] == "pending"
                && e["tool_kind"] == "edit"
        }));
        assert!(events.iter().any(|e| {
            e["kind"] == "tool_call"
                && e["tool_call_id"] == "tc-edit"
                && e["status"] == "in_progress"
                && e["diffs"][0]["path"] == "src/lib.rs"
        }));
        assert!(events.iter().any(|e| {
            e["kind"] == "tool_call"
                && e["tool_call_id"] == "tc-edit"
                && e["status"] == "completed"
                && e["duration_ms"] == 42
                && e["output"] == "ok"
        }));
        assert!(events.iter().any(|e| e["kind"] == "activity" && e["phase"] == "retrying"));
        assert!(events.iter().any(|e| e["kind"] == "activity" && e["phase"] == "compacting"));
        assert!(events.iter().any(|e| {
            e["kind"] == "activity"
                && e["usage"]["total_tokens"] == 195
                && e["usage"]["input_tokens"] == 120
        }));
        assert!(events
            .iter()
            .any(|e| e["kind"] == "text_delta" && e["delta"] == "Patched!"));

        let _ = std::fs::remove_dir_all(dir);
    }
}
