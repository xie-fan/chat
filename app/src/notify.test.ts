import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  notificationBody,
  notifyEnabled,
  notifyPreview,
  setNotifyEnabled,
  setNotifyPreview,
  shouldSystemNotify,
  unreadTitle,
} from "./notify";
import type { AppState } from "./store";
import type { ConversationView, MessageView } from "./types";

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
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => storage.set(k, v),
    removeItem: (k: string) => storage.delete(k),
  });
});

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
