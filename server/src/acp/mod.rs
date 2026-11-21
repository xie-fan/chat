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
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
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
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        "codex" => PresetDefaults {
            short_tag: "CX",
            badge_bg: "from-emerald-500 to-teal-600",
            adapter: "codex-acp",
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        "opencode" => PresetDefaults {
            short_tag: "OC",
            badge_bg: "from-violet-500 to-purple-600",
            adapter: "opencode acp",
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
            capabilities: vec!["fs.read".into(), "fs.write".into(), "terminal.run".into()],
        },
        "pi" => PresetDefaults {
            short_tag: "PI",
            badge_bg: "from-amber-500 to-orange-600",
            adapter: "pi-rpc",
            default_model: "Default Model",
            model_options: vec!["Default Model".into()],
            capabilities: vec![
                "fs.read".into(),
                "fs.write".into(),
                "terminal.run".into(),
                "diff.stream".into(),
            ],
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

pub(crate) fn is_placeholder_or_legacy_model(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        ""
            | "default model"
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

pub(crate) fn sanitize_model_list(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for item in raw {
        let trimmed: String = item.trim().chars().take(120).collect();
        if trimmed.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == &trimmed) {
            out.push(trimmed);
            if out.len() >= 64 {
                break;
            }
        }
    }
    out
}

pub(crate) fn resolve_probed_models(
    cur_default: &str,
    probed_default: Option<&str>,
    probed_options: &[String],
) -> Option<(String, Vec<String>)> {
    let real_options: Vec<String> = sanitize_model_list(probed_options)
        .into_iter()
        .filter(|m| !is_placeholder_or_legacy_model(m))
        .collect();
    if real_options.is_empty() {
        return None;
    }

    let cur_trimmed = cur_default.trim();
    let next_default = if !is_placeholder_or_legacy_model(cur_trimmed)
        && real_options.iter().any(|x| x == cur_trimmed)
    {
        cur_trimmed.to_string()
    } else if let Some(pd) = probed_default
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|pd| real_options.iter().find(|x| x.as_str() == pd))
    {
        pd.clone()
    } else {
        real_options[0].clone()
    };

    Some((next_default, real_options))
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
    st.acp_rpc_wait.insert(req_id, (host_id, wait_tx));
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

fn mask_env_secrets(raw: Value) -> Value {
    let Some(obj) = raw.as_object() else {
        return json!({});
    };
    let mut masked = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        let s = v.as_str().unwrap_or("");
        masked.insert(
            k.clone(),
            json!(if s.is_empty() { "" } else { "••••••••" }),
        );
    }
    Value::Object(masked)
}

fn agent_from_row_inner(row: &sqlx::postgres::PgRow, mask_env: bool) -> AcpAgentView {
    let raw_env: Value = row.get("env_json");
    AcpAgentView {
        id: row.get("id"),
        host_id: row.get("acp_host_id"),
        preset: row.get("preset"),
        name: row.get("name"),
        adapter: row.get("adapter"),
        cmd: row.get("cmd"),
        env: if mask_env { mask_env_secrets(raw_env) } else { raw_env },
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

pub(crate) fn agent_from_row(row: &sqlx::postgres::PgRow) -> AcpAgentView {
    agent_from_row_inner(row, true)
}

pub(crate) async fn apply_agent_probed_models(
    st: &SharedState,
    uid: Uuid,
    agent_id: Uuid,
    expected_host_id: Option<Uuid>,
    target_session_id: Option<Uuid>,
    probed_default: Option<&str>,
    probed_options: &[String],
) -> AppResult<AcpAgentView> {
    let cur = load_agent(&st.pool, uid, agent_id).await?;
    if let Some(hid) = expected_host_id {
        if cur.host_id != hid {
            return Err(AppError::not_found("agent not found"));
        }
    }

    let Some((next_default, next_options)) =
        resolve_probed_models(&cur.default_model, probed_default, probed_options)
    else {
        return Ok(cur);
    };

    let agent_changed =
        cur.default_model != next_default || cur.model_options != next_options || cur.status != "online";
    let updated_agent = if agent_changed {
        let row = sqlx::query(
            "UPDATE acp_agents
                SET default_model = $3,
                    model_options = $4,
                    status = 'online',
                    updated_at = now()
              WHERE id = $1 AND user_id = $2
              RETURNING id, acp_host_id, preset, name, adapter, cmd, env_json, short_tag, badge_bg,
                        default_model, model_options, permission_override, capabilities, status,
                        created_at, updated_at",
        )
        .bind(agent_id)
        .bind(uid)
        .bind(&next_default)
        .bind(json!(&next_options))
        .fetch_one(&st.pool)
        .await?;
        let view = agent_from_row(&row);
        broadcast_acp(st, uid, "agent_upserted", json!({ "agent": view }));
        view
    } else {
        cur.clone()
    };

    // 对尚未真正启动过对话（acp_session_key IS NULL 且非运行态）的会话，若仍处于占位/旧默认模型或不在有效列表中则同步到新默认模型
    let candidate_sessions = sqlx::query(
        "SELECT id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                permission_policy, acp_session_key, running, diffs_json, rpc_logs_json,
                created_at, updated_at
           FROM acp_sessions
          WHERE user_id = $1 AND acp_agent_id = $2 AND running = false AND acp_session_key IS NULL",
    )
    .bind(uid)
    .bind(agent_id)
    .fetch_all(&st.pool)
    .await?;

    for srow in candidate_sessions {
        let sid: Uuid = srow.get("id");
        let sel: String = srow.get("selected_model");
        let should_sync = is_placeholder_or_legacy_model(&sel)
            || sel == cur.default_model
            || (target_session_id == Some(sid) && !next_options.iter().any(|x| x == &sel));
        if should_sync && sel != next_default {
            if let Ok(urow) = sqlx::query(
                "UPDATE acp_sessions
                    SET selected_model = $3, updated_at = now()
                  WHERE id = $1 AND user_id = $2 AND running = false
                  RETURNING id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                            permission_policy, acp_session_key, running, diffs_json, rpc_logs_json,
                            created_at, updated_at",
            )
            .bind(sid)
            .bind(uid)
            .bind(&next_default)
            .fetch_one(&st.pool)
            .await
            {
                let sview = session_from_row(&urow, None);
                broadcast_acp(st, uid, "session_upserted", json!({ "session": sview }));
            }
        }
    }

    Ok(updated_agent)
}

pub(crate) async fn load_agent_raw(
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
    Ok(agent_from_row_inner(&row, false))
}

pub(crate) async fn delete_acp_attachment_files(files_dir: &str, paths: &[String]) {
    for p in paths {
        if !p.starts_with("acp-") {
            continue;
        }
        let fp = crate::api::stored_file_path(files_dir, p);
        let _ = tokio::fs::remove_file(fp).await;
    }
}

pub async fn reset_stale_acp_state(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE acp_hosts SET online = false, latency_ms = 0 WHERE online = true")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE acp_sessions SET running = false WHERE running = true")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE acp_turns
            SET status = 'rejected',
                payload = jsonb_set(payload, '{chosen_option}', '\"server_restarted\"'::jsonb),
                updated_at = now()
          WHERE role = 'permission' AND status = 'pending'",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE acp_turns
            SET status = 'done',
                updated_at = now()
          WHERE role IN ('agent_text', 'agent_thought') AND status = 'streaming'",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE acp_turns
            SET status = 'failed',
                payload = jsonb_set(payload, '{is_error}', 'true'::jsonb),
                updated_at = now()
          WHERE role = 'tool_call' AND status IN ('pending', 'in_progress')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn sweep_orphan_acp_attachments(pool: &PgPool, files_dir: &str) -> Result<usize, sqlx::Error> {
    let paths: Vec<String> = sqlx::query_scalar(
        "DELETE FROM acp_attachments
          WHERE acp_session_id IS NULL
            AND created_at < now() - interval '24 hours'
          RETURNING storage_path",
    )
    .fetch_all(pool)
    .await?;
    let count = paths.len();
    delete_acp_attachment_files(files_dir, &paths).await;
    Ok(count)
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

#[cfg(test)]
mod tests {
    use super::{preset_defaults, resolve_probed_models};

    #[test]
    fn preset_defaults_use_neutral_placeholder_until_probed() {
        for id in ["claude", "codex", "gemini", "opencode", "pi", "custom"] {
            let p = preset_defaults(id);
            assert_eq!(p.default_model, "Default Model");
            assert_eq!(p.model_options, vec!["Default Model".to_string()]);
        }
        let pi = preset_defaults("pi");
        assert_eq!(pi.short_tag, "PI");
        assert_eq!(pi.adapter, "pi-rpc");
        assert!(pi.capabilities.contains(&"fs.read".to_string()));
        assert!(pi.capabilities.contains(&"terminal.run".to_string()));
    }

    #[test]
    fn resolve_probed_models_replaces_legacy_placeholders_and_preserves_valid_user_choice() {
        let probed = vec![
            "xy-open/glm-5.3".to_string(),
            "xy-openai/gpt-6.2-astra".to_string(),
        ];

        // 旧预设占位名应被探测返回的默认模型替换
        let (def1, opts1) = resolve_probed_models(
            "GPT-5 Codex",
            Some("xy-openai/gpt-6.2-astra"),
            &probed,
        )
        .unwrap();
        assert_eq!(def1, "xy-openai/gpt-6.2-astra");
        assert_eq!(opts1, probed);

        // 用户已手动选中的真实模型若仍在探测列表中，应保留用户选择
        let (def2, _) = resolve_probed_models(
            "xy-open/glm-5.3",
            Some("xy-openai/gpt-6.2-astra"),
            &probed,
        )
        .unwrap();
        assert_eq!(def2, "xy-open/glm-5.3");
    }
}
