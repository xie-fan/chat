import { convTitle, isMuted, isOwnMessage, type AppState } from "./store";
import { RECORD_TITLE_MAX, recordLabel } from "./ui/chat/chatRecord";
import { systemBarText } from "./ui/chat/systemMessage";
import type { ConversationView, MessageView } from "./types";

/**
 * ===== V2 P1-1 =====
 * 系统通知（Web Notification / Tauri 通知插件）+ 标题未读数 + 任务栏提醒。
 * 判定逻辑做成纯函数便于单测；Tauri API 一律动态 import，Web 端不受影响。
 */

/** 本设备持久化开关（localStorage；读写都吞异常，隐私模式/禁用存储不炸） */
const LS_ENABLED = "chat_notify_enabled";
const LS_PREVIEW = "chat_notify_preview";

export type NotifyKind = "web" | "tauri" | "none";
export type NotifyPermission = "granted" | "denied" | "default";

/** 系统通知能力：Tauri 壳走插件通道；浏览器要求 Web Notification 可用 */
export function notifySupport(): NotifyKind {
  if (typeof window === "undefined") return "none";
  if ("__TAURI_INTERNALS__" in window) return "tauri";
  if ("Notification" in window) return "web";
  return "none";
}

export function notifyEnabled(): boolean {
  try {
    return localStorage.getItem(LS_ENABLED) === "1";
  } catch {
    return false;
  }
}

export function setNotifyEnabled(v: boolean) {
  try {
    if (v) localStorage.setItem(LS_ENABLED, "1");
    else localStorage.removeItem(LS_ENABLED);
  } catch {}
}

/** 「通知显示消息内容」默认开 */
export function notifyPreview(): boolean {
  try {
    return localStorage.getItem(LS_PREVIEW) !== "0";
  } catch {
    return true;
  }
}

export function setNotifyPreview(v: boolean) {
  try {
    if (v) localStorage.removeItem(LS_PREVIEW);
    else localStorage.setItem(LS_PREVIEW, "0");
  } catch {}
}

export async function notifyPermission(): Promise<NotifyPermission> {
  const kind = notifySupport();
  if (kind === "web") {
    try {
      return Notification.permission;
    } catch {
      return "denied";
    }
  }
  if (kind === "tauri") {
    try {
      const n = await import("@tauri-apps/plugin-notification");
      // 插件 JS 侧 internally 读 window.Notification.permission / 走原生 is_permission_granted
      return (await n.isPermissionGranted()) ? "granted" : "default";
    } catch {
      return "default";
    }
  }
  return "denied";
}

/** 必须在用户点击事件里调用（浏览器不允许自动弹权限框） */
export async function requestNotifyPermission(): Promise<boolean> {
  const kind = notifySupport();
  try {
    if (kind === "web") return (await Notification.requestPermission()) === "granted";
    if (kind === "tauri") {
      const n = await import("@tauri-apps/plugin-notification");
      // Android 13+ 经插件 requestPermission 走运行时权限申请
      return (await n.isPermissionGranted()) || (await n.requestPermission()) === "granted";
    }
  } catch {}
  return false;
}

/** 判定环境：窗口是否在前台 + 本设备是否已开启桌面通知 */
export interface NotifyEnv {
  foreground: boolean;
  enabled: boolean;
}

function windowForeground(): boolean {
  try {
    return !document.hidden && document.hasFocus();
  } catch {
    return true; // 读不到就当前台处理，宁可不打扰
  }
}

/**
 * 系统通知判定：本设备已开启 + 窗口不在前台 + 非本人消息；
 * 静音会话只放行 @本人。与 shouldNotify 的差异：窗口已在后台时，
 * 「当前打开的会话」也需要系统通知（用户实际看不到它）。
 */
export function shouldSystemNotify(s: AppState, m: MessageView, env: NotifyEnv): boolean {
  if (!env.enabled || env.foreground) return false;
  if (s.me == null || isOwnMessage(m, s.me.id)) return false;
  const conv = s.conversations.find((c) => c.id === m.conversation_id);
  if (conv && isMuted(conv) && !m.mentions.includes(s.me.id)) return false;
  return true;
}

/** 标题未读数：未静音会话 unread_count 之和 + 静音但 @我 会话的未读数；>99 显示 99+ */
export function unreadTitle(conversations: ConversationView[]): string {
  let n = 0;
  for (const c of conversations) {
    if (isMuted(c) && !c.has_mention) continue;
    n += c.unread_count;
  }
  return n > 0 ? `(${n > 99 ? "99+" : n}) Chat` : "Chat";
}

/** 非文本 kind 的占位摘要（与 ChatView 的 kindLabel 一致） */
function kindLabel(kind: string): string {
  if (kind === "image") return "[图片]";
  if (kind === "file") return "[文件]";
  if (kind === "audio") return "[语音]";
  if (kind === "poll") return "[投票]";
  if (kind === "chat_record") return "[聊天记录]";
  if (kind === "system") return "[系统消息]";
  return "";
}

/** toast / 系统通知共用摘要：附件、聊天记录、系统条（JSON → 中文文案）等按 kind 映射 */
export function messageExcerpt(m: MessageView): string {
  if (m.attachment) {
    if (m.kind === "image") return "[图片]";
    if (m.kind === "audio" || m.attachment.duration_ms != null) return "[语音]";
    return `[文件] ${m.attachment.file_name}`;
  }
  if (m.kind === "chat_record") return recordLabel(m, RECORD_TITLE_MAX);
  if (m.kind === "system") return systemBarText(m.content);
  if (m.kind === "meeting") return "[视频会议]";
  return kindLabel(m.kind) || m.content.slice(0, 80);
}

/** "发送者: 摘要"；系统条文案自带操作者，不再加发送者前缀 */
export function messageLine(m: MessageView): string {
  const excerpt = messageExcerpt(m);
  return m.kind === "system" ? excerpt : `${m.sender_name}: ${excerpt}`;
}

/**
 * 通知正文：预览开 → "发送者: 摘要"（@我 加前缀，摘要规则与 toast 一致）；
 * 预览关 → "你收到一条新消息" / "有人@了你"，不含消息内容。
 */
export function notificationBody(
  m: MessageView,
  o: { preview: boolean; mentioned: boolean },
): string {
  if (!o.preview) return o.mentioned ? "有人@了你" : "你收到一条新消息";
  return `${o.mentioned ? "[有人@我] " : ""}${messageLine(m)}`;
}

/**
 * 弹一条系统通知。Web 端用 tag 保证同会话通知互相替换（不刷屏）；
 * 点击聚焦窗口并回调打开会话（Tauri 桌面端插件拿不到点击回调时只弹出）。
 * Tauri 同时闪烁任务栏图标；Android 上该调用失败静默吞掉。
 */
export async function showSystemNotification(o: {
  title: string;
  body: string;
  convId: string;
  onOpen?: (convId: string) => void;
}): Promise<void> {
  const kind = notifySupport();
  if (kind === "web") {
    try {
      if (Notification.permission !== "granted") return;
      const n = new Notification(o.title, { body: o.body, tag: `chat-${o.convId}` });
      n.onclick = () => {
        try {
          window.focus();
        } catch {}
        o.onOpen?.(o.convId);
      };
    } catch {}
    return;
  }
  if (kind === "tauri") {
    try {
      const { sendNotification } = await import("@tauri-apps/plugin-notification");
      sendNotification({ title: o.title, body: o.body });
    } catch {}
    try {
      const { getCurrentWindow, UserAttentionType } = await import("@tauri-apps/api/window");
      await getCurrentWindow().requestUserAttention(UserAttentionType.Informational);
    } catch {}
  }
}

/** ws 新消息入口：读环境 → 判定 → 组文案 → 弹通知（Tauri 顺带闪任务栏） */
export async function notifyIncoming(
  s: AppState,
  m: MessageView,
  onOpen?: (convId: string) => void,
): Promise<void> {
  if (notifySupport() === "none") return;
  if (!shouldSystemNotify(s, m, { foreground: windowForeground(), enabled: notifyEnabled() }))
    return;
  if ((await notifyPermission()) !== "granted") return;
  const conv = s.conversations.find((c) => c.id === m.conversation_id);
  const mentioned = s.me != null && m.mentions.includes(s.me.id);
  await showSystemNotification({
    title: conv ? convTitle(conv) : "新消息",
    body: notificationBody(m, { preview: notifyPreview(), mentioned }),
    convId: m.conversation_id,
    onOpen,
  });
}

/** 同步标题：浏览器 document.title + Tauri 窗口 setTitle */
export function syncWindowTitle(title: string): void {
  try {
    document.title = title;
  } catch {}
  if (notifySupport() === "tauri") {
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().setTitle(title))
      .catch(() => {});
  }
}
