// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { auth, saveSession, serverScope } from "./api";
import { cacheKey, writeLocalCache } from "./localCache";
import { getState, isReadonlyDm, resetSessionState, setState } from "./store";
import type { ConversationView, MessageView } from "./types";
import App from "./App";

vi.mock("./acpHost", () => ({ startAcpHost: vi.fn(), stopAcpHost: vi.fn() }));
vi.mock("./ws", () => ({ startWs: vi.fn(), stopWs: vi.fn() }));
vi.mock("./notify", () => ({ syncWindowTitle: vi.fn(), unreadTitle: vi.fn() }));
vi.mock("./ui/ChatScreen", () => ({ default: () => <div>chat</div> }));
vi.mock("./ui/AuthScreen", () => ({ default: () => <div>auth</div> }));
vi.mock("./ui/Modal", () => ({ DialogHost: () => null, dismissAllDialogs: vi.fn() }));

const me = { id: "me", username: "me", display_name: "Me", role: "user" as const, last_seen_at: null };
const peer = { ...me, id: "friend" };
const conv: ConversationView = { id: "c", kind: "dm", peer, name: null, owner_id: null, member_count: 2, last_seq: 1, unread_count: 0, has_mention: false, last_message: null, created_at: "2026-01-01" };
const message: MessageView = { id: "m", conversation_id: "c", seq: 1, sender_id: peer.id, sender_name: "Me", sender_username: "me", kind: "text", content: "cached history", attachment: null, mentions: [], created_at: "2026-01-01" };
let root: ReturnType<typeof createRoot>;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  sessionStorage.clear();
  setState(resetSessionState());
  saveSession({ user: me, access_token: "a", refresh_token: "r", access_expires_in: 900 });
  writeLocalCache(localStorage, cacheKey(serverScope(), me.id), {
    conversations: [conv], recent: ["c"], messages: { c: [message] }, friends: [{ user: peer, since: "2026-01-01" }],
  });
  root = createRoot(document.createElement("div"));
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.restoreAllMocks();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("cold-start cache hydration", () => {
  it.each([true, false])("restores history and keeps cached DMs writable when auth fails=%s", async (offline) => {
    vi.spyOn(auth, "me").mockImplementation(async () => {
      if (offline) throw new TypeError("offline");
      return me;
    });
    await act(async () => root.render(<App />));
    await act(async () => { await vi.advanceTimersByTimeAsync(600); });
    expect(getState().messages.c).toEqual([message]);
    expect(getState().friends.map((f) => f.user.id)).toEqual([peer.id]);
    expect(isReadonlyDm(conv, getState())).toBe(false);
    expect(JSON.parse(localStorage.getItem(cacheKey(serverScope(), me.id))!).messages.c).toEqual([message]);
  });
});
