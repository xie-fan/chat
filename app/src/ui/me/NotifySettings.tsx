import { useEffect, useState } from "react";
import {
  notifyEnabled,
  notifyPermission,
  notifyPreview,
  notifySupport,
  requestNotifyPermission,
  setNotifyEnabled,
  setNotifyPreview,
  showSystemNotification,
  type NotifyPermission,
} from "../../notify";
import SetRow from "./SetRow";

const PERMISSION_LABEL: Record<NotifyPermission, string> = {
  granted: "已允许",
  denied: "已拒绝，请在浏览器 / 系统设置中允许通知",
  default: "未请求",
  unavailable: "无法读取",
};

/** ===== V2 P1-1 ===== 「我」页通知设置卡片（桌面通知开关 / 预览开关 / 测试通知） */
export default function NotifySettings() {
  const support = notifySupport();
  const [enabled, setEnabled] = useState(notifyEnabled);
  const [preview, setPreview] = useState(notifyPreview);
  const [perm, setPerm] = useState<NotifyPermission>("default");
  const [busy, setBusy] = useState(false);
  const [hint, setHint] = useState("");

  useEffect(() => {
    if (support === "none") return;
    let active = true;
    const refresh = () => void notifyPermission().then((p) => {
      if (active) setPerm(p);
    });
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      active = false;
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [support]);

  const toggleEnabled = async () => {
    if (support === "none" || busy) return;
    if (enabled) {
      setEnabled(false);
      setNotifyEnabled(false);
      setHint("");
      return;
    }
    setBusy(true);
    try {
      // 权限请求必须在点击事件里发起，浏览器不允许自动弹权限框
      const ok = await requestNotifyPermission();
      const p = await notifyPermission();
      setPerm(p);
      setEnabled(true);
      setNotifyEnabled(true);
      setHint(ok ? "" : "未获得通知权限");
    } finally {
      setBusy(false);
    }
  };

  const togglePreview = (v: boolean) => {
    setPreview(v);
    setNotifyPreview(v);
  };

  const sendTest = async () => {
    setBusy(true);
    try {
      const ok = await requestNotifyPermission();
      setPerm(await notifyPermission());
      const shown = ok && await showSystemNotification({
        title: "Chat",
        body: "这是一条测试通知",
      });
      setHint(shown ? "已发送，请查看系统通知中心" : "未能发送测试通知");
    } finally {
      setBusy(false);
    }
  };

  if (support === "none") return <SetRow label="桌面通知" desc="当前环境不支持系统通知" />;

  return (
    <>
      <SetRow
        label="桌面通知"
        desc="窗口不在前台时弹出系统通知"
        toggle={{
          checked: enabled,
          disabled: busy,
          onChange: () => void toggleEnabled(),
        }}
      />
      <SetRow label="通知显示消息内容" toggle={{ checked: preview, onChange: togglePreview }} />
      <SetRow
        label="测试通知"
        desc={`权限：${PERMISSION_LABEL[perm]}${hint ? ` · ${hint}` : ""}`}
      >
        <button disabled={busy} onClick={() => void sendTest()}>
          发送
        </button>
      </SetRow>
    </>
  );
}
