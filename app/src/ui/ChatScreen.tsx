import { useEffect, useState } from "react";
import { revealDownloaded } from "../download";
import {
  dismissToast,
  isMuted,
  pendingAcpApprovalsCount,
  setState,
  useApp,
} from "../store";
import {
  readThemeChoice,
  resolveRenderedDark,
  toggleQuickTheme,
} from "../theme";
import { openConversation, refreshCatalogs } from "../ws";
import Avatar from "./Avatar";
import ContactsPanel from "./ContactsPanel";
import AcpWorkspaceView from "./acp/AcpWorkspaceView";
import AdminPanel from "./admin/AdminPanel";
import ChatView from "./chat/ChatView";
import ConvList from "./chat/ConvList";
import { consumeBack } from "./chat/backStack";
import {
  ChatBubbleIcon,
  ContactsIcon,
  CpuIcon,
  GearIcon,
  MoonIcon,
  ShieldIcon,
  SunIcon,
  WifiIcon,
  WifiOffIcon,
} from "./icons";
import MePanel from "./me/MePanel";

export default function ChatScreen() {
  const s = useApp();
  const [isDark, setIsDark] = useState(() =>
    resolveRenderedDark(readThemeChoice(localStorage)),
  );

  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
    const sync = () =>
      setIsDark(resolveRenderedDark(readThemeChoice(localStorage)));
    mq?.addEventListener("change", sync);
    return () => mq?.removeEventListener("change", sync);
  }, []);

  // 初始化拉会话与联系人目录 + Android 硬件返回键支持
  useEffect(() => {
    void refreshCatalogs().catch((e) => console.warn("initial load failed", e));
    history.pushState({ chat: 1 }, "");
    const onPop = () => {
      if (consumeBack()) history.pushState({ chat: 1 }, "");
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const totalUnread = s.conversations.reduce(
    (n, c) => (isMuted(c) && !c.has_mention ? n : n + c.unread_count),
    0,
  );
  const pendingReq = s.requests.incoming.length;
  const pendingApprovals = pendingAcpApprovalsCount(s.acpSessions);

  return (
    <div className="shell">
      <nav className="rail" aria-label="主导航">
        <button
          type="button"
          className="rail-avatar-btn"
          onClick={() => setState({ tab: "me" })}
          title={s.me?.display_name || "个人设置"}
          style={{ border: "none", background: "transparent", padding: 0, cursor: "pointer" }}
        >
          <Avatar
            className="rail-avatar"
            userId={s.me?.id}
            name={s.me?.display_name ?? ""}
            avatarVersion={s.me?.avatar_version}
            hasAvatar={s.me?.has_avatar}
          />
        </button>

        <button
          className={`rail-btn${s.tab === "chats" ? " on" : ""}`}
          onClick={() => setState({ tab: "chats" })}
          data-testid="rail-chats"
        >
          <ChatBubbleIcon />
          <span>会话</span>
          {totalUnread > 0 && (
            <em className="badge">{totalUnread > 99 ? "99+" : totalUnread}</em>
          )}
        </button>

        <button
          className={`rail-btn${s.tab === "contacts" ? " on" : ""}`}
          onClick={() => setState({ tab: "contacts" })}
          data-testid="rail-contacts"
        >
          <ContactsIcon />
          <span>联系人</span>
          {pendingReq > 0 && <em className="badge">{pendingReq}</em>}
        </button>

        <button
          className={`rail-btn${s.tab === "agents" ? " on" : ""}`}
          onClick={() => setState({ tab: "agents" })}
          data-testid="rail-agents"
        >
          <CpuIcon size={20} />
          <span>Agent</span>
          {pendingApprovals > 0 && (
            <em className="badge amber">{pendingApprovals}</em>
          )}
        </button>

        <button
          className={`rail-btn${s.tab === "me" ? " on" : ""}`}
          onClick={() => setState({ tab: "me" })}
          data-testid="rail-me"
        >
          <GearIcon />
          <span>设置</span>
        </button>

        {s.me?.role === "admin" && (
          <button
            className={`rail-btn${s.tab === "admin" ? " on" : ""}`}
            onClick={() => setState({ tab: "admin" })}
            data-testid="rail-admin"
          >
            <ShieldIcon />
            <span>管理</span>
          </button>
        )}

        <div className="rail-foot">
          <div
            title={s.wsReady ? "实时连接正常" : "正在重连服务器…"}
            style={{ color: s.wsReady ? "var(--ok)" : "var(--text-muted)" }}
          >
            {s.wsReady ? <WifiIcon size={16} /> : <WifiOffIcon size={16} />}
          </div>
          <button
            type="button"
            className="rail-icon-btn"
            onClick={() => setIsDark(toggleQuickTheme() === "dark")}
            title={isDark ? "切换浅色模式" : "切换深色模式"}
            aria-label="切换浅色/深色主题"
          >
            {isDark ? <SunIcon size={16} /> : <MoonIcon size={16} />}
          </button>
        </div>
      </nav>

      {s.tab === "chats" && (
        <>
          <ConvList />
          <ChatView />
        </>
      )}
      {s.tab === "contacts" && <ContactsPanel />}
      {s.tab === "agents" && <AcpWorkspaceView />}
      {s.tab === "me" && <MePanel />}
      {s.tab === "admin" && s.me?.role === "admin" && <AdminPanel />}

      <div className="toasts" aria-live="polite">
        {s.toasts.map((t) => (
          <div
            key={t.id}
            className="toast"
            onClick={() => {
              dismissToast(t.id);
              if (t.filePath) void revealDownloaded(t.filePath);
              else if (t.convId) openConversation(t.convId);
            }}
          >
            <b>{t.title}</b>
            {t.text && <div className="toast-body">{t.text}</div>}
            {t.action && (
              <button
                type="button"
                className="toast-act"
                onClick={(e) => {
                  e.stopPropagation();
                  dismissToast(t.id);
                  t.action!.run();
                }}
              >
                {t.action.label}
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
