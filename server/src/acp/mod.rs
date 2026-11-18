use crate::error::{AppError, AppResult};
use crate::state::{ConnMsg, SharedState};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

pub mod host_ws;
pub mod routes;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSettingsView {
    pub default_permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpHostView {
    pub id: Uuid,
    pub name: String,
    pub os: String,
    pub ip: String,
    pub roots: Value,
    pub online: bool,
    pub latency_ms: i32,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpAgentView {
    pub id: Uuid,
    pub host_id: Uuid,
    pub preset: String,
    pub name: String,
    pub adapter: String,
    pub cmd: String,
    pub env: Value,
    pub short_tag: String,
    pub badge_bg: String,
    pub default_model: String,
    pub model_options: Vec<String>,
    pub permission_override: String,
    pub capabilities: Vec<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpWorkspaceView {
    pub id: Uuid,
    pub host_id: Uuid,
    pub cwd: String,
    pub name: String,
    pub branch: String,
    pub branches: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpTurnView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub seq: i64,
    pub role: String,
    pub status: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSessionView {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub title: String,
    pub selected_model: String,
    pub thinking_level: String,
    pub permission_policy: String,
    pub acp_session_key: Option<String>,
    pub running: bool,
    pub diffs: Value,
    pub rpc_logs: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<AcpTurnView>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpBootstrapView {
    pub settings: AcpSettingsView,
    pub hosts: Vec<AcpHostView>,
    pub agents: Vec<AcpAgentView>,
    pub workspaces: Vec<AcpWorkspaceView>,
    pub sessions: Vec<AcpSessionView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpAttachmentView {
    pub id: Uuid,
    pub file_name: String,
    pub mime: String,
    pub size: i64,
}

pub(crate) struct PresetDefaults {
    pub short_tag: &'static str,
    pub badge_bg: &'static str,
    pub adapter: &'static str,
    pub default_model: &'static str,
    pub model_options: Vec<String>,
    pub capabilities: Vec<String>,
}

pub(crate) fn preset_defaults(preset: &str) -> PresetDefaults {
    match preset {
        "claude" => PresetDefaults {
            short_tag: "CC",
            badge_bg: "from-orange-500 to-amber-600",
            adapter: "claude-agent-acp",
            default_model: "Claude Sonnet 4.5",
            model_options: vec![
                "Claude Sonnet 4.5".into(),
                "Claude Opus 4.1".into(),
                "Claude Haiku 4.5".into(),
            ],
            capabilities: vec![
                "fs.read".into(),
                "fs.write".into(),
                "terminal.run".into(),
                "diff.stream".into(),
            ],
        },
        "gemini" => PresetDefaults {
            short_tag: "GM",
            badge_bg: "from-indigo-500 to-sky-500",
            adapter: "gemini --acp",
            default_model: "Gemini 2.5 Pro",
            model_options: vec!["Gemini 2.5 Pro".into(), "Gemini 2.5 Flash".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        "codex" => PresetDefaults {
            short_tag: "CX",
            badge_bg: "from-emerald-500 to-teal-600",
            adapter: "codex-acp",
            default_model: "GPT-5 Codex",
            model_options: vec!["GPT-5 Codex".into(), "o3-pro".into(), "GPT-4.1".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        "opencode" => PresetDefaults {
            short_tag: "OC",
            badge_bg: "from-violet-500 to-purple-600",
            adapter: "opencode acp",
            default_model: "Claude Sonnet 4.5",
            model_options: vec![
                "Claude Sonnet 4.5".into(),
                "Gemini 2.5 Pro".into(),
                "GPT-5 Codex".into(),
            ],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        _ => PresetDefaults {
            short_tag: "AG",
            badge_bg: "from-indigo-500 to-sky-500",
            adapter: "acp-stdio",
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
    }
}

pub(crate) fn derive_workspace_name(cwd: &str, alias: Option<&str>) -> String {
    if let Some(a) = alias.map(str::trim).filter(|s| !s.is_empty()) {
        return a.chars().take(120).collect();
    }
    let trimmed = cwd.trim().trim_end_matches(&['/', '\\'][..]);
    let seg = trimmed
        .rsplit(&['/', '\\'][..])
        .find(|s| !s.is_empty())
        .unwrap_or(cwd.trim());
    let out: String = seg.chars().take(120).collect();
    if out.is_empty() {
        "workspace".to_string()
    } else {
        out
    }
}

pub(crate) fn broadcast_acp(st: &SharedState, uid: Uuid, event: &str, data: Value) {
    let frame = json!({
        "type": "acp_event",
        "event": event,
        "data": data,
    })
    .to_string();
    st.send_to_user(uid, &frame);
}

pub(crate) async fn call_host_rpc(
    st: &SharedState,
    uid: Uuid,
    host_id: Uuid,
    method: &str,
    params: Value,
) -> AppResult<Value> {
    let owns = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM acp_hosts WHERE id = $1 AND user_id = $2)",
    )
    .bind(host_id)
    .bind(uid)
    .fetch_one(&st.pool)
    .await?;
    if !owns {
        return Err(AppError::not_found("host not found"));
    }

    let tx = {
        let Some(conn) = st.acp_hosts.get(&host_id) else {
            return Err(AppError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "host_offline",
                "机器节点当前离线",
            ));
        };
        if conn.uid != uid {
            return Err(AppError::not_found("host not found"));
        }
        conn.tx.clone()
    };

    let req_id = Uuid::new_v4();
    let (wait_tx, wait_rx) = oneshot::channel();
    st.acp_rpc_wait.insert(req_id, wait_tx);
    let payload = json!({
        "type": "rpc_call",
        "req_id": req_id,
        "method": method,
        "params": params,
    })
    .to_string();
    if tx.try_send(ConnMsg::Frame(payload)).is_err() {
        st.acp_rpc_wait.remove(&req_id);
        return Err(AppError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_offline",
            "机器节点连接繁忙或已断开",
        ));
    }

    match tokio::time::timeout(Duration::from_secs(15), wait_rx).await {
        Ok(Ok(Ok(data))) => Ok(data),
        Ok(Ok(Err(err_msg))) => Err(AppError::bad_request(err_msg)),
        Ok(Err(_)) => Err(AppError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_offline",
            "机器节点已断开连接",
        )),
        Err(_) => {
            st.acp_rpc_wait.remove(&req_id);
            Err(AppError::bad_gateway("宿主机器响应超时"))
        }
    }
}

pub(crate) async fn load_settings(pool: &PgPool, uid: Uuid) -> AppResult<AcpSettingsView> {
    let perm = sqlx::query_scalar::<_, String>(
        "SELECT default_permission FROM acp_user_settings WHERE user_id = $1",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await?
    .unwrap_or_else(|| "auto_read".to_string());
    Ok(AcpSettingsView {
        default_permission: perm,
    })
}

fn host_from_row(st: &SharedState, row: &sqlx::postgres::PgRow) -> AcpHostView {
    let id: Uuid = row.get("id");
    let db_online: bool = row.get("online");
    AcpHostView {
        id,
        name: row.get("name"),
        os: row.get("os"),
        ip: row.get("ip"),
        roots: row.get("roots_json"),
        online: db_online && st.acp_hosts.contains_key(&id),
        latency_ms: row.get("latency_ms"),
        last_seen: row.get("last_seen"),
    }
}

pub(crate) async fn load_host(
    st: &SharedState,
    uid: Uuid,
    host_id: Uuid,
) -> AppResult<AcpHostView> {
    let row = sqlx::query(
        "SELECT id, name, os, ip, roots_json, online, latency_ms, last_seen
           FROM acp_hosts WHERE id = $1 AND user_id = $2",
    )
    .bind(host_id)
    .bind(uid)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("host not found"))?;
    Ok(host_from_row(st, &row))
}

fn json_string_vec(v: Value) -> Vec<String> {
    serde_json::from_value(v).unwrap_or_default()
}

fn agent_from_row(row: &sqlx::postgres::PgRow) -> AcpAgentView {
    AcpAgentView {
        id: row.get("id"),
        host_id: row.get("acp_host_id"),
        preset: row.get("preset"),
        name: row.get("name"),
        adapter: row.get("adapter"),
        cmd: row.get("cmd"),
        env: row.get("env_json"),
        short_tag: row.get("short_tag"),
        badge_bg: row.get("badge_bg"),
        default_model: row.get("default_model"),
        model_options: json_string_vec(row.get("model_options")),
        permission_override: row.get("permission_override"),
        capabilities: json_string_vec(row.get("capabilities")),
        status: row.get("status"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub(crate) async fn load_agent(
    pool: &PgPool,
    uid: Uuid,
    agent_id: Uuid,
) -> AppResult<AcpAgentView> {
    let row = sqlx::query(
        "SELECT id, acp_host_id, preset, name, adapter, cmd, env_json, short_tag, badge_bg,
                default_model, model_options, permission_override, capabilities, status,
                created_at, updated_at
           FROM acp_agents WHERE id = $1 AND user_id = $2",
    )
    .bind(agent_id)
    .bind(uid)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("agent not found"))?;
    Ok(agent_from_row(&row))
}

fn workspace_from_row(row: &sqlx::postgres::PgRow) -> AcpWorkspaceView {
    AcpWorkspaceView {
        id: row.get("id"),
        host_id: row.get("acp_host_id"),
        cwd: row.get("cwd"),
        name: row.get("name"),
        branch: row.get("branch"),
        branches: json_string_vec(row.get("branches")),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub(crate) async fn load_workspace(
    pool: &PgPool,
    uid: Uuid,
    ws_id: Uuid,
) -> AppResult<AcpWorkspaceView> {
    let row = sqlx::query(
        "SELECT id, acp_host_id, cwd, name, branch, branches, created_at, updated_at
           FROM acp_workspaces WHERE id = $1 AND user_id = $2",
    )
    .bind(ws_id)
    .bind(uid)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("workspace not found"))?;
    Ok(workspace_from_row(&row))
}

pub(crate) fn turn_from_row(row: &sqlx::postgres::PgRow) -> AcpTurnView {
    AcpTurnView {
        id: row.get("id"),
        session_id: row.get("acp_session_id"),
        seq: row.get("seq"),
        role: row.get("role"),
        status: row.get("status"),
        payload: row.get("payload"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub(crate) fn session_from_row(
    row: &sqlx::postgres::PgRow,
    turns: Option<Vec<AcpTurnView>>,
) -> AcpSessionView {
    AcpSessionView {
        id: row.get("id"),
        workspace_id: row.get("acp_workspace_id"),
        agent_id: row.get("acp_agent_id"),
        title: row.get("title"),
        selected_model: row.get("selected_model"),
        thinking_level: row.get("thinking_level"),
        permission_policy: row.get("permission_policy"),
        acp_session_key: row.get("acp_session_key"),
        running: row.get("running"),
        diffs: row.get("diffs_json"),
        rpc_logs: row.get("rpc_logs_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        turns,
    }
}

pub(crate) async fn load_session_meta(
    pool: &PgPool,
    uid: Uuid,
    sess_id: Uuid,
) -> AppResult<AcpSessionView> {
    let row = sqlx::query(
        "SELECT id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                permission_policy, acp_session_key, running, diffs_json, rpc_logs_json,
                created_at, updated_at
           FROM acp_sessions WHERE id = $1 AND user_id = $2",
    )
    .bind(sess_id)
    .bind(uid)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("session not found"))?;
    Ok(session_from_row(&row, None))
}

pub(crate) async fn load_bootstrap(st: &SharedState, uid: Uuid) -> AppResult<AcpBootstrapView> {
    let settings = load_settings(&st.pool, uid).await?;

    let host_rows = sqlx::query(
        "SELECT id, name, os, ip, roots_json, online, latency_ms, last_seen
           FROM acp_hosts WHERE user_id = $1 ORDER BY online DESC, created_at ASC",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;
    let hosts = host_rows.iter().map(|r| host_from_row(st, r)).collect();

    let agent_rows = sqlx::query(
        "SELECT id, acp_host_id, preset, name, adapter, cmd, env_json, short_tag, badge_bg,
                default_model, model_options, permission_override, capabilities, status,
                created_at, updated_at
           FROM acp_agents WHERE user_id = $1 ORDER BY created_at ASC",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;
    let agents = agent_rows.iter().map(agent_from_row).collect();

    let ws_rows = sqlx::query(
        "SELECT id, acp_host_id, cwd, name, branch, branches, created_at, updated_at
           FROM acp_workspaces WHERE user_id = $1 ORDER BY updated_at DESC, created_at ASC",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;
    let workspaces = ws_rows.iter().map(workspace_from_row).collect();

    let sess_rows = sqlx::query(
        "SELECT id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                permission_policy, acp_session_key, running, diffs_json, rpc_logs_json,
                created_at, updated_at
           FROM acp_sessions WHERE user_id = $1 ORDER BY updated_at DESC, created_at ASC",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;

    let turn_rows = sqlx::query(
        "SELECT id, acp_session_id, seq, role, status, payload, created_at, updated_at
           FROM acp_turns WHERE user_id = $1 ORDER BY acp_session_id, seq ASC",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;
    let mut turns_by_sess: HashMap<Uuid, Vec<AcpTurnView>> = HashMap::new();
    for r in &turn_rows {
        let tv = turn_from_row(r);
        turns_by_sess.entry(tv.session_id).or_default().push(tv);
    }

    let sessions = sess_rows
        .iter()
        .map(|r| {
            let sid: Uuid = r.get("id");
            let turns = turns_by_sess.remove(&sid).unwrap_or_default();
            session_from_row(r, Some(turns))
        })
        .collect();

    Ok(AcpBootstrapView {
        settings,
        hosts,
        agents,
        workspaces,
        sessions,
    })
}

pub(crate) fn base64_encode(input: &[u8]) -> String {
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
