import { useEffect, useState } from "react";
import { acp, isDesktopShell } from "../../api";
import {
  deleteAgentLocalFirst,
  getLocalPolicy,
  isLocalAcpHost,
  mutateAgentLocalFirst,
  mutateSettingsLocalFirst,
  probeAgentModelsLocalFirst,
  renameHostLocalFirst,
  setCloudControlEnabled,
  setLocalAllowAll,
} from "../../acpHost";
import { applyAcpBootstrap, setState, useApp } from "../../store";
import type { AcpAgent, AcpPermissionPolicy } from "../../types";
import { AttachAgentModal } from "../acp/AcpModals";
import { agentStatusLabel } from "../acp/adapterHint";
import {
  ChevronDownIcon,
  ChevronRightIcon,
  CpuIcon,
  PlusIcon,
  RefreshIcon,
  TrashIcon,
  XIcon,
} from "../icons";
import { confirmDialog } from "../Modal";

const GLOBAL_POLICIES: {
  v: AcpPermissionPolicy;
  title: string;
  desc: string;
}[] = [
  {
    v: "ask",
    title: "每次询问 (Ask)",
    desc: "所有读写与终端命令均需手动审批，最稳妥",
  },
  {
    v: "auto_read",
    title: "自动允许只读 (Auto Safe)",
    desc: "自动放行目录读取与检索，写入和命令需审批",
  },
  {
    v: "allow_all",
    title: "完全自动执行 (Full Auto)",
    desc: "工作目录内读写与终端命令自动放行，慎选",
  },
];

export default function AgentSettingsSection() {
  const s = useApp();
  const [collapsedHosts, setCollapsedHosts] = useState<Set<string>>(() => new Set());
  const [customInputs, setCustomInputs] = useState<Record<string, string>>({});
  const [renamingHostId, setRenamingHostId] = useState<string | null>(null);
  const [hostNameDraft, setHostNameDraft] = useState("");
  const [attachHostId, setAttachHostId] = useState<string | null | undefined>(undefined);
  const [err, setErr] = useState("");
  const [probingAgents, setProbingAgents] = useState<Record<string, boolean>>({});
  const [localAllowAll, setLocalAllowAllState] = useState(false);
  const [cloudControl, setCloudControlState] = useState(true);
  const [localPolicyBusy, setLocalPolicyBusy] = useState(false);
  const [cloudControlBusy, setCloudControlBusy] = useState(false);

  useEffect(() => {
    if (isDesktopShell()) {
      void getLocalPolicy().then((p) => {
        setLocalAllowAllState(p.allow_all_local);
        setCloudControlState(p.cloud_control_enabled);
      });
    }
  }, []);

  useEffect(() => {
    if (!s.acpLoaded) {
      void acp
        .bootstrap()
        .then((bs) => setState((cur) => applyAcpBootstrap(cur, bs)))
        .catch(() => {});
    }
  }, [s.acpLoaded]);

  const setPolicy = (policy: AcpPermissionPolicy) => {
    setErr("");
    mutateSettingsLocalFirst(policy);
  };

  const toggleHost = (id: string) => {
    setCollapsedHosts((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const addModelOption = (ag: AcpAgent) => {
    const val = (customInputs[ag.id] ?? "").trim();
    if (!val || ag.model_options.includes(val)) return;
    const nextOptions = [...ag.model_options, val];
    setErr("");
    setCustomInputs((m) => ({ ...m, [ag.id]: "" }));
    mutateAgentLocalFirst(ag.id, { model_options: nextOptions });
  };

  const removeModelOption = (ag: AcpAgent, target: string) => {
    const nextOptions = ag.model_options.filter((m) => m !== target);
    if (nextOptions.length === 0) return;
    const nextDefault = nextOptions.includes(ag.default_model)
      ? ag.default_model
      : nextOptions[0];
    setErr("");
    mutateAgentLocalFirst(ag.id, {
      model_options: nextOptions,
      default_model: nextDefault,
    });
  };

  const refreshAgentModels = async (ag: AcpAgent) => {
    if (probingAgents[ag.id]) return;
    setErr("");
    setProbingAgents((m) => ({ ...m, [ag.id]: true }));
    try {
      const updated = await probeAgentModelsLocalFirst(ag, { wait: true });
      setState((cur) => ({
        acpAgents: cur.acpAgents.map((a) => (a.id === updated.id ? updated : a)),
      }));
    } catch (e) {
      setErr(e instanceof Error ? e.message : "探测模型失败");
    } finally {
      setProbingAgents((m) => ({ ...m, [ag.id]: false }));
    }
  };

  const removeAgent = async (agentId: string, name: string) => {
    if (
      !(await confirmDialog(`确定移除 Agent「${name}」？关联的对话也将删除。`, {
        danger: true,
        okText: "移除",
      }))
    )
      return;
    try {
      await deleteAgentLocalFirst(agentId);
      setState((cur) => ({
        acpAgents: cur.acpAgents.filter((a) => a.id !== agentId),
        acpSessions: cur.acpSessions.filter((sess) => sess.agent_id !== agentId),
      }));
    } catch (e) {
      setErr(e instanceof Error ? e.message : "移除失败");
    }
  };

  const removeHost = async (hostId: string, name: string) => {
    if (
      !(await confirmDialog(`确定解绑机器「@${name}」及其下所有工作区与 Agent？`, {
        danger: true,
        okText: "解绑",
      }))
    )
      return;
    try {
      await acp.deleteHost(hostId);
      setState((cur) => ({
        acpHosts: cur.acpHosts.filter((h) => h.id !== hostId),
        acpAgents: cur.acpAgents.filter((a) => a.host_id !== hostId),
        acpWorkspaces: cur.acpWorkspaces.filter((w) => w.host_id !== hostId),
      }));
    } catch (e) {
      setErr(e instanceof Error ? e.message : "解绑失败");
    }
  };

  return (
    <div className="st-container">
      {err && <div className="auth-err">{err}</div>}

      <section className="st-card">
        <div className="st-card-head">
          <div>
            <h3>全局权限策略</h3>
            <div className="st-card-sub">作为新对话及未单独覆写 Agent 的默认审批策略</div>
          </div>
        </div>

        <div className="agent-policy-grid">
          {GLOBAL_POLICIES.map((p) => (
            <button
              key={p.v}
              type="button"
              className={`agent-policy-card${s.acpSettings.default_permission === p.v ? " on" : ""}`}
              onClick={() => void setPolicy(p.v)}
            >
              <strong>{p.title}</strong>
              <span>{p.desc}</span>
            </button>
          ))}
        </div>

        {isDesktopShell() && (
          <>
            <div
              className="row between"
              style={{
                marginTop: 12,
                paddingTop: 12,
                borderTop: "1px solid var(--border-subtle, rgba(0,0,0,0.08))",
                gap: 12,
              }}
            >
              <div>
                <div style={{ fontSize: 13.5, fontWeight: 600 }}>
                  本机允许被云端控制（多端同步）
                </div>
                <div className="muted" style={{ fontSize: 12 }}>
                  开启时手机与网页端可查看并远程操控本机 Agent；关闭后其他端不可见，且本机数据立即从云端全部删除、不再同步（本机自身仍可纯本地正常使用）。
                </div>
              </div>
              <input
                type="checkbox"
                aria-label="本机允许被云端控制"
                checked={cloudControl}
                disabled={cloudControlBusy}
                onChange={(e) => {
                  const next = e.target.checked;
                  setErr("");
                  setCloudControlBusy(true);
                  void setCloudControlEnabled(next)
                    .then((v) => setCloudControlState(v.cloud_control_enabled))
                    .catch((ex) =>
                      setErr(ex instanceof Error ? ex.message : "切换云端控制状态失败"),
                    )
                    .finally(() => setCloudControlBusy(false));
                }}
              />
            </div>

            <div
              className="row between"
              style={{
                marginTop: 12,
                paddingTop: 12,
                borderTop: "1px solid var(--border-subtle, rgba(0,0,0,0.08))",
                gap: 12,
              }}
            >
              <div>
                <div style={{ fontSize: 13.5, fontWeight: 600 }}>
                  本机允许完全自动执行 (allow_all)
                </div>
                <div className="muted" style={{ fontSize: 12 }}>
                  未开启本机放行时，传入的 allow_all 将在本机自动降级为 auto_read。
                </div>
              </div>
              <input
                type="checkbox"
                aria-label="本机允许完全自动执行"
                checked={localAllowAll}
                disabled={localPolicyBusy}
                onChange={(e) => {
                  const next = e.target.checked;
                  setErr("");
                  setLocalPolicyBusy(true);
                  void setLocalAllowAll(next)
                    .then((v) => setLocalAllowAllState(v.allow_all_local))
                    .catch((ex) => setErr(ex instanceof Error ? ex.message : "更新本机策略失败"))
                    .finally(() => setLocalPolicyBusy(false));
                }}
              />
            </div>
          </>
        )}
      </section>

      <section className="st-card">
        <div className="st-card-head">
          <div>
            <h3>已接入宿主节点 (@Host) 与 Agent 列表</h3>
            <div className="st-card-sub">
              管理各机器上注册的 ACP CLI Agent、默认模型、自定义模型与权限覆写
            </div>
          </div>
          <button
            type="button"
            className="btn-accent"
            onClick={() => setAttachHostId(s.acpHosts[0]?.id ?? null)}
          >
            <PlusIcon size={14} /> 通过 ACP 协议接入本地 Agent
          </button>
        </div>

        {s.acpHosts.length === 0 ? (
          <div className="empty-state" style={{ padding: "28px 16px" }}>
            <CpuIcon size={30} />
            <div className="empty-title">还没有已接入的机器节点</div>
            <div className="empty-hint-sm">
              使用桌面端登录本账号后本机将自动上线，或点击右上角查看远程无头接入命令。
            </div>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {s.acpHosts.map((h) => {
              const open = !collapsedHosts.has(h.id);
              const agents = s.acpAgents.filter((a) => a.host_id === h.id);
              return (
                <div key={h.id} className="host-group-card">
                  <div
                    className="host-group-head"
                    aria-expanded={open}
                    onClick={() => toggleHost(h.id)}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      {open ? <ChevronDownIcon size={15} /> : <ChevronRightIcon size={15} />}
                      <span
                        className="dot"
                        style={{ background: h.online ? "var(--ok)" : "var(--text-muted)" }}
                      />
                      {renamingHostId === h.id ? (
                        <div className="row" style={{ gap: 6 }} onClick={(e) => e.stopPropagation()}>
                          <input
                            value={hostNameDraft}
                            onChange={(e) => setHostNameDraft(e.target.value)}
                          />
                          <button
                            type="button"
                            className="btn-accent"
                            onClick={() => {
                              setErr("");
                              void renameHostLocalFirst(h.id, hostNameDraft.trim())
                                .then(() => {
                                  setRenamingHostId(null);
                                })
                                .catch((e) =>
                                  setErr(e instanceof Error ? e.message : "重命名主机失败"),
                                );
                            }}
                          >
                            保存
                          </button>
                        </div>
                      ) : (
                        <span>@{h.name}</span>
                      )}
                      <span className="acp-chip">{h.os}</span>
                      <span className="acp-chip">
                        {h.ip === "Local-Only"
                          ? "仅本机"
                          : h.online
                            ? `${h.latency_ms}ms`
                            : "离线"}
                      </span>
                    </div>

                    <div className="row" style={{ gap: 6 }} onClick={(e) => e.stopPropagation()}>
                      <button
                        type="button"
                        className="btn-ghost"
                        style={{ padding: "4px 8px", fontSize: 12 }}
                        onClick={() => setAttachHostId(h.id)}
                      >
                        + 添加 Agent
                      </button>
                      <button
                        type="button"
                        className="btn-ghost"
                        style={{ padding: "4px 8px", fontSize: 12 }}
                        onClick={() => {
                          setHostNameDraft(h.name);
                          setRenamingHostId(h.id);
                        }}
                      >
                        重命名
                      </button>
                      {h.ip !== "Local-Only" && (
                        <button
                          type="button"
                          className="icon-btn"
                          title="解绑节点"
                          onClick={() => void removeHost(h.id, h.name)}
                        >
                          <TrashIcon size={14} />
                        </button>
                      )}
                    </div>
                  </div>

                  {open && (
                    <div className="host-group-body">
                      {agents.length === 0 ? (
                        <div className="muted" style={{ paddingTop: 10, fontSize: 12.5 }}>
                          该机器上尚未注册 Agent。
                        </div>
                      ) : (
                        agents.map((ag) => {
                          const st = agentStatusLabel(ag.status);
                          return (
                            <div key={ag.id} className="agent-item-card">
                              <div className="row between">
                                <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                                  <span className="acp-chip">{ag.short_tag}</span>
                                  <b>{ag.name}</b>
                                  <span
                                    className={`status-pill${st.tone === "online" || st.tone === "installing" ? " online" : ""}`}
                                  >
                                    {st.text}
                                  </span>
                                  <code className="muted" style={{ fontSize: 12 }}>
                                    {ag.cmd}
                                  </code>
                                </div>
                              <button
                                type="button"
                                className="icon-btn"
                                title="移除 Agent"
                                onClick={() => void removeAgent(ag.id, ag.name)}
                              >
                                <TrashIcon size={14} />
                              </button>
                            </div>

                            <div className="row" style={{ flexWrap: "wrap", gap: 12 }}>
                              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12.5 }}>
                                <span className="muted">默认模型</span>
                                <select
                                  className="acp-select"
                                  value={ag.default_model}
                                  onChange={(e) => {
                                    setErr("");
                                    mutateAgentLocalFirst(ag.id, { default_model: e.target.value });
                                  }}
                                >
                                  {ag.model_options.map((m) => (
                                    <option key={m} value={m}>
                                      {m}
                                    </option>
                                  ))}
                                </select>
                                <button
                                  type="button"
                                  className="btn-ghost"
                                  style={{ padding: "4px 8px", fontSize: 12 }}
                                  disabled={
                                    (!h.online && !isLocalAcpHost(h.id)) ||
                                    Boolean(probingAgents[ag.id])
                                  }
                                  title={
                                    h.online || isLocalAcpHost(h.id)
                                      ? "重新探测可用模型列表"
                                      : "宿主节点离线，无法探测模型"
                                  }
                                  onClick={() => void refreshAgentModels(ag)}
                                >
                                  <RefreshIcon size={12} />{" "}
                                  {probingAgents[ag.id] ? "探测中…" : "探测模型"}
                                </button>
                              </label>

                              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12.5 }}>
                                <span className="muted">权限策略</span>
                                <select
                                  className="acp-select"
                                  value={ag.permission_override}
                                  onChange={(e) => {
                                    setErr("");
                                    mutateAgentLocalFirst(ag.id, {
                                      permission_override: e.target.value as
                                        | "inherit"
                                        | AcpPermissionPolicy,
                                    });
                                  }}
                                >
                                  <option value="inherit">继承全局策略</option>
                                  <option value="ask">每次询问</option>
                                  <option value="auto_read">自动允许只读</option>
                                  <option value="allow_all">完全自动执行</option>
                                </select>
                              </label>
                            </div>

                            <div className="acp-composer-pills">
                              {ag.model_options.map((m) => (
                                <span key={m} className="acp-chip">
                                  <span>{m}</span>
                                  {ag.model_options.length > 1 && (
                                    <button
                                      type="button"
                                      style={{ border: "none", background: "none", cursor: "pointer", padding: 0 }}
                                      title="移除模型选项"
                                      aria-label={`移除模型 ${m}`}
                                      onClick={() => removeModelOption(ag, m)}
                                    >
                                      <XIcon size={11} />
                                    </button>
                                  )}
                                </span>
                              ))}
                              <div className="row" style={{ gap: 6 }}>
                                <input
                                  style={{ width: 160, height: 28, fontSize: 12, padding: "0 8px" }}
                                  value={customInputs[ag.id] ?? ""}
                                  onChange={(e) =>
                                    setCustomInputs((map) => ({
                                      ...map,
                                      [ag.id]: e.target.value,
                                    }))
                                  }
                                  placeholder="自定义模型名…"
                                  onKeyDown={(e) =>
                                    e.key === "Enter" && addModelOption(ag)
                                  }
                                />
                                <button
                                  type="button"
                                  className="btn-ghost"
                                  style={{ padding: "4px 8px", fontSize: 12 }}
                                  onClick={() => addModelOption(ag)}
                                >
                                  添加模型
                                </button>
                              </div>
                            </div>
                          </div>
                          );
                        })
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>

      {attachHostId !== undefined && (
        <AttachAgentModal
          defaultHostId={attachHostId}
          onClose={() => setAttachHostId(undefined)}
        />
      )}
    </div>
  );
}
