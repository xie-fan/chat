import {
  type CSSProperties,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  cancelSessionLocalFirst,
  deleteSessionLocalFirst,
  deleteWorkspaceLocalFirst,
  downloadAcpAttachmentLocalFirst,
  fetchAcpAttachmentBlobLocalFirst,
  gitCheckoutLocalFirst,
  isLocalAcpHost,
  mutateSessionLocalFirst,
  probeAgentModelsLocalFirst,
  readHostFileLocalFirst,
  replyPermissionLocalFirst,
  sendPromptLocalFirst,
  uploadAttachmentLocalFirst,
} from "../../acpHost";
import { acp } from "../../api";
import { applyAcpBootstrap, applyAcpEvent, getState, setState, useApp } from "../../store";
import type {
  AcpActivityPhase,
  AcpAttachment,
  AcpPermissionPolicy,
  AcpThinkingLevel,
  AcpUsageStats,
} from "../../types";
import FilePreview, {
  type FilePreviewTarget,
  useIsMobilePreview,
} from "../FilePreview";
import { confirmDialog } from "../Modal";
import { pushBackLayer } from "../chat/backStack";
import { parseFileReference, type ParsedFileRef } from "../filePathRef";
import {
  ActivityIcon,
  ArrowDownIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CpuIcon,
  FolderIcon,
  GitBranchIcon,
  MoreVerticalIcon,
  PlusIcon,
  TrashIcon,
  XIcon,
} from "../icons";
import {
  AddWorkspaceModal,
  AttachAgentModal,
  NewSessionModal,
} from "./AcpModals";
import {
  AcpTurnItem,
  groupTurnsForDisplay,
  ProcessGroupCard,
} from "./AcpTurnView";

const THINKING_LEVELS: AcpThinkingLevel[] = ["关", "低", "中", "高"];
const PERMISSION_POLICIES: { v: AcpPermissionPolicy; label: string }[] = [
  { v: "ask", label: "权限：每次询问" },
  { v: "auto_read", label: "权限：自动只读" },
  { v: "allow_all", label: "权限：全部允许" },
];
const ACTIVITY_PHASE_LABELS: Record<AcpActivityPhase, string> = {
  connecting: "连接中",
  thinking: "深度思考",
  tooling: "执行工具",
  responding: "生成回复",
  waiting_permission: "等待授权",
  compacting: "压缩上下文",
  retrying: "重试请求",
  idle: "就绪",
};

function formatCompactNum(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(Math.round(n));
}

function formatUsageSummary(u?: AcpUsageStats): string | null {
  if (!u) return null;
  const parts: string[] = [];
  if (typeof u.total_tokens === "number") {
    parts.push(`${formatCompactNum(u.total_tokens)} tok`);
  } else if (typeof u.input_tokens === "number" || typeof u.output_tokens === "number") {
    const inp = typeof u.input_tokens === "number" ? `↑${formatCompactNum(u.input_tokens)}` : "";
    const out = typeof u.output_tokens === "number" ? `↓${formatCompactNum(u.output_tokens)}` : "";
    const joined = [inp, out].filter(Boolean).join(" ");
    if (joined) parts.push(joined);
  }
  if (typeof u.context_used === "number" && typeof u.context_size === "number" && u.context_size > 0) {
    const pct = Math.min(100, Math.round((u.context_used / u.context_size) * 100));
    parts.push(`ctx ${pct}%`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

export default function AcpWorkspaceView() {
  const s = useApp();
  const [hostMenuOpen, setHostMenuOpen] = useState(false);
  const [collapsedWs, setCollapsedWs] = useState<Set<string>>(() => new Set());
  const [showAddWs, setShowAddWs] = useState(false);
  const [newSessWsId, setNewSessWsId] = useState<string | null>(null);
  const [showAttach, setShowAttach] = useState(false);

  const [promptText, setPromptText] = useState("");
  const [pendingAtts, setPendingAtts] = useState<AcpAttachment[]>([]);
  const [uploading, setUploading] = useState(false);
  const [sending, setSending] = useState(false);
  const [errMsg, setErrMsg] = useState("");
  const [diffsOpen, setDiffsOpen] = useState(false);
  const [rpcOpen, setRpcOpen] = useState(false);
  const [stickToBottom, setStickToBottom] = useState(true);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [previewTarget, setPreviewTarget] = useState<FilePreviewTarget | null>(null);
  const [previewSeq, setPreviewSeq] = useState(0);
  const [previewCollapsed, setPreviewCollapsed] = useState(false);
  const isMobilePreview = useIsMobilePreview();

  const fileRef = useRef<HTMLInputElement>(null);
  const streamRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const stickRef = useRef(true);
  const runAnchorRef = useRef<number>(Date.now());
  const probedAgentAtRef = useRef<Record<string, number>>({});

  // 若进入 Agent 工作区时尚未 bootstrap，则补拉一次状态
  useEffect(() => {
    if (!s.acpLoaded) {
      void acp
        .bootstrap()
        .then((bs) => setState((cur) => applyAcpBootstrap(cur, bs)))
        .catch(() => {});
    }
  }, [s.acpLoaded]);

  const activeHost = useMemo(
    () =>
      s.acpHosts.find((h) => h.id === s.activeAcpHostId) ??
      s.acpHosts[0] ??
      null,
    [s.acpHosts, s.activeAcpHostId],
  );

  const hostWorkspaces = useMemo(
    () =>
      activeHost
        ? s.acpWorkspaces.filter((w) => w.host_id === activeHost.id)
        : s.acpWorkspaces,
    [s.acpWorkspaces, activeHost],
  );

  const activeSession = useMemo(
    () => s.acpSessions.find((x) => x.id === s.activeAcpSessionId) ?? null,
    [s.acpSessions, s.activeAcpSessionId],
  );

  const activeWs = useMemo(
    () =>
      (activeSession &&
        s.acpWorkspaces.find((w) => w.id === activeSession.workspace_id)) ??
      hostWorkspaces.find((w) => w.id === s.activeAcpWorkspaceId) ??
      hostWorkspaces[0] ??
      null,
    [activeSession, s.acpWorkspaces, hostWorkspaces, s.activeAcpWorkspaceId],
  );

  const activeAgent = useMemo(
    () =>
      (activeSession &&
        s.acpAgents.find((a) => a.id === activeSession.agent_id)) ??
      null,
    [activeSession, s.acpAgents],
  );

  const activeHostOfSess = useMemo(
    () =>
      (activeWs && s.acpHosts.find((h) => h.id === activeWs.host_id)) ??
      activeHost,
    [activeWs, s.acpHosts, activeHost],
  );

  const activeActivity = activeSession ? s.acpActivity[activeSession.id] : undefined;

  useEffect(() => {
    setPendingAtts([]);
    setErrMsg("");
    setEditingTitle(false);
    setDiffsOpen(false);
    setRpcOpen(false);
    setPreviewTarget(null);
    setPreviewCollapsed(false);
    stickRef.current = true;
    setStickToBottom(true);
  }, [activeSession?.id]);

  const mobilePreviewBlocksBack = Boolean(
    isMobilePreview && previewTarget && !previewCollapsed,
  );
  useEffect(
    () =>
      mobilePreviewBlocksBack
        ? pushBackLayer(() => {
            setPreviewTarget(null);
            setPreviewCollapsed(false);
          })
        : undefined,
    [mobilePreviewBlocksBack],
  );
  useEffect(() => {
    if (!mobilePreviewBlocksBack) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setPreviewTarget(null);
        setPreviewCollapsed(false);
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [mobilePreviewBlocksBack]);

  // 实时运行耗时计时器：优先对齐服务端/宿主的 activity startedAt
  useEffect(() => {
    if (!activeSession?.running) {
      setElapsedMs(0);
      return;
    }
    const anchor =
      activeActivity && activeActivity.phase !== "idle"
        ? activeActivity.startedAt
        : Date.now();
    runAnchorRef.current = anchor;
    setElapsedMs(Math.max(0, Date.now() - anchor));
    const timer = window.setInterval(() => {
      setElapsedMs(Math.max(0, Date.now() - runAnchorRef.current));
    }, 500);
    return () => window.clearInterval(timer);
  }, [
    activeSession?.id,
    activeSession?.running,
    activeActivity?.phase,
    activeActivity?.startedAt,
  ]);

  // 打开尚未开始对话的新会话时，本机直连（或远端后台）异步刷新该 Agent 的可用模型列表
  useEffect(() => {
    const hostAvailable =
      Boolean(activeHostOfSess?.online) || isLocalAcpHost(activeHostOfSess?.id);
    if (!activeSession || !activeAgent || activeSession.running || !hostAvailable) return;
    const isFreshSession =
      !activeSession.acp_session_key && (activeSession.turns ?? []).length === 0;
    if (!isFreshSession) return;
    const now = Date.now();
    const last = probedAgentAtRef.current[activeSession.agent_id] ?? 0;
    if (now - last < 15_000) return;
    probedAgentAtRef.current[activeSession.agent_id] = now;
    void probeAgentModelsLocalFirst(activeAgent, {
      sessionId: activeSession.id,
      cwd: activeWs?.cwd,
      wait: false,
    }).catch(() => {});
  }, [
    activeSession?.id,
    activeSession?.agent_id,
    activeSession?.acp_session_key,
    activeSession?.turns?.length,
    activeSession?.running,
    activeAgent,
    activeWs?.cwd,
    activeHostOfSess?.id,
    activeHostOfSess?.online,
  ]);

  const displayEntries = useMemo(
    () => groupTurnsForDisplay(activeSession?.turns ?? []),
    [activeSession?.turns],
  );

  const turnsScrollKey = useMemo(() => {
    const turns = activeSession?.turns ?? [];
    const last = turns[turns.length - 1];
    if (!last) return "0";
    const textLen = typeof last.payload.text === "string" ? last.payload.text.length : 0;
    const outLen = typeof last.payload.output === "string" ? last.payload.output.length : 0;
    return `${turns.length}:${last.status}:${last.updated_at}:${textLen}:${outLen}`;
  }, [activeSession?.turns]);

  useEffect(() => {
    const el = streamRef.current;
    if (el && stickRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [activeSession?.id, turnsScrollKey]);

  const onStreamScroll = () => {
    const el = streamRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const nearBottom = dist <= 80;
    if (nearBottom !== stickRef.current) {
      stickRef.current = nearBottom;
      setStickToBottom(nearBottom);
    }
  };

  const scrollToBottom = () => {
    const el = streamRef.current;
    stickRef.current = true;
    setStickToBottom(true);
    if (el) el.scrollTop = el.scrollHeight;
  };

  useLayoutEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    const nextH = promptText ? Math.min(Math.max(ta.scrollHeight, 52), 160) : 52;
    ta.style.height = `${nextH}px`;
  }, [promptText]);

  const selectHost = (hostId: string) => {
    setHostMenuOpen(false);
    const ws = s.acpWorkspaces.find((w) => w.host_id === hostId) ?? null;
    const sess = ws
      ? s.acpSessions.find((x) => x.workspace_id === ws.id) ?? null
      : null;
    setState({
      activeAcpHostId: hostId,
      activeAcpWorkspaceId: ws?.id ?? null,
      activeAcpSessionId: sess?.id ?? null,
    });
  };

  const toggleWs = (id: string) => {
    setCollapsedWs((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const removeWorkspace = async (wsId: string, label: string) => {
    if (
      !(await confirmDialog(`移除工作目录「${label}」及其下的会话记录？`, {
        danger: true,
        okText: "移除",
      }))
    )
      return;
    try {
      await deleteWorkspaceLocalFirst(wsId);
      setState((cur) => {
        const acpWorkspaces = cur.acpWorkspaces.filter((w) => w.id !== wsId);
        const acpSessions = cur.acpSessions.filter((x) => x.workspace_id !== wsId);
        return {
          acpWorkspaces,
          acpSessions,
          activeAcpWorkspaceId:
            cur.activeAcpWorkspaceId === wsId ? acpWorkspaces[0]?.id ?? null : cur.activeAcpWorkspaceId,
          activeAcpSessionId:
            cur.activeAcpSessionId &&
            acpSessions.some((x) => x.id === cur.activeAcpSessionId)
              ? cur.activeAcpSessionId
              : acpSessions[0]?.id ?? null,
        };
      });
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : "删除失败");
    }
  };

  const removeSession = async (sessId: string) => {
    if (
      !(await confirmDialog("删除该 Agent 对话及其历史记录？", {
        danger: true,
        okText: "删除",
      }))
    )
      return;
    try {
      await deleteSessionLocalFirst(sessId);
      setState((cur) => {
        const acpSessions = cur.acpSessions.filter((x) => x.id !== sessId);
        return {
          acpSessions,
          activeAcpSessionId:
            cur.activeAcpSessionId === sessId ? acpSessions[0]?.id ?? null : cur.activeAcpSessionId,
        };
      });
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : "删除失败");
    }
  };

  const patchSessionState = (updated: typeof activeSession) => {
    if (!updated) return;
    setState((cur) => applyAcpEvent(cur, "session_upserted", { session: updated }));
  };

  const uploadFilesList = async (picked: File[]) => {
    if (!picked.length || !activeSession) return;
    const targetSessId = activeSession.id;
    setUploading(true);
    setErrMsg("");
    try {
      const uploaded = await Promise.all(
        picked.map((f) => uploadAttachmentLocalFirst(f, targetSessId)),
      );
      if (getState().activeAcpSessionId === targetSessId) {
        setPendingAtts((cur) => [...cur, ...uploaded]);
      }
    } catch (ex) {
      if (getState().activeAcpSessionId === targetSessId) {
        setErrMsg(ex instanceof Error ? ex.message : "上传附件失败");
      }
    } finally {
      setUploading(false);
    }
  };

  const onPickFiles = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const picked = Array.from(e.target.files ?? []);
    e.target.value = "";
    await uploadFilesList(picked);
  };

  const onPasteComposer = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const items = Array.from(e.clipboardData?.items ?? []);
    const images: File[] = [];
    for (const item of items) {
      if (item.kind === "file" && item.type.startsWith("image/")) {
        const f = item.getAsFile();
        if (f) images.push(f);
      }
    }
    if (images.length > 0) {
      e.preventDefault();
      void uploadFilesList(images);
    }
  };

  const sendPrompt = async () => {
    if (!activeSession || activeSession.running || sending) return;
    const targetSessId = activeSession.id;
    const raw = promptText.trim();
    if (!raw && pendingAtts.length === 0) return;
    const fileRefs = Array.from(raw.matchAll(/@([^\s@]+)/g)).map((m) => m[1]);
    setSending(true);
    setErrMsg("");
    stickRef.current = true;
    setStickToBottom(true);
    try {
      const { localHandled, turn } = await sendPromptLocalFirst(
        activeSession,
        activeWs,
        activeAgent,
        {
          text: raw,
          files: fileRefs,
          attachments: pendingAtts,
          model: activeSession.selected_model,
          thinking_level: activeSession.thinking_level,
          permission_policy: activeSession.permission_policy,
        },
      );
      if (getState().activeAcpSessionId === targetSessId) {
        setPromptText("");
        setPendingAtts([]);
      }
      if (!localHandled) {
        setState((cur) => ({
          acpSessions: cur.acpSessions.map((x) => {
            if (x.id !== targetSessId) return x;
            const prevTurns = x.turns ?? [];
            const has = prevTurns.some((t) => t.id === turn.id);
            return {
              ...x,
              running: true,
              turns: has
                ? prevTurns.map((t) => (t.id === turn.id ? turn : t))
                : [...prevTurns, turn],
            };
          }),
        }));
      }
    } catch (ex) {
      setErrMsg(ex instanceof Error ? ex.message : "发送指令失败");
    } finally {
      setSending(false);
    }
  };

  const replyPermission = useCallback(
    async (
      turnId: string,
      opt: "allow_once" | "allow_always" | "reject_once" | "reject_always",
    ) => {
      const sessId = getState().activeAcpSessionId;
      if (!sessId) return;
      try {
        const updatedTurn = await replyPermissionLocalFirst(sessId, turnId, opt);
        setState((cur) => ({
          acpSessions: cur.acpSessions.map((sess) => ({
            ...sess,
            turns: (sess.turns ?? []).map((t) =>
              t.id === updatedTurn.id ? updatedTurn : t,
            ),
          })),
        }));
      } catch (ex) {
        setErrMsg(ex instanceof Error ? ex.message : "权限应答失败");
      }
    },
    [],
  );

  const downloadAttachment = useCallback((id: string, fileName: string) => {
    void downloadAcpAttachmentLocalFirst(id, fileName).catch((ex) =>
      setErrMsg(ex instanceof Error ? ex.message : "下载附件失败"),
    );
  }, []);

  const openAcpAttachmentPreview = useCallback(
    (att: AcpAttachment, badge?: string) => {
      setPreviewTarget({
        kind: "acp-attachment",
        key: `acp-att:${att.id}:${badge ?? ""}`,
        att,
        badge,
        loadBlob: () => fetchAcpAttachmentBlobLocalFirst(att),
        onDownload: () => downloadAttachment(att.id, att.file_name),
      });
      setPreviewSeq((n) => n + 1);
    },
    [downloadAttachment],
  );

  const previewSentAttachment = useCallback(
    (att: AcpAttachment) => openAcpAttachmentPreview(att),
    [openAcpAttachmentPreview],
  );

  const openHostPathPreview = useCallback(
    (ref: ParsedFileRef, badge?: string) => {
      const hostId = activeHostOfSess?.id ?? activeWs?.host_id ?? "";
      const cwd = activeWs?.cwd;
      setPreviewTarget({
        kind: "host-path",
        key: `host-path:${hostId}:${cwd ?? ""}:${ref.path}:${ref.lineStart ?? ""}:${ref.lineEnd ?? ""}`,
        path: ref.path,
        cwd,
        hostName: activeHostOfSess?.name,
        lineStart: ref.lineStart,
        lineEnd: ref.lineEnd,
        badge,
        loadHostFile: () => {
          if (!hostId) {
            return Promise.reject(new Error("当前对话未绑定机器节点"));
          }
          return readHostFileLocalFirst(hostId, ref.path, cwd);
        },
      });
      setPreviewSeq((n) => n + 1);
    },
    [activeHostOfSess?.id, activeHostOfSess?.name, activeWs?.cwd, activeWs?.host_id],
  );

  const pendingPromptFileRefs = useMemo(() => {
    if (!promptText) return [];
    const seen = new Set<string>();
    const out: ParsedFileRef[] = [];
    for (const m of promptText.matchAll(/(?:^|\s)@([^\s@]+)/g)) {
      const token = m[1];
      if (!token || seen.has(token)) continue;
      seen.add(token);
      out.push(
        parseFileReference(token, { fromMarkdownLink: true }) ?? {
          raw: token,
          path: token,
          display: token,
        },
      );
    }
    return out;
  }, [promptText]);

  const newSessionTargetWs =
    s.acpWorkspaces.find((w) => w.id === newSessWsId) ?? null;

  return (
    <main className="acp-shell">
      <aside className="acp-sidebar">
        <div className="acp-sidebar-top">
          <div className="acp-sidebar-title-row">
            <h2>Agent 工作区</h2>
            <button
              type="button"
              className="btn-ghost"
              style={{ padding: "5px 9px", fontSize: 12 }}
              onClick={() => setShowAttach(true)}
            >
              <PlusIcon size={13} /> 接入
            </button>
          </div>

          <div style={{ position: "relative" }}>
            <button
              type="button"
              className="acp-host-btn"
              onClick={() => setHostMenuOpen((v) => !v)}
            >
              <span style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                <span
                  className={`dot${activeHost?.online ? " on" : ""}`}
                  style={{
                    background: activeHost?.online ? "var(--ok)" : "var(--text-muted)",
                    flexShrink: 0,
                  }}
                />
                <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {activeHost ? `@${activeHost.name}` : "未接入主机"}
                </span>
              </span>
              <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                {activeHost?.online && (
                  <span className="muted" style={{ fontSize: 11 }}>
                    {activeHost.ip === "Local-Only" ? "仅本机" : `${activeHost.latency_ms}ms`}
                  </span>
                )}
                <ChevronDownIcon size={15} />
              </span>
            </button>

            {hostMenuOpen && (
              <div className="acp-host-dropdown" onClick={(e) => e.stopPropagation()}>
                {s.acpHosts.length === 0 ? (
                  <div className="muted" style={{ padding: 10, fontSize: 12 }}>
                    暂无已接入节点
                  </div>
                ) : (
                  s.acpHosts.map((h) => (
                    <button
                      key={h.id}
                      type="button"
                      className={`acp-host-opt${activeHost?.id === h.id ? " on" : ""}`}
                      onClick={() => selectHost(h.id)}
                    >
                      <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span
                          className="dot"
                          style={{
                            background: h.online ? "var(--ok)" : "var(--text-muted)",
                          }}
                        />
                        <span>@{h.name}</span>
                      </span>
                      <span className="muted" style={{ fontSize: 11 }}>
                        {h.ip === "Local-Only"
                          ? "仅本机"
                          : h.online
                            ? `${h.latency_ms}ms`
                            : "离线"}
                      </span>
                    </button>
                  ))
                )}
                <button
                  type="button"
                  className="acp-host-opt"
                  onClick={() => {
                    setHostMenuOpen(false);
                    setShowAttach(true);
                  }}
                >
                  <PlusIcon size={14} /> 接入机器或配置 Agent…
                </button>
              </div>
            )}
          </div>

          <button
            type="button"
            className="btn-accent"
            style={{ width: "100%" }}
            disabled={!activeHost}
            onClick={() => setShowAddWs(true)}
          >
            <FolderIcon size={15} /> 添加工作目录
          </button>
        </div>

        <div className="acp-tree">
          {hostWorkspaces.length === 0 ? (
            <div className="empty-state" style={{ padding: "28px 14px" }}>
              <FolderIcon size={26} />
              <div className="empty-title">暂无工作目录</div>
              <div className="empty-hint-sm">
                选择主机并添加项目目录 (cwd) 后即可多对话并行操作
              </div>
            </div>
          ) : (
            hostWorkspaces.map((w) => {
              const collapsed = collapsedWs.has(w.id);
              const sessions = s.acpSessions.filter((x) => x.workspace_id === w.id);
              return (
                <div key={w.id} className="acp-ws-card">
                  <div className="acp-ws-head" onClick={() => toggleWs(w.id)}>
                    {collapsed ? (
                      <ChevronRightIcon size={14} />
                    ) : (
                      <ChevronDownIcon size={14} />
                    )}
                    <FolderIcon size={15} />
                    <span
                      className="acp-ws-name"
                      title={w.cwd ? `${w.name} (${w.cwd})` : w.name}
                    >
                      {w.name}
                    </span>
                    <div
                      className="acp-ws-actions"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <button
                        type="button"
                        className="acp-ws-action-btn acp-ws-add-btn"
                        title="在此目录新建对话"
                        aria-label="在此目录新建对话"
                        onClick={() => setNewSessWsId(w.id)}
                      >
                        <PlusIcon size={15} />
                      </button>
                      <WorkspaceActionMenu
                        workspaceName={w.name}
                        onDelete={() => void removeWorkspace(w.id, w.name)}
                      />
                    </div>
                  </div>
                  {!collapsed && (
                    <div className="acp-ws-sessions">
                      {sessions.length === 0 ? (
                        <button
                          type="button"
                          className="acp-sess-item"
                          onClick={() => setNewSessWsId(w.id)}
                        >
                          <PlusIcon size={13} /> 新建对话…
                        </button>
                      ) : (
                        sessions.map((sess) => {
                          const hasPendingPerm = (sess.turns ?? []).some(
                            (t) => t.role === "permission" && t.status === "pending",
                          );
                          return (
                            <button
                              key={sess.id}
                              type="button"
                              className={`acp-sess-item${activeSession?.id === sess.id ? " on" : ""}`}
                              onClick={() =>
                                setState({
                                  activeAcpHostId: w.host_id,
                                  activeAcpWorkspaceId: w.id,
                                  activeAcpSessionId: sess.id,
                                })
                              }
                            >
                              <span
                                className={`dot${sess.running && !hasPendingPerm ? " acp-pulse-dot" : ""}`}
                                style={{
                                  flexShrink: 0,
                                  background: hasPendingPerm
                                    ? "#F59E0B"
                                    : sess.running
                                      ? "var(--accent)"
                                      : "var(--text-muted)",
                                }}
                              />
                              <span
                                style={{
                                  flex: 1,
                                  overflow: "hidden",
                                  textOverflow: "ellipsis",
                                  whiteSpace: "nowrap",
                                }}
                              >
                                {sess.title}
                              </span>
                            </button>
                          );
                        })
                      )}
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>
      </aside>

      <section className="acp-main">
        {!activeSession ? (
          <div className="empty-state" style={{ flex: 1 }}>
            <CpuIcon size={38} />
            <div className="empty-title" style={{ fontSize: 16 }}>
              ACP 多工作区 Agent 控制台
            </div>
            <div className="empty-hint-sm" style={{ maxWidth: 420 }}>
              先确保主机 (@Host) 在线并在「设置 → Agent 配置」或右上角注册本地 CLI Agent，然后选择工作目录即可并发运行对话与审批。
            </div>
            <div className="row" style={{ gap: 10, marginTop: 8 }}>
              <button type="button" className="btn-ghost" onClick={() => setShowAttach(true)}>
                + 接入机器 / Agent
              </button>
              {activeHost && (
                <button type="button" className="btn-accent" onClick={() => setShowAddWs(true)}>
                  + 添加工作目录
                </button>
              )}
              {activeWs && (
                <button
                  type="button"
                  className="btn-accent"
                  onClick={() => setNewSessWsId(activeWs.id)}
                >
                  + 在当前目录新建对话
                </button>
              )}
            </div>
          </div>
        ) : (
          <>
            <header className="acp-header">
              <div className="acp-header-row">
                {editingTitle ? (
                  <div className="row" style={{ gap: 8, flex: 1 }}>
                    <input
                      value={titleDraft}
                      onChange={(e) => setTitleDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && titleDraft.trim()) {
                          mutateSessionLocalFirst(activeSession.id, { title: titleDraft.trim() });
                          setEditingTitle(false);
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="btn-accent"
                      onClick={() => {
                        if (!titleDraft.trim()) return;
                        mutateSessionLocalFirst(activeSession.id, { title: titleDraft.trim() });
                        setEditingTitle(false);
                      }}
                    >
                      <CheckIcon size={14} /> 保存
                    </button>
                  </div>
                ) : (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 10,
                      minWidth: 0,
                      cursor: "pointer",
                    }}
                    onClick={() => {
                      setTitleDraft(activeSession.title);
                      setEditingTitle(true);
                    }}
                    title="点击重命名会话"
                  >
                    <h3
                      style={{
                        margin: 0,
                        fontSize: 16,
                        fontWeight: 700,
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {activeSession.title}
                    </h3>
                    {(() => {
                      const installing = !activeSession.running && activeAgent?.status === "installing";
                      const active = activeSession.running || installing;
                      const phase = activeActivity?.phase ?? "idle";
                      const phaseLabel =
                        phase !== "idle" ? ACTIVITY_PHASE_LABELS[phase] : "运行中…";
                      return (
                        <span
                          className={`status-pill${active ? " online" : ""}${activeSession.running ? " is-pulsing" : ""}`}
                        >
                          {activeSession.running
                            ? phaseLabel
                            : installing
                              ? "正在安装 ACP 适配器…"
                              : "就绪"}
                        </span>
                      );
                    })()}
                  </div>
                )}

                <div style={{ display: "flex", alignItems: "center", gap: 6, flexShrink: 0 }}>
                  <button
                    type="button"
                    className={`btn-ghost${rpcOpen ? " is-active" : ""}`}
                    title="查看 RPC 通信日志"
                    aria-expanded={rpcOpen}
                    onClick={() => setRpcOpen((v) => !v)}
                    style={{ fontSize: 12, padding: "4px 8px" }}
                  >
                    <ActivityIcon size={14} />
                    <span>RPC ({activeSession.rpc_logs.length})</span>
                  </button>
                  <button
                    type="button"
                    className="btn-ghost"
                    title="删除对话"
                    onClick={() => void removeSession(activeSession.id)}
                  >
                    <TrashIcon size={14} />
                  </button>
                </div>
              </div>

              <div className="acp-header-meta">
                {activeHostOfSess && <span className="acp-chip">@{activeHostOfSess.name}</span>}
                {activeAgent && (
                  <span className="acp-chip">
                    <CpuIcon size={12} /> {activeAgent.name}
                  </span>
                )}
                {activeWs && (
                  <span className="acp-chip mono-code" title={activeWs.cwd}>
                    <FolderIcon size={12} /> {activeWs.cwd}
                  </span>
                )}
                {activeWs?.branch && (
                  <span className="acp-chip mono-code">
                    <GitBranchIcon size={12} /> {activeWs.branch}
                  </span>
                )}
                {(() => {
                  const tok = formatUsageSummary(activeActivity?.usage);
                  return tok ? (
                    <span className="acp-chip mono-code" title="Token / 上下文用量">
                      {tok}
                    </span>
                  ) : null;
                })()}
              </div>

              {rpcOpen && (
                <div className="acp-rpc-drawer">
                  <div className="row between" style={{ marginBottom: 6 }}>
                    <span style={{ fontWeight: 600, fontSize: 12 }}>
                      RPC 通信日志（最近 {activeSession.rpc_logs.length} 条）
                    </span>
                    <button
                      type="button"
                      className="btn-ghost"
                      style={{ padding: "2px 6px", fontSize: 11 }}
                      onClick={() => setRpcOpen(false)}
                    >
                      收起
                    </button>
                  </div>
                  {activeSession.rpc_logs.length === 0 ? (
                    <div className="muted" style={{ fontSize: 12 }}>
                      暂无 RPC 日志
                    </div>
                  ) : (
                    <div className="acp-rpc-list mono-code">
                      {activeSession.rpc_logs.map((entry, idx) => (
                        <div key={idx} className="acp-rpc-item">
                          <span className={`acp-rpc-dir dir-${entry.dir}`}>
                            {entry.dir === "out" ? "→" : "←"}
                          </span>
                          <span className="acp-rpc-method">{entry.method}</span>
                          {entry.summary && (
                            <span className="acp-rpc-summary">{entry.summary}</span>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </header>

            <div className="acp-turns-stage">
              <div className="acp-turns" ref={streamRef} onScroll={onStreamScroll}>
                {displayEntries.length === 0 ? (
                  <div className="empty-state" style={{ margin: "auto" }}>
                    <div className="empty-title">对话已就绪</div>
                    <div className="empty-hint-sm">
                      支持 `@文件路径` 引用代码库文件、直接上传或粘贴图片，或在下方选择模型与思考深度。
                    </div>
                  </div>
                ) : (
                  displayEntries.map((entry) =>
                    entry.kind === "process_group" ? (
                      <ProcessGroupCard
                        key={entry.id}
                        turns={entry.turns}
                        onDownloadAttachment={downloadAttachment}
                        onPreviewAttachment={previewSentAttachment}
                        onPreviewFilePath={openHostPathPreview}
                        onReplyPermission={replyPermission}
                      />
                    ) : (
                      <AcpTurnItem
                        key={entry.turn.id}
                        turn={entry.turn}
                        onDownloadAttachment={downloadAttachment}
                        onPreviewAttachment={previewSentAttachment}
                        onPreviewFilePath={openHostPathPreview}
                        onReplyPermission={replyPermission}
                      />
                    ),
                  )
                )}
              </div>

              {!stickToBottom && (activeSession.turns ?? []).length > 0 && (
                <button
                  type="button"
                  className="acp-scroll-down-btn"
                  onClick={scrollToBottom}
                >
                  <span>回到底部</span>
                  <ArrowDownIcon size={13} />
                </button>
              )}
            </div>

            {activeSession.diffs.length > 0 && (
              <div className="acp-diff-bar">
                <div
                  className="row between"
                  style={{ cursor: "pointer" }}
                  onClick={() => setDiffsOpen((v) => !v)}
                >
                  <span>
                    代码变更：{activeSession.diffs.length} 个文件（
                    <span className="acp-diff-add-num">
                      +{activeSession.diffs.reduce((n, d) => n + d.added, 0)}
                    </span>
                    {" / "}
                    <span className="acp-diff-del-num">
                      -{activeSession.diffs.reduce((n, d) => n + d.removed, 0)}
                    </span>
                    ）
                  </span>
                  <span>{diffsOpen ? "收起 ▲" : "展开 ▼"}</span>
                </div>
                {diffsOpen && (
                  <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 6 }}>
                    {activeSession.diffs.map((d, i) => (
                      <div key={i} className="mono-code" style={{ fontSize: 11.5 }}>
                        <button
                          type="button"
                          className="acp-file-link-btn mono-code"
                          title={`点击预览：${d.file}`}
                          onClick={() =>
                            openHostPathPreview(
                              parseFileReference(d.file, { fromMarkdownLink: true }) ?? {
                                raw: d.file,
                                path: d.file,
                                display: d.file,
                              },
                            )
                          }
                        >
                          <b>{d.file}</b>
                        </button>{" "}
                        (
                        <span className="acp-diff-add-num">+{d.added}</span> /{" "}
                        <span className="acp-diff-del-num">-{d.removed}</span>) · {d.status}
                        {d.diff_lines && d.diff_lines.length > 0 && (
                          <pre className="acp-term-pre">{d.diff_lines.join("\n")}</pre>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {(activeSession.running ||
              (activeActivity && activeActivity.phase !== "idle")) && (
              <div className="acp-activity-bar" role="status" aria-live="polite">
                <div className="acp-activity-left">
                  <span
                    className={`acp-pulse-dot${activeActivity?.phase === "waiting_permission" ? " is-warn" : ""}`}
                  />
                  <span className="acp-activity-phase">
                    {ACTIVITY_PHASE_LABELS[activeActivity?.phase ?? "connecting"]}
                  </span>
                  <span className="acp-activity-detail">
                    {activeActivity?.detail || "等待 Agent 响应…"}
                  </span>
                </div>
                <div className="acp-activity-right mono-code">
                  {activeSession.running && (
                    <span className="acp-activity-timer">
                      {(elapsedMs / 1000).toFixed(1)}s
                    </span>
                  )}
                  {(() => {
                    const tok = formatUsageSummary(activeActivity?.usage);
                    return tok ? <span>{tok}</span> : null;
                  })()}
                </div>
              </div>
            )}

            <div className="acp-composer-wrap">
              {errMsg && (
                <div className="auth-err" style={{ marginBottom: 6 }}>
                  {errMsg}
                </div>
              )}
              <div className="acp-composer">
                {(pendingAtts.length > 0 || pendingPromptFileRefs.length > 0) && (
                  <div className="acp-composer-pills">
                    {pendingPromptFileRefs.map((ref) => (
                      <button
                        key={`ref-${ref.raw}`}
                        type="button"
                        className="acp-chip acp-chip-clickable mono-code"
                        data-testid="acp-pending-file-ref"
                        title={`点击预览待发送文件：${ref.path}`}
                        onClick={() => openHostPathPreview(ref, "待发送")}
                      >
                        @{ref.raw}
                      </button>
                    ))}
                    {pendingAtts.map((a) => (
                      <span
                        key={a.id}
                        className="acp-chip acp-chip-clickable"
                        data-testid="acp-pending-att"
                        onClick={() => openAcpAttachmentPreview(a, "待发送")}
                      >
                        <button
                          type="button"
                          className="acp-chip-main-btn"
                          title={`点击预览待发送附件：${a.file_name}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            openAcpAttachmentPreview(a, "待发送");
                          }}
                        >
                          📎 {a.file_name}
                        </button>
                        <button
                          type="button"
                          className="acp-chip-del-btn"
                          aria-label={`移除附件 ${a.file_name}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            setPendingAtts((cur) => cur.filter((x) => x.id !== a.id));
                            setPreviewTarget((cur) =>
                              cur?.kind === "acp-attachment" && cur.att.id === a.id
                                ? null
                                : cur,
                            );
                          }}
                        >
                          <XIcon size={12} />
                        </button>
                      </span>
                    ))}
                  </div>
                )}

                <input ref={fileRef} type="file" multiple hidden onChange={onPickFiles} />

                <textarea
                  ref={textareaRef}
                  value={promptText}
                  onChange={(e) => setPromptText(e.target.value)}
                  onPaste={onPasteComposer}
                  placeholder="输入任务指令，支持 @文件 引用、粘贴截图，Enter 发送，Shift+Enter 换行…"
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      void sendPrompt();
                    }
                  }}
                />

                <div className="acp-composer-bar">
                  <div className="acp-composer-pills">
                    <button
                      type="button"
                      className="btn-ghost"
                      style={{ height: 30, padding: "0 10px", borderRadius: 999 }}
                      disabled={uploading}
                      onClick={() => fileRef.current?.click()}
                      title="上传参考文件或截图"
                    >
                      <PlusIcon size={14} />
                      <span>{uploading ? "上传中…" : "附件"}</span>
                    </button>

                    <select
                      className="acp-select"
                      aria-label="选择模型"
                      value={activeSession.selected_model}
                      onChange={(e) =>
                        mutateSessionLocalFirst(activeSession.id, {
                          selected_model: e.target.value,
                        })
                      }
                    >
                      {(() => {
                        const base = activeAgent?.model_options.length
                          ? activeAgent.model_options
                          : [activeSession.selected_model];
                        const opts =
                          activeSession.selected_model &&
                          !base.includes(activeSession.selected_model)
                            ? [activeSession.selected_model, ...base]
                            : base;
                        return opts.map((m) => (
                          <option key={m} value={m}>
                            模型: {m}
                          </option>
                        ));
                      })()}
                    </select>

                    <select
                      className="acp-select"
                      aria-label="思考深度"
                      value={activeSession.thinking_level}
                      onChange={(e) =>
                        mutateSessionLocalFirst(activeSession.id, {
                          thinking_level: e.target.value as AcpThinkingLevel,
                        })
                      }
                    >
                      {THINKING_LEVELS.map((lv) => (
                        <option key={lv} value={lv}>
                          思考: {lv}
                        </option>
                      ))}
                    </select>

                    <select
                      className="acp-select"
                      aria-label="权限策略"
                      value={activeSession.permission_policy}
                      onChange={(e) =>
                        mutateSessionLocalFirst(activeSession.id, {
                          permission_policy: e.target.value as AcpPermissionPolicy,
                        })
                      }
                    >
                      {PERMISSION_POLICIES.map((p) => (
                        <option key={p.v} value={p.v}>
                          {p.label}
                        </option>
                      ))}
                    </select>

                    {activeWs && activeWs.branches.length > 0 && (
                      <select
                        className="acp-select mono-code"
                        aria-label="Git 分支"
                        value={activeWs.branch}
                        onChange={(e) =>
                          void gitCheckoutLocalFirst(activeWs, e.target.value).catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "切换分支失败"),
                          )
                        }
                      >
                        {activeWs.branches.map((b) => (
                          <option key={b} value={b}>
                            ⎇ {b}
                          </option>
                        ))}
                      </select>
                    )}
                  </div>

                  {activeSession.running ? (
                    <button
                      type="button"
                      className="btn-danger-soft"
                      onClick={() =>
                        void cancelSessionLocalFirst(activeSession)
                          .then(patchSessionState)
                          .catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "停止运行失败"),
                          )
                      }
                    >
                      停止运行
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="btn-accent"
                      disabled={sending || (!promptText.trim() && pendingAtts.length === 0)}
                      onClick={() => void sendPrompt()}
                    >
                      {sending ? "发送中…" : "发送指令"}
                    </button>
                  )}
                </div>
              </div>
            </div>
          </>
        )}
      </section>

      {previewTarget && (
        <FilePreview
          variant="agent"
          target={previewTarget}
          openSeq={previewSeq}
          onClose={() => {
            setPreviewTarget(null);
            setPreviewCollapsed(false);
          }}
          onMobileCollapsedChange={setPreviewCollapsed}
        />
      )}

      {showAddWs && (
        <AddWorkspaceModal
          defaultHostId={activeHost?.id}
          onClose={() => setShowAddWs(false)}
        />
      )}
      {newSessionTargetWs && (
        <NewSessionModal
          workspace={newSessionTargetWs}
          onClose={() => setNewSessWsId(null)}
        />
      )}
      {showAttach && (
        <AttachAgentModal
          defaultHostId={activeHost?.id}
          onClose={() => setShowAttach(false)}
        />
      )}
    </main>
  );
}

function WorkspaceActionMenu({
  workspaceName,
  onDelete,
}: {
  workspaceName: string;
  onDelete: () => void;
}) {
  const supportsNativePopover =
    typeof HTMLElement !== "undefined" &&
    typeof (HTMLElement.prototype as { showPopover?: unknown }).showPopover ===
      "function";
  const supportsAnchorPos =
    typeof CSS !== "undefined" &&
    typeof CSS.supports === "function" &&
    CSS.supports("position-area: block-end");

  const rawId = useId();
  const safeId = rawId.replace(/[^a-zA-Z0-9_-]/g, "");
  const menuId = `ws-menu-${safeId}`;
  const anchorName = `--ws-menu-${safeId}`;

  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);

  const positionMenu = useCallback(() => {
    const trigger = triggerRef.current;
    const popover = popoverRef.current;
    if (!trigger || !popover) return;
    const triggerRect = trigger.getBoundingClientRect();
    const menuRect = popover.getBoundingClientRect();
    const menuWidth = menuRect.width || 140;
    const menuHeight = menuRect.height || 40;
    const maxLeft = Math.max(8, window.innerWidth - menuWidth - 8);
    const left = Math.min(Math.max(8, triggerRect.right - menuWidth), maxLeft);
    let top = triggerRect.bottom + 4;
    if (top + menuHeight > window.innerHeight - 8) {
      top = Math.max(8, triggerRect.top - menuHeight - 4);
    }
    popover.style.left = `${left}px`;
    popover.style.top = `${top}px`;
  }, []);

  useEffect(() => {
    if (!supportsNativePopover) return;
    const popover = popoverRef.current;
    if (!popover) return;
    const onToggle = (e: Event) => {
      const nextState = (e as Event & { newState?: string }).newState;
      const isOpen = nextState
        ? nextState === "open"
        : popover.matches(":popover-open");
      setOpen(isOpen);
      if (isOpen && !supportsAnchorPos) {
        positionMenu();
        requestAnimationFrame(positionMenu);
      }
    };
    popover.addEventListener("toggle", onToggle);
    return () => popover.removeEventListener("toggle", onToggle);
  }, [supportsNativePopover, supportsAnchorPos, positionMenu]);

  useEffect(() => {
    if (!open || !supportsNativePopover || supportsAnchorPos) return;
    const sync = () => positionMenu();
    window.addEventListener("resize", sync);
    window.addEventListener("scroll", sync, { capture: true, passive: true });
    return () => {
      window.removeEventListener("resize", sync);
      window.removeEventListener("scroll", sync, true);
    };
  }, [open, supportsNativePopover, supportsAnchorPos, positionMenu]);

  useLayoutEffect(() => {
    if (!supportsNativePopover && open) {
      positionMenu();
    }
  }, [supportsNativePopover, open, positionMenu]);

  useEffect(() => {
    if (supportsNativePopover || !open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    const onSync = () => positionMenu();
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", onSync);
    window.addEventListener("scroll", onSync, { capture: true, passive: true });
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("resize", onSync);
      window.removeEventListener("scroll", onSync, true);
    };
  }, [supportsNativePopover, open, positionMenu]);

  const handleDelete = () => {
    if (supportsNativePopover) {
      (
        popoverRef.current as (HTMLDivElement & { hidePopover?: () => void }) | null
      )?.hidePopover?.();
    } else {
      setOpen(false);
    }
    onDelete();
  };

  return (
    <div className="acp-ws-menu-wrap" ref={wrapRef}>
      <button
        ref={triggerRef}
        type="button"
        className="acp-ws-action-btn"
        title="更多操作"
        aria-label={`${workspaceName} 更多操作`}
        aria-expanded={open}
        aria-controls={menuId}
        style={
          supportsNativePopover
            ? ({ anchorName } as CSSProperties)
            : undefined
        }
        {...(supportsNativePopover
          ? ({ popovertarget: menuId } as Record<string, string>)
          : { onClick: () => setOpen((v) => !v) })}
      >
        <MoreVerticalIcon size={15} />
      </button>
      {(supportsNativePopover || open) && (
        <div
          ref={popoverRef}
          id={menuId}
          className="acp-ws-popover"
          style={
            supportsNativePopover
              ? ({ positionAnchor: anchorName } as CSSProperties)
              : undefined
          }
          {...(supportsNativePopover
            ? ({ popover: "auto" } as Record<string, string>)
            : undefined)}
        >
          <button
            type="button"
            className="acp-ws-popover-item danger"
            onClick={handleDelete}
          >
            <TrashIcon size={14} />
            <span>删除工作目录</span>
          </button>
        </div>
      )}
    </div>
  );
}
