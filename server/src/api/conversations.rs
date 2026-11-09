use super::*;

#[derive(sqlx::FromRow)]
pub(super) struct MemberRow {
    id: Uuid,
    username: String,
    display_name: String,
    urole: String,
    last_seen_at: Option<DateTime<Utc>>,
    role: String,
    joined_at: DateTime<Utc>,
    avatar_version: i32,
    has_avatar: bool,
    is_bot: bool,
}

// ---------- conversations ----------

#[derive(sqlx::FromRow)]
pub(super) struct ConvRow {
    id: Uuid,
    kind: String,
    name: Option<String>,
    owner_id: Option<Uuid>,
    last_seq: i64,
    created_at: DateTime<Utc>,
    unread_count: i64,
    has_mention: bool,
    member_count: i64,
    peer_id: Option<Uuid>,
    peer_username: Option<String>,
    peer_display: Option<String>,
    peer_role: Option<String>,
    peer_last_seen: Option<DateTime<Utc>>,
    peer_avatar_version: Option<i32>,
    peer_has_avatar: Option<bool>,
    peer_is_bot: Option<bool>,
    m_id: Option<Uuid>,
    m_seq: Option<i64>,
    m_sender_id: Option<Uuid>,
    m_sender_username: Option<String>,
    m_sender_name: Option<String>,
    m_content: Option<String>,
    m_kind: Option<String>,
    m_created_at: Option<DateTime<Utc>>,
    m_att_id: Option<Uuid>,
    m_att_name: Option<String>,
    m_att_mime: Option<String>,
    m_att_size: Option<i64>,
    m_mentions: Option<Vec<Uuid>>,
    m_reply_to_id: Option<Uuid>,
    m_reply_to: Option<sqlx::types::Json<ReplyPreview>>,
    m_edited_at: Option<DateTime<Utc>>,
    m_edit_version: Option<i32>,
    m_deleted_at: Option<DateTime<Utc>>,
    m_reactions: Option<sqlx::types::Json<Vec<domain::ReactionAgg>>>,
    m_poll: Option<sqlx::types::Json<domain::PollAgg>>,
    m_att_width: Option<i32>,
    m_att_height: Option<i32>,
    m_att_has_thumb: Option<bool>,
    m_att_duration_ms: Option<i32>,
    m_sender_avatar_version: Option<i32>,
    m_sender_has_avatar: Option<bool>,
    m_forwarded_from: Option<String>,
    // 撤回（0039）：最后一条消息的撤回者与展示名
    m_recalled_by: Option<Uuid>,
    m_recalled_by_name: Option<String>,
    // 聊天记录（0040）：完整快照仅用于派生 record_preview，不外泄
    m_record: Option<sqlx::types::Json<serde_json::Value>>,
    // 本人偏好（conversation_preferences 左连接，无行则全 NULL/false）
    p_pinned_at: Option<DateTime<Utc>>,
    p_archived_at: Option<DateTime<Utc>>,
    p_muted_until: Option<DateTime<Utc>>,
    p_mute_forever: Option<bool>,
    p_updated_at: Option<DateTime<Utc>>,
    announcement: Option<String>,
    announcement_by_name: Option<String>,
    announcement_at: Option<DateTime<Utc>>,
}

pub(super) async fn list_conversations(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<ConversationView>>> {
    let uid = uid_of(&headers, &st).await?;
    let rows = sqlx::query_as::<_, ConvRow>(
        "SELECT c.id, c.type AS kind, c.name, c.owner_id, c.last_seq, c.created_at,
                (c.last_seq - m.last_read_seq) AS unread_count,
                EXISTS(SELECT 1 FROM message_mentions mm JOIN messages mmsg ON mmsg.id = mm.message_id
                        WHERE mm.user_id = $1 AND mmsg.conversation_id = c.id
                          AND mmsg.seq > m.last_read_seq) AS has_mention,
                (SELECT count(*) FROM memberships mc WHERE mc.conversation_id = c.id) AS member_count,
                p.id AS peer_id, p.username::text AS peer_username, p.display_name AS peer_display,
                p.role AS peer_role, p.last_seen_at AS peer_last_seen,
                p.avatar_version AS peer_avatar_version, p.has_avatar AS peer_has_avatar,
                p.is_bot AS peer_is_bot,
                lm.id AS m_id, lm.seq AS m_seq, lm.sender_id AS m_sender_id,
                lm.sender_username AS m_sender_username, lm.sender_name AS m_sender_name,
                lm.content AS m_content, lm.kind AS m_kind, lm.created_at AS m_created_at,
                lm.att_id AS m_att_id, lm.att_name AS m_att_name, lm.att_mime AS m_att_mime,
                lm.att_size AS m_att_size, lm.mentions AS m_mentions,
                lm.reply_to_id AS m_reply_to_id, lm.reply_to AS m_reply_to,
                lm.edited_at AS m_edited_at, lm.edit_version AS m_edit_version,
                lm.deleted_at AS m_deleted_at, lm.reactions AS m_reactions,
                lm.poll AS m_poll,
                lm.att_width AS m_att_width, lm.att_height AS m_att_height,
                lm.att_has_thumb AS m_att_has_thumb, lm.att_duration_ms AS m_att_duration_ms,
                lm.sender_avatar_version AS m_sender_avatar_version,
                lm.sender_has_avatar AS m_sender_has_avatar, lm.forwarded_from AS m_forwarded_from,
                lm.recalled_by AS m_recalled_by, lm.recalled_by_name AS m_recalled_by_name,
                lm.record AS m_record,
                cp.pinned_at AS p_pinned_at, cp.archived_at AS p_archived_at,
                cp.muted_until AS p_muted_until, cp.mute_forever AS p_mute_forever,
                cp.updated_at AS p_updated_at,
                c.announcement, au.display_name AS announcement_by_name, c.announcement_at
           FROM memberships m
           JOIN conversations c ON c.id = m.conversation_id
           LEFT JOIN users au ON au.id = c.announcement_by
           LEFT JOIN conversation_preferences cp
                  ON cp.conversation_id = c.id AND cp.user_id = m.user_id
           LEFT JOIN LATERAL (
               SELECT u.id, u.username, u.display_name, u.role,
                      u.avatar_version, u.avatar_path IS NOT NULL AS has_avatar,
                      u.bot_owner_id IS NOT NULL AS is_bot,
                      -- last_seen_at 属于 presence：删好友后保留的只读 DM 不得继续暴露
                      CASE WHEN EXISTS(SELECT 1 FROM friendships f
                                        WHERE (f.user_a = $1 AND f.user_b = u.id)
                                           OR (f.user_b = $1 AND f.user_a = u.id))
                            OR EXISTS(SELECT 1 FROM memberships mx1
                                       JOIN memberships mx2
                                         ON mx1.conversation_id = mx2.conversation_id
                                       JOIN conversations cx
                                         ON cx.id = mx1.conversation_id AND cx.type = 'group'
                                       WHERE mx1.user_id = $1 AND mx2.user_id = u.id)
                           THEN u.last_seen_at END AS last_seen_at
                 FROM memberships mm JOIN users u ON u.id = mm.user_id
                WHERE mm.conversation_id = c.id AND mm.user_id <> $1
                LIMIT 1
           ) p ON c.type = 'dm'
           LEFT JOIN LATERAL (
               SELECT id, seq, sender_id, sender_username, sender_name, content, kind, created_at,
                      att_id, att_name, att_mime, att_size, mentions,
                      reply_to_id, reply_to, edited_at, edit_version, deleted_at, reactions, poll,
                      att_width, att_height, att_has_thumb, att_duration_ms,
                      sender_avatar_version, sender_has_avatar, forwarded_from,
                      recalled_by, recalled_by_name, record
                 FROM message_views
                WHERE conversation_id = c.id
                ORDER BY seq DESC
                LIMIT 1
           ) lm ON true
          WHERE m.user_id = $1
          -- 置顶块在前按 pinned_at DESC，块内与未置顶均按原活动时间 DESC、id 兜底
          ORDER BY (cp.pinned_at IS NOT NULL) DESC, cp.pinned_at DESC,
                   COALESCE(lm.created_at, c.created_at) DESC, c.id",
    )
    .bind(uid)
    .fetch_all(&st.pool)
    .await?;

    let out = rows
        .into_iter()
        .map(|r| {
            let peer = r.peer_id.map(|id| UserPublic {
                id,
                username: r.peer_username.unwrap_or_default(),
                display_name: r.peer_display.unwrap_or_default(),
                role: r.peer_role.unwrap_or_else(|| "user".into()),
                last_seen_at: r.peer_last_seen,
                avatar_version: r.peer_avatar_version.unwrap_or(0),
                has_avatar: r.peer_has_avatar.unwrap_or(false),
                is_bot: r.peer_is_bot.unwrap_or(false),
            });
            let last_message = r.m_id.map(|id| MessageView {
                id,
                conversation_id: r.id,
                seq: r.m_seq.unwrap_or(0),
                sender_id: r.m_sender_id.unwrap_or_default(),
                sender_username: r.m_sender_username.unwrap_or_default(),
                sender_name: r.m_sender_name.unwrap_or_default(),
                content: r.m_content.unwrap_or_default(),
                kind: r.m_kind.unwrap_or_else(|| "text".into()),
                attachment: r.m_att_id.map(|att_id| AttachmentView {
                    id: att_id,
                    file_name: r.m_att_name.clone().unwrap_or_default(),
                    mime: r.m_att_mime.clone().unwrap_or_default(),
                    size: r.m_att_size.unwrap_or_default(),
                    width: r.m_att_width,
                    height: r.m_att_height,
                    has_thumb: r.m_att_has_thumb.unwrap_or(false),
                    duration_ms: r.m_att_duration_ms,
                }),
                mentions: r.m_mentions.clone().unwrap_or_default(),
                created_at: r.m_created_at.unwrap_or_else(Utc::now),
                reply_to_id: r.m_reply_to_id,
                reply_to_preview: r.m_reply_to.clone().map(|j| j.0),
                edited_at: r.m_edited_at,
                edit_version: r.m_edit_version.unwrap_or(0),
                deleted_at: r.m_deleted_at,
                reactions: r
                    .m_reactions
                    .as_ref()
                    .map(|j| {
                        j.0.iter()
                            .map(|a| ReactionView {
                                emoji: a.emoji.clone(),
                                count: a.users.len() as i64,
                                reacted_by_me: a.users.contains(&uid),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                poll: r.m_poll.map(|j| j.0.into_view(uid)),
                record_preview: r
                    .m_record
                    .and_then(|j| domain::record_preview(&j.0)),
                sender_avatar_version: r.m_sender_avatar_version.unwrap_or(0),
                sender_has_avatar: r.m_sender_has_avatar.unwrap_or(false),
                forwarded_from: r.m_forwarded_from.clone(),
                recalled: r.m_deleted_at.is_some(),
                recalled_by: r.m_recalled_by,
                recalled_by_name: r.m_recalled_by_name.clone(),
                // 会话列表预览永不携带撤回附件 id（广播同理不泄露）
                recalled_attachment_id: None,
            });
            ConversationView {
                id: r.id,
                kind: r.kind,
                name: r.name,
                owner_id: r.owner_id,
                peer,
                member_count: r.member_count,
                last_seq: r.last_seq,
                unread_count: r.unread_count,
                has_mention: r.has_mention,
                last_message,
                preference: ConversationPref {
                    pinned_at: r.p_pinned_at,
                    archived_at: r.p_archived_at,
                    muted_until: r.p_muted_until,
                    mute_forever: r.p_mute_forever.unwrap_or(false),
                    updated_at: r.p_updated_at,
                },
                announcement: r.announcement.map(|text| AnnouncementView {
                    text,
                    updated_by_name: r.announcement_by_name.clone(),
                    updated_at: r.announcement_at,
                }),
                created_at: r.created_at,
            }
        })
        .collect();
    Ok(Json(out))
}

// ---------- 会话偏好（仅本人可见、同步本人全部设备） ----------

/// 更新本人偏好：缺省字段不改；mute 形状/时间在领域层校验；解散/非成员 403。
/// client_op_id 仅作契约参数（状态覆写天然幂等，无需持久化 op_id）。
/// 成功后给本人全部连接发 preference_updated（含发起连接，幂等收敛）。
pub(super) async fn put_preferences(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
    Query(_q): Query<ClientOpQuery>,
    Json(req): Json<PrefUpdateReq>,
) -> AppResult<Json<ConversationPref>> {
    let uid = uid_of(&headers, &st).await?;
    let pref = domain::set_preferences(&st.pool, conv, uid, &req)
        .await
        .map_err(post_err)?;
    let frame = serde_json::to_string(&WsOut::PreferenceUpdated {
        conversation_id: conv,
        preference: pref.clone(),
    })
    .unwrap();
    st.send_to_user(uid, &frame);
    Ok(Json(pref))
}

pub(super) async fn create_group(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<GroupCreateReq>,
) -> AppResult<Json<ConversationDetail>> {
    let uid = uid_of(&headers, &st).await?;
    let ids: Vec<Uuid> = {
        let mut v: Vec<Uuid> = req.member_ids.into_iter().filter(|i| *i != uid).collect();
        v.sort();
        v.dedup();
        v
    };
    if ids.is_empty() {
        return Err(AppError::bad_request("invite at least one friend"));
    }
    domain::ensure_friends(&st.pool, uid, &ids).await?;

    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("群聊")
        .to_string();
    if name.len() > 100 {
        return Err(AppError::bad_request("name too long"));
    }

    let mut tx = st.pool.begin().await?;
    let conv_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO conversations (type, name, owner_id) VALUES ('group', $1, $2) RETURNING id",
    )
    .bind(&name)
    .bind(uid)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO memberships (conversation_id, user_id, role)
         VALUES ($1, $2, 'owner')",
    )
    .bind(conv_id)
    .bind(uid)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO memberships (conversation_id, user_id, role)
         SELECT $1, unnest($2::uuid[]), 'member'",
    )
    .bind(conv_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    // UI3：建群系统消息（同事务；不回填历史）
    let actor_name = domain::display_name(&mut *tx, uid).await?;
    let text = format!("{actor_name} 创建了群聊");
    let content = domain::system_content(
        domain::CODE_CREATED,
        &text,
        domain::params_actor_group(&actor_name, &name),
    );
    let sys_msg = domain::insert_system_in_tx(&mut tx, conv_id, uid, &content).await?;
    let all = domain::member_ids(&mut *tx, conv_id).await?;
    tx.commit().await?;

    ws::broadcast_posted(
        &st,
        conv_id,
        &domain::Posted {
            message: sys_msg,
            inserted: true,
            member_ids: all.clone(),
            unarchived: Vec::new(),
        },
    )
    .await;
    notify(&st, &all, "conversations");
    conversation_detail(&st, conv_id).await.map(Json)
}

pub(super) async fn require_member(
    st: &SharedState,
    conv: Uuid,
    uid: Uuid,
) -> AppResult<domain::MemberInfo> {
    domain::membership(&st.pool, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))
}

pub(super) async fn conversation_detail(
    st: &SharedState,
    conv: Uuid,
) -> AppResult<ConversationDetail> {
    let (kind, name, owner_id, last_seq, created_at) =
        sqlx::query_as::<_, (String, Option<String>, Option<Uuid>, i64, DateTime<Utc>)>(
            "SELECT type, name, owner_id, last_seq, created_at FROM conversations WHERE id = $1",
        )
        .bind(conv)
        .fetch_optional(&st.pool)
        .await?
        .ok_or_else(|| AppError::not_found("conversation not found"))?;

    let members = sqlx::query_as::<_, MemberRow>(
        "SELECT u.id, u.username::text AS username, u.display_name, u.role AS urole, u.last_seen_at,
                m.role, m.joined_at, u.avatar_version, u.avatar_path IS NOT NULL AS has_avatar,
                u.bot_owner_id IS NOT NULL AS is_bot
           FROM memberships m JOIN users u ON u.id = m.user_id
          WHERE m.conversation_id = $1 ORDER BY m.joined_at",
    )
    .bind(conv)
    .fetch_all(&st.pool)
    .await?;

    Ok(ConversationDetail {
        id: conv,
        kind,
        name,
        owner_id,
        last_seq,
        created_at,
        members: members
            .into_iter()
            .map(|r| MemberView {
                user: UserPublic {
                    id: r.id,
                    username: r.username,
                    display_name: r.display_name,
                    role: r.urole,
                    last_seen_at: r.last_seen_at,
                    avatar_version: r.avatar_version,
                    has_avatar: r.has_avatar,
                    is_bot: r.is_bot,
                },
                role: r.role,
                joined_at: r.joined_at,
            })
            .collect(),
    })
}

pub(super) async fn get_conversation(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
) -> AppResult<Json<ConversationDetail>> {
    let uid = uid_of(&headers, &st).await?;
    require_member(&st, conv, uid).await?;
    conversation_detail(&st, conv).await.map(Json)
}

pub(super) async fn add_members(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
    Json(req): Json<AddMembersReq>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let ids: Vec<Uuid> = {
        let mut v: Vec<Uuid> = req.user_ids.into_iter().filter(|i| *i != uid).collect();
        v.sort();
        v.dedup();
        v
    };
    if ids.is_empty() {
        return Err(AppError::bad_request("no users to add"));
    }
    domain::ensure_friends(&st.pool, uid, &ids).await?;

    // 与解散/退群/踢人共用会话行锁：锁内重检类型、解散标记与我的成员身份，
    // 防止「邀请」恰好落在解散与成员清空之间把人插回已解散会话。
    let mut tx = st.pool.begin().await?;
    let Some((kind, _, dissolved)) = domain::lock_conversation(&mut tx, conv).await? else {
        return Err(AppError::forbidden("not a member of this conversation"));
    };
    if kind != "group" {
        return Err(AppError::bad_request(
            "only group conversations accept members",
        ));
    }
    if dissolved {
        return Err(AppError::bad_request("group already dissolved"));
    }
    domain::membership(&mut *tx, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))?;
    let before = domain::member_ids(&mut *tx, conv).await?;
    // delivered_seq 从入群这一刻算：bot 只拉得到入群之后 @ 它的消息（行锁内读 last_seq）
    sqlx::query(
        "INSERT INTO memberships (conversation_id, user_id, role, delivered_seq)
         SELECT $1, unnest($2::uuid[]), 'member', (SELECT last_seq FROM conversations WHERE id = $1)
             ON CONFLICT DO NOTHING",
    )
    .bind(conv)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    let all = domain::member_ids(&mut *tx, conv).await?;
    let newly: Vec<Uuid> = all.iter().copied().filter(|id| !before.contains(id)).collect();
    let mut sys_posted = None;
    if !newly.is_empty() {
        let invitee_names = domain::display_names(&mut *tx, &newly).await?;
        let actor_name = domain::display_name(&mut *tx, uid).await?;
        let joined = invitee_names.join("、");
        let text = format!("{actor_name} 邀请 {joined} 加入了群聊");
        let content = domain::system_content(
            domain::CODE_INVITED,
            &text,
            domain::params_invite(&actor_name, &invitee_names),
        );
        let msg = domain::insert_system_in_tx(&mut tx, conv, uid, &content).await?;
        sys_posted = Some(domain::Posted {
            message: msg,
            inserted: true,
            member_ids: all.clone(),
            unarchived: Vec::new(),
        });
    }
    tx.commit().await?;

    if let Some(posted) = sys_posted {
        ws::broadcast_posted(&st, conv, &posted).await;
    }
    notify(&st, &all, "conversations");
    // 新成员与既有成员互推在线状态
    for nid in &ids {
        for m in &all {
            if m != nid {
                push_presence(&st, *nid, *m).await;
                push_presence(&st, *m, *nid).await;
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn leave_conversation(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;

    // 全程在会话行锁内做检查与改写：转让/退群/踢人/解散互斥，杜绝零群主。
    let mut tx = st.pool.begin().await?;
    let Some((kind, _, _)) = domain::lock_conversation(&mut tx, conv).await? else {
        return Err(AppError::forbidden("not a member of this conversation"));
    };
    if kind != "group" {
        return Err(AppError::bad_request("cannot leave a direct conversation"));
    }
    let me = domain::membership(&mut *tx, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))?;
    // bot 不接任群主、也不撑着群：只剩 bot 时群随最后一个人解散
    let others = sqlx::query_scalar::<_, Uuid>(
        "SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.conversation_id = $1 AND m.user_id <> $2 AND u.bot_owner_id IS NULL
         ORDER BY m.joined_at",
    )
    .bind(conv)
    .bind(uid)
    .fetch_all(&mut *tx)
    .await?;

    // 最后一人退出会删会话：无系统消息可挂。否则先写 group_left，再摘成员。
    let mut sys_posted = None;
    let deleting = me.role == "owner" && others.is_empty();
    if !deleting {
        let actor_name = domain::display_name(&mut *tx, uid).await?;
        let text = format!("{actor_name} 退出了群聊");
        let content = domain::system_content(
            domain::CODE_LEFT,
            &text,
            domain::params_actor(&actor_name),
        );
        let msg = domain::insert_system_in_tx(&mut tx, conv, uid, &content).await?;
        // 广播给剩余成员（不含即将离开的自己）
        sys_posted = Some(domain::Posted {
            message: msg,
            inserted: true,
            member_ids: others.clone(),
            unarchived: Vec::new(),
        });
    }

    if me.role == "owner" {
        if others.is_empty() {
            sqlx::query("DELETE FROM conversations WHERE id = $1")
                .bind(conv)
                .execute(&mut *tx)
                .await?;
        } else {
            // 转让给最早加入的成员；锁内执行，rows_affected 异常即整体回滚
            let new_owner = sqlx::query_scalar::<_, Uuid>(
                "UPDATE memberships SET role = 'owner'
                 WHERE conversation_id = $1 AND user_id = $2
                 RETURNING user_id",
            )
            .bind(conv)
            .bind(others[0])
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::internal("ownership transfer failed"))?;
            sqlx::query("UPDATE conversations SET owner_id = $2 WHERE id = $1")
                .bind(conv)
                .bind(new_owner)
                .execute(&mut *tx)
                .await?;
        }
    }
    if me.role == "owner" && others.is_empty() {
        // 最后一人退出：会话已连同成员一起删除
    } else {
        let res =
            sqlx::query("DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2")
                .bind(conv)
                .bind(uid)
                .execute(&mut *tx)
                .await?;
        if res.rows_affected() != 1 {
            return Err(AppError::internal("leave failed"));
        }
    }
    tx.commit().await?;

    if let Some(posted) = sys_posted {
        ws::broadcast_posted(&st, conv, &posted).await;
    }
    notify(&st, &others, "conversations");
    notify(&st, &[uid], "conversations");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn kick_member(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path((conv, target)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    if uid == target {
        return Err(AppError::bad_request("use leave endpoint for yourself"));
    }
    // 锁内重检：我仍是群主、目标仍是非群主成员，才能删
    let mut tx = st.pool.begin().await?;
    let Some((kind, _, _)) = domain::lock_conversation(&mut tx, conv).await? else {
        return Err(AppError::forbidden("not a member of this conversation"));
    };
    // self 会话单人不可变：成员操作与 dm 一样拒绝，但统一 400（群主身份本是唯一成员，
    // 不做类型守卫会落到 member not found 的 404）
    if kind == "self" {
        return Err(AppError::bad_request(
            "this conversation has no members to manage",
        ));
    }
    let me = domain::membership(&mut *tx, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))?;
    if me.role != "owner" {
        return Err(AppError::forbidden("only the owner can remove members"));
    }
    let res = sqlx::query(
        "DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2 AND role <> 'owner'",
    )
    .bind(conv)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::not_found("member not found"));
    }
    let rest = domain::member_ids(&mut *tx, conv).await?;
    tx.commit().await?;

    notify(&st, &rest, "conversations");
    notify(&st, &[target], "conversations");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn rename_conversation(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
    Json(req): Json<RenameReq>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let me = require_member(&st, conv, uid).await?;
    if me.role != "owner" {
        return Err(AppError::forbidden("only the owner can rename"));
    }
    // self 会话 name 恒 NULL（客户端按 kind 显示标题），改名无意义 → 400
    if matches!(
        domain::conv_kind(&st.pool, conv).await?.as_deref(),
        Some("self")
    ) {
        return Err(AppError::bad_request("this conversation cannot be renamed"));
    }
    let name = req.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request("name must be 1-100 chars"));
    }
    sqlx::query("UPDATE conversations SET name = $2 WHERE id = $1")
        .bind(conv)
        .bind(name)
        .execute(&st.pool)
        .await?;
    let actor_name = domain::display_name(&st.pool, uid).await?;
    let text = format!("{actor_name} 将群名改为「{name}」");
    let content = domain::system_content(
        domain::CODE_RENAMED,
        &text,
        domain::params_actor_group(&actor_name, name),
    );
    if let Ok(posted) = domain::post_system(&st.pool, conv, uid, &content).await {
        ws::broadcast_posted(&st, conv, &posted).await;
    }
    let members = domain::member_ids(&st.pool, conv).await?;
    notify(&st, &members, "conversations");
    Ok(StatusCode::NO_CONTENT)
}

/// 群公告：仅群主可改。空字符串 / null 清空三列。变更后 invalidate conversations。
pub(super) async fn put_announcement(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
    Json(req): Json<AnnouncementReq>,
) -> AppResult<Json<Value>> {
    let uid = uid_of(&headers, &st).await?;
    let me = require_member(&st, conv, uid).await?;
    let kind = domain::conv_kind(&st.pool, conv).await?.unwrap_or_default();
    if kind != "group" {
        return Err(AppError::bad_request("只有群聊可以设置公告"));
    }
    if me.role != "owner" {
        return Err(AppError::forbidden(
            "only the owner can set the announcement",
        ));
    }
    let text = req
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(t) = &text {
        if t.chars().count() > 2000 {
            return Err(AppError::bad_request("announcement must be 1-2000 chars"));
        }
    }
    sqlx::query(
        "UPDATE conversations
            SET announcement = $2, announcement_by = $3, announcement_at = CASE WHEN $2 IS NULL THEN NULL ELSE now() END
          WHERE id = $1",
    )
    .bind(conv)
    .bind(&text)
    .bind(text.as_ref().map(|_| uid))
    .execute(&st.pool)
    .await?;
    // 发布/更新公告写系统条；清空不写
    if text.is_some() {
        let actor_name = domain::display_name(&st.pool, uid).await?;
        let bar = format!("{actor_name} 更新了群公告");
        let content = domain::system_content(
            domain::CODE_ANNOUNCEMENT,
            &bar,
            domain::params_actor(&actor_name),
        );
        if let Ok(posted) = domain::post_system(&st.pool, conv, uid, &content).await {
            ws::broadcast_posted(&st, conv, &posted).await;
        }
    }
    let members = domain::member_ids(&st.pool, conv).await?;
    notify(&st, &members, "conversations");
    let view = announcement_of(&st, conv).await?;
    Ok(Json(match view {
        Some(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        None => json!({"text": null, "updated_by_name": null, "updated_at": null}),
    }))
}

pub(super) async fn announcement_of(
    st: &SharedState,
    conv: Uuid,
) -> AppResult<Option<AnnouncementView>> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<DateTime<Utc>>)>(
        "SELECT c.announcement, u.display_name, c.announcement_at
           FROM conversations c
           LEFT JOIN users u ON u.id = c.announcement_by
          WHERE c.id = $1",
    )
    .bind(conv)
    .fetch_optional(&st.pool)
    .await?;
    Ok(row.and_then(|(text, name, at)| {
        text.map(|text| AnnouncementView {
            text,
            updated_by_name: name,
            updated_at: at,
        })
    }))
}

#[derive(Serialize)]
pub(super) struct PinEntry {
    message: MessageView,
    pinned_by_name: Option<String>,
    pinned_at: DateTime<Utc>,
}

/// 置顶列表：只返回未删除消息，按 pinned_at 倒序。非成员 403。
pub(super) async fn list_pins(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
) -> AppResult<Json<Vec<PinEntry>>> {
    let uid = uid_of(&headers, &st).await?;
    require_member(&st, conv, uid).await?;
    let rows = sqlx::query_as::<_, (Uuid, Option<String>, DateTime<Utc>)>(
        "SELECT p.message_id, u.display_name, p.pinned_at
           FROM conversation_pins p
           JOIN messages m ON m.id = p.message_id AND m.deleted_at IS NULL
           LEFT JOIN users u ON u.id = p.pinned_by
          WHERE p.conversation_id = $1
          ORDER BY p.pinned_at DESC, p.message_id",
    )
    .bind(conv)
    .fetch_all(&st.pool)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let views = domain::pinned_message_views(&st.pool, conv, uid, &ids).await?;
    let mut by_id: std::collections::HashMap<Uuid, MessageView> =
        views.into_iter().map(|m| (m.id, m)).collect();
    let out = rows
        .into_iter()
        .filter_map(|(id, name, pinned_at)| {
            by_id.remove(&id).map(|message| PinEntry {
                message,
                pinned_by_name: name,
                pinned_at,
            })
        })
        .collect();
    Ok(Json(out))
}

pub(super) async fn pin_message(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path((conv, message_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let change = domain::pin_message(&st.pool, conv, message_id, uid)
        .await
        .map_err(post_err)?;
    notify_pins(&st, &change);
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn unpin_message(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path((conv, message_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    let change = domain::unpin_message(&st.pool, conv, message_id, uid)
        .await
        .map_err(post_err)?;
    notify_pins(&st, &change);
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn dissolve_conversation(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    // 与拉人/退群/踢人共用会话行锁，锁内重检群主身份与解散标记
    let mut tx = st.pool.begin().await?;
    let Some((kind, _, _)) = domain::lock_conversation(&mut tx, conv).await? else {
        return Err(AppError::forbidden("not a member of this conversation"));
    };
    // self 会话不可解散：唯一成员即 owner，不做类型守卫会把会话连同成员一起删掉
    if kind == "self" {
        return Err(AppError::bad_request(
            "this conversation cannot be dissolved",
        ));
    }
    let me = domain::membership(&mut *tx, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))?;
    if me.role != "owner" {
        return Err(AppError::forbidden("only the owner can dissolve"));
    }
    // 可测解散路径：先插 group_dissolved（仍有成员、未标解散），再 dissolved_at + 清空成员；
    // 提交后按捕获的 member_ids 广播系统条，再 invalidate conversations 清列表。
    let members =
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM memberships WHERE conversation_id = $1")
            .bind(conv)
            .fetch_all(&mut *tx)
            .await?;
    let actor_name = domain::display_name(&mut *tx, uid).await?;
    let text = format!("{actor_name} 解散了群聊");
    let content = domain::system_content(
        domain::CODE_DISSOLVED,
        &text,
        domain::params_actor(&actor_name),
    );
    let sys_msg = domain::insert_system_in_tx(&mut tx, conv, uid, &content).await?;
    let res = sqlx::query(
        "UPDATE conversations SET dissolved_at = now() WHERE id = $1 AND dissolved_at IS NULL",
    )
    .bind(conv)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::conflict("group already dissolved"));
    }
    sqlx::query("DELETE FROM memberships WHERE conversation_id = $1")
        .bind(conv)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    ws::broadcast_posted(
        &st,
        conv,
        &domain::Posted {
            message: sys_msg,
            inserted: true,
            member_ids: members.clone(),
            unarchived: Vec::new(),
        },
    )
    .await;
    notify(&st, &members, "conversations");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct TransferReq {
    user_id: Uuid,
}

/// 群主转让：仅群主；目标必须是普通成员。全程持会话行锁，两步角色变更必须各自命中。
pub(super) async fn transfer_ownership(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Path(conv): Path<Uuid>,
    Json(req): Json<TransferReq>,
) -> AppResult<StatusCode> {
    let uid = uid_of(&headers, &st).await?;
    if uid == req.user_id {
        return Err(AppError::bad_request("already the owner"));
    }
    let mut tx = st.pool.begin().await?;
    let Some((kind, _, _)) = domain::lock_conversation(&mut tx, conv).await? else {
        return Err(AppError::forbidden("not a member of this conversation"));
    };
    // self 会话归属本人不可转让（唯一成员即 owner）
    if kind == "self" {
        return Err(AppError::bad_request(
            "this conversation cannot be transferred",
        ));
    }
    let me = domain::membership(&mut *tx, conv, uid)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this conversation"))?;
    if me.role != "owner" {
        return Err(AppError::forbidden("only the owner can transfer ownership"));
    }
    let target = domain::membership(&mut *tx, conv, req.user_id)
        .await?
        .ok_or_else(|| AppError::not_found("member not found"))?;
    if target.role == "owner" {
        return Err(AppError::bad_request("target is already the owner"));
    }
    let target_is_bot =
        sqlx::query_scalar::<_, bool>("SELECT bot_owner_id IS NOT NULL FROM users WHERE id = $1")
            .bind(req.user_id)
            .fetch_one(&mut *tx)
            .await?;
    if target_is_bot {
        return Err(AppError::bad_request("cannot transfer ownership to a bot"));
    }

    let res = sqlx::query(
        "UPDATE memberships SET role = 'member'
         WHERE conversation_id = $1 AND user_id = $2 AND role = 'owner'",
    )
    .bind(conv)
    .bind(uid)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() != 1 {
        return Err(AppError::internal("ownership transfer failed"));
    }
    let res = sqlx::query(
        "UPDATE memberships SET role = 'owner'
         WHERE conversation_id = $1 AND user_id = $2 AND role = 'member'",
    )
    .bind(conv)
    .bind(req.user_id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() != 1 {
        return Err(AppError::internal("ownership transfer failed"));
    }
    sqlx::query("UPDATE conversations SET owner_id = $2 WHERE id = $1")
        .bind(conv)
        .bind(req.user_id)
        .execute(&mut *tx)
        .await?;
    let members = domain::member_ids(&mut *tx, conv).await?;
    tx.commit().await?;

    notify(&st, &members, "conversations");
    Ok(StatusCode::NO_CONTENT)
}
