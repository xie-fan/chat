import { cacheKey, removeLocalCache } from "./localCache";
import {
  accountIdOf,
  authKey,
  encServer,
  kickReasonKey,
  listSavedAccounts as listSavedAccountsIn,
  listServers as listServersIn,
  migrateLegacyStorage,
  normalizeServerUrl,
  readAuth,
  removeSavedAccount as removeSavedAccountIn,
  removeServerData,
  removeServerEntry,
  removeUserServerData,
  updateSavedAccountTokens as updateSavedAccountTokensIn,
  upsertSavedAccount as upsertSavedAccountIn,
  upsertServer,
  writeAuth,
  type SavedAccount,
  type ServerEntry,
} from "./servers";

export { normalizeServerUrl } from "./servers";
export type { SavedAccount, ServerEntry } from "./servers";
import type {
  AcpAgent,
  AcpAttachment,
  AcpBootstrap,
  AcpDirListing,
  AcpHost,
  AcpHostFileContent,
  AcpPermissionPolicy,
  AcpSession,
  AcpSettings,
  AcpThinkingLevel,
  AcpTurn,
  AcpWorkspace,
  AdminOverview,
  AdminSettings,
  AdminUserView,
  AttachmentView,
  AuthResponse,
  BotView,
  BackupCreated,
  BackupList,
  BackupTicket,
  RestoreResult,
  ChatRecord,
  ConversationDetail,
  ConversationPref,
  ConversationView,
  CreateInviteReq,
  CreatePollReq,
  ForwardBatchResp,
  ForwardResp,
  FriendView,
  InviteView,
  MessagesPage,
  MessageView,
  PrefUpdate,
  PushState,
  RequestsView,
  SessionView,
  AnnouncementView,
  PinEntry,
  ServerInfo,
  StorageView,
  UserPublic,
} from "./types";

// ---------- 服务器地址 ----------
// 「当前服务器」按标签页：sessionStorage chat_cur_server 是本页钉住的服务器；
// localStorage chat_servers 里最近使用的条目供新标签页/重启兜底。
// 桌面端没有同源服务器，未配置时用正式部署地址
const DEFAULT_DESKTOP_SERVER = "https://chat.287152.xyz";
const SS_CUR_SERVER = "chat_cur_server";

export const inTauri = () => "__TAURI_INTERNALS__" in globalThis || "__TAURI__" in globalThis;

/** 兜底服务器：Web 端 = 同源（规范化后恰为 location.origin）；桌面端 = 正式部署地址 */
function defaultServer(): string {
  if (!inTauri() && typeof location !== "undefined") {
    return normalizeServerUrl(location.origin) ?? DEFAULT_DESKTOP_SERVER;
  }
  return DEFAULT_DESKTOP_SERVER;
}

// 启动时把旧版全局凭据/cache/outbox/drafts 一次性迁到多服务器键名下（幂等）。
// 迁移目标的「当前服务器」：旧版配置的 chat_server_url；没有则默认（同源时必须等于 location.origin）
if (typeof localStorage !== "undefined" && typeof sessionStorage !== "undefined") {
  try {
    const legacy = normalizeServerUrl(localStorage.getItem("chat_server_url"));
    migrateLegacyStorage(localStorage, legacy ?? defaultServer());
    sessionStorage.removeItem("chat_kick_reason"); // 旧版无服务器段的踢人提示
  } catch {}
}

let currentServer: string | null = null;

function resolveCurrent(): string {
  if (currentServer) return currentServer;
  try {
    const pinned = normalizeServerUrl(sessionStorage.getItem(SS_CUR_SERVER));
    if (pinned) return (currentServer = pinned);
  } catch {}
  const top =
    typeof localStorage !== "undefined" ? listServersIn(localStorage)[0]?.url : undefined;
  currentServer = top ?? defaultServer();
  try {
    sessionStorage.setItem(SS_CUR_SERVER, currentServer);
  } catch {}
  return currentServer;
}

export function getServer(): string {
  return resolveCurrent();
}
/** 本地键名的服务器段：encodeURIComponent(规范化地址)，不含 : 或 / */
export function serverScope(): string {
  return encServer(getServer());
}
export function setServer(url: string) {
  const next = normalizeServerUrl(url);
  if (!next) return;
  try {
    sessionStorage.setItem(SS_CUR_SERVER, next); // 每次都重写：清了 sessionStorage 也能补回
  } catch {}
  if (next === getServer()) return;
  currentServer = next;
  // 已在列表里的服务器顶到最近使用；没登录过的新地址等登录成功才入列
  try {
    if (listServersIn(localStorage).some((s) => s.url === next)) {
      upsertServer(localStorage, next, {});
    }
  } catch {}
  sessionGen++; // 服务器切换：旧服务器的异步结果一律作废
  boundIdentity = storedIdentity();
}
export function wsBase(): string {
  return getServer().replace(/^http/, "ws");
}

// ---------- 服务器列表 / 多服务器管理（T5） ----------
/** 已保存的服务器（最近使用在前） */
export function listServers(): ServerEntry[] {
  return typeof localStorage === "undefined" ? [] : listServersIn(localStorage);
}
/** Rust 侧下载/安装命令的 origin 白名单：当前服务器 + 服务器列表（T5 多服务器） */
export function allowedOrigins(): string[] {
  return [...new Set([getServer(), ...listServers().map((s) => s.url)])];
}
/** 该服务器是否已有登录凭据（切过去不用重新登录） */
export function hasServerAuth(url: string): boolean {
  const n = normalizeServerUrl(url);
  return (
    !!n && typeof localStorage !== "undefined" && readAuth(localStorage, encServer(n)) != null
  );
}
/** Web 端「本站」：与页面同源的条目；桌面端没有本站 */
export function isSameOriginServer(url: string): boolean {
  if (inTauri() || typeof location === "undefined") return false;
  return normalizeServerUrl(url) === normalizeServerUrl(location.origin);
}
/** 列表显示名：本站 > 自定义名 > 地址本身 */
export function serverLabel(url: string, entry?: ServerEntry): string {
  if (isSameOriginServer(url)) return "本站";
  return entry?.name || url;
}
/** 新地址入列（登录成功也走这里）；返回入列条目，地址非法返回 null */
export function addServerEntry(input: string): ServerEntry | null {
  const n = normalizeServerUrl(input);
  if (!n || typeof localStorage === "undefined") return null;
  upsertServer(localStorage, n, {});
  return listServers().find((s) => s.url === n) ?? null;
}
/** 删除服务器的二次确认（登录页与设置页共用文案；返回 false = 用户取消） */
export function confirmServerRemove(sv: ServerEntry): boolean {
  return window.confirm(
    `删除服务器 ${serverLabel(sv.url, sv)}（${sv.url}）？\n它的登录凭据、本地缓存和未发出的消息都会被清除。`,
  );
}
/**
 * 删除服务器：尽力调它的 /auth/logout（失败忽略）→ 清本地 auth/cache/outbox/drafts → 移出列表。
 * 返回建议切到的服务器（最近使用的另一个；没有则默认）。
 */
export function removeServer(url: string): string {
  const norm = normalizeServerUrl(url) ?? url;
  const scope = encServer(norm);
  try {
    const rts = new Set<string>();
    const curRt = readAuth(localStorage, scope)?.refresh;
    if (curRt) rts.add(curRt);
    for (const acc of listSavedAccountsIn(localStorage)) {
      if ((normalizeServerUrl(acc.serverUrl) ?? acc.serverUrl) === norm) {
        if (acc.refresh) rts.add(acc.refresh);
        removeSavedAccountIn(localStorage, acc.id);
      }
    }
    for (const rt of rts) {
      void timedFetch(`${norm}/v1/auth/logout`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ refresh_token: rt }),
      }).catch(() => {});
    }
    removeServerData(localStorage, scope);
    removeServerEntry(localStorage, norm);
  } catch {}
  try {
    sessionStorage.removeItem(kickReasonKey(scope));
  } catch {}
  return listServers()[0]?.url ?? defaultServer();
}
// 「删除当前服务器」后重载：即使回退服务器有凭据也停在登录页（一次有效）
const SS_SKIP_AUTOLOGIN = "chat_skip_autologin";
export function skipAutoLoginOnce() {
  try {
    sessionStorage.setItem(SS_SKIP_AUTOLOGIN, "1");
  } catch {}
}
export function hasSkipAutoLogin(): boolean {
  try {
    return !!sessionStorage.getItem(SS_SKIP_AUTOLOGIN);
  } catch {
    return false;
  }
}
export function clearSkipAutoLogin() {
  try {
    sessionStorage.removeItem(SS_SKIP_AUTOLOGIN);
  } catch {}
}
/** 登录页用的被踢提示：按当前服务器读一次即清 */
export function takeKickReason(): string {
  try {
    const k = kickReasonKey(serverScope());
    const r = sessionStorage.getItem(k);
    if (r) sessionStorage.removeItem(k);
    return r ?? "";
  } catch {
    return "";
  }
}

// ---------- 会话凭据 ----------
// 会话代次：登录/登出/换服务器都会 +1；异步回调先比对代次再落地，防旧账号结果污染新账号
let sessionGen = 0;
export function session(): number {
  return sessionGen;
}

// localStorage 是所有标签页共享的，但凭据按服务器分键：本页只关心自己这台服务器的
// chat_auth:<enc> 键。别的标签页在同一台服务器上换账号/登出 → 本页身份对不上，
// 一律不发请求，等页面重载；在别的服务器上的操作与本页无关。
// 同一账号的令牌轮换不改身份，照常使用。
function storedUserId(): string {
  return readAuth(localStorage, serverScope())?.user.id ?? "";
}
function storedIdentity(): string {
  return `${getServer()}|${storedUserId()}`;
}
let boundIdentity: string | null = null;
function identityMatches(): boolean {
  boundIdentity ??= storedIdentity();
  return storedIdentity() === boundIdentity;
}
function requireIdentity() {
  if (!identityMatches()) throw new ApiError(0, "session changed");
}
/** 别的标签页改了本页服务器上的账号/凭据时回调（本页应重载）。 */
export function watchIdentity(onChange: () => void) {
  window.addEventListener("storage", (e) => {
    // e.key === null = 别的标签页 storage.clear()：本页凭据多半也没了，重查身份
    if (e.key !== null && e.key !== authKey(serverScope())) return;
    if (!identityMatches()) onChange();
  });
}

export function saveSession(r: AuthResponse, serverName?: string) {
  const srv = getServer();
  const scope = encServer(srv);
  const prev = readAuth(localStorage, scope)?.user.id ?? "";
  // 换号：清掉上一个账号在本服务器键下的快照，防串号（别的服务器不动）
  if (prev && prev !== r.user.id) removeLocalCache(localStorage, cacheKey(scope, prev));
  writeAuth(localStorage, scope, {
    access: r.access_token,
    refresh: r.refresh_token,
    user: r.user,
  });
  try {
    upsertServer(localStorage, srv, {
      username: r.user.username,
      ...(serverName ? { name: serverName } : {}),
    });
    const knownName =
      serverName ?? listServersIn(localStorage).find((s) => s.url === srv)?.name;
    upsertSavedAccountIn(localStorage, {
      serverUrl: srv,
      serverName: knownName,
      access: r.access_token,
      refresh: r.refresh_token,
      user: r.user,
    });
  } catch {}
  sessionGen++; // 新登录 = 新会话
  boundIdentity = storedIdentity();
}
export function loadSession(): { user: UserPublic } | null {
  const a = readAuth(localStorage, serverScope());
  return a ? { user: a.user } : null;
}
/** 仅清某服务器上当前活跃用户的凭据与敏感/用户专属数据，保留同服务器其他已保存账号的缓存与草稿 */
function clearActiveUserOnScope(srv: string, scope: string) {
  const uid = readAuth(localStorage, scope)?.user.id;
  try {
    localStorage.removeItem(authKey(scope));
  } catch {}
  if (uid) {
    removeSavedAccountIn(localStorage, accountIdOf(srv, uid));
    removeUserServerData(localStorage, scope, uid);
  } else {
    const norm = normalizeServerUrl(srv) ?? srv;
    const hasOthers = listSavedAccountsIn(localStorage).some(
      (a) => (normalizeServerUrl(a.serverUrl) ?? a.serverUrl) === norm,
    );
    if (!hasOthers) removeServerData(localStorage, scope);
  }
}
export function clearSession() {
  // 登出/被踢：只清本服务器当前账号的凭据与数据，其他服务器及同服其他已保存账号不受影响
  const srv = getServer();
  clearActiveUserOnScope(srv, encServer(srv));
  sessionGen++; // 登出 = 旧会话作废
  boundIdentity = storedIdentity();
}

export function savedAccounts(): SavedAccount[] {
  const list = listSavedAccountsIn(localStorage);
  // 若当前会话未在多账号表里（例如从旧版升级上来），自动补一条展示
  const curSrv = getServer();
  const cur = readAuth(localStorage, serverScope());
  if (cur) {
    const id = accountIdOf(curSrv, cur.user.id);
    if (!list.some((x) => x.id === id)) {
      const srvName = listServersIn(localStorage).find((s) => s.url === curSrv)?.name;
      return upsertSavedAccountIn(localStorage, {
        serverUrl: curSrv,
        serverName: srvName,
        access: cur.access,
        refresh: cur.refresh,
        user: cur.user,
      });
    }
  }
  return list;
}

export function currentAccountId(): string | null {
  const cur = readAuth(localStorage, serverScope());
  return cur ? accountIdOf(getServer(), cur.user.id) : null;
}

export function removeAccount(id: string): SavedAccount[] {
  const curId = currentAccountId();
  const target = listSavedAccountsIn(localStorage).find((a) => a.id === id);
  if (target?.refresh) {
    const base = normalizeServerUrl(target.serverUrl) ?? target.serverUrl;
    void timedFetch(`${base}/v1/auth/logout`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ refresh_token: target.refresh }),
    }).catch(() => {});
  }
  const next = removeSavedAccountIn(localStorage, id);
  if (curId && id === curId) {
    clearSession();
    window.dispatchEvent(new Event("chat:logout"));
  } else if (target) {
    const base = normalizeServerUrl(target.serverUrl) ?? target.serverUrl;
    removeUserServerData(localStorage, encServer(base), target.user.id);
  }
  return next;
}

export async function addAccount(
  rawServer: string,
  serverName: string | undefined,
  username: string,
  password: string,
  displayName?: string,
): Promise<SavedAccount> {
  const srv = normalizeServerUrl(rawServer);
  if (!srv) throw new ApiError(400, "服务器地址格式无效");
  const res = await timedFetch(`${srv}/v1/auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      username: username.trim(),
      password,
      device_label: deviceLabel(),
    }),
  });
  const text = await res.text();
  if (!res.ok) {
    let msg = text || "登录失败";
    try {
      const j = JSON.parse(text) as { error?: unknown };
      if (typeof j.error === "string") msg = j.error;
    } catch {}
    if (res.status === 401) msg = "用户名或密码错误";
    throw new ApiError(res.status, msg);
  }
  const authResp = JSON.parse(text) as AuthResponse;
  let user = authResp.user;
  if (displayName && displayName.trim() && displayName.trim() !== user.display_name) {
    try {
      const patchRes = await timedFetch(`${srv}/v1/users/me`, {
        method: "PATCH",
        headers: {
          "Content-Type": "application/json",
          authorization: `Bearer ${authResp.access_token}`,
        },
        body: JSON.stringify({ display_name: displayName.trim() }),
      });
      if (patchRes.ok) {
        user = (await patchRes.json()) as UserPublic;
      }
    } catch {}
  }
  const cleanName = serverName?.trim() || undefined;
  upsertServer(localStorage, srv, {
    username: user.username,
    ...(cleanName ? { name: cleanName } : {}),
  });
  const updated = upsertSavedAccountIn(localStorage, {
    serverUrl: srv,
    serverName: cleanName ?? listServersIn(localStorage).find((s) => s.url === srv)?.name,
    access: authResp.access_token,
    refresh: authResp.refresh_token,
    user,
  });
  return updated.find((a) => a.id === accountIdOf(srv, user.id))!;
}

export function switchToAccount(id: string): SavedAccount | null {
  const target = listSavedAccountsIn(localStorage).find((a) => a.id === id);
  if (!target) return null;
  window.dispatchEvent(new Event("chat:before-switch-account"));

  const curSrv = getServer();
  const curAuth = readAuth(localStorage, encServer(curSrv));
  if (curAuth) {
    updateSavedAccountTokensIn(localStorage, accountIdOf(curSrv, curAuth.user.id), {
      access: curAuth.access,
      refresh: curAuth.refresh,
      user: curAuth.user,
    });
  }

  const targetScope = encServer(target.serverUrl);
  const prev = readAuth(localStorage, targetScope);
  if (prev && prev.user.id !== target.user.id) {
    updateSavedAccountTokensIn(localStorage, accountIdOf(target.serverUrl, prev.user.id), {
      access: prev.access,
      refresh: prev.refresh,
      user: prev.user,
    });
  }
  const chosen =
    prev && prev.user.id === target.user.id
      ? { access: prev.access, refresh: prev.refresh, user: prev.user }
      : { access: target.access, refresh: target.refresh, user: target.user };

  writeAuth(localStorage, targetScope, chosen);
  setServer(target.serverUrl);
  upsertServer(localStorage, target.serverUrl, {
    username: chosen.user.username,
    ...(target.serverName ? { name: target.serverName } : {}),
  });
  upsertSavedAccountIn(localStorage, {
    serverUrl: target.serverUrl,
    serverName: target.serverName,
    access: chosen.access,
    refresh: chosen.refresh,
    user: chosen.user,
  });
  sessionGen++;
  boundIdentity = storedIdentity();
  window.dispatchEvent(new Event("chat:switch-account"));
  return {
    ...target,
    access: chosen.access,
    refresh: chosen.refresh,
    user: chosen.user,
  };
}

export class ApiError extends Error {
  status: number;
  /** 稳定机器码：服务端附带（recall_expired / attachment_expired）或为已知服务端文案补的客户端码（not_member）；无则 undefined */
  code?: string;
  /** 响应体是否是带 "error" 字段的 JSON（区分业务 404 与路由不存在的裸 404） */
  hasErrorField?: boolean;
  constructor(status: number, msg: string, opts?: { code?: string; hasErrorField?: boolean }) {
    super(msg);
    this.status = status;
    this.code = opts?.code;
    this.hasErrorField = opts?.hasErrorField;
  }
}

// 刷新结果：凭据确实失效才登出；断网、5xx、维护中只是暂时不可用，保留凭据等下次
type Refreshed = "ok" | "invalid" | "transient";
// 明确拒绝时的登出提示（invalid 才可能有）
type RefreshOutcome = { r: Refreshed; reason?: string };

let refreshing: { gen: number; srv: string; promise: Promise<RefreshOutcome> } | null = null;

function requireSession(gen: number) {
  if (gen !== session()) throw new ApiError(0, "session changed");
}

// Web Locks 锁名按服务器区分：跨标签页串行化刷新，避免并发轮换互相消费掉对方的刷新令牌
const refreshLock = (srv: string) => `chat_token_refresh:${encServer(srv)}`;

// refresh 的明确拒绝（核对响应体，代理/WAF 拦下的 4xx 不得误判成凭据失效）：
// 401 invalid refresh token / session expired…（无效、吊销、被挤下线）；
// 403 账号已被禁用。其余响应一律按暂时失败处理。
const REFRESH_DENIED_401 = new Set([
  "invalid refresh token",
  "session expired, please login again",
]);
const REFRESH_DENIED_403 = "账号已被禁用";
const LOGOUT_EXPIRED = "登录已失效，请重新登录";
const LOGOUT_DISABLED = "账号已被禁用，请联系管理员"; // 与 ws.ts kickReasonText("disabled") 同文案

/** 服务端明确拒收 refresh token 时的登出文案；非明确拒绝返回 null（按暂时失败） */
async function refreshDeniedReason(res: Response): Promise<string | null> {
  if (res.status !== 401 && res.status !== 403) return null;
  let err: unknown;
  try {
    err = (await res.json())?.error;
  } catch {
    return null; // 非 JSON 响应（代理/网关拦下）：只是连不上，不动凭据
  }
  if (res.status === 403) return err === REFRESH_DENIED_403 ? LOGOUT_DISABLED : null;
  return REFRESH_DENIED_401.has(String(err)) ? LOGOUT_EXPIRED : null;
}

/** 业务接口 401（已换新 access token 仍被拒）：本服务的 JSON 报文才算会话级拒绝，代理/WAF 的不算 */
async function serverDenied(res: Response): Promise<boolean> {
  try {
    const j = await res.clone().json();
    return !!j && typeof j.error === "string";
  } catch {
    return false;
  }
}

/** 服务端明确踢下线：只清发起时那台服务器的凭据与快照、记它的登录页提示、广播登出。
 * 别的标签页已换账号时不替它登出；其他服务器的凭据不受影响。 */
function denySession(status: number, reason: string, srv: string): never {
  requireIdentity();
  const scope = encServer(srv);
  clearActiveUserOnScope(srv, scope);
  try {
    sessionStorage.setItem(kickReasonKey(scope), reason);
  } catch {}
  window.dispatchEvent(new Event("chat:logout"));
  throw new ApiError(status, reason);
}

// 单次请求超时：服务端不可达（IP 不通/防火墙丢包）时不能无限挂起，否则启动页一直停在「加载中…」
const REQUEST_TIMEOUT_MS = 10_000;

function timedFetch(url: string, init?: RequestInit, timeoutMs = REQUEST_TIMEOUT_MS): Promise<Response> {
  if (timeoutMs <= 0 || typeof AbortController === "undefined") return fetch(url, init);
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(new ApiError(0, "连接超时，请稍后再试")), timeoutMs);
  const outer = init?.signal ?? null;
  const onOuter = () => ctrl.abort(outer!.reason);
  if (outer) {
    if (outer.aborted) ctrl.abort(outer.reason);
    else outer.addEventListener("abort", onOuter, { once: true });
  }
  return fetch(url, { ...init, signal: ctrl.signal }).finally(() => {
    clearTimeout(timer);
    outer?.removeEventListener("abort", onOuter);
  });
}

// 单次刷新（在 Web Locks 内执行）：localStorage 是所有标签页共享的，
// 刷新令牌已被换成别的值 = 别的标签页刚刷新成功（或新登录），直接当作成功、
// 用存储里的新访问令牌重试即可，不再请求服务端也不清空会话
async function refreshOnce(gen: number, srv: string, rt: string): Promise<RefreshOutcome> {
  const scope = encServer(srv);
  const swapped = () => {
    const cur = readAuth(localStorage, scope)?.refresh;
    return cur !== undefined && cur !== rt;
  };
  if (swapped()) return { r: "ok" };
  let r: AuthResponse;
  try {
    const res = await timedFetch(`${srv}/v1/auth/refresh`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ refresh_token: rt }),
    });
    if (!res.ok) {
      if (swapped()) return { r: "ok" };
      const reason = await refreshDeniedReason(res);
      // 只有服务端明确拒绝（核对过响应体）才判 invalid，其余全部按暂时失败
      return reason ? { r: "invalid", reason } : { r: "transient" };
    }
    r = await res.json();
  } catch {
    return { r: swapped() ? "ok" : "transient" };
  }
  // 期间发生了登录/登出/换服务器：不能把旧凭据写回新会话
  if (gen !== session() || getServer() !== srv) return { r: "invalid" };
  if (readAuth(localStorage, scope)?.refresh !== rt) return { r: swapped() ? "ok" : "invalid" };
  writeAuth(localStorage, scope, {
    access: r.access_token,
    refresh: r.refresh_token,
    user: r.user,
  });
  try {
    updateSavedAccountTokensIn(localStorage, accountIdOf(srv, r.user.id), {
      access: r.access_token,
      refresh: r.refresh_token,
      user: r.user,
    });
  } catch {}
  return { r: "ok" };
}

async function tryRefresh(gen: number, srv: string): Promise<RefreshOutcome> {
  requireSession(gen);
  requireIdentity();
  // 单飞：同一代会话的并发 401 共用一次刷新，避免旧令牌被重复提交触发盗用判定
  if (refreshing?.gen === gen && refreshing.srv === srv) return refreshing.promise;
  const rt = readAuth(localStorage, encServer(srv))?.refresh;
  if (!rt) return { r: "invalid" };
  const owner = { gen, srv, promise: Promise.resolve<RefreshOutcome>({ r: "invalid" }) };
  refreshing = owner;
  const run = () => refreshOnce(gen, srv, rt);
  owner.promise = (async () => {
    try {
      const locks = typeof navigator !== "undefined" ? navigator.locks : undefined;
      if (locks) {
        try {
          return await locks.request(refreshLock(srv), run);
        } catch {
          // 锁服务本身异常：退化为无锁刷新
        }
      }
      return await run();
    } catch {
      return { r: "transient" } as RefreshOutcome;
    } finally {
      setTimeout(() => { if (refreshing === owner) refreshing = null; }, 0);
    }
  })();
  return owner.promise;
}

async function raw(srv: string, path: string, init?: RequestInit, timeoutMs?: number): Promise<Response> {
  requireIdentity();
  const headers: Record<string, string> = {
    ...(init?.headers as Record<string, string>),
  };
  const at = readAuth(localStorage, encServer(srv))?.access;
  if (at) headers["authorization"] = `Bearer ${at}`;
  if (typeof init?.body === "string") headers["Content-Type"] = "application/json";
  return timedFetch(`${srv}/v1${path}`, { ...init, headers }, timeoutMs);
}

// 登录/注册的 401 是业务错误（密码错/邀请码无效），与会话凭据无关：不触发刷新、不登出
const NO_REFRESH_PATHS = new Set(["/auth/login", "/auth/register"]);

async function call<T>(path: string, init?: RequestInit, opts?: { timeoutMs?: number }): Promise<T> {
  const gen = session();
  const srv = getServer();
  let res = await raw(srv, path, init, opts?.timeoutMs);
  requireSession(gen);
  if (res.status === 401 && !NO_REFRESH_PATHS.has(path)) {
    const refreshed = await tryRefresh(gen, srv);
    requireSession(gen);
    if (refreshed.r === "transient") throw new ApiError(503, "暂时连不上服务器，请稍后再试");
    if (refreshed.r === "ok") {
      res = await raw(srv, path, init, opts?.timeoutMs);
      requireSession(gen);
    }
    // 只有服务端明确拒绝 refresh 或新 access token 才登出；
    // 代理/网关拦下的 401（非本服务报文）按暂时失败保留会话
    const reason =
      refreshed.r === "invalid"
        ? refreshed.reason ?? LOGOUT_EXPIRED
        : res.status === 401 && (await serverDenied(res))
          ? LOGOUT_EXPIRED
          : null;
    if (reason) denySession(401, reason, srv);
    if (res.status === 401) throw new ApiError(503, "暂时连不上服务器，请稍后再试");
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  requireSession(gen);
  if (!res.ok) {
    let msg = text;
    let code: string | undefined;
    let hasErrorField = false;
    try {
      const j = JSON.parse(text) as { error?: unknown; code?: unknown };
      if (typeof j?.error === "string") {
        msg = j.error;
        hasErrorField = true;
      }
      if (typeof j?.code === "string") code = j.code;
    } catch {}
    // 账号被禁用：服务端对本会话一切 API 一律 403——与 refresh 被拒同等对待，踢回登录页
    if (res.status === 403 && msg === REFRESH_DENIED_403 && !NO_REFRESH_PATHS.has(path)) {
      denySession(403, LOGOUT_DISABLED, srv);
    }
    // 成员资格错误统一中文（服务端原文是英文，不能直接弹给用户）；补稳定码便于客户端分支
    if (msg === "not a member of this conversation") {
      msg = "你已不在该群";
      code ??= "not_member";
    }
    // 登录失败统一口语文案（服务端报文是英文代号 invalid credentials）
    if (path === "/auth/login" && res.status === 401) msg = "用户名或密码错误";
    // 服务端没有这个接口（客户端更新了、服务端还没更新）
    if (res.status === 404 && msg === "unknown_api") msg = "服务端版本较旧，不支持这个操作，请更新服务端";
    throw new ApiError(res.status, msg, { code, hasErrorField });
  }
  return JSON.parse(text) as T;
}

const post = <T>(p: string, body?: unknown) =>
  call<T>(p, { method: "POST", body: body == null ? "{}" : JSON.stringify(body) });
const get = <T>(p: string) => call<T>(p);
const del = <T>(p: string) => call<T>(p, { method: "DELETE" });

const patch = <T>(p: string, body: unknown) =>
  call<T>(p, { method: "PATCH", body: JSON.stringify(body) });
const put = <T>(p: string, body?: unknown) =>
  call<T>(p, {
    method: "PUT",
    body: body === undefined ? undefined : JSON.stringify(body),
  });

export type AndroidPushRegistration = {
  token: string; project: string; binding: string; rotationKey: string; generation: number;
};
export const registerAndroidPush = (registration: AndroidPushRegistration) =>
  put<void>("/users/me/push/android", registration);

// ---------- 设备名（V2 P1-5） ----------
// 登录/注册带给服务端的 device_label：Tauri 壳固定文案，Web 端按 UA 拼「浏览器 · 系统」。
export function osOfUa(ua: string): string {
  if (/Windows NT/i.test(ua)) return "Windows";
  if (/Android/i.test(ua)) return "Android";
  if (/iPhone|iPad|iPod/i.test(ua)) return "iOS";
  if (/Mac OS X/i.test(ua)) return "macOS";
  if (/Linux/i.test(ua)) return "Linux";
  return "未知系统";
}
function browserOfUa(ua: string): string {
  // Edge/Opera 的 UA 也含 Chrome/，必须先判
  if (/Edg(e|A|iOS)?\//i.test(ua)) return "Edge";
  if (/OPR\/|Opera/i.test(ua)) return "Opera";
  if (/Firefox\//i.test(ua)) return "Firefox";
  if (/Chrome\//i.test(ua)) return "Chrome";
  if (/Safari\//i.test(ua)) return "Safari";
  return "浏览器";
}
/** 纯函数：按 UA 生成 Web 端设备名（便于测试） */
export function browserDeviceLabel(ua: string): string {
  return `${browserOfUa(ua)} · ${osOfUa(ua)}`;
}
/** 当前设备的 device_label：Tauri Android → Chat Android；Tauri 桌面 → Chat 桌面端 (OS)；其余按 UA */
export function deviceLabel(): string {
  const w = globalThis as { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown; navigator?: { userAgent?: string } };
  const ua = w.navigator?.userAgent ?? "";
  if (w.__TAURI_INTERNALS__ || w.__TAURI__) {
    return /Android/i.test(ua) ? "Chat Android" : `Chat 桌面端 (${osOfUa(ua)})`;
  }
  return browserDeviceLabel(ua);
}

/** Tauri 桌面壳（Windows / macOS / Linux）；网页与 Android 为 false */
export function isDesktopShell(): boolean {
  const w = globalThis as { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown; navigator?: { userAgent?: string } };
  if (!w.__TAURI_INTERNALS__ && !w.__TAURI__) return false;
  return !/Android/i.test(w.navigator?.userAgent ?? "");
}

function syncAccountProfile(u: UserPublic) {
  try {
    const srv = getServer();
    const cur = readAuth(localStorage, encServer(srv));
    if (cur && cur.user.id === u.id) {
      writeAuth(localStorage, encServer(srv), { ...cur, user: u });
      upsertSavedAccountIn(localStorage, {
        serverUrl: srv,
        serverName: listServersIn(localStorage).find((s) => s.url === srv)?.name,
        access: cur.access,
        refresh: cur.refresh,
        user: u,
      });
    }
  } catch {}
}

// ---------- auth ----------
export const auth = {
  register: (username: string, password: string, display_name?: string, inviteCode?: string) =>
    post<AuthResponse>("/auth/register", {
      username,
      password,
      display_name,
      device_label: deviceLabel(),
      invite_code: inviteCode,
    }),
  login: (username: string, password: string) =>
    post<AuthResponse>("/auth/login", { username, password, device_label: deviceLabel() }),
  logout: () => {
    if (!identityMatches()) return; // 存储里已是别的标签页的账号
    const rt = readAuth(localStorage, serverScope())?.refresh;
    if (rt) post("/auth/logout", { refresh_token: rt }).catch(() => {});
    clearSession();
  },
  me: async () => {
    const u = await get<UserPublic>("/users/me");
    syncAccountProfile(u);
    return u;
  },
  updateMe: async (display_name: string) => {
    const u = await patch<UserPublic>("/users/me", { display_name });
    syncAccountProfile(u);
    return u;
  },
  /** 上传已裁好的头像；字段名 file，返回更新后的本人资料 */
  uploadAvatar: async (file: Blob) => {
    const fd = new FormData();
    const ext = file.type === "image/png" ? "png" : file.type === "image/webp" ? "webp" : "jpg";
    fd.append("file", file, `avatar.${ext}`);
    const u = await call<UserPublic>("/users/me/avatar", { method: "PUT", body: fd }, { timeoutMs: 0 });
    syncAccountProfile(u);
    return u;
  },
  removeAvatar: async () => {
    const u = await call<UserPublic>("/users/me/avatar", { method: "DELETE" });
    syncAccountProfile(u);
    return u;
  },
  /** 自助改密：返回为当前 session 新签发的凭据（其他 session 已被服务端撤销） */
  changePassword: (old_password: string, new_password: string) =>
    post<AuthResponse>("/users/me/password", { old_password, new_password }),
  /** 登录设备列表 */
  sessions: () => get<SessionView[]>("/users/me/sessions"),
  /** 移出一台设备（不能是当前会话） */
  revokeSession: (id: string) => del<void>(`/users/me/sessions/${id}`),
  /** 退出其他所有设备 */
  revokeOtherSessions: () => post<void>("/users/me/sessions/revoke-others"),
  /** 本人附件用量与配额（V2 P2-1） */
  storage: () => get<StorageView>("/users/me/storage"),
  /** ntfy 推送设置（V2 P3-3） */
  push: () => get<PushState>("/users/me/push"),
  /** 部分更新：只传要改的字段；服务器未配置时 enabled=true 返回 400 */
  updatePush: (patch: { enabled?: boolean; preview?: boolean }) =>
    put<PushState>("/users/me/push", patch),
  /** 登记一台手机，服务端给它生成订阅主题 */
  addPushDevice: (name: string) => post<PushState>("/users/me/push/devices", { name }),
  /** 移除一台手机，它的主题立即不再收到推送 */
  removePushDevice: (id: string) => del<PushState>(`/users/me/push/devices/${id}`),
  /** 给这台手机立即发一条测试推送；失败 502 */
  testPushDevice: (id: string) => post<PushState>(`/users/me/push/devices/${id}/test`),
  searchUser: async (q: string) => {
    const r = await get<unknown>(`/users/search?q=${encodeURIComponent(q)}`);
    // 兼容旧服务端返回单对象的形态
    return Array.isArray(r) ? (r as UserPublic[]) : r ? [r as UserPublic] : [];
  },
};

// ---------- friends ----------
export const friends = {
  list: () => get<FriendView[]>("/friends"),
  remove: (userId: string) => del<void>(`/friends/${userId}`),
  requests: () => get<RequestsView>("/friends/requests"),
  sendRequest: (username: string, message: string) =>
    post<{ id: string; status: string }>("/friends/requests", { username, message }),
  accept: (id: string) => post(`/friends/requests/${id}/accept`),
  decline: (id: string) => post(`/friends/requests/${id}/decline`),
};

// ---------- conversations ----------
export const convs = {
  list: () => get<ConversationView[]>("/conversations"),
  detail: (id: string) => get<ConversationDetail>(`/conversations/${id}`),
  messages: (id: string, opts?: { before_seq?: number; after_seq?: number; limit?: number }) => {
    const p = new URLSearchParams();
    if (opts?.before_seq != null) p.set("before_seq", String(opts.before_seq));
    if (opts?.after_seq != null) p.set("after_seq", String(opts.after_seq));
    p.set("limit", String(opts?.limit ?? 50));
    return get<MessagesPage>(`/conversations/${id}/messages?${p}`);
  },
  search: (id: string, q: string) =>
    get<MessageView[]>(`/conversations/${id}/messages/search?q=${encodeURIComponent(q)}`),
  createGroup: (name: string, member_ids: string[]) =>
    post<ConversationDetail>("/conversations/group", { name, member_ids }),
  addMembers: (id: string, user_ids: string[]) =>
    post(`/conversations/${id}/members`, { user_ids }),
  rename: (id: string, name: string) => patch(`/conversations/${id}`, { name }),
  leave: (id: string) => del<void>(`/conversations/${id}/members/me`),
  kick: (id: string, uid: string) => del<void>(`/conversations/${id}/members/${uid}`),
  dissolve: (id: string) => del<void>(`/conversations/${id}`),
  transfer: (id: string, uid: string) =>
    post(`/conversations/${id}/transfer`, { user_id: uid }),
  /** 更新本人偏好：只传要改的字段；返回最新偏好（幂等覆写） */
  setPref: (id: string, body: PrefUpdate) =>
    put<ConversationPref>(`/conversations/${id}/preferences`, body),
  /** 群主设置公告；null / 空字符串清空 */
  setAnnouncement: (id: string, text: string | null) =>
    put<AnnouncementView>(`/conversations/${id}/announcement`, { text }),
  pins: (id: string) => get<PinEntry[]>(`/conversations/${id}/pins`),
  pin: (id: string, messageId: string) => put<void>(`/conversations/${id}/pins/${messageId}`),
  unpin: (id: string, messageId: string) => del<void>(`/conversations/${id}/pins/${messageId}`),
  ticket: () => get<{ ticket: string; expires_in: number }>("/ws/ticket"),
};

// ---------- webhook bot ----------
export const bots = {
  list: () => get<BotView[]>("/bots"),
  /** 409 用户名已被占用；400 用户名格式 / 地址不对 / 数量上限 */
  create: (body: { username: string; display_name?: string; webhook_url?: string }) =>
    post<BotView>("/bots", body),
  patch: (id: string, body: { display_name?: string; webhook_url?: string }) =>
    patch<BotView>(`/bots/${id}`, body),
  resetSecret: (id: string) => post<BotView>(`/bots/${id}/secret`),
  addToken: (id: string, name: string) => post<BotView>(`/bots/${id}/tokens`, { name }),
  removeToken: (id: string, tokenId: string) =>
    del<BotView>(`/bots/${id}/tokens/${tokenId}`),
  remove: (id: string) => del<void>(`/bots/${id}`),
};

/** bot 的入站地址：带 Authorization: Bearer <令牌> POST JSON {"content": "..."} 即以 bot 身份发消息 */
export function hookUrl(): string {
  return `${getServer() || location.origin}/v1/hooks`;
}

// ---------- 消息生命周期 ----------
// mutation 全部服务端确认后更新（不乐观回滚）；client_op_id 为 UUID 幂等键，
// 操作结果未知时只能用原 id 重试。reaction/删除响应带 edit_version 供版本合并。

/** MM2：POST /messages/{id}/recall 不被旧服务端认识（路由缺失的裸 404/405）时
 * 退到 DELETE 别名并记住该服务器能力，之后直接用 DELETE（撤回=旧版删除）。 */
const recallFallbackServers = new Set<string>();

/** 当前服务器是否支持 MM1 撤回语义（24h 时限 / /recalled 取回 / 附件复用） */
export function serverSupportsRecall(): boolean {
  return !recallFallbackServers.has(getServer());
}

export const msgs = {
  edit: (id: string, content: string, expected_version: number, client_op_id?: string) =>
    patch<MessageView>(`/messages/${id}`, { content, expected_version, client_op_id }),
  remove: (id: string, client_op_id?: string) =>
    del<MessageView>(
      `/messages/${id}${client_op_id ? `?client_op_id=${client_op_id}` : ""}`,
    ),
  /** 撤回（MM2）：首选 POST /recall；旧服务端（405 或无 error 字段的裸 404）退 DELETE 别名 */
  recall: async (id: string, client_op_id?: string): Promise<MessageView> => {
    const q = client_op_id ? `?client_op_id=${client_op_id}` : "";
    if (recallFallbackServers.has(getServer())) return del<MessageView>(`/messages/${id}${q}`);
    try {
      return await post<MessageView>(`/messages/${id}/recall${q}`);
    } catch (e) {
      if (
        e instanceof ApiError &&
        (e.status === 405 || (e.status === 404 && !e.hasErrorField))
      ) {
        recallFallbackServers.add(getServer());
        return del<MessageView>(`/messages/${id}${q}`);
      }
      throw e;
    }
  },
  react: (id: string, emoji: string, add: boolean, client_op_id?: string) => {
    const q = client_op_id ? `?client_op_id=${client_op_id}` : "";
    const p = `/messages/${id}/reactions/${encodeURIComponent(emoji)}${q}`;
    return add ? put<MessageView>(p) : del<MessageView>(p);
  },
  /** 重连批量回刷缓存消息（每批 ≤100 个 id） */
  refresh: (convId: string, ids: string[]) =>
    post<MessagesPage>(`/conversations/${convId}/messages/refresh`, { ids }),
  /** 转发/收藏（V2 P2-4）：targets ≤10（服务端去重）；client_op_id 幂等——
   * 结果未确认时只能用原值重试，否则会产生第二条转发消息。
   * MF1 扩展：record_attachment_id = 转发聊天记录内的单个附件条目（404/400 由服务端统一拒绝）。 */
  forward: (id: string, conversation_ids: string[], client_op_id: string, opts?: { recordAttachmentId?: string }) =>
    post<ForwardResp>(`/messages/${id}/forward`, {
      conversation_ids,
      client_op_id,
      ...(opts?.recordAttachmentId ? { record_attachment_id: opts.recordAttachmentId } : {}),
    }),
  /** MF1：同一源会话多选转发（≤100 条 → ≤10 目标）；mode merged=每目标一条聊天记录 / separate=逐条。
   * 旧服务端没有此路由（405 或裸 404）时由调用方走逐条兜底。 */
  forwardBatch: (
    conversation_id: string,
    message_ids: string[],
    targets: string[],
    mode: "separate" | "merged",
    client_op_id: string,
  ) =>
    post<ForwardBatchResp>("/messages/forward-batch", {
      conversation_id,
      message_ids,
      targets,
      mode,
      client_op_id,
    }),
  /** MF1：聊天记录完整快照（仅所在会话成员可取；撤回/非记录消息 → 404 聊天记录不可用） */
  record: (id: string) => get<ChatRecord>(`/messages/${id}/record`),
};

// ---------- 轻量投票 ----------
// mutation 走 REST（服务端确认后更新）；同步靠既有 message / message_updated 广播帧。
export const polls = {
  /** 创建投票（仅群聊）；client_msg_id 为发送幂等键（重试收敛同一消息） */
  create: (convId: string, body: CreatePollReq) =>
    post<MessageView>(`/conversations/${convId}/polls`, body),
  /** 替换本人选择集合；空数组 = 撤票。返回本人视角完整 MessageView */
  vote: (msgId: string, optionIds: string[], client_op_id?: string) =>
    put<MessageView>(`/messages/${msgId}/poll/votes`, {
      option_ids: optionIds,
      client_op_id,
    }),
  /** 关闭投票（创建者或群主；幂等） */
  close: (msgId: string, client_op_id?: string) =>
    post<MessageView>(
      `/messages/${msgId}/poll/close${client_op_id ? `?client_op_id=${client_op_id}` : ""}`,
    ),
};

// ---------- 全局搜索（V2 P3-1） ----------
/** GET /v1/search 的查询参数：全部可选，但 q 或至少一个筛选必填（服务端强制 400） */
export interface SearchParams {
  q?: string;
  conversation_id?: string;
  sender_id?: string;
  kind?: "text" | "image" | "file" | "audio";
  /** RFC3339；含 from、不含 to */
  from?: string;
  to?: string;
  /** 翻页游标：上一页末条的 created_at + id */
  before?: string;
  before_id?: string;
  limit?: number;
}
export const searchApi = {
  messages: (p: SearchParams) => {
    const qs = new URLSearchParams();
    for (const [k, v] of Object.entries(p)) {
      if (v !== undefined && v !== "") qs.set(k, String(v));
    }
    return get<MessagesPage>(`/search?${qs}`);
  },
};

// ---------- files ----------
export const files = {
  /** 附件地址（能力 URL）：dl=true 强制下载；thumb=true 取 JPEG 缩略图（有则返回），默认 inline */
  url: (id: string, opts?: { dl?: boolean; thumb?: boolean }) => {
    const p = new URLSearchParams();
    if (opts?.dl) p.set("dl", "1");
    else if (opts?.thumb) p.set("thumb", "1");
    const q = p.toString();
    return `${getServer()}/v1/files/${id}${q ? `?${q}` : ""}`;
  },
  upload: async (
    conversationId: string,
    file: File,
    opts?: { durationMs?: number },
  ): Promise<AttachmentView> => {
    const fd = new FormData();
    fd.append("conversation_id", conversationId);
    fd.append("file", file, file.name);
    if (opts?.durationMs != null) fd.append("duration_ms", String(opts.durationMs));
    return call<AttachmentView>("/files", { method: "POST", body: fd }, { timeoutMs: 0 });
  },
  /**
   * MM2 撤回附件取回：GET /files/{id}/recalled → object URL。
   * Bearer 头鉴权（token 永不出现在 URL），no-store；仅上传者 + TTL 内有效，其余一律 404。
   */
  recalledUrl: async (id: string, opts?: { thumb?: boolean }): Promise<string> => {
    const srv = getServer();
    const q = opts?.thumb ? "?thumb=1" : "";
    const url = `${srv}/v1/files/${id}/recalled${q}`;
    const req = () =>
      timedFetch(url, {
        cache: "no-store",
        headers: {
          authorization: `Bearer ${readAuth(localStorage, encServer(srv))?.access ?? ""}`,
        },
      });
    let res = await req();
    if (res.status === 401) {
      const r = await tryRefresh(session(), srv);
      if (r.r === "ok") res = await req();
      else if (r.r === "invalid") denySession(401, r.reason ?? LOGOUT_EXPIRED, srv);
      else throw new ApiError(503, "暂时连不上服务器，请稍后再试");
    }
    if (!res.ok) throw new ApiError(res.status, "file not found", { hasErrorField: true });
    return URL.createObjectURL(await res.blob());
  },
};

// ---------- 公共服务器信息 ----------
/** 登录前探测注册模式。请求失败/404/字段缺失一律视为 open（兼容旧服务端） */
export async function serverInfo(server?: string): Promise<ServerInfo> {
  // 探测目标要先规范化（手动输入可能缺协议）；规范化不了的按原样试，失败走 open 兜底
  const srv =
    server === undefined ? getServer() : normalizeServerUrl(server) ?? server.trim().replace(/\/+$/, "");
  try {
    const res = await timedFetch(`${srv}/v1/server-info`);
    if (!res.ok) return { registration_mode: "open" };
    const j = (await res.json()) as Partial<ServerInfo>;
    const m = j.registration_mode;
    // push_available 缺省（旧服务端）按未配置处理
    return {
      registration_mode: m === "invite" || m === "closed" ? m : "open",
      push_available: j.push_available === true,
      android_fcm_available: j.android_fcm_available === true,
      meeting_base_url: typeof j.meeting_base_url === "string" ? j.meeting_base_url : undefined,
    };
  } catch {
    return { registration_mode: "open" };
  }
}

// ---------- admin ----------
export const admin = {
  overview: () => get<AdminOverview>("/admin/overview"),
  settings: () => get<AdminSettings>("/admin/settings"),
  /** 部分更新：只传要改的字段；返回完整设置 */
  updateSettings: (patch: Partial<AdminSettings>) =>
    put<AdminSettings>("/admin/settings", patch),
  invites: () => get<InviteView[]>("/admin/invites"),
  createInvite: (body: CreateInviteReq) => post<InviteView>("/admin/invites", body),
  /** 撤销（幂等 204） */
  revokeInvite: (id: string) => del<void>(`/admin/invites/${id}`),
  users: (q = "", offset = 0) =>
    get<AdminUserView[]>(
      `/admin/users?offset=${offset}${q ? `&q=${encodeURIComponent(q)}` : ""}`,
    ),
  disable: (id: string) => post(`/admin/users/${id}/disable`),
  enable: (id: string) => post(`/admin/users/${id}/enable`),
  setRole: (id: string, role: "admin" | "user") =>
    post(`/admin/users/${id}/role`, { role }),
  resetPassword: (id: string, password: string) =>
    post(`/admin/users/${id}/password`, { password }),
  // ---------- V2 P4-1 备份/恢复 ----------
  backups: () => get<BackupList>("/admin/backups"),
  createBackup: () => post<BackupCreated>("/admin/backups"),
  deleteBackup: (name: string) =>
    del<void>(`/admin/backups/${encodeURIComponent(name)}`),
  /** 浏览器 <a href> 不能带 Authorization：换一次性下载票据 */
  backupDownloadTicket: (name: string) =>
    post<BackupTicket>(
      `/admin/backups/${encodeURIComponent(name)}/download-ticket`,
    ),
  /**
   * 上传备份包恢复（confirm=RESTORE 由面板收口）。
   * 用 XMLHttpRequest 是为了 upload.onprogress 的进度回调。
   */
  restore: (file: File, onProgress?: (pct: number) => void) =>
    new Promise<RestoreResult>((resolve, reject) => {
      const fd = new FormData();
      fd.append("file", file, "backup.tar.gz");
      fd.append("confirm", "RESTORE");
      if (!identityMatches()) return reject(new ApiError(0, "session changed"));
      const xhr = new XMLHttpRequest();
      xhr.open("POST", `${getServer()}/v1/admin/restore`);
      const at = readAuth(localStorage, serverScope())?.access;
      if (at) xhr.setRequestHeader("authorization", `Bearer ${at}`);
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable && onProgress)
          onProgress(Math.round((e.loaded / e.total) * 100));
      };
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          try {
            resolve(JSON.parse(xhr.responseText) as RestoreResult);
          } catch {
            resolve({ tables: [], files: 0 });
          }
          return;
        }
        let msg = xhr.responseText;
        try {
          msg = (JSON.parse(xhr.responseText).error as string) ?? msg;
        } catch {}
        reject(new ApiError(xhr.status, msg || "恢复失败"));
      };
      xhr.onerror = () => reject(new ApiError(0, "网络错误"));
      xhr.send(fd);
    }),
  restoreDroppedBackup: async (id: string, onProgress: (pct: number) => void) => {
    const { Channel, invoke } = await import("@tauri-apps/api/core");
    requireIdentity();
    const progress = new Channel<number>();
    progress.onmessage = onProgress;
    try {
      await invoke("restore_dropped_backup", {
        id,
        server: getServer(),
        access: readAuth(localStorage, serverScope())?.access ?? "",
        progress,
      });
    } catch (e) {
      throw new Error(String(e));
    }
  },
};

// ---------- ACP Agent Workspace ----------
export const acp = {
  bootstrap: () => get<AcpBootstrap>("/acp/bootstrap"),
  updateSettings: (default_permission: AcpPermissionPolicy) =>
    put<AcpSettings>("/acp/settings", { default_permission }),
  createPairCode: () => post<{ code: string; expires_in: number }>("/acp/hosts/pair-code"),
  renameHost: (id: string, name: string) => patch<AcpHost>(`/acp/hosts/${id}`, { name }),
  deleteHost: (id: string) => del<void>(`/acp/hosts/${id}`),
  listDirs: (hostId: string, path = "") =>
    get<AcpDirListing>(`/acp/hosts/${hostId}/dirs?path=${encodeURIComponent(path)}`),
  readHostFile: (hostId: string, path: string, cwd?: string) => {
    const qs = new URLSearchParams({ path });
    if (cwd) qs.set("cwd", cwd);
    return get<AcpHostFileContent>(`/acp/hosts/${hostId}/file?${qs}`);
  },
  createAgent: (body: {
    host_id: string;
    preset: string;
    name: string;
    adapter?: string;
    cmd: string;
    env?: Record<string, string>;
    local_probed?: boolean;
    status?: string;
    default_model?: string;
    model_options?: string[];
  }) => post<AcpAgent>("/acp/agents", body),
  updateAgent: (
    id: string,
    body: {
      name?: string;
      cmd?: string;
      env?: Record<string, string>;
      default_model?: string;
      model_options?: string[];
      permission_override?: "inherit" | AcpPermissionPolicy;
    },
  ) => patch<AcpAgent>(`/acp/agents/${id}`, body),
  deleteAgent: (id: string) => del<void>(`/acp/agents/${id}`),
  probeAgentModels: (
    id: string,
    body?: { session_id?: string; wait?: boolean },
  ) => post<AcpAgent>(`/acp/agents/${id}/probe-models`, body ?? {}),
  createWorkspace: (body: {
    host_id: string;
    cwd: string;
    name?: string;
    local_probed?: boolean;
    branch?: string;
    branches?: string[];
  }) => post<AcpWorkspace>("/acp/workspaces", body),
  deleteWorkspace: (id: string) => del<void>(`/acp/workspaces/${id}`),
  gitCheckout: (
    workspaceId: string,
    branch: string,
    extra?: { local_probed?: boolean; branches?: string[] },
  ) => post<AcpWorkspace>(`/acp/workspaces/${workspaceId}/git-checkout`, { branch, ...extra }),
  createSession: (body: {
    workspace_id: string;
    agent_id: string;
    title?: string;
    skip_model_probe?: boolean;
  }) => post<AcpSession>("/acp/sessions", body),
  updateSession: (
    id: string,
    body: {
      title?: string;
      selected_model?: string;
      thinking_level?: AcpThinkingLevel;
      permission_policy?: AcpPermissionPolicy;
    },
  ) => patch<AcpSession>(`/acp/sessions/${id}`, body),
  deleteSession: (id: string) => del<void>(`/acp/sessions/${id}`),
  sendPrompt: (
    sessionId: string,
    body: {
      text: string;
      files?: string[];
      attachment_ids?: string[];
      model?: string;
      thinking_level?: AcpThinkingLevel;
      permission_policy?: AcpPermissionPolicy;
    },
  ) => post<AcpTurn>(`/acp/sessions/${sessionId}/prompts`, body),
  cancelSession: (sessionId: string) =>
    post<AcpSession>(`/acp/sessions/${sessionId}/cancel`, {}),
  replyPermission: (
    turnId: string,
    option_id: "allow_once" | "allow_always" | "reject_once" | "reject_always",
  ) => post<AcpTurn>(`/acp/permissions/${turnId}`, { option_id }),
  uploadFile: (file: File, sessionId?: string): Promise<AcpAttachment> => {
    const fd = new FormData();
    if (sessionId) fd.append("session_id", sessionId);
    fd.append("file", file, file.name);
    return call<AcpAttachment>("/acp/files", { method: "POST", body: fd }, { timeoutMs: 0 });
  },
  fetchFileBlob: async (id: string, opts?: { dl?: boolean }): Promise<Blob> => {
    const gen = session();
    const srv = getServer();
    const path = `/acp/files/${id}${opts?.dl ? "?dl=1" : ""}`;
    let res = await raw(srv, path, undefined, 0);
    requireSession(gen);
    if (res.status === 401) {
      const refreshed = await tryRefresh(gen, srv);
      requireSession(gen);
      if (refreshed.r === "transient") {
        throw new ApiError(503, "暂时连不上服务器，请稍后再试");
      }
      if (refreshed.r === "ok") {
        res = await raw(srv, path, undefined, 0);
        requireSession(gen);
      }
      const reason =
        refreshed.r === "invalid"
          ? refreshed.reason ?? LOGOUT_EXPIRED
          : res.status === 401 && (await serverDenied(res))
            ? LOGOUT_EXPIRED
            : null;
      if (reason) denySession(401, reason, srv);
      if (res.status === 401) {
        throw new ApiError(503, "暂时连不上服务器，请稍后再试");
      }
    }
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let msg = text || `读取附件失败 (${res.status})`;
      try {
        const j = JSON.parse(text) as { error?: unknown };
        if (typeof j?.error === "string") msg = j.error;
      } catch {}
      throw new ApiError(res.status, msg);
    }
    return res.blob();
  },
  downloadFile: async (id: string, fileName: string): Promise<void> => {
    const blob = await acp.fetchFileBlob(id, { dl: true });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = fileName || "attachment";
    a.style.display = "none";
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  },
  fileUrl: (id: string, dl?: boolean) =>
    `${getServer()}/v1/acp/files/${id}${dl ? "?dl=1" : ""}`,
};
