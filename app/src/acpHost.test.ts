// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import {
  createSessionLocalFirst,
  gitCheckoutLocalFirst,
  listDirsLocalFirst,
  mutateAgentLocalFirst,
  probeAgentModelsLocalFirst,
  sendPromptLocalFirst,
  setCloudControlEnabled,
  setLocalAcpHostId,
} from "./acpHost";
import {
  addLocalVaultAgent,
  addLocalVaultSession,
  applyLocalRunnerEvent,
  ensureLocalOnlyHost,
  resetLocalVaultForTests,
  settleLocalSessionOnCancel,
  upsertLocalVaultWorkspace,
} from "./acpLocalVault";
import { resetAcpOverlayForTests } from "./acpOverlay";
import * as api from "./api";
import { applyAcpBootstrap, getState, resetSessionState, setState } from "./store";
import type { AcpAgent, AcpHost, AcpSession, AcpWorkspace } from "./types";

const invokeMock = vi.fn();
const listenMock = vi.fn(async (..._args: unknown[]) => () => {});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

afterEach(() => {
  invokeMock.mockReset();
  listenMock.mockClear();
  setLocalAcpHostId(null);
  resetLocalVaultForTests();
  resetAcpOverlayForTests();
  setState(resetSessionState());
  vi.restoreAllMocks();
});

it("本机 @Host 浏览目录、探测模型与切换分支直接走本地 Tauri IPC，仅后台异步同步服务器", async () => {
  vi.spyOn(api, "isDesktopShell").mockReturnValue(true);
  const serverDirsSpy = vi.spyOn(api.acp, "listDirs");
  const serverProbeSpy = vi.spyOn(api.acp, "probeAgentModels");
  const serverPatchAgentSpy = vi
    .spyOn(api.acp, "updateAgent")
    .mockImplementation(async (_id, body) => ({
      ...localAgent,
      default_model: body.default_model ?? localAgent.default_model,
      model_options: body.model_options ?? localAgent.model_options,
    }));
  const serverPatchSessionSpy = vi
    .spyOn(api.acp, "updateSession")
    .mockImplementation(async (_id, body) => ({
      ...freshSession,
      selected_model: body.selected_model ?? freshSession.selected_model,
    }));
  const serverCheckoutSpy = vi
    .spyOn(api.acp, "gitCheckout")
    .mockImplementation(async (_id, branch, extra) => ({
      ...localWs,
      branch,
      branches: extra?.branches ?? localWs.branches,
    }));

  setLocalAcpHostId("host-local");

  const localAgent: AcpAgent = {
    id: "ag-local",
    host_id: "host-local",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "GPT-5 Codex",
    model_options: ["GPT-5 Codex", "o3-pro", "GPT-4.1"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };
  const localWs: AcpWorkspace = {
    id: "ws-local",
    host_id: "host-local",
    cwd: "C:\\work\\chat",
    name: "chat",
    branch: "main",
    branches: ["main", "feat/local"],
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };
  const freshSession: AcpSession = {
    id: "sess-local",
    workspace_id: "ws-local",
    agent_id: "ag-local",
    title: "本地新对话",
    selected_model: "GPT-5 Codex",
    thinking_level: "中",
    permission_policy: "auto_read",
    acp_session_key: null,
    running: false,
    diffs: [],
    rpc_logs: [],
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
    turns: [],
  };

  setState({
    acpLoaded: true,
    acpAgents: [localAgent],
    acpWorkspaces: [localWs],
    acpSessions: [freshSession],
  });

  // 1. 本机目录浏览直接走 acp_host_list_dir，不请求服务器
  invokeMock.mockResolvedValueOnce({
    path: "C:\\work",
    parent: "C:\\",
    roots: ["C:\\work"],
    entries: [{ name: "chat", path: "C:\\work\\chat", has_children: true }],
  });
  const listing = await listDirsLocalFirst("host-local", "C:\\work");
  expect(listing.entries[0].name).toBe("chat");
  expect(invokeMock).toHaveBeenCalledWith("acp_host_list_dir", { path: "C:\\work" });
  expect(serverDirsSpy).not.toHaveBeenCalled();

  // 2. 本机模型探测直接走 acp_host_probe_agent，不调服务器 /probe-models，完成后异步 PATCH 服务器
  invokeMock.mockResolvedValueOnce({
    ok: true,
    status: "online",
    default_model: "xy-openai/gpt-6.2-astra",
    model_options: ["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"],
  });
  const updatedAg = await probeAgentModelsLocalFirst(localAgent, {
    sessionId: "sess-local",
    cwd: "C:\\work\\chat",
  });
  expect(updatedAg.default_model).toBe("xy-openai/gpt-6.2-astra");
  expect(getState().acpAgents[0].model_options).toEqual([
    "xy-open/glm-5.3",
    "xy-openai/gpt-6.2-astra",
  ]);
  expect(getState().acpSessions[0].selected_model).toBe("xy-openai/gpt-6.2-astra");
  expect(serverProbeSpy).not.toHaveBeenCalled();
  expect(serverPatchAgentSpy).toHaveBeenCalledWith("ag-local", {
    default_model: "xy-openai/gpt-6.2-astra",
    model_options: ["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"],
  });
  expect(serverPatchSessionSpy).toHaveBeenCalledWith("sess-local", {
    selected_model: "xy-openai/gpt-6.2-astra",
  });

  // 3. 本机 Git 分支切换直接调 acp_host_git_checkout，后台带 local_probed 异步回写服务器
  invokeMock.mockResolvedValueOnce({
    branch: "feat/local",
    branches: ["main", "feat/local"],
  });
  const coWs = await gitCheckoutLocalFirst(localWs, "feat/local");
  expect(coWs.branch).toBe("feat/local");
  expect(getState().acpWorkspaces[0].branch).toBe("feat/local");
  expect(serverCheckoutSpy).toHaveBeenCalledWith("ws-local", "feat/local", {
    local_probed: true,
    branches: ["main", "feat/local"],
  });
});

it("关闭「本机允许被云端控制」后立即删除云端主机全部数据，本机仍可纯本地使用且不再同步云端", async () => {
  vi.spyOn(api, "isDesktopShell").mockReturnValue(true);
  const deleteHostSpy = vi.spyOn(api.acp, "deleteHost").mockResolvedValue(undefined);
  const serverUpdateAgentSpy = vi.spyOn(api.acp, "updateAgent");
  const serverCreateSessSpy = vi.spyOn(api.acp, "createSession");
  const serverSendPromptSpy = vi.spyOn(api.acp, "sendPrompt");

  const cloudHost: AcpHost = {
    id: "cloud-host-1",
    name: "14X",
    os: "windows · x86_64",
    ip: "Client-Daemon",
    roots: ["C:\\work"],
    online: true,
    latency_ms: 680,
    last_seen: "2026-11-20T00:00:00Z",
  };
  const agent: AcpAgent = {
    id: "ag-1",
    host_id: "cloud-host-1",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "xy-openai/gpt-6.2-astra",
    model_options: ["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };
  const ws: AcpWorkspace = {
    id: "ws-1",
    host_id: "cloud-host-1",
    cwd: "C:\\work\\chat",
    name: "chat",
    branch: "main",
    branches: ["main"],
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };

  setLocalAcpHostId("cloud-host-1");
  setState({
    acpLoaded: true,
    acpHosts: [cloudHost],
    acpAgents: [agent],
    acpWorkspaces: [ws],
    acpSessions: [],
    activeAcpHostId: "cloud-host-1",
  });

  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "acp_host_info") {
      return {
        host_id: "cloud-host-1",
        install_id: "install-uuid-1",
        name: "14X",
        os: "windows · x86_64",
        roots: ["C:\\work"],
      };
    }
    if (cmd === "acp_host_set_cloud_control") {
      return {
        allow_all_local: false,
        cloud_control_enabled: false,
        approved_count: 0,
      };
    }
    if (cmd === "acp_host_probe_agent") {
      return {
        ok: true,
        status: "online",
        default_model: "xy-openai/gpt-6.2-astra",
        model_options: ["xy-open/glm-5.3", "xy-openai/gpt-6.2-astra"],
      };
    }
    if (cmd === "acp_host_local_prompt_start") {
      return null;
    }
    return null;
  });

  // 1. 关闭云端控制：断开并调用 deleteHost 从云端擦除全部数据，本机节点转为 Local-Only 保留可用
  const policy = await setCloudControlEnabled(false);
  expect(policy.cloud_control_enabled).toBe(false);
  expect(deleteHostSpy).toHaveBeenCalledWith("cloud-host-1");
  expect(getState().acpHosts).toHaveLength(1);
  expect(getState().acpHosts[0].ip).toBe("Local-Only");
  expect(getState().acpHosts[0].id).toBe("install-uuid-1");
  expect(getState().acpAgents[0].host_id).toBe("install-uuid-1");

  // 2. 云端再次 bootstrap 返回空列表（因为云端已删光），本机 Vault 数据依然完整存在
  setState((cur) =>
    applyAcpBootstrap(cur, {
      settings: { default_permission: "auto_read" },
      hosts: [],
      agents: [],
      workspaces: [],
      sessions: [],
    }),
  );
  expect(getState().acpHosts).toHaveLength(1);
  expect(getState().acpAgents).toHaveLength(1);
  expect(getState().acpWorkspaces).toHaveLength(1);

  // 3. 在纯本地模式下删改模型、新建对话、发送指令均零云端请求
  mutateAgentLocalFirst("ag-1", {
    model_options: ["xy-openai/gpt-6.2-astra"],
    default_model: "xy-openai/gpt-6.2-astra",
  });
  expect(serverUpdateAgentSpy).not.toHaveBeenCalled();

  const localWs = getState().acpWorkspaces[0];
  const sess = await createSessionLocalFirst({
    workspace: localWs,
    agent_id: "ag-1",
    title: "纯本地调试",
  });
  expect(serverCreateSessSpy).not.toHaveBeenCalled();

  const res = await sendPromptLocalFirst(sess, localWs, getState().acpAgents[0], {
    text: "hello local",
    files: [],
    attachments: [],
    model: "xy-openai/gpt-6.2-astra",
    thinking_level: "中",
    permission_policy: "auto_read",
  });
  expect(res.localHandled).toBe(true);
  expect(serverSendPromptSpy).not.toHaveBeenCalled();
  expect(invokeMock).toHaveBeenCalledWith(
    "acp_host_local_prompt_start",
    expect.objectContaining({
      msg: expect.objectContaining({
        session_id: sess.id,
        text: "hello local",
      }),
    }),
  );
});

it("纯本地 Vault 对等支持思考流、tool_call 增量合并与 Diff、非落盘 activity 以及 prompt_done/取消收敛", () => {
  ensureLocalOnlyHost({ install_id: "h-loc", name: "mac", os: "macos", roots: ["/repo"] });
  const ag = addLocalVaultAgent({
    preset: "pi",
    name: "Pi",
    cmd: "pi --mode rpc",
    env: {},
    status: "online",
  });
  const ws = upsertLocalVaultWorkspace({
    cwd: "/repo",
    name: "repo",
    branch: "main",
    branches: ["main"],
  });
  const sess = addLocalVaultSession({
    workspace: ws,
    agent: ag,
    defaultPermission: "auto_read",
  });

  const setItemSpy = vi.spyOn(localStorage, "setItem");
  setItemSpy.mockClear();

  // 1. thought_delta 中间态不触发 localStorage 写盘；done=true 后落盘
  const t1 = applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "thought_delta",
    turn_key: "tk-1",
    delta: "Step 1 ",
    done: false,
  });
  expect(t1.session?.turns?.[0].role).toBe("agent_thought");
  expect(t1.session?.turns?.[0].status).toBe("streaming");
  expect(setItemSpy).not.toHaveBeenCalled();

  const t2 = applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "thought_delta",
    turn_key: "tk-1",
    delta: "done",
    done: true,
    duration_ms: 320,
  });
  expect(t2.session?.turns?.[0].status).toBe("done");
  expect(t2.session?.turns?.[0].payload.text).toBe("Step 1 done");
  expect(t2.session?.turns?.[0].payload.duration_ms).toBe(320);
  expect(setItemSpy).toHaveBeenCalledTimes(1);
  setItemSpy.mockClear();

  // 2. tool_call 增量合并不覆盖已累计输出，且中间态不落盘
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-1",
    name: "edit",
    tool_kind: "edit",
    title: "edit: src/a.ts",
    status: "pending",
    locations: ["src/a.ts"],
  });
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-1",
    status: "in_progress",
    output_delta: "chunk1\n",
  });
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-1",
    output_delta: "chunk2",
    diffs: [{ path: "src/a.ts", old_text: "a", new_text: "b" }],
  });
  expect(setItemSpy).not.toHaveBeenCalled();

  const tcDone = applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-1",
    status: "completed",
    output: "", // 空字符串不应冲掉已有 output
    duration_ms: 45,
  });
  const toolTurn = tcDone.session?.turns?.find((t) => t.role === "tool_call");
  expect(toolTurn?.status).toBe("completed");
  expect(toolTurn?.payload.output).toBe("chunk1\nchunk2");
  expect(toolTurn?.payload.duration_ms).toBe(45);
  expect((toolTurn?.payload.diffs as Array<{ path: string }>)[0].path).toBe("src/a.ts");
  expect(setItemSpy).toHaveBeenCalledTimes(1);
  setItemSpy.mockClear();

  // 3. activity 仅作短暂返回，不写入 Vault 或 localStorage
  const actDelta = applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "activity",
    phase: "tooling",
    detail: "执行测试…",
    usage: { input_tokens: 10, output_tokens: 20, total_tokens: 30 },
  });
  expect(actDelta.session).toBeUndefined();
  expect(actDelta.activity?.phase).toBe("tooling");
  expect(actDelta.activity?.usage?.total_tokens).toBe(30);
  expect(setItemSpy).not.toHaveBeenCalled();

  // 4. prompt_done 自动收敛残留 streaming 思考/正文与 in_progress 工具，并发送 idle
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "thought_delta",
    turn_key: "tk-stale",
    delta: "partial",
    done: false,
  });
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-stale",
    name: "bash",
    status: "in_progress",
  });
  const doneDelta = applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "prompt_done",
    acp_session_key: "pi-k1",
  });
  expect(doneDelta.session?.running).toBe(false);
  expect(doneDelta.activity?.phase).toBe("idle");
  expect(doneDelta.session?.turns?.find((t) => t.payload.turn_key === "tk-stale")?.status).toBe(
    "done",
  );
  expect(
    doneDelta.session?.turns?.find((t) => t.payload.tool_call_id === "tc-stale")?.status,
  ).toBe("completed");

  // 5. settleLocalSessionOnCancel 收敛流式正文/思考、工具与待审批权限
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "permission_request",
    req_id: "pr-cancel",
    tool_kind: "bash",
    command: "rm -rf build",
    reason: "clean",
  });
  applyLocalRunnerEvent({
    type: "session_update",
    session_id: sess.id,
    kind: "tool_call",
    tool_call_id: "tc-cancel",
    name: "bash",
    status: "in_progress",
  });
  const cancelled = settleLocalSessionOnCancel(sess.id);
  expect(cancelled?.running).toBe(false);
  expect(
    cancelled?.turns?.find((t) => t.payload.req_id === "pr-cancel")?.status,
  ).toBe("rejected");
  expect(
    cancelled?.turns?.find((t) => t.payload.tool_call_id === "tc-cancel")?.status,
  ).toBe("failed");
});

it("纯本地发送指令时若 acp_host_local_prompt_start 抛错，自动回滚 running 与 idle 活动态", async () => {
  vi.spyOn(api, "isDesktopShell").mockReturnValue(true);
  ensureLocalOnlyHost({ install_id: "h-fail", name: "win", os: "windows", roots: ["C:\\repo"] });
  const ag = addLocalVaultAgent({
    preset: "codex",
    name: "Codex",
    cmd: "codex-acp",
    env: {},
    status: "online",
  });
  const ws = upsertLocalVaultWorkspace({
    cwd: "C:\\repo",
    name: "repo",
    branch: "main",
    branches: ["main"],
  });
  const sess = addLocalVaultSession({
    workspace: ws,
    agent: ag,
    defaultPermission: "auto_read",
  });
  setState({
    acpLoaded: true,
    acpAgents: [ag],
    acpWorkspaces: [ws],
    acpSessions: [sess],
  });

  invokeMock.mockRejectedValueOnce(new Error("spawn failed"));

  await expect(
    sendPromptLocalFirst(sess, ws, ag, {
      text: "hello",
      files: [],
      attachments: [],
      model: ag.default_model,
      thinking_level: "中",
      permission_policy: "auto_read",
    }),
  ).rejects.toThrow("spawn failed");

  expect(getState().acpSessions.find((s) => s.id === sess.id)?.running).toBe(false);
  expect(getState().acpActivity[sess.id]).toBeUndefined();
});

