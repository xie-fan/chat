// ===== T1 =====
// 会话/消息的本地快照：localStorage 键 chat_cache:<enc(server)>:<userId>
// （enc = encodeURIComponent(规范化地址)，见 api.serverScope()）。
// 服务端暂时连不上时回放（会话列表 + 最近打开会话的最后 50 条消息），登出/换号清除。
// 读写都吞异常（隐私模式/损坏数据按空处理），总量约 1MB，超限丢最旧。

import type { ConversationView, FriendView, MessageView } from "./types";

export interface CacheStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
  /** 枚举用；真实 localStorage 有，内存替身可省略（省略时按用户删键退化为只删精确键） */
  readonly length?: number;
  key?(index: number): string | null;
}

export interface LocalCacheData {
  friends?: FriendView[];
  conversations: ConversationView[];
  /** 最近打开过的会话（新→旧），messages 只为它们保留 */
  recent: string[];
  messages: Record<string, MessageView[]>;
}

const PREFIX = "chat_cache:";
// 键名第二段是 encServer(规范化地址)，不含 : 或 /；第三段才是 userId
const MAX_BYTES = 1024 * 1024; // 总量上限 ~1MB（按 UTF-8 字节数，CJK 也按实际占用）
const MAX_MSG_PER_CONV = 50; // 每个会话只留最近 50 条
const MAX_CACHED_CONVS = 5; // 最多为最近 5 个会话留消息
const MAX_RECENT_TRACKED = 10; // recent 记录长度上限（> 缓存的会话数，留出打开顺序的余量）

export function cacheKey(server: string, userId: string): string {
  return `${PREFIX}${server}:${userId}`;
}

/** 读取某用户在某服务器上的快照；缺失/损坏/形态不对一律返回 null */
export function readLocalCache(storage: CacheStorage, key: string): LocalCacheData | null {
  try {
    const raw = storage.getItem(key);
    if (!raw) return null;
    const v: unknown = JSON.parse(raw);
    if (!v || typeof v !== "object" || Array.isArray(v)) return null;
    const o = v as Partial<LocalCacheData>;
    if (!Array.isArray(o.conversations) || !o.messages || typeof o.messages !== "object") return null;
    // messages 逐条校验：非数组的直接丢掉，不让坏数据进 store
    const messages: Record<string, MessageView[]> = {};
    for (const [id, list] of Object.entries(o.messages)) {
      if (Array.isArray(list)) messages[id] = list as MessageView[];
    }
    return {
      ...(Array.isArray(o.friends) ? { friends: o.friends } : {}),
      conversations: o.conversations,
      recent: Array.isArray(o.recent)
        ? o.recent.filter((x): x is string => typeof x === "string")
        : [],
      messages,
    };
  } catch {
    return null;
  }
}

const encoder = new TextEncoder();

/** 覆写某用户的快照；超限时先丢最旧会话的消息，再丢末尾会话卡片，仍放不下就放弃写入 */
export function writeLocalCache(
  storage: CacheStorage,
  key: string,
  data: LocalCacheData,
): void {
  try {
    let recent = data.recent.slice(0, MAX_CACHED_CONVS);
    let conversations = data.conversations;
    const pack = () => ({
      friends: data.friends,
      conversations,
      recent,
      messages: Object.fromEntries(
        recent
          .map((id) => [id, (data.messages[id] ?? []).slice(-MAX_MSG_PER_CONV)] as const)
          .filter(([, list]) => list.length > 0),
      ),
    });
    const size = (j: string) => encoder.encode(j).length;
    let json = JSON.stringify(pack());
    while (size(json) > MAX_BYTES && recent.length > 0) {
      recent = recent.slice(0, -1);
      json = JSON.stringify(pack());
    }
    while (size(json) > MAX_BYTES && conversations.length > 0) {
      conversations = conversations.slice(0, -1);
      json = JSON.stringify(pack());
    }
    if (size(json) <= MAX_BYTES) storage.setItem(key, json);
  } catch {}
}

export function removeLocalCache(storage: CacheStorage, key: string): void {
  try {
    storage.removeItem(key);
  } catch {}
}

// ---------- 最近打开的会话（内存 MRU，随快照落盘） ----------
let recentIds: string[] = [];

export function touchRecent(convId: string) {
  recentIds = [convId, ...recentIds.filter((x) => x !== convId)].slice(0, MAX_RECENT_TRACKED);
}
export function getRecent(): string[] {
  return recentIds;
}
export function seedRecent(ids: string[]) {
  recentIds = ids.filter((x) => typeof x === "string").slice(0, MAX_RECENT_TRACKED);
}
export function clearRecent() {
  recentIds = [];
}
