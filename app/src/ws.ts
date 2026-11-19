import {
  acp,
  ApiError,
  auth,
  clearSession,
  convs,
  friends as friendsApi,
  msgs,
  serverScope,
  session,
  wsBase,
} from "./api";
import { messageLine, notifyIncoming } from "./notify";
import { clearReuseOnAck, failReuseSend } from "./drafts";
import { kickReasonKey } from "./servers";
import {
  absorbAvatarUsers,
  applyAcpBootstrap,
  applyAcpEvent,
  applyGroupLeft,
  applyLiveMessage,
  applyPrefUpdate,
  applyRefreshed,
  applySyncMessages,
  applyUpdatedMessage,
  convTitle,
  dropConversationCache,
  dropOptimistic,
  getState,
  isOwnMessage,
  isViewing,
  mergeConversationLists,
  pushToast,
  replyPreview,
  resolveAck,
  setState,
  shouldNotify,
  updateDrafts,
  upsertMessage,
} from "./store";
import type { AttachmentView, MessageView, WsOut } from "./types";

let ws: WebSocket | null = null;
let reconnectTimer: number | null = null;
let attempts = 0;
let stopped = false;
let catalogGen = 0;
let wsGen = -1; // 建立连接时的会话代次快照；换代后旧 socket/异步回调一律作废
let connectionAttempt = 0;
let heartbeat: ReturnType<typeof setTimeout> | null = null;
let pongDeadline: ReturnType<typeof setTimeout> | null = null;

function clearHeartbeat() {
  if (heartbeat != null) clearTimeout(heartbeat);
  if (pongDeadline != null) clearTimeout(pongDeadline);
  heartbeat = pongDeadline = null;
}

function reconnectNow() {
  if (stopped || wsGen !== session()) return;
  connectionAttempt++;
  clearHeartbeat();
  const sock = ws;
  ws = null;
  if (sock) {
    sock.onclose = null;
    sock.close();
  }
  setState({ wsReady: false });
  scheduleReconnect(wsGen);
}

function scheduleHeartbeat(sock: WebSocket) {
  clearHeartbeat();
  heartbeat = setTimeout(() => {
    if (ws !== sock || sock.readyState !== WebSocket.OPEN) return;
    sock.send(JSON.stringify({ type: "ping" }));
    pongDeadline = setTimeout(reconnectNow, 3_000);
  }, 4_000);
}

function onOnline() {
  if (stopped || getState().wsReady) return;
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) return;
  if (reconnectTimer != null) clearTimeout(reconnectTimer);
  reconnectTimer = null;
  attempts = 0;
  void connect();
}

// ---- 同步水位（P1-9）：以「最高连续 seq」为准，缺口靠 sync 补齐 ----
const syncedSeq = new Map<string, number>(); // conv_id → 已确认连续的最大 seq
const syncInflight = new Set<string>(); // 每会话只允许一个未完成 sync
const readSeq = new Map<string, number>(); // conv_id → 已应用的最大 read_sync seq（P2-4 单调）

type Outgoing = {
  convId: string;
  clientMsgId: string;
  content: string;
  attachment?: AttachmentView;
  /** MM2：true 时发 reuse_attachment_id（撤回重编辑的附件复用），而不是 attachment_id */
  reuse?: boolean;
  replyToId?: string;
  /** UI4：kind=meeting 时服务端落 Meeting Card */
  kind?: string;
  /** 乐观消息原样存：刷新/重启后照它放回列表 */
  optimistic: MessageView;
  /** 永久失败的原因：不再自动重发，等用户重试或删除 */
  error?: string;
  sentOn?: WebSocket;
  /** 最近一次写入 socket 的时间（不持久化）：ack 看门狗据此判超时 */
  sentAt?: number;
};

/** ack 看门狗：发出 ACK_TIMEOUT_MS 未确认即重连重放，
 * 期间每 ACK_WATCHDOG_MS 扫一次——ack 丢了但消息本体已回显的，直接按 ack 收敛。 */
const ACK_TIMEOUT_MS = 10_000;
const ACK_WATCHDOG_MS = 3_000;
let ackWatchdog: number | null = null;

/** 未收到 ack 的发送。重连后按原 client_msg_id 重放，服务端幂等。
 * 持久化到 localStorage（按服务器 + 账号），刷新、退出、手机杀进程都不丢。 */
const outbox: Outgoing[] = [];
let outboxKey = "";

// ponytail: 同账号多标签页共用一个键、互相覆写；最坏是某页的待发记录没存上（发送本身幂等不重复），需要时按标签页分键再合并
function saveOutbox() {
  if (!outboxKey) return;
  try {
    if (outbox.length === 0) localStorage.removeItem(outboxKey);
    else localStorage.setItem(outboxKey, JSON.stringify(outbox.map(({ sentOn: _s, sentAt: _t, ...item }) => item)));
  } catch {}
}

/** 登录后取回本账号未发出的消息，放回各会话列表；连上后 flushOutbox 按原 client_msg_id 重发 */
function restoreOutbox() {
  const me = getState().me;
  outboxKey = me ? `chat_outbox:${serverScope()}:${me.id}` : "";
  outbox.length = 0;
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(outboxKey) || "[]");
    if (Array.isArray(saved)) outbox.push(...saved.filter((x) => x?.clientMsgId && x.optimistic));
  } catch {}
  if (outbox.length === 0) return;
  setState((s) => {
    const messages = { ...s.messages };
    for (const item of outbox) {
      messages[item.convId] = upsertMessage(messages[item.convId] ?? [], {
        ...item.optimistic,
        send_error: item.error,
      });
    }
    return { messages };
  });
}

const SEND_ERROR_TEXT: Record<string, string> = {
  bad_message: "内容不合法（1–4000 字）",
  forbidden: "没有权限",
  not_friends: "对方已不是你的好友",
  bad_attachment: "附件无效",
  attachment_expired: "附件已过期",
  not_found: "会话不存在",
  bad_request: "内容不合法（1–4000 字）",
  bad_frame: "客户端版本过旧",
};

const PERMANENT_SEND_ERRORS = new Set([
  "bad_message",
  "forbidden",
  "not_friends",
  "bad_attachment",
  "attachment_expired",
  "not_found",
  "bad_request",
  "bad_frame",
]);

/** MM2：复用发送被服务端拒收时按这些码走「退回草稿 + chip 过期」路径 */
const REUSE_FAIL_CODES = new Set(["attachment_expired", "forbidden", "bad_request"]);

export function stopWs() {
  connectionAttempt++;
  clearHeartbeat();
  window.removeEventListener?.("offline", reconnectNow);
  window.removeEventListener?.("online", onOnline);
  stopped = true;
  wsGen = -1; // 使在途连接/回调全部失效（P1-10）
  catalogGen++; // 使在途目录刷新失效
  // 只清内存：本地存的待发记录留给该账号下次登录
  outbox.length = 0;
  outboxKey = "";
  // P2-7：清理重连定时器并复位计数/水位，避免新会话继承旧状态
  if (reconnectTimer != null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  attempts = 0;
  syncedSeq.clear();
  syncInflight.clear();
  readSeq.clear();
  if (ackWatchdog != null) {
    clearTimeout(ackWatchdog);
    ackWatchdog = null;
  }
  if (ws) {
    ws.onclose = null;
    ws.close();
    ws = null;
  }
  setState({ wsReady: false });
}

export async function startWs() {
  stopped = false;
  wsGen = session(); // P1-10：绑定当前登录会话
  window.addEventListener?.("offline", reconnectNow);
  window.addEventListener?.("online", onOnline);
  restoreOutbox();
  scheduleAckWatchdog();
  await connect();
}

export async function refreshCatalogs(): Promise<void> {
  const gen = ++catalogGen;
  const sess = session();
  const [conversations, friends, requests] = await Promise.all([
    convs.list(),
    friendsApi.list(),
    friendsApi.requests(),
  ]);
  if (gen !== catalogGen || sess !== session()) return;
  setState((s) => {
    let next = {
      ...s,
      conversations: mergeConversationLists(s.conversations, conversations),
      friends,
      friendsLoaded: true,
      requests,
      avatars: absorbAvatarUsers(s.avatars, [
        s.me,
        ...conversations.map((c) => c.peer),
        ...friends.map((f) => f.user),
        ...requests.incoming.map((r) => r.user),
        ...requests.outgoing.map((r) => r.user),
      ]),
    };
    for (const c of conversations) {
      if (c.last_message && s.messages[c.id]) {
        next = { ...next, ...applyRefreshed(next, c.id, [c.last_message]) };
      }
    }
    return next;
  });
}

async function connect() {
  if (stopped) return;
  const attempt = ++connectionAttempt;
  const gen = wsGen;
  let ticket: string;
  try {
    ticket = (await convs.ticket()).ticket;
  } catch {
    if (gen === wsGen && attempt === connectionAttempt) scheduleReconnect(gen);
    return;
  }
  if (stopped || gen !== wsGen || gen !== session() || attempt !== connectionAttempt) return; // ticket 落地前会话已换代 → 丢弃
  if (ws) {
    clearHeartbeat();
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    ws.close();
  }
  const sock = new WebSocket(`${wsBase()}/v1/ws?ticket=${ticket}`);
  ws = sock;

  sock.onopen = () => {
    if (gen !== wsGen || gen !== session() || ws !== sock) return;
    attempts = 0;
    setState({ wsReady: true });
    scheduleHeartbeat(sock);
    void afterOpen(sock, gen);
  };
  sock.onmessage = (ev) => {
    if (gen !== wsGen || gen !== session() || ws !== sock) return;
    scheduleHeartbeat(sock);
    try {
      const frame = JSON.parse(ev.data);
      if (frame.type === "pong") return;
      dispatch(frame as WsOut);
    } catch (e) {
      console.warn("bad ws frame", e);
    }
  };
  sock.onclose = () => {
    if (gen !== wsGen || gen !== session() || ws !== sock) return;
    setState({ wsReady: false });
    clearHeartbeat();
    if (!stopped) scheduleReconnect(gen);
  };
  sock.onerror = () => sock.close();
}

async function afterOpen(sock: WebSocket, gen: number) {
  const alive = () =>
    !stopped && gen === wsGen && gen === session() && ws === sock && sock.readyState === WebSocket.OPEN;
  // 重连恢复：刷新本人资料（离线期间可能被改）+ 拉全部目录 + 同步 ACP 工作区
  const [me] = await Promise.all([
    auth.me().catch((e: unknown) => {
      console.warn("me refresh failed", e);
      return null;
    }),
    refreshCatalogs().catch((e: unknown) => console.warn("refresh failed", e)),
    acp
      .bootstrap()
      .then((bs) => {
        if (alive()) setState((cur) => applyAcpBootstrap(cur, bs));
      })
      .catch(() => {}),
  ]);
  if (me && alive()) setState({ me });
  if (!alive()) return;
  const s = getState();
  syncInflight.clear(); // 旧 socket 上的未完成 sync 随连接作废
  for (const conv of new Set([...Object.keys(s.messages), ...syncedSeq.keys()])) {
    const last = syncedSeq.get(conv) ?? contiguousSeq(s.messages[conv] ?? []);
    syncedSeq.set(conv, last);
    sock.send(
      JSON.stringify({
        type: "sync",
        conversation_id: conv,
        last_seq: last,
      }),
    );
    // seq 同步只补新消息；离线期间的编辑/删除/reaction 要靠按 id 回刷缓存补齐
    void refreshCachedMessages(conv, gen);
  }
  flushOutbox();
}

/**
 * 断线恢复第二步：把本地已缓存的 message id 分批（≤100）回读服务端最新状态，
 * 合并正文/墓碑/reaction/回复预览。会话已无权限时清空本地缓存。
 */
async function refreshCachedMessages(convId: string, gen: number) {
  const ids = (getState().messages[convId] ?? [])
    .filter((m) => !m.id.startsWith("client:"))
    .map((m) => m.id);
  for (let i = 0; i < ids.length; i += 100) {
    if (stopped || gen !== wsGen || gen !== session()) return;
    try {
      const p = await msgs.refresh(convId, ids.slice(i, i + 100));
      if (stopped || gen !== wsGen || gen !== session()) return;
      setState((cur) => applyRefreshed(cur, convId, p.messages));
    } catch (e) {
      if (stopped || gen !== wsGen || gen !== session()) return;
      let inaccessible = e instanceof ApiError && e.status === 403;
      if (e instanceof ApiError && e.status === 404) {
        try {
          await convs.detail(convId);
        } catch (detailError) {
          inaccessible = detailError instanceof ApiError && (detailError.status === 403 || detailError.status === 404);
        }
      }
      if (stopped || gen !== wsGen || gen !== session()) return;
      if (inaccessible) setState((cur) => dropConversationCache(cur, convId));
      else if (!(e instanceof ApiError && e.status === 404)) console.warn("message refresh failed", convId, e);
      return;
    }
  }
}

/** 缓存中已确认连续的最高 seq：从最小观测 seq 向上推进，遇到缺口即停（不含乐观消息）。 */
function contiguousSeq(msgs: MessageView[]): number {
  const seqs = msgs
    .filter((m) => !m.id.startsWith("client:"))
    .map((m) => m.seq)
    .sort((a, b) => a - b);
  if (!seqs.length) return 0;
  let w = seqs[0];
  for (const seq of seqs) {
    if (seq > w + 1) break;
    if (seq > w) w = seq;
  }
  return w;
}

/**
 * 实时消息计入连续水位（P1-9）：
 * - seq <= 水位：已覆盖，忽略；
 * - seq == 水位+1 或无基线（水位 0）：推进水位；
 * - seq 跳过水位：有缺口 → 发 sync 从水位处补齐。
 */
function trackIncomingSeq(conv: string, seq: number) {
  let wm = syncedSeq.get(conv);
  if (wm === undefined) {
    wm = contiguousSeq(getState().messages[conv] ?? []);
    syncedSeq.set(conv, wm);
  }
  if (seq <= wm) return;
  if (seq === wm + 1 || wm === 0) {
    syncedSeq.set(conv, seq);
  } else {
    requestSync(conv, wm);
  }
}

/** 从连续水位发起 sync（防抖：同会话只允许一个未完成请求） */
function requestSync(conv: string, lastSeq: number) {
  if (syncInflight.has(conv)) return;
  if (ws?.readyState !== WebSocket.OPEN) return;
  syncInflight.add(conv);
  wsSend({ type: "sync", conversation_id: conv, last_seq: lastSeq });
}

function scheduleReconnect(gen: number) {
  if (stopped || reconnectTimer != null) return;
  const delay = Math.min(15000, 1000 * 2 ** attempts++);
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null;
    if (stopped || gen !== wsGen || gen !== session()) return; // P2-7/P1-10：换代后的定时器不动作
    void connect();
  }, delay);
}

/** force_logout reason → 登录页提示文案（V2 P1-5：新增改密/移出设备两类原因） */
export function kickReasonText(reason: string): string {
  switch (reason) {
    case "disabled":
      return "账号已被禁用，请联系管理员";
    case "password_reset":
      return "密码已被管理员重置，请重新登录";
    case "password_changed":
      return "密码已在其他设备修改，请重新登录";
    case "session_revoked":
      return "该设备已被移出登录，请重新登录";
    case "maintenance":
      return "服务器维护中，请稍后重新登录";
    case "restoring":
      return "正在恢复备份，完成后请重新登录";
    default:
      return "登录已失效，请重新登录";
  }
}

function dispatch(f: WsOut) {
  const s = getState();
  switch (f.type) {
    case "presence_init": {
      const online: Record<string, boolean> = {};
      f.online.forEach((u) => (online[u] = true));
      setState({ online });
      break;
    }
    case "presence":
      setState({
        online: { ...s.online, [f.user_id]: f.online },
        // 同步 DM 会话里 peer 的 last_seen_at（聊天头「最后在线」）
        conversations: f.last_seen_at
          ? s.conversations.map((c) =>
              c.peer?.id === f.user_id
                ? { ...c, peer: { ...c.peer, last_seen_at: f.last_seen_at } }
                : c,
            )
          : s.conversations,
      });
      break;
    case "message":
      onMessage(f.message);
      trackIncomingSeq(f.message.conversation_id, f.message.seq);
      break;
    case "message_ack": {
      resolveSend(f.client_msg_id, f.message);
      break;
    }
    case "sync_messages": {
      setState((cur) => applySyncMessages(cur, f.conversation_id, f.messages));
      // 服务端按 last_seq 之后连续有序返回 → 本页末尾即新水位
      const lastMsg = f.messages[f.messages.length - 1];
      if (lastMsg) {
        syncedSeq.set(
          f.conversation_id,
          Math.max(syncedSeq.get(f.conversation_id) ?? 0, lastMsg.seq),
        );
      }
      // P2-3：服务端分页（>500 条）→ 按新水位续拉；无 has_more 字段的旧服务端视为完结
      if (f.has_more && lastMsg) {
        syncInflight.delete(f.conversation_id);
        requestSync(f.conversation_id, lastMsg.seq);
      } else {
        syncInflight.delete(f.conversation_id);
      }
      break;
    }
    case "typing":
      setState({
        typing: {
          ...s.typing,
          [f.conversation_id]: { user: f.user, ts: Date.now() },
        },
      });
      break;
    case "read_sync": {
      // P2-4：单调推进——迟到的旧事件不得回退已读位置、清掉新未读
      if (f.seq <= (readSeq.get(f.conversation_id) ?? -1)) break;
      readSeq.set(f.conversation_id, f.seq);
      let incomplete = false;
      setState((cur) => ({
        conversations: cur.conversations.map((c) => {
          if (c.id !== f.conversation_id) return c;
          const unread = Math.min(c.unread_count, Math.max(0, c.last_seq - f.seq));
          const remaining = (cur.messages[c.id] ?? []).filter((m) =>
            !m.id.startsWith("client:") && m.seq > c.last_seq - unread && m.seq <= c.last_seq);
          const complete = remaining.length === unread;
          if (!complete) incomplete = true;
          return { ...c, unread_count: unread, has_mention: unread > 0 &&
            (remaining.some((m) => !m.deleted_at && m.mentions.includes(cur.me?.id ?? "")) || (!complete && c.has_mention)) };
        }),
      }));
      if (incomplete) void refreshCatalogs().catch((e) => console.warn("read refresh failed", e));
      break;
    }
    // 所有生命周期帧都携带完整视图（reactions 已个性化），统一整视图版本合并
    case "message_updated":
    case "message_deleted":
    case "reaction_update":
      setState((cur) => applyUpdatedMessage(cur, f.message));
      break;
    case "preference_updated": {
      // 偏好只发本人连接：直接按 conversation_id 合并；本地不认识该会话时拉目录兜底
      const knownConv = s.conversations.some((c) => c.id === f.conversation_id);
      setState((cur) => applyPrefUpdate(cur, f.conversation_id, f.preference));
      if (!knownConv)
        void refreshCatalogs().catch((e) => console.warn("refresh failed", e));
      break;
    }
    case "invalidate":
      void refreshByWhat(f.what);
      break;
    case "pins_updated":
      window.dispatchEvent(
        new CustomEvent("chat:pins", { detail: f.conversation_id }),
      );
      break;
    case "acp_event":
      setState((cur) => applyAcpEvent(cur, f.event, f.data));
      break;
    case "force_logout": {
      // 被踢下线（禁用/重置密码/改密/移出设备）：记本服务器的原因 → 停连接 → 回登录页
      sessionStorage.setItem(kickReasonKey(serverScope()), kickReasonText(f.reason));
      stopWs();
      clearSession();
      window.dispatchEvent(new Event("chat:logout"));
      break;
    }
    case "error": {
      // MM2：复用发送被拒（attachment_expired/forbidden/bad_request）→
      // 撤乐观气泡、文字退回草稿、chip 标「附件已过期」，由用户手动重发，绝不自动重发。
      const item = f.client_msg_id ? outbox.find((x) => x.clientMsgId === f.client_msg_id) : undefined;
      if (item?.reuse && REUSE_FAIL_CODES.has(f.code)) {
        takeOutbox(item.clientMsgId);
        const convId = item.convId;
        const content = item.content;
        const attachmentId = item.attachment?.id;
        setState((cur) => dropOptimistic(cur, convId, item.clientMsgId));
        updateDrafts((d) => failReuseSend(d, convId, attachmentId, content));
        // 打开着的会话需要同步 composer 文本态（draft 已改，组件据此回填）
        window.dispatchEvent(new CustomEvent("chat:composer-restore", { detail: { convId } }));
      } else if (f.client_msg_id && PERMANENT_SEND_ERRORS.has(f.code)) {
        markFailed(f.client_msg_id, SEND_ERROR_TEXT[f.code] ?? f.message);
      }
      console.warn("ws error", f.code, f.message);
      break;
    }
  }
}

/** 窗口在前台（可见且有焦点）：只有这时「打开着的会话」才算正在看 */
function inForeground(): boolean {
  return typeof document === "undefined" || (!document.hidden && document.hasFocus());
}

// 回到前台：补报当前打开会话的已读（后台期间不自动已读，未读数与标题才准确）
function readActiveOnReturn() {
  const s = getState();
  if (s.activeConv && isViewing(s, s.activeConv) && inForeground()) openConversation(s.activeConv);
}
if (typeof window !== "undefined") {
  window.addEventListener("focus", readActiveOnReturn);
  document.addEventListener("visibilitychange", readActiveOnReturn);
}

function onMessage(m: MessageView) {
  const known = getState().conversations.some((c) => c.id === m.conversation_id);
  const s = getState();
  const fg = inForeground();
  const isActive = isViewing(s, m.conversation_id) && fg;
  const mine = isOwnMessage(m, s.me?.id);
  setState((cur) => applyLiveMessage(cur, m, fg));
  if (!known) void refreshCatalogs().catch((e) => console.warn("refresh failed", e));
  // 当前打开且在前台的会话收到别人消息 → 上报已读
  if (isActive && !mine) sendRead(m.conversation_id, m.seq);
  // 最小应用内提示：静音会话被抑制（@我 除外）；点击打开会话
  const cur = getState();
  const existed = (s.messages[m.conversation_id] ?? []).some((x) => x.id === m.id)
    || s.conversations.some((c) => c.last_message?.id === m.id);
  if (!existed && shouldNotify(cur, m)) {
    const c = cur.conversations.find((x) => x.id === m.conversation_id);
    const tag = cur.me && m.mentions.includes(cur.me.id) ? "[有人@我] " : "";
    pushToast({
      convId: m.conversation_id,
      title: c ? convTitle(c) : "新消息",
      text: `${tag}${messageLine(m)}`,
    });
  }
  // P1-1 系统通知：窗口不在前台时弹出（与 toast 共用 !existed 去重；开关/权限/静音在内部判定）
  if (!existed) void notifyIncoming(cur, m, openConversation);
}

async function refreshByWhat(what: string) {
  try {
    if (what === "conversations" || what === "friends" || what === "requests") {
      await refreshCatalogs();
    } else if (what === "me") {
      setState({ me: await auth.me() });
    }
  } catch (e) {
    console.warn("refresh failed", e);
  }
}

/** 服务端真身到手（message_ack，或 ack 丢失后经广播/sync 回显匹配）：
 * 移出待发队列、按 ack 语义用真身替换乐观气泡。ack 对应条目已不在队列里
 * （如另一标签页清过）也照常并入真身，不丢消息。 */
function resolveSend(clientMsgId: string | null, real: MessageView) {
  const item = takeOutbox(clientMsgId);
  // MM2：复用发送成功 → 摘掉草稿上的「复用附件」chip（一次成功发送才算消耗）
  if (item?.reuse) updateDrafts((d) => clearReuseOnAck(d, item.convId, item.attachment?.id));
  setState((cur) => {
    const merged = { ...cur, ...applySyncMessages(cur, real.conversation_id, [real]) };
    return { ...merged, messages: { ...merged.messages,
      [real.conversation_id]: resolveAck(merged.messages[real.conversation_id] ?? [], clientMsgId, real),
    } };
  });
  trackIncomingSeq(real.conversation_id, real.seq);
}

function scheduleAckWatchdog() {
  if (stopped || ackWatchdog != null) return;
  ackWatchdog = window.setTimeout(() => {
    ackWatchdog = null;
    checkOutboxAcks();
    scheduleAckWatchdog();
  }, ACK_WATCHDOG_MS);
}

/**
 * P0-3：「发送中」兜底。flushOutbox 跳过 sentOn===当前连接 的条目——消息已发出
 * 而 message_ack 单帧丢失（服务端回包通道满、半开连接等）时，没有重连触发就
 * 永远卡在「发送中」。看门狗两路兜底：
 * 1. 真身已从广播/sync 回显（同会话、本人、同正文/附件）→ 按 ack 语义收敛；
 * 2. 在当前存活连接上发出且超时未确认 → 重连，按原幂等键重放。
 * 死连接上的待发项交给重连 flushOutbox 重放，不在此判失败；
 * 从没上过 socket 的离线待发项同理（无 sentAt，直接跳过）。
 */
function checkOutboxAcks() {
  const consumed = new Set<string>(); // 本轮已认领的真身：一条真身只收敛一条待发
  for (const item of [...outbox]) {
    if (item.sentAt == null) continue;
    const echoed = findEchoedMessage(item, consumed);
    if (echoed) {
      consumed.add(echoed.id);
      resolveSend(item.clientMsgId, echoed);
    } else if (
      !item.error &&
      ws != null &&
      item.sentOn === ws &&
      ws.readyState === WebSocket.OPEN &&
      Date.now() - item.sentAt >= ACK_TIMEOUT_MS
    ) {
      reconnectNow();
    }
  }
}

/** 在会话缓存里找「这条已发消息」的服务端真身：ack 丢了但广播/sync 已送达。
 * 匹配同会话、本人所发、落库正文（服务端 trim + 附件占位符与乐观文案同规则）、
 * 附件同名；本轮已被认领的真身不再复用。 */
function findEchoedMessage(item: Outgoing, consumed: Set<string>): MessageView | undefined {
  const me = getState().me;
  if (!me) return undefined;
  const content = item.optimistic.content.trim();
  return (getState().messages[item.convId] ?? []).find(
    (m) =>
      !m.id.startsWith("client:") &&
      !consumed.has(m.id) &&
      m.sender_id === me.id &&
      m.content === content &&
      (m.attachment?.file_name ?? null) === (item.attachment?.file_name ?? null),
  );
}

function takeOutbox(clientMsgId: string | null): Outgoing | undefined {
  if (!clientMsgId) return;
  const i = outbox.findIndex((item) => item.clientMsgId === clientMsgId);
  if (i < 0) return;
  const [item] = outbox.splice(i, 1);
  saveOutbox();
  return item;
}

function setOptimisticError(item: Outgoing, sendError: string | undefined) {
  const id = `client:${item.clientMsgId}`;
  setState((s) => ({
    messages: {
      ...s.messages,
      [item.convId]: (s.messages[item.convId] ?? []).map((m) =>
        m.id === id ? { ...m, send_error: sendError } : m,
      ),
    },
  }));
}

function markFailed(clientMsgId: string, reason: string) {
  const item = outbox.find((x) => x.clientMsgId === clientMsgId);
  if (!item) return;
  item.error = reason;
  item.sentOn = undefined;
  saveOutbox();
  setOptimisticError(item, reason);
}

/** 失败气泡「重试」：同一 client_msg_id 再发（失败的那次没落库，服务端不会重复） */
export function retrySend(clientMsgId: string) {
  const item = outbox.find((x) => x.clientMsgId === clientMsgId);
  if (!item) return;
  item.error = undefined;
  saveOutbox();
  setOptimisticError(item, undefined);
  flushOutbox();
}

/** 失败气泡「删除」：放弃这条 */
export function discardSend(clientMsgId: string) {
  const item = takeOutbox(clientMsgId);
  if (item) setState((s) => dropOptimistic(s, item.convId, clientMsgId));
}

function flushOutbox() {
  if (wsGen !== session() || ws?.readyState !== WebSocket.OPEN) return;
  for (const item of outbox) {
    if (item.sentOn === ws || item.error) continue;
    ws.send(
      JSON.stringify({
        type: "send_message",
        conversation_id: item.convId,
        client_msg_id: item.clientMsgId,
        content: item.content,
        ...(item.reuse
          ? { reuse_attachment_id: item.attachment?.id } // MM2：撤回件复用（服务端校验上传者/TTL）
          : { attachment_id: item.attachment?.id }),
        reply_to_id: item.replyToId,
        ...(item.kind ? { kind: item.kind } : {}),
      }),
    );
    item.sentOn = ws;
    item.sentAt = Date.now();
  }
}

// ---------- 发送 ----------
export function wsSend(obj: unknown) {
  if (wsGen === session() && ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

export function sendChat(
  convId: string,
  content: string,
  attachment?: AttachmentView,
  replyTo?: MessageView,
  reuse?: boolean,
): string | undefined {
  const clientMsgId = crypto.randomUUID();
  const me = getState().me;
  if (!me) return undefined;
  // MM2 fix3：复用发送的乐观附件打上 recalled 标——id 是撤回保留原件，
  // 公共 /v1/files/{id} 已 404，气泡只能渲染 chip 已取回的 object URL。
  const optimisticAtt =
    attachment && reuse ? { ...attachment, recalled: true } : attachment;
  const optimistic: MessageView = {
    id: `client:${clientMsgId}`,
    conversation_id: convId,
    seq: Number.MAX_SAFE_INTEGER - Math.floor(Math.random() * 1e6),
    sender_id: me.id,
    sender_username: me.username,
    sender_name: me.display_name,
    content:
      content ||
      (optimisticAtt
        ? optimisticAtt.duration_ms != null
          ? "[语音]"
          : optimisticAtt.mime.startsWith("image/")
            ? "[图片]"
            : `[文件] ${optimisticAtt.file_name}`
        : ""),
    kind: optimisticAtt
      ? optimisticAtt.duration_ms != null
        ? "audio"
        : optimisticAtt.mime.startsWith("image/")
          ? "image"
          : "file"
      : "text",
    attachment: optimisticAtt ?? null,
    mentions: [],
    reply_to_id: replyTo?.id ?? null,
    reply_to_preview: replyTo ? replyPreview(replyTo) : null,
    edited_at: null,
    edit_version: 0,
    deleted_at: null,
    reactions: [],
    created_at: new Date().toISOString(),
  };
  outbox.push({
    convId,
    clientMsgId,
    content,
    attachment,
    reuse,
    replyToId: replyTo?.id,
    optimistic,
  });
  saveOutbox();
  setState((s) => ({
    messages: {
      ...s.messages,
      [convId]: [...(s.messages[convId] ?? []), optimistic],
    },
  }));
  flushOutbox();
  return clientMsgId;
}

/** 发送 Meeting Card（kind=meeting）；content 为可加入的绝对 URL。遗留纯文本会议链接仍走 sendChat。 */
export function sendMeeting(convId: string, url: string): string | undefined {
  const clientMsgId = crypto.randomUUID();
  const me = getState().me;
  if (!me) return undefined;
  const optimistic: MessageView = {
    id: `client:${clientMsgId}`,
    conversation_id: convId,
    seq: Number.MAX_SAFE_INTEGER - Math.floor(Math.random() * 1e6),
    sender_id: me.id,
    sender_username: me.username,
    sender_name: me.display_name,
    content: url,
    kind: "meeting",
    attachment: null,
    mentions: [],
    reply_to_id: null,
    reply_to_preview: null,
    edited_at: null,
    edit_version: 0,
    deleted_at: null,
    reactions: [],
    created_at: new Date().toISOString(),
  };
  outbox.push({
    convId,
    clientMsgId,
    content: url,
    kind: "meeting",
    optimistic,
  });
  saveOutbox();
  setState((s) => ({
    messages: {
      ...s.messages,
      [convId]: [...(s.messages[convId] ?? []), optimistic],
    },
  }));
  flushOutbox();
  return clientMsgId;
}

export function sendTyping(convId: string) {
  wsSend({ type: "typing", conversation_id: convId });
}

export function sendRead(convId: string, seq: number) {
  wsSend({ type: "read", conversation_id: convId, seq });
}

/**
 * 退出群聊：成功后本地立刻摘除该会话（不回拉 detail——已非成员只会 403）。
 * 「已非成员」错误（重复退出/别处已退）同样收口，返回 "already_left" 让界面给中文提示。
 */
export async function leaveGroup(convId: string): Promise<"left" | "already_left"> {
  let already = false;
  try {
    await convs.leave(convId);
  } catch (e) {
    if (!(e instanceof ApiError && e.code === "not_member")) throw e;
    already = true;
  }
  // 已退出的会话不再参与 seq 同步/回刷：水位与在途 sync 一并作废
  syncedSeq.delete(convId);
  readSeq.delete(convId);
  syncInflight.delete(convId);
  setState((s) => applyGroupLeft(s, convId));
  return already ? "already_left" : "left";
}

/**
 * 打开会话：切到聊天页、上报已读、本地清未读；
 * 归档会话被主动打开时即取消本人归档（§8.3，新普通消息不自动恢复）。
 */
export function openConversation(id: string) {
  const s = getState();
  const c = s.conversations.find((x) => x.id === id);
  setState({ activeConv: id, tab: "chats" });
  if (c && c.unread_count > 0 && c.last_seq > 0) {
    sendRead(id, c.last_seq);
    setState((cur) => ({
      conversations: cur.conversations.map((x) =>
        x.id === id ? { ...x, unread_count: 0, has_mention: false } : x,
      ),
    }));
  }
}
