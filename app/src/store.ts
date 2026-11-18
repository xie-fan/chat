import { useSyncExternalStore } from "react";
import { loadSession, serverScope } from "./api";
import { writeDrafts, type DraftEntry, type DraftReuse } from "./drafts";
import { cacheKey, getRecent, touchRecent, writeLocalCache } from "./localCache";
import { recordLabel } from "./ui/chat/chatRecord";
import type {
  AcpAgent,
  AcpBootstrap,
  AcpHost,
  AcpSession,
  AcpSettings,
  AcpTurn,
  AcpWorkspace,
  ConversationPref,
  ConversationView,
  FriendView,
  MessageView,
  RequestsView,
  UserPublic,
} from "./types";

/** 应用内新消息提示（最小通知出口；点击打开会话） */
export interface Toast {
  id: string;
  /** 点击后打开的会话；下载结果等非会话 toast 为空 */
  convId?: string;
  title: string;
  text: string;
  ts: number;
  /** MM4：下载成功 toast 点击「在文件夹中显示」（仅桌面端设置） */
  filePath?: string;
  /** MF2：失败操作的重试入口（如批量转发网络错误后同 client_op_id 重发） */
  action?: { label: string; run: () => void };
}

export interface AppState {
  me: UserPublic | null;
  conversations: ConversationView[];
  /** conv_id -> 按 seq 升序的消息（含乐观未确认消息，pending 用 client: 前缀 id） */
  messages: Record<string, MessageView[]>;
  hasMore: Record<string, boolean>;
  /** conv_id -> 初始历史页是否已加载（P1-11：实时缓存非空也不能跳过历史拉取） */
  historyLoaded: Record<string, boolean>;
  activeConv: string | null;
  online: Record<string, boolean>;
  /** conv_id -> { user, ts } 正在输入 */
  typing: Record<string, { user: UserPublic; ts: number }>;
  friends: FriendView[];
  requests: RequestsView;
  wsReady: boolean;
  tab: "chats" | "contacts" | "agents" | "me" | "admin";
  /** composer 引用卡片：正在回复的目标消息（null=普通发送） */
  replyingTo: MessageView | null;
  /** composer 编辑态：正在编辑的消息（expected_version 取其当前 edit_version） */
  editingMessage: MessageView | null;
  /** `${messageId}:${emoji}` -> 提交中（reaction 按钮 busy 态） */
  reactionPending: Record<string, boolean>;
  /** 应用内通知栈（新消息提示，自动过期） */
  toasts: Toast[];
  /** conv_id -> 输入框草稿（V2 P1-3；持久化到 localStorage，按用户隔离。MM2 起含复用附件 chip） */
  drafts: Record<string, DraftEntry>;
  /** user_id -> 已知最新头像版本。只增不减；404 回落时把 has 记为 false。 */
  avatars: Record<string, AvatarInfo>;
  /** 全局搜索跳转目标（V2 P3-1）：打开会话后由 ChatView 消费并清空 */
  pendingJump: { convId: string; messageId: string; seq: number } | null;
  /** ACP Agent 工作区状态 */
  acpSettings: AcpSettings;
  acpHosts: AcpHost[];
  acpAgents: AcpAgent[];
  acpWorkspaces: AcpWorkspace[];
  acpSessions: AcpSession[];
  acpLoaded: boolean;
  activeAcpHostId: string | null;
  activeAcpWorkspaceId: string | null;
  activeAcpSessionId: string | null;
}

/** 头像缓存条目：版本较大的那组才有效。 */
export interface AvatarInfo {
  version: number;
  has: boolean;
}

const emptySessionData = () => ({
  me: null as UserPublic | null,
  conversations: [] as ConversationView[],
  messages: {} as Record<string, MessageView[]>,
  hasMore: {} as Record<string, boolean>,
  historyLoaded: {} as Record<string, boolean>,
  activeConv: null as string | null,
  online: {} as Record<string, boolean>,
  typing: {} as Record<string, { user: UserPublic; ts: number }>,
  friends: [] as FriendView[],
  requests: { incoming: [], outgoing: [] } as RequestsView,
  wsReady: false,
  tab: "chats" as AppState["tab"],
  replyingTo: null as MessageView | null,
  editingMessage: null as MessageView | null,
  reactionPending: {} as Record<string, boolean>,
  toasts: [] as Toast[],
  drafts: {} as Record<string, DraftEntry>,
  avatars: {} as Record<string, AvatarInfo>,
  pendingJump: null as AppState["pendingJump"],
  acpSettings: { default_permission: "auto_read" } as AcpSettings,
  acpHosts: [] as AcpHost[],
  acpAgents: [] as AcpAgent[],
  acpWorkspaces: [] as AcpWorkspace[],
  acpSessions: [] as AcpSession[],
  acpLoaded: false,
  activeAcpHostId: null as string | null,
  activeAcpWorkspaceId: null as string | null,
  activeAcpSessionId: null as string | null,
});

export function resetSessionState(): Partial<AppState> {
  return emptySessionData();
}

let state: AppState = emptySessionData();

const listeners = new Set<() => void>();

export function getState() {
  return state;
}

export function setState(patch: Partial<AppState> | ((s: AppState) => Partial<AppState>)) {
  const p = typeof patch === "function" ? patch(state) : patch;
  const me = p.me === undefined ? state.me : p.me;
  const avatars = p.avatars ?? state.avatars;
  const absorbed = me === state.me ? avatars : absorbAvatarUsers(avatars, [me]);
  state = { ...state, ...p, ...(absorbed === avatars ? {} : { avatars: absorbed }) };
  listeners.forEach((l) => l());
  // 本地快照只关心会话数据变化；typing/online/toast 这类高频变更不落盘
  if (p.activeConv) touchRecent(p.activeConv);
  if (
    p.me !== undefined ||
    p.conversations !== undefined ||
    p.messages !== undefined ||
    p.activeConv !== undefined
  ) {
    scheduleCachePersist();
  }
}

// ---------- 本地快照（T1：离线启动回放用） ----------

let cacheTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleCachePersist() {
  if (cacheTimer != null) clearTimeout(cacheTimer);
  cacheTimer = setTimeout(() => {
    cacheTimer = null;
    const s = state;
    // 凭据已清（登出/被踢/换号）就不得再写：防抖窗口里清过的快照不能被救活
    if (!s.me || typeof localStorage === "undefined" || loadSession()?.user.id !== s.me.id) return;
    // 已打开过的会话排前（touchRecent 顺序），仅被别的路径加载过消息的排后——丢消息时也先丢后者
    const recent = getRecent().filter((id) => s.messages[id]?.length);
    const ids = [...recent, ...Object.keys(s.messages).filter((id) => !recent.includes(id))];
    writeLocalCache(localStorage, cacheKey(serverScope(), s.me.id), {
      conversations: s.conversations,
      recent: ids,
      messages: Object.fromEntries(ids.map((id) => [id, s.messages[id]])),
    });
  }, 500);
}

export function useApp(): AppState {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => state,
    () => state,
  );
}

// ---------- 领域辅助 ----------

export function convTitle(c: ConversationView, meId?: string): string {
  if (c.kind === "group") return c.name ?? `群聊(${c.member_count})`;
  // V2 P2-4：self 会话 name 恒 NULL、peer 恒空，标题固定
  if (c.kind === "self") return "文件传输助手";
  return c.peer?.display_name ?? c.name ?? "会话";
}

/** 「这条消息是不是我发的」：气泡在右、不计未读、不通知、可编辑。 */
export function isOwnMessage(m: MessageView, meId?: string | null): boolean {
  return !!meId && m.sender_id === meId;
}

/** 把一批 UserPublic 合并进头像表，只接受更大的版本。 */
export function absorbAvatarUsers(
  avatars: Record<string, AvatarInfo>,
  users: Array<UserPublic | null | undefined>,
): Record<string, AvatarInfo> {
  let next = avatars;
  for (const user of users) {
    if (!user?.id) continue;
    const merged = mergeAvatar(next[user.id], {
      version: user.avatar_version ?? 0,
      has: !!user.has_avatar,
    });
    if (next[user.id]?.version === merged.version && next[user.id]?.has === merged.has) continue;
    if (next === avatars) next = { ...avatars };
    next[user.id] = merged;
  }
  return next;
}

/** 只接受更大的版本；同版本允许更新 has（例如同一次刷新把有无写实）。 */
export function mergeAvatar(
  current: AvatarInfo | undefined,
  incoming: AvatarInfo,
): AvatarInfo {
  if (!current || incoming.version > current.version) return incoming;
  if (incoming.version === current.version) return { version: current.version, has: incoming.has };
  return current;
}

/** 消息行：消息自带版本与 store 里的值取较大的那组。 */
export function senderAvatar(
  stored: AvatarInfo | undefined,
  version: number | undefined,
  has: boolean | undefined,
): AvatarInfo {
  return mergeAvatar(stored, { version: version ?? 0, has: !!has });
}

/** 图片 404：该版本记为无头像，避免反复请求。 */
export function noteAvatarMiss(userId: string, version: number): void {
  setState((s) => ({
    avatars: { ...s.avatars, [userId]: mergeAvatar(s.avatars[userId], { version, has: false }) },
  }));
}

export function convAvatarText(c: ConversationView): string {
  const t = convTitle(c);
  return (t[0] ?? "?").toUpperCase();
}

/**
 * 消息按 seq 插入（乐观消息 client:xxx 排最后），去重。
 * 统一 id+version 合并规则：incoming.edit_version（缺省按 0）低于缓存版本时丢弃，
 * 保证迟到的旧生命周期事件不覆盖较新的编辑/删除/reaction 状态。
 */
function selectMessage(local: MessageView, incoming: MessageView): MessageView {
  const delta = (incoming.edit_version ?? 0) - (local.edit_version ?? 0);
  if (delta < 0) return local;
  if (delta > 0) return incoming;
  return {
    ...local,
    reply_to_preview: local.deleted_at || local.reply_to_preview?.deleted
      ? local.reply_to_preview
      : incoming.reply_to_preview ?? local.reply_to_preview,
    ...(local.poll && incoming.poll?.expired ? { poll: { ...local.poll, expired: true } } : {}),
  };
}

export function replyPreview(m: MessageView) {
  return {
    message_id: m.id,
    seq: m.seq,
    sender_name: m.sender_name,
    // MF2：回复聊天记录引用「[聊天记录] {title}」首行（与服务端 preview 同口径），不带逐行摘要
    excerpt: m.deleted_at ? "" : m.kind === "chat_record"
      ? recordLabel(m)
      : m.attachment
      ? m.kind === "image"
        ? "[图片]"
        : m.kind === "audio" || m.attachment.duration_ms != null
          ? "[语音]"
          : `[文件] ${m.attachment.file_name}`
      : Array.from(m.content).slice(0, 120).join(""),
    kind: m.kind,
    deleted: !!m.deleted_at,
    // MM2：撤回者名随预览走，引用块据此区分「原消息已撤回/已删除」
    recalled_by_name: m.recalled_by_name ?? null,
  };
}

function selectLast(local: MessageView | null, incoming: MessageView | null): MessageView | null {
  if (!local) return incoming;
  if (!incoming) return local;
  if (local.id === incoming.id) return selectMessage(local, incoming);
  return incoming.seq > local.seq ? incoming : local;
}

export function upsertMessage(list: MessageView[], m: MessageView): MessageView[] {
  const idx = list.findIndex((x) => x.id === m.id);
  if (idx >= 0) {
    const selected = selectMessage(list[idx], m);
    if (selected === list[idx]) return list;
    const next = [...list];
    next[idx] = selected;
    return next;
  }
  const next = [...list, m];
  next.sort((a, b) => a.seq - b.seq);
  return next;
}

/**
 * 从 after 起向后逐页拉，直到拉到 stop 前一条或没有更多。
 * 跳转到缓存之外的旧消息时用它补齐到缓存最早一条：缓存必须连续（hasMore/loadMore 只看最小 seq）。
 * ponytail: 远距离跳转会逐页（200 条/页）补齐整段；历史到万级再改成独立窗口 + 向下加载。
 */
export async function fetchForward(
  page: (after: number) => Promise<{ messages: MessageView[]; has_more: boolean }>,
  after: number,
  stop: number,
): Promise<MessageView[]> {
  const out: MessageView[] = [];
  for (;;) {
    const p = await page(after);
    out.push(...p.messages);
    const last = p.messages[p.messages.length - 1];
    if (!p.has_more || !last || last.seq >= stop - 1) return out;
    after = last.seq;
  }
}

export function prependMessages(list: MessageView[], older: MessageView[]): MessageView[] {
  const seen = new Set(list.map((m) => m.id));
  return [...older.filter((m) => !seen.has(m.id)), ...list];
}

// ---------- 会话偏好 ----------

export function isPinned(c: ConversationView): boolean {
  return !!c.preference?.pinned_at;
}

export function isArchived(c: ConversationView): boolean {
  return !!c.preference?.archived_at;
}

/** 静音是否生效：永久静音或未过期的定时静音（过期时间按本地时钟判断） */
export function isMuted(c: ConversationView): boolean {
  const p = c.preference;
  if (!p) return false;
  if (p.mute_forever) return true;
  return !!p.muted_until && Date.parse(p.muted_until) > Date.now();
}

function selectPreference(
  local: ConversationView["preference"],
  incoming: ConversationView["preference"],
): ConversationView["preference"] {
  if (!local?.updated_at) return incoming ?? local;
  if (!incoming?.updated_at) return local;
  return Date.parse(incoming.updated_at) < Date.parse(local.updated_at) ? local : incoming;
}

/** 偏好变更合并入口（preference_updated 帧 / PUT 响应共用）：更新字段并重排列表 */
export function applyPrefUpdate(
  s: AppState,
  conversationId: string,
  preference: ConversationPref,
): Partial<AppState> {
  if (!s.conversations.some((c) => c.id === conversationId)) return {};
  return {
    conversations: sortConversations(
      s.conversations.map((c) =>
        c.id === conversationId ? { ...c, preference: selectPreference(c.preference, preference) } : c,
      ),
    ),
  };
}

/** 这个会话此刻是否正打开在会话页上 */
export function isViewing(s: AppState, id: string): boolean {
  return s.activeConv === id && s.tab === "chats";
}

/**
 * 新消息是否触发应用内提示：非本人所发、非当前打开的会话；
 * 静音会话抑制提示，但 @本人 可突破静音（§8.3）。
 */
export function shouldNotify(s: AppState, m: MessageView): boolean {
  if (s.me == null || isOwnMessage(m, s.me.id)) return false;
  if (isViewing(s, m.conversation_id)) return false;
  const conv = s.conversations.find((c) => c.id === m.conversation_id);
  if (conv && isMuted(conv) && !m.mentions.includes(s.me.id)) return false;
  return true;
}

/** timeoutMs 传 0 = 常驻不自动消失（MM4「下载中…」在途提示，由结果 toast 接替） */
export function pushToast(t: Omit<Toast, "id" | "ts">, opts?: { timeoutMs?: number }): string {
  const id = crypto.randomUUID();
  setState((s) => ({
    toasts: [...s.toasts.slice(-3), { ...t, id, ts: Date.now() }],
  }));
  const ms = opts?.timeoutMs ?? 5000;
  if (ms > 0) setTimeout(() => dismissToast(id), ms);
  return id;
}

/** MM4：更新已弹出的 toast（下载中 → 结果）；结果重新计 5s 消失 */
export function updateToast(id: string, patch: Partial<Omit<Toast, "id" | "ts">>) {
  setState((s) => ({
    toasts: s.toasts.map((t) => (t.id === id ? { ...t, ...patch } : t)),
  }));
  setTimeout(() => dismissToast(id), 5000);
}

export function dismissToast(id: string) {
  setState((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
}

// ---------- 会话草稿（V2 P1-3） ----------

let draftTimer: ReturnType<typeof setTimeout> | null = null;

/** 草稿表更新统一入口：state 立即生效 + localStorage 防抖 300ms 落盘 */
export function updateDrafts(fn: (d: Record<string, DraftEntry>) => Record<string, DraftEntry>) {
  setState((s) => ({ drafts: fn(s.drafts) }));
  if (draftTimer != null) clearTimeout(draftTimer);
  draftTimer = setTimeout(flushDrafts, 300);
}

/** 更新某会话草稿文本；空串清文本但保留复用附件 chip（MM2：清文字不摘 chip） */
export function setDraft(convId: string, text: string) {
  updateDrafts((d) => {
    const next = { ...d };
    const cur = next[convId];
    if (text) next[convId] = { text, ...(cur?.reuse ? { reuse: cur.reuse } : {}) };
    else if (cur?.reuse) next[convId] = { text: "", reuse: cur.reuse };
    else delete next[convId];
    return next;
  });
}

/** 挂上/摘掉某会话的「复用附件」chip；chip 独立存在（文字可为空） */
export function setDraftReuse(convId: string, reuse: DraftReuse | null) {
  updateDrafts((d) => {
    const next = { ...d };
    const cur = next[convId] ?? { text: "" };
    if (reuse) next[convId] = { ...cur, reuse };
    else if (cur.text) next[convId] = { text: cur.text };
    else delete next[convId];
    return next;
  });
}

/** 立即把当前草稿表落盘（pagehide 时调用，堵住防抖窗口丢草稿） */
export function flushDrafts() {
  if (draftTimer != null) {
    clearTimeout(draftTimer);
    draftTimer = null;
  }
  const { me, drafts } = getState();
  if (me && typeof localStorage !== "undefined") {
    writeDrafts(localStorage, serverScope(), me.id, drafts);
  }
}

/** 置顶优先：置顶块内按 pinned_at DESC，其余按原活动时间 DESC，id 兜底保序 */
export function sortConversations(list: ConversationView[]): ConversationView[] {
  const act = (c: ConversationView) => c.last_message?.created_at ?? c.created_at;
  return [...list].sort((a, b) => {
    const ap = a.preference?.pinned_at ?? null;
    const bp = b.preference?.pinned_at ?? null;
    if (ap == null && bp != null) return 1;
    if (ap != null && bp == null) return -1;
    if (ap != null && bp != null && ap !== bp) return bp.localeCompare(ap);
    return act(b).localeCompare(act(a)) || a.id.localeCompare(b.id);
  });
}

/** 目录刷新和实时消息可能交错。本地 seq 更新的预览留下来，远端没有的会话以远端为准（退群会消失）。 */
export function mergeConversationLists(
  local: ConversationView[],
  remote: ConversationView[],
): ConversationView[] {
  const localById = new Map(local.map((c) => [c.id, c]));
  const merged = remote.map((rc) => {
    const lc = localById.get(rc.id);
    if (!lc) return rc;
    rc = { ...rc, preference: selectPreference(lc.preference, rc.preference) };
    if (!lc.last_message) return rc;
    if (lc.last_message.seq > (rc.last_message?.seq ?? 0)) {
      return {
        ...rc,
        last_message: lc.last_message,
        last_seq: Math.max(lc.last_seq, rc.last_seq),
        unread_count: lc.unread_count,
        has_mention: lc.has_mention || rc.has_mention,
      };
    }
    return { ...rc, last_message: selectLast(lc.last_message, rc.last_message) };
  });
  return sortConversations(merged);
}

/**
 * 实时消息。同一 id 再投递一次不重复加未读。
 * foreground=false（窗口在后台）时打开着的会话也照常计未读，回到前台再补报已读。
 */
export function applyLiveMessage(s: AppState, m: MessageView, foreground = true): Partial<AppState> {
  const list = s.messages[m.conversation_id] ?? [];
  const exists = list.some((x) => x.id === m.id) || s.conversations.some((c) => c.last_message?.id === m.id);
  const refreshed = applyRefreshed(s, m.conversation_id, [m]);
  const isActive = isViewing(s, m.conversation_id) && foreground;
  const mine = isOwnMessage(m, s.me?.id);
  const { [m.conversation_id]: _typing, ...typing } = s.typing;
  return {
    ...refreshed,
    conversations: sortConversations(
      (refreshed.conversations ?? s.conversations).map((c) =>
        c.id === m.conversation_id
          ? {
              ...c,
              last_message: selectLast(c.last_message, refreshed.messages?.[m.conversation_id].find((x) => x.id === m.id) ?? m),
              last_seq: Math.max(c.last_seq, m.seq),
              unread_count: exists || mine || isActive ? c.unread_count : c.unread_count + 1,
              has_mention:
                c.has_mention ||
                (!exists && !mine && !isActive && s.me != null && m.mentions.includes(s.me.id)),
            }
          : c,
      ),
    ),
    typing,
  };
}

/** 断线补齐。未读以目录刷新为准，这里只补线程和更新的预览。 */
export function applySyncMessages(
  s: AppState,
  conversationId: string,
  incoming: MessageView[],
): Partial<AppState> {
  if (incoming.length === 0) return {};
  const refreshed = applyRefreshed(s, conversationId, incoming);
  const { [conversationId]: _typing, ...typing } = s.typing;
  return {
    ...refreshed,
    typing,
    conversations: sortConversations(
      (refreshed.conversations ?? s.conversations).map((c) => {
        if (c.id !== conversationId) return c;
        let last = c.last_message;
        let lastSeq = c.last_seq;
        for (const m of refreshed.messages?.[conversationId] ?? incoming) {
          if (m.id.startsWith("client:")) continue;
          last = selectLast(last, m);
          if (m.seq > lastSeq) lastSeq = m.seq;
        }
        return { ...c, last_message: last, last_seq: lastSeq };
      }),
    ),
  };
}

export function dropOptimistic(s: AppState, convId: string, clientMsgId: string): Partial<AppState> {
  const id = `client:${clientMsgId}`;
  return {
    messages: {
      ...s.messages,
      [convId]: (s.messages[convId] ?? []).filter((m) => m.id !== id),
    },
  };
}

/** 生命周期广播（编辑/删除/reaction）唯一合并入口：服务端所有 mutation 帧
 * 均携带完整 MessageView（reactions 已按本接收者个性化），按 id+edit_version
 * 整视图合并——版本更高的视图天然包含低版本的全部变更，迟到的旧帧直接丢弃，
 * 不存在部分字段帧互相吞更新的问题。会话卡片上的 last_message 同步换新版。 */
export function applyUpdatedMessage(s: AppState, m: MessageView): Partial<AppState> {
  return applyRefreshed(s, m.conversation_id, [m]);
}

/** 重连批量刷新：回包按同一 id+version 规则合并；last_message 引用同 id 时换新 */
export function applyRefreshed(
  s: AppState,
  convId: string,
  incoming: MessageView[],
): Partial<AppState> {
  if (incoming.length === 0) return {};
  const cached = s.messages[convId] ?? [];
  const byId = new Map(cached.map((m) => [m.id, m]));
  const merge = (m: MessageView) => {
    const old = byId.get(m.id);
    byId.set(m.id, old ? selectMessage(old, m) : m);
  };
  for (const c of s.conversations) {
    if (c.id === convId && c.last_message) merge(c.last_message);
  }
  incoming.filter((m) => m.conversation_id === convId).forEach(merge);
  const withReply = (m: MessageView): MessageView => {
    const source = m.reply_to_id && byId.get(m.reply_to_id);
    return source && !m.deleted_at && !(m.reply_to_preview?.deleted && !source.deleted_at)
      ? { ...m, reply_to_preview: replyPreview(source) }
      : m;
  };
  const ids = new Set([...cached.map((m) => m.id), ...incoming.filter((m) => m.conversation_id === convId).map((m) => m.id)]);
  const list = [...ids].map((id) => withReply(byId.get(id)!)).sort((a, b) => a.seq - b.seq);
  return {
    messages: { ...s.messages, [convId]: list },
    conversations: s.conversations.map((c) => c.id === convId && c.last_message
      ? { ...c, last_message: withReply(byId.get(c.last_message.id)!) }
      : c),
    replyingTo: s.replyingTo?.conversation_id === convId
      ? withReply(byId.get(s.replyingTo.id) ?? s.replyingTo)
      : s.replyingTo,
  };
}

/**
 * 退群/解散后的本地收口：列表摘掉该会话，正在看它则切走，本地消息缓存一并丢弃。
 * 不能再回拉 detail——已非成员只会 403，会把整次刷新拖垮。
 */
export function applyGroupLeft(s: AppState, convId: string): Partial<AppState> {
  return {
    ...dropConversationCache(s, convId),
    conversations: s.conversations.filter((c) => c.id !== convId),
    ...(s.activeConv === convId ? { activeConv: null } : {}),
    ...(s.pendingJump?.convId === convId ? { pendingJump: null } : {}),
  };
}

/** 会话不再可访问（refresh 403/404）：丢弃本地消息缓存与滚动状态 */
export function dropConversationCache(s: AppState, convId: string): Partial<AppState> {
  const messages = { ...s.messages };
  delete messages[convId];
  const hasMore = { ...s.hasMore };
  delete hasMore[convId];
  const historyLoaded = { ...s.historyLoaded };
  delete historyLoaded[convId];
  const typing = { ...s.typing };
  delete typing[convId];
  return { messages, hasMore, historyLoaded, typing };
}

/** ack 到达：用服务端消息替换乐观副本（message 回显可能先于 ack，需按 id 去重） */
export function resolveAck(
  list: MessageView[],
  clientMsgId: string | null,
  real: MessageView,
): MessageView[] {
  return upsertMessage(list.filter((m) => m.id !== `client:${clientMsgId}`), real);
}

// ---------- ACP Workspace 状态归并与选择器 ----------

export function pendingAcpApprovalsCount(sessions: AcpSession[]): number {
  let n = 0;
  for (const s of sessions) {
    for (const t of s.turns ?? []) {
      if (t.role === "permission" && t.status === "pending") n++;
    }
  }
  return n;
}

function reconcileAcpSelection(
  cur: Pick<
    AppState,
    "acpHosts" | "acpWorkspaces" | "acpSessions" | "activeAcpHostId" | "activeAcpWorkspaceId" | "activeAcpSessionId"
  >,
): Pick<AppState, "activeAcpHostId" | "activeAcpWorkspaceId" | "activeAcpSessionId"> {
  const hostId =
    cur.acpHosts.find((h) => h.id === cur.activeAcpHostId)?.id ??
    cur.acpHosts.find((h) => h.online)?.id ??
    cur.acpHosts[0]?.id ??
    null;

  const hostWorkspaces = hostId
    ? cur.acpWorkspaces.filter((w) => w.host_id === hostId)
    : cur.acpWorkspaces;

  const currentSess = cur.acpSessions.find((s) => s.id === cur.activeAcpSessionId) ?? null;
  let wsId: string | null =
    hostWorkspaces.find((w) => w.id === cur.activeAcpWorkspaceId)?.id ??
    (currentSess ? hostWorkspaces.find((w) => w.id === currentSess.workspace_id)?.id : undefined) ??
    hostWorkspaces[0]?.id ??
    null;

  const sessInWs = wsId ? cur.acpSessions.filter((s) => s.workspace_id === wsId) : [];
  let sessId: string | null =
    sessInWs.find((s) => s.id === cur.activeAcpSessionId)?.id ?? sessInWs[0]?.id ?? null;

  // 若选中的会话不属于当前 host 的第一个目录，优先保住同 host 下有会话的目录
  if (!sessId && hostWorkspaces.length > 0) {
    const withSess = hostWorkspaces.find((w) =>
      cur.acpSessions.some((s) => s.workspace_id === w.id),
    );
    if (withSess) {
      wsId = withSess.id;
      sessId = cur.acpSessions.find((s) => s.workspace_id === wsId)?.id ?? null;
    }
  }

  return {
    activeAcpHostId: hostId,
    activeAcpWorkspaceId: wsId,
    activeAcpSessionId: sessId,
  };
}

export function applyAcpBootstrap(s: AppState, bs: AcpBootstrap): Partial<AppState> {
  const nextBase = {
    acpSettings: bs.settings,
    acpHosts: bs.hosts,
    acpAgents: bs.agents,
    acpWorkspaces: bs.workspaces,
    acpSessions: bs.sessions,
    acpLoaded: true,
    activeAcpHostId: s.activeAcpHostId,
    activeAcpWorkspaceId: s.activeAcpWorkspaceId,
    activeAcpSessionId: s.activeAcpSessionId,
  };
  return {
    ...nextBase,
    ...reconcileAcpSelection(nextBase),
  };
}

function upsertById<T extends { id: string }>(list: T[], item: T, append = true): T[] {
  const idx = list.findIndex((x) => x.id === item.id);
  if (idx >= 0) {
    const next = [...list];
    next[idx] = item;
    return next;
  }
  return append ? [...list, item] : [item, ...list];
}

export function applyAcpEvent(
  s: AppState,
  event: string,
  raw: unknown,
): Partial<AppState> {
  const data = (raw ?? {}) as Record<string, unknown>;
  switch (event) {
    case "settings_updated": {
      const settings = data.settings as AcpSettings | undefined;
      return settings ? { acpSettings: settings } : {};
    }
    case "host_upserted": {
      const host = data.host as AcpHost | undefined;
      if (!host) return {};
      const acpHosts = upsertById(s.acpHosts, host);
      return {
        acpHosts,
        ...reconcileAcpSelection({ ...s, acpHosts }),
      };
    }
    case "host_deleted": {
      const hostId = String(data.host_id ?? "");
      const acpHosts = s.acpHosts.filter((h) => h.id !== hostId);
      const removedWsIds = new Set(
        s.acpWorkspaces.filter((w) => w.host_id === hostId).map((w) => w.id),
      );
      const acpWorkspaces = s.acpWorkspaces.filter((w) => w.host_id !== hostId);
      const acpAgents = s.acpAgents.filter((a) => a.host_id !== hostId);
      const acpSessions = s.acpSessions.filter((x) => !removedWsIds.has(x.workspace_id));
      return {
        acpHosts,
        acpAgents,
        acpWorkspaces,
        acpSessions,
        ...reconcileAcpSelection({
          ...s,
          acpHosts,
          acpWorkspaces,
          acpSessions,
        }),
      };
    }
    case "agent_upserted": {
      const agent = data.agent as AcpAgent | undefined;
      if (!agent) return {};
      return { acpAgents: upsertById(s.acpAgents, agent) };
    }
    case "agent_deleted": {
      const agentId = String(data.agent_id ?? "");
      const acpAgents = s.acpAgents.filter((a) => a.id !== agentId);
      const acpSessions = s.acpSessions.filter((x) => x.agent_id !== agentId);
      return {
        acpAgents,
        acpSessions,
        ...reconcileAcpSelection({ ...s, acpSessions }),
      };
    }
    case "workspace_upserted": {
      const ws = data.workspace as AcpWorkspace | undefined;
      if (!ws) return {};
      const acpWorkspaces = upsertById(s.acpWorkspaces, ws, false);
      return {
        acpWorkspaces,
        ...reconcileAcpSelection({ ...s, acpWorkspaces }),
      };
    }
    case "workspace_deleted": {
      const wsId = String(data.workspace_id ?? "");
      const acpWorkspaces = s.acpWorkspaces.filter((w) => w.id !== wsId);
      const acpSessions = s.acpSessions.filter((x) => x.workspace_id !== wsId);
      return {
        acpWorkspaces,
        acpSessions,
        ...reconcileAcpSelection({ ...s, acpWorkspaces, acpSessions }),
      };
    }
    case "session_upserted": {
      const incoming = data.session as AcpSession | undefined;
      if (!incoming) return {};
      const prev = s.acpSessions.find((x) => x.id === incoming.id);
      const merged: AcpSession = {
        ...incoming,
        turns: incoming.turns ?? prev?.turns ?? [],
      };
      const acpSessions = upsertById(s.acpSessions, merged, false);
      return {
        acpSessions,
        ...reconcileAcpSelection({ ...s, acpSessions }),
      };
    }
    case "session_deleted": {
      const sessId = String(data.session_id ?? "");
      const acpSessions = s.acpSessions.filter((x) => x.id !== sessId);
      return {
        acpSessions,
        ...reconcileAcpSelection({ ...s, acpSessions }),
      };
    }
    case "turn_upserted": {
      const sessId = String(data.session_id ?? "");
      const turn = data.turn as AcpTurn | undefined;
      if (!sessId || !turn) return {};
      const acpSessions = s.acpSessions.map((sess) => {
        if (sess.id !== sessId) return sess;
        const turns = upsertById(sess.turns ?? [], turn);
        turns.sort((a, b) => a.seq - b.seq);
        return { ...sess, turns };
      });
      return { acpSessions };
    }
    default:
      return {};
  }
}
