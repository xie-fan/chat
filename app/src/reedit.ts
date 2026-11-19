// ===== MM2 =====
// 「重新编辑」的本地原文暂存：服务端不保存已撤消息的正文，
// 撤回前一刻由发起端自己把原文留到本机（明文，按设计如此——本机草稿同理）。
// localStorage 键 chat_reedit:<enc(server)>:<userId> → ReeditEntry[]，
// 按服务器 + 账号隔离；15 分钟窗口外读取时顺带清除；
// 登出由 clearReedits 整键删除；移除服务器由 servers.removeServerData 的键前缀统一清掉。

import type { Stor } from "./servers";

/** 「重新编辑」入口可见窗口：撤回后 15 分钟（与编辑窗口 EDIT_WINDOW_MS 一致） */
export const REEDIT_WINDOW_MS = 15 * 60 * 1000;

export interface ReeditAttachment {
  id: string;
  file_name: string;
  mime: string;
  size: number;
  kind: string; // "image" | "file" | ...
}

export interface ReeditEntry {
  conversation_id: string;
  message_id: string;
  text: string;
  /** 仅留档不还原（重新编辑不恢复回复/mention 关系） */
  reply_to?: string | null;
  attachment?: ReeditAttachment;
  /** 撤回响应带回的附件 id（仅上传者可见）；有它才能挂「复用附件」chip */
  recalled_attachment_id?: string | null;
  savedAt: number;
}

const PREFIX = "chat_reedit:";

export function reeditKey(scope: string, userId: string): string {
  return `${PREFIX}${scope}:${userId}`;
}

export function reeditFresh(e: ReeditEntry, now = Date.now()): boolean {
  return now - e.savedAt < REEDIT_WINDOW_MS;
}

function parseList(storage: Stor, key: string): ReeditEntry[] {
  let raw: string | null;
  try {
    raw = storage.getItem(key);
  } catch {
    return [];
  }
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (e): e is ReeditEntry =>
        !!e &&
        typeof e === "object" &&
        typeof (e as ReeditEntry).message_id === "string" &&
        typeof (e as ReeditEntry).conversation_id === "string" &&
        typeof (e as ReeditEntry).text === "string" &&
        typeof (e as ReeditEntry).savedAt === "number",
    );
  } catch {
    return [];
  }
}

function writeList(storage: Stor, key: string, list: ReeditEntry[]) {
  try {
    if (list.length === 0) storage.removeItem(key);
    else storage.setItem(key, JSON.stringify(list));
  } catch {}
}

/** 读取该账号在本服务器的暂存条目；过期条目读取时顺带清除落盘 */
export function readReedits(
  storage: Stor,
  scope: string,
  userId: string,
  now = Date.now(),
): ReeditEntry[] {
  const key = reeditKey(scope, userId);
  const all = parseList(storage, key);
  const fresh = all.filter((e) => reeditFresh(e, now));
  if (fresh.length !== all.length) writeList(storage, key, fresh);
  return fresh;
}

/** 撤回前调用：按 message_id 覆盖保存原文 */
export function saveReedit(storage: Stor, scope: string, userId: string, entry: ReeditEntry) {
  const key = reeditKey(scope, userId);
  const list = parseList(storage, key).filter(
    (e) => e.message_id !== entry.message_id && reeditFresh(e),
  );
  list.push(entry);
  writeList(storage, key, list);
}

/** 撤回响应回来后补记 recalled_attachment_id */
export function patchReedit(
  storage: Stor,
  scope: string,
  userId: string,
  messageId: string,
  patch: Partial<Pick<ReeditEntry, "recalled_attachment_id">>,
) {
  const key = reeditKey(scope, userId);
  const list = parseList(storage, key);
  const i = list.findIndex((e) => e.message_id === messageId);
  if (i < 0) return;
  list[i] = { ...list[i], ...patch };
  writeList(storage, key, list);
}

/** 点了「重新编辑」或撤回失败：立即移除（不留一次没成功的原文） */
export function removeReedit(storage: Stor, scope: string, userId: string, messageId: string) {
  const key = reeditKey(scope, userId);
  writeList(storage, key, parseList(storage, key).filter((e) => e.message_id !== messageId));
}

/** 应用启动时清过期条目 */
export function purgeReedits(storage: Stor, scope: string, userId: string, now = Date.now()) {
  const key = reeditKey(scope, userId);
  writeList(storage, key, parseList(storage, key).filter((e) => reeditFresh(e, now)));
}

/** 登出时整键删除（含未过期条目——暂存是敏感明文，不留给下一次登录） */
export function clearReedits(storage: Stor, scope: string, userId: string) {
  try {
    storage.removeItem(reeditKey(scope, userId));
  } catch {}
}
