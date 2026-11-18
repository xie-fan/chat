import { useState } from "react";
import {
  addServerEntry,
  confirmServerRemove,
  getServer,
  hasServerAuth,
  isSameOriginServer,
  listServers,
  normalizeServerUrl,
  removeServer,
  serverLabel,
  setServer,
  skipAutoLoginOnce,
} from "../../api";
import type { ServerEntry } from "../../servers";
import { flushDrafts, useApp } from "../../store";
import SetRow from "./SetRow";

/** 「我」页通用：当前服务器 + 切换/添加/删除服务器。
 * 切换 = 写入目标服务器为本标签页当前 → 整页重载，复用正常启动/离线流程。 */
export default function ServerSetting() {
  const s = useApp();
  const [open, setOpen] = useState(false);
  const [list, setList] = useState<ServerEntry[]>(() => listServers());
  const [addr, setAddr] = useState("");
  const [err, setErr] = useState("");
  const cur = getServer();

  const switchTo = (url: string) => {
    if (url === cur) return;
    flushDrafts();
    setServer(url);
    location.reload();
  };

  const del = (sv: ServerEntry) => {
    if (!confirmServerRemove(sv)) return;
    flushDrafts();
    if (sv.url === cur) {
      // 删的是当前服务器：先设跳过自动登录再换到最近使用的另一个，回登录页——
      // 即使回退服务器还带着凭据也不自动登入
      skipAutoLoginOnce();
      const fallback = removeServer(sv.url);
      setServer(fallback);
      location.reload();
    } else {
      removeServer(sv.url);
      setList(listServers());
    }
  };

  const add = () => {
    const norm = normalizeServerUrl(addr);
    if (!norm) {
      setErr("服务器地址格式不对，如 http://192.168.1.5:28080");
      return;
    }
    // https 页面连 http 服务器会被浏览器拦截，先说清楚再放行
    if (
      location.protocol === "https:" &&
      norm.startsWith("http://") &&
      !window.confirm("浏览器会阻止 https 页面访问 http 服务器，仍要添加吗？")
    )
      return;
    addServerEntry(norm);
    setAddr("");
    setErr("");
    setList(listServers());
  };

  return (
    <>
      <SetRow
        label="服务器"
        desc={serverLabel(cur, list.find((x) => x.url === cur))}
      >
        <span className="set-value">
          <span className={`dot${s.wsReady ? " on" : ""}`} />
          {s.wsReady ? "已连接" : "未连接"}
        </span>
        <button onClick={() => setOpen((o) => !o)}>{open ? "收起" : "切换服务器"}</button>
      </SetRow>
      {open && (
        <div className="set-block srv-panel">
          {list.map((sv) => (
            <div className="srv-item" key={sv.url}>
              <div className="grow">
                <div>
                  {serverLabel(sv.url, sv)}
                  {sv.url === cur && <span className="muted">（当前）</span>}
                </div>
                <div className="muted">
                  {serverLabel(sv.url, sv) !== sv.url && `${sv.url} · `}
                  {sv.url === cur
                    ? "本机正在使用"
                    : hasServerAuth(sv.url)
                      ? `已登录${sv.username ? ` @${sv.username}` : ""}`
                      : "未登录"}
                </div>
              </div>
              {sv.url !== cur && (
                <button onClick={() => switchTo(sv.url)}>切换</button>
              )}
              {!isSameOriginServer(sv.url) && (
                <button className="danger" onClick={() => del(sv)}>
                  删除
                </button>
              )}
            </div>
          ))}
          <div className="srv-add">
            <input
              placeholder="添加服务器，如 http://192.168.1.5:28080"
              value={addr}
              onChange={(e) => setAddr(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") add();
              }}
            />
            <button onClick={add}>添加</button>
          </div>
          {err && <div className="mut-err">{err}</div>}
        </div>
      )}
    </>
  );
}
