import { memo, useEffect, useRef, useState } from "react";
import type {
  AcpAttachment,
  AcpPlanStep,
  AcpToolCallPayload,
  AcpToolDiff,
  AcpTurn,
} from "../../types";
import {
  ActivityIcon,
  AlertCircleIcon,
  CheckIcon,
  ChevronRightIcon,
  CopyIcon,
  FileTextIcon,
  SparklesIcon,
  TerminalIcon,
  WrenchIcon,
} from "../icons";
import { parseFileReference, type ParsedFileRef } from "../filePathRef";
import { AcpMarkdown } from "./AcpMarkdown";

export type AcpDisplayEntry =
  | { kind: "turn"; turn: AcpTurn }
  | { kind: "process_group"; id: string; turns: AcpTurn[] };

function isStandaloneTurn(t: AcpTurn): boolean {
  return (
    t.role === "user" ||
    t.role === "agent_text" ||
    (t.role === "permission" && t.status === "pending")
  );
}

/**
 * 将两次正式回复 / 用户输入 / 待确认授权之间的思考、工具调用、计划与终端输出
 * 合并为一个二级折叠过程块；组 id 取首条 turn.id 以在流式追加时保持折叠状态稳定。
 */
export function groupTurnsForDisplay(turns: AcpTurn[]): AcpDisplayEntry[] {
  const out: AcpDisplayEntry[] = [];
  let buf: AcpTurn[] = [];

  const flush = () => {
    if (buf.length === 0) return;
    out.push({
      kind: "process_group",
      id: `group-${buf[0].id}`,
      turns: buf,
    });
    buf = [];
  };

  for (const t of turns) {
    if (isStandaloneTurn(t)) {
      flush();
      out.push({ kind: "turn", turn: t });
    } else {
      buf.push(t);
    }
  }
  flush();
  return out;
}

const TOOL_KIND_LABELS: Record<string, string> = {
  read: "读取",
  edit: "编辑",
  delete: "删除",
  move: "移动",
  search: "搜索",
  execute: "执行",
  think: "思考",
  fetch: "网络",
  switch_mode: "切换模式",
  other: "工具",
};

const PERM_TOOL_LABELS: Record<string, string> = {
  "fs/read_text_file": "读取文件",
  "fs/write_text_file": "写入文件",
  "terminal/run": "终端命令",
  extension_confirm: "扩展确认",
  extension_select: "扩展选择",
  bash: "Shell 命令",
  powershell: "PowerShell 命令",
  read: "读取文件",
  edit: "编辑文件",
  write: "写入文件",
  grep: "搜索内容",
  find: "查找文件",
  ls: "列出目录",
};

const PERM_RESULT_LABELS: Record<string, string> = {
  allow_once: "已允许（仅本次）",
  allow_always: "已始终允许",
  approved: "已允许",
  reject_once: "已拒绝（仅本次）",
  reject_always: "已始终拒绝",
  rejected: "已拒绝",
  cancelled: "已取消",
  session_cancelled: "会话已取消",
  host_disconnected: "宿主已断开",
};

function formatDurationMs(ms: unknown): string | null {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return null;
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function firstNonEmptyLine(text: string): string {
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (trimmed) return trimmed;
  }
  return "";
}

function copyClipboard(text: string, onCopied: () => void) {
  try {
    void navigator.clipboard?.writeText(text);
  } catch {}
  onCopied();
}

function toFileRef(raw: string): ParsedFileRef {
  return (
    parseFileReference(raw, { fromMarkdownLink: true }) ?? {
      raw,
      path: raw,
      display: raw,
    }
  );
}

function UserTurnCard({
  turn,
  onDownloadAttachment,
  onPreviewAttachment,
  onPreviewFilePath,
}: {
  turn: AcpTurn;
  onDownloadAttachment: (id: string, fileName: string) => void;
  onPreviewAttachment?: (att: AcpAttachment) => void;
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
}) {
  const text = String(turn.payload.text ?? "");
  const files = Array.isArray(turn.payload.files)
    ? (turn.payload.files as string[])
    : [];
  const atts = Array.isArray(turn.payload.attachments)
    ? (turn.payload.attachments as AcpAttachment[])
    : [];
  return (
    <div className="acp-turn-user">
      {text && <div>{text}</div>}
      {(files.length > 0 || atts.length > 0) && (
        <div className="acp-composer-pills" style={{ marginTop: 8 }}>
          {files.map((f, i) => (
            <button
              key={`${f}-${i}`}
              type="button"
              onClick={() => onPreviewFilePath?.(toFileRef(f))}
              title={onPreviewFilePath ? `预览文件：${f}` : f}
              className="acp-chip mono-code"
              style={{
                background: "rgba(255,255,255,0.16)",
                color: "#fff",
                borderColor: "transparent",
                cursor: onPreviewFilePath ? "pointer" : "default",
              }}
            >
              @{f}
            </button>
          ))}
          {atts.map((a) => (
            <button
              key={a.id}
              type="button"
              title={`预览文件：${a.file_name}`}
              onClick={() =>
                onPreviewAttachment
                  ? onPreviewAttachment(a)
                  : onDownloadAttachment(a.id, a.file_name)
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

function ThoughtTurnCard({ turn }: { turn: AcpTurn }) {
  const streaming = turn.status === "streaming";
  const text = String(turn.payload.text ?? "");
  const preview = firstNonEmptyLine(text);
  const durationLabel = formatDurationMs(turn.payload.duration_ms);
  const [isOpen, setIsOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const preRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (streaming && isOpen && preRef.current) {
      preRef.current.scrollTop = preRef.current.scrollHeight;
    }
  }, [streaming, isOpen, text]);

  return (
    <details
      className={`acp-accordion acp-thought-card${streaming ? " is-streaming" : ""}`}
      onToggle={(e) => setIsOpen(e.currentTarget.open)}
    >
      <summary className="acp-acc-summary">
        <ChevronRightIcon size={14} className="acp-acc-chevron" />
        <SparklesIcon size={14} className="acp-thought-icon" />
        <span className="acp-acc-label">
          {streaming ? "正在思考…" : "思考过程"}
        </span>
        {preview && (
          <span className="acp-acc-preview" title={preview}>
            {preview}
          </span>
        )}
        <span className="acp-tool-summary-right">
          {streaming ? (
            <span className="acp-mini-badge is-live">推理中</span>
          ) : durationLabel ? (
            <span className="acp-mini-badge">{durationLabel}</span>
          ) : null}
        </span>
      </summary>
      <div className="acp-acc-body">
        {text ? (
          <div className="acp-tool-sec-head" style={{ paddingTop: 8 }}>
            <span className="acp-tool-sec-label">推理内容</span>
            <button
              type="button"
              className="acp-inline-icon-btn"
              aria-label="复制思考过程"
              title="复制思考过程"
              onClick={() =>
                copyClipboard(text, () => {
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1500);
                })
              }
            >
              {copied ? <CheckIcon size={13} /> : <CopyIcon />}
              <span>{copied ? "已复制" : "复制"}</span>
            </button>
          </div>
        ) : null}
        <pre ref={preRef} className="acp-thought-pre">
          {text || "…"}
        </pre>
      </div>
    </details>
  );
}

function ToolDiffPreview({
  diffs,
  onPreviewFilePath,
}: {
  diffs: AcpToolDiff[];
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
}) {
  return (
    <div className="acp-tool-diffs">
      {diffs.map((d, idx) => {
        const oldLines = d.old_text ? d.old_text.split(/\r?\n/) : [];
        const newLines = d.new_text ? d.new_text.split(/\r?\n/) : [];
        const previewCap = 120;
        const truncated = oldLines.length + newLines.length > previewCap;
        return (
          <div key={`${d.path}-${idx}`} className="acp-tool-diff-file">
            <div className="acp-tool-diff-path mono-code">
              <FileTextIcon size={12} />
              {onPreviewFilePath ? (
                <button
                  type="button"
                  className="acp-file-link-btn mono-code"
                  title={`预览文件：${d.path}`}
                  onClick={() => onPreviewFilePath(toFileRef(d.path))}
                >
                  {d.path}
                </button>
              ) : (
                <span>{d.path}</span>
              )}
              <span className="acp-diff-stat">
                {oldLines.length > 0 && (
                  <span className="acp-diff-del-num">-{oldLines.length}</span>
                )}
                {newLines.length > 0 && (
                  <span className="acp-diff-add-num">+{newLines.length}</span>
                )}
              </span>
            </div>
            <pre className="acp-tool-diff-pre mono-code">
              {oldLines.slice(0, 60).map((line, i) => (
                <div key={`o-${i}`} className="acp-diff-line is-del">
                  - {line}
                </div>
              ))}
              {newLines.slice(0, 60).map((line, i) => (
                <div key={`n-${i}`} className="acp-diff-line is-add">
                  + {line}
                </div>
              ))}
              {truncated && (
                <div className="acp-diff-line is-meta">… 变更行数较多，已截断预览 …</div>
              )}
            </pre>
          </div>
        );
      })}
    </div>
  );
}

function ToolCallCard({
  turn,
  onPreviewFilePath,
}: {
  turn: AcpTurn;
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
}) {
  const payload = turn.payload as unknown as AcpToolCallPayload;
  const kind = String(payload.kind ?? "other");
  const title = String(payload.title || payload.name || "工具调用");
  const input = typeof payload.input === "string" ? payload.input : "";
  const output = typeof payload.output === "string" ? payload.output : "";
  const locations = Array.isArray(payload.locations) ? payload.locations : [];
  const diffs = Array.isArray(payload.diffs) ? payload.diffs : [];
  const durationLabel = formatDurationMs(payload.duration_ms);

  const isRunning = turn.status === "pending" || turn.status === "in_progress";
  const isFailed = turn.status === "failed" || Boolean(payload.is_error);

  const [isOpen, setIsOpen] = useState(false);
  const [copiedOut, setCopiedOut] = useState(false);
  const outPreRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (isRunning && isOpen && outPreRef.current) {
      outPreRef.current.scrollTop = outPreRef.current.scrollHeight;
    }
  }, [isRunning, isOpen, output]);

  const kindLabel = TOOL_KIND_LABELS[kind] ?? TOOL_KIND_LABELS.other;
  const hasBody =
    locations.length > 0 || Boolean(input) || diffs.length > 0 || Boolean(output);

  return (
    <details
      className={`acp-accordion acp-tool-card${isRunning ? " is-running" : ""}${isFailed ? " is-failed" : ""}`}
      onToggle={(e) => setIsOpen(e.currentTarget.open)}
    >
      <summary className="acp-acc-summary">
        <ChevronRightIcon size={14} className="acp-acc-chevron" />
        <span className="acp-tool-state-icon" aria-hidden="true">
          {isRunning ? (
            <span className="acp-tool-spinner" />
          ) : isFailed ? (
            <AlertCircleIcon size={14} className="acp-status-err" />
          ) : (
            <CheckIcon size={14} className="acp-status-ok" />
          )}
        </span>
        <span className={`acp-tool-kind-badge kind-${kind}`}>
          <WrenchIcon size={11} />
          {kindLabel}
        </span>
        <span className="acp-acc-label mono-code" title={title}>
          {title}
        </span>
        <span className="acp-tool-summary-right">
          {isRunning ? (
            <span className="acp-mini-badge is-live">执行中</span>
          ) : isFailed ? (
            <span className="acp-mini-badge is-err">失败</span>
          ) : durationLabel ? (
            <span className="acp-mini-badge">{durationLabel}</span>
          ) : null}
        </span>
      </summary>

      {hasBody && (
        <div className="acp-acc-body">
          {locations.length > 0 && (
            <div className="acp-tool-locs">
              {locations.map((loc, i) =>
                onPreviewFilePath ? (
                  <button
                    key={`${loc}-${i}`}
                    type="button"
                    className="acp-chip acp-chip-clickable mono-code"
                    title={`预览文件：${loc}`}
                    onClick={() => onPreviewFilePath(toFileRef(loc))}
                  >
                    <FileTextIcon size={11} />
                    {loc}
                  </button>
                ) : (
                  <span key={`${loc}-${i}`} className="acp-chip mono-code" title={loc}>
                    <FileTextIcon size={11} />
                    {loc}
                  </span>
                ),
              )}
            </div>
          )}

          {input && (
            <div className="acp-tool-sec">
              <div className="acp-tool-sec-label">参数 / 命令</div>
              <pre className="acp-tool-input-pre mono-code">{input}</pre>
            </div>
          )}

          {diffs.length > 0 && (
            <div className="acp-tool-sec">
              <div className="acp-tool-sec-label">文件变更预览</div>
              <ToolDiffPreview diffs={diffs} onPreviewFilePath={onPreviewFilePath} />
            </div>
          )}

          {output && (
            <div className="acp-tool-sec">
              <div className="acp-tool-sec-head">
                <span className="acp-tool-sec-label">输出</span>
                <button
                  type="button"
                  className="acp-inline-icon-btn"
                  aria-label="复制工具输出"
                  onClick={() =>
                    copyClipboard(output, () => {
                      setCopiedOut(true);
                      window.setTimeout(() => setCopiedOut(false), 1500);
                    })
                  }
                >
                  {copiedOut ? <CheckIcon size={12} /> : <CopyIcon />}
                  <span>{copiedOut ? "已复制" : "复制"}</span>
                </button>
              </div>
              <pre ref={outPreRef} className="acp-term-pre">
                {output}
              </pre>
            </div>
          )}
        </div>
      )}
    </details>
  );
}

function AgentTextTurn({
  turn,
  onPreviewFilePath,
}: {
  turn: AcpTurn;
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
}) {
  const text = String(turn.payload.text ?? "");
  const isError = turn.status === "error";
  const isStreaming = turn.status === "streaming";
  const [copied, setCopied] = useState(false);

  if (isError) {
    return (
      <div className="acp-turn-agent is-error">
        <div className="acp-err-line">
          <AlertCircleIcon size={15} />
          <span>{text}</span>
        </div>
      </div>
    );
  }

  return (
    <div className="acp-turn-agent">
      <AcpMarkdown
        text={text}
        streaming={isStreaming}
        onOpenFilePath={onPreviewFilePath}
      />
      {text && !isStreaming ? (
        <div className="acp-agent-actions">
          <button
            type="button"
            className="acp-inline-icon-btn"
            aria-label="复制回复"
            onClick={() =>
              copyClipboard(text, () => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              })
            }
          >
            {copied ? <CheckIcon size={12} /> : <CopyIcon />}
            <span>{copied ? "已复制" : "复制"}</span>
          </button>
        </div>
      ) : null}
    </div>
  );
}

function PermissionTurnCard({
  turn,
  onReply,
}: {
  turn: AcpTurn;
  onReply: (
    turnId: string,
    opt: "allow_once" | "allow_always" | "reject_once" | "reject_always",
  ) => void;
}) {
  const pending = turn.status === "pending";
  const rawKind = String(turn.payload.tool_kind ?? "tool");
  const kindLabel = PERM_TOOL_LABELS[rawKind] ?? rawKind;
  const chosen = String(turn.payload.chosen_option ?? "");
  const resultLabel =
    PERM_RESULT_LABELS[chosen] ??
    PERM_RESULT_LABELS[turn.status] ??
    (chosen || turn.status);
  const cwd = String(turn.payload.cwd ?? "");
  const cmd = String(turn.payload.command ?? "");
  const reason = String(turn.payload.reason ?? "");

  if (!pending) {
    const summaryLabel = cmd || reason || "授权操作";
    return (
      <details className="acp-accordion acp-perm-resolved-card">
        <summary className="acp-acc-summary">
          <ChevronRightIcon size={14} className="acp-acc-chevron" />
          <span className="acp-chip">{kindLabel}</span>
          <span className="acp-acc-label mono-code" title={summaryLabel}>
            {summaryLabel}
          </span>
          <span className="acp-tool-summary-right">
            <span className="acp-mini-badge">{resultLabel}</span>
          </span>
        </summary>
        <div className="acp-acc-body" style={{ paddingTop: 10 }}>
          {reason && (
            <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>{reason}</div>
          )}
          {cwd && (
            <div className="muted mono-code" style={{ fontSize: 11.5 }}>
              目录：{cwd}
            </div>
          )}
          {cmd && <pre className="acp-term-pre">{cmd}</pre>}
          <div className="muted" style={{ fontSize: 12 }}>
            结果：{resultLabel}
          </div>
        </div>
      </details>
    );
  }

  return (
    <div className="acp-turn-card acp-perm-card">
      <div className="row between" style={{ marginBottom: 6 }}>
        <b style={{ fontSize: 13 }}>⚠️ 需要授权执行操作</b>
        <span className="acp-chip">{kindLabel}</span>
      </div>
      <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>{reason}</div>
      {cwd && (
        <div className="muted mono-code" style={{ fontSize: 11.5, marginTop: 4 }}>
          目录：{cwd}
        </div>
      )}
      {cmd && <pre className="acp-term-pre">{cmd}</pre>}
      <div className="acp-perm-actions">
        <button
          type="button"
          className="btn-accent"
          onClick={() => onReply(turn.id, "allow_once")}
        >
          允许一次
        </button>
        <button
          type="button"
          className="btn-ghost"
          onClick={() => onReply(turn.id, "allow_always")}
        >
          始终允许
        </button>
        <button
          type="button"
          className="btn-danger-soft"
          onClick={() => onReply(turn.id, "reject_once")}
        >
          拒绝一次
        </button>
        <button
          type="button"
          className="btn-danger-soft"
          onClick={() => onReply(turn.id, "reject_always")}
        >
          始终拒绝
        </button>
      </div>
    </div>
  );
}

function PlanTurnCard({ turn }: { turn: AcpTurn }) {
  const steps = Array.isArray(turn.payload.steps)
    ? (turn.payload.steps as AcpPlanStep[])
    : [];
  const doneCount = steps.filter((st) => st.state === "done").length;
  const activeStep = steps.find((st) => st.state === "active");

  return (
    <details className="acp-accordion acp-plan-card">
      <summary className="acp-acc-summary">
        <ChevronRightIcon size={14} className="acp-acc-chevron" />
        <span className="acp-acc-label">
          执行计划 ({doneCount}/{steps.length})
        </span>
        {activeStep && (
          <span className="acp-acc-preview" title={activeStep.label}>
            {activeStep.label}
          </span>
        )}
        <span className="acp-tool-summary-right">
          <span className="acp-mini-badge">
            {doneCount}/{steps.length}
          </span>
        </span>
      </summary>
      <div className="acp-acc-body" style={{ paddingTop: 10 }}>
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
    </details>
  );
}

function TerminalOutTurnCard({ turn }: { turn: AcpTurn }) {
  const exit = Number(turn.payload.exit_code ?? 0);
  const cmd = String(turn.payload.command ?? "终端输出");
  const output = String(turn.payload.output ?? "");
  const [copied, setCopied] = useState(false);

  return (
    <details className="acp-accordion acp-term-card">
      <summary className="acp-acc-summary">
        <ChevronRightIcon size={14} className="acp-acc-chevron" />
        <TerminalIcon size={14} />
        <span className="acp-acc-label mono-code" title={cmd}>
          {cmd}
        </span>
        <span className="acp-tool-summary-right">
          <span className={`acp-mini-badge${exit !== 0 ? " is-err" : ""}`}>
            exit {exit}
          </span>
        </span>
      </summary>
      <div className="acp-acc-body" style={{ paddingTop: 8 }}>
        {output && (
          <div className="acp-tool-sec-head">
            <span className="acp-tool-sec-label">终端输出</span>
            <button
              type="button"
              className="acp-inline-icon-btn"
              aria-label="复制终端输出"
              onClick={() =>
                copyClipboard(output, () => {
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1500);
                })
              }
            >
              {copied ? <CheckIcon size={12} /> : <CopyIcon />}
              <span>{copied ? "已复制" : "复制"}</span>
            </button>
          </div>
        )}
        <pre className="acp-term-pre">{output}</pre>
      </div>
    </details>
  );
}

export interface AcpTurnItemProps {
  turn: AcpTurn;
  onDownloadAttachment: (id: string, fileName: string) => void;
  onPreviewAttachment?: (att: AcpAttachment) => void;
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
  onReplyPermission: (
    turnId: string,
    opt: "allow_once" | "allow_always" | "reject_once" | "reject_always",
  ) => void;
}

export const AcpTurnItem = memo(function AcpTurnItem({
  turn,
  onDownloadAttachment,
  onPreviewAttachment,
  onPreviewFilePath,
  onReplyPermission,
}: AcpTurnItemProps) {
  switch (turn.role) {
    case "user":
      return (
        <UserTurnCard
          turn={turn}
          onDownloadAttachment={onDownloadAttachment}
          onPreviewAttachment={onPreviewAttachment}
          onPreviewFilePath={onPreviewFilePath}
        />
      );
    case "agent_thought":
      return <ThoughtTurnCard turn={turn} />;
    case "tool_call":
      return <ToolCallCard turn={turn} onPreviewFilePath={onPreviewFilePath} />;
    case "agent_plan":
      return <PlanTurnCard turn={turn} />;
    case "permission":
      return <PermissionTurnCard turn={turn} onReply={onReplyPermission} />;
    case "terminal_out":
      return <TerminalOutTurnCard turn={turn} />;
    case "agent_text":
    default:
      return <AgentTextTurn turn={turn} onPreviewFilePath={onPreviewFilePath} />;
  }
});

export interface ProcessGroupCardProps {
  turns: AcpTurn[];
  onDownloadAttachment: (id: string, fileName: string) => void;
  onPreviewAttachment?: (att: AcpAttachment) => void;
  onPreviewFilePath?: (ref: ParsedFileRef) => void;
  onReplyPermission: (
    turnId: string,
    opt: "allow_once" | "allow_always" | "reject_once" | "reject_always",
  ) => void;
}

export const ProcessGroupCard = memo(function ProcessGroupCard({
  turns,
  onDownloadAttachment,
  onPreviewAttachment,
  onPreviewFilePath,
  onReplyPermission,
}: ProcessGroupCardProps) {
  let thoughtCount = 0;
  let toolCount = 0;
  let termCount = 0;
  let planCount = 0;
  let permCount = 0;
  let hasRunning = false;
  let failedCount = 0;
  let totalDurationMs = 0;

  for (const t of turns) {
    if (
      t.status === "streaming" ||
      t.status === "pending" ||
      t.status === "in_progress"
    ) {
      hasRunning = true;
    }
    if (typeof t.payload.duration_ms === "number" && Number.isFinite(t.payload.duration_ms) && t.payload.duration_ms >= 0) {
      totalDurationMs += t.payload.duration_ms;
    }
    switch (t.role) {
      case "agent_thought":
        thoughtCount++;
        break;
      case "tool_call":
        toolCount++;
        if (t.status === "failed" || Boolean(t.payload.is_error)) {
          failedCount++;
        }
        break;
      case "terminal_out":
        termCount++;
        if (Number(t.payload.exit_code ?? 0) !== 0) {
          failedCount++;
        }
        break;
      case "agent_plan":
        planCount++;
        break;
      case "permission":
        permCount++;
        break;
      default:
        break;
    }
  }

  const summaryParts = [
    thoughtCount > 0 ? `思考 ${thoughtCount} 次` : "",
    toolCount > 0 ? `工具 ${toolCount} 次` : "",
    termCount > 0 ? `终端 ${termCount} 次` : "",
    planCount > 0 ? `计划 ${planCount} 项` : "",
    permCount > 0 ? `授权 ${permCount} 次` : "",
  ]
    .filter(Boolean)
    .join(" · ");

  let livePreview = "";
  if (hasRunning) {
    for (let i = turns.length - 1; i >= 0; i--) {
      const t = turns[i];
      if (
        t.status !== "streaming" &&
        t.status !== "pending" &&
        t.status !== "in_progress"
      ) {
        continue;
      }
      if (t.role === "tool_call") {
        livePreview = String(t.payload.title || t.payload.name || "执行工具中");
        break;
      }
      if (t.role === "agent_thought") {
        livePreview = firstNonEmptyLine(String(t.payload.text ?? "")) || "正在思考…";
        break;
      }
      if (t.role === "terminal_out") {
        livePreview = String(t.payload.command ?? "终端执行中");
        break;
      }
    }
  }

  const totalDurationLabel =
    !hasRunning && totalDurationMs > 0 ? formatDurationMs(totalDurationMs) : null;

  return (
    <details
      className={`acp-accordion acp-process-group${hasRunning ? " is-running" : ""}${!hasRunning && failedCount > 0 ? " is-failed" : ""}`}
    >
      <summary className="acp-acc-summary acp-group-summary">
        <ChevronRightIcon size={14} className="acp-acc-chevron" />
        <span className="acp-tool-state-icon" aria-hidden="true">
          {hasRunning ? (
            <span className="acp-tool-spinner" />
          ) : failedCount > 0 ? (
            <AlertCircleIcon size={14} className="acp-status-err" />
          ) : (
            <ActivityIcon size={14} className="acp-group-icon" />
          )}
        </span>
        <span className="acp-acc-label">
          {hasRunning
            ? `执行中 · ${summaryParts || "处理中"}`
            : `执行过程 · ${summaryParts || `${turns.length} 项`}`}
        </span>
        {hasRunning && livePreview && (
          <span className="acp-acc-preview" title={livePreview}>
            {livePreview}
          </span>
        )}
        <span className="acp-tool-summary-right">
          {hasRunning ? (
            <span className="acp-mini-badge is-live">进行中</span>
          ) : (
            <>
              {failedCount > 0 && (
                <span className="acp-mini-badge is-err">{failedCount} 项失败</span>
              )}
              {totalDurationLabel && (
                <span className="acp-mini-badge">{totalDurationLabel}</span>
              )}
            </>
          )}
        </span>
      </summary>
      <div className="acp-acc-body acp-group-body">
        {turns.map((t) => (
          <AcpTurnItem
            key={t.id}
            turn={t}
            onDownloadAttachment={onDownloadAttachment}
            onPreviewAttachment={onPreviewAttachment}
            onPreviewFilePath={onPreviewFilePath}
            onReplyPermission={onReplyPermission}
          />
        ))}
      </div>
    </details>
  );
});
