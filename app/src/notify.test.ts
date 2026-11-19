import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getServer, session } from "./api";
import { accountIdOf } from "./servers";
import {
  listenForNotificationClicks,
  messageLine,
  notificationBody,
  notifyEnabled,
  notifyIncoming,
  notifyPermission,
  notificationReady,
  notifyPreview,
  setNotifyEnabled,
  setNotifyPreview,
  showSystemNotification,
  shouldSystemNotify,
  unreadTitle,
  unreadLabel,
} from "./notify";
import type { AppState } from "./store";
import type { ConversationView, MessageView } from "./types";

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    constructor(public onmessage: (msg: string) => void) {}
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

// ===== V2 P1-1 系统通知 =====

const me = {
  id: "me",
  username: "me",
  display_name: "Me",
  role: "user" as const,
  last_seen_at: null,
};

function msg(patch: Partial<MessageView> = {}): MessageView {
  return {
    id: "m1",
    conversation_id: "c",
    seq: 1,
    sender_id: "other",
    sender_username: "other",
    sender_name: "Other",
    content: "hello",
    kind: "text",
    attachment: null,
    mentions: [],
    created_at: "2026-01-01T00:00:00Z",
    ...patch,
  };
}

const MUTED = {
  pinned_at: null,
  archived_at: null,
  muted_until: null,
  mute_forever: true,
  updated_at: null,
};

function conv(patch: Partial<ConversationView> = {}): ConversationView {
  return {
    id: "c",
    kind: "group",
    name: "C",
    owner_id: "me",
    member_count: 2,
    peer: null,
    last_seq: 0,
    unread_count: 0,
    has_mention: false,
    last_message: null,
    created_at: "2026-01-01T00:00:00Z",
    ...patch,
  };
}

function state(patch: Partial<AppState> = {}): AppState {
  return {
    me,
    drafts: {},
    conversations: [conv()],
    messages: {},
    hasMore: {},
    historyLoaded: {},
    activeConv: null,
    online: {},
    typing: {},
    friends: [],
    requests: { incoming: [], outgoing: [] },
    wsReady: false,
    tab: "chats",
    replyingTo: null,
    editingMessage: null,
    reactionPending: {},
    toasts: [],
    avatars: {},
    pendingJump: null,
    acpSettings: { default_permission: "auto_read" },
    acpHosts: [],
    acpAgents: [],
    acpWorkspaces: [],
    acpSessions: [],
    acpLoaded: false,
    activeAcpHostId: null,
    activeAcpWorkspaceId: null,
    activeAcpSessionId: null,
    ...patch,
  };
}

/** 后台 + 本设备已开启 */
const BG = { foreground: false, enabled: true };

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => storage.set(k, v),
    removeItem: (k: string) => storage.delete(k),
  });
});

afterEach(() => vi.unstubAllGlobals());

describe("shouldSystemNotify", () => {
  it("窗口在前台不弹", () => {
    expect(shouldSystemNotify(state(), msg(), { foreground: true, enabled: true })).toBe(false);
  });

  it("本人消息不弹", () => {
    expect(shouldSystemNotify(state(), msg({ sender_id: "me" }), BG)).toBe(false);
  });

  it("静音会话普通消息不弹、@我弹", () => {
    const s = state({ conversations: [conv({ preference: MUTED })] });
    expect(shouldSystemNotify(s, msg(), BG)).toBe(false);
    expect(shouldSystemNotify(s, msg({ mentions: ["me"] }), BG)).toBe(true);
  });

  it("本设备未开启通知不弹", () => {
    expect(shouldSystemNotify(state(), msg(), { foreground: false, enabled: false })).toBe(false);
  });

  it("当前打开的会话：前台不弹、窗口在后台时弹", () => {
    const s = state({ activeConv: "c" });
    expect(shouldSystemNotify(s, msg(), { foreground: true, enabled: true })).toBe(false);
    expect(shouldSystemNotify(s, msg(), BG)).toBe(true);
  });

  it("后台 + 非当前会话 + 已开启 → 弹", () => {
    const s = state({ activeConv: "other" });
    expect(shouldSystemNotify(s, msg(), BG)).toBe(true);
  });
});

describe("unreadTitle", () => {
  it("无未读 → Chat", () => {
    expect(unreadTitle([])).toBe("Chat");
    expect(unreadTitle([conv()])).toBe("Chat");
  });

  it("未静音会话未读计入总数", () => {
    expect(unreadTitle([conv({ unread_count: 3 }), conv({ id: "d", unread_count: 2 })])).toBe(
      "(5) Chat",
    );
  });

  it("静音会话不计数、静音但 @我 计数", () => {
    const muted = conv({ unread_count: 5, preference: MUTED });
    expect(unreadTitle([muted])).toBe("Chat");
    expect(unreadTitle([{ ...muted, has_mention: true, unread_count: 2 }])).toBe("(2) Chat");
  });

  it("总数 > 99 显示 99+", () => {
    expect(unreadTitle([conv({ unread_count: 150 })])).toBe("(99+) Chat");
  });

  it("Dock badge uses the same count and clears when read", () => {
    expect(unreadLabel([conv({ unread_count: 150 })])).toBe("99+");
    expect(unreadLabel([conv({ unread_count: 0 })])).toBeNull();
  });
});

describe("notificationBody", () => {
  it("预览开：正文为 发送者: 摘要", () => {
    expect(notificationBody(msg({ content: "hi" }), { preview: true, mentioned: false })).toBe(
      "Other: hi",
    );
  });

  it("图片 / 文件 / 投票摘要", () => {
    const img = msg({
      kind: "image",
      attachment: { id: "a", file_name: "x.png", mime: "image/png", size: 1 },
    });
    expect(notificationBody(img, { preview: true, mentioned: false })).toBe("Other: [图片]");
    const file = msg({
      kind: "file",
      attachment: { id: "a", file_name: "x.zip", mime: "application/zip", size: 1 },
    });
    expect(notificationBody(file, { preview: true, mentioned: false })).toBe(
      "Other: [文件] x.zip",
    );
    expect(notificationBody(msg({ kind: "poll" }), { preview: true, mentioned: false })).toBe(
      "Other: [投票]",
    );
    const voice = msg({
      kind: "audio",
      attachment: { id: "a", file_name: "v.webm", mime: "audio/webm", size: 1, duration_ms: 3000 },
    });
    expect(notificationBody(voice, { preview: true, mentioned: false })).toBe("Other: [语音]");
  });

  it("@我 时正文加 [有人@我] 前缀", () => {
    expect(
      notificationBody(msg({ mentions: ["me"] }), { preview: true, mentioned: true }),
    ).toBe("[有人@我] Other: hello");
  });

  it("预览关闭时正文不含消息内容", () => {
    const plain = notificationBody(msg({ content: "secret-content" }), {
      preview: false,
      mentioned: false,
    });
    expect(plain).toBe("你收到一条新消息");
    expect(plain).not.toContain("secret-content");
    expect(plain).not.toContain("Other");
    expect(notificationBody(msg(), { preview: false, mentioned: true })).toBe("有人@了你");
  });
});

describe("本机通知设置", () => {
  it("默认：桌面通知关、显示消息内容开", () => {
    expect(notifyEnabled()).toBe(false);
    expect(notifyPreview()).toBe(true);
  });

  it("开关读写 roundtrip", () => {
    setNotifyEnabled(true);
    setNotifyPreview(false);
    expect(notifyEnabled()).toBe(true);
    expect(notifyPreview()).toBe(false);
    setNotifyEnabled(false);
    setNotifyPreview(true);
    expect(notifyEnabled()).toBe(false);
    expect(notifyPreview()).toBe(true);
  });

  it("defaults on in the native app and remembers an explicit opt-out", () => {
    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
    expect(notifyEnabled()).toBe(true);
    setNotifyEnabled(false);
    expect(localStorage.getItem("chat_notify_enabled")).toBe("0");
    expect(notifyEnabled()).toBe(false);
  });

  it("localStorage 异常时读写不抛、回落默认值", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {},
    });
    expect(() => setNotifyEnabled(true)).not.toThrow();
    expect(() => setNotifyPreview(false)).not.toThrow();
    expect(notifyEnabled()).toBe(false);
    expect(notifyPreview()).toBe(true);
  });
});
describe("messageLine", () => {
  it("renders group system JSON as copy without sender prefix", () => {
    const content = JSON.stringify({ code: "group_invited", actor_name: "A", invitee_names: ["B"] });
    expect(messageLine(msg({ kind: "system", content }))).toBe("A 邀请 B 加入了群聊");
    expect(
      notificationBody(msg({ kind: "system", content: JSON.stringify({ code: "group_created", actor_name: "A" }) }), {
        preview: true,
        mentioned: false,
      }),
    ).toBe("A 创建了群聊");
  });
});

describe("native notifications", () => {
  beforeEach(() => {
    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
  });

  it("releases pending Windows activation after the notification listener is ready", async () => {
    vi.stubGlobal("navigator", { userAgent: "Windows NT 10.0" });
    invoke.mockResolvedValueOnce(undefined);
    await notificationReady();
    expect(invoke).toHaveBeenCalledExactlyOnceWith("notification_ready", { session: session() });
  });

  it("does not change activation startup on other platforms or the web", async () => {
    vi.stubGlobal("navigator", { userAgent: "Macintosh" });
    await notificationReady();
    vi.stubGlobal("window", {});
    vi.stubGlobal("navigator", { userAgent: "Windows NT 10.0" });
    await notificationReady();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("queries current OS permission rather than the WebView's cached permission", async () => {
    invoke.mockResolvedValueOnce("denied").mockResolvedValueOnce("granted");
    await expect(notifyPermission()).resolves.toBe("denied");
    await expect(notifyPermission()).resolves.toBe("granted");
    expect(invoke).toHaveBeenCalledWith("notification_permission", undefined);
  });

  it("asks Android to use the activity's foreground state, not document focus", async () => {
    vi.stubGlobal("navigator", { userAgent: "Android" });
    vi.stubGlobal("document", { hidden: false, hasFocus: () => true });
    invoke.mockResolvedValue(true);
    await notifyIncoming(state(), msg());
    expect(invoke).toHaveBeenCalledWith("show_chat_notification", {
      title: "C",
      body: "Other: hello",
      target: { convId: "c", session: session() },
      androidAccount: accountIdOf(getServer(), me.id),
    });
  });

  it("routes Android notification targets for the current account until disposed", async () => {
    vi.stubGlobal("navigator", { userAgent: "Android" });
    const open = vi.fn();
    invoke.mockImplementationOnce((_command, { channel }) => {
      channel.onmessage("cold");
    });
    const stop = await listenForNotificationClicks(open, me.id);
    const [command, { account, channel }] = invoke.mock.calls[0];
    expect(command).toBe("listen_for_android_notification_open");
    expect(account).toBe(accountIdOf(getServer(), me.id));
    channel.onmessage("warm");
    stop?.();
    channel.onmessage("after-disposal");
    expect(open.mock.calls).toEqual([["cold"], ["warm"]]);
  });

  it("test notifications carry no conversation target and report send failures", async () => {
    invoke.mockRejectedValueOnce(new Error("notification unavailable"));
    await expect(showSystemNotification({ title: "Chat", body: "Test" })).resolves.toBe(false);
    expect(invoke).toHaveBeenCalledWith("show_chat_notification", {
      title: "Chat",
      body: "Test",
      target: null,
    });
  });

  it("opens the clicked conversation only in the original login session", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const open = vi.fn();
    const stop = vi.fn();
    listen.mockResolvedValue(stop);
    await expect(listenForNotificationClicks(open)).resolves.toBe(stop);
    const [event, clicked] = listen.mock.calls[0];
    expect(event).toBe("chat-notification-open");
    clicked({ payload: { convId: "old", session: session() + 1 } });
    clicked({ payload: { convId: "c", session: session() } });
    expect(open).toHaveBeenCalledExactlyOnceWith("c");
  });
});
