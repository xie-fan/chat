import { useEffect, useRef, useState } from "react";
import {
  auth,
  confirmServerRemove,
  getServer,
  inTauri,
  isSameOriginServer,
  listServers,
  removeServer,
  saveSession,
  savedAccounts,
  serverInfo,
  serverLabel,
  serverScope,
  setServer,
  switchToAccount,
  takeKickReason,
  type SavedAccount,
  type ServerEntry,
} from "../api";
import { readDrafts } from "../drafts";
import { parseInviteParam } from "../invite";
import {
  firstEmptyCredential,
  isImeEnter,
  needsInsecureConfirm,
  planServerEnter,
  requestFormSubmit,
  resolveServerBlur,
  resolveServerInput,
} from "../servers";
import { setState } from "../store";
import {
  readThemeChoice,
  resolveRenderedDark,
  toggleQuickTheme,
} from "../theme";
import type { RegistrationMode } from "../types";
import { startWs } from "../ws";
import {
  ChevronDownIcon,
  CpuIcon,
  EyeIcon,
  EyeOffIcon,
  KeyIcon,
  LockIcon,
  MoonIcon,
  PlusIcon,
  ServerIcon,
  SunIcon,
  TrashIcon,
  UserIcon,
} from "./icons";
import { confirmDialog } from "./Modal";

export default function AuthScreen() {
  const [mode, setMode] = useState<"login" | "register">(() =>
    parseInviteParam(window.location.search) ? "register" : "login",
  );
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPw, setShowPw] = useState(false);
  const [displayName, setDisplayName] = useState("");
  const [inviteCode, setInviteCode] = useState(() =>
    parseInviteParam(window.location.search),
  );
  const [regMode, setRegMode] = useState<RegistrationMode>("open");
  const [server, setServerUrl] = useState(getServer());
  const [servers, setServers] = useState<ServerEntry[]>(() => listServers());
  const [accounts, setAccounts] = useState<SavedAccount[]>(() => savedAccounts());
  const [editServer, setEditServer] = useState(false);
  const [err, setErr] = useState(takeKickReason);
  const [busy, setBusy] = useState(false);
  const [isDark, setIsDark] = useState(() =>
    resolveRenderedDark(readThemeChoice(localStorage)),
  );

  const formRef = useRef<HTMLFormElement>(null);
  const userRef = useRef<HTMLInputElement>(null);
  const passRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
    const sync = () =>
      setIsDark(resolveRenderedDark(readThemeChoice(localStorage)));
    mq?.addEventListener("change", sync);
    return () => mq?.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    let stale = false;
    const t = setTimeout(() => {
      void serverInfo(server).then((i) => {
        if (!stale) setRegMode(i.registration_mode);
      });
    }, 500);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [server]);

  const effMode = regMode === "closed" ? "login" : mode;
  const desktop = inTauri();
  const cur = getServer();

  const focusCred = (which: "username" | "password") => {
    (which === "username" ? userRef : passRef).current?.focus();
  };

  const pickServer = (url: string) => {
    setServer(url);
    setServerUrl(getServer());
    setErr("");
    setEditServer(false);
  };

  const removeSv = async (sv: ServerEntry) => {
    if (confirmServerRemove(sv)) {
      const label = sv.name || sv.url || "当前站点";
      if (
        !(await confirmDialog(
          `服务器「${label}」上仍有已保存的登录账号，移除后需要重新登录。确定移除？`,
          { danger: true, okText: "移除" },
        ))
      ) {
        return;
      }
    }
    const fallback = removeServer(sv.url);
    setServers(listServers());
    setAccounts(savedAccounts());
    if (sv.url === cur) {
      setServer(fallback);
      setServerUrl(getServer());
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setErr("");

    const r0 = resolveServerInput(server, { desktop, origin: location.origin });
    if (!r0.ok) {
      setErr(r0.err);
      setEditServer(true);
      return;
    }

    const norm = r0.url;
    if (
      needsInsecureConfirm(desktop, location.protocol, norm) &&
      !(await confirmDialog(
        `即将通过未加密的 HTTP 连接「${norm}」，凭据可能被窃听。确定继续？`,
        { danger: true, okText: "继续连接" },
      ))
    ) {
      return;
    }

    if (editServer) {
      const empty = firstEmptyCredential(username, password);
      if (empty) {
        setServer(norm);
        setServerUrl(norm);
        setServers(listServers());
        setEditServer(false);
        focusCred(empty);
        return;
      }
    }

    setBusy(true);
    try {
      setServer(norm);
      setServerUrl(norm);
      const r =
        effMode === "login"
          ? await auth.login(username.trim(), password)
          : await auth.register(
              username.trim(),
              password,
              displayName.trim() || undefined,
              inviteCode.trim() || undefined,
            );
      saveSession(r);
      setServers(listServers());
      setAccounts(savedAccounts());
      setState({
        me: r.user,
        drafts: readDrafts(localStorage, serverScope(), r.user.id),
      });
      void startWs();
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "操作失败");
      focusCred(firstEmptyCredential(username, password) ?? "password");
    } finally {
      setBusy(false);
    }
  };

  const onServerKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter") return;
    const plan = planServerEnter({
      composing: isImeEnter(e.nativeEvent),
      busy,
      input: server,
      username,
      password,
      desktop,
      origin: location.origin,
    });
    if (plan.t === "ignore") {
      if (busy) e.preventDefault();
      return;
    }
    e.preventDefault();
    if (plan.t === "err") {
      setErr(plan.err);
      return;
    }
    setServer(plan.url);
    setServerUrl(plan.url);
    setServers(listServers());
    if (plan.t === "focus") {
      setEditServer(false);
      focusCred(plan.field);
      return;
    }
    requestFormSubmit(formRef.current);
  };

  const toggleTheme = () => {
    const next = toggleQuickTheme();
    setIsDark(next === "dark");
  };

  return (
    <div className="auth-wrap">
      <button
        type="button"
        className="auth-theme-pill"
        onClick={toggleTheme}
        title="切换浅色 / 深色主题"
      >
        {isDark ? <SunIcon size={15} /> : <MoonIcon size={15} />}
        <span>{isDark ? "浅色模式" : "深色模式"}</span>
      </button>

      <div className="auth-col">
        <div className="auth-showcase">
          <div className="auth-hero">
            <div className="auth-brand">
              <div className="auth-brand-icon">
                <CpuIcon size={20} />
              </div>
              <span>Chat Workspace</span>
            </div>

            <div className="auth-hero-art" aria-hidden="true">
              <svg width="240" height="168" viewBox="0 0 240 168" fill="none">
                <rect
                  x="16"
                  y="16"
                  width="208"
                  height="136"
                  rx="18"
                  fill="rgba(255,255,255,0.10)"
                  stroke="rgba(255,255,255,0.22)"
                />
                <rect
                  x="30"
                  y="34"
                  width="98"
                  height="32"
                  rx="10"
                  fill="rgba(255,255,255,0.22)"
                />
                <circle cx="46" cy="50" r="8" fill="#A5B4FC" />
                <rect x="60" y="45" width="52" height="5" rx="2.5" fill="rgba(255,255,255,0.8)" />
                <rect x="60" y="54" width="34" height="4" rx="2" fill="rgba(255,255,255,0.5)" />

                <rect
                  x="96"
                  y="78"
                  width="114"
                  height="34"
                  rx="10"
                  fill="rgba(255,255,255,0.16)"
                  stroke="rgba(255,255,255,0.25)"
                />
                <circle cx="192" cy="95" r="8" fill="#34D399" />
                <rect x="110" y="89" width="66" height="5" rx="2.5" fill="#fff" />
                <rect x="128" y="98" width="48" height="4" rx="2" fill="rgba(255,255,255,0.55)" />

                <rect
                  x="30"
                  y="120"
                  width="180"
                  height="18"
                  rx="9"
                  fill="rgba(15,23,42,0.28)"
                />
                <circle cx="42" cy="129" r="3" fill="#34D399" />
                <rect x="52" y="127" width="84" height="4" rx="2" fill="rgba(255,255,255,0.72)" />
              </svg>
            </div>

            <div>
              <h2>连接团队与本地 AI 工作流</h2>
              <p>
                实时协同聊天、群公告置顶与文件流转，集成多账号一键切换与 ACP 本地机器智能体工作区。
              </p>
            </div>
          </div>

          <form ref={formRef} className="auth-form-pane" onSubmit={submit}>
            {regMode !== "closed" ? (
              <div className="auth-tabs">
                <button
                  type="button"
                  className={`auth-tab${effMode === "login" ? " on" : ""}`}
                  onClick={() => {
                    setMode("login");
                    setErr("");
                  }}
                >
                  登录账号
                </button>
                <button
                  type="button"
                  className={`auth-tab${effMode === "register" ? " on" : ""}`}
                  onClick={() => {
                    setMode("register");
                    setErr("");
                  }}
                >
                  注册新用户
                </button>
              </div>
            ) : (
              <div className="auth-note">当前服务器已关闭公开注册，请使用已有账号登录。</div>
            )}

            <div className="auth-field">
              <span className="auth-field-icon">
                <UserIcon />
              </span>
              <input
                ref={userRef}
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder="用户名"
                autoComplete="username"
                required
              />
            </div>

            {effMode === "register" && (
              <div className="auth-field">
                <span className="auth-field-icon">
                  <PlusIcon size={16} />
                </span>
                <input
                  value={displayName}
                  onChange={(e) => setDisplayName(e.target.value)}
                  placeholder="显示昵称（可选）"
                  maxLength={50}
                />
              </div>
            )}

            <div className="auth-field">
              <span className="auth-field-icon">
                <LockIcon size={17} />
              </span>
              <input
                ref={passRef}
                type={showPw ? "text" : "password"}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={effMode === "login" ? "密码" : "设置密码（6-72 位）"}
                autoComplete={effMode === "login" ? "current-password" : "new-password"}
                required
              />
              <button
                type="button"
                className="auth-eye-btn"
                onClick={() => setShowPw((v) => !v)}
                aria-label={showPw ? "隐藏密码" : "显示密码"}
              >
                {showPw ? <EyeOffIcon size={16} /> : <EyeIcon />}
              </button>
            </div>

            {effMode === "register" && regMode === "invite" && (
              <div className="auth-field">
                <span className="auth-field-icon">
                  <KeyIcon size={16} />
                </span>
                <input
                  value={inviteCode}
                  onChange={(e) => setInviteCode(e.target.value)}
                  placeholder="邀请码（必填）"
                  required
                />
              </div>
            )}

            {err && <div className="auth-err">{err}</div>}

            <button className="auth-submit" disabled={busy}>
              {busy ? "处理中…" : effMode === "login" ? "立即登录" : "创建账号并登录"}
            </button>

            {accounts.length > 0 && (
              <div className="auth-saved-box">
                <div className="auth-saved-title">已保存账号 · 一键进入</div>
                <div className="auth-saved-list">
                  {accounts.slice(0, 6).map((acc) => (
                    <button
                      key={acc.id}
                      type="button"
                      className="auth-saved-chip"
                      onClick={() => {
                        setErr("");
                        switchToAccount(acc.id);
                      }}
                      title={`${acc.user.display_name} @ ${acc.serverName || acc.serverUrl}`}
                    >
                      <span>{acc.user.display_name || acc.user.username}</span>
                      <span className="muted">@{acc.user.username}</span>
                    </button>
                  ))}
                </div>
              </div>
            )}
          </form>
        </div>

        <div className="auth-server">
          {!editServer ? (
            <button
              type="button"
              className="auth-server-pill"
              onClick={() => {
                setServers(listServers());
                setEditServer(true);
              }}
            >
              <ServerIcon />
              <span>服务器：{serverLabel(cur, servers.find((x) => x.url === cur))}</span>
              <ChevronDownIcon size={14} />
            </button>
          ) : (
            <div className="st-card" style={{ width: "min(440px, 92vw)" }}>
              <div className="st-card-head">
                <h3>选择或输入服务器</h3>
                <button
                  type="button"
                  className="btn-ghost"
                  onClick={() => setEditServer(false)}
                >
                  收起
                </button>
              </div>
              <div className="auth-server-form">
                <input
                  value={server}
                  onChange={(e) => setServerUrl(e.target.value)}
                  onKeyDown={onServerKeyDown}
                  onBlur={() => {
                    const r = resolveServerBlur(server, {
                      desktop,
                      origin: location.origin,
                    });
                    if (r.t === "commit") {
                      setServer(r.url);
                      setServerUrl(r.url);
                      setServers(listServers());
                    } else if (r.t === "collapse") {
                      setEditServer(false);
                    } else {
                      setErr(r.err);
                    }
                  }}
                  placeholder={desktop ? "https://chat.example.com" : "留空使用当前站点"}
                />
                {servers.length > 0 && (
                  <div className="auth-server-list">
                    {servers.map((sv) => (
                      <div key={sv.url} className="auth-server-item">
                        <button
                          type="button"
                          className="auth-server-pick"
                          onClick={() => pickServer(sv.url)}
                        >
                          <span>{sv.name || (isSameOriginServer(sv.url) ? "当前站点" : sv.url)}</span>
                          {sv.url && <span className="muted">{sv.url}</span>}
                        </button>
                        {(sv.url !== cur || servers.length > 1) && (
                          <button
                            type="button"
                            className="auth-server-del"
                            aria-label="移除服务器"
                            onClick={() => void removeSv(sv)}
                          >
                            <TrashIcon size={14} />
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
