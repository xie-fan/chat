import { describe, expect, it } from "vitest";
import {
  cacheKey,
  clearRecent,
  getRecent,
  readLocalCache,
  removeLocalCache,
  seedRecent,
  touchRecent,
  writeLocalCache,
} from "./localCache";
import type { ConversationView, MessageView } from "./types";
import { memStorage } from "./testStorage";

const conv = (id: string): ConversationView => ({
  id,
  kind: "group",
  name: `群${id}`,
  owner_id: "u1",
  member_count: 2,
  peer: null,
  last_seq: 5,
  unread_count: 0,
  has_mention: false,
  last_message: null,
  created_at: "2026-01-01T00:00:00Z",
});

const msg = (id: string, seq: number, content = "hello"): MessageView => ({
  id,
  conversation_id: "c",
  seq,
  sender_id: "u1",
  sender_username: "u1",
  sender_name: "U1",
  content,
  kind: "text",
  attachment: null,
  mentions: [],
  reply_to_id: null,
  reply_to_preview: null,
  edited_at: null,
  edit_version: 0,
  deleted_at: null,
  reactions: [],
  created_at: "2026-01-01T00:00:00Z",
});

describe("本地会话缓存（localStorage chat_cache:<enc(server)>:<userId>）", () => {
  it("键名按服务器 + 用户隔离", () => {
    expect(cacheKey("http%3A%2F%2Fa", "u1")).toBe("chat_cache:http%3A%2F%2Fa:u1");
    expect(cacheKey("enc-a", "u1")).not.toBe(cacheKey("enc-b", "u1"));
    expect(cacheKey("enc-a", "u1")).not.toBe(cacheKey("enc-a", "u2"));
  });

  it("persists friends with cached history and tolerates legacy snapshots", () => {
    const st = memStorage();
    const friends = [{ user: { id: "u1", username: "u1", display_name: "U1", role: "user" as const, last_seen_at: null }, since: "2026-01-01" }];
    const snapshot = { conversations: [conv("c")], messages: { c: [msg("m1", 1)] }, recent: ["c"], friends };
    writeLocalCache(st, "k", snapshot);
    expect(readLocalCache(st, "k")).toEqual(snapshot);
    st.setItem("k", JSON.stringify({ ...snapshot, friends: null }));
    expect(readLocalCache(st, "k")?.friends).toBeUndefined();
    expect(readLocalCache(st, "k")?.messages.c).toHaveLength(1);
  });

  it("写入→读回：会话列表与消息往返一致", () => {
    const st = memStorage();
    const key = cacheKey("", "u1");
    const messages = { c: [msg("m1", 1), msg("m2", 2)] };
    writeLocalCache(st, key, { conversations: [conv("c")], messages, recent: ["c"] });
    expect(readLocalCache(st, key)).toEqual({
      conversations: [conv("c")],
      messages,
      recent: ["c"],
    });
  });

  it("每个会话最多保留最近 50 条，且只缓存 recent 里最多 5 个会话的消息", () => {
    const st = memStorage();
    const key = cacheKey("", "u1");
    const many = Array.from({ length: 80 }, (_, i) => msg(`m${i}`, i + 1));
    const messages: Record<string, MessageView[]> = { c: many, d: [msg("x", 1)] };
    const recent = ["c", "d", "e", "f", "g", "h"];
    writeLocalCache(st, key, { conversations: [conv("c")], messages, recent });
    const got = readLocalCache(st, key)!;
    expect(got.messages.c).toHaveLength(50);
    expect(got.messages.c[0].seq).toBe(31); // 丢最旧，留最新
    expect(Object.keys(got.messages).sort()).toEqual(["c", "d"]);
    expect(got.recent).toEqual(["c", "d", "e", "f", "g"]);
  });

  it("总量超过 ~1MB 时先丢最旧会话的消息缓存", () => {
    const st = memStorage();
    const key = cacheKey("", "u1");
    const fat = (id: string) =>
      Array.from({ length: 50 }, (_, i) => msg(`${id}-m${i}`, i + 1, "x".repeat(4000)));
    writeLocalCache(st, key, {
      conversations: [conv("a")],
      messages: { a: fat("a"), b: fat("b"), c: fat("c"), d: fat("d"), e: fat("e") },
      recent: ["a", "b", "c", "d", "e"],
    });
    const raw = st.raw.get(key)!;
    expect(raw.length).toBeLessThanOrEqual(1024 * 1024 + 256);
    const got = readLocalCache(st, key)!;
    // 最新的会话缓存留下，最旧的被逐出
    expect(Object.keys(got.messages)).toContain("a");
    expect(Object.keys(got.messages)).not.toContain("e");
    expect(got.conversations).toEqual([conv("a")]);
  });

  it("损坏 JSON / 形态不对 → 返回 null", () => {
    const key = cacheKey("", "u1");
    for (const bad of ["{", "null", "[1,2]", '"str"', "42", '{"conversations":{}}']) {
      const st = memStorage({ [key]: bad });
      expect(readLocalCache(st, key), `input=${bad}`).toBeNull();
    }
  });

  it("storage 抛错不炸：读返回 null，写/删静默失败", () => {
    const throwing = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {
        throw new Error("denied");
      },
    };
    expect(readLocalCache(throwing, "k")).toBeNull();
    expect(() =>
      writeLocalCache(throwing, "k", { conversations: [], messages: {}, recent: [] }),
    ).not.toThrow();
    expect(() => removeLocalCache(throwing, "k")).not.toThrow();
  });

  it("removeLocalCache 删除键", () => {
    const st = memStorage();
    const key = cacheKey("", "u1");
    writeLocalCache(st, key, { conversations: [conv("c")], messages: {}, recent: [] });
    removeLocalCache(st, key);
    expect(readLocalCache(st, key)).toBeNull();
  });

  it("removeLocalCache 只删精确键，不跨服务器误删（T5：清理按服务器隔离）", () => {
    const st = memStorage();
    const keys = [cacheKey("enc-a", "u1"), cacheKey("enc-b", "u1"), cacheKey("enc-a", "u2")];
    for (const k of keys) writeLocalCache(st, k, { conversations: [conv("c")], messages: {}, recent: [] });
    removeLocalCache(st, keys[0]);
    expect(st.raw.has(keys[0])).toBe(false);
    expect(st.raw.has(keys[1])).toBe(true); // 别的服务器不受影响
    expect(st.raw.has(keys[2])).toBe(true); // 别的用户不受影响
  });

  it("messages 里混有非数组数据时只保留合法条目", () => {
    const key = cacheKey("", "u1");
    const st = memStorage({
      [key]: JSON.stringify({
        conversations: [conv("c")],
        recent: ["c", "bad"],
        messages: { c: [msg("m1", 1)], bad: 42, worse: "x" },
      }),
    });
    const got = readLocalCache(st, key)!;
    expect(Object.keys(got.messages)).toEqual(["c"]);
    expect(got.messages.c).toHaveLength(1);
  });
});

describe("recent 会话顺序", () => {
  it("seed / touch（经 getRecent 观察），最多 10 条", () => {
    clearRecent();
    seedRecent(["a", "b"]);
    expect(getRecent()).toEqual(["a", "b"]);
    touchRecent("b");
    expect(getRecent()).toEqual(["b", "a"]);
    touchRecent("c");
    expect(getRecent()).toEqual(["c", "b", "a"]);
    clearRecent();
    expect(getRecent()).toEqual([]);
    seedRecent(Array.from({ length: 20 }, (_, i) => `c${i}`));
    expect(getRecent()).toHaveLength(10);
    clearRecent();
  });
});
