import { useEffect, useState } from "react";
import { acp } from "../../api";
import { applyAcpBootstrap, setState, useApp } from "../../store";
import type { AcpPermissionPolicy } from "../../types";
import { AttachAgentModal } from "../acp/AcpModals";
import { ChevronDownIcon, ChevronRightIcon, CpuIcon, PlusIcon, TrashIcon, XIcon } from "../icons";
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
  const [expandedHosts, setExpandedHosts] = useState<Set<string>>(() => new Set());
  const [customInputs, setCustomInputs] = useState<Record<string, string>>({});
  const [renamingHostId, setRenamingHostId] = useState<string | null>(null);
  const [hostNameDraft, setHostNameDraft] = useState("");
  const [attachHostId, setAttachHostId] = useState<string | null | undefined>(undefined);
  const [err, setErr] = useState("");

  useEffect(() => {
    if (!s.acpLoaded) {
      void acp
        .bootstrap()
        .then((bs) => setState((cur) => applyAcpBootstrap(cur, bs)))
        .catch(() => {});
    }
  }, [s.acpLoaded]);

  useEffect(() => {
    if (s.acpHosts.length > 0 && expandedHosts.size === 0) {
      setExpandedHosts(new Set(s.acpHosts.map((h) => h.id)));
    }
  }, [s.acpHosts, expandedHosts.size]);

  const setPolicy = async (policy: AcpPermissionPolicy) => {
    setErr("");
    try {
      const settings = await acp.updateSettings(policy);
      setState({ acpSettings: settings });
    } catch (e) {
      setErr(e instanceof Error ? e.message : "保存权限策略失败");
    }
  };

  const toggleHost = (id: string) => {
    setExpandedHosts((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const addModelOption = async (agentId: string, curOptions: string[]) => {
    const val = (customInputs[agentId] ?? "").trim();
    if (!val || curOptions.includes(val)) return;
    setErr("");
    try {
      const updated = await acp.updateAgent(agentId, {
        model_options: [...curOptions, val],
      });
      setCustomInputs((m) => ({ ...m, [agentId]: "" }));
      setState((cur) => ({
        acpAgents: cur.acpAgents.map((a) => (a.id === updated.id ? updated : a)),
      }));
    } catch (e) {
      setErr(e instanceof Error ? e.message : "添加模型失败");
    }
  };

  const removeModelOption = async (agentId: string, curOptions: string[], target: string) => {
    const next = curOptions.filter((m) => m !== target);
    if (next.length === 0) return;
    setErr("");
    try {
      const updated = await acp.updateAgent(agentId, { model_options: next });
      setState((cur) => ({
        acpAgents: cur.acpAgents.map((a) => (a.id === updated.id ? updated : a)),
      }));
    } catch (e) {
      setErr(e instanceof Error ? e.message : "移除模型失败");
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
      await acp.deleteAgent(agentId);
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
              const open = expandedHosts.has(h.id);
              const agents = s.acpAgents.filter((a) => a.host_id === h.id);
              return (
                <div key={h.id} className="host-group-card">
                  <div className="host-group-head" onClick={() => toggleHost(h.id)}>
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
                            onClick={() =>
                              void acp.renameHost(h.id, hostNameDraft.trim()).then((uh) => {
                                setRenamingHostId(null);
                                setState((cur) => ({
                                  acpHosts: cur.acpHosts.map((x) => (x.id === uh.id ? uh : x)),
                                }));
                              })
                            }
                          >
                            保存
                          </button>
                        </div>
                      ) : (
                        <span>@{h.name}</span>
                      )}
                      <span className="acp-chip">{h.os}</span>
                      <span className="acp-chip">
                        {h.online ? `${h.latency_ms}ms` : "离线"}
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
                      <button
                        type="button"
                        className="icon-btn"
                        title="解绑节点"
                        onClick={() => void removeHost(h.id, h.name)}
                      >
                        <TrashIcon size={14} />
                      </button>
                    </div>
                  </div>

                  {open && (
                    <div className="host-group-body">
                      {agents.length === 0 ? (
                        <div className="muted" style={{ paddingTop: 10, fontSize: 12.5 }}>
                          该机器上尚未注册 Agent。
                        </div>
                      ) : (
                        agents.map((ag) => (
                          <div key={ag.id} className="agent-item-card">
                            <div className="row between">
                              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                                <span className="acp-chip">{ag.short_tag}</span>
                                <b>{ag.name}</b>
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
                                  onChange={(e) =>
                                    void acp
                                      .updateAgent(ag.id, { default_model: e.target.value })
                                      .then((u) =>
                                        setState((cur) => ({
                                          acpAgents: cur.acpAgents.map((a) =>
                                            a.id === u.id ? u : a,
                                          ),
                                        })),
                                      )
                                  }
                                >
                                  {ag.model_options.map((m) => (
                                    <option key={m} value={m}>
                                      {m}
                                    </option>
                                  ))}
                                </select>
                              </label>

                              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12.5 }}>
                                <span className="muted">权限策略</span>
                                <select
                                  className="acp-select"
                                  value={ag.permission_override}
                                  onChange={(e) =>
                                    void acp
                                      .updateAgent(ag.id, {
                                        permission_override: e.target.value as
                                          | "inherit"
                                          | AcpPermissionPolicy,
                                      })
                                      .then((u) =>
                                        setState((cur) => ({
                                          acpAgents: cur.acpAgents.map((a) =>
                                            a.id === u.id ? u : a,
                                          ),
                                        })),
                                      )
                                  }
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
                                      onClick={() =>
                                        void removeModelOption(ag.id, ag.model_options, m)
                                      }
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
                                    e.key === "Enter" &&
                                    void addModelOption(ag.id, ag.model_options)
                                  }
                                />
                                <button
                                  type="button"
                                  className="btn-ghost"
                                  style={{ padding: "4px 8px", fontSize: 12 }}
                                  onClick={() => void addModelOption(ag.id, ag.model_options)}
                                >
                                  添加模型
                                </button>
                              </div>
                            </div>
                          </div>
                        ))
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
