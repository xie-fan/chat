use super::{
    agent_from_row_inner, broadcast_acp, delete_acp_attachment_files, host_from_row,
    session_from_row, turn_from_row, workspace_from_row, AcpAgentView, AcpHostView, AcpSessionView,
    AcpWorkspaceView,
};
use crate::api::stored_file_path;
use crate::auth;
use crate::error::{AppError, AppResult};
use crate::state::{ConnMsg, SharedState};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct SnapshotAttachment {
    id: Uuid,
    session_id: Uuid,
    file_name: String,
    mime: String,
    size: i64,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    host: AcpHostView,
    agents: Vec<AcpAgentView>,
    workspaces: Vec<AcpWorkspaceView>,
    sessions: Vec<AcpSessionView>,
    attachments: Vec<SnapshotAttachment>,
}

#[derive(Serialize)]
pub(super) struct Export {
    revision: String,
    import_id: Option<Uuid>,
    snapshot: Snapshot,
}

/// Lock parents before children. Foreign keys prevent new children from slipping
/// between this snapshot and deletion, and updates to captured rows must wait.
async fn load(
    st: &SharedState,
    conn: &mut PgConnection,
    uid: Uuid,
    install_id: Uuid,
) -> AppResult<Option<(Export, Vec<String>)>> {
    let Some(host_row) = sqlx::query(
        "SELECT id, name, os, ip, roots_json, online, latency_ms, last_seen, snapshot_import_id
           FROM acp_hosts WHERE user_id = $1 AND install_id = $2 FOR UPDATE",
    )
    .bind(uid)
    .bind(install_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let host = host_from_row(st, &host_row);
    let agents = sqlx::query(
        "SELECT id, acp_host_id, preset, name, adapter, cmd, env_json, short_tag, badge_bg,
                default_model, model_options, permission_override, capabilities, status,
                created_at, updated_at
           FROM acp_agents WHERE user_id = $1 AND acp_host_id = $2 ORDER BY id FOR UPDATE",
    )
    .bind(uid)
    .bind(host.id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|row| agent_from_row_inner(row, false))
    .collect();
    let workspaces: Vec<_> = sqlx::query(
        "SELECT id, acp_host_id, cwd, name, branch, branches, created_at, updated_at
           FROM acp_workspaces WHERE user_id = $1 AND acp_host_id = $2 ORDER BY id FOR UPDATE",
    )
    .bind(uid)
    .bind(host.id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(workspace_from_row)
    .collect();
    let workspace_ids: Vec<_> = workspaces.iter().map(|ws| ws.id).collect();
    let mut sessions: Vec<_> = sqlx::query(
        "SELECT id, acp_workspace_id, acp_agent_id, title, selected_model, thinking_level,
                permission_policy, acp_session_key, running, run_id, diffs_json, rpc_logs_json,
                created_at, updated_at
           FROM acp_sessions WHERE user_id = $1 AND acp_workspace_id = ANY($2)
          ORDER BY id FOR UPDATE",
    )
    .bind(uid)
    .bind(&workspace_ids)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|row| session_from_row(row, Some(Vec::new())))
    .collect();
    let session_ids: Vec<_> = sessions.iter().map(|session| session.id).collect();
    let turn_rows = sqlx::query(
        "SELECT id, acp_session_id, seq, role, status, payload, created_at, updated_at
           FROM acp_turns WHERE user_id = $1 AND acp_session_id = ANY($2)
          ORDER BY acp_session_id, seq FOR UPDATE",
    )
    .bind(uid)
    .bind(&session_ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut turns = HashMap::<_, Vec<_>>::new();
    for row in &turn_rows {
        let turn = turn_from_row(row);
        turns.entry(turn.session_id).or_default().push(turn);
    }
    for session in &mut sessions {
        session.turns = Some(turns.remove(&session.id).unwrap_or_default());
    }
    let attachment_rows = sqlx::query(
        "SELECT id, acp_session_id, file_name, mime, size, storage_path
           FROM acp_attachments WHERE user_id = $1 AND acp_session_id = ANY($2)
          ORDER BY id FOR UPDATE",
    )
    .bind(uid)
    .bind(&session_ids)
    .fetch_all(conn)
    .await?;
    let attachments = attachment_rows
        .iter()
        .map(|row| SnapshotAttachment {
            id: row.get("id"),
            session_id: row.get("acp_session_id"),
            file_name: row.get("file_name"),
            mime: row.get("mime"),
            size: row.get("size"),
        })
        .collect();
    let paths = attachment_rows
        .iter()
        .map(|row| row.get("storage_path"))
        .collect();
    let snapshot = Snapshot {
        host,
        agents,
        workspaces,
        sessions,
        attachments,
    };
    // Connection heartbeat fields are independent of the data being transferred.
    let bytes = serde_json::to_vec(&(
        snapshot.host.id,
        &snapshot.host.name,
        &snapshot.agents,
        &snapshot.workspaces,
        &snapshot.sessions,
        &snapshot.attachments,
    ))
    .map_err(AppError::internal)?;
    Ok(Some((
        Export {
            revision: hex::encode(Sha256::digest(bytes)),
            import_id: host_row.get("snapshot_import_id"),
            snapshot,
        },
        paths,
    )))
}

pub(super) async fn export_snapshot(
    State(st): State<SharedState>,
    Path(install_id): Path<Uuid>,
    headers: HeaderMap,
) -> AppResult<(
    [(header::HeaderName, &'static str); 1],
    Json<Option<Export>>,
)> {
    let uid = auth::require_user(&st, &headers).await?.id;
    let mut conn = st.pool.begin().await?;
    let exported = load(&st, &mut conn, uid, install_id)
        .await?
        .map(|(snapshot, _)| snapshot);
    conn.commit().await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(exported),
    ))
}

#[derive(Deserialize)]
pub(super) struct DeleteQuery {
    revision: String,
}

pub(super) async fn delete_snapshot(
    State(st): State<SharedState>,
    Path(install_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<DeleteQuery>,
) -> AppResult<StatusCode> {
    let uid = auth::require_user(&st, &headers).await?.id;
    let Some(host_id) = host_id_for_install(&st, uid, install_id).await? else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let _host_guard = st.lock_acp_host(host_id).await;
    let mut conn = st.pool.begin().await?;
    let Some((exported, paths)) = load(&st, &mut conn, uid, install_id).await? else {
        return Ok(StatusCode::NO_CONTENT);
    };
    if exported.snapshot.host.id != host_id
        || exported.revision != query.revision
        || exported
            .snapshot
            .sessions
            .iter()
            .any(|session| session.running)
    {
        return Err(AppError::conflict(
            "云端 ACP 数据已变化，请重新下载快照后再切换",
        ));
    }
    sqlx::query("DELETE FROM acp_hosts WHERE user_id = $1 AND id = $2")
        .bind(uid)
        .bind(host_id)
        .execute(&mut *conn)
        .await?;
    conn.commit().await?;
    if let Some((_, conn)) = st.acp_hosts.remove(&host_id) {
        let _ = conn.tx.try_send(ConnMsg::Shutdown(
            json!({ "type": "force_logout", "reason": "host_removed" }).to_string(),
        ));
    }
    st.fail_acp_host_rpcs(host_id);
    delete_acp_attachment_files(&st.cfg.files_dir, &paths).await;
    broadcast_acp(&st, uid, "host_deleted", json!({ "host_id": host_id }));
    Ok(StatusCode::NO_CONTENT)
}

async fn host_id_for_install(
    st: &SharedState,
    uid: Uuid,
    install_id: Uuid,
) -> AppResult<Option<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM acp_hosts WHERE user_id = $1 AND install_id = $2")
            .bind(uid)
            .bind(install_id)
            .fetch_optional(&st.pool)
            .await?,
    )
}

#[derive(Deserialize)]
pub(super) struct Import {
    transfer_id: Uuid,
    snapshot: Snapshot,
}

pub(super) async fn import_snapshot(
    State(st): State<SharedState>,
    Path(install_id): Path<Uuid>,
    headers: HeaderMap,
    Json(import): Json<Import>,
) -> AppResult<Json<serde_json::Value>> {
    let uid = auth::require_user(&st, &headers).await?.id;
    let host_id = host_id_for_install(&st, uid, install_id)
        .await?
        .ok_or_else(|| AppError::not_found("宿主机器尚未连接"))?;
    let _host_guard = st.lock_acp_host(host_id).await;
    let mut conn = st.pool.begin().await?;
    let target = sqlx::query(
        "SELECT snapshot_import_id,
                EXISTS(SELECT 1 FROM acp_agents WHERE acp_host_id = $1)
                OR EXISTS(SELECT 1 FROM acp_workspaces WHERE acp_host_id = $1) AS has_data
           FROM acp_hosts WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(host_id)
    .bind(uid)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::not_found("宿主机器尚未连接"))?;
    let receipt: Option<Uuid> = target.get("snapshot_import_id");
    if receipt == Some(import.transfer_id) {
        return Ok(Json(
            json!({ "host_id": host_id, "transfer_id": import.transfer_id }),
        ));
    }
    if target.get::<bool, _>("has_data") {
        return Err(AppError::conflict("云端宿主已存在数据，本地快照已保留"));
    }
    let Snapshot {
        host,
        agents,
        workspaces,
        sessions,
        attachments,
    } = import.snapshot;
    let agent_ids: HashSet<_> = agents.iter().map(|agent| agent.id).collect();
    let workspace_ids: HashSet<_> = workspaces.iter().map(|ws| ws.id).collect();
    let session_ids: HashSet<_> = sessions.iter().map(|session| session.id).collect();
    if sessions.iter().any(|session| {
        session.running
            || !agent_ids.contains(&session.agent_id)
            || !workspace_ids.contains(&session.workspace_id)
    }) || attachments
        .iter()
        .any(|att| !session_ids.contains(&att.session_id))
    {
        return Err(AppError::bad_request("ACP 快照包含进行中的任务或无效引用"));
    }
    let attachment_ids: HashSet<_> = attachments.iter().map(|att| att.id).collect();
    for session in &sessions {
        for turn in session.turns.as_deref().unwrap_or_default() {
            if turn.session_id != session.id
                || turn.payload["attachments"].as_array().is_some_and(|refs| {
                    refs.iter().any(|att| {
                        !att["id"]
                            .as_str()
                            .and_then(|id| Uuid::parse_str(id).ok())
                            .is_some_and(|id| attachment_ids.contains(&id))
                    })
                })
            {
                return Err(AppError::bad_request(
                    "ACP 快照包含无效的会话记录或附件引用",
                ));
            }
        }
    }
    for agent in agents {
        sqlx::query(
            "INSERT INTO acp_agents (id, user_id, acp_host_id, preset, name, adapter, cmd,
                 env_json, short_tag, badge_bg, default_model, model_options, permission_override,
                 capabilities, status, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(agent.id)
        .bind(uid)
        .bind(host_id)
        .bind(agent.preset)
        .bind(agent.name)
        .bind(agent.adapter)
        .bind(agent.cmd)
        .bind(agent.env)
        .bind(agent.short_tag)
        .bind(agent.badge_bg)
        .bind(agent.default_model)
        .bind(json!(agent.model_options))
        .bind(agent.permission_override)
        .bind(json!(agent.capabilities))
        .bind(agent.status)
        .bind(agent.created_at)
        .bind(agent.updated_at)
        .execute(&mut *conn)
        .await?;
    }
    for workspace in workspaces {
        sqlx::query(
            "INSERT INTO acp_workspaces (id, user_id, acp_host_id, cwd, name, branch,
                 branches, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(workspace.id)
        .bind(uid)
        .bind(host_id)
        .bind(workspace.cwd)
        .bind(workspace.name)
        .bind(workspace.branch)
        .bind(json!(workspace.branches))
        .bind(workspace.created_at)
        .bind(workspace.updated_at)
        .execute(&mut *conn)
        .await?;
    }
    for session in sessions {
        sqlx::query(
            "INSERT INTO acp_sessions (id, user_id, acp_workspace_id, acp_agent_id, title,
                 selected_model, thinking_level, permission_policy, acp_session_key,
                 diffs_json, rpc_logs_json, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(session.id)
        .bind(uid)
        .bind(session.workspace_id)
        .bind(session.agent_id)
        .bind(session.title)
        .bind(session.selected_model)
        .bind(session.thinking_level)
        .bind(session.permission_policy)
        .bind(session.acp_session_key)
        .bind(session.diffs)
        .bind(session.rpc_logs)
        .bind(session.created_at)
        .bind(session.updated_at)
        .execute(&mut *conn)
        .await?;
        for turn in session.turns.unwrap_or_default() {
            sqlx::query(
                "INSERT INTO acp_turns (id, acp_session_id, user_id, seq, role, status,
                     payload, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(turn.id)
            .bind(session.id)
            .bind(uid)
            .bind(turn.seq)
            .bind(turn.role)
            .bind(turn.status)
            .bind(turn.payload)
            .bind(turn.created_at)
            .bind(turn.updated_at)
            .execute(&mut *conn)
            .await?;
        }
    }
    for att in attachments {
        let stored: Option<String> = sqlx::query_scalar(
            "UPDATE acp_attachments SET acp_session_id = $1
              WHERE id = $2 AND user_id = $3 AND acp_session_id IS NULL
                AND size = $4 AND file_name = $5 AND mime = $6 RETURNING storage_path",
        )
        .bind(att.session_id)
        .bind(att.id)
        .bind(uid)
        .bind(att.size)
        .bind(att.file_name)
        .bind(att.mime)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(path) = stored else {
            return Err(AppError::conflict("ACP 快照的附件尚未完整上传"));
        };
        let on_disk = tokio::fs::metadata(stored_file_path(&st.cfg.files_dir, &path))
            .await
            .map(|metadata| metadata.len())
            .ok();
        if on_disk != Some(att.size as u64) {
            return Err(AppError::conflict("ACP 快照的附件尚未完整上传"));
        }
    }
    sqlx::query("UPDATE acp_hosts SET name = $2, snapshot_import_id = $3 WHERE id = $1")
        .bind(host_id)
        .bind(host.name)
        .bind(import.transfer_id)
        .execute(&mut *conn)
        .await?;
    conn.commit().await?;
    broadcast_acp(&st, uid, "bootstrap_changed", json!({}));
    Ok(Json(
        json!({ "host_id": host_id, "transfer_id": import.transfer_id }),
    ))
}
