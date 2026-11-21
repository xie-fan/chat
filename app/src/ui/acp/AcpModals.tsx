import { useEffect, useState } from "react";
import {
  createAgentLocalFirst,
  createSessionLocalFirst,
  createWorkspaceLocalFirst,
  listDirsLocalFirst,
} from "../../acpHost";
import { acp, getServer, inTauri } from "../../api";
import { copyPlainText } from "../../clipboard";
import { setState, useApp } from "../../store";
import type { AcpDirListing, AcpWorkspace } from "../../types";
import { autoInstallPackageForCmd } from "./adapterHint";
import {
  CheckIcon,
  ChevronLeftIcon,
  CpuIcon,
  FolderIcon,
  RefreshIcon,
  TerminalIcon,
  XIcon,
} from "../icons";

const PRESETS: {
  id: string;
  label: string;
  name: string;
  cmd: string;
}[] = [
  { id: "claude", label: "Claude Code", name: "Claude Code", cmd: "claude-agent-acp" },
  { id: "codex", label: "Codex CLI", name: "Codex CLI", cmd: "codex-acp" },
  { id: "gemini", label: "Gemini CLI", name: "Gemini CLI", cmd: "gemini --acp" },
  { id: "opencode", label: "OpenCode", name: "OpenCode", cmd: "opencode acp" },
  { id: "pi", label: "Pi", name: "Pi", cmd: "pi --mode rpc" },
  { id: "custom", label: "自定义 ACP", name: "Custom Agent", cmd: "" },
];

function useEscapeClose(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

export function FolderPickerModal({
  hostId,
  initialPath = "",
  onPick,
  onClose,
}: {
  hostId: string;
  initialPath?: string;
  onPick: (path: string) => void;
  onClose: () => void;
}) {
  useEscapeClose(onClose);
  const [path, setPath] = useState(initialPath);
  const [listing, setListing] = useState<AcpDirListing | null>(null);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState("");

  const loadDir = async (target: string) => {
    setLoading(true);
    setErr("");
    try {
      const res = await listDirsLocalFirst(hostId, target);
      setListing(res);
      setPath(res.path);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "读取远程目录失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadDir(initialPath);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hostId]);

  return (
    <div className="acp-modal-mask" onClick={onClose}>
      <div
        className="acp-modal"
        role="dialog"
        aria-modal="true"
        aria-label="选择工作目录"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="st-card-head">
          <h3>选择工作目录</h3>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭">
            <XIcon size={16} />
          </button>
        </div>

        {listing && listing.roots.length > 0 && (
          <div className="acp-composer-pills">
            {listing.roots.map((r) => (
              <button
                key={r}
                type="button"
                className="btn-ghost"
                style={{ padding: "4px 9px", fontSize: 12 }}
                onClick={() => void loadDir(r)}
              >
                {r}
              </button>
            ))}
          </div>
        )}

        <div className="row" style={{ gap: 8 }}>
          <input
            className="mono-code"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void loadDir(path)}
            placeholder="输入路径后回车浏览"
          />
          <button type="button" className="btn-ghost" onClick={() => void loadDir(path)}>
            前往
          </button>
        </div>

        {err && <div className="auth-err">{err}</div>}

        <div className="acp-dir-list">
          {listing?.parent && (
            <button
              type="button"
              className="acp-dir-row"
              onClick={() => void loadDir(listing.parent!)}
            >
              <ChevronLeftIcon size={15} />
              <span>..（返回上一级）</span>
            </button>
          )}
          {loading && <div className="muted" style={{ padding: 10 }}>正在读取目录…</div>}
          {!loading && listing && listing.entries.length === 0 && (
            <div className="muted" style={{ padding: 10 }}>此目录下无子文件夹</div>
          )}
          {!loading &&
            listing?.entries.map((d) => (
              <button
                key={d.path}
                type="button"
                className="acp-dir-row"
                onClick={() => void loadDir(d.path)}
              >
                <FolderIcon size={15} />
                <span className="grow">{d.name}</span>
                {d.has_children && <span className="muted">›</span>}
              </button>
            ))}
        </div>

        <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
          <button type="button" className="btn-ghost" onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className="btn-accent"
            disabled={!listing?.path && !path.trim()}
            onClick={() => onPick(listing?.path || path.trim())}
          >
            选择此目录
          </button>
        </div>
      </div>
    </div>
  );
}

export function AddWorkspaceModal({
  defaultHostId,
  onClose,
}: {
  defaultHostId?: string | null;
  onClose: () => void;
}) {
  useEscapeClose(onClose);
  const s = useApp();
  const [hostId, setHostId] = useState(
    () =>
      defaultHostId ||
      s.acpHosts.find((h) => h.online)?.id ||
      s.acpHosts[0]?.id ||
      "",
  );
  const [cwd, setCwd] = useState("");
  const [name, setName] = useState("");
  const [showPicker, setShowPicker] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!hostId || !cwd.trim()) {
      setErr("请选择机器并填写工作目录路径");
      return;
    }
    setBusy(true);
    setErr("");
    try {
      const ws = await createWorkspaceLocalFirst({
        host_id: hostId,
        cwd: cwd.trim(),
        name: name.trim() || undefined,
      });
      setState((cur) => ({
        acpWorkspaces: [ws, ...cur.acpWorkspaces.filter((x) => x.id !== ws.id)],
        activeAcpHostId: ws.host_id,
        activeAcpWorkspaceId: ws.id,
      }));
      onClose();
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "添加工作目录失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="acp-modal-mask" onClick={onClose}>
        <form
          className="acp-modal"
          role="dialog"
          aria-modal="true"
          aria-label="添加工作目录"
          onClick={(e) => e.stopPropagation()}
          onSubmit={submit}
        >
          <div className="st-card-head">
            <h3>添加工作目录 (cwd)</h3>
            <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭">
              <XIcon size={16} />
            </button>
          </div>

          <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <span style={{ fontSize: 13, fontWeight: 500 }}>所属机器 (@Host)</span>
            <select
              className="acp-select"
              style={{ height: 38, borderRadius: 10 }}
              value={hostId}
              onChange={(e) => setHostId(e.target.value)}
            >
              {s.acpHosts.length === 0 && <option value="">暂无已接入机器</option>}
              {s.acpHosts.map((h) => (
                <option key={h.id} value={h.id}>
                  @{h.name} ({h.online ? "在线" : "离线"})
                </option>
              ))}
            </select>
          </label>

          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <span style={{ fontSize: 13, fontWeight: 500 }}>目录路径</span>
            <div className="row" style={{ gap: 8 }}>
              <input
                className="mono-code"
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="/home/user/projects/app 或 C:\projects\app"
                required
              />
              <button
                type="button"
                className="btn-ghost"
                disabled={!hostId}
                onClick={() => setShowPicker(true)}
              >
                浏览…
              </button>
            </div>
          </div>

          <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <span style={{ fontSize: 13, fontWeight: 500 }}>显示别名（可选）</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="留空则使用文件夹名称"
            />
          </label>

          {err && <div className="auth-err">{err}</div>}

          <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
            <button type="button" className="btn-ghost" onClick={onClose}>
              取消
            </button>
            <button type="submit" className="btn-accent" disabled={busy || !hostId}>
              {busy ? "创建中…" : "确认添加"}
            </button>
          </div>
        </form>
      </div>

      {showPicker && hostId && (
        <FolderPickerModal
          hostId={hostId}
          initialPath={cwd}
          onPick={(p) => {
            setCwd(p);
            setShowPicker(false);
          }}
          onClose={() => setShowPicker(false)}
        />
      )}
    </>
  );
}

export function NewSessionModal({
  workspace,
  onClose,
}: {
  workspace: AcpWorkspace;
  onClose: () => void;
}) {
  useEscapeClose(onClose);
  const s = useApp();
  const hostAgents = s.acpAgents.filter((a) => a.host_id === workspace.host_id);
  const [agentId, setAgentId] = useState(hostAgents[0]?.id ?? "");
  const [title, setTitle] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!agentId) {
      setErr("该机器上还没有可用的 Agent，请先在设置或工作区接入 Agent");
      return;
    }
    setBusy(true);
    setErr("");
    try {
      await createSessionLocalFirst({
        workspace,
        agent_id: agentId,
        title: title.trim() || undefined,
      });
      onClose();
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "新建会话失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="acp-modal-mask" onClick={onClose}>
      <form
        className="acp-modal"
        role="dialog"
        aria-modal="true"
        aria-label="新建 Agent 对话"
        onClick={(e) => e.stopPropagation()}
        onSubmit={submit}
      >
        <div className="st-card-head">
          <h3>在「{workspace.name}」新建对话</h3>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭">
            <XIcon size={16} />
          </button>
        </div>

        <div className="muted mono-code" style={{ fontSize: 12 }}>
          {workspace.cwd}
        </div>

        <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <span style={{ fontSize: 13, fontWeight: 500 }}>选择驱动 Agent</span>
          <select
            className="acp-select"
            style={{ height: 38, borderRadius: 10 }}
            value={agentId}
            onChange={(e) => setAgentId(e.target.value)}
          >
            {hostAgents.length === 0 && <option value="">当前机器暂无 Agent</option>}
            {hostAgents.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name} ({a.cmd})
              </option>
            ))}
          </select>
        </label>

        <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <span style={{ fontSize: 13, fontWeight: 500 }}>对话标题（可选）</span>
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="例如：重构登录流程 / 排查性能问题"
          />
        </label>

        {err && <div className="auth-err">{err}</div>}

        <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
          <button type="button" className="btn-ghost" onClick={onClose}>
            取消
          </button>
          <button type="submit" className="btn-accent" disabled={busy || !agentId}>
            {busy ? "创建中…" : "开始对话"}
          </button>
        </div>
      </form>
    </div>
  );
}

export function AttachAgentModal({
  defaultHostId,
  initialTab = "agent",
  onClose,
}: {
  defaultHostId?: string | null;
  initialTab?: "agent" | "host";
  onClose: () => void;
}) {
  useEscapeClose(onClose);
  const s = useApp();
  const [tab, setTab] = useState<"agent" | "host">(
    s.acpHosts.length === 0 ? "host" : initialTab,
  );
  const [hostId, setHostId] = useState(
    () =>
      defaultHostId ||
      s.acpHosts.find((h) => h.online)?.id ||
      s.acpHosts[0]?.id ||
      "",
  );
  const [preset, setPreset] = useState("claude");
  const [name, setName] = useState("Claude Code");
  const [cmd, setCmd] = useState("claude-agent-acp");
  const [envText, setEnvText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [copied, setCopied] = useState(false);
  const [pairCode, setPairCode] = useState("");
  const [pairExpires, setPairExpires] = useState<number | null>(null);
  const [pairBusy, setPairBusy] = useState(false);

  const srv = getServer() || (typeof window !== "undefined" ? window.location.origin : "");
  const cliCmd = `chat-app --acp-host --server "${srv}" --pair "${pairCode || "<PAIR_CODE>"}" --name "dev-node"`;

  const generatePairCode = async () => {
    setPairBusy(true);
    setErr("");
    try {
      const res = await acp.createPairCode();
      setPairCode(res.code);
      setPairExpires(res.expires_in);
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "生成配对码失败");
    } finally {
      setPairBusy(false);
    }
  };

  useEffect(() => {
    if (tab === "host" && !pairCode && !pairBusy) {
      void generatePairCode();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  const applyPreset = (id: string) => {
    setPreset(id);
    const p = PRESETS.find((x) => x.id === id);
    if (p) {
      setName(p.name);
      if (p.cmd) setCmd(p.cmd);
    }
  };

  const submitAgent = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!hostId) {
      setErr("请先接入至少一台在线宿主机器");
      return;
    }
    if (!name.trim() || !cmd.trim()) {
      setErr("名称和启动命令不能为空");
      return;
    }
    const env: Record<string, string> = {};
    for (const raw of envText.split(/\r?\n/)) {
      const line = raw.trim();
      if (!line || line.startsWith("#")) continue;
      const eq = line.indexOf("=");
      if (eq > 0) env[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
    }

    setBusy(true);
    setErr("");
    try {
      await createAgentLocalFirst({
        host_id: hostId,
        preset,
        name: name.trim(),
        cmd: cmd.trim(),
        env: Object.keys(env).length ? env : undefined,
      });
      onClose();
    } catch (ex) {
      setErr(ex instanceof Error ? ex.message : "添加 Agent 失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="acp-modal-mask" onClick={onClose}>
      <div
        className="acp-modal"
        role="dialog"
        aria-modal="true"
        aria-label="通过 ACP 协议接入本地 Agent"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="st-card-head">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <CpuIcon size={18} />
            <h3 style={{ margin: 0 }}>通过 ACP 协议接入 Agent</h3>
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭">
            <XIcon size={16} />
          </button>
        </div>

        <div className="auth-tabs">
          <button
            type="button"
            className={`auth-tab${tab === "agent" ? " on" : ""}`}
            onClick={() => setTab("agent")}
          >
            添加机器上的 Agent
          </button>
          <button
            type="button"
            className={`auth-tab${tab === "host" ? " on" : ""}`}
            onClick={() => setTab("host")}
          >
            命令行接入新机器
          </button>
        </div>

        {tab === "agent" ? (
          <form onSubmit={submitAgent} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>运行宿主 (@Host)</span>
              <select
                className="acp-select"
                style={{ height: 38, borderRadius: 10 }}
                value={hostId}
                onChange={(e) => setHostId(e.target.value)}
              >
                {s.acpHosts.length === 0 && <option value="">暂无已接入机器</option>}
                {s.acpHosts.map((h) => (
                  <option key={h.id} value={h.id}>
                    @{h.name} ({h.online ? "在线" : "离线"} · {h.os})
                  </option>
                ))}
              </select>
            </label>

            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>快捷预设</span>
              <div className="acp-composer-pills">
                {PRESETS.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    className={`accent-pill${preset === p.id ? " on" : ""}`}
                    style={{ padding: "5px 10px", fontSize: 12 }}
                    onClick={() => applyPreset(p.id)}
                  >
                    {p.label}
                  </button>
                ))}
              </div>
            </div>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>Agent 名称</span>
              <input value={name} onChange={(e) => setName(e.target.value)} required />
            </label>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>ACP Stdio 启动命令</span>
              <input
                className="mono-code"
                value={cmd}
                onChange={(e) => setCmd(e.target.value)}
                placeholder="例如 claude-agent-acp、codex-acp 或 pi --mode rpc"
                required
              />
              {(() => {
                const pkg = autoInstallPackageForCmd(cmd);
                return pkg ? (
                  <span className="muted" style={{ fontSize: 12, lineHeight: 1.5 }}>
                    本机未检测到该命令时，接入或首次对话将自动通过 npm 安装{" "}
                    <code className="mono-code">{pkg}</code>（需已安装 Node.js / npm）。
                  </span>
                ) : null;
              })()}
            </label>

            <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span style={{ fontSize: 13, fontWeight: 500 }}>环境变量（可选，每行 KEY=VALUE）</span>
              <textarea
                rows={3}
                className="mono-code"
                value={envText}
                onChange={(e) => setEnvText(e.target.value)}
                placeholder="ANTHROPIC_API_KEY=sk-..."
              />
            </label>

            {err && <div className="auth-err">{err}</div>}

            <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
              <button type="button" className="btn-ghost" onClick={onClose}>
                取消
              </button>
              <button type="submit" className="btn-accent" disabled={busy || !hostId}>
                {busy ? "注册中…" : "确认接入"}
              </button>
            </div>
          </form>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <div className="bento-card" style={{ padding: 14 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 8, fontWeight: 600 }}>
                <TerminalIcon size={16} />
                <span>桌面端与远程无头节点接入指南</span>
              </div>
              <p className="muted" style={{ margin: 0, fontSize: 12.5, lineHeight: 1.6 }}>
                {inTauri()
                  ? "你正在使用桌面客户端，本机已在登录后自动通过 ACP 宿主通道上线。若需接入其他 Linux / macOS / GPU 服务器，请在目标机器运行以下一次性配对命令（5 分钟内单次有效，无头模式可按需附加 --trust-agents 与 --allow-cwd <DIR>）："
                  : "Web 端无法直接启动本机进程。请下载桌面端登录后自动接入本机，或在任意服务器终端运行以下一次性配对命令以无头守护进程接入（5 分钟内单次有效，可按需附加 --trust-agents 与 --allow-cwd <DIR>）："}
              </p>
              <pre className="acp-term-pre" style={{ userSelect: "all" }}>
                {cliCmd}
              </pre>
              {err && <div className="auth-err">{err}</div>}
              <div className="row" style={{ justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: 8 }}>
                <span className="muted" style={{ fontSize: 11.5 }}>
                  {pairCode
                    ? `配对码有效期 ${Math.round((pairExpires ?? 300) / 60)} 分钟（单次兑换后自动失效）`
                    : "点击右侧生成一次性配对码后再复制命令"}
                </span>
                <div className="row" style={{ gap: 8 }}>
                  <button
                    type="button"
                    className="btn-ghost"
                    disabled={pairBusy}
                    onClick={() => void generatePairCode()}
                  >
                    {pairBusy ? "生成中…" : pairCode ? "重新生成配对码" : "生成一次性配对码"}
                  </button>
                  <button
                    type="button"
                    className="btn-accent"
                    disabled={!pairCode}
                    onClick={() => {
                      void copyPlainText(cliCmd).then((ok) => {
                        if (ok) {
                          setCopied(true);
                          setTimeout(() => setCopied(false), 1800);
                        }
                      });
                    }}
                  >
                    {copied ? (
                      <>
                        <CheckIcon size={14} /> 已复制
                      </>
                    ) : (
                      "复制命令"
                    )}
                  </button>
                </div>
              </div>
            </div>

            <div className="row" style={{ justifyContent: "flex-end", gap: 8 }}>
              <button
                type="button"
                className="btn-ghost"
                onClick={() =>
                  void acp.bootstrap().then((bs) =>
                    setState((cur) => ({
                      acpSettings: bs.settings,
                      acpHosts: bs.hosts,
                      acpAgents: bs.agents,
                      acpWorkspaces: bs.workspaces,
                      acpSessions: bs.sessions,
                      acpLoaded: true,
                    })),
                  )
                }
              >
                <RefreshIcon size={14} /> 刷新节点状态
              </button>
              <button type="button" className="btn-accent" onClick={onClose}>
                完成
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
