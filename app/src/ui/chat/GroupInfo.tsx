import { useEffect, useState } from "react";
import { convs } from "../../api";
import {
  absorbAvatarUsers,
  applyGroupLeft,
  applyPrefUpdate,
  convAvatarText,
  convTitle,
  isMuted,
  isPinned,
  setState,
  useApp,
} from "../../store";
import type {
  AttachmentView,
  ConversationDetail,
  ConversationView,
  MessageView,
  PrefUpdate,
} from "../../types";
import { leaveGroup, refreshCatalogs } from "../../ws";
import Avatar from "../Avatar";
import { alertDialog, confirmDialog } from "../Modal";
import { BotIcon, FolderSyncIcon, XIcon } from "../icons";
import AnnouncementEditor from "./AnnouncementEditor";
import ConvMedia from "./ConvMedia";

export default function GroupInfo({
  conv,
  onClose,
  onPreview,
}: {
  conv: ConversationView;
  onClose: () => void;
  /** V2 P3-1：文件与图片页签里点缩略图或文件条目 → 外层预览弹窗 */
  onPreview?: (att: AttachmentView, m?: MessageView) => void;
}) {
  const s = useApp();
  const [detail, setDetail] = useState<ConversationDetail | null>(null);
  const [tab, setTab] = useState<"info" | "media">("info");
  const [renaming, setRenaming] = useState(false);
  const [newName, setNewName] = useState("");
  const [adding, setAdding] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [editingAnn, setEditingAnn] = useState(false);
  const [prefBusy, setPrefBusy] = useState(false);

  const isGroup = conv.kind === "group";
  const isOwner = detail?.owner_id === s.me?.id;
  const memberIds = new Set(detail?.members.map((m) => m.user.id));
  const invitable = s.friends.filter((f) => !memberIds.has(f.user.id));
  const title = convTitle(conv, s.me?.id);
  const pinned = isPinned(conv);
  const muted = isMuted(conv);

  const reload = async () => {
    if (!isGroup) return;
    const [d, l] = await Promise.all([convs.detail(conv.id), convs.list()]);
    setDetail(d);
    setState((cur) => ({
      conversations: l,
      avatars: absorbAvatarUsers(cur.avatars, [
        ...d.members.map((m) => m.user),
        ...l.map((c) => c.peer),
      ]),
    }));
  };

  useEffect(() => {
    setEditingAnn(false);
    setDetail(null);
    setTab("info");
    if (isGroup) reload().catch(console.warn);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conv.id, isGroup]);

  const showErr = (e: unknown) =>
    void alertDialog(e instanceof Error ? e.message : "操作失败");
  const act = (p: Promise<unknown>) => p.then(reload).catch(showErr);

  const updatePref = async (body: PrefUpdate) => {
    if (prefBusy) return;
    setPrefBusy(true);
    try {
      const pref = await convs.setPref(conv.id, body);
      setState((cur) => applyPrefUpdate(cur, conv.id, pref));
    } catch (e) {
      showErr(e);
    } finally {
      setPrefBusy(false);
    }
  };

  const leave = async () => {
    if (
      !(await confirmDialog(
        isOwner ? "你是群主，退出后将自动转让；确定退出？" : "退出该群？",
        { danger: true, okText: "退出" },
      ))
    )
      return;
    try {
      if ((await leaveGroup(conv.id)) === "already_left") {
        void alertDialog("你已不在该群");
      }
    } catch (e) {
      showErr(e);
      return;
    }
    onClose();
  };

  const kick = async (userId: string, name: string) => {
    if (
      !(await confirmDialog(`确定将「${name}」移出群聊？`, { danger: true, okText: "移出" }))
    )
      return;
    await act(convs.kick(conv.id, userId));
  };

  const dissolve = async () => {
    if (
      !(await confirmDialog(
        "解散后所有成员将被移出，消息记录保留但不可再发言。确定？",
        { danger: true, okText: "解散" },
      ))
    )
      return;
    try {
      await convs.dissolve(conv.id);
    } catch (e) {
      showErr(e);
      return;
    }
    setState((st) => applyGroupLeft(st, conv.id));
    onClose();
  };

  return (
    <aside className="group-info" data-testid="slide-over">
      <div className="gi-head">
        <h3>
          {isGroup ? "群聊信息" : conv.kind === "self" ? "文件传输助手" : "会话详情"}
        </h3>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭">
          <XIcon size={16} />
        </button>
      </div>

      <div className="gi-tabs" role="tablist">
        <button
          role="tab"
          aria-selected={tab === "info"}
          className={`gs-chip ${tab === "info" ? "on" : ""}`}
          onClick={() => setTab("info")}
        >
          详情与设置
        </button>
        <button
          role="tab"
          aria-selected={tab === "media"}
          className={`gs-chip ${tab === "media" ? "on" : ""}`}
          onClick={() => setTab("media")}
        >
          文件与图片
        </button>
      </div>

      {tab === "media" && <ConvMedia conv={conv} onPreview={onPreview} />}

      {tab === "info" && (
        <>
          <div className="row" style={{ gap: 12, paddingBottom: 10 }}>
            {conv.kind === "dm" && conv.peer && !conv.peer.is_bot ? (
              <Avatar
                userId={conv.peer.id}
                name={conv.peer.display_name}
                avatarVersion={conv.peer.avatar_version}
                hasAvatar={conv.peer.has_avatar}
              />
            ) : conv.kind === "self" ? (
              <div className="avatar self-avatar">
                <FolderSyncIcon size={20} />
              </div>
            ) : conv.kind === "dm" && conv.peer?.is_bot ? (
              <div className="avatar bot-avatar">
                <BotIcon />
              </div>
            ) : (
              <div className="avatar">{convAvatarText(conv)}</div>
            )}
            <div className="grow">
              <div style={{ fontWeight: 600 }}>{title}</div>
              <div className="muted" style={{ fontSize: 12 }}>
                {conv.kind === "self"
                  ? "个人设备间同步文件与便签"
                  : isGroup
                    ? `${conv.member_count} 名成员`
                    : conv.peer?.is_bot
                      ? "机器人会话"
                      : `@${conv.peer?.username ?? ""}`}
              </div>
            </div>
          </div>

          <div className="gi-switch-row">
            <div>
              <div style={{ fontWeight: 500, fontSize: 13 }}>置顶会话</div>
              <div className="muted" style={{ fontSize: 11 }}>
                固定在消息列表顶部
              </div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={pinned}
              disabled={prefBusy}
              className={`toggle-switch${pinned ? " on" : ""}`}
              onClick={() => void updatePref({ pinned: !pinned })}
            />
          </div>

          {conv.kind !== "self" && (
            <div className="gi-switch-row">
              <div>
                <div style={{ fontWeight: 500, fontSize: 13 }}>消息免打扰</div>
                <div className="muted" style={{ fontSize: 11 }}>
                  仅 @我 时弹出提醒
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={muted}
                disabled={prefBusy}
                className={`toggle-switch${muted ? " on" : ""}`}
                onClick={() =>
                  void updatePref({ mute: muted ? { mode: "none" } : { mode: "forever" } })
                }
              />
            </div>
          )}

          {isGroup && detail && (
            <>
              <div className="gi-name" style={{ marginTop: 10 }}>
                {renaming ? (
                  <div className="row">
                    <input value={newName} onChange={(e) => setNewName(e.target.value)} />
                    <button
                      className="primary"
                      onClick={() =>
                        act(convs.rename(conv.id, newName.trim())).then(() =>
                          setRenaming(false),
                        )
                      }
                    >
                      保存
                    </button>
                  </div>
                ) : (
                  <>
                    <b>{detail.name ?? "群聊"}</b>
                    {isOwner && (
                      <button
                        onClick={() => {
                          setNewName(detail.name ?? "");
                          setRenaming(true);
                        }}
                      >
                        改名
                      </button>
                    )}
                  </>
                )}
              </div>

              <div className="gi-ann">
                <h4>群公告</h4>
                {editingAnn && conv.owner_id === s.me?.id ? (
                  <AnnouncementEditor
                    initial={conv.announcement?.text ?? ""}
                    onCancel={() => setEditingAnn(false)}
                    onSave={async (text) => {
                      await convs.setAnnouncement(conv.id, text);
                      await refreshCatalogs();
                      setEditingAnn(false);
                    }}
                  />
                ) : (
                  <>
                    <pre className="ann-text">{conv.announcement?.text || "暂无公告"}</pre>
                    {conv.announcement?.updated_by_name && (
                      <div className="muted">
                        由 {conv.announcement.updated_by_name} 更新于{" "}
                        {conv.announcement.updated_at
                          ? new Date(conv.announcement.updated_at).toLocaleString()
                          : ""}
                      </div>
                    )}
                    {conv.owner_id === s.me?.id && (
                      <button type="button" onClick={() => setEditingAnn(true)}>
                        编辑
                      </button>
                    )}
                  </>
                )}
              </div>

              <div className="gi-members">
                <div className="row between">
                  <h4>成员（{detail.members.length}）</h4>
                  <button onClick={() => setAdding(!adding)}>拉好友</button>
                </div>
                {adding && (
                  <div className="gi-add">
                    {invitable.length === 0 && <div className="muted">没有可邀请的好友</div>}
                    {invitable.map((f) => (
                      <label className="row pick" key={f.user.id}>
                        <input
                          type="checkbox"
                          checked={picked.has(f.user.id)}
                          onChange={() => {
                            const n = new Set(picked);
                            if (n.has(f.user.id)) n.delete(f.user.id);
                            else n.add(f.user.id);
                            setPicked(n);
                          }}
                        />
                        <span>{f.user.display_name}</span>
                      </label>
                    ))}
                    {invitable.length > 0 && (
                      <button
                        className="primary"
                        onClick={() =>
                          act(convs.addMembers(conv.id, [...picked])).then(() => {
                            setAdding(false);
                            setPicked(new Set());
                          })
                        }
                      >
                        邀请（{picked.size}）
                      </button>
                    )}
                  </div>
                )}
                {detail.members.map((m) => (
                  <div className="row" key={m.user.id}>
                    <Avatar
                      userId={m.user.id}
                      name={m.user.display_name}
                      avatarVersion={m.user.avatar_version}
                      hasAvatar={m.user.has_avatar}
                      size="sm"
                    >
                      {s.online[m.user.id] && <i className="online-dot" />}
                    </Avatar>
                    <div className="grow">
                      {m.user.display_name}
                      {m.role === "owner" && <span className="tag">群主</span>}
                      {m.user.is_bot && <span className="tag">机器人</span>}
                    </div>
                    {isOwner && m.user.id !== s.me?.id && (
                      <>
                        <button onClick={() => act(convs.transfer(conv.id, m.user.id))}>
                          转让群主
                        </button>
                        <button
                          className="danger"
                          onClick={() => void kick(m.user.id, m.user.display_name)}
                        >
                          移出
                        </button>
                      </>
                    )}
                  </div>
                ))}
              </div>

              <div className="gi-actions">
                <button className="danger" onClick={leave}>
                  退出群聊
                </button>
                {isOwner && (
                  <button className="danger" onClick={dissolve}>
                    解散群聊
                  </button>
                )}
              </div>
            </>
          )}
        </>
      )}
    </aside>
  );
}
