use super::{
    base64_encode, broadcast_acp, call_host_rpc, derive_workspace_name, host_ws::insert_turn,
    load_agent, load_bootstrap, load_host, load_session_meta, load_settings, load_workspace,
    preset_defaults, turn_from_row, AcpAttachmentView, AcpBootstrapView, AcpSettingsView,
};
use crate::api::{
    inline_safe, pct_encode, quota_message, read_upload, serve_stored, stored_file_path,
    upload_quota,
};
use crate::auth;
use crate::error::{AppError, AppResult};
use crate::state::{ConnMsg, SharedState};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const UPLOAD_BODY_MARGIN: usize = 1024 * 1024;
const INLINE_ATTACHMENT_LIMIT: usize = 4 * 1024 * 1024;

pub fn router(cfg: &crate::config::Config) -> Router<SharedState> {
    let body_cap = cfg.upload_max_bytes.saturating_add(UPLOAD_BODY_MARGIN);
    Router::new()
        .route("/bootstrap", get(get_bootstrap))
        .route("/settings", put(put_settings))
        .route("/hosts/{id}", patch(rename_host).delete(delete_host))
        .route("/hosts/{id}/dirs", get(list_host_dirs))
        .route("/agents", post(create_agent))
        .route("/agents/{id}", patch(update_agent).delete(delete_agent))
        .route("/workspaces", post(create_workspace))
        .route("/workspaces/{id}", delete(delete_workspace))
        .route("/workspaces/{id}/git-checkout", post(git_checkout))
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", patch(update_session).delete(delete_session))
        .route("/sessions/{id}/prompts", post(send_prompt))
        .route("/sessions/{id}/cancel", post(cancel_prompt))
        .route("/permissions/{turn_id}", post(reply_permission))
        .route(
            "/files",
            post(upload_acp_file).layer(DefaultBodyLimit::max(body_cap)),
        )
        .route("/files/{id}", get(download_acp_file))
}

async fn uid_of(headers: &HeaderMap, st: &SharedState) -> AppResult<Uuid> {
    Ok(auth::require_user(st, headers).await?.id)
}

async fn get_bootstrap(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<AcpBootstrapView>> {
    let uid = uid_of(&headers, &st).await?;
    Ok(Json(load_bootstrap(&st, uid).await?))
}

#[derive(Deserialize)]
struct PutSettingsReq {
    default_permission: String,
}

async fn put_settings(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<PutSettingsReq>,
) -> AppResult<Json<AcpSettingsView>> {
    let uid = uid_of(&headers, &st).await?;
    let perm = req.default_permission.trim();
    if !matches!(perm, "ask" | "auto_read" | "allow_all") {
        return Err(AppError::bad_request("invalid default_permission"));
    }
    sqlx::query(
        "INSERT INTO acp_user_settings (user_id, default_permission, updated_at)
         VALUES ($1, $2, now())
         ON CONFLICT (user_id)
         DO UPDATE SET default_permission = EXCLUDED.default_permission, updated_at = now()",
    )
    .bind(uid)
    .bind(perm)
    .execute(&st.pool)
    .await?;
    let view = load_settings(&st.pool, uid).await?;
    broadcast_acp(&st, uid, "settings_updated", json!({ "settings": view }));
    Ok(Json(view))
}

#[derive(Deserialize)]
struct RenameHostReq {
    name: String,
}

async fn rename_host(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(host_id): Path<Uuid>,
    Json(req): Json<RenameHostReq>,
) -> AppResult<Json<super::AcpHostView>> {
    let uid = uid_of(&headers, &st).await?;
    let name: String = req.name.trim().chars().take(80).collect();
    if name.is_empty() {
        return Err(AppError::bad_request("host name cannot be empty"));
    }
    let affected = sqlx::query("UPDATE acp_hosts SET name = $2 WHERE id = $1 AND user_id = $3")
        .bind(host_id)
        .bind(&name)
        .bind(uid)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("host not found"));
    }
    let host = load_host(&st, uid, host_id).await?;
    broadcast_acp(&st, uid, "host_upserted", json!({ "host": host }));
    Ok(Json(host))
}

async fn delete_host(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(host_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let affected = sqlx::query("DELETE FROM acp_hosts WHERE id = $1 AND user_id = $2")
        .bind(host_id)
        .bind(uid)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("host not found"));
    }
    if let Some((_, conn)) = st.acp_hosts.remove(&host_id) {
        let _ = conn.tx.try_send(ConnMsg::Shutdown(
            json!({ "type": "force_logout", "reason": "host_removed" }).to_string(),
        ));
    }
    broadcast_acp(&st, uid, "host_deleted", json!({ "host_id": host_id }));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DirQuery {
    path: Option<String>,
}

async fn list_host_dirs(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(host_id): Path<Uuid>,
    Query(q): Query<DirQuery>,
) -> AppResult<Json<Value>> {
    let uid = uid_of(&headers, &st).await?;
    let data = call_host_rpc(
        &st,
        uid,
        host_id,
        "list_dir",
        json!({ "path": q.path.unwrap_or_default() }),
    )
    .await?;
    Ok(Json(data))
}

#[derive(Deserialize)]
struct CreateAgentReq {
    host_id: Uuid,
    #[serde(default = "default_custom_preset")]
    preset: String,
    name: String,
    #[serde(default)]
    adapter: Option<String>,
    cmd: String,
    #[serde(default)]
    env: Option<Value>,
}

fn default_custom_preset() -> String {
    "custom".to_string()
}

async fn create_agent(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<CreateAgentReq>,
) -> AppResult<Json<super::AcpAgentView>> {
    let uid = uid_of(&headers, &st).await?;
    let owns = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM acp_hosts WHERE id = $1 AND user_id = $2)",
    )
    .bind(req.host_id)
    .bind(uid)
    .fetch_one(&st.pool)
    .await?;
    if !owns {
        return Err(AppError::not_found("host not found"));
    }
    let name: String = req.name.trim().chars().take(80).collect();
    let cmd: String = req.cmd.trim().chars().take(500).collect();
    if name.is_empty() || cmd.is_empty() {
        return Err(AppError::bad_request("name and cmd are required"));
    }
    let defaults = preset_defaults(req.preset.trim());
    let adapter = req
        .adapter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(defaults.adapter);
    let env_json = match req.env {
        Some(v) if v.is_object() => v,
        _ => json!({}),
    };

    // 如果宿主机在线，尝试探测本地可执行文件；探测不阻止创建（离线或待命同样允许预配）
    let status = if st.acp_hosts.contains_key(&req.host_id) {
        match call_host_rpc(
            &st,
            uid,
            req.host_id,
            "probe_agent",
            json!({ "cmd": cmd }),
        )
        .await
        {
            Ok(res) if res["ok"].as_bool() == Some(true) => "online",
            _ => "standby",
        }
    } else {
        "standby"
    };

    let agent_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO acp_agents
             (user_id, acp_host_id, preset, name, adapter, cmd, env_json, short_tag, badge_bg,
              default_model, model_options, permission_override, capabilities, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'inherit', $12, $13)
         RETURNING id",
    )
    .bind(uid)
    .bind(req.host_id)
    .bind(req.preset.trim())
    .bind(&name)
    .bind(adapter)
    .bind(&cmd)
    .bind(&env_json)
    .bind(defaults.short_tag)
    .bind(defaults.badge_bg)
    .bind(defaults.default_model)
    .bind(json!(defaults.model_options))
    .bind(json!(defaults.capabilities))
    .bind(status)
    .fetch_one(&st.pool)
    .await?;

    let view = load_agent(&st.pool, uid, agent_id).await?;
    broadcast_acp(&st, uid, "agent_upserted", json!({ "agent": view }));
    Ok(Json(view))
}

#[derive(Deserialize)]
struct UpdateAgentReq {
    name: Option<String>,
    cmd: Option<String>,
    default_model: Option<String>,
    model_options: Option<Vec<String>>,
    permission_override: Option<String>,
}

async fn update_agent(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(agent_id): Path<Uuid>,
    Json(req): Json<UpdateAgentReq>,
) -> AppResult<Json<super::AcpAgentView>> {
    let uid = uid_of(&headers, &st).await?;
    let cur = load_agent(&st.pool, uid, agent_id).await?;

    let name = match req.name {
        Some(n) => {
            let trimmed: String = n.trim().chars().take(80).collect();
            if trimmed.is_empty() {
                return Err(AppError::bad_request("agent name cannot be empty"));
            }
            trimmed
        }
        None => cur.name,
    };
    let cmd = match req.cmd {
        Some(c) => {
            let trimmed: String = c.trim().chars().take(500).collect();
            if trimmed.is_empty() {
                return Err(AppError::bad_request("agent cmd cannot be empty"));
            }
            trimmed
        }
        None => cur.cmd,
    };
    let mut model_options = req.model_options.unwrap_or(cur.model_options);
    let default_model = match req.default_model {
        Some(m) => {
            let trimmed = m.trim().to_string();
            if !trimmed.is_empty() && !model_options.contains(&trimmed) {
                model_options.push(trimmed.clone());
            }
            trimmed
        }
        None => cur.default_model,
    };
    let permission_override = match req.permission_override {
        Some(p) => {
            let trimmed = p.trim().to_string();
            if !matches!(
                trimmed.as_str(),
                "inherit" | "ask" | "auto_read" | "allow_all"
            ) {
                return Err(AppError::bad_request("invalid permission_override"));
            }
            trimmed
        }
        None => cur.permission_override,
    };

    sqlx::query(
        "UPDATE acp_agents
            SET name = $2, cmd = $3, default_model = $4, model_options = $5,
                permission_override = $6, updated_at = now()
          WHERE id = $1 AND user_id = $7",
    )
    .bind(agent_id)
    .bind(&name)
    .bind(&cmd)
    .bind(&default_model)
    .bind(json!(model_options))
    .bind(&permission_override)
    .bind(uid)
    .execute(&st.pool)
    .await?;

    let view = load_agent(&st.pool, uid, agent_id).await?;
    broadcast_acp(&st, uid, "agent_upserted", json!({ "agent": view }));
    Ok(Json(view))
}

async fn delete_agent(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(agent_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let affected = sqlx::query("DELETE FROM acp_agents WHERE id = $1 AND user_id = $2")
        .bind(agent_id)
        .bind(uid)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("agent not found"));
    }
    broadcast_acp(&st, uid, "agent_deleted", json!({ "agent_id": agent_id }));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CreateWorkspaceReq {
    host_id: Uuid,
    cwd: String,
    #[serde(default)]
    name: Option<String>,
}

async fn create_workspace(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<CreateWorkspaceReq>,
) -> AppResult<Json<super::AcpWorkspaceView>> {
    let uid = uid_of(&headers, &st).await?;
    let owns = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM acp_hosts WHERE id = $1 AND user_id = $2)",
    )
    .bind(req.host_id)
    .bind(uid)
    .fetch_one(&st.pool)
    .await?;
    if !owns {
        return Err(AppError::not_found("host not found"));
    }
    let mut cwd = req.cwd.trim().to_string();
    if cwd.is_empty() || cwd.len() > 500 {
        return Err(AppError::bad_request("invalid cwd path"));
    }

    let mut branch = String::new();
    let mut branches: Vec<String> = Vec::new();
    if st.acp_hosts.contains_key(&req.host_id) {
        let probed = call_host_rpc(
            &st,
            uid,
            req.host_id,
            "probe_workspace",
            json!({ "cwd": cwd }),
        )
        .await?;
        if let Some(canon) = probed["cwd"].as_str().filter(|s| !s.is_empty()) {
            cwd = canon.to_string();
        }
        if let Some(b) = probed["branch"].as_str() {
            branch = b.to_string();
        }
        if let Ok(bs) = serde_json::from_value::<Vec<String>>(probed["branches"].clone()) {
            branches = bs;
        }
    }

    let name = derive_workspace_name(&cwd, req.name.as_deref());
    let ws_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO acp_workspaces (user_id, acp_host_id, cwd, name, branch, branches)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, acp_host_id, cwd)
         DO UPDATE SET
             name = CASE WHEN $7 THEN EXCLUDED.name ELSE acp_workspaces.name END,
             branch = CASE WHEN EXCLUDED.branch <> '' THEN EXCLUDED.branch ELSE acp_workspaces.branch END,
             branches = CASE WHEN jsonb_array_length(EXCLUDED.branches) > 0 THEN EXCLUDED.branches ELSE acp_workspaces.branches END,
             updated_at = now()
         RETURNING id",
    )
    .bind(uid)
    .bind(req.host_id)
    .bind(&cwd)
    .bind(&name)
    .bind(&branch)
    .bind(json!(branches))
    .bind(req.name.as_deref().is_some_and(|s| !s.trim().is_empty()))
    .fetch_one(&st.pool)
    .await?;

    let view = load_workspace(&st.pool, uid, ws_id).await?;
    broadcast_acp(&st, uid, "workspace_upserted", json!({ "workspace": view }));
    Ok(Json(view))
}

async fn delete_workspace(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(ws_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let affected = sqlx::query("DELETE FROM acp_workspaces WHERE id = $1 AND user_id = $2")
        .bind(ws_id)
        .bind(uid)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("workspace not found"));
    }
    broadcast_acp(
        &st,
        uid,
        "workspace_deleted",
        json!({ "workspace_id": ws_id }),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct GitCheckoutReq {
    branch: String,
}

async fn git_checkout(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(ws_id): Path<Uuid>,
    Json(req): Json<GitCheckoutReq>,
) -> AppResult<Json<super::AcpWorkspaceView>> {
    let uid = uid_of(&headers, &st).await?;
    let ws = load_workspace(&st.pool, uid, ws_id).await?;
    let branch = req.branch.trim();
    if branch.is_empty() {
        return Err(AppError::bad_request("branch cannot be empty"));
    }

    let data = call_host_rpc(
        &st,
        uid,
        ws.host_id,
        "git_checkout",
        json!({ "cwd": ws.cwd, "branch": branch }),
    )
    .await?;
    let next_branch = data["branch"].as_str().unwrap_or(branch).to_string();
    let next_branches: Vec<String> = serde_json::from_value(data["branches"].clone())
        .ok()
        .filter(|b: &Vec<String>| !b.is_empty())
        .unwrap_or(ws.branches);

    sqlx::query(
        "UPDATE acp_workspaces SET branch = $2, branches = $3, updated_at = now()
          WHERE id = $1 AND user_id = $4",
    )
    .bind(ws_id)
    .bind(&next_branch)
    .bind(json!(next_branches))
    .bind(uid)
    .execute(&st.pool)
    .await?;

    let updated = load_workspace(&st.pool, uid, ws_id).await?;
    broadcast_acp(
        &st,
        uid,
        "workspace_upserted",
        json!({ "workspace": updated }),
    );
    Ok(Json(updated))
}

#[derive(Deserialize)]
struct CreateSessionReq {
    workspace_id: Uuid,
    agent_id: Uuid,
    #[serde(default)]
    title: Option<String>,
}

async fn create_session(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<CreateSessionReq>,
) -> AppResult<Json<super::AcpSessionView>> {
    let uid = uid_of(&headers, &st).await?;
    let ws = load_workspace(&st.pool, uid, req.workspace_id).await?;
    let ag = load_agent(&st.pool, uid, req.agent_id).await?;
    if ws.host_id != ag.host_id {
        return Err(AppError::bad_request(
            "workspace and agent must belong to the same host",
        ));
    }

    let settings = load_settings(&st.pool, uid).await?;
    let perm = if ag.permission_override != "inherit" {
        ag.permission_override.clone()
    } else {
        settings.default_permission
    };
    let title: String = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(160).collect())
        .unwrap_or_else(|| format!("{} · {}", ag.name, ws.name));

    let row = sqlx::query(
        "INSERT INTO acp_sessions
             (user_id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level, permission_policy)
         VALUES ($1, $2, $3, $4, $5, '中', $6)
         RETURNING id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                   permission_policy, acp_session_key, running, diffs_json, rpc_logs_json,
                   created_at, updated_at",
    )
    .bind(uid)
    .bind(ws.id)
    .bind(ag.id)
    .bind(&title)
    .bind(&ag.default_model)
    .bind(&perm)
    .fetch_one(&st.pool)
    .await?;

    let view = super::session_from_row(&row, Some(Vec::new()));
    broadcast_acp(&st, uid, "session_upserted", json!({ "session": view }));
    Ok(Json(view))
}

#[derive(Deserialize)]
struct UpdateSessionReq {
    title: Option<String>,
    selected_model: Option<String>,
    thinking_level: Option<String>,
    permission_policy: Option<String>,
}

async fn update_session(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(sess_id): Path<Uuid>,
    Json(req): Json<UpdateSessionReq>,
) -> AppResult<Json<super::AcpSessionView>> {
    let uid = uid_of(&headers, &st).await?;
    let cur = load_session_meta(&st.pool, uid, sess_id).await?;

    let title = match req.title {
        Some(t) => {
            let trimmed: String = t.trim().chars().take(160).collect();
            if trimmed.is_empty() {
                return Err(AppError::bad_request("title cannot be empty"));
            }
            trimmed
        }
        None => cur.title,
    };
    let selected_model = req
        .selected_model
        .map(|s| s.trim().to_string())
        .unwrap_or(cur.selected_model);
    let thinking_level = match req.thinking_level {
        Some(lv) => {
            if !matches!(lv.as_str(), "关" | "低" | "中" | "高") {
                return Err(AppError::bad_request("invalid thinking_level"));
            }
            lv
        }
        None => cur.thinking_level,
    };
    let permission_policy = match req.permission_policy {
        Some(p) => {
            if !matches!(p.as_str(), "ask" | "auto_read" | "allow_all") {
                return Err(AppError::bad_request("invalid permission_policy"));
            }
            p
        }
        None => cur.permission_policy,
    };

    sqlx::query(
        "UPDATE acp_sessions
            SET title = $2, selected_model = $3, thinking_level = $4,
                permission_policy = $5, updated_at = now()
          WHERE id = $1 AND user_id = $6",
    )
    .bind(sess_id)
    .bind(&title)
    .bind(&selected_model)
    .bind(&thinking_level)
    .bind(&permission_policy)
    .bind(uid)
    .execute(&st.pool)
    .await?;

    let updated = load_session_meta(&st.pool, uid, sess_id).await?;
    broadcast_acp(&st, uid, "session_upserted", json!({ "session": updated }));
    Ok(Json(updated))
}

async fn delete_session(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(sess_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let affected = sqlx::query("DELETE FROM acp_sessions WHERE id = $1 AND user_id = $2")
        .bind(sess_id)
        .bind(uid)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("session not found"));
    }
    broadcast_acp(
        &st,
        uid,
        "session_deleted",
        json!({ "session_id": sess_id }),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SendPromptReq {
    text: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    attachment_ids: Vec<Uuid>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    thinking_level: Option<String>,
    #[serde(default)]
    permission_policy: Option<String>,
}

async fn send_prompt(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(sess_id): Path<Uuid>,
    Json(req): Json<SendPromptReq>,
) -> AppResult<Json<super::AcpTurnView>> {
    let uid = uid_of(&headers, &st).await?;
    let sess = load_session_meta(&st.pool, uid, sess_id).await?;
    let ws = load_workspace(&st.pool, uid, sess.workspace_id).await?;
    let ag = load_agent(&st.pool, uid, sess.agent_id).await?;

    if sess.running {
        return Err(AppError::conflict("session is already running"));
    }
    let text = req.text.trim().to_string();
    if text.is_empty() && req.files.is_empty() && req.attachment_ids.is_empty() {
        return Err(AppError::bad_request("prompt cannot be empty"));
    }

    let model = req
        .model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(sess.selected_model);
    let thinking_level = req
        .thinking_level
        .filter(|lv| matches!(lv.as_str(), "关" | "低" | "中" | "高"))
        .unwrap_or(sess.thinking_level);
    let permission_policy = req
        .permission_policy
        .filter(|p| matches!(p.as_str(), "ask" | "auto_read" | "allow_all"))
        .unwrap_or(sess.permission_policy);

    let host_tx = {
        let Some(conn) = st.acp_hosts.get(&ws.host_id) else {
            return Err(AppError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "host_offline",
                "工作目录所在的机器节点当前离线",
            ));
        };
        if conn.uid != uid {
            return Err(AppError::not_found("host not found"));
        }
        conn.tx.clone()
    };

    // 读取随附上传文件（若 < INLINE_ATTACHMENT_LIMIT 则直接内联 base64/text 给宿主）
    let mut attachments_meta = Vec::new();
    let mut host_attachments = Vec::new();
    if !req.attachment_ids.is_empty() {
        let rows = sqlx::query(
            "SELECT id, file_name, mime, size, storage_path
               FROM acp_attachments WHERE user_id = $1 AND id = ANY($2)",
        )
        .bind(uid)
        .bind(&req.attachment_ids)
        .fetch_all(&st.pool)
        .await?;
        for r in rows {
            let att_id: Uuid = r.get("id");
            let file_name: String = r.get("file_name");
            let mime: String = r.get("mime");
            let size: i64 = r.get("size");
            let stored: String = r.get("storage_path");
            attachments_meta.push(json!({
                "id": att_id,
                "file_name": file_name,
                "mime": mime,
                "size": size,
            }));
            let mut inline_b64: Option<String> = None;
            if (size as usize) <= INLINE_ATTACHMENT_LIMIT {
                let p = stored_file_path(&st.cfg.files_dir, &stored);
                if let Ok(bytes) = tokio::fs::read(p).await {
                    inline_b64 = Some(base64_encode(&bytes));
                }
            }
            host_attachments.push(json!({
                "id": att_id,
                "file_name": file_name,
                "mime": mime,
                "size": size,
                "data_base64": inline_b64,
            }));
        }
        let _ = sqlx::query(
            "UPDATE acp_attachments SET acp_session_id = $2
              WHERE user_id = $1 AND id = ANY($3) AND acp_session_id IS NULL",
        )
        .bind(uid)
        .bind(sess_id)
        .bind(&req.attachment_ids)
        .execute(&st.pool)
        .await;
    }

    sqlx::query(
        "UPDATE acp_sessions
            SET running = true,
                selected_model = $2,
                thinking_level = $3,
                permission_policy = $4,
                updated_at = now()
          WHERE id = $1 AND user_id = $5",
    )
    .bind(sess_id)
    .bind(&model)
    .bind(&thinking_level)
    .bind(&permission_policy)
    .bind(uid)
    .execute(&st.pool)
    .await?;

    let turn_row = insert_turn(
        &st,
        sess_id,
        uid,
        "user",
        "done",
        json!({
            "text": text,
            "files": req.files,
            "attachments": attachments_meta,
        }),
    )
    .await
    .ok_or_else(|| AppError::internal("failed to store prompt turn"))?;
    let turn_view = turn_from_row(&turn_row);

    broadcast_acp(
        &st,
        uid,
        "turn_upserted",
        json!({ "session_id": sess_id, "turn": turn_view }),
    );
    if let Ok(updated_sess) = load_session_meta(&st.pool, uid, sess_id).await {
        broadcast_acp(
            &st,
            uid,
            "session_upserted",
            json!({ "session": updated_sess }),
        );
    }

    let start_frame = json!({
        "type": "prompt_start",
        "session_id": sess_id,
        "acp_session_key": sess.acp_session_key,
        "cwd": ws.cwd,
        "agent": {
            "id": ag.id,
            "preset": ag.preset,
            "name": ag.name,
            "cmd": ag.cmd,
            "env": ag.env,
        },
        "model": model,
        "thinking_level": thinking_level,
        "permission_policy": permission_policy,
        "text": text,
        "files": req.files,
        "attachments": host_attachments,
    })
    .to_string();

    if host_tx.try_send(ConnMsg::Frame(start_frame)).is_err() {
        let _ = sqlx::query("UPDATE acp_sessions SET running = false WHERE id = $1")
            .bind(sess_id)
            .execute(&st.pool)
            .await;
        if let Ok(updated_sess) = load_session_meta(&st.pool, uid, sess_id).await {
            broadcast_acp(
                &st,
                uid,
                "session_upserted",
                json!({ "session": updated_sess }),
            );
        }
        return Err(AppError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_offline",
            "无法送达指令到机器节点",
        ));
    }

    Ok(Json(turn_view))
}

async fn cancel_prompt(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(sess_id): Path<Uuid>,
) -> AppResult<Json<super::AcpSessionView>> {
    let uid = uid_of(&headers, &st).await?;
    let sess = load_session_meta(&st.pool, uid, sess_id).await?;
    let ws = load_workspace(&st.pool, uid, sess.workspace_id).await?;

    if let Some(conn) = st.acp_hosts.get(&ws.host_id) {
        if conn.uid == uid {
            let _ = conn.tx.try_send(ConnMsg::Frame(
                json!({ "type": "prompt_cancel", "session_id": sess_id }).to_string(),
            ));
        }
    }

    sqlx::query("UPDATE acp_sessions SET running = false, updated_at = now() WHERE id = $1")
        .bind(sess_id)
        .execute(&st.pool)
        .await?;
    let updated = load_session_meta(&st.pool, uid, sess_id).await?;
    broadcast_acp(&st, uid, "session_upserted", json!({ "session": updated }));
    Ok(Json(updated))
}

#[derive(Deserialize)]
struct PermissionReplyReq {
    option_id: String,
}

async fn reply_permission(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(turn_id): Path<Uuid>,
    Json(req): Json<PermissionReplyReq>,
) -> AppResult<Json<super::AcpTurnView>> {
    let uid = uid_of(&headers, &st).await?;
    let option_id = req.option_id.trim();
    if !matches!(
        option_id,
        "allow_once" | "allow_always" | "reject_once" | "reject_always"
    ) {
        return Err(AppError::bad_request("invalid option_id"));
    }

    let row = sqlx::query(
        "SELECT t.id, t.acp_session_id, t.role, t.status, t.payload, w.acp_host_id
           FROM acp_turns t
           JOIN acp_sessions s ON s.id = t.acp_session_id
           JOIN acp_workspaces w ON w.id = s.acp_workspace_id
          WHERE t.id = $1 AND t.user_id = $2",
    )
    .bind(turn_id)
    .bind(uid)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("permission request not found"))?;

    let role: String = row.get("role");
    let status: String = row.get("status");
    let sess_id: Uuid = row.get("acp_session_id");
    let host_id: Uuid = row.get("acp_host_id");
    let mut payload: Value = row.get("payload");
    if role != "permission" {
        return Err(AppError::bad_request("turn is not a permission request"));
    }
    if status != "pending" {
        return Err(AppError::conflict("permission already resolved"));
    }

    let req_id = payload["req_id"].as_str().unwrap_or("").to_string();
    let new_status = if option_id.starts_with("allow") {
        "approved"
    } else {
        "rejected"
    };
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("chosen_option".to_string(), json!(option_id));
    }

    let updated_row = sqlx::query(
        "UPDATE acp_turns SET status = $2, payload = $3, updated_at = now()
          WHERE id = $1
          RETURNING id, acp_session_id, seq, role, status, payload, created_at, updated_at",
    )
    .bind(turn_id)
    .bind(new_status)
    .bind(&payload)
    .fetch_one(&st.pool)
    .await?;
    let turn_view = turn_from_row(&updated_row);
    broadcast_acp(
        &st,
        uid,
        "turn_upserted",
        json!({ "session_id": sess_id, "turn": turn_view }),
    );

    if let Some(conn) = st.acp_hosts.get(&host_id) {
        if conn.uid == uid {
            let _ = conn.tx.try_send(ConnMsg::Frame(
                json!({
                    "type": "permission_reply",
                    "session_id": sess_id,
                    "req_id": req_id,
                    "option_id": option_id,
                })
                .to_string(),
            ));
        }
    }

    Ok(Json(turn_view))
}

async fn upload_acp_file(
    State(st): State<SharedState>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> AppResult<Json<AcpAttachmentView>> {
    let uid = uid_of(&headers, &st).await?;
    let (quota_mb, quota_bytes, used_bytes) = upload_quota(&st, uid).await?;
    let rd = read_upload(
        &st.cfg.files_dir,
        &mut mp,
        st.cfg.upload_max_bytes,
        quota_bytes,
        used_bytes,
        &["session_id"],
    )
    .await?;
    if rd.too_large {
        return Err(AppError::bad_request("file too large"));
    }
    if rd.over_quota {
        return Err(AppError::payload_too_large(quota_message(
            used_bytes, quota_mb,
        )));
    }
    let Some(file) = rd.file.as_ref().filter(|_| rd.has_file()) else {
        return Err(AppError::bad_request("missing file"));
    };

    let sess_id = match rd.fields.get("session_id") {
        Some(raw) if !raw.trim().is_empty() => {
            let parsed = Uuid::parse_str(raw.trim())
                .map_err(|_| AppError::bad_request("invalid session_id"))?;
            let owns = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM acp_sessions WHERE id = $1 AND user_id = $2)",
            )
            .bind(parsed)
            .bind(uid)
            .fetch_one(&st.pool)
            .await?;
            if !owns {
                return Err(AppError::not_found("session not found"));
            }
            Some(parsed)
        }
        _ => None,
    };

    let id = Uuid::new_v4();
    let stored = format!("acp-{id}");
    let dest = stored_file_path(&st.cfg.files_dir, &stored);
    tokio::fs::rename(&file.path, &dest)
        .await
        .map_err(|e| AppError::internal(format!("store acp file: {e}")))?;

    let size = file.size as i64;
    sqlx::query(
        "INSERT INTO acp_attachments (id, user_id, acp_session_id, file_name, mime, size, storage_path)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(uid)
    .bind(sess_id)
    .bind(&rd.file_name)
    .bind(&rd.mime)
    .bind(size)
    .bind(&stored)
    .execute(&st.pool)
    .await?;

    Ok(Json(AcpAttachmentView {
        id,
        file_name: rd.file_name,
        mime: rd.mime,
        size,
    }))
}

#[derive(Deserialize)]
struct AcpFileQuery {
    dl: Option<u8>,
}

async fn download_acp_file(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<AcpFileQuery>,
    req: axum::extract::Request,
) -> AppResult<axum::response::Response> {
    let uid = uid_of(&headers, &st).await?;
    let (file_name, mime, stored) = sqlx::query_as::<_, (String, String, String)>(
        "SELECT file_name, mime, storage_path
           FROM acp_attachments WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(uid)
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::not_found("file not found"))?;

    let mode = if q.dl != Some(1) && inline_safe(&mime) {
        "inline"
    } else {
        "attachment"
    };
    let dispo = format!("{mode}; filename*=UTF-8''{}", pct_encode(&file_name));
    serve_stored(
        stored_file_path(&st.cfg.files_dir, &stored),
        req,
        &mime,
        &dispo,
        "private, no-store",
    )
    .await
}
