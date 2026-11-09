use crate::auth;
use crate::domain;
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::model::*;
use crate::settings;
use crate::state::SharedState;
use crate::ws;
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

mod account;
mod admin;
mod avatar;
mod bots;
mod conversations;
mod files;
mod friends;
mod messages;
mod users;

use account::*;
use admin::*;
pub(crate) use avatar::*;
pub(crate) use bots::dispatch as dispatch_bots;
use bots::*;
use conversations::*;
pub(crate) use files::*;
use friends::*;
use messages::*;
use users::*;

/// multipart 边界/表单字段/文件头的额外余量；文件本体仍由流式检查卡 upload_max_bytes
const UPLOAD_BODY_MARGIN: usize = 1024 * 1024;

/// 头像上限。路由单独设 body 上限，不复用附件的 25MB。
pub(crate) const AVATAR_MAX_BYTES: usize = 1024 * 1024;

/// 账号管理操作（角色/禁用）的事务级顾问锁：串行化，防止并发降权越过末位保护
const ADMIN_LOCK_KEY: i64 = 0x63_68_61_74_61_64_6D_69; // "chatadmi"

pub fn router(cfg: &crate::config::Config, st: &SharedState) -> Router<SharedState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/users/me", get(get_me).patch(update_me))
        .route("/users/me/storage", get(my_storage))
        .route(
            "/users/me/avatar",
            put(put_avatar)
                .delete(delete_avatar)
                .layer(DefaultBodyLimit::max(AVATAR_MAX_BYTES + UPLOAD_BODY_MARGIN)),
        )
        .route("/users/{id}/avatar", get(get_avatar))
        .route("/users/me/password", post(crate::sessions::change_password))
        .route("/users/me/sessions", get(crate::sessions::list_sessions))
        .route(
            "/users/me/sessions/revoke-others",
            post(crate::sessions::revoke_other_sessions),
        )
        .route(
            "/users/me/sessions/{id}",
            delete(crate::sessions::revoke_session),
        )
        .route(
            "/users/me/push",
            get(crate::push::get_push).put(crate::push::put_push),
        )
        .route("/users/me/push/devices", post(crate::push::add_device))
        .route(
            "/users/me/push/devices/{id}",
            delete(crate::push::delete_device),
        )
        .route(
            "/users/me/push/devices/{id}/test",
            post(crate::push::test_device),
        )
        .route("/users/search", get(search_user))
        .route("/search", get(global_search))
        .route("/friends", get(list_friends))
        .route("/friends/{user_id}", delete(delete_friend))
        .route("/friends/requests", get(list_requests).post(create_request))
        .route("/friends/requests/{id}/accept", post(accept_request))
        .route("/friends/requests/{id}/decline", post(decline_request))
        .route("/conversations", get(list_conversations))
        .route("/conversations/group", post(create_group))
        .route(
            "/conversations/{id}",
            get(get_conversation)
                .patch(rename_conversation)
                .delete(dissolve_conversation),
        )
        .route("/conversations/{id}/messages", get(list_messages))
        .route("/conversations/{id}/messages/search", get(search_messages))
        .route(
            "/conversations/{id}/messages/refresh",
            post(refresh_messages),
        )
        .route("/conversations/{id}/preferences", put(put_preferences))
        .route("/conversations/{id}/announcement", put(put_announcement))
        .route("/conversations/{id}/pins", get(list_pins))
        .route(
            "/conversations/{id}/pins/{message_id}",
            put(pin_message).delete(unpin_message),
        )
        .route(
            "/messages/{id}",
            // DELETE 保留为旧客户端的撤回别名，语义与 POST recall 完全一致
            axum::routing::patch(edit_message).delete(recall_message),
        )
        .route("/messages/{id}/recall", post(recall_message))
        .route(
            "/messages/{id}/reactions/{emoji}",
            put(reaction_add).delete(reaction_remove),
        )
        .route("/conversations/{id}/polls", post(create_poll))
        .route("/messages/{id}/poll/votes", put(vote_poll))
        .route("/messages/{id}/poll/close", post(close_poll))
        .route("/messages/{id}/forward", post(forward_message))
        // MF1：记录完整快照 + 批量/合并转发（64KiB 上限：100 uuid + 10 target + 外壳足够）
        .route("/messages/{id}/record", get(get_record))
        .route(
            "/messages/forward-batch",
            post(forward_batch).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/conversations/{id}/members", post(add_members))
        .route("/conversations/{id}/members/me", delete(leave_conversation))
        .route("/conversations/{id}/members/{uid}", delete(kick_member))
        .route("/conversations/{id}/transfer", post(transfer_ownership))
        .route("/bots", get(list_bots).post(create_bot))
        .route("/bots/{id}", patch(patch_bot).delete(delete_bot))
        .route("/bots/{id}/secret", post(reset_bot_secret))
        .route("/bots/{id}/tokens", post(create_bot_token))
        .route("/bots/{id}/tokens/{token_id}", delete(delete_bot_token))
        // Bot API（描述见 /bot/openapi.json）：bot 令牌能调的就是下面合进来的这几条。
        // bot_auth 把令牌换成 bot 自己的身份，处理函数与用户接口共用；用户凭据在这里不认。
        .route("/bot/openapi.json", get(bot_openapi))
        .merge(
            Router::new()
                .route("/hooks", post(post_hook))
                .route("/bot/me", get(get_me))
                .route("/bot/updates", get(bot_updates))
                .route("/bot/conversations", get(list_conversations))
                .route("/bot/conversations/{id}", get(get_conversation))
                .route("/bot/conversations/{id}/messages", get(list_messages))
                .route(
                    "/bot/files",
                    post(upload_file).layer(DefaultBodyLimit::max(
                        cfg.upload_max_bytes + UPLOAD_BODY_MARGIN,
                    )),
                )
                .route("/bot/messages/{id}", patch(edit_message))
                .route("/bot/messages/{id}/recall", post(recall_message))
                .route("/bot/messages/{id}/record", get(get_record))
                .route_layer(axum::middleware::from_fn_with_state(st.clone(), bot_auth)),
        )
        .route(
            "/files",
            post(upload_file).layer(DefaultBodyLimit::max(
                cfg.upload_max_bytes + UPLOAD_BODY_MARGIN,
            )),
        )
        .route("/files/{id}", get(download_file))
        // 撤回附件取回：仅上传者本人、Bearer 认证、TTL 内；no-store
        .route("/files/{id}/recalled", get(download_recalled))
        .route("/ws/ticket", get(ws_ticket))
        .route("/ws", get(ws::ws_handler))
        .route("/server-info", get(settings::server_info))
        .route("/client/latest", get(crate::client_update::latest))
        .route(
            "/client/update/{target}/{arch}/{current}",
            get(crate::client_update::tauri_update),
        )
        .route(
            "/client/download/{name}",
            get(crate::client_update::download),
        )
        // ---------- 后台管理（admin only） ----------
        .route("/admin/overview", get(admin_overview))
        .route(
            "/admin/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route(
            "/admin/invites",
            get(settings::list_invites).post(settings::create_invite),
        )
        .route("/admin/invites/{id}", delete(settings::revoke_invite))
        .route("/admin/users", get(admin_users))
        .route("/admin/users/{id}/disable", post(admin_disable))
        .route("/admin/users/{id}/enable", post(admin_enable))
        .route("/admin/users/{id}/role", post(admin_set_role))
        .route("/admin/users/{id}/password", post(admin_reset_password))
        // ---------- V2 P4-1：备份/恢复 ----------
        .route(
            "/admin/backups",
            get(crate::backup::list_backups).post(crate::backup::create_backup),
        )
        .route(
            "/admin/backups/{name}/download-ticket",
            post(crate::backup::download_ticket),
        )
        .route(
            "/admin/backups/{name}/download",
            get(crate::backup::download_backup),
        )
        .route(
            "/admin/backups/{name}",
            delete(crate::backup::delete_backup),
        )
        .route(
            "/admin/restore",
            // 备份包远大于默认 2MB 上限：走路由级放大；内容流式落盘不整包进内存
            post(crate::backup::restore).layer(DefaultBodyLimit::disable()),
        )
    // （维护模式闸在 build_router 里 .layer 到 /v1 子路由上：from_fn_with_state 需要 state）
}

async fn uid_of(headers: &HeaderMap, st: &SharedState) -> AppResult<Uuid> {
    Ok(auth::require_user(st, headers).await?.id)
}

async fn self_public(_st: &SharedState, row: &UserRow) -> AppResult<UserPublic> {
    Ok(UserPublic::from(row))
}

pub(crate) fn notify(st: &SharedState, uids: &[Uuid], what: &'static str) {
    let payload = serde_json::to_string(&WsOut::Invalidate { what }).unwrap();
    st.send_to_users(uids, &payload);
}

fn notify_pins(st: &SharedState, change: &domain::PinChange) {
    if !change.changed {
        return;
    }
    let payload = serde_json::to_string(&WsOut::PinsUpdated {
        conversation_id: change.conversation_id,
    })
    .unwrap();
    st.send_to_users(&change.member_ids, &payload);
}

/// 消息 mutation 的领域拒绝 → HTTP 状态（WS 侧走 PostError::into_ws 错误帧）。
fn post_err(e: domain::PostError) -> AppError {
    match e {
        domain::PostError::Deny { code, message } => match code {
            "forbidden" | "not_friends" => AppError::forbidden(message),
            "not_found" => AppError::not_found(message),
            "conflict" => AppError::conflict(message),
            // 撤回超时仍是 403，但带稳定 code 让客户端区分「无权」与「过期」
            "recall_expired" => {
                AppError::coded(StatusCode::FORBIDDEN, "recall_expired", message)
            }
            // 附件撤回期已过/已清扫/不存在：410 Gone + code
            "attachment_expired" => {
                AppError::coded(StatusCode::GONE, "attachment_expired", message)
            }
            _ => AppError::bad_request(message),
        },
        domain::PostError::Db(e) => e.into(),
    }
}

/// 关系建立后补发 presence：to 立刻看到 from 的真实在线状态
async fn push_presence(st: &SharedState, from: Uuid, to: Uuid) {
    let seen = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT last_seen_at FROM users WHERE id = $1",
    )
    .bind(from)
    .fetch_one(&st.pool)
    .await
    .ok()
    .flatten();
    if let Ok(p) = serde_json::to_string(&WsOut::Presence {
        user_id: from,
        online: st.is_online(from),
        last_seen_at: seen,
    }) {
        st.send_to_user(to, &p);
    }
}
/// 限流用的客户端 IP。TRUST_PROXY 时取 X-Forwarded-For 最右一跳（代理自己追加的那个），
/// 否则就是 TCP 对端——挂在代理后面却不开，所有人会共用代理的 IP 和同一个限流窗口。
fn client_ip(st: &SharedState, headers: &HeaderMap, addr: SocketAddr) -> std::net::IpAddr {
    st.cfg
        .trust_proxy
        .then(|| forwarded_ip(headers))
        .flatten()
        .unwrap_or(addr.ip())
}

/// X-Forwarded-For 最右一跳：前面的都是客户端自己能随便写的
fn forwarded_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    headers
        .get("x-forwarded-for")?
        .to_str()
        .ok()?
        .rsplit(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod forwarded_ip_tests {
    #[test]
    fn takes_the_proxy_appended_hop() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(super::forwarded_ip(&h), None);
        h.insert("x-forwarded-for", "6.6.6.6, 10.0.0.7".parse().unwrap());
        assert_eq!(super::forwarded_ip(&h), Some("10.0.0.7".parse().unwrap()));
        h.insert("x-forwarded-for", "junk".parse().unwrap());
        assert_eq!(super::forwarded_ip(&h), None);
    }
}

/// IP/UID 滑动窗口限流（注册/登录按 IP、改密按 uid）：20 次 / 5 分钟
pub(crate) fn rate_limit(st: &SharedState, key: String) -> AppResult<()> {
    const LIMIT: usize = 20;
    const WINDOW_SECS: u64 = 300;
    // 过期的窗口不再占内存：表大了就整体扫一遍（扫一次很便宜，平时根本不触发）
    if st.rate.len() > 10_000 {
        st.rate.retain(|_, v| {
            v.last()
                .is_some_and(|t| t.elapsed().as_secs() < WINDOW_SECS)
        });
    }
    let mut entry = st.rate.entry(key).or_default();
    entry.retain(|t| t.elapsed().as_secs() < WINDOW_SECS);
    if entry.len() >= LIMIT {
        return Err(AppError::bad_request("too many attempts, try later"));
    }
    entry.push(Instant::now());
    Ok(())
}
