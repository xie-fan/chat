import { useEffect, useRef, useState } from "react";
import { auth, osOfUa, serverInfo } from "../../api";
import type { PushDevice, PushState } from "../../types";
import SetRow from "./SetRow";

/**
 * ===== V2 P3-3 ===== 「我」页手机推送卡片（ntfy）。
 * 推送服务器地址只能由管理员配置；每台手机登记后拿到自己的随机订阅地址，可单独移除。
 */
export default function PushSettings() {
  const [info, setInfo] = useState<PushState | null>(null);
  const [fcmAvailable, setFcmAvailable] = useState(false);
  const android = "__TAURI_INTERNALS__" in window && osOfUa(navigator.userAgent) === "Android";
  const [err, setErr] = useState("");
  const [hint, setHint] = useState("");
  const [busy, setBusy] = useState("");
  const [copied, setCopied] = useState("");
  const [name, setName] = useState("");
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (android) {
      serverInfo()
        .then((v) => setFcmAvailable(v.android_fcm_available === true))
        .catch(() => setFcmAvailable(false));
    }
    auth
      .push()
      .then(setInfo)
      .catch((e) => setErr(e instanceof Error ? e.message : "加载失败"));
  }, [android]);

  const run = async (what: string, f: () => Promise<PushState>) => {
    if (busy) return false;
    setBusy(what);
    setErr("");
    setHint("");
    try {
      setInfo(await f());
      return true;
    } catch (e) {
      setErr(e instanceof Error ? e.message : "操作失败");
      return false;
    } finally {
      setBusy("");
    }
  };

  const copy = async (d: PushDevice) => {
    try {
      await navigator.clipboard.writeText(d.subscribe_url!);
      setCopied(d.id);
      if (copiedTimer.current) clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(""), 2000);
    } catch {
      // 剪贴板不可用（非安全上下文/权限被拒）→ 弹出手动复制
      window.prompt("复制失败，请手动复制：", d.subscribe_url!);
    }
  };

  const add = () =>
    void run("add", () => auth.addPushDevice(name.trim())).then((ok) => {
      if (!ok) return;
      setName("");
      setHint("已添加，在这台手机的 ntfy App 里订阅它的地址");
    });

  const remove = (d: PushDevice) => {
    if (!confirm(`移除「${d.name}」？它将不再收到推送。`)) return;
    void run(`rm-${d.id}`, () => auth.removePushDevice(d.id));
  };

  const test = (d: PushDevice) =>
    void run(`test-${d.id}`, async () => {
      const v = await auth.testPushDevice(d.id);
      setHint(`已向「${d.name}」发送测试推送，请查看 ntfy App`);
      return v;
    });

  if (info == null)
    return (
      <SetRow label="手机推送" desc={err ? <span className="auth-err">{err}</span> : "加载中…"} />
    );
  const fcmRow = fcmAvailable ? (
    <SetRow label="手机推送" desc="系统推送（FCM）已启用，无需额外配置" />
  ) : null;
  if (!info.available) return fcmRow ?? <SetRow label="手机推送" desc="管理员未配置推送服务" />;

  return (
    <>
      {fcmRow}
      <SetRow
        label={fcmAvailable ? "额外推送（ntfy）" : "手机推送"}
        desc="新消息通过 ntfy 推到手机；在任一端读过后自动消除"
        toggle={{
          checked: info.enabled,
          disabled: busy !== "",
          onChange: () => void run("toggle", () => auth.updatePush({ enabled: !info.enabled })),
        }}
      />
      {info.enabled && (
        <SetRow
          label="推送显示消息内容"
          desc="公共 ntfy 服务器上订阅了同一主题的人可能看到推送内容"
          toggle={{
            checked: info.preview,
            disabled: busy !== "",
            onChange: (v) => void run("preview", () => auth.updatePush({ preview: v })),
          }}
        />
      )}
      {info.enabled && (
        <div className="set-block">
          <div className="muted">每台手机添加一项 → 在它的 ntfy App 里订阅对应地址</div>
          {info.devices.map((d) => (
            <div key={d.id} className="set-block">
              <div className="row push-actions">
                <span className="grow">{d.name}</span>
                <button disabled={busy !== ""} onClick={() => test(d)}>
                  {busy === `test-${d.id}` ? "发送中…" : "测试"}
                </button>
                <button className="danger" disabled={busy !== ""} onClick={() => remove(d)}>
                  移除
                </button>
              </div>
              {d.subscribe_url && (
                <div className="row push-url">
                  <code className="push-url-text">{d.subscribe_url}</code>
                  <button onClick={() => void copy(d)}>{copied === d.id ? "已复制" : "复制"}</button>
                </div>
              )}
            </div>
          ))}
          <div className="row">
            <input
              className="grow"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="手机名称（如 我的 iPhone）"
              maxLength={50}
            />
            <button disabled={busy !== "" || !name.trim()} onClick={add}>
              {busy === "add" ? "添加中…" : "添加手机"}
            </button>
          </div>
        </div>
      )}
      {(err || hint) && (
        <div className="set-block">
          {err && <div className="auth-err">{err}</div>}
          {hint && <div className="muted">{hint}</div>}
        </div>
      )}
    </>
  );
}
