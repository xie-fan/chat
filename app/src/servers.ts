// ===== T5：多服务器本地存储 =====
// 一个浏览器里可以保存多个服务器地址，各自的登录凭据、会话快照、待发消息、草稿
// 完全隔离。键名一律用 encodeURIComponent(规范化地址) 作一段，保证「服务器」
// 部分不含 : 或 /，键名分段永不串位：
//   chat_servers                  → ServerEntry[]（最近使用在前）
//   chat_auth:<enc>               → StoredAuth（该服务器的登录凭据）
//   chat_cache:<enc>:<uid>        → 会话/消息快照（localCache.ts）
//   chat_outbox:<enc>:<uid>       → 未发出的消息（ws.ts）
//   chat_drafts:<enc>:<uid>       → 输入草稿（drafts.ts；MM2 起含复用附件 chip）
//   chat_reedit:<enc>:<uid>       → 撤回后「重新编辑」的本地原文（reedit.ts）
//   sessionStorage chat_cur_server        → 本标签页当前服务器（规范化地址）
//   sessionStorage chat_kick_reason:<enc> → 该服务器的被踢提示（一次性）

import type { UserPublic } from "./types";

/** localStorage 最小接口；真实 Storage 与内存替身都满足 */
export interface Stor {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
  readonly length?: number;
  key?(index: number): string | null;
}

// ---------- 地址规范化 ----------
// 无协议补 http://；主机小写；去默认端口（:80 对 http、:443 对 https）；
// 去尾斜杠、保留路径；http 与 https 是不同条目；localhost 与 127.0.0.1 不合并。
const SCHEME_RE = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//;

export function normalizeServerUrl(input: string | null | undefined): string | null {
  let s = (input ?? "").trim();
  if (!s) return null;
  if (!SCHEME_RE.test(s)) s = `http://${s}`;
  let u: URL;
  try {
    u = new URL(s);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  // URL 解析已小写主机名、去默认端口；这里只收尾斜杠（路径保留）
  const path = u.pathname.replace(/\/+$/, "");
  return `${u.protocol}//${u.host}${path}`;
}

// ---------- 登录提交时的地址解析 ----------
// 地址输入框是「未提交的草稿」：登录/注册提交、地址框内回车一律用当前输入解析，
// 不依赖 blur 先落盘——否则改完直接回车/点登录会连到旧地址（T6）。
export type ServerInputResult = { ok: true; url: string } | { ok: false; err: string };

/**
 * 解析地址框当前输入：非空规范化，非法给文案；空 = Web 同源（origin）、桌面端必填。
 */
export function resolveServerInput(
  input: string,
  opts: { desktop: boolean; origin?: string },
): ServerInputResult {
  const s = input.trim();
  if (s) {
    const n = normalizeServerUrl(s);
    return n
      ? { ok: true, url: n }
      : { ok: false, err: "服务器地址格式不对，如 http://192.168.1.5:28080" };
  }
  const n = opts.desktop ? null : normalizeServerUrl(opts.origin ?? "");
  return n
    ? { ok: true, url: n }
    : { ok: false, err: "请填写服务器地址，如 http://192.168.1.5:28080" };
}

/** https 页面连 http 服务器会被浏览器拦，先确认再放行；桌面端无此限制 */
export function needsInsecureConfirm(
  desktop: boolean,
  pageProtocol: string,
  serverUrl: string,
): boolean {
  return !desktop && pageProtocol === "https:" && serverUrl.startsWith("http://");
}

// ---------- T6b：地址框交互的纯逻辑（UI 只负责执行） ----------
// 这些决策从 AuthScreen 抽出来：回车/失焦时「收不收起、保不保存、提不提交、
// 焦点去哪」全部在这里定死，组件按结果动作，便于单测覆盖各浏览器差异。

/** IME（中/日/韩输入法）合成期间的 Enter 是选词键，不算提交；keyCode 229 是旧内核兜底 */
export function isImeEnter(e: { isComposing?: boolean; keyCode?: number }): boolean {
  return !!e.isComposing || e.keyCode === 229;
}

/** 第一个空凭据字段（用户名优先）；都填了 → null */
export function firstEmptyCredential(
  username: string,
  password: string,
): "username" | "password" | null {
  if (!username.trim()) return "username";
  if (!password) return "password";
  return null;
}

/** 地址框失焦去向：收起 / 提交地址 / 报错保持展开（错误必须看得见，不能连面板一起藏） */
export type ServerBlurResult =
  | { t: "collapse" }
  | { t: "commit"; url: string }
  | { t: "err"; err: string };

export function resolveServerBlur(
  input: string,
  opts: { desktop: boolean; origin?: string },
): ServerBlurResult {
  // Web 端留空 = 不改动（同源兜底），直接收起；桌面端地址必填，留空按错误处理
  if (!input.trim() && !opts.desktop) return { t: "collapse" };
  const r = resolveServerInput(input, opts);
  return r.ok ? { t: "commit", url: r.url } : { t: "err", err: r.err };
}

/**
 * 地址框回车的动作决策：
 *   ignore —— IME 合成中 / 登录请求在飞：什么都不做，地址也不动
 *   err    —— 地址非法：提示，面板保持展开
 *   focus  —— 凭据没填齐：只提交地址（不提交表单），聚焦第一个空凭据字段
 *   submit —— 提交表单（https→http 确认由 submit 处理，取消就不保存地址）
 */
export type ServerEnterPlan =
  | { t: "ignore" }
  | { t: "err"; err: string }
  | { t: "focus"; url: string; field: "username" | "password" }
  | { t: "submit"; url: string };

export function planServerEnter(a: {
  composing: boolean;
  busy: boolean;
  input: string;
  username: string;
  password: string;
  desktop: boolean;
  origin?: string;
}): ServerEnterPlan {
  if (a.composing || a.busy) return { t: "ignore" };
  const r = resolveServerInput(a.input, { desktop: a.desktop, origin: a.origin });
  if (!r.ok) return { t: "err", err: r.err };
  const field = firstEmptyCredential(a.username, a.password);
  if (field) return { t: "focus", url: r.url, field };
  return { t: "submit", url: r.url };
}

/** 触发表单提交；Safari/WKWebView <16 没有 requestSubmit，派发可取消冒泡的 submit 事件兜底 */
export function requestFormSubmit(form: HTMLFormElement | null): void {
  if (!form) return;
  if (typeof form.requestSubmit === "function") {
    form.requestSubmit();
  } else {
    form.dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  }
}

/** 键名里的服务器段：编码后不含 : 或 /，与 :uid 分段永不冲突 */
export function encServer(normalized: string): string {
  return encodeURIComponent(normalized);
}

// ---------- 服务器列表 ----------
export interface ServerEntry {
  /** 规范化地址，唯一键 */
  url: string;
  /** 显示名（可选；Web 同源条目由 UI 显示「本站」） */
  name?: string;
  /** 最近登录成功的用户名 */
  username?: string;
  /** 最近使用时间戳；列表按它倒序 */
  lastUsedAt: number;
}

const LS_SERVERS = "chat_servers";

/** 读服务器列表（最近使用在前）；损坏数据按空处理 */
export function listServers(storage: Stor): ServerEntry[] {
  try {
    const raw = storage.getItem(LS_SERVERS);
    if (!raw) return [];
    const v: unknown = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    const out: ServerEntry[] = [];
    for (const e of v) {
      if (e && typeof e === "object" && typeof (e as ServerEntry).url === "string") {
        out.push({
          url: (e as ServerEntry).url,
          name: typeof (e as ServerEntry).name === "string" ? (e as ServerEntry).name : undefined,
          username:
            typeof (e as ServerEntry).username === "string"
              ? (e as ServerEntry).username
              : undefined,
          lastUsedAt:
            typeof (e as ServerEntry).lastUsedAt === "number"
              ? (e as ServerEntry).lastUsedAt
              : 0,
        });
      }
    }
    out.sort((a, b) => b.lastUsedAt - a.lastUsedAt);
    return out;
  } catch {
    return [];
  }
}

function saveServers(storage: Stor, list: ServerEntry[]) {
  try {
    storage.setItem(LS_SERVERS, JSON.stringify(list));
  } catch {}
}

/** 新增/更新条目并顶到最近使用最前；patch 里未给的字段保留原值 */
export function upsertServer(
  storage: Stor,
  url: string,
  patch: Partial<Omit<ServerEntry, "url">>,
): ServerEntry[] {
  const list = listServers(storage);
  const i = list.findIndex((s) => s.url === url);
  const entry: ServerEntry = {
    ...(i >= 0 ? list[i] : { url }),
    ...patch,
    url,
    lastUsedAt: patch.lastUsedAt ?? Date.now(),
  };
  if (i >= 0) list.splice(i, 1);
  list.unshift(entry);
  saveServers(storage, list);
  return list;
}

export function removeServerEntry(storage: Stor, url: string) {
  saveServers(storage, listServers(storage).filter((s) => s.url !== url));
}

// ---------- 凭据 chat_auth:<enc> ----------
export interface StoredAuth {
  access: string;
  refresh: string;
  user: UserPublic;
}

export function authKey(scope: string): string {
  return `chat_auth:${scope}`;
}

export function kickReasonKey(scope: string): string {
  return `chat_kick_reason:${scope}`;
}

/** 读某服务器的登录凭据；缺失/损坏/形态不对一律 null */
export function readAuth(storage: Stor, scope: string): StoredAuth | null {
  try {
    const raw = storage.getItem(authKey(scope));
    if (!raw) return null;
    const v: unknown = JSON.parse(raw);
    const o = v as Partial<StoredAuth> | null;
    if (!o || typeof o !== "object") return null;
    if (typeof o.refresh !== "string" || !o.refresh) return null;
    if (!o.user || typeof o.user !== "object" || typeof o.user.id !== "string") return null;
    return {
      access: typeof o.access === "string" ? o.access : "",
      refresh: o.refresh,
      user: o.user,
    };
  } catch {
    return null;
  }
}

export function writeAuth(storage: Stor, scope: string, auth: StoredAuth) {
  try {
    storage.setItem(authKey(scope), JSON.stringify(auth));
  } catch {}
}

export function clearAuth(storage: Stor, scope: string) {
  try {
    storage.removeItem(authKey(scope));
  } catch {}
}

// ---------- 多账号切换表 chat_accounts_v1 ----------
export interface SavedAccount {
  id: string;
  serverUrl: string;
  serverName?: string;
  access: string;
  refresh: string;
  user: UserPublic;
  updatedAt: number;
}

const LS_ACCOUNTS = "chat_accounts_v1";

export function accountIdOf(serverUrl: string, userId: string): string {
  const norm = serverUrl ? (normalizeServerUrl(serverUrl) ?? serverUrl) : "";
  return `${encServer(norm)}:${userId}`;
}

export function listSavedAccounts(storage: Stor): SavedAccount[] {
  try {
    const raw = storage.getItem(LS_ACCOUNTS);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: SavedAccount[] = [];
    for (const item of parsed) {
      const a = item as Partial<SavedAccount> | null;
      if (
        !a ||
        typeof a !== "object" ||
        typeof a.id !== "string" ||
        typeof a.serverUrl !== "string" ||
        typeof a.refresh !== "string" ||
        !a.refresh ||
        !a.user ||
        typeof a.user !== "object" ||
        typeof a.user.id !== "string"
      ) {
        continue;
      }
      out.push({
        id: a.id,
        serverUrl: a.serverUrl,
        serverName: typeof a.serverName === "string" ? a.serverName : undefined,
        access: typeof a.access === "string" ? a.access : "",
        refresh: a.refresh,
        user: a.user,
        updatedAt: typeof a.updatedAt === "number" ? a.updatedAt : 0,
      });
    }
    out.sort((a, b) => b.updatedAt - a.updatedAt);
    return out;
  } catch {
    return [];
  }
}

function saveAccounts(storage: Stor, list: SavedAccount[]) {
  try {
    storage.setItem(LS_ACCOUNTS, JSON.stringify(list));
  } catch {}
}

export function upsertSavedAccount(
  storage: Stor,
  entry: Omit<SavedAccount, "id" | "updatedAt"> & { updatedAt?: number },
): SavedAccount[] {
  const norm = entry.serverUrl ? (normalizeServerUrl(entry.serverUrl) ?? entry.serverUrl) : "";
  const id = accountIdOf(norm, entry.user.id);
  const list = listSavedAccounts(storage).filter((x) => x.id !== id);
  const next: SavedAccount = {
    id,
    serverUrl: norm,
    serverName: entry.serverName,
    access: entry.access,
    refresh: entry.refresh,
    user: entry.user,
    updatedAt: entry.updatedAt ?? Date.now(),
  };
  list.unshift(next);
  saveAccounts(storage, list);
  return list;
}

export function removeSavedAccount(storage: Stor, id: string): SavedAccount[] {
  const next = listSavedAccounts(storage).filter((x) => x.id !== id);
  saveAccounts(storage, next);
  return next;
}

export function updateSavedAccountTokens(
  storage: Stor,
  id: string,
  patch: { access: string; refresh: string; user?: UserPublic },
): void {
  const list = listSavedAccounts(storage);
  let changed = false;
  const next = list.map((a) => {
    if (a.id !== id) return a;
    changed = true;
    return {
      ...a,
      access: patch.access,
      refresh: patch.refresh,
      user: patch.user ?? a.user,
    };
  });
  if (changed) saveAccounts(storage, next);
}

// ---------- 删除服务器 / 删除单账号：按 scope (+ userId) 精确清理 ----------
// 只认 `<prefix>:<enc>:`（带尾冒号）开头的键：enc 本身无冒号，
// 别的服务器的 enc 即使以它为前缀也接不上冒号，不会误匹配。
const DATA_PREFIXES = ["chat_cache:", "chat_outbox:", "chat_drafts:", "chat_reedit:"];

export function removeUserServerData(storage: Stor, scope: string, userId: string): void {
  try {
    storage.removeItem(`chat_cache:${scope}:${userId}`);
    storage.removeItem(`chat_outbox:${scope}:${userId}`);
    storage.removeItem(`chat_reedit:${scope}:${userId}`);
    const dKey = `chat_drafts:${scope}:${userId}`;
    const raw = storage.getItem(dKey);
    if (raw) {
      try {
        const parsed: unknown = JSON.parse(raw);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          const cleaned: Record<string, string> = {};
          for (const [cid, val] of Object.entries(parsed)) {
            if (typeof val === "string" && val) {
              cleaned[cid] = val;
            } else if (
              val &&
              typeof val === "object" &&
              !Array.isArray(val) &&
              typeof (val as { text?: unknown }).text === "string" &&
              (val as { text: string }).text
            ) {
              cleaned[cid] = (val as { text: string }).text;
            }
          }
          if (Object.keys(cleaned).length === 0) storage.removeItem(dKey);
          else storage.setItem(dKey, JSON.stringify(cleaned));
        } else {
          storage.removeItem(dKey);
        }
      } catch {
        storage.removeItem(dKey);
      }
    }
  } catch {}
}

export function removeServerData(storage: Stor, scope: string) {
  try {
    storage.removeItem(authKey(scope));
    const doomed = [authKey(scope)];
    const prefixes = DATA_PREFIXES.map((p) => `${p}${scope}:`);
    for (let i = 0; i < (storage.length ?? 0); i++) {
      const k = storage.key?.(i);
      if (k && prefixes.some((p) => k.startsWith(p))) doomed.push(k);
    }
    doomed.forEach((k) => storage.removeItem(k));
  } catch {}
}

// ---------- 旧版键一次性迁移 ----------
// 旧版（多服务器支持之前）布局：
//   chat_access / chat_refresh / chat_user   全局凭据，属于当时的 chat_server_url
//   chat_server_url                          已配置的服务器地址（未规范化）
//   chat_cache:<raw>:<uid> / chat_outbox:<raw>:<uid>   服务器段是未编码原始地址
//   chat_drafts:<uid>                        无服务器段，属于当时的服务器
// 迁移：凭据与无服务器段的草稿归到「当前服务器」（调用方已解析好，同源部署时
// 必须等于 location.origin）；cache/outbox 按各自内嵌的原始服务器地址规范化
// 重编码——它们的 convId/userId 只属于原服务器，搬到别的服务器会串号，未发出的
// outbox 更是会发错地方。迁移后旧键一律删除；重跑幂等。
const LEGACY_KEYS = ["chat_access", "chat_refresh", "chat_user", "chat_server_url"];

function enumKeys(storage: Stor): string[] {
  const keys: string[] = [];
  for (let i = 0; i < (storage.length ?? 0); i++) {
    const k = storage.key?.(i);
    if (k) keys.push(k);
  }
  return keys;
}

function moveKey(storage: Stor, oldKey: string, newKey: string) {
  try {
    if (newKey !== oldKey) {
      const v = storage.getItem(oldKey);
      // 新键已有值时以新键为准，不覆盖
      if (v != null && storage.getItem(newKey) == null) storage.setItem(newKey, v);
    }
  } catch {
  } finally {
    try {
      storage.removeItem(oldKey);
    } catch {}
  }
}

export function migrateLegacyStorage(storage: Stor, current: string): void {
  try {
    const keys = enumKeys(storage);
    const discovered = new Set<string>([current]);

    // 1) 全局凭据 → chat_auth:<enc(current)>（只迁齐 refresh+user 的，残缺直接丢）
    const refresh = storage.getItem("chat_refresh");
    const userRaw = storage.getItem("chat_user");
    if (refresh && userRaw) {
      try {
        const u = JSON.parse(userRaw) as UserPublic;
        if (u && typeof u.id === "string") {
          const scope = encServer(current);
          if (readAuth(storage, scope) == null) {
            writeAuth(storage, scope, {
              access: storage.getItem("chat_access") ?? "",
              refresh,
              user: u,
            });
          }
        }
      } catch {}
    }

    // 2) cache / outbox：按内嵌的原始服务器重编码归位
    for (const key of keys) {
      for (const prefix of ["chat_cache:", "chat_outbox:"]) {
        if (!key.startsWith(prefix)) continue;
        const rest = key.slice(prefix.length);
        const cut = rest.lastIndexOf(":");
        if (cut <= 0) break; // 形态不对，不动它
        const srv = rest.slice(0, cut);
        const uid = rest.slice(cut + 1);
        // 服务器段含 / 或 : → 旧版未编码地址；无 → 已是 enc 新键
        if (!/[/:]/.test(srv)) break;
        const norm = normalizeServerUrl(srv);
        if (!norm) {
          storage.removeItem(key); // 地址救不回，快照留着也归不了档
          break;
        }
        discovered.add(norm);
        moveKey(storage, key, `${prefix}${encServer(norm)}:${uid}`);
        break;
      }
      if (key.startsWith("chat_drafts:")) {
        const rest = key.slice("chat_drafts:".length);
        if (!rest.includes(":")) {
          // 无服务器段的旧草稿 → 当前服务器
          moveKey(storage, key, `chat_drafts:${encServer(current)}:${rest}`);
        }
      }
    }

    // 3) 服务器列表：发现的地址补进列表（当前服务器最后 upsert → 排在最前）
    const now = Date.now();
    for (const url of [...discovered].filter((u) => u !== current).concat(current)) {
      if (!listServers(storage).some((s) => s.url === url)) {
        upsertServer(storage, url, { lastUsedAt: now });
      }
    }

    // 4) 删旧键
    LEGACY_KEYS.forEach((k) => {
      try {
        storage.removeItem(k);
      } catch {}
    });
  } catch {}
}
