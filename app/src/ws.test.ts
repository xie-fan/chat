import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { clearSession, saveSession } from "./api";
import { getState, isReadonlyDm, setState } from "./store";
import type { ConversationView, MessageView } from "./types";
import { discardSend, kickReasonText, leaveGroup, refreshCatalogs, retrySend, sendChat, startWs, stopWs } from "./ws";

const me = { id: "me", username: "me", display_name: "Me", role: "user" as const, last_seen_at: null };
const message = (seq: number, mentions: string[] = []): MessageView => ({ id: `m${seq}`, seq, conversation_id: "c", sender_id: "other", sender_username: "other", sender_name: "Other", content: "hello", kind: "text", attachment: null, mentions, created_at: "2026-01-01T00:00:00Z" });
const conversation = (): ConversationView => ({ id: "c", kind: "group", name: "C", owner_id: "me", member_count: 2, peer: null, last_seq: 0, unread_count: 0, has_mention: false, last_message: null, created_at: "2026-01-01T00:00:00Z" });
class Socket {
  static CONNECTING = 0;
  static OPEN = 1;
  static all: Socket[] = [];
  readyState = 1;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  sent: Record<string, unknown>[] = [];
  respondPing = true;
  constructor() { Socket.all.push(this); }
  send(data: string) {
    const frame = JSON.parse(data);
    this.sent.push(frame);
    if (frame.type === "ping" && this.respondPing) queueMicrotask(() => this.frame({ type: "pong" }));
  }
  close() { this.readyState = 3; this.onclose?.(); }
  frame(data: unknown) { this.onmessage?.({ data: JSON.stringify(data) }); }
}
let refreshStatus = 200;
beforeEach(() => {
  vi.useFakeTimers();
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (k: string) => storage.get(k) ?? null, setItem: (k: string, v: string) => storage.set(k, v), removeItem: (k: string) => storage.delete(k) });
  const events = new EventTarget();
  vi.stubGlobal("window", { setTimeout, dispatchEvent: events.dispatchEvent.bind(events), addEventListener: events.addEventListener.bind(events), removeEventListener: events.removeEventListener.bind(events) });
  vi.stubGlobal("location", { origin: "http://localhost" });
  vi.stubGlobal("WebSocket", Socket);
  Socket.all = [];
  refreshStatus = 200;
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    if (url.endsWith("/ws/ticket")) return Response.json({ ticket: "t" });
    if (url.endsWith("/users/me")) return Response.json(me);
    if (url.endsWith("/messages/refresh")) return new Response(refreshStatus === 200 ? JSON.stringify({ messages: [], has_more: false }) : "", { status: refreshStatus });
    if (url.endsWith("/conversations/c")) return Response.json({ id: "c" });
    if (url.endsWith("/conversations")) return Response.json(getState().conversations);
    if (url.endsWith("/friends/requests")) return Response.json({ incoming: [], outgoing: [] });
    return Response.json([]);
  }));
  saveSession({ user: me, access_token: "a", refresh_token: "r", access_expires_in: 900 });
  setState({ me, conversations: [conversation()], messages: {}, historyLoaded: {}, hasMore: {}, activeConv: null, tab: "chats", toasts: [], typing: {} });
});
afterEach(() => { stopWs(); clearSession(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); });
const open = async () => {
  await startWs();
  const socket = Socket.all[Socket.all.length - 1];
  socket.onopen?.();
  await vi.waitFor(() => expect(getState().wsReady).toBe(true));
  await vi.advanceTimersByTimeAsync(0);
  return socket;
};

describe("WebSocket delivery", () => {
  it("sends each outstanding item once per connection and replays only on reconnect", async () => {
    const first = await open();
    sendChat("c", "one"); sendChat("c", "two"); sendChat("c", "three");
    const sent = first.sent.filter((f) => f.type === "send_message");
    expect(sent).toHaveLength(3);
    first.frame({ type: "message_ack", client_msg_id: sent[0].client_msg_id, message: message(1) });
    expect(getState().messages.c.filter((m) => m.id.startsWith("client:"))).toHaveLength(2);
    first.close();
    await vi.advanceTimersByTimeAsync(1000);
    const second = Socket.all[Socket.all.length - 1];
    second.onopen?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(second.sent.filter((f) => f.type === "send_message").map((f) => f.client_msg_id)).toEqual(sent.slice(1).map((f) => f.client_msg_id));
    sendChat("c", "four");
    expect(second.sent.filter((f) => f.type === "send_message")).toHaveLength(3);
  });

  it("keeps unsent messages across a reload and resends them with the same client_msg_id", async () => {
    (await open()).close(); // 断网
    sendChat("c", "offline"); // 连接不在：只进队列
    const id = getState().messages.c[0].id;
    stopWs(); // 模拟刷新：内存清空，只剩本地存储
    setState({ messages: {} });
    const sock = await open();
    expect(getState().messages.c.map((m) => m.id)).toEqual([id]);
    const sent = sock.sent.filter((f) => f.type === "send_message");
    expect(sent.map((f) => f.client_msg_id)).toEqual([id.slice("client:".length)]);
    sock.frame({ type: "message_ack", client_msg_id: sent[0].client_msg_id, message: message(1) });
    stopWs();
    setState({ messages: {} });
    await open();
    expect(getState().messages.c ?? []).toEqual([]); // ack 后不再恢复
  });

  it("keeps a permanently rejected message as a failed bubble that can be retried or discarded", async () => {
    const sock = await open();
    sendChat("c", "nope");
    const [frame] = sock.sent.filter((f) => f.type === "send_message");
    const cid = frame.client_msg_id as string;
    sock.frame({ type: "error", code: "not_friends", message: "x", client_msg_id: cid });
    expect(getState().messages.c[0].send_error).toBe("对方已不是你的好友");
    retrySend(cid);
    expect(getState().messages.c[0].send_error).toBeUndefined();
    sock.frame({ type: "error", code: "not_friends", message: "x", client_msg_id: cid });
    discardSend(cid);
    expect(getState().messages.c).toEqual([]);
    stopWs();
    await open();
    expect(getState().messages.c).toEqual([]); // 删掉的不会被恢复
  });

  it("reconnects and auto-resends an ack timeout with the same id, converging duplicate acks", async () => {
    const sock = await open();
    sendChat("c", "lost-ack");
    const [frame] = sock.sent.filter((f) => f.type === "send_message");
    const cid = frame.client_msg_id as string;
    // 心跳正常但 ack 丢失，仍需重连重放。
    await vi.advanceTimersByTimeAsync(12_000);
    expect(getState().wsReady).toBe(false);
    expect(getState().messages.c[0].send_error).toBeUndefined();
    await vi.advanceTimersByTimeAsync(1_000);
    const second = Socket.all[Socket.all.length - 1];
    second.onopen?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(second.sent.filter((f) => f.type === "send_message").map((f) => f.client_msg_id)).toEqual([cid]);
    const ack = { type: "message_ack", client_msg_id: cid, message: { ...message(1), sender_id: "me", sender_name: "Me" } };
    second.frame(ack);
    second.frame(ack);
    expect(getState().messages.c.map((m) => m.id)).toEqual(["m1"]);
  });

  it("detects a silent dead socket within seven seconds even without sending a message", async () => {
    const sock = await open();
    sock.respondPing = false;
    await vi.advanceTimersByTimeAsync(4_000);
    expect(sock.sent).toContainEqual({ type: "ping" });
    expect(getState().wsReady).toBe(true);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(getState().wsReady).toBe(false);
    expect(sock.readyState).toBe(3);
  });

  it("responds immediately to browser offline and online events", async () => {
    await open();
    window.dispatchEvent(new Event("offline"));
    expect(getState().wsReady).toBe(false);
    window.dispatchEvent(new Event("online"));
    await vi.advanceTimersByTimeAsync(0);
    const second = Socket.all[Socket.all.length - 1];
    second.onopen?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(getState().wsReady).toBe(true);
  });

  it("does not orphan a reconnect socket still connecting when the browser goes online", async () => {
    const first = await open();
    first.close();
    await vi.advanceTimersByTimeAsync(1_000);
    const reconnect = Socket.all[Socket.all.length - 1];
    reconnect.readyState = Socket.CONNECTING;
    const count = Socket.all.length;
    window.dispatchEvent(new Event("online"));
    await vi.advanceTimersByTimeAsync(0);
    expect(Socket.all).toHaveLength(count);
    stopWs();
    expect(Socket.all.every((socket) => socket.readyState === 3)).toBe(true);
  });

  it("closes and detaches any previous socket before replacing it", async () => {
    await startWs();
    const first = Socket.all[Socket.all.length - 1];
    first.readyState = Socket.CONNECTING;
    await startWs();
    expect(first.readyState).toBe(3);
    expect([first.onopen, first.onmessage, first.onclose, first.onerror]).toEqual([null, null, null, null]);
    stopWs();
    expect(Socket.all.every((socket) => socket.readyState === 3)).toBe(true);
  });

  it("does not treat a failed startup friendship fetch as a confirmed removal", async () => {
    const peer = { ...me, id: "friend" };
    const dm = { ...conversation(), kind: "dm", peer };
    setState({ conversations: [dm], friends: [{ user: peer, since: "2026-01-01" }], friendsLoaded: false });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    await expect(refreshCatalogs()).rejects.toThrow("offline");
    expect(isReadonlyDm(dm, getState())).toBe(false);
    expect(getState().friends.map((f) => f.user.id)).toEqual([peer.id]);
    sendChat("c", "queued offline");
    expect(getState().messages.c[0].send_error).toBeUndefined();
  });

  it("resolves the optimistic bubble when the ack is lost but the message echoes via broadcast", async () => {
    const sock = await open();
    sendChat("c", "echo me");
    const [frame] = sock.sent.filter((f) => f.type === "send_message");
    // 消息本体经广播到达、ack 帧丢失：乐观气泡应按 ack 语义收敛，不与真身并存
    sock.frame({ type: "message", message: { ...message(1), sender_id: "me", sender_username: "me", sender_name: "Me", content: "echo me" } });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(getState().messages.c.map((m) => m.id)).toEqual(["m1"]);
    // 已入库的消息不得再重放：重连 flush 不重发，不产生第二条
    sock.close();
    await vi.advanceTimersByTimeAsync(1000);
    const second = Socket.all[Socket.all.length - 1];
    second.onopen?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(second.sent.filter((f) => f.type === "send_message").map((f) => f.client_msg_id)).not.toContain(frame.client_msg_id);
  });

  it("keeps an unsent offline message pending for the reconnect flush instead of failing it", async () => {
    const sock = await open();
    sock.close(); // 断网后才写：从没上过任何 socket
    sendChat("c", "queued");
    await vi.advanceTimersByTimeAsync(30_000);
    expect(getState().messages.c[0].send_error).toBeUndefined();
    const second = Socket.all[Socket.all.length - 1];
    second.onopen?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(second.sent.filter((f) => f.type === "send_message")).toHaveLength(1);
  });

  it("does not toast or count a duplicate message twice", async () => {
    const sock = await open();
    sock.frame({ type: "message", message: message(1) });
    sock.frame({ type: "message", message: message(1) });
    expect(getState().toasts).toHaveLength(1);
    expect(getState().conversations[0].unread_count).toBe(1);
  });

  it("read_sync clears only the covered range and preserves later mentions", async () => {
    const sock = await open();
    sock.frame({ type: "message", message: message(1) });
    sock.frame({ type: "message", message: message(2, ["me"]) });
    sock.frame({ type: "read_sync", conversation_id: "c", seq: 1 });
    expect(getState().conversations[0]).toMatchObject({ unread_count: 1, has_mention: true });
    sock.frame({ type: "read_sync", conversation_id: "c", seq: 2 });
    expect(getState().conversations[0]).toMatchObject({ unread_count: 0, has_mention: false });
  });

  it("preserves history when a legacy server lacks refresh but still grants conversation access", async () => {
    refreshStatus = 404;
    setState({ messages: { c: [message(1)] }, historyLoaded: { c: true } });
    await open();
    await vi.advanceTimersByTimeAsync(0);
    expect(getState().messages.c).toEqual([message(1)]);
    expect(getState().historyLoaded.c).toBe(true);
  });
});


// ===== P0-1：退群后列表更新 + 错误中文化 =====
describe("leaveGroup", () => {
  it("removes the group locally without re-fetching its detail and clears the selection", async () => {
    setState({ conversations: [conversation(), { ...conversation(), id: "d" }], activeConv: "c" });
    const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetcher);
    await expect(leaveGroup("c")).resolves.toBe("left");
    expect(getState().conversations.map((c) => c.id)).toEqual(["d"]);
    expect(getState().activeConv).toBeNull();
    // 退群成功后不得再回拉 detail/list：已非成员只会 403，把整次刷新拖垮
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(String(fetcher.mock.calls[0][0])).toContain("/conversations/c/members/me");
    expect(fetcher.mock.calls[0][1]?.method).toBe("DELETE");
  });

  it("keeps the selection when a different conversation is open", async () => {
    setState({ conversations: [conversation(), { ...conversation(), id: "d" }], activeConv: "d" });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })));
    await expect(leaveGroup("c")).resolves.toBe("left");
    expect(getState().conversations.map((c) => c.id)).toEqual(["d"]);
    expect(getState().activeConv).toBe("d");
  });

  it("treats a not-member 403 as already gone: still removes locally and reports it", async () => {
    setState({ conversations: [conversation()], activeConv: "c" });
    vi.stubGlobal("fetch", vi.fn(async () =>
      Response.json({ error: "not a member of this conversation" }, { status: 403 })));
    await expect(leaveGroup("c")).resolves.toBe("already_left");
    expect(getState().conversations).toEqual([]);
    expect(getState().activeConv).toBeNull();
  });

  it("propagates other failures and keeps the list untouched", async () => {
    setState({ conversations: [conversation()], activeConv: "c" });
    vi.stubGlobal("fetch", vi.fn(async () =>
      Response.json({ error: "boom" }, { status: 500 })));
    await expect(leaveGroup("c")).rejects.toMatchObject({ status: 500 });
    expect(getState().conversations).toHaveLength(1);
    expect(getState().activeConv).toBe("c");
  });
});

// ===== V2 P1-5 =====
describe("force_logout reason copy", () => {
  it.each([
    ["disabled", "账号已被禁用，请联系管理员"],
    ["password_reset", "密码已被管理员重置，请重新登录"],
    ["password_changed", "密码已在其他设备修改，请重新登录"],
    ["session_revoked", "该设备已被移出登录，请重新登录"],
    ["maintenance", "服务器维护中，请稍后重新登录"],
    ["restoring", "正在恢复备份，完成后请重新登录"],
    ["unknown_reason", "登录已失效，请重新登录"],
  ])("kickReasonText(%j) → %s", (reason, want) => {
    expect(kickReasonText(reason)).toBe(want);
  });
});
