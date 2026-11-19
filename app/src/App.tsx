import { useEffect, useState } from "react";
import { auth, clearSkipAutoLogin, getServer, hasSkipAutoLogin, loadSession, serverScope } from "./api";
import { startAcpHost, stopAcpHost } from "./acpHost";
import { draftsKey, readDrafts } from "./drafts";
import { clearReedits, purgeReedits } from "./reedit";
import { cacheKey, clearRecent, readLocalCache, seedRecent } from "./localCache";
import { listenForNotificationClicks, notificationReady, syncWindowTitle } from "./notify";
import { flushDrafts, getState, resetSessionState, setState, useApp } from "./store";
import { openConversationFromNotification, startWs, stopWs } from "./ws";
import AuthScreen from "./ui/AuthScreen";
import ChatScreen from "./ui/ChatScreen";
import { DialogHost, dismissAllDialogs } from "./ui/Modal";

export default function App() {
  const s = useApp();
  const [booting, setBooting] = useState(() => !hasSkipAutoLogin() && loadSession() !== null);
  const [bootSeq, setBootSeq] = useState(0);

  useEffect(() => {
    clearSkipAutoLogin();
  }, []);

  // 未读消息数同步窗口标题
  useEffect(() => {
    syncWindowTitle(s.conversations);
  }, [s.conversations]);

  useEffect(() => {
    if (!s.me) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenForNotificationClicks((id) => {
      if (!disposed && getState().me) openConversationFromNotification(id);
    }).then((stop) => {
      if (disposed) stop?.();
      else {
        unlisten = stop;
        void notificationReady().catch(() => {});
      }
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [s.me?.id]);

  // 桌面壳登录/切换账号后自动接入本地 ACP 宿主；退出时关闭
  useEffect(() => {
    if (s.me) {
      void startAcpHost(getServer());
    } else {
      void stopAcpHost();
    }
  }, [s.me?.id, bootSeq]);

  // 401/403 或手动登出：清敏感暂存与复用附件 chip；切账号仅切换视图态并保留多账号隔离数据
  useEffect(() => {
    const onBeforeSwitch = () => {
      flushDrafts();
    };
    const onSwitch = () => {
      clearRecent();
      dismissAllDialogs();
      stopWs();
      void stopAcpHost();
      setState(resetSessionState());
      setBooting(loadSession() !== null);
      setBootSeq((n) => n + 1);
    };
    const onLogout = () => {
      const { me, drafts } = getState();
      const scope = serverScope();
      const clean = Object.fromEntries(
        Object.entries(drafts)
          .filter(([, e]) => e.text)
          .map(([k, e]) => [k, { text: e.text }]),
      );
      setState({ drafts: clean });
      flushDrafts();
      if (me?.id) {
        clearReedits(localStorage, scope, me.id);
        if (Object.keys(clean).length === 0) {
          try {
            localStorage.removeItem(draftsKey(scope, me.id));
          } catch {}
        }
      }
      clearRecent();
      dismissAllDialogs();
      stopWs();
      void stopAcpHost();
      setState(resetSessionState());
      setBooting(false);
    };
    window.addEventListener("chat:before-switch-account", onBeforeSwitch);
    window.addEventListener("chat:switch-account", onSwitch);
    window.addEventListener("chat:logout", onLogout);
    return () => {
      window.removeEventListener("chat:before-switch-account", onBeforeSwitch);
      window.removeEventListener("chat:switch-account", onSwitch);
      window.removeEventListener("chat:logout", onLogout);
    };
  }, []);

  useEffect(() => {
    if (!booting) return;
    let cancelled = false;
    (async () => {
      const cached = loadSession()?.user;
      if (cached) {
        const snap = readLocalCache(localStorage, cacheKey(serverScope(), cached.id));
        if (snap) {
          seedRecent(snap.recent);
          setState({ conversations: snap.conversations, messages: snap.messages, friends: snap.friends ?? [], friendsLoaded: false });
        }
      }
      try {
        const me = await auth.me();
        if (cancelled) return;
        purgeReedits(localStorage, serverScope(), me.id);
        setState({ me, drafts: readDrafts(localStorage, serverScope(), me.id) });
        void startWs();
      } catch {
        if (cancelled) return;
        const cached = loadSession()?.user;
        if (cached) {
          purgeReedits(localStorage, serverScope(), cached.id);
          const snap = readLocalCache(localStorage, cacheKey(serverScope(), cached.id));
          if (snap) seedRecent(snap.recent);
          setState({
            me: cached,
            drafts: readDrafts(localStorage, serverScope(), cached.id),
            conversations: snap?.conversations ?? [],
            messages: snap?.messages ?? {},
            friends: snap?.friends ?? [],
            friendsLoaded: false,
          });
          void startWs();
        }
      } finally {
        if (!cancelled) setBooting(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [booting, bootSeq]);

  return (
    <>
      {booting ? (
        <div className="boot">
          <div className="boot-pill">
            <span className="spinner-dot" />
            <span>正在加载会话…</span>
          </div>
        </div>
      ) : s.me ? (
        <ChatScreen />
      ) : (
        <AuthScreen />
      )}
      <DialogHost />
    </>
  );
}
