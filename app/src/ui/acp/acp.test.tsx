import { describe, expect, it } from "vitest";
import {
  applyAcpBootstrap,
  applyAcpEvent,
  pendingAcpApprovalsCount,
  type AppState,
} from "../../store";
import { nextQuickTheme, resolveRenderedDark } from "../../theme";
import type { AcpAgent, AcpBootstrap, AcpHost, AcpSession, AcpTurn, AcpWorkspace } from "../../types";
import { agentStatusLabel, autoInstallPackageForCmd } from "./adapterHint";

function baseState(): AppState {
  return {
    me: {
      id: "u1",
      username: "alice",
      display_name: "Alice",
      role: "user",
      last_seen_at: null,
    },
    conversations: [],
    messages: {},
    hasMore: {},
    historyLoaded: {},
    activeConv: null,
    online: {},
    typing: {},
    friends: [],
    requests: { incoming: [], outgoing: [] },
    wsReady: true,
    tab: "agents",
    replyingTo: null,
    editingMessage: null,
    reactionPending: {},
    toasts: [],
    drafts: {},
    avatars: {},
    pendingJump: null,
    acpSettings: { default_permission: "auto_read" },
    acpHosts: [],
    acpAgents: [],
    acpWorkspaces: [],
    acpSessions: [],
    acpLoaded: false,
    activeAcpHostId: null,
    activeAcpWorkspaceId: null,
    activeAcpSessionId: null,
  };
}

const sampleHost = (patch: Partial<AcpHost> = {}): AcpHost => ({
  id: "h1",
  name: "mbp-local",
  os: "macos",
  ip: "127.0.0.1",
  roots: ["/Users/alice"],
  online: true,
  latency_ms: 8,
  last_seen: "2026-11-18T00:00:00Z",
  ...patch,
});

const sampleWs = (patch: Partial<AcpWorkspace> = {}): AcpWorkspace => ({
  id: "w1",
  host_id: "h1",
  cwd: "/Users/alice/chat",
  name: "chat",
  branch: "main",
  branches: ["main", "feat/ui"],
  created_at: "2026-11-18T00:00:00Z",
  updated_at: "2026-11-18T00:00:00Z",
  ...patch,
});

const sampleSess = (patch: Partial<AcpSession> = {}): AcpSession => ({
  id: "s1",
  workspace_id: "w1",
  agent_id: "a1",
  title: "UI refactor",
  selected_model: "Claude Sonnet 4.5",
  thinking_level: "中",
  permission_policy: "auto_read",
  acp_session_key: null,
  running: false,
  diffs: [],
  rpc_logs: [],
  created_at: "2026-11-18T00:00:00Z",
  updated_at: "2026-11-18T00:00:00Z",
  turns: [],
  ...patch,
});

const sampleTurn = (patch: Partial<AcpTurn> = {}): AcpTurn => ({
  id: "t1",
  session_id: "s1",
  seq: 1,
  role: "agent_text",
  status: "done",
  payload: { text: "已完成分析" },
  created_at: "2026-11-18T00:00:00Z",
  updated_at: "2026-11-18T00:00:00Z",
  ...patch,
});

describe("ACP Workspace store reducers", () => {
  it("applyAcpBootstrap reconciles active host, workspace, and session", () => {
    const bs: AcpBootstrap = {
      settings: { default_permission: "ask" },
      hosts: [sampleHost()],
      agents: [],
      workspaces: [sampleWs()],
      sessions: [sampleSess()],
    };
    const next = applyAcpBootstrap(baseState(), bs);
    expect(next.acpLoaded).toBe(true);
    expect(next.acpSettings?.default_permission).toBe("ask");
    expect(next.activeAcpHostId).toBe("h1");
    expect(next.activeAcpWorkspaceId).toBe("w1");
    expect(next.activeAcpSessionId).toBe("s1");
  });

  it("applyAcpEvent handles host_upserted, turn_upserted, session_upserted, and host_deleted", () => {
    let st = { ...baseState(), ...applyAcpBootstrap(baseState(), {
      settings: { default_permission: "auto_read" },
      hosts: [sampleHost()],
      agents: [],
      workspaces: [sampleWs()],
      sessions: [sampleSess()],
    }) };

    // host_upserted 更新在线延迟与状态
    st = {
      ...st,
      ...applyAcpEvent(st, "host_upserted", { host: sampleHost({ latency_ms: 42 }) }),
    };
    expect(st.acpHosts[0].latency_ms).toBe(42);

    // turn_upserted 增量插入并按 seq 排序，且能数出待审批数量
    st = {
      ...st,
      ...applyAcpEvent(st, "turn_upserted", {
        session_id: "s1",
        turn: sampleTurn({ id: "t2", seq: 2, role: "permission", status: "pending" }),
      }),
    };
    st = {
      ...st,
      ...applyAcpEvent(st, "turn_upserted", {
        session_id: "s1",
        turn: sampleTurn({ id: "t1", seq: 1, role: "user", payload: { text: "hi" } }),
      }),
    };
    expect(st.acpSessions[0].turns?.map((t) => t.id)).toEqual(["t1", "t2"]);
    expect(pendingAcpApprovalsCount(st.acpSessions)).toBe(1);

    // session_upserted 保留已拉取的 turns
    st = {
      ...st,
      ...applyAcpEvent(st, "session_upserted", {
        session: sampleSess({ running: true, turns: undefined }),
      }),
    };
    expect(st.acpSessions[0].running).toBe(true);
    expect(st.acpSessions[0].turns).toHaveLength(2);

    // host_deleted 级联清除 host/workspace/session 并清空选中态
    st = {
      ...st,
      ...applyAcpEvent(st, "host_deleted", { host_id: "h1" }),
    };
    expect(st.acpHosts).toHaveLength(0);
    expect(st.acpWorkspaces).toHaveLength(0);
    expect(st.acpSessions).toHaveLength(0);
    expect(st.activeAcpHostId).toBeNull();
    expect(st.activeAcpWorkspaceId).toBeNull();
    expect(st.activeAcpSessionId).toBeNull();
  });
  it("applyAcpEvent updates agent status from installing to online", () => {
    const sampleAgent: AcpAgent = {
      id: "a1",
      host_id: "h1",
      preset: "codex",
      name: "Codex CLI",
      adapter: "codex-acp",
      cmd: "codex-acp",
      env: {},
      short_tag: "CX",
      badge_bg: "from-emerald-500 to-teal-600",
      default_model: "GPT-5 Codex",
      model_options: ["GPT-5 Codex"],
      permission_override: "inherit",
      capabilities: ["fs.read"],
      status: "installing",
      created_at: "2026-11-20T00:00:00Z",
      updated_at: "2026-11-20T00:00:00Z",
    };
    let st = {
      ...baseState(),
      ...applyAcpBootstrap(baseState(), {
        settings: { default_permission: "auto_read" },
        hosts: [sampleHost()],
        agents: [sampleAgent],
        workspaces: [sampleWs()],
        sessions: [sampleSess()],
      }),
    };
    expect(st.acpAgents[0].status).toBe("installing");
    st = {
      ...st,
      ...applyAcpEvent(st, "agent_upserted", {
        agent: { ...sampleAgent, status: "online" },
      }),
    };
    expect(st.acpAgents[0].status).toBe("online");
  });
});

describe("ACP adapter auto-install hints & status labels", () => {
  it("autoInstallPackageForCmd maps codex-acp and claude-agent-acp only", () => {
    expect(autoInstallPackageForCmd("codex-acp")).toBe("@agentclientprotocol/codex-acp");
    expect(autoInstallPackageForCmd("CODEX-ACP.cmd --help")).toBe("@agentclientprotocol/codex-acp");
    expect(autoInstallPackageForCmd("claude-agent-acp")).toBe(
      "@agentclientprotocol/claude-agent-acp",
    );
    expect(autoInstallPackageForCmd('"claude-agent-acp"')).toBe(
      "@agentclientprotocol/claude-agent-acp",
    );
    expect(autoInstallPackageForCmd("gemini --acp")).toBeNull();
    expect(autoInstallPackageForCmd("opencode acp")).toBeNull();
    expect(autoInstallPackageForCmd("pi --mode rpc")).toBeNull();
    expect(autoInstallPackageForCmd("C:\\tools\\codex-acp.cmd")).toBeNull();
    expect(autoInstallPackageForCmd("/usr/local/bin/codex-acp")).toBeNull();
    expect(autoInstallPackageForCmd("")).toBeNull();
  });

  it("agentStatusLabel formats all agent lifecycle states", () => {
    expect(agentStatusLabel("online")).toEqual({ text: "就绪", tone: "online" });
    expect(agentStatusLabel("installing")).toEqual({
      text: "正在通过 npm 安装适配器…",
      tone: "installing",
    });
    expect(agentStatusLabel("offline")).toEqual({ text: "离线", tone: "muted" });
    expect(agentStatusLabel("standby")).toEqual({ text: "待命", tone: "muted" });
  });
});

describe("Theme quick toggle helpers", () => {
  it("resolveRenderedDark & nextQuickTheme invert rendered appearance", () => {
    expect(resolveRenderedDark("system", true)).toBe(true);
    expect(resolveRenderedDark("system", false)).toBe(false);
    expect(nextQuickTheme("system", true)).toBe("light");
    expect(nextQuickTheme("system", false)).toBe("dark");
    expect(nextQuickTheme("light", true)).toBe("dark");
    expect(nextQuickTheme("dark", false)).toBe("light");
  });
});
