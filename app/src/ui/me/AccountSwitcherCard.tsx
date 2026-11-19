import { useState } from "react";
import {
  addAccount,
  currentAccountId,
  getServer,
  listServers,
  removeAccount,
  savedAccounts,
  serverLabel,
  switchToAccount,
  type SavedAccount,
} from "../../api";
import { useApp } from "../../store";
import Avatar from "../Avatar";
import { confirmDialog } from "../Modal";
import { PlusIcon, TrashIcon, XIcon } from "../icons";

/** 账号与安全页的多账号切换卡片：支持同站或跨服务器添加多个账号，一键秒切。 */
export default function AccountSwitcherCard() {
  const { me } = useApp();
  const [accounts, setAccounts] = useState<SavedAccount[]>(() => savedAccounts());
  const curId = currentAccountId();
  const [modalOpen, setModalOpen] = useState(false);

  const [serverUrl, setServerUrl] = useState(() => getServer());
  const [serverName, setServerName] = useState(() => {
    const sv = listServers().find((x) => x.url === getServer());
    return sv?.name ?? "";
  });
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [switchNow, setSwitchNow] = useState(true);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const refresh = () => setAccounts(savedAccounts());

  const onRemove = async (acc: SavedAccount) => {
    if (
      !(await confirmDialog(
        `从快捷切换列表中移除「${acc.user.display_name || acc.user.username}」？`,
        { danger: true, okText: "移除" },
      ))
    )
      return;
    setAccounts(removeAccount(acc.id));
  };

  const onAdd = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setErr("");
    setBusy(true);
    try {
      const added = await addAccount(
        serverUrl,
        serverName.trim() || undefined,
        username.trim(),
        password,
      );
      refresh();
      setModalOpen(false);
      setUsername("");
      setPassword("");
      if (switchNow) {
        switchToAccount(added.id);
      }
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "登录验证失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="st-card">
      <div className="st-card-head">
        <div>
          <h3>多账号登录与切换</h3>
          <div className="st-card-sub">
            同一或不同服务器添加多个身份凭证，单连接一键秒切无需重复输入密码
          </div>
        </div>
        <button
          type="button"
          className="btn-accent"
          onClick={() => {
            setErr("");
            setModalOpen(true);
          }}
        >
          <PlusIcon size={14} /> 添加账号
        </button>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {accounts.map((acc) => {
          const active = acc.id === curId;
          const user = active && me?.id === acc.user.id ? me : acc.user;
          const svLabel = acc.serverName || serverLabel(acc.serverUrl);
          return (
            <div key={acc.id} className={`account-card${active ? " active" : ""}`}>
              <Avatar
                size="sm"
                userId={user.id}
                name={user.display_name}
                avatarVersion={user.avatar_version}
                hasAvatar={user.has_avatar}
              />
              <div className="account-meta">
                <div className="account-name">
                  <span>{user.display_name || user.username}</span>
                  <span className="muted" style={{ fontWeight: 400, fontSize: 12 }}>
                    @{user.username}
                  </span>
                  {active && <span className="account-tag">当前使用</span>}
                </div>
                <div className="account-server">{svLabel}</div>
              </div>
              {!active && (
                <>
                  <button
                    type="button"
                    className="btn-ghost"
                    onClick={() => switchToAccount(acc.id)}
                  >
                    切换
                  </button>
                  <button
                    type="button"
                    className="icon-btn"
                    title="移除该账号记录"
                    onClick={() => void onRemove(acc)}
                  >
                    <TrashIcon size={14} />
                  </button>
                </>
              )}
            </div>
          );
        })}
      </div>

      {modalOpen && (
        <div className="acp-modal-mask" onClick={() => setModalOpen(false)}>
          <form
            className="acp-modal"
            role="dialog"
            aria-modal="true"
            aria-label="添加切换账号"
            onClick={(e) => e.stopPropagation()}
            onSubmit={onAdd}
          >
            <div className="st-card-head">
              <h3>添加登录账号</h3>
              <button
                type="button"
                className="icon-btn"
                onClick={() => setModalOpen(false)}
                aria-label="关闭"
              >
                <XIcon size={16} />
              </button>
            </div>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>服务器地址</span>
              <input
                value={serverUrl}
                onChange={(e) => setServerUrl(e.target.value)}
                placeholder="https://chat.example.com"
                required
              />
            </label>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>站点备注名（可选）</span>
              <input
                value={serverName}
                onChange={(e) => setServerName(e.target.value)}
                placeholder="如：研发主站 / 个人测试服"
              />
            </label>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>用户名</span>
              <input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder="username"
                required
              />
            </label>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>密码</span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="登录密码"
                required
              />
            </label>

            <label className="row" style={{ gap: 8, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={switchNow}
                onChange={(e) => setSwitchNow(e.target.checked)}
              />
              <span style={{ fontSize: 13 }}>登录成功后立即切换到该账号</span>
            </label>

            {err && <div className="auth-err">{err}</div>}

            <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
              <button
                type="button"
                className="btn-ghost"
                onClick={() => setModalOpen(false)}
              >
                取消
              </button>
              <button type="submit" className="btn-accent" disabled={busy}>
                {busy ? "验证登录中…" : "验证并添加"}
              </button>
            </div>
          </form>
        </div>
      )}
    </section>
  );
}
