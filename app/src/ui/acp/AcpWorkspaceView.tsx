import { useEffect, useMemo, useRef, useState } from "react";
import { acp } from "../../api";
import { applyAcpBootstrap, getState, setState, useApp } from "../../store";
import type {
  AcpAttachment,
  AcpPermissionPolicy,
  AcpPlanStep,
  AcpThinkingLevel,
  AcpTurn,
} from "../../types";
import { confirmDialog } from "../Modal";
import {
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CpuIcon,
  FolderIcon,
  GitBranchIcon,
  PlusIcon,
  TerminalIcon,
  TrashIcon,
  XIcon,
} from "../icons";
import {
  AddWorkspaceModal,
  AttachAgentModal,
  NewSessionModal,
} from "./AcpModals";

const THINKING_LEVELS: AcpThinkingLevel[] = ["关", "低", "中", "高"];
const PERMISSION_POLICIES: { v: AcpPermissionPolicy; label: string }[] = [
  { v: "ask", label: "权限：每次询问" },
  { v: "auto_read", label: "权限：自动只读" },
  { v: "allow_all", label: "权限：全部允许" },
];

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
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");

  const fileRef = useRef<HTMLInputElement>(null);
  const streamRef = useRef<HTMLDivElement>(null);

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

  useEffect(() => {
    setPendingAtts([]);
    setErrMsg("");
    setEditingTitle(false);
    setDiffsOpen(false);
  }, [activeSession?.id]);

  useEffect(() => {
    const el = streamRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [activeSession?.id, activeSession?.turns?.length]);

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
      await acp.deleteWorkspace(wsId);
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
      await acp.deleteSession(sessId);
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
    setState((cur) => ({
      acpSessions: cur.acpSessions.map((x) =>
        x.id === updated.id ? { ...updated, turns: updated.turns ?? x.turns } : x,
      ),
    }));
  };

  const onPickFiles = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const picked = Array.from(e.target.files ?? []);
    e.target.value = "";
    if (!picked.length || !activeSession) return;
    const targetSessId = activeSession.id;
    setUploading(true);
    setErrMsg("");
    try {
      const uploaded = await Promise.all(
        picked.map((f) => acp.uploadFile(f, targetSessId)),
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

  const sendPrompt = async () => {
    if (!activeSession || activeSession.running || sending) return;
    const targetSessId = activeSession.id;
    const raw = promptText.trim();
    if (!raw && pendingAtts.length === 0) return;
    const fileRefs = Array.from(raw.matchAll(/@([^\s@]+)/g)).map((m) => m[1]);
    setSending(true);
    setErrMsg("");
    try {
      const turn = await acp.sendPrompt(targetSessId, {
        text: raw,
        files: fileRefs,
        attachment_ids: pendingAtts.map((a) => a.id),
        model: activeSession.selected_model,
        thinking_level: activeSession.thinking_level,
        permission_policy: activeSession.permission_policy,
      });
      if (getState().activeAcpSessionId === targetSessId) {
        setPromptText("");
        setPendingAtts([]);
      }
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
    } catch (ex) {
      setErrMsg(ex instanceof Error ? ex.message : "发送指令失败");
    } finally {
      setSending(false);
    }
  };

  const replyPermission = async (
    turnId: string,
    opt: "allow_once" | "allow_always" | "reject_once" | "reject_always",
  ) => {
    try {
      const updatedTurn = await acp.replyPermission(turnId, opt);
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
  };

  const renderTurn = (t: AcpTurn) => {
    if (t.role === "user") {
      const text = String(t.payload.text ?? "");
      const files = Array.isArray(t.payload.files)
        ? (t.payload.files as string[])
        : [];
      const atts = Array.isArray(t.payload.attachments)
        ? (t.payload.attachments as AcpAttachment[])
        : [];
      return (
        <div key={t.id} className="acp-turn-user">
          {text && <div>{text}</div>}
          {(files.length > 0 || atts.length > 0) && (
            <div className="acp-composer-pills" style={{ marginTop: 8 }}>
              {files.map((f, i) => (
                <span
                  key={`${f}-${i}`}
                  className="acp-chip mono-code"
                  style={{ background: "rgba(255,255,255,0.16)", color: "#fff", borderColor: "transparent" }}
                >
                  @{f}
                </span>
              ))}
              {atts.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  onClick={() =>
                    void acp.downloadFile(a.id, a.file_name).catch((ex) =>
                      setErrMsg(ex instanceof Error ? ex.message : "下载附件失败"),
                    )
                  }
                  className="acp-chip"
                  style={{
                    background: "rgba(255,255,255,0.16)",
                    color: "#fff",
                    borderColor: "transparent",
                    cursor: "pointer",
                  }}
                >
                  📎 {a.file_name}
                </button>
              ))}
            </div>
          )}
        </div>
      );
    }

    if (t.role === "agent_plan") {
      const steps = Array.isArray(t.payload.steps)
        ? (t.payload.steps as AcpPlanStep[])
        : [];
      return (
        <div key={t.id} className="acp-turn-card">
          <div style={{ fontWeight: 600, fontSize: 13, marginBottom: 8 }}>
            执行计划 ({steps.filter((st) => st.state === "done").length}/{steps.length})
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            {steps.map((st, idx) => (
              <div
                key={idx}
                style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13 }}
              >
                <span
                  style={{
                    width: 18,
                    textAlign: "center",
                    color:
                      st.state === "done"
                        ? "var(--ok)"
                        : st.state === "active"
                          ? "var(--accent)"
                          : "var(--text-muted)",
                  }}
                >
                  {st.state === "done" ? "✓" : st.state === "active" ? "●" : "○"}
                </span>
                <span
                  style={{
                    color: st.state === "pending" ? "var(--text-muted)" : "var(--text-main)",
                  }}
                >
                  {st.label}
                </span>
              </div>
            ))}
          </div>
        </div>
      );
    }

    if (t.role === "permission") {
      const pending = t.status === "pending";
      const chosen = String(t.payload.chosen_option ?? "");
      return (
        <div key={t.id} className="acp-turn-card acp-perm-card">
          <div className="row between" style={{ marginBottom: 6 }}>
            <b style={{ fontSize: 13 }}>
              {pending ? "⚠️ 需要授权执行操作" : "已处理的授权请求"}
            </b>
            <span className="acp-chip">{String(t.payload.tool_kind ?? "tool")}</span>
          </div>
          <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>
            {String(t.payload.reason ?? "")}
          </div>
          {Boolean(t.payload.command) && (
            <pre className="acp-term-pre">{String(t.payload.command)}</pre>
          )}
          {pending ? (
            <div className="acp-perm-actions">
              <button
                type="button"
                className="btn-accent"
                onClick={() => void replyPermission(t.id, "allow_once")}
              >
                允许一次
              </button>
              <button
                type="button"
                className="btn-ghost"
                onClick={() => void replyPermission(t.id, "allow_always")}
              >
                始终允许
              </button>
              <button
                type="button"
                className="btn-danger-soft"
                onClick={() => void replyPermission(t.id, "reject_once")}
              >
                拒绝一次
              </button>
              <button
                type="button"
                className="btn-danger-soft"
                onClick={() => void replyPermission(t.id, "reject_always")}
              >
                始终拒绝
              </button>
            </div>
          ) : (
            <div className="muted" style={{ fontSize: 12, marginTop: 8 }}>
              结果：{chosen || t.status}
            </div>
          )}
        </div>
      );
    }

    if (t.role === "terminal_out") {
      const exit = Number(t.payload.exit_code ?? 0);
      return (
        <div key={t.id} className="acp-turn-card">
          <div className="row between" style={{ fontSize: 12.5 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 6, fontWeight: 600 }}>
              <TerminalIcon size={14} />
              <span className="mono-code">{String(t.payload.command ?? "终端输出")}</span>
            </div>
            <span className="acp-chip">exit {exit}</span>
          </div>
          <pre className="acp-term-pre">{String(t.payload.output ?? "")}</pre>
        </div>
      );
    }

    return (
      <div key={t.id} className="acp-turn-agent">
        {String(t.payload.text ?? "")}
        {t.status === "streaming" && <span className="muted"> ▌</span>}
      </div>
    );
  };

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
                    {activeHost.latency_ms}ms
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
                        {h.online ? `${h.latency_ms}ms` : "离线"}
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
                    <span className="acp-ws-name" title={w.name}>
                      {w.name}
                    </span>
                    <button
                      type="button"
                      className="icon-btn"
                      title="在此目录新建对话"
                      onClick={(e) => {
                        e.stopPropagation();
                        setNewSessWsId(w.id);
                      }}
                    >
                      <PlusIcon size={14} />
                    </button>
                    <button
                      type="button"
                      className="icon-btn"
                      title="删除工作目录"
                      onClick={(e) => {
                        e.stopPropagation();
                        void removeWorkspace(w.id, w.name);
                      }}
                    >
                      <TrashIcon size={13} />
                    </button>
                  </div>
                  <div className="acp-ws-cwd mono-code" title={w.cwd}>
                    {w.cwd}
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
                                className="dot"
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
                          void acp
                            .updateSession(activeSession.id, { title: titleDraft.trim() })
                            .then((u) => {
                              patchSessionState(u);
                              setEditingTitle(false);
                            })
                            .catch((ex) =>
                              setErrMsg(ex instanceof Error ? ex.message : "更新会话标题失败"),
                            );
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="btn-accent"
                      onClick={() => {
                        if (!titleDraft.trim()) return;
                        void acp
                          .updateSession(activeSession.id, { title: titleDraft.trim() })
                          .then((u) => {
                            patchSessionState(u);
                            setEditingTitle(false);
                          })
                          .catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "更新会话标题失败"),
                          );
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
                    <span className={`status-pill${activeSession.running ? " online" : ""}`}>
                      {activeSession.running ? "运行中…" : "就绪"}
                    </span>
                  </div>
                )}

                <button
                  type="button"
                  className="btn-ghost"
                  title="删除对话"
                  onClick={() => void removeSession(activeSession.id)}
                >
                  <TrashIcon size={14} />
                </button>
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
              </div>
            </header>

            <div className="acp-turns" ref={streamRef}>
              {(activeSession.turns ?? []).length === 0 ? (
                <div className="empty-state" style={{ margin: "auto" }}>
                  <div className="empty-title">对话已就绪</div>
                  <div className="empty-hint-sm">
                    支持 `@文件路径` 引用代码库文件、直接上传附件，或在下方选择模型与思考深度。
                  </div>
                </div>
              ) : (
                (activeSession.turns ?? []).map(renderTurn)
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
                    +{activeSession.diffs.reduce((n, d) => n + d.added, 0)} / -
                    {activeSession.diffs.reduce((n, d) => n + d.removed, 0)}）
                  </span>
                  <span>{diffsOpen ? "收起 ▲" : "展开 ▼"}</span>
                </div>
                {diffsOpen && (
                  <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 6 }}>
                    {activeSession.diffs.map((d, i) => (
                      <div key={i} className="mono-code" style={{ fontSize: 11.5 }}>
                        <b>{d.file}</b> (+{d.added} / -{d.removed}) · {d.status}
                        {d.diff_lines && d.diff_lines.length > 0 && (
                          <pre className="acp-term-pre">{d.diff_lines.join("\n")}</pre>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            <div className="acp-composer-wrap">
              {errMsg && (
                <div className="auth-err" style={{ marginBottom: 6 }}>
                  {errMsg}
                </div>
              )}
              <div className="acp-composer">
                {pendingAtts.length > 0 && (
                  <div className="acp-composer-pills">
                    {pendingAtts.map((a) => (
                      <span key={a.id} className="acp-chip">
                        <span>📎 {a.file_name}</span>
                        <button
                          type="button"
                          style={{ border: "none", background: "none", cursor: "pointer", padding: 0 }}
                          onClick={() =>
                            setPendingAtts((cur) => cur.filter((x) => x.id !== a.id))
                          }
                        >
                          <XIcon size={12} />
                        </button>
                      </span>
                    ))}
                  </div>
                )}

                <input ref={fileRef} type="file" multiple hidden onChange={onPickFiles} />

                <textarea
                  value={promptText}
                  onChange={(e) => setPromptText(e.target.value)}
                  placeholder="输入任务指令，支持 @文件 引用，Enter 发送，Shift+Enter 换行…"
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
                        void acp
                          .updateSession(activeSession.id, { selected_model: e.target.value })
                          .then(patchSessionState)
                          .catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "切换模型失败"),
                          )
                      }
                    >
                      {(activeAgent?.model_options.length
                        ? activeAgent.model_options
                        : [activeSession.selected_model]
                      ).map((m) => (
                        <option key={m} value={m}>
                          模型: {m}
                        </option>
                      ))}
                    </select>

                    <select
                      className="acp-select"
                      aria-label="思考深度"
                      value={activeSession.thinking_level}
                      onChange={(e) =>
                        void acp
                          .updateSession(activeSession.id, {
                            thinking_level: e.target.value as AcpThinkingLevel,
                          })
                          .then(patchSessionState)
                          .catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "更新思考深度失败"),
                          )
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
                        void acp
                          .updateSession(activeSession.id, {
                            permission_policy: e.target.value as AcpPermissionPolicy,
                          })
                          .then(patchSessionState)
                          .catch((ex) =>
                            setErrMsg(ex instanceof Error ? ex.message : "更新权限策略失败"),
                          )
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
                          void acp
                            .gitCheckout(activeWs.id, e.target.value)
                            .then((ws) =>
                              setState((cur) => ({
                                acpWorkspaces: cur.acpWorkspaces.map((x) =>
                                  x.id === ws.id ? ws : x,
                                ),
                              })),
                            )
                            .catch((ex) =>
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
                        void acp
                          .cancelSession(activeSession.id)
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
