import { useEffect, useMemo, useState } from "react";
import { convs } from "../../api";
import {
  applyPrefUpdate,
  convAvatarText,
  convTitle,
  isMuted,
  isPinned,
  setState,
  sortConversations,
  useApp,
} from "../../store";
import type { ConversationView, PrefUpdate } from "../../types";
import { openConversation } from "../../ws";
import Avatar from "../Avatar";
import EmptyState from "../EmptyState";
import Skeleton from "../Skeleton";
import { BotIcon, FolderSyncIcon, PlusIcon, SearchIcon } from "../icons";
import CtxMenu from "./CtxMenu";
import { convPreview } from "./convPreview";
import { fmtTime } from "./fmtTime";
import NewGroupModal from "./NewGroupModal";
import SearchPanel from "./SearchPanel";

/** 静音状态的悬浮说明 */
function muteLabel(c: ConversationView): string {
  const p = c.preference;
  if (!p) return "已静音";
  if (p.mute_forever) return "已永久静音";
  if (p.muted_until && Date.parse(p.muted_until) > Date.now())
    return `静音至 ${new Date(p.muted_until).toLocaleString()}`;
  return "已静音";
}

export default function ConvList() {
  const s = useApp();
  const [q, setQ] = useState("");
  const [showNewGroup, setShowNewGroup] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; c: ConversationView } | null>(null);
  const [menuBusy, setMenuBusy] = useState(false);
  const [menuErr, setMenuErr] = useState<string | null>(null);

  const chats = useMemo(() => sortConversations(s.conversations), [s.conversations]);

  const list = chats.filter((c) => {
    const title = convTitle(c, s.me?.id);
    return !q.trim() || title.toLowerCase().includes(q.trim().toLowerCase());
  });

  const openMenu = (e: React.MouseEvent, c: ConversationView) => {
    e.preventDefault();
    e.stopPropagation();
    setMenuErr(null);
    setMenu({ x: e.clientX, y: e.clientY, c });
  };

  const submitPref = async (id: string, body: PrefUpdate) => {
    if (menuBusy) return;
    setMenuBusy(true);
    setMenuErr(null);
    try {
      const pref = await convs.setPref(id, body);
      setState((cur) => applyPrefUpdate(cur, id, pref));
      setMenu(null);
    } catch (e) {
      setMenuErr(e instanceof Error ? e.message : "设置失败");
    } finally {
      setMenuBusy(false);
    }
  };

  useEffect(() => {
    if (!menu) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu]);

  const muteOneHour = () => new Date(Date.now() + 3600_000).toISOString();

  const muteToday = () => {
    const end = new Date();
    end.setHours(24, 0, 0, 0);
    return end.toISOString();
  };

  return (
    <aside className="conv-list">
      <div className="conv-header-wrap">
        <div className="conv-head-top">
          <div className="conv-head-title-row">
            <h2 className="conv-head-title">消息会话</h2>
            <span className={`status-pill${s.wsReady ? " online" : ""}`}>
              <i className={`dot${s.wsReady ? " on" : ""}`} />
              <span>{s.wsReady ? "在线" : "重连中"}</span>
            </span>
          </div>
          <button
            type="button"
            className="conv-new-btn"
            title="发起群聊"
            onClick={() => setShowNewGroup(true)}
          >
            <PlusIcon size={14} />
            <span>发起群聊</span>
          </button>
        </div>

        <div className="conv-search-bar">
          <span className="conv-search-icon">
            <SearchIcon size={14} />
          </span>
          <input
            className="search"
            placeholder="搜索会话或聊天记录"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
        </div>
      </div>

      {!s.wsReady && <div className="offline-banner">离线，正在重连…</div>}

      {q.trim() ? (
        <SearchPanel q={q} />
      ) : (
        <div className="conv-scroll">
          {list.length === 0 && !s.wsReady && <Skeleton rows={5} />}
          {list.length === 0 && s.wsReady && (
            <EmptyState
              title="还没有会话"
              detail="去「联系人」加好友或点击右上角发起群聊"
            />
          )}
          {list.map((c) => {
            const title = convTitle(c, s.me?.id);
            const peerOnline = c.kind === "dm" && c.peer && s.online[c.peer.id];
            const typing = s.typing[c.id];
            const typingFresh = typing && Date.now() - typing.ts < 5000;
            const muted = isMuted(c);
            const pv = convPreview(c, {
              draft: s.drafts[c.id],
              active: s.activeConv === c.id,
              typing: !!typingFresh,
              meId: s.me?.id,
            });
            return (
              <div
                key={c.id}
                className={`conv-item${s.activeConv === c.id ? " on" : ""}`}
                onClick={() => openConversation(c.id)}
                onContextMenu={(e) => openMenu(e, c)}
              >
                {c.kind === "dm" && c.peer && !c.peer.is_bot ? (
                  <Avatar
                    userId={c.peer.id}
                    name={c.peer.display_name}
                    avatarVersion={c.peer.avatar_version}
                    hasAvatar={c.peer.has_avatar}
                  >
                    {peerOnline && <i className="online-dot" />}
                  </Avatar>
                ) : c.kind === "self" ? (
                  <div className="avatar self-avatar">
                    <FolderSyncIcon size={20} />
                  </div>
                ) : c.kind === "dm" && c.peer?.is_bot ? (
                  <div className="avatar bot-avatar">
                    <BotIcon />
                  </div>
                ) : (
                  <div className="avatar">{convAvatarText(c)}</div>
                )}

                <div className="conv-mid">
                  <div className="conv-title-row">
                    {isPinned(c) && (
                      <span className="conv-flag" title="已置顶">
                        📌
                      </span>
                    )}
                    <span className="conv-title">{title}</span>
                    {muted && (
                      <span className="conv-flag" title={muteLabel(c)}>
                        🔕
                      </span>
                    )}
                  </div>
                  <div className="conv-sub">
                    {c.has_mention && c.unread_count > 0 && (
                      <span className="mention-tag">[有人@我]</span>
                    )}
                    {pv.kind === "draft" && <span className="draft-tag">[草稿]</span>}
                    {pv.text}
                  </div>
                </div>

                <div className="conv-right">
                  <div className="conv-time">
                    {fmtTime(c.last_message?.created_at ?? c.created_at)}
                  </div>
                  {c.unread_count > 0 && (
                    <div className={`unread ${muted ? "muted" : ""}`}>
                      {c.unread_count > 99 ? "99+" : c.unread_count}
                    </div>
                  )}
                </div>

                <button
                  type="button"
                  className="icon-btn conv-menu-btn"
                  title="会话操作"
                  aria-label="会话操作"
                  onClick={(e) => {
                    e.stopPropagation();
                    const r = e.currentTarget.getBoundingClientRect();
                    setMenuErr(null);
                    setMenu({ x: r.right, y: r.bottom, c });
                  }}
                >
                  ⋯
                </button>
              </div>
            );
          })}
        </div>
      )}

      {showNewGroup && <NewGroupModal onClose={() => setShowNewGroup(false)} />}

      {menu && (
        <CtxMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
          <button
            type="button"
            className="ctx-item"
            disabled={menuBusy}
            onClick={() => void submitPref(menu.c.id, { pinned: !isPinned(menu.c) })}
          >
            {isPinned(menu.c) ? "取消置顶" : "置顶会话"}
          </button>
          {menu.c.kind !== "self" &&
            (isMuted(menu.c) ? (
              <button
                type="button"
                className="ctx-item"
                disabled={menuBusy}
                onClick={() => void submitPref(menu.c.id, { mute: { mode: "none" } })}
              >
                取消静音{menu.c.preference?.mute_forever ? "（永久）" : ""}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  className="ctx-item"
                  disabled={menuBusy}
                  onClick={() =>
                    void submitPref(menu.c.id, {
                      mute: { mode: "until", until: muteOneHour() },
                    })
                  }
                >
                  静音 1 小时
                </button>
                <button
                  type="button"
                  className="ctx-item"
                  disabled={menuBusy}
                  onClick={() =>
                    void submitPref(menu.c.id, {
                      mute: { mode: "until", until: muteToday() },
                    })
                  }
                >
                  今天内静音
                </button>
                <button
                  type="button"
                  className="ctx-item"
                  disabled={menuBusy}
                  onClick={() =>
                    void submitPref(menu.c.id, { mute: { mode: "forever" } })
                  }
                >
                  永久静音
                </button>
              </>
            ))}
          {menuErr && <div className="ctx-err">{menuErr}</div>}
        </CtxMenu>
      )}
    </aside>
  );
}
