import { useEffect, useRef, useState } from "react";
import { ApiError, convs, files, msgs as msgsApi, serverInfo, serverScope, serverSupportsRecall, session } from "../../api";
import {
  applyLiveMessage,
  applyUpdatedMessage,
  convAvatarText,
  convTitle,
  getState,
  fetchForward,
  isOwnMessage,
  isReadonlyDm,
  prependMessages,
  pushToast,
  setDraft,
  setDraftReuse,
  setState,
  upsertMessage,
  useApp,
} from "../../store";
import { copyPlainText } from "../../clipboard";
import { saveAttachment } from "../../download";
import { unsentText, type DraftReuse } from "../../drafts";
import {
  REEDIT_WINDOW_MS,
  patchReedit,
  readReedits,
  reeditFresh,
  removeReedit,
  saveReedit,
  type ReeditEntry,
} from "../../reedit";
import type {
  AttachmentView,
  ChatRecord,
  ConversationView,
  MemberView,
  MessageView,
  PinEntry,
  ReplyPreview,
} from "../../types";
import { discardSend, retrySend, sendChat,
  sendMeeting, sendTyping } from "../../ws";
import { confirmDialog } from "../Modal";
import RichText from "../../richText";
import AnnouncementBar from "./AnnouncementBar";
import AudioBubble from "./AudioBubble";
import CtxMenu from "./CtxMenu";
import ForwardModal from "./ForwardModal";
import GroupInfo from "./GroupInfo";
import { meetingUrl, parseMeetingUrl } from "./meeting";
import { openExternal } from "../../richText";
import { parseSystemPayload, systemBarText } from "./systemMessage";
import PinnedBar from "./PinnedBar";
import NewPollModal from "./NewPollModal";
import PollCard from "./PollCard";
import ChatRecordView, { ChatRecordCard, type RecordViewState } from "./ChatRecordView";
import { attImgSrc, imageBubbleSize } from "../mediaFormat";
import EmptyState from "../EmptyState";
import Skeleton from "../Skeleton";
import Avatar from "../Avatar";
import VoiceRecorder from "./VoiceRecorder";
import { FEATURE_HOVER_TOOLBAR } from "./features";
import { inBubbleGroup } from "./bubbleGroup";
import { pushBackLayer, resetBackLayers } from "./backStack";
import { Z } from "./layers";
import { recordLabel } from "./chatRecord";
import {
  enterMultiSelect,
  orderedSelectedIds,
  pruneSelection,
  selectable,
  toggleSelect,
  type MultiSelect,
} from "./multiSelect";
import {
  FALLBACK_MERGED_TOAST,
  FALLBACK_SEPARATE_CAP,
  batchResultToast,
  fallbackSeparateToast,
  forwardRespToast,
  isOldServerUnsupported,
  prepareBatchSubmission,
  type BatchPayload,
} from "./forwardBatch";
import {
  canRecall,
  copyTextFor,
  escClose,
  isGhostClick,
  isTombstone,
  menuItems,
  msgDeletedText,
  quoteBodyText,
  reeditTextFor,
  shouldOpenMenuOnClick,
  shouldOpenMenuOnContextMenu,
  tombstoneText,
  type MenuAction,
  type MenuCtx,
  type MenuItem,
  type OverlayOpen,
} from "./msgMenu";
import {
  CopyIcon,
  DownloadIcon,
  EditIcon,
  EmojiIcon,
  EyeIcon,
  ForwardIcon,
  MultiSelectIcon,
  PaperclipIcon,
  PinIcon,
  PollIcon,
  RecallIcon,
  ReplyIcon,
  StarIcon,
  BotIcon,
  CodeIcon,
  FolderSyncIcon,
  ImageIcon,
  SearchIcon,
  SmileIcon,
} from "../icons";

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 附件消息的 content 是服务端占位文本时不再重复渲染 */
function showText(m: MessageView): boolean {
  if (m.kind === "meeting" || m.kind === "system") return false;
  if (!m.attachment) return true;
  return (
    m.content !== "[图片]" &&
    m.content !== "[语音]" &&
    m.content !== `[文件] ${m.attachment.file_name}`
  );
}

/** 可按文本预览的附件（mime 或扩展名判断） */
function isTextLike(a: AttachmentView): boolean {
  return (
    a.mime.startsWith("text/") ||
    /\.(txt|md|log|json|js|jsx|ts|tsx|py|rs|csv|ya?ml|xml|html?|css|sh|bat|ps1|c|h|cpp|java|go|toml|ini|cfg|sql)$/i.test(
      a.file_name,
    )
  );
}

/** 正在拉取初始历史页的会话（避免 StrictMode 双挂载/快速切换重复请求） */
const historyInflight = new Set<string>();

type EditSubmission = { id: string; content: string; expectedVersion: number; clientOpId: string };
export function prepareEditSubmission(previous: EditSubmission | null, id: string, text: string, expectedVersion: number): EditSubmission {
  const content = text.trim();
  if (!content || Array.from(content).length > 4000) throw new Error("消息需 1–4000 字");
  if (previous?.id === id && previous.content === content) return previous;
  return { id, content, expectedVersion, clientOpId: crypto.randomUUID() };
}

/** 编辑结果归属：仅当组件 owner、编辑代次、目标消息、会话与账号代次全部未变时才收尾编辑态 */
export function ownsEditResult(
  submitted: { token: object | null; rev: number; messageId: string; convId: string; gen: number },
  live: { owner: object | null; rev: number; editingId: string | null; activeConv: string | null; gen: number },
): boolean {
  return (
    submitted.token != null &&
    submitted.token === live.owner &&
    submitted.rev === live.rev &&
    live.editingId === submitted.messageId &&
    submitted.gen === live.gen &&
    live.activeConv === submitted.convId
  );
}

/** 服务端明确拒绝的编辑结果：下一次提交必须是新 op；其余错误按“结果未确认”保留原 clientOpId 供幂等重试 */
export function editRejected(e: unknown): boolean {
  return e instanceof ApiError && [400, 403, 404, 409, 422].includes(e.status);
}

/** 快捷 emoji 表态集合 */
const QUICK_EMOJI = ["👍", "❤️", "😂", "😮", "😢", "🙏", "🎉", "🔥"];

/** 回复预览里非文本 kind 的占位摘要 */
function kindLabel(kind: string): string {
  if (kind === "image") return "[图片]";
  if (kind === "file") return "[文件]";
  if (kind === "audio") return "[语音]";
  if (kind === "poll") return "[投票]";
  if (kind === "chat_record") return "[聊天记录]";
  return "";
}

/** 编辑窗口 15 分钟（客户端只用于隐藏入口，服务端按库时间强制判定） */
function withinEditWindow(m: MessageView): boolean {
  return Date.now() - new Date(m.created_at).getTime() < 15 * 60 * 1000;
}

/** MM2：菜单动作 → 图标（右键菜单、动作面板、预览工具栏共用） */
function menuIcon(action: MenuAction) {
  switch (action) {
    case "preview":
      return <EyeIcon />;
    case "reply":
      return <ReplyIcon />;
    case "copy":
      return <CopyIcon />;
    case "forward":
      return <ForwardIcon />;
    case "download":
      return <DownloadIcon />;
    case "multi":
      return <MultiSelectIcon />;
    case "recall":
      return <RecallIcon />;
    case "reaction":
      return <EmojiIcon />;
    case "edit":
      return <EditIcon />;
    case "pin":
      return <PinIcon />;
    case "favorite":
      return <StarIcon />;
  }
}

/** 图片气泡占位尺寸（V2 P2-1）：按原始宽高比预留，加载前不跳版；无尺寸回退 CSS 上限 */
function imgDim(att: AttachmentView): React.CSSProperties | undefined {
  if (att.width == null || att.height == null) return undefined;
  const d = imageBubbleSize(att.width, att.height);
  if (d.width === 0) return undefined;
  return { width: d.width, height: d.height };
}

export default function ChatView() {
  const s = useApp();
  // 输入框初值取当前会话草稿（组件重挂时不再闪空）
  const [text, setText] = useState(() => {
    const st = getState();
    return st.activeConv ? (st.drafts[st.activeConv]?.text ?? "") : "";
  });
  const [showInfo, setShowInfo] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickBottom = useRef(true);
  const lastTypingSent = useRef(0);
  const fileInput = useRef<HTMLInputElement>(null);
  const imgInput = useRef<HTMLInputElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const [showEmoji, setShowEmoji] = useState(false);
  const [showCode, setShowCode] = useState(false);
  const [codeLang, setCodeLang] = useState("ts");
  const [codeBody, setCodeBody] = useState("");
  const draftBeforeEdit = useRef(""); // 编辑态接管 composer 前的草稿
  const prevConv = useRef<string | null>(null); // 上一次渲染的会话（草稿归属判定）
  const [members, setMembers] = useState<MemberView[]>([]);
  const [ac, setAc] = useState<{ start: number; prefix: string } | null>(null);
  const [showSearch, setShowSearch] = useState(false);
  const [searchQ, setSearchQ] = useState("");
  const [searchRes, setSearchRes] = useState<MessageView[] | null>(null);
  const [flashId, setFlashId] = useState<string | null>(null);
  const [preview, setPreview] = useState<{
    att: AttachmentView;
    m: MessageView | null;
    /** MF2：从聊天记录查看页打开的附件（工具栏恰为 下载/转发/关闭） */
    rec?: { outerId: string; attachmentId: string };
  } | null>(null);
  const [previewText, setPreviewText] = useState<string | null>(null);
  const [pendingFiles, setPendingFiles] = useState<File[]>([]);
  const [sending, setSending] = useState(false);
  // 生命周期：emoji 选择器展开的消息 id、消息右键菜单、触屏动作面板、mutation 错误/忙碌
  const [emojiPick, setEmojiPick] = useState<string | null>(null);
  const [msgMenu, setMsgMenu] = useState<{ x: number; y: number; m: MessageView } | null>(null);
  const [sheet, setSheet] = useState<{ m: MessageView } | null>(null);
  // MM2：撤回后「重新编辑」暂存条目；复用附件 chip 的缩略图 objectURL
  // （按撤回附件 id 建表：chip 与复用发送的在飞乐观气泡共用，谁都不用了才回收）
  const [reedits, setReedits] = useState<ReeditEntry[]>([]);
  const [reuseUrls, setReuseUrls] = useState<Record<string, string>>({});
  const reuseUrlsRef = useRef<Record<string, string>>({});
  reuseUrlsRef.current = reuseUrls;
  // 轻量操作反馈（已复制/撤回提示等），自动消失
  const [hint, setHint] = useState<string | null>(null);
  const hintTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 点按/右键手势追踪（触屏 tap → 动作面板；contextmenu 只对鼠标/笔放行）
  const tapStart = useRef<{ x: number; y: number; t: number; type: string } | null>(null);
  // pointerup 记下的完整手势，交给随后的 click 判定（开面板挪到 click 阶段——幽灵点击修复）
  const tapEnd = useRef<{ type: string; movedPx: number; pressMs: number } | null>(null);
  const lastPointer = useRef<{ type: string; ts: number }>({ type: "", ts: 0 });
  // 面板/菜单/预览的打开记录：仅触屏打开的浮层，GHOST_CLICK_MS 内到达的 click
  // 才按幽灵点击忽略；鼠标/笔打开的永不忽略（右键→快速点菜单项是正常操作）
  const sheetAt = useRef<OverlayOpen>({ at: 0, touch: false });
  const menuAt = useRef<OverlayOpen>({ at: 0, touch: false });
  const previewAt = useRef<OverlayOpen>({ at: 0, touch: false });
  const [mutErr, setMutErr] = useState<string | null>(null);
  // 发送（含上传附件）失败的服务端文案，显示在附件条处（V2 P2-1：413 配额提示走这里）
  const [sendErr, setSendErr] = useState<string | null>(null);
  const [mutBusy, setMutBusy] = useState(false);
  // 投票：创建弹窗开关 + 每条消息的操作 busy 态
  const [showPoll, setShowPoll] = useState(false);
  const [pollBusy, setPollBusy] = useState<Record<string, boolean>>({});
  // V2 P2-4 + MF2：转发弹窗——单条 / 多选批量 / 记录内附件条目（各自带幂等 op）
  const [fwdMsg, setFwdMsg] = useState<MessageView | null>(null);
  const [fwdBatch, setFwdBatch] = useState<{ mode: "separate" | "merged"; ids: string[] } | null>(null);
  const [fwdRecord, setFwdRecord] = useState<{ outerId: string; attachmentId: string; fileName: string } | null>(null);
  // MF2 多选模式（选中集只认服务端消息 id；作用域随 conv.id）
  const [ms, setMs] = useState<MultiSelect | null>(null);
  // MF2 聊天记录查看页（快照只活在组件 state；永不进全局 store/本地缓存/日志）
  const [recordView, setRecordView] = useState<RecordViewState | null>(null);
  // 批量/记录转发最近一次确认的载荷（网络错误后同 op 重试复用）
  const batchSub = useRef<BatchPayload | null>(null);
  const recSub = useRef<{ outerId: string; attachmentId: string; targets: string[]; op: string } | null>(null);
  // Esc/返回键层栈要读到最新 recordView（effect 闭包会滞后一次渲染）
  const rvRef = useRef<RecordViewState | null>(null);
  rvRef.current = recordView;
  const [pins, setPins] = useState<PinEntry[]>([]);
  const pinnedIds = new Set(pins.map((p) => p.message.id));
  const pinsRequest = useRef(0);
  const [meetingBase, setMeetingBase] = useState("");
  const gen = session();
  const viewOwner = useRef<object | null>(null);
  const mutation = useRef<object | null>(null);
  const editSubmission = useRef<EditSubmission | null>(null);
  const draftRevision = useRef(0);
  const jumpRequest = useRef(0);

  const conv = s.conversations.find((c) => c.id === s.activeConv) ?? null;
  const msgs = conv ? (s.messages[conv.id] ?? []) : [];
  const hasMore = conv ? (s.hasMore[conv.id] ?? false) : false;
  // MF2：多选态只在本会话生效；recordView 的最外层消息（撤回监听、附件转发的 outerId）
  const msMode = !!ms && !!conv && ms.convId === conv.id;
  const recordDead = (outerId: string) => !!msgs.find((x) => x.id === outerId)?.deleted_at;
  const current = (token: object | null) => token != null && token === viewOwner.current && gen === session() && getState().activeConv === conv?.id;

  // 群聊拉成员列表（@补全用）
  useEffect(() => {
    const token = {};
    viewOwner.current = token;
    setMembers([]);
    if (conv?.kind === "group") {
      convs
        .detail(conv.id)
        .then((d) => { if (current(token)) setMembers(d.members); })
        .catch(() => { if (current(token)) setMembers([]); });
    }
    setAc(null);
    setShowSearch(false);
    setSearchRes(null);
    setFlashId(null);
    setEmojiPick(null);
    setMsgMenu(null);
    setSheet(null);
    setMutErr(null);
    // MF2：会话切换即退出多选并丢弃选中集；覆盖层（查看页/预览/转发弹窗）一并关
    setMs(null);
    setRecordView(null);
    setFwdBatch(null);
    setFwdRecord(null);
    setFwdMsg(null);
    setPreview(null);
    batchSub.current = null;
    recSub.current = null;
    // 待发附件属于旧会话，不能带到新会话里发出去
    setPendingFiles([]);
    setSendErr(null);
    // 切换会话：在途 mutation 回调失权（owner token 已换），忙碌态与编辑代次一并复位
    mutation.current = null;
    draftRevision.current++;
    setMutBusy(false);
    // V2 P1-3：旧会话草稿落库。编辑态中输入框是被编辑内容，真草稿在 draftBeforeEdit
    if (prevConv.current != null && prevConv.current !== conv?.id) {
      setDraft(prevConv.current, getState().editingMessage ? draftBeforeEdit.current : text);
      draftBeforeEdit.current = "";
    }
    prevConv.current = conv?.id ?? null;
    // 切换会话时退出回复/编辑态（目标属于旧会话）
    setState({ replyingTo: null, editingMessage: null });
    setShowEmoji(false);
    setShowCode(false);
    setText(conv ? (getState().drafts[conv.id]?.text ?? "") : "");
  }, [conv?.id]);

  // 只认最后一次请求，换会话后旧会话的响应作废
  const loadPins = (convId: string) => {
    const req = ++pinsRequest.current;
    convs
      .pins(convId)
      .then((rows) => {
        if (req === pinsRequest.current) setPins(rows);
      })
      .catch(() => {
        if (req === pinsRequest.current) setPins([]);
      });
  };

  // 打开会话，以及收到 pins_updated 时，刷新置顶列表与菜单状态
  useEffect(() => {
    if (!conv) return;
    setPins([]);
    loadPins(conv.id);
    const onPins = (e: Event) => {
      if ((e as CustomEvent<string>).detail === conv.id) loadPins(conv.id);
    };
    window.addEventListener("chat:pins", onPins);
    return () => {
      pinsRequest.current++;
      window.removeEventListener("chat:pins", onPins);
    };
  }, [conv?.id]);

  // ===== MM2：重新编辑暂存 =====
  // 打开会话时装载本账号在本服务器的暂存；每 30s 扫一遍把超过 10 分钟的剔除；
  // 登出时清掉（servers.removeServerData 按 chat_reedit: 键前缀兜底）。
  useEffect(() => {
    const meId = getState().me?.id;
    const reload = () =>
      setReedits(meId ? readReedits(localStorage, serverScope(), meId) : []);
    reload();
    const tick = setInterval(reload, 30_000);
    const onLogout = () => {
      setReedits([]);
      // MF2：登出/服务端切换丢弃多选与查看页；返回键层栈兜底清空
      setMs(null);
      setRecordView(null);
      setFwdMsg(null);
      setFwdBatch(null);
      setFwdRecord(null);
      setPreview(null);
      batchSub.current = null;
      recSub.current = null;
      resetBackLayers();
    };
    window.addEventListener("chat:logout", onLogout);
    return () => {
      clearInterval(tick);
      window.removeEventListener("chat:logout", onLogout);
    };
  }, [conv?.id]);

  // MM2：复用发送被拒 → ws.ts 把文字退回草稿后发事件，这里把 composer 文本同步回来
  useEffect(() => {
    const onRestore = (e: Event) => {
      const { convId } = (e as CustomEvent<{ convId: string }>).detail;
      if (conv?.id === convId) {
        setText(getState().drafts[convId]?.text ?? "");
      }
    };
    window.addEventListener("chat:composer-restore", onRestore);
    return () => window.removeEventListener("chat:composer-restore", onRestore);
  }, [conv?.id]);

  // MM2：消息变墓碑时关掉开着的菜单/面板/预览（预览开着则补一条提示）
  useEffect(() => {
    const becameTombstone = (id: string | null | undefined) =>
      !!id && !!msgs.find((m) => m.id === id && isTombstone(m));
    if (msgMenu && becameTombstone(msgMenu.m.id)) setMsgMenu(null);
    if (sheet && becameTombstone(sheet.m.id)) setSheet(null);
    if (preview?.m && becameTombstone(preview.m.id)) {
      setPreview(null);
      showHint("该消息已被撤回");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [msgs]);

  // Esc 关闭触屏动作面板（MM2 fix3）：捕获阶段拦截，composer 的 Esc 不再连发；
  // 右键菜单同款逻辑在 CtxMenu 内部。
  useEffect(() => {
    if (!sheet) return;
    const onKey = (e: KeyboardEvent) => escClose(e, () => setSheet(null));
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [sheet]);

  // MM2：复用 chip 的缩略图走鉴权的 /recalled → objectURL，存进 reuseUrls 表。
  // 只取图片 kind：file 的 ?thumb=1 回退原件全文（白拉一个大 blob，chip 也用不上）。
  const reuse = conv ? s.drafts[conv.id]?.reuse : undefined;
  const reuseId = reuse?.attachment_id ?? null;
  useEffect(() => {
    if (!reuse || reuse.expired || !reuseId || reuse.kind !== "image" || !serverSupportsRecall()) {
      return;
    }
    let alive = true;
    files
      .recalledUrl(reuseId, { thumb: true })
      .then((u) => {
        if (alive) setReuseUrls((m) => (m[reuseId] ? m : { ...m, [reuseId]: u }));
        else URL.revokeObjectURL(u);
      })
      .catch(() => {
        // 过期/被删：chip 标「附件已过期」，不再重试
        if (alive) setDraftReuse(conv!.id, { ...reuse, expired: true });
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reuseId, reuse?.expired]);

  // objectURL 回收：仅当「未过期 chip」与「引用它的在飞乐观气泡」都不存在时
  // 才 revoke（chip 可能在复用发送在途时被摘掉，URL 要给气泡续命到 ack/拒收）。
  // 组件卸载时无条件全收。
  useEffect(() => {
    const inUse = new Set<string>();
    for (const d of Object.values(s.drafts)) {
      if (d.reuse && !d.reuse.expired) inUse.add(d.reuse.attachment_id);
    }
    for (const list of Object.values(s.messages)) {
      for (const m of list) {
        const a = m.attachment;
        if (a?.recalled && m.id.startsWith("client:")) inUse.add(a.id);
      }
    }
    setReuseUrls((cur) => {
      const dead = Object.keys(cur).filter((id) => !inUse.has(id));
      if (dead.length === 0) return cur;
      for (const id of dead) URL.revokeObjectURL(cur[id]);
      const next = { ...cur };
      for (const id of dead) delete next[id];
      return next;
    });
  }, [s.drafts, s.messages]);
  useEffect(
    () => () => {
      for (const u of Object.values(reuseUrlsRef.current)) URL.revokeObjectURL(u);
    },
    [],
  );

  useEffect(() => {
    let alive = true;
    serverInfo()
      .then((info) => {
        if (alive) setMeetingBase(info.meeting_base_url ?? "");
      })
      .catch(() => {
        if (alive) setMeetingBase("");
      });
    return () => {
      alive = false;
    };
  }, []);

  // 打开会话 → 拉历史（P1-11：每会话至少一次；实时缓存非空也要拉，按 id 合并不覆盖）
  useEffect(() => {
    if (!conv) return;
    setShowInfo(false);
    if (getState().historyLoaded[conv.id] || historyInflight.has(conv.id)) return;
    historyInflight.add(conv.id);
    convs
      .messages(conv.id, { limit: 50 })
      .then((p) =>
        setState((st) => {
          const merged = p.messages.reduce(
            (l, m) => upsertMessage(l, m),
            st.messages[conv.id] ?? [],
          );
          return {
            messages: { ...st.messages, [conv.id]: merged },
            hasMore: { ...st.hasMore, [conv.id]: (merged[0]?.seq ?? 1) > 1 },
            historyLoaded: { ...st.historyLoaded, [conv.id]: true },
          };
        }),
      )
      .catch(console.warn)
      .finally(() => historyInflight.delete(conv.id));
  }, [conv?.id]);

  // 新消息滚到底（若本来就在底部）
  useEffect(() => {
    if (stickBottom.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [msgs.length, conv?.id]);

  // V2 P3-1：消费全局搜索跳转 —— 会话打开后定位并闪烁目标消息，随后清空
  useEffect(() => {
    const pj = s.pendingJump;
    if (!pj || !conv || pj.convId !== conv.id) return;
    setState({ pendingJump: null });
    void jumpToRef(pj.messageId, pj.seq);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.pendingJump, conv?.id]);

  // ===== MF2：多选模式 / 批量转发 / 聊天记录查看页 =====

  // 选中集收编：消息列表变化（WS 撤回/删除）后把已变墓碑/消失的选中剔除；
  // 不在已加载窗口的 id 保留（可能分页还没拉到）
  useEffect(() => {
    setMs((cur) => (cur && conv && cur.convId === conv.id ? pruneSelection(cur, msgs) : cur));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [msgs, conv?.id]);

  // 记录查看页：打开时恰好一次 GET /record；外层消息被撤回 → 「聊天记录不可用」
  useEffect(() => {
    if (!recordView?.loading) return;
    const outerId = recordView.outerId;
    let alive = true;
    msgsApi
      .record(outerId)
      .then((rec) => {
        if (!alive) return;
        setRecordView((cur) =>
          cur && cur.outerId === outerId && cur.loading
            ? { ...cur, loading: false, levels: [{ title: rec.title, items: rec.items }] }
            : cur,
        );
      })
      .catch(() => {
        if (!alive) return;
        setRecordView((cur) =>
          cur && cur.outerId === outerId ? { ...cur, loading: false, error: true } : cur,
        );
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recordView?.outerId, recordView?.loading]);

  // WS 撤回外层消息：查看页换成不可用；若正在看其中附件（preview.rec 同 outerId）先关掉
  useEffect(() => {
    if (recordView && recordDead(recordView.outerId)) {
      setRecordView((cur) => (cur ? { ...cur, loading: false, error: true } : cur));
      setPreview((cur) => (cur?.rec && cur.rec.outerId === recordView.outerId ? null : cur));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [msgs, recordView]);

  // Android 返回键层栈（顺序由下往上：多选 → 记录查看 → 预览 → 转发弹窗）
  useEffect(() => (msMode ? pushBackLayer(() => setMs(null)) : undefined), [msMode]);
  useEffect(() => {
    if (!recordView) return;
    let unreg: (() => void) | undefined;
    const popOrClose = () => {
      const cur = rvRef.current;
      if (!cur) return;
      if (cur.levels.length > 1) {
        // 弹一层后查看页仍开着：本次 back 已消费注册项，重新占栈顶
        setRecordView({ ...cur, levels: cur.levels.slice(0, -1) });
        unreg = pushBackLayer(popOrClose);
      } else {
        setRecordView(null);
      }
    };
    unreg = pushBackLayer(popOrClose);
    return () => unreg?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recordView != null]);
  useEffect(() => (preview ? pushBackLayer(() => setPreview(null)) : undefined), [preview != null]);
  const fwdOpen = !!(fwdMsg || fwdBatch || fwdRecord);
  useEffect(
    () =>
      fwdOpen
        ? pushBackLayer(() => {
            setFwdMsg(null);
            setFwdBatch(null);
            setFwdRecord(null);
          })
        : undefined,
    [fwdOpen],
  );

  // Esc 编排（document 捕获阶段）：ForwardModal → 查看器/预览 → 记录查看页(逐层) → 多选。
  // 右键菜单/动作面板各自挂了捕获 Esc（escClose + stopPropagation），这里让路；
  // composer 的 Esc 在 textarea 目标阶段，捕获拦截后事件不下沉，不会连发。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (msgMenu || sheet) return; // 浮层自己的 Esc 处理器负责
      const closeTop = fwdMsg || fwdBatch || fwdRecord
        ? () => {
            setFwdMsg(null);
            setFwdBatch(null);
            setFwdRecord(null);
          }
        : preview
          ? () => setPreview(null)
          : recordView
            ? () =>
                setRecordView((cur) =>
                  cur && cur.levels.length > 1
                    ? { ...cur, levels: cur.levels.slice(0, -1) }
                    : null,
                )
            : msMode
              ? () => setMs(null)
              : null;
      if (closeTop) escClose(e, closeTop);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  });

  if (!conv) {
    return (
      <main className="chat-view empty">
        <EmptyState title="选择一个会话开始聊天" detail="从左侧列表打开会话，或去联系人发起聊天" testId="chat-empty" />
      </main>
    );
  }

  const title = convTitle(conv, s.me?.id);
  const peerOnline = conv.kind === "dm" && conv.peer ? !!s.online[conv.peer.id] : false;
  const typing = s.typing[conv.id];
  const typingFresh = typing && Date.now() - typing.ts < 5000;
  const readonly = isReadonlyDm(conv, s);

  // 生命周期入口可见性（服务端在锁内二次校验；这里只做 UI 层收敛）
  // —— 以下谓词服务已下线功能（feature flag 关闭），代码路径保留 ——
  const canEditMsg = (m: MessageView) =>
    !m.id.startsWith("client:") &&
    !m.deleted_at &&
    isOwnMessage(m, s.me?.id) &&
    m.kind === "text" &&
    withinEditWindow(m);
  const canActMsg = (m: MessageView) =>
    !m.id.startsWith("client:") && !m.deleted_at && !readonly && m.kind !== "system";
  // V2 P2-4 转发/收藏入口：非乐观消息、非墓碑、非投票。只读会话仍允许（
  // 成员读得到就能转出去；目标侧权限由服务端逐条判定）
  const canForwardMsg = (m: MessageView) =>
    !m.id.startsWith("client:") && !m.deleted_at && m.kind !== "poll" && m.kind !== "system";
  // 投票可置顶；已删除、乐观消息、只读会话不显示入口
  const canPinMsg = (m: MessageView) => canActMsg(m);

  // ===== MM2：菜单上下文 / 动作分发 =====

  const showHint = (t: string) => {
    setHint(t);
    if (hintTimer.current) clearTimeout(hintTimer.current);
    hintTimer.current = setTimeout(() => setHint(null), 1800);
  };

  const menuCtx = (): MenuCtx => ({
    meId: s.me?.id,
    myRole:
      conv.kind === "group" && conv.owner_id === s.me?.id ? "owner" : undefined,
    convType: conv.kind,
    now: Date.now(),
    readOnly: !!readonly,
    serverSupportsRecall: serverSupportsRecall(),
  });

  const itemsFor = (m: MessageView): MenuItem[] =>
    menuItems(m, { ...menuCtx(), pinned: pinnedIds.has(m.id) });

  const doCopy = async (m: MessageView) => {
    showHint((await copyPlainText(copyTextFor(m))) ? "已复制" : "复制失败");
  };

  const openMenu = (m: MessageView, x: number, y: number) => {
    if (itemsFor(m).length === 0) return; // 墓碑/乐观消息没有菜单
    setSheet(null);
    // contextmenu 已对 touch 源放行屏蔽，这里照常记录来源（恒为非 touch）
    menuAt.current = { at: Date.now(), touch: lastPointer.current.type === "touch" };
    setMsgMenu({ x, y, m });
  };

  const openSheet = (m: MessageView) => {
    if (itemsFor(m).length === 0) return;
    setMsgMenu(null);
    // 动作面板只由触屏点按打开（shouldOpenMenuOnClick 要求 pointerType==="touch"）
    sheetAt.current = { at: Date.now(), touch: true };
    setSheet({ m });
  };

  /**
   * 撤回（MM1 POST /recall；旧服务端自动退 DELETE）。
   * 我的消息：先留本地原文（re-edit 暂存），失败立刻删掉，不留没撤回成功的明文；
   * 成功后用响应里的 recalled_attachment_id 补记，给「复用附件」chip 用。
   */
  const doRecall = async (m: MessageView) => {
    setMsgMenu(null);
    setSheet(null);
    if (mutBusy) return;
    if (!(await confirmDialog("撤回这条消息？所有人将看到“消息已撤回”", { danger: true }))) return;
    const mine = m.sender_id === s.me?.id;
    const meId = s.me?.id;
    const canReedit =
      mine &&
      !!meId &&
      serverSupportsRecall() && // 旧服务端撤回=纯删除，没有 re-edit/复用
      (m.kind === "text" || !!m.attachment) &&
      m.kind !== "poll" &&
      m.kind !== "audio";
    if (canReedit && meId) {
      saveReedit(localStorage, serverScope(), meId, {
        conversation_id: conv.id,
        message_id: m.id,
        // 附件消息的 content 是服务端占位（[图片]/[文件] 名/[语音]），不是用户文本
        text: reeditTextFor(m),
        reply_to: m.reply_to_preview?.message_id ?? null,
        ...(m.attachment
          ? {
              attachment: {
                id: m.attachment.id,
                file_name: m.attachment.file_name,
                mime: m.attachment.mime,
                size: m.attachment.size,
                kind: m.kind,
              },
            }
          : {}),
        savedAt: Date.now(),
      });
      setReedits(readReedits(localStorage, serverScope(), meId));
    }
    setMutBusy(true);
    setMutErr(null);
    try {
      const tomb = await msgsApi.recall(m.id, crypto.randomUUID());
      setState((st) => applyUpdatedMessage(st, tomb));
      if (canReedit && meId) {
        patchReedit(localStorage, serverScope(), meId, m.id, {
          recalled_attachment_id: tomb.recalled_attachment_id ?? null,
        });
        setReedits(readReedits(localStorage, serverScope(), meId));
      }
    } catch (e) {
      // 撤回没发生：不留原文
      if (canReedit && meId) {
        removeReedit(localStorage, serverScope(), meId, m.id);
        setReedits(readReedits(localStorage, serverScope(), meId));
      }
      if (e instanceof ApiError && e.code === "recall_expired") {
        setMutErr("超过 24 小时的消息不能撤回");
      } else {
        setMutErr(e instanceof Error ? e.message : "撤回失败");
      }
    } finally {
      setMutBusy(false);
    }
  };

  /** 「重新编辑」：原文回填 composer；带过附件则挂「复用附件」chip。用过即删。 */
  const reeditMsg = async (entry: ReeditEntry) => {
    const meId = s.me?.id;
    if (!meId) return;
    removeReedit(localStorage, serverScope(), meId, entry.message_id);
    setReedits(readReedits(localStorage, serverScope(), meId));
    if (entry.conversation_id !== conv.id) return;
    // 附件消息的暂存正文是 ""（占位非用户文本）：不动 composer，只恢复 chip
    if (entry.text) {
      let next = text;
      if (next) {
        next = (await confirmDialog("替换当前输入？"))
          ? entry.text
          : `${next}\n${entry.text}`;
      } else {
        next = entry.text;
      }
      setText(next);
      setDraft(conv.id, next);
    }
    // 附件原件还在 TTL 内 → chip；旧服务端没有 /recalled，chip 不出现
    const rid = entry.recalled_attachment_id;
    if (entry.attachment && rid && serverSupportsRecall()) {
      const chip: DraftReuse = {
        attachment_id: rid,
        file_name: entry.attachment.file_name,
        mime: entry.attachment.mime,
        size: entry.attachment.size,
        kind: entry.attachment.kind,
        conversation_id: conv.id,
      };
      setDraftReuse(conv.id, chip);
    }
    taRef.current?.focus();
  };

  // ---------- 手势：右键菜单（鼠标/笔）+ 触屏点按（动作面板） ----------
  const onBubblePointerDown = (e: React.PointerEvent) => {
    lastPointer.current = { type: e.pointerType ?? "", ts: Date.now() };
    tapStart.current = { x: e.clientX, y: e.clientY, t: Date.now(), type: e.pointerType ?? "" };
    tapEnd.current = null; // 新手势开始：作废上一条没等到 click 的记录
  };

  const onMsgContextMenu = (m: MessageView) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (msMode) return; // MF2：多选模式下不开菜单（点击已被行级捕获改成勾选切换）
    // Android WebView 长按也发 contextmenu；源指针是 touch 时让给系统文本选择
    if (!shouldOpenMenuOnContextMenu(lastPointer.current.type)) return;
    openMenu(m, e.clientX, e.clientY);
  };

  const onBubblePointerUp = (e: React.PointerEvent) => {
    const st = tapStart.current;
    tapStart.current = null;
    if (!st) return;
    // 只记手势，不开面板：随后到来的 click 才判定（pointerup 开面板会让
    // 兼容 click 落到刚渲染的面板项上，触发随机动作——Android WebView 幽灵点击）
    tapEnd.current = {
      type: st.type,
      movedPx: Math.hypot(e.clientX - st.x, e.clientY - st.y),
      pressMs: Date.now() - st.t,
    };
  };

  const onBubbleClick = (m: MessageView) => (e: React.MouseEvent) => {
    const tap = tapEnd.current;
    tapEnd.current = null;
    if (msMode) return; // MF2：多选模式点按只切勾选（行级捕获已做），不弹动作面板
    if (!tap || tap.type !== "touch") return; // 点按规则只对触摸生效；鼠标单击照旧
    const sel =
      typeof window !== "undefined" && window.getSelection
        ? (window.getSelection()?.toString() ?? "")
        : "";
    if (
      shouldOpenMenuOnClick({
        pointerType: tap.type,
        movedPx: tap.movedPx,
        pressMs: tap.pressMs,
        target: e.target,
        hasSelection: sel.length > 0,
      })
    ) {
      openSheet(m);
    }
  };

  /** 菜单/面板/预览工具栏共用：按 action 分发到具体动作 */
  const runMenuAction = (m: MessageView, action: MenuAction) => {
    setMsgMenu(null);
    setSheet(null);
    switch (action) {
      case "preview":
        if (m.attachment) openPreview(m.attachment, m);
        break;
      case "reply":
        startReply(m);
        break;
      case "copy":
        void doCopy(m);
        break;
      case "forward":
        setFwdMsg(m);
        break;
      case "download":
        if (m.attachment) void saveAttachment(m.attachment);
        break;
      case "multi":
        onEnterMultiSelect(m); // MM3 占位
        break;
      case "recall":
        void doRecall(m);
        break;
      case "reaction":
        setEmojiPick(m.id);
        break;
      case "edit":
        startEdit(m);
        break;
      case "pin":
        void doPin(m);
        break;
      case "favorite":
        void doFavorite(m);
        break;
    }
  };

  // ===== MF2：多选模式入口/切换 =====

  /** 消息菜单「多选」→ 进模式并预选该条（投票等不可选条目只进模式） */
  const onEnterMultiSelect = (m: MessageView) => {
    if (!conv) return;
    setMsgMenu(null);
    setSheet(null);
    setMs(enterMultiSelect(conv.id, m));
  };

  /** 多选模式：点行任意位置切换勾选（含卡片/⋯/链接——行级捕获已拦掉内层动作） */
  const toggleMs = (m: MessageView) => {
    const cur = ms;
    if (!cur || cur.convId !== conv.id) return;
    const r = toggleSelect(cur, m);
    if (r.toast) showHint(r.toast);
    setMs(r.next);
  };

  const exitMultiSelect = () => setMs(null);

  // ===== MF2：批量转发（forward-batch；旧服务端逐条兜底） =====

  const openBatchForward = (mode: "separate" | "merged") => {
    if (!msMode || !ms || ms.ids.size === 0) return;
    const ids = orderedSelectedIds(ms, msgs); // seq（气泡）序
    if (ids.length === 0) return;
    setFwdBatch({ mode, ids });
  };

  /** 弹窗确认 → 载荷（op 复用由 prepareBatchSubmission 按参数判同）→ 执行 */
  const submitBatchForward = async (mode: "separate" | "merged", ids: string[], targets: string[]) => {
    if (!conv) return;
    const p = prepareBatchSubmission(batchSub.current, conv.id, ids, targets, mode);
    batchSub.current = p;
    await runBatchForward(p);
  };

  const runBatchForward = async (p: BatchPayload): Promise<void> => {
    try {
      const resp = await msgsApi.forwardBatch(
        p.conversation_id,
        p.message_ids,
        p.targets,
        p.mode,
        p.client_op_id,
      );
      const t = batchResultToast(resp);
      pushToast({ title: "转发", text: t.text });
      // MF2-C5：至少一个目标成功才退出多选；全败保留选中。
      // 已落定：清掉载荷，下一次「同样的选择再发一次」拿新 op（不命中服务端幂等去重）
      if (t.ok >= 1) {
        setMs(null);
        batchSub.current = null;
      }
    } catch (e) {
      if (isOldServerUnsupported(e)) {
        await runBatchFallback(p);
        batchSub.current = null; // 兜底路径已完结
      } else if (e instanceof ApiError) {
        // 整单拒绝（400 等：没有可转发/嵌套过深/太大/超上限）：服务端文案原样展示，保留选中
        pushToast({ title: "转发失败", text: e.message || "转发失败" });
        // 明确拒绝 → 下次提交是新 op；网络/5xx 未确认 → 保留 op 供重试
        if ([400, 403, 404, 409, 422].includes(e.status)) batchSub.current = null;
      } else {
        // 结果未确认（网络错误/超时无 HTTP 响应）：toast 重试按钮用同一 client_op_id 重发
        pushToast(
          {
            title: "转发失败",
            text: "结果未确认；点「重试」以同一请求重发（幂等，不会重复）",
            action: { label: "重试", run: () => void runBatchForward(p) },
          },
          { timeoutMs: 0 },
        );
      }
    }
  };

  /** 旧服务端兜底（MF2-C6）：merged 直接提示；separate 逐条单转（≤20 条、首个失败即止） */
  const runBatchFallback = async (p: BatchPayload) => {
    if (p.mode === "merged") {
      pushToast({ title: "转发", text: FALLBACK_MERGED_TOAST });
      return;
    }
    let sent = 0;
    for (const id of p.message_ids.slice(0, FALLBACK_SEPARATE_CAP)) {
      try {
        // 单转幂等键 fwd:{op}:{target} 不含源消息 id——每条必须新 op
        const r = await msgsApi.forward(id, p.targets, crypto.randomUUID());
        const oks = r.results.filter((x) => x.ok && x.message);
        for (const x of oks) setState((st) => applyLiveMessage(st, x.message!));
        if (r.results.some((x) => x.ok)) sent++;
      } catch {
        break; // 首个 4xx/网络错误停手，不放大失败
      }
    }
    pushToast({ title: "转发", text: fallbackSeparateToast(sent, p.message_ids.length) });
    if (sent > 0) setMs(null);
  };

  // ===== MF2：记录内附件条目转发（POST /messages/{outerId}/forward + record_attachment_id） =====

  const submitRecordForward = async (outerId: string, attachmentId: string, targets: string[]) => {
    const sorted = [...targets].sort();
    const prev = recSub.current;
    const p =
      prev &&
      prev.outerId === outerId &&
      prev.attachmentId === attachmentId &&
      prev.targets.join("\n") === sorted.join("\n")
        ? prev
        : { outerId, attachmentId, targets: sorted, op: crypto.randomUUID() };
    recSub.current = p;
    await runRecordForward(p);
  };

  const runRecordForward = async (p: { outerId: string; attachmentId: string; targets: string[]; op: string }) => {
    try {
      const resp = await msgsApi.forward(p.outerId, p.targets, p.op, {
        recordAttachmentId: p.attachmentId,
      });
      const oks = resp.results.filter((r) => r.ok && r.message);
      for (const r of oks) setState((st) => applyLiveMessage(st, r.message!));
      pushToast({ title: "转发", text: forwardRespToast(resp).text });
      if (oks.length > 0) recSub.current = null; // 同上：落定后下次同参换新 op
    } catch (e) {
      if (e instanceof ApiError) {
        // 400/404：服务端文案原样（附件不存在/记录不可用/参数错误）
        pushToast({ title: "转发失败", text: e.message || "转发失败" });
        recSub.current = null; // 明确拒绝 → 下次是新提交
      } else {
        pushToast(
          {
            title: "转发失败",
            text: "结果未确认；点「重试」以同一请求重发（幂等，不会重复）",
            action: { label: "重试", run: () => void runRecordForward(p) },
          },
          { timeoutMs: 0 },
        );
      }
    }
  };

  // ===== MF2：聊天记录查看页 =====

  const openRecord = (m: MessageView) => {
    setMsgMenu(null);
    setSheet(null);
    setRecordView({ outerId: m.id, levels: [], loading: true, error: false });
  };

  const pushRecordLevel = (rec: ChatRecord) =>
    setRecordView((cur) =>
      cur ? { ...cur, levels: [...cur.levels, { title: rec.title, items: rec.items }] } : cur,
    );

  const popRecordLevel = () =>
    setRecordView((cur) =>
      cur && cur.levels.length > 1 ? { ...cur, levels: cur.levels.slice(0, -1) } : null,
    );

  /** 记录内附件 → 查看器/预览（带记录上下文：工具栏恰为 下载/转发/关闭） */
  const openRecordPreview = (att: AttachmentView) => {
    const outerId = rvRef.current?.outerId;
    if (!outerId) return;
    openPreview(att, undefined, { outerId, attachmentId: att.id });
  };

  const unpinMsg = async (messageId: string) => {
    await convs.unpin(conv.id, messageId);
    setPins((rows) => rows.filter((p) => p.message.id !== messageId));
  };

  const doPin = async (m: MessageView) => {
    setMsgMenu(null);
    setMutErr(null);
    const pinned = pinnedIds.has(m.id);
    try {
      if (pinned) await unpinMsg(m.id);
      else {
        await convs.pin(conv.id, m.id);
        loadPins(conv.id);
      }
    } catch (e) {
      setMutErr(e instanceof Error ? e.message : "置顶失败");
    }
  };

  const startMeeting = async () => {
    if (!meetingBase || readonly) return;
    if (!(await confirmDialog("发起视频会议？将在本会话发送一条会议卡片。", { okText: "发起" })))
      return;
    sendMeeting(conv.id, meetingUrl(meetingBase));
  };

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    stickBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
    if (el.scrollTop < 40 && hasMore && !loadingMore) loadMore();
  };

  const loadMore = async () => {
    if (!conv) return;
    const first = (getState().messages[conv.id] ?? [])[0];
    if (!first) return;
    setLoadingMore(true);
    try {
      const p = await convs.messages(conv.id, { before_seq: first.seq, limit: 50 });
      setState((st) => ({
        messages: {
          ...st.messages,
          [conv.id]: prependMessages(st.messages[conv.id] ?? [], p.messages),
        },
        hasMore: { ...st.hasMore, [conv.id]: p.has_more },
      }));
      // 保持视口不跳
      const el = scrollRef.current;
      if (el) requestAnimationFrame(() => (el.scrollTop += 1));
    } finally {
      setLoadingMore(false);
    }
  };

  // ---------- 生命周期动作 ----------

  const startReply = (m: MessageView) => {
    setState({ replyingTo: m, editingMessage: null });
    setMutErr(null);
    taRef.current?.focus();
  };

  const startEdit = (m: MessageView) => {
    draftRevision.current++; // 新编辑态：在途的旧编辑回调一律作废
    draftBeforeEdit.current = text; // 暂存草稿，取消/保存后还原
    setState({ editingMessage: m, replyingTo: null });
    setText(m.content);
    setMutErr(null);
    taRef.current?.focus();
  };

  const cancelEdit = () => {
    draftRevision.current++;
    setState({ editingMessage: null });
    setText(draftBeforeEdit.current);
    draftBeforeEdit.current = "";
    setMutErr(null);
  };

  /** 编辑提交：REST + expected_version；结果未确认的重试复用同一 client_op_id 幂等回放，
   * 409 说明期间被别人（另一设备）改过。回调先判归属：取消/改编辑另一条/换会话或账号后不碰编辑态与草稿。 */
  const submitEdit = async () => {
    const em = s.editingMessage;
    const t = text.trim();
    const token = viewOwner.current;
    const rev = draftRevision.current;
    if (!em || !t || mutBusy || mutation.current != null || !current(token)) return;
    let payload: EditSubmission;
    try {
      payload = prepareEditSubmission(editSubmission.current, em.id, t, em.edit_version ?? 0);
    } catch (e) {
      setMutErr(e instanceof Error ? e.message : "编辑失败");
      return;
    }
    editSubmission.current = payload;
    mutation.current = token;
    setMutBusy(true);
    setMutErr(null);
    const mine = () =>
      ownsEditResult(
        { token, rev, messageId: em.id, convId: conv.id, gen },
        {
          owner: viewOwner.current,
          rev: draftRevision.current,
          editingId: getState().editingMessage?.id ?? null,
          activeConv: getState().activeConv,
          gen: session(),
        },
      );
    try {
      const updated = await msgsApi.edit(
        payload.id,
        payload.content,
        payload.expectedVersion,
        payload.clientOpId,
      );
      if (!current(token)) return;
      // 服务端确认（含幂等回放视图）按版本合并进列表；
      // 只有用户仍停留在本次编辑态时才退出编辑并还原草稿
      const own = mine();
      setState((st) => ({
        messages: {
          ...st.messages,
          [conv.id]: upsertMessage(st.messages[conv.id] ?? [], updated),
        },
        ...(own ? { editingMessage: null } : {}),
      }));
      if (own) {
        editSubmission.current = null;
        setText(draftBeforeEdit.current);
        draftBeforeEdit.current = "";
      }
    } catch (e) {
      if (!mine()) return;
      if (editRejected(e)) editSubmission.current = null;
      setMutErr(e instanceof Error ? e.message : "编辑失败");
    } finally {
      if (mutation.current === token) mutation.current = null;
      if (current(token)) setMutBusy(false);
    }
  };

  /** 收藏（V2 P2-4）= 转发到本人文件传输助手；每次点击是新 op（重发即有意重复收藏）。
   * 服务端确认后立即并入 self 会话本地缓存；广播帧到达按 id 去重。 */
  const doFavorite = async (m: MessageView) => {
    setMsgMenu(null);
    const self = s.conversations.find((c) => c.kind === "self");
    if (!self || mutBusy) return;
    setMutBusy(true);
    setMutErr(null);
    try {
      const resp = await msgsApi.forward(m.id, [self.id], crypto.randomUUID());
      const r = resp.results[0];
      if (r?.ok && r.message) {
        setState((st) => ({ ...st, ...applyLiveMessage(st, r.message!) }));
        pushToast({ convId: self.id, title: "收藏", text: "已收藏到文件传输助手" });
      } else {
        setMutErr(r?.message_text ?? "收藏失败");
      }
    } catch (e) {
      setMutErr(e instanceof Error ? e.message : "收藏失败");
    } finally {
      setMutBusy(false);
    }
  };

  /** reaction 切换：已表态→取消，未表态→添加；服务端确认后更新 */
  const toggleReaction = async (m: MessageView, emoji: string) => {
    const key = `${m.id}:${emoji}`;
    if (s.reactionPending[key]) return;
    setState((st) => ({
      reactionPending: { ...st.reactionPending, [key]: true },
    }));
    setMutErr(null);
    try {
      const mine = (m.reactions ?? []).some((r) => r.emoji === emoji && r.reacted_by_me);
      const res = await msgsApi.react(m.id, emoji, !mine, crypto.randomUUID());
      setState((st) => applyUpdatedMessage(st, res));
    } catch (e) {
      setMutErr(e instanceof Error ? e.message : "表情操作失败");
    } finally {
      setState((st) => {
        const next = { ...st.reactionPending };
        delete next[key];
        return { reactionPending: next };
      });
    }
  };

  const send = async () => {
    const t = text.trim();
    if (readonly || sending || mutBusy) return;
    if (s.editingMessage) {
      await submitEdit();
      return;
    }
    // MM2：chip 还在但没过期 → 以 reuse_attachment_id 发出（文字随行）；
    // 已过期的 chip 不参与发送，也不自动重发。
    const chip = reuse && !reuse.expired ? reuse : undefined;
    if (!t && pendingFiles.length === 0 && !chip) return;
    setSending(true);
    setSendErr(null);
    // 上传期间用户可能继续输入、加附件、切会话：完成后只清本次发出的那部分
    const sentText = text;
    const sentFiles = pendingFiles;
    const rt0 = s.replyingTo;
    const still = () => getState().activeConv === conv.id;
    try {
      if (chip) {
        // 复用发送：附件来自撤回保留原件，本地没有 File 可直接上传
        const attView: AttachmentView = {
          id: chip.attachment_id,
          file_name: chip.file_name,
          mime: chip.mime,
          size: chip.size,
          duration_ms: null,
          width: null,
          height: null,
        };
        sendChat(conv.id, t, attView, rt0 ?? undefined, true);
        setDraft(conv.id, "");
        if (still()) {
          setText("");
          setAc(null);
          stickBottom.current = true;
        }
        if (getState().replyingTo === rt0) setState({ replyingTo: null });
        return;
      }
      // 先上传待发附件，文字随最后一个附件消息一起发
      const atts: AttachmentView[] = [];
      for (const f of sentFiles) {
        atts.push(await files.upload(conv.id, f));
      }
      const rt = rt0 ?? undefined;
      atts.forEach((att, i) =>
        sendChat(
          conv.id,
          i === atts.length - 1 ? t : "",
          att,
          i === atts.length - 1 ? rt : undefined,
        ),
      );
      if (atts.length === 0 && t) sendChat(conv.id, t, undefined, rt);
      const draft = getState().drafts[conv.id]?.text ?? "";
      if (unsentText(draft, sentText) !== draft) setDraft(conv.id, unsentText(draft, sentText)); // 发送即清该会话草稿
      if (getState().replyingTo === rt0) setState({ replyingTo: null });
      if (still()) {
        setText((cur) => unsentText(cur, sentText));
            setPendingFiles((p) => p.filter((f) => !sentFiles.includes(f)));
        setAc(null);
        stickBottom.current = true;
      }
    } catch (err) {
      // 保留服务端文案（如 413 配额提示）；待发附件不清，便于重试
      if (still()) setSendErr(err instanceof Error ? err.message : "发送失败");
    } finally {
      setSending(false);
    }
  };

  const addPending = (list: Iterable<File>) => {
    setPendingFiles((p) => [...p, ...Array.from(list)]);
  };

  // @补全：群成员（用户名前缀或昵称包含）
  const acItems: MemberView[] = ac
    ? members
        .filter(
          (x) =>
            x.user.id !== s.me?.id &&
            (x.user.username.toLowerCase().startsWith(ac.prefix.toLowerCase()) ||
              x.user.display_name.toLowerCase().includes(ac.prefix.toLowerCase())),
        )
        .slice(0, 8)
    : [];

  const pickAc = (item: MemberView) => {
    if (!ac) return;
    const caret = taRef.current?.selectionStart ?? text.length;
    const next = text.slice(0, ac.start) + "@" + item.user.username + " " + text.slice(caret);
    setText(next);
    if (!s.editingMessage) setDraft(conv.id, next);
    setAc(null);
    taRef.current?.focus();
  };

  const doSearch = (q: string) => {
    setSearchQ(q);
    if (!q.trim()) {
      setSearchRes(null);
      return;
    }
    convs
      .search(conv.id, q.trim())
      .then(setSearchRes)
      .catch(() => setSearchRes([]));
  };

  // 跳到消息：目标不在本地时拉取以其为中心的窗口（回复预览/搜索结果共用）
  const jumpToRef = (id: string, seq: number) => {
    const token = ++jumpRequest.current;
    void jumpToMessage(id, seq, token);
  };

  const jumpToMessage = async (id: string, seq: number, token: number) => {
    setShowSearch(false);
    setFlashId(id);
    if (!(getState().messages[conv.id] ?? []).some((x) => x.id === id)) {
      try {
        // 补到缓存最早一条为止，不留断档；缓存为空时补到最新（随后的首屏历史与之重叠）
        const cached = (getState().messages[conv.id] ?? []).filter((x) => !x.id.startsWith("client:"));
        const stop = cached[0]?.seq ?? conv.last_seq + 1;
        const got = await fetchForward(
          (after) => convs.messages(conv.id, { after_seq: after, limit: 200 }),
          Math.max(0, seq - 25),
          stop,
        );
        if (token !== jumpRequest.current) return;
        // P1-11：按 id/seq 合并进现有窗口，不能整体替换（会丢弃实时/乐观消息）
        setState((st) => {
          const merged = got.reduce(
            (l, x) => upsertMessage(l, x),
            st.messages[conv.id] ?? [],
          );
          return {
            messages: { ...st.messages, [conv.id]: merged },
            hasMore: { ...st.hasMore, [conv.id]: (merged[0]?.seq ?? 1) > 1 },
          };
        });
      } catch (e) {
        console.warn(e);
      }
    }
    stickBottom.current = false;
    const scrollTo = () => {
      if (token !== jumpRequest.current) return;
      scrollRef.current
        ?.querySelector(`[data-mid="${id}"]`)
        ?.scrollIntoView({ block: "center" });
    };
    setTimeout(scrollTo, 60);
    setTimeout(scrollTo, 300);
    setTimeout(() => {
      if (token === jumpRequest.current) setFlashId(null);
    }, 2600);
  };

  const jump = (m: MessageView) => jumpToRef(m.id, m.seq);

  /** 回复预览点击 → 定位原消息（删除的墓碑仍可定位） */
  const jumpPreview = (p: ReplyPreview) => jumpToRef(p.message_id, p.seq);

  const openPreview = (att: AttachmentView, m?: MessageView, rec?: { outerId: string; attachmentId: string }) => {
    previewAt.current = { at: Date.now(), touch: lastPointer.current.type === "touch" };
    setPreview({ att, m: m ?? null, rec });
    setPreviewText(null);
    // 撤回件复用的乐观附件（att.recalled）：公共 URL 已 404，不拉取
    if (isTextLike(att) && !att.recalled) {
      fetch(files.url(att.id))
        .then((r) => r.text())
        .then((t) => setPreviewText(t.slice(0, 300_000)))
        .catch(() => setPreviewText("（加载失败）"));
    }
  };

  const onFile = (e: React.ChangeEvent<HTMLInputElement>) => {
    // files 是活的 FileList：必须先拷成数组再清 value，否则清空后列表跟着归零
    const picked = Array.from(e.target.files ?? []);
    e.target.value = "";
    if (!picked.length || readonly) return;
    addPending(picked);
  };

  // 外面复制的文件/截图直接粘贴进待发区
  const onPaste = (e: React.ClipboardEvent) => {
    const fl = e.clipboardData?.files;
    if (fl && fl.length > 0) {
      e.preventDefault();
      if (!readonly) addPending(fl);
    }
  };

  const onInput = (v: string) => {
    setText(v);
    // V2 P1-3：编辑态下输入框是被编辑消息内容，不能当草稿存
    if (!s.editingMessage) setDraft(conv.id, v);
    // @补全：光标前最近的 @ 前缀，只在群里
    const caret = taRef.current?.selectionStart ?? v.length;
    const mm = /(^|[\s\n])@([A-Za-z0-9_]*)$/.exec(v.slice(0, caret));
    setAc(conv.kind === "group" && mm ? { start: caret - mm[2].length - 1, prefix: mm[2] } : null);
    const now = Date.now();
    // self 会话不广播 typing（服务端同样短路兜底）
    if (conv.kind !== "self" && now - lastTypingSent.current > 2500) {
      lastTypingSent.current = now;
      sendTyping(conv.id);
    }
  };

  return (
    <main
      className="chat-view"
      // 全视图捕获 pointerType：lastPointer 供 contextmenu 来源判定与
      // openPreview 的幽灵点击豁免（触屏打开才豁免）使用，不限于气泡区域
      onPointerDownCapture={(e) => {
        lastPointer.current = { type: e.pointerType ?? "", ts: Date.now() };
      }}
    >
      <header className="chat-head">
        <button className="chat-back" aria-label="返回" onClick={() => setState({ activeConv: null })}>
          ‹
        </button>
        {conv.kind === "dm" && conv.peer && !conv.peer.is_bot ? (
          <Avatar
            size="sm"
            userId={conv.peer.id}
            name={conv.peer.display_name}
            avatarVersion={conv.peer.avatar_version}
            hasAvatar={conv.peer.has_avatar}
          >
            {peerOnline && <i className="online-dot" />}
          </Avatar>
        ) : conv.kind === "self" ? (
          <div className="avatar sm self-avatar">
            <FolderSyncIcon size={16} />
          </div>
        ) : conv.kind === "dm" && conv.peer?.is_bot ? (
          <div className="avatar sm bot-avatar">
            <BotIcon />
          </div>
        ) : (
          <div className="avatar sm">{convAvatarText(conv)}</div>
        )}
        <div className="chat-head-main">
          <div className="chat-title">{title}</div>
          <div className="chat-status">
            {conv.kind === "self"
              ? "在你的设备间同步消息与文件"
              : conv.kind === "dm"
                ? conv.peer?.is_bot
                  ? "机器人"
                  : peerOnline
                    ? "在线"
                    : conv.peer?.last_seen_at
                      ? `最后在线 ${new Date(conv.peer.last_seen_at).toLocaleString()}`
                      : "离线"
                : `${conv.member_count} 名成员`}
          </div>
        </div>
        {meetingBase && conv.kind !== "self" && !readonly && (
          <button className="icon-btn chat-head-btn" title="发起会议" onClick={startMeeting}>
            📹 发起会议
          </button>
        )}
        <button
          className="icon-btn chat-head-btn"
          title="搜索聊天记录"
          aria-label="搜索聊天记录"
          onClick={() => {
            setShowSearch(!showSearch);
            setSearchQ("");
            setSearchRes(null);
          }}
        >
          <SearchIcon size={16} />
        </button>
        <button
          className="icon-btn chat-head-btn"
          title={conv.kind === "group" ? "群聊信息" : "会话详情与文件"}
          aria-label={conv.kind === "group" ? "群聊信息" : "会话详情与文件"}
          onClick={() => setShowInfo(!showInfo)}
        >
          ⋯
        </button>
      </header>

      {conv.kind === "group" && (
        <AnnouncementBar
          key={conv.id}
          convId={conv.id}
          announcement={conv.announcement}
          canEdit={conv.owner_id === s.me?.id}
        />
      )}
      <PinnedBar
        convId={conv.id}
        pins={pins}
        readonly={!!readonly}
        onUnpin={unpinMsg}
        onJump={jumpToRef}
      />

      {showSearch && (
        <div className="search-panel">
          <input
            autoFocus
            placeholder="搜索聊天记录"
            value={searchQ}
            onChange={(e) => doSearch(e.target.value)}
          />
          <div className="search-res">
            {searchRes?.length === 0 && <div className="msg-sys">无结果</div>}
            {searchRes?.map((m) => (
              <div key={m.id} className="sr-item" onClick={() => jump(m)}>
                <span className="sr-sender">{m.sender_name}</span>
                <span className="sr-content">{m.content}</span>
                <span className="sr-time">
                  {new Date(m.created_at).toLocaleDateString()}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="msg-scroll chat-dot-bg" ref={scrollRef} onScroll={onScroll}>
        {loadingMore && (<><div className="msg-sys">加载历史…</div><Skeleton rows={2} /></>)}
        {!hasMore && msgs.length > 0 && <div className="msg-sys">— 没有更多了 —</div>}
        {msgs.map((m, i) => {
          const mine = isOwnMessage(m, s.me?.id);
          const pending = m.id.startsWith("client:");
          const prev = msgs[i - 1];
          const grouped = inBubbleGroup(m, prev);
          const showTime =
            !prev || new Date(m.created_at).getTime() - new Date(prev.created_at).getTime() > 300000;
          const msSelected = msMode && ms!.ids.has(m.id);
          // UI3：系统消息居中灰条（无头像/气泡/菜单）
          if (m.kind === "system" && !isTombstone(m)) {
            const code = parseSystemPayload(m.content)?.code;
            return (
              <div key={m.id}>
                {showTime && (
                  <div className="msg-sys">{new Date(m.created_at).toLocaleString()}</div>
                )}
                <div
                  className="system-bar"
                  data-testid="system-bar"
                  data-code={code ?? ""}
                >
                  {systemBarText(m.content)}
                </div>
              </div>
            );
          }
          return (
            <div key={m.id}>
              {showTime && (
                <div className="msg-sys">{new Date(m.created_at).toLocaleString()}</div>
              )}
              {/* MF2 多选：整行任意位置点击切换勾选——捕获阶段拦掉内层所有动作
                  （卡片/⋯/链接/附件预览/语音播放），只剩勾选切换。日期分隔条在行外不带勾选 */}
              <div
                className={`ms-wrap${msMode ? " on" : ""}${msSelected ? " sel" : ""}`}
                onClickCapture={
                  msMode
                    ? (e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        toggleMs(m);
                      }
                    : undefined
                }
              >
                {msMode && (
                  <span className="ms-col">
                    <input
                      type="checkbox"
                      className="ms-checkbox"
                      data-testid="ms-checkbox"
                      checked={msSelected}
                      disabled={!selectable(m)}
                      readOnly
                      tabIndex={-1}
                      aria-label="选择此消息"
                    />
                  </span>
                )}
                <div
                  className={`msg-row ${mine ? "mine" : ""} ${grouped ? "grouped" : ""} ${msSelected ? "ms-sel" : ""}`}
                  data-mid={m.id}
                  data-grouped={grouped ? "1" : "0"}
                >
                {!mine && (
                  grouped ? (
                    <div className="msg-avatar-spacer" aria-hidden />
                  ) : (
                    <Avatar
                      userId={m.sender_id}
                      name={m.sender_name}
                      avatarVersion={m.sender_avatar_version}
                      hasAvatar={m.sender_has_avatar}
                      size="sm"
                    />
                  )
                )}
                <div
                  className="msg-body"
                  onPointerDown={onBubblePointerDown}
                  onPointerUp={onBubblePointerUp}
                  onClick={onBubbleClick(m)}
                  onContextMenu={onMsgContextMenu(m)}
                >
                  {!mine && conv.kind === "group" && !grouped && (
                    <div className="msg-sender">{m.sender_name}</div>
                  )}
                  <div className="bubble-wrap">
                    {/* UI6：悬停显示时间 + 回复/复制/转发/更多 */}
                    {!FEATURE_HOVER_TOOLBAR && !isTombstone(m) && (
                      <div className="msg-actions" data-testid="msg-hover-actions">
                        <button type="button" title="回复" aria-label="回复" onClick={() => startReply(m)}>↩</button>
                        <button type="button" title="复制" aria-label="复制" onClick={() => void doCopy(m)}>复制</button>
                        {canForwardMsg(m) && (
                          <button type="button" title="转发" aria-label="转发" onClick={() => setFwdMsg(m)}>➦</button>
                        )}
                        <button
                          type="button"
                          title="更多"
                          aria-label="更多"
                          onClick={(e) => openMenu(m, e.clientX, e.clientY)}
                        >
                          ···
                        </button>
                      </div>
                    )}
                    <div className="msg-time" data-testid="msg-hover-time">
                      {new Date(m.created_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                    </div>
                    {/* MM2：悬停工具栏已下线（FEATURE_HOVER_TOOLBAR），代码保留 */}
                    {FEATURE_HOVER_TOOLBAR && (canActMsg(m) || canForwardMsg(m) || canPinMsg(m)) && (
                      <div className={`msg-actions ${emojiPick === m.id ? "open" : ""}`}>
                        {emojiPick === m.id && canActMsg(m) ? (
                          <>
                            {QUICK_EMOJI.map((e) => (
                              <button
                                key={e}
                                className="rx-pick"
                                disabled={!!s.reactionPending[`${m.id}:${e}`]}
                                onClick={() => {
                                  setEmojiPick(null);
                                  void toggleReaction(m, e);
                                }}
                              >
                                {e}
                              </button>
                            ))}
                            <button
                              className="rx-pick"
                              title="收起"
                              onClick={() => setEmojiPick(null)}
                            >
                              ✕
                            </button>
                          </>
                        ) : (
                          <>
                            {canActMsg(m) && (
                              <>
                                <button
                                  title="表情"
                                  aria-label="表情"
                                  onClick={() => setEmojiPick(m.id)}
                                >
                                  😊
                                </button>
                                <button title="回复" aria-label="回复" onClick={() => startReply(m)}>
                                  ↩
                                </button>
                                {canEditMsg(m) && (
                                  <button
                                    title="编辑"
                                    aria-label="编辑"
                                    onClick={() => startEdit(m)}
                                  >
                                    ✏️
                                  </button>
                                )}
                                {canRecall(m, menuCtx()) && (
                                  <button
                                    title="撤回"
                                    aria-label="撤回"
                                    onClick={() => void doRecall(m)}
                                  >
                                    🗑
                                  </button>
                                )}
                              </>
                            )}
                            {canPinMsg(m) && (
                              <button
                                title={pinnedIds.has(m.id) ? "取消置顶" : "置顶"}
                                aria-label={pinnedIds.has(m.id) ? "取消置顶" : "置顶"}
                                onClick={() => void doPin(m)}
                              >
                                📌
                              </button>
                            )}
                            {canForwardMsg(m) && (
                              <>
                                <button
                                  title="转发"
                                  aria-label="转发"
                                  onClick={() => setFwdMsg(m)}
                                >
                                  ➦
                                </button>
                                <button
                                  title="收藏"
                                  aria-label="收藏"
                                  onClick={() => void doFavorite(m)}
                                >
                                  ⭐
                                </button>
                              </>
                            )}
                          </>
                        )}
                      </div>
                    )}
                    {isTombstone(m) ? (
                      <div
                        className={`bubble deleted ${flashId === m.id ? "flash" : ""}`}
                        data-testid="msg-tombstone"
                      >
                        {tombstoneText(m, { meId: s.me?.id, convType: conv.kind })}
                        {(() => {
                          // MM2：本地留有原文且在 10 分钟窗口内 → 「重新编辑」
                          const entry = reedits.find(
                            (e) => e.message_id === m.id && reeditFresh(e),
                          );
                          return entry ? (
                            <button
                              type="button"
                              className="reedit-link"
                              data-testid="msg-reedit"
                              onClick={() => void reeditMsg(entry)}
                            >
                              重新编辑
                            </button>
                          ) : null;
                        })()}
                      </div>
                    ) : (
                      <div
                        className={`bubble ${pending ? "pending" : ""} ${
                          m.mentions.includes(s.me?.id ?? "") ? "mentioned" : ""
                        } ${flashId === m.id ? "flash" : ""}`}
                      >
                        {m.forwarded_from && (
                          <div className="fwd-from">转发自 {m.forwarded_from}</div>
                        )}
                        {(m.reply_to_preview || m.reply_to_id) && (
                          <button
                            type="button"
                            className="reply-quote"
                            onClick={() => m.reply_to_preview && jumpPreview(m.reply_to_preview)}
                          >
                            <span className="rq-name">
                              {m.reply_to_preview?.sender_name || "回复"}
                            </span>
                            <span className="rq-text">
                              {quoteBodyText(m.reply_to_preview, {
                                forwarded: !!m.forwarded_from,
                                kindFallback: m.reply_to_preview
                                  ? kindLabel(m.reply_to_preview.kind)
                                  : undefined,
                              })}
                            </span>
                          </button>
                        )}
                        {m.attachment &&
                          m.kind === "image" &&
                          (() => {
                            const att = m.attachment;
                            if (!att) return null;
                            // 复用发送的乐观附件（att.recalled）：公共 URL 已 404，
                            // 只渲染 chip 取回的 objectURL；没取回画中性占位，不闪破图
                            const src = attImgSrc(
                              att,
                              att.recalled ? reuseUrls[att.id] : undefined,
                              files.url(att.id, { thumb: !!att.has_thumb }),
                            );
                            return src ? (
                              <img
                                className="msg-img"
                                src={src}
                                alt={att.file_name}
                                style={imgDim(att)}
                                data-own-action
                                onClick={() => openPreview(att, m)}
                              />
                            ) : (
                              <div
                                className="msg-img-ph"
                                style={imgDim(att)}
                                aria-label={att.file_name}
                              >
                                🖼
                              </div>
                            );
                          })()}
                        {m.attachment && m.kind === "audio" && (
                          <AudioBubble att={m.attachment} />
                        )}
                        {m.attachment && m.kind === "file" && (
                          <div
                            className="msg-file"
                            data-own-action
                            // 复用在途的撤回件：公共 URL 已 404，预览没有意义
                            onClick={
                              m.attachment.recalled
                                ? undefined
                                : () => openPreview(m.attachment!, m)
                            }
                          >
                            <span className="file-ic">📄</span>
                            <span className="file-meta">
                              <b>{m.attachment.file_name}</b>
                              <i>{fmtSize(m.attachment.size)}</i>
                            </span>
                          </div>
                        )}
                        {m.kind === "chat_record" ? (
                          // MF2 聊天记录卡片：点按/回车开查看页；⋯ 开动作面板（无长按菜单）
                          <ChatRecordCard
                            m={m}
                            onOpen={() => openRecord(m)}
                            onMore={() => openSheet(m)}
                          />
                        ) : m.kind === "meeting" ? (
                          <div className="meeting-card" data-testid="meeting-card">
                            <div className="meeting-card-title">视频会议</div>
                            <div className="meeting-card-link" title={m.content}>
                              {parseMeetingUrl(m.content) ?? m.content}
                            </div>
                            <div className="meeting-card-actions">
                              <button
                                type="button"
                                className="primary"
                                data-testid="meeting-join"
                                onClick={() => {
                                  const u = parseMeetingUrl(m.content);
                                  if (u) openExternal(u);
                                }}
                              >
                                加入
                              </button>
                              <button
                                type="button"
                                data-testid="meeting-copy"
                                onClick={() => void copyPlainText(m.content)}
                              >
                                复制链接
                              </button>
                            </div>
                          </div>
                        ) : m.kind === "poll" && m.poll ? (
                          <PollCard
                            m={m}
                            conv={conv}
                            members={members}
                            busy={!!pollBusy[m.id]}
                            setBusy={(v) =>
                              setPollBusy((b) => ({ ...b, [m.id]: v }))
                            }
                            onErr={setMutErr}
                          />
                        ) : (
                          showText(m) && <RichText text={m.content} mentions={!m.forwarded_from} />
                        )}
                        {m.edited_at && <span className="edited-mark">（已编辑）</span>}
                      </div>
                    )}
                    {(m.reactions?.length ?? 0) > 0 && (
                      <div className="reactions">
                        {m.reactions!.map((r) => (
                          <button
                            key={r.emoji}
                            className={`rx-chip ${r.reacted_by_me ? "me" : ""}`}
                            disabled={!!s.reactionPending[`${m.id}:${r.emoji}`]}
                            title={r.reacted_by_me ? "取消表态" : "表态"}
                            onClick={() => void toggleReaction(m, r.emoji)}
                          >
                            {r.emoji}
                            <span className="rx-count">{r.count}</span>
                          </button>
                        ))}
                      </div>
                    )}
                    {pending && m.send_error && (
                      <div className="send-fail">
                        <span>发送失败：{m.send_error}</span>
                        <button type="button" onClick={() => retrySend(m.id.slice("client:".length))}>
                          重试
                        </button>
                        <button type="button" onClick={() => discardSend(m.id.slice("client:".length))}>
                          删除
                        </button>
                      </div>
                    )}
                  </div>
                </div>
                </div>
              </div>
            </div>
          );
        })}
        {typingFresh && <div className="msg-sys">{typing.user.display_name} 正在输入…</div>}
      </div>

      {msMode ? (
        // MF2 多选底栏：已选数 + 逐条/合并转发 + 取消（替代 composer；转发 0 条时禁用）
        <footer className="ms-bar" data-testid="ms-bar">
          <span className="ms-count">已选 {ms!.ids.size} 条</span>
          <span className="ms-actions">
            <button
              type="button"
              data-testid="ms-forward-separate"
              disabled={ms!.ids.size === 0}
              onClick={() => openBatchForward("separate")}
            >
              逐条转发
            </button>
            <button
              type="button"
              data-testid="ms-forward-merged"
              disabled={ms!.ids.size === 0}
              onClick={() => openBatchForward("merged")}
            >
              合并转发
            </button>
            <button type="button" data-testid="ms-cancel" onClick={exitMultiSelect}>
              取消
            </button>
          </span>
        </footer>
      ) : (
      <footer className="composer">
        {readonly ? (
          <div className="readonly-hint">对方已不是你的好友，会话只读</div>
        ) : (
          <>
            <input ref={fileInput} type="file" hidden multiple onChange={onFile} />
            <input ref={imgInput} type="file" accept="image/*" hidden multiple onChange={onFile} />
            {showEmoji && (
              <div className="composer-popover" onClick={(e) => e.stopPropagation()}>
                <div className="emoji-grid">
                  {[
                    "😀", "😄", "😂", "🥰", "😎", "🤔", "😮", "😭",
                    "👍", "👏", "🙏", "🎉", "🔥", "❤️", "✨", "🚀",
                    "👀", "✅", "❌", "⚠️", "💡", "💻", "📦", "🐛",
                  ].map((em) => (
                    <button
                      key={em}
                      type="button"
                      className="emoji-btn"
                      onClick={() => {
                        onInput(text + em);
                        setShowEmoji(false);
                        taRef.current?.focus();
                      }}
                    >
                      {em}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {showCode && (
              <div className="composer-popover code-popover" onClick={(e) => e.stopPropagation()}>
                <div className="row between" style={{ alignItems: "center" }}>
                  <b style={{ fontSize: 13 }}>插入代码片段</b>
                  <select value={codeLang} onChange={(e) => setCodeLang(e.target.value)} style={{ width: "auto" }}>
                    <option value="ts">TypeScript</option>
                    <option value="js">JavaScript</option>
                    <option value="py">Python</option>
                    <option value="rust">Rust</option>
                    <option value="bash">Bash</option>
                    <option value="sql">SQL</option>
                    <option value="json">JSON</option>
                  </select>
                </div>
                <textarea
                  rows={5}
                  placeholder="粘贴或输入代码…"
                  value={codeBody}
                  onChange={(e) => setCodeBody(e.target.value)}
                />
                <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
                  <button type="button" className="btn-ghost" onClick={() => setShowCode(false)}>
                    取消
                  </button>
                  <button
                    type="button"
                    className="btn-accent"
                    disabled={!codeBody.trim()}
                    onClick={() => {
                      const block = `\`\`\`${codeLang}\n${codeBody.trimEnd()}\n\`\`\``;
                      onInput(text ? `${text.trimEnd()}\n${block}` : block);
                      setCodeBody("");
                      setShowCode(false);
                      taRef.current?.focus();
                    }}
                  >
                    插入到输入框
                  </button>
                </div>
              </div>
            )}
            {s.replyingTo && (
              <div className="composer-banner">
                <span className="cb-label">回复 {s.replyingTo.sender_name}</span>
                <span className="cb-text">
                  {s.replyingTo.deleted_at
                    ? msgDeletedText(s.replyingTo)
                    : s.replyingTo.kind === "chat_record"
                      ? recordLabel(s.replyingTo)
                      : s.replyingTo.attachment
                        ? kindLabel(s.replyingTo.kind)
                        : s.replyingTo.content.slice(0, 60)}
                </span>
                <button
                  className="icon-btn"
                  title="取消回复"
                  aria-label="取消回复"
                  onClick={() => setState({ replyingTo: null })}
                >
                  ✕
                </button>
              </div>
            )}
            {s.editingMessage && (
              <div className="composer-banner editing">
                <span className="cb-label">正在编辑消息</span>
                <span className="cb-text">{s.editingMessage.content.slice(0, 60)}</span>
                <button
                  className="icon-btn"
                  title="取消编辑"
                  aria-label="取消编辑"
                  onClick={cancelEdit}
                >
                  ✕
                </button>
              </div>
            )}
            {mutErr && <div className="mut-err">{mutErr}</div>}
            {/* MM2：撤回重编辑挂上的「复用附件」chip；✕ 或成功发送才摘（清文字不摘） */}
            {reuse && (
              <div
                className={`reuse-chip ${reuse.expired ? "expired" : ""}`}
                data-testid="reuse-chip"
              >
                {reuse.kind === "image" && reuseUrls[reuse.attachment_id] ? (
                  <img
                    className="rc-thumb"
                    src={reuseUrls[reuse.attachment_id]}
                    alt=""
                  />
                ) : (
                  <span className="rc-ic">📎</span>
                )}
                <span className="rc-name" title={reuse.file_name}>
                  {reuse.file_name}
                </span>
                {reuse.expired && <span className="rc-expired">附件已过期</span>}
                <button
                  type="button"
                  className="rc-x"
                  aria-label="移除附件"
                  onClick={() => setDraftReuse(conv.id, null)}
                >
                  ✕
                </button>
              </div>
            )}
            {acItems.length > 0 && (
              <div className="ac-list">
                {acItems.map((item) => (
                  <div
                    key={item.user.id}
                    className="ac-item"
                    onMouseDown={(e) => {
                      e.preventDefault();
                      pickAc(item);
                    }}
                  >
                    <span className="ac-name">@{item.user.username}</span>
                    <span className="ac-disp">{item.user.display_name}</span>
                  </div>
                ))}
              </div>
            )}
            {pendingFiles.length > 0 && (
              <div className="pending-files">
                {pendingFiles.map((f, i) => (
                  <span key={i} className="pf-chip" title={f.name}>
                    {f.type.startsWith("image/") ? "🖼" : "📄"}
                    <span className="pf-name">{f.name}</span>
                    <i>{fmtSize(f.size)}</i>
                    <b
                      onClick={() =>
                        setPendingFiles((p) => p.filter((_, j) => j !== i))
                      }
                    >
                      ✕
                    </b>
                  </span>
                ))}
              </div>
            )}
            {sendErr && <div className="pf-err">{sendErr}</div>}
            <div className="composer-box">
              <textarea
                ref={taRef}
                value={text}
                placeholder="输入消息，Enter 发送，Shift+Enter 换行"
                onChange={(e) => onInput(e.target.value)}
                onPaste={onPaste}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    if (acItems.length > 0) {
                      pickAc(acItems[0]);
                    } else {
                      send();
                    }
                  }
                  if (e.key === "Escape") {
                    if (ac) setAc(null);
                    else if (emojiPick) setEmojiPick(null);
                    else if (s.editingMessage) cancelEdit();
                    else if (s.replyingTo) setState({ replyingTo: null });
                  }
                }}
                rows={3}
              />
              <div className="composer-tools">
                <button
                  type="button"
                  className="icon-btn"
                  title="表情"
                  aria-label="表情"
                  onClick={() => {
                    setShowEmoji((v) => !v);
                    setShowCode(false);
                  }}
                >
                  <SmileIcon size={18} />
                </button>
                <button
                  type="button"
                  className="icon-btn"
                  title="发送图片"
                  aria-label="发送图片"
                  onClick={() => imgInput.current?.click()}
                >
                  <ImageIcon size={18} />
                </button>
                <button
                  type="button"
                  className="icon-btn"
                  title="发送文件（也可直接粘贴）"
                  aria-label="发送文件"
                  onClick={() => fileInput.current?.click()}
                >
                  <PaperclipIcon />
                </button>
                <button
                  type="button"
                  className="icon-btn"
                  title="插入代码片段"
                  aria-label="插入代码片段"
                  onClick={() => {
                    setShowCode((v) => !v);
                    setShowEmoji(false);
                  }}
                >
                  <CodeIcon size={18} />
                </button>
                <VoiceRecorder
                  convId={conv.id}
                  disabled={sending || mutBusy}
                  onError={setMutErr}
                />
                {conv.kind === "group" && (
                  <button
                    className="icon-btn"
                    title="发起投票"
                    aria-label="发起投票"
                    onClick={() => setShowPoll(true)}
                  >
                    <PollIcon />
                  </button>
                )}
                <button
                  className="primary send-btn"
                  onClick={send}
                  disabled={
                    sending ||
                    mutBusy ||
                    (!text.trim() && pendingFiles.length === 0 && !(reuse && !reuse.expired))
                  }
                >
                  {s.editingMessage ? (mutBusy ? "保存中…" : "保存") : sending ? "发送中…" : "发送"}
                </button>
              </div>
            </div>
          </>
        )}
      </footer>
      )}
      {showInfo && (
        <>
          <button
            type="button"
            className="slide-mask"
            aria-label="关闭面板"
            data-testid="slide-mask"
            onClick={() => setShowInfo(false)}
          />
          <GroupInfo
            conv={conv}
            onClose={() => setShowInfo(false)}
            onPreview={openPreview}
          />
        </>
      )}
      {showPoll && conv.kind === "group" && (
        <NewPollModal convId={conv.id} onClose={() => setShowPoll(false)} />
      )}
      {fwdMsg && (
        <ForwardModal message={fwdMsg} onClose={() => setFwdMsg(null)} />
      )}
      {/* MF2 批量转发：确认后走 forward-batch（结果 toast + 旧服务端兜底在调用方） */}
      {fwdBatch && (
        <ForwardModal
          submit={{
            title: `${fwdBatch.mode === "merged" ? "合并转发" : "逐条转发"} ${fwdBatch.ids.length} 条消息`,
            run: (targets) => submitBatchForward(fwdBatch.mode, fwdBatch.ids, targets),
          }}
          onClose={() => setFwdBatch(null)}
        />
      )}
      {/* MF2 记录内附件条目转发：POST /messages/{outerId}/forward + record_attachment_id */}
      {fwdRecord && (
        <ForwardModal
          submit={{
            title: `转发附件 ${fwdRecord.fileName}`,
            run: (targets) => submitRecordForward(fwdRecord.outerId, fwdRecord.attachmentId, targets),
          }}
          onClose={() => setFwdRecord(null)}
        />
      )}
      {/* MF2 记录查看页（嵌套层下钻在组件 state；外层被撤回时显示不可用） */}
      {recordView && (
        <ChatRecordView
          view={{
            ...recordView,
            error: recordView.error || recordDead(recordView.outerId),
          }}
          onClose={() => setRecordView(null)}
          onPopLevel={popRecordLevel}
          onPushLevel={pushRecordLevel}
          onPreview={openRecordPreview}
        />
      )}

      {/* 附件预览 / 大图查看器（MM2：带动作工具栏；工具栏项与消息菜单共用 items 函数）。
          触屏打开后 GHOST_CLICK_MS 内的 click 按幽灵点击忽略（兼容 click 可能落到
          刚渲染的工具栏按钮上）；鼠标/笔打开的预览永不忽略点击 */}
      {preview && (
        // Z.modal：记录查看页(Z.recordView)之上、转发弹窗(Z.forwardModal)之下
        <div
          className="modal-mask"
          style={{ zIndex: Z.modal }}
          onClick={() => {
            if (!isGhostClick(previewAt.current)) setPreview(null);
          }}
        >
          <div className="pv-modal" onClick={(e) => e.stopPropagation()}>
            <div className="pv-head">
              <span className="pv-name" title={preview.att.file_name}>
                {preview.att.file_name}
              </span>
              <span className="muted">{fmtSize(preview.att.size)}</span>
              <span className="pv-toolbar">
                {/* 回复/转发/撤回与消息菜单同一来源（viewer-<action>）；多选不进预览工具栏 */}
                {preview.rec ? (
                  /* MF2-C8 记录上下文：恰为 下载 → 转发（关闭在右侧 pv-head 固定位置） */
                  <>
                    <button
                      type="button"
                      className="pv-act"
                      data-testid="viewer-download"
                      onClick={() => {
                        if (!isGhostClick(previewAt.current)) void saveAttachment(preview.att);
                      }}
                    >
                      <DownloadIcon />
                      <span>下载</span>
                    </button>
                    <button
                      type="button"
                      className="pv-act"
                      data-testid="viewer-forward"
                      onClick={() => {
                        if (isGhostClick(previewAt.current)) return;
                        setFwdRecord({
                          outerId: preview.rec!.outerId,
                          attachmentId: preview.rec!.attachmentId,
                          fileName: preview.att.file_name,
                        });
                      }}
                    >
                      <ForwardIcon />
                      <span>转发</span>
                    </button>
                  </>
                ) : (
                  <>
                    {preview.m &&
                      itemsFor(preview.m)
                        .filter((it) => ["reply", "forward", "recall"].includes(it.action))
                        .map((it) => (
                          <button
                            key={it.action}
                            type="button"
                            className={`pv-act ${it.danger ? "danger" : ""}`}
                            data-testid={`viewer-${it.action}`}
                            title={it.label}
                            onClick={() => {
                              if (isGhostClick(previewAt.current)) return;
                              const m = preview.m!;
                              setPreview(null);
                              runMenuAction(m, it.action);
                            }}
                          >
                            {menuIcon(it.action)}
                            <span>{it.label}</span>
                          </button>
                        ))}
                    {!preview.att.recalled && (
                      <button
                        type="button"
                        className="pv-act"
                        data-testid="viewer-download"
                        onClick={() => {
                          if (!isGhostClick(previewAt.current)) void saveAttachment(preview.att);
                        }}
                      >
                        <DownloadIcon />
                        <span>下载</span>
                      </button>
                    )}
                  </>
                )}
              </span>
              <button
                className="icon-btn"
                data-testid="viewer-close"
                onClick={() => {
                  if (!isGhostClick(previewAt.current)) setPreview(null);
                }}
              >
                ✕
              </button>
            </div>
            <div className="pv-body">
              {preview.att.mime.startsWith("image/") &&
                (() => {
                  const src = attImgSrc(
                    preview.att,
                    preview.att.recalled ? reuseUrls[preview.att.id] : undefined,
                    files.url(preview.att.id),
                  );
                  return src ? (
                    <img src={src} alt={preview.att.file_name} />
                  ) : (
                    <div className="pv-unsupported">附件加载中…</div>
                  );
                })()}
              {isTextLike(preview.att) && !preview.att.mime.startsWith("image/") && (
                <pre>{previewText ?? "加载中…"}</pre>
              )}
              {preview.att.mime === "application/pdf" && (
                <iframe src={files.url(preview.att.id)} title={preview.att.file_name} />
              )}
              {preview.att.mime.startsWith("audio/") && (
                <audio controls src={files.url(preview.att.id)} />
              )}
              {preview.att.mime.startsWith("video/") && (
                <video controls src={files.url(preview.att.id)} />
              )}
              {!preview.att.mime.startsWith("image/") &&
                !preview.att.mime.startsWith("audio/") &&
                !preview.att.mime.startsWith("video/") &&
                preview.att.mime !== "application/pdf" &&
                !isTextLike(preview.att) && (
                  <div className="pv-unsupported">暂不支持预览此格式，请下载后查看</div>
                )}
            </div>
          </div>
        </div>
      )}

      {/* MM2：统一消息菜单（图标+文字）；附件不再有单独的右键菜单 */}
      {msgMenu && (
        <CtxMenu
          x={msgMenu.x}
          y={msgMenu.y}
          onClose={() => {
            if (!isGhostClick(menuAt.current)) setMsgMenu(null);
          }}
        >
          {itemsFor(msgMenu.m).map((it) => (
            <div
              key={it.action}
              className={`ctx-item ${it.danger ? "danger" : ""}`}
              data-testid={`msg-menu-${it.action}`}
              onClick={() => {
                if (!isGhostClick(menuAt.current)) runMenuAction(msgMenu.m, it.action);
              }}
            >
              {menuIcon(it.action)}
              <span>{it.label}</span>
            </div>
          ))}
        </CtxMenu>
      )}

      {/* MM2：触屏点按 → 底部动作面板（同一套菜单项 + 取消）。
          打开后 GHOST_CLICK_MS 内的 click 按幽灵点击忽略（Android WebView 兼容 click
          会落到刚打开的面板项/遮罩上）；面板仅由触屏点按打开 */}
      {sheet && (
        <div
          className="sheet-mask"
          data-testid="action-sheet"
          onClick={() => {
            if (!isGhostClick(sheetAt.current)) setSheet(null);
          }}
        >
          <div className="action-sheet" onClick={(e) => e.stopPropagation()}>
            {itemsFor(sheet.m).map((it) => (
              <button
                key={it.action}
                type="button"
                className={`sheet-item ${it.danger ? "danger" : ""}`}
                data-testid={`msg-menu-${it.action}`}
                onClick={() => {
                  if (!isGhostClick(sheetAt.current)) runMenuAction(sheet.m, it.action);
                }}
              >
                {menuIcon(it.action)}
                <span>{it.label}</span>
              </button>
            ))}
            <button
              type="button"
              className="sheet-item sheet-cancel"
              onClick={() => {
                if (!isGhostClick(sheetAt.current)) setSheet(null);
              }}
            >
              取消
            </button>
          </div>
        </div>
      )}

      {/* 轻量操作反馈 */}
      {hint && <div className="op-hint">{hint}</div>}
    </main>
  );
}
