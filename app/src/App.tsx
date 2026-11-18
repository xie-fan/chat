import { useEffect, useState } from "react";
import { auth, getServer, loadSession, serverScope } from "./api";
import { startAcpHost, stopAcpHost } from "./acpHost";
import { readDrafts } from "./drafts";
import { purgeReedits } from "./reedit";
import { cacheKey, clearRecent, readLocalCache, seedRecent } from "./localCache";
import { syncWindowTitle, unreadTitle } from "./notify";
import { flushDrafts, getState, resetSessionState, setState, useApp } from "./store";
import { startWs, stopWs } from "./ws";
import AuthScreen from "./ui/AuthScreen";
import ChatScreen from "./ui/ChatScreen";
import { DialogHost, dismissAllDialogs } from "./ui/Modal";

export default function App() {
  const s = useApp();
  const [booting, setBooting] = useState(() => loadSession() !== null);
  const [bootSeq, setBootSeq] = useState(0);

  // 未读消息数同步窗口标题
  useEffect(() => {
    syncWindowTitle(unreadTitle(s.conversations));
  }, [s.conversations]);

  // 桌面壳登录/切换账号后自动接入本地 ACP 宿主；退出时关闭
  useEffect(() => {
    if (s.me) {
      void startAcpHost(getServer());
    } else {
      void stopAcpHost();
    }
  }, [s.me?.id]);

  // 401/403 或手动登出：停 WS/ACP、丢内存态回 AuthScreen
  useEffect(() => {
    const clearClient = () => {
      flushDrafts();
      const uid = getState().me?.id;
      if (uid) purgeReedits(localStorage, serverScope(), uid);
      clearRecent();
      dismissAllDialogs();
      stopWs();
      void stopAcpHost();
      setState(resetSessionState());
    };
    const onLogout = () => {
      clearClient();
      setBooting(false);
    };
    const onSwitch = () => {
      clearClient();
      setBooting(loadSession() !== null);
      setBootSeq((n) => n + 1);
    };
    window.addEventListener("chat:logout", onLogout);
    window.addEventListener("chat:switch-account", onSwitch);
    return () => {
      window.removeEventListener("chat:logout", onLogout);
      window.removeEventListener("chat:switch-account", onSwitch);
    };
  }, []);

  useEffect(() => {
    if (!booting) return;
    let cancelled = false;
    (async () => {
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
