// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { acp } from "../../api";
import { resetSessionState, setState } from "../../store";
import type { AcpAgent, AcpHost, AcpSession, AcpWorkspace } from "../../types";
import AcpWorkspaceView from "./AcpWorkspaceView";

const sampleHost: AcpHost = {
  id: "h1",
  name: "14X",
  os: "windows · x86_64",
  ip: "127.0.0.1",
  roots: ["C:\\Users\\xie_f\\projects"],
  online: true,
  latency_ms: 42,
  last_seen: "2026-11-20T00:00:00Z",
};

const sampleWorkspace: AcpWorkspace = {
  id: "w1",
  host_id: "h1",
  cwd: "C:\\Users\\xie_f\\projects\\other\\decision_model",
  name: "decision_model",
  branch: "main",
  branches: ["main"],
  created_at: "2026-11-20T00:00:00Z",
  updated_at: "2026-11-20T00:00:00Z",
};

it("工作目录卡片隐藏完整路径并通过三点菜单触发删除", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [sampleHost],
        acpAgents: [],
        acpWorkspaces: [sampleWorkspace],
        acpSessions: [],
        activeAcpHostId: sampleHost.id,
        activeAcpWorkspaceId: sampleWorkspace.id,
        activeAcpSessionId: null,
      });
      root.render(<AcpWorkspaceView />);
    });

    const card = container.querySelector(".acp-ws-card") as HTMLElement;
    expect(card).not.toBeNull();
    expect(container.querySelector(".acp-ws-cwd")).toBeNull();
    expect(card.textContent).toContain("decision_model");
    expect(card.textContent).not.toContain("C:\\Users\\xie_f");

    const wsName = card.querySelector(".acp-ws-name") as HTMLElement;
    expect(wsName.getAttribute("title")).toContain(sampleWorkspace.cwd);

    // 删除按钮默认收纳在三点菜单内
    expect(card.textContent).not.toContain("删除工作目录");
    expect(container.querySelector(".acp-ws-sessions")).not.toBeNull();

    const menuBtn = card.querySelector(
      'button[aria-label="decision_model 更多操作"]',
    ) as HTMLButtonElement;
    expect(menuBtn).not.toBeNull();
    expect(menuBtn.getAttribute("aria-expanded")).toBe("false");

    // 点击三点按钮展开菜单，同时不触发展开/折叠切换
    act(() => {
      menuBtn.click();
    });
    expect(menuBtn.getAttribute("aria-expanded")).toBe("true");
    expect(card.textContent).toContain("删除工作目录");
    expect(container.querySelector(".acp-ws-sessions")).not.toBeNull();

    // 按 Escape 关闭菜单
    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    expect(menuBtn.getAttribute("aria-expanded")).toBe("false");
    expect(card.textContent).not.toContain("删除工作目录");

    // 再次打开后点击外部区域关闭菜单
    act(() => {
      menuBtn.click();
    });
    expect(menuBtn.getAttribute("aria-expanded")).toBe("true");
    expect(card.textContent).toContain("删除工作目录");

    act(() => {
      document.body.dispatchEvent(
        new Event("pointerdown", { bubbles: true }),
      );
    });
    expect(menuBtn.getAttribute("aria-expanded")).toBe("false");
    expect(card.textContent).not.toContain("删除工作目录");

    // 点击新建对话加号打开弹窗且不折叠目录
    const addBtn = card.querySelector(
      'button[aria-label="在此目录新建对话"]',
    ) as HTMLButtonElement;
    expect(addBtn).not.toBeNull();
    act(() => {
      addBtn.click();
    });
    expect(
      container.querySelector('[role="dialog"][aria-label="新建 Agent 对话"]'),
    ).not.toBeNull();
    expect(container.querySelector(".acp-ws-sessions")).not.toBeNull();
  } finally {
    act(() => root.unmount());
    container.remove();
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});

it("打开尚未开始对话的新会话时异步触发 Agent 模型探测且不阻塞渲染", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const probeSpy = vi.spyOn(acp, "probeAgentModels").mockResolvedValue({
    id: "a1",
    host_id: "h1",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "xy-openai/gpt-6.2-astra",
    model_options: ["xy-openai/gpt-6.2-astra"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  });

  const agent: AcpAgent = {
    id: "a1",
    host_id: "h1",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "Default Model",
    model_options: ["Default Model"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };
  const freshSession: AcpSession = {
    id: "s1",
    workspace_id: "w1",
    agent_id: "a1",
    title: "新对话",
    selected_model: "Default Model",
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

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [sampleHost],
        acpAgents: [agent],
        acpWorkspaces: [sampleWorkspace],
        acpSessions: [freshSession],
        activeAcpHostId: sampleHost.id,
        activeAcpWorkspaceId: sampleWorkspace.id,
        activeAcpSessionId: freshSession.id,
      });
      root.render(<AcpWorkspaceView />);
    });

    expect(container.textContent).toContain("对话已就绪");
    expect(probeSpy).toHaveBeenCalledTimes(1);
    expect(probeSpy).toHaveBeenCalledWith("a1", {
      session_id: "s1",
      wait: false,
    });
  } finally {
    probeSpy.mockRestore();
    act(() => root.unmount());
    container.remove();
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});

it("渲染思考过程折叠块、工具调用卡片与 diffs、实时状态栏、RPC 日志抽屉与本地化权限卡", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });

  const agent: AcpAgent = {
    id: "a1",
    host_id: "h1",
    preset: "pi",
    name: "Pi Agent",
    adapter: "pi-rpc",
    cmd: "pi --mode rpc",
    env: {},
    short_tag: "PI",
    badge_bg: "from-indigo-500 to-violet-600",
    default_model: "default",
    model_options: ["default"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };

  const buildSession = (thoughtStatus: "streaming" | "done"): AcpSession => ({
    id: "s-rich",
    workspace_id: "w1",
    agent_id: "a1",
    title: "深度重构任务",
    selected_model: "default",
    thinking_level: "高",
    permission_policy: "auto_read",
    acp_session_key: "pi-rich",
    running: thoughtStatus === "streaming",
    diffs: [
      {
        file: "src/app.ts",
        added: 12,
        removed: 3,
        status: "modified",
        diff_lines: ["+ const ok = true;", "- const ok = false;"],
      },
    ],
    rpc_logs: [
      { dir: "out", method: "session/prompt", summary: "blocks: 1" },
      { dir: "in", method: "tool_call", summary: "edit src/app.ts" },
    ],
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
    turns: [
      {
        id: "t-thought",
        session_id: "s-rich",
        seq: 1,
        role: "agent_thought",
        status: thoughtStatus,
        payload: {
          turn_key: "th-1",
          text: "First check existing tests",
          ...(thoughtStatus === "done" ? { duration_ms: 1500 } : {}),
        },
        created_at: "2026-11-20T00:00:00Z",
        updated_at: "2026-11-20T00:00:01Z",
      },
      {
        id: "t-tool-edit",
        session_id: "s-rich",
        seq: 2,
        role: "tool_call",
        status: "completed",
        payload: {
          tool_call_id: "tc-1",
          name: "edit",
          kind: "edit",
          title: "edit: src/app.ts",
          locations: ["src/app.ts"],
          input: '{"path":"src/app.ts"}',
          output: "applied cleanly",
          diffs: [{ path: "src/app.ts", old_text: "oldLine", new_text: "newLine" }],
          duration_ms: 85,
          is_error: false,
        },
        created_at: "2026-11-20T00:00:01Z",
        updated_at: "2026-11-20T00:00:02Z",
      },
      {
        id: "t-tool-fail",
        session_id: "s-rich",
        seq: 3,
        role: "tool_call",
        status: "failed",
        payload: {
          tool_call_id: "tc-2",
          name: "bash",
          kind: "execute",
          title: "bash: cargo test",
          output: "assertion failed",
          is_error: true,
        },
        created_at: "2026-11-20T00:00:02Z",
        updated_at: "2026-11-20T00:00:03Z",
      },
      {
        id: "t-perm",
        session_id: "s-rich",
        seq: 4,
        role: "permission",
        status: "approved",
        payload: {
          req_id: "r1",
          tool_kind: "terminal/run",
          command: "npm test",
          cwd: "/repo/app",
          reason: "执行测试套件",
          chosen_option: "allow_once",
        },
        created_at: "2026-11-20T00:00:03Z",
        updated_at: "2026-11-20T00:00:04Z",
      },
      {
        id: "t-text",
        session_id: "s-rich",
        seq: 5,
        role: "agent_text",
        status: "done",
        payload: { text: "**已完成** `src/app.ts` 修改。" },
        created_at: "2026-11-20T00:00:04Z",
        updated_at: "2026-11-20T00:00:05Z",
      },
    ],
  });

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [sampleHost],
        acpAgents: [agent],
        acpWorkspaces: [sampleWorkspace],
        acpSessions: [buildSession("streaming")],
        acpActivity: {
          "s-rich": {
            phase: "thinking",
            detail: "正在分析测试结构…",
            startedAt: Date.now() - 2000,
            updatedAt: Date.now(),
            usage: { input_tokens: 1200, output_tokens: 300, total_tokens: 1500 },
          },
        },
        activeAcpHostId: sampleHost.id,
        activeAcpWorkspaceId: sampleWorkspace.id,
        activeAcpSessionId: "s-rich",
      });
      root.render(<AcpWorkspaceView />);
    });

    // 1. 一级、二级过程卡默认全部折叠为单行；底部状态栏与头部 Token 用量可见
    const groupDetails = container.querySelector(".acp-process-group") as HTMLDetailsElement;
    const thoughtDetails = container.querySelector(".acp-thought-card") as HTMLDetailsElement;
    const toolCards = container.querySelectorAll(".acp-tool-card");
    const permResolvedDetails = container.querySelector(
      ".acp-perm-resolved-card",
    ) as HTMLDetailsElement;
    expect(groupDetails).not.toBeNull();
    expect(thoughtDetails).not.toBeNull();
    expect(toolCards).toHaveLength(2);
    expect(permResolvedDetails).not.toBeNull();
    // 默认均未展开（包括流式思考、带 diff 的工具、失败工具与已处理授权）
    expect(groupDetails.open).toBe(false);
    expect(thoughtDetails.open).toBe(false);
    expect((toolCards[0] as HTMLDetailsElement).open).toBe(false);
    expect((toolCards[1] as HTMLDetailsElement).open).toBe(false);
    expect(permResolvedDetails.open).toBe(false);
    expect(groupDetails.querySelector("summary")?.textContent).toContain("思考 1 次");
    expect(groupDetails.querySelector("summary")?.textContent).toContain("工具 2 次");
    // 无 heading 在 summary 内（accessible accordion 规范）
    expect(container.querySelectorAll("summary h1, summary h2, summary h3, summary h4")).toHaveLength(
      0,
    );

    const activityBar = container.querySelector(".acp-activity-bar") as HTMLElement;
    expect(activityBar).not.toBeNull();
    expect(activityBar.textContent).toContain("深度思考");
    expect(activityBar.textContent).toContain("正在分析测试结构…");
    expect(container.querySelector(".acp-header-meta")?.textContent).toContain("1.5k tok");

    // 2. 点击二级外层 summary 仅展开外层；内部各一级思考/工具卡依然保持单行折叠
    const groupSummary = groupDetails.querySelector(":scope > summary") as HTMLElement;
    act(() => {
      groupSummary.click();
    });
    expect(groupDetails.open).toBe(true);
    expect(thoughtDetails.open).toBe(false);
    expect((toolCards[0] as HTMLDetailsElement).open).toBe(false);
    expect((toolCards[1] as HTMLDetailsElement).open).toBe(false);
    expect(permResolvedDetails.open).toBe(false);

    // 3. 点击一级思考卡 summary 展开并复制推理内容
    act(() => {
      (thoughtDetails.querySelector(":scope > summary") as HTMLElement).click();
    });
    expect(thoughtDetails.open).toBe(true);
    expect(thoughtDetails.textContent).toContain("正在思考…");
    expect(thoughtDetails.textContent).toContain("First check existing tests");
    const copyThoughtBtn = thoughtDetails.querySelector(
      'button[aria-label="复制思考过程"]',
    ) as HTMLButtonElement;
    act(() => {
      copyThoughtBtn.click();
    });
    expect(writeText).toHaveBeenCalledWith("First check existing tests");
    expect(thoughtDetails.open).toBe(true);

    // 4. 点击工具调用 #1 与已处理授权卡展开各自详情；工具 #2 保持折叠且带失败标记
    act(() => {
      (toolCards[0].querySelector(":scope > summary") as HTMLElement).click();
      (permResolvedDetails.querySelector(":scope > summary") as HTMLElement).click();
    });
    expect((toolCards[0] as HTMLDetailsElement).open).toBe(true);
    expect((toolCards[1] as HTMLDetailsElement).open).toBe(false);
    expect(toolCards[0].textContent).toContain("编辑");
    expect(toolCards[0].textContent).toContain("85ms");
    expect(toolCards[0].querySelector(".acp-diff-line.is-del")?.textContent).toContain("oldLine");
    expect(toolCards[0].querySelector(".acp-diff-line.is-add")?.textContent).toContain("newLine");
    expect(toolCards[1].textContent).toContain("失败");
    expect(toolCards[1].textContent).toContain("执行");

    expect(permResolvedDetails.open).toBe(true);
    expect(permResolvedDetails.textContent).toContain("终端命令");
    expect(permResolvedDetails.textContent).toContain("/repo/app");
    expect(permResolvedDetails.textContent).toContain("已允许（仅本次）");

    // 5. RPC 日志抽屉可切换展开；Markdown 正文渲染加粗与行内代码
    const rpcToggle = container.querySelector('button[title="查看 RPC 通信日志"]') as HTMLButtonElement;
    expect(rpcToggle.textContent).toContain("RPC (2)");
    act(() => {
      rpcToggle.click();
    });
    expect(container.querySelector(".acp-rpc-drawer")?.textContent).toContain("session/prompt");
    expect(container.querySelector(".acp-md strong")?.textContent).toBe("已完成");
    expect(container.querySelector(".acp-md-inline-code")?.textContent).toBe("src/app.ts");

    // 6. 思考变为 done 且会话退出运行后：统计徽章与耗时更新，底部状态栏隐藏，外/内展开状态稳定
    act(() => {
      setState((cur) => ({
        acpSessions: [buildSession("done")],
        acpActivity: {
          ...cur.acpActivity,
          "s-rich": {
            phase: "idle",
            detail: "",
            startedAt: Date.now() - 2000,
            updatedAt: Date.now(),
            usage: { total_tokens: 1500 },
          },
        },
      }));
    });
    expect(container.querySelector(".acp-thought-card")?.textContent).toContain("1.5s");
    expect(container.querySelector(".acp-activity-bar")).toBeNull();

    // 7. 向上滚动远离底部时出现回到底部按钮，点击后恢复吸底
    const turnsEl = container.querySelector(".acp-turns") as HTMLDivElement;
    Object.defineProperty(turnsEl, "scrollHeight", { value: 1000, configurable: true });
    Object.defineProperty(turnsEl, "clientHeight", { value: 400, configurable: true });
    turnsEl.scrollTop = 100;
    act(() => {
      turnsEl.dispatchEvent(new Event("scroll"));
    });
    const scrollDownBtn = container.querySelector(".acp-scroll-down-btn") as HTMLButtonElement;
    expect(scrollDownBtn).not.toBeNull();
    act(() => {
      scrollDownBtn.click();
    });
    expect(turnsEl.scrollTop).toBe(1000);
    expect(container.querySelector(".acp-scroll-down-btn")).toBeNull();
  } finally {
    act(() => root.unmount());
    container.remove();
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});

it("两次正式回答之间与待确认授权隔离分组成各自默认折叠的二级行", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const agent: AcpAgent = {
    id: "a1",
    host_id: "h1",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "default",
    model_options: ["default"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };
  const sess: AcpSession = {
    id: "s-multi",
    workspace_id: "w1",
    agent_id: "a1",
    title: "多轮问答",
    selected_model: "default",
    thinking_level: "中",
    permission_policy: "ask",
    acp_session_key: null,
    running: true,
    diffs: [],
    rpc_logs: [],
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
    turns: [
      {
        id: "t1",
        session_id: "s-multi",
        seq: 1,
        role: "agent_thought",
        status: "done",
        payload: { text: "t1" },
        created_at: "2026-11-20T00:00:00Z",
        updated_at: "2026-11-20T00:00:00Z",
      },
      {
        id: "t2",
        session_id: "s-multi",
        seq: 2,
        role: "agent_text",
        status: "done",
        payload: { text: "第一次正式回答" },
        created_at: "2026-11-20T00:00:01Z",
        updated_at: "2026-11-20T00:00:01Z",
      },
      {
        id: "t3",
        session_id: "s-multi",
        seq: 3,
        role: "tool_call",
        status: "completed",
        payload: { tool_call_id: "c1", kind: "read", title: "read a.ts" },
        created_at: "2026-11-20T00:00:02Z",
        updated_at: "2026-11-20T00:00:02Z",
      },
      {
        id: "t4",
        session_id: "s-multi",
        seq: 4,
        role: "agent_thought",
        status: "done",
        payload: { text: "t4" },
        created_at: "2026-11-20T00:00:03Z",
        updated_at: "2026-11-20T00:00:03Z",
      },
      {
        id: "t5",
        session_id: "s-multi",
        seq: 5,
        role: "permission",
        status: "pending",
        payload: { tool_kind: "bash", command: "npm test", reason: "运行测试" },
        created_at: "2026-11-20T00:00:04Z",
        updated_at: "2026-11-20T00:00:04Z",
      },
      {
        id: "t6",
        session_id: "s-multi",
        seq: 6,
        role: "agent_text",
        status: "done",
        payload: { text: "第二次正式回答" },
        created_at: "2026-11-20T00:00:05Z",
        updated_at: "2026-11-20T00:00:05Z",
      },
    ],
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [sampleHost],
        acpAgents: [agent],
        acpWorkspaces: [sampleWorkspace],
        acpSessions: [sess],
        activeAcpHostId: sampleHost.id,
        activeAcpWorkspaceId: sampleWorkspace.id,
        activeAcpSessionId: sess.id,
      });
      root.render(<AcpWorkspaceView />);
    });

    const groups = container.querySelectorAll(".acp-process-group");
    expect(groups).toHaveLength(2);
    expect((groups[0] as HTMLDetailsElement).open).toBe(false);
    expect((groups[1] as HTMLDetailsElement).open).toBe(false);
    expect(groups[1].textContent).toContain("思考 1 次");
    expect(groups[1].textContent).toContain("工具 1 次");

    // 两次正式回答直出且不受折叠影响；待确认权限卡保持展开在顶层以便立即操作
    const replies = container.querySelectorAll(".acp-turn-agent");
    expect(replies).toHaveLength(2);
    expect(replies[0].textContent).toContain("第一次正式回答");
    expect(replies[1].textContent).toContain("第二次正式回答");

    const pendingPerm = container.querySelector(".acp-perm-card") as HTMLElement;
    expect(pendingPerm).not.toBeNull();
    expect(pendingPerm.textContent).toContain("允许一次");
  } finally {
    act(() => root.unmount());
    container.remove();
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});

it("Agent 对话支持预览已发送附件、待发送 @文件 引用及模型返回文件地址（桌面右侧栏 / 移动右侧抽屉）", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: 1280,
  });

  const readFileSpy = vi.spyOn(acp, "readHostFile").mockImplementation(async (_hostId, path) => {
    if (path === "src/config.ts") {
      return {
        path: "C:/repo/src/config.ts",
        file_name: "config.ts",
        mime: "text/typescript",
        size: 32,
        encoding: "utf8",
        content: "export const PORT = 8080;\n",
        truncated: false,
      };
    }
    return {
      path: "C:/repo/src/engine.rs",
      file_name: "engine.rs",
      mime: "text/x-rust",
      size: 40,
      encoding: "utf8",
      content: "pub fn init() {}\npub fn run() { /* target */ }\n",
      truncated: false,
    };
  });

  const fetchBlobSpy = vi
    .spyOn(acp, "fetchFileBlob")
    .mockResolvedValue(new Blob(["# 已发送规格说明\n\n详细需求"], { type: "text/markdown" }));

  const agent: AcpAgent = {
    id: "a1",
    host_id: "h1",
    preset: "codex",
    name: "Codex CLI",
    adapter: "codex-acp",
    cmd: "codex-acp",
    env: {},
    short_tag: "CX",
    badge_bg: "from-emerald-500 to-teal-600",
    default_model: "gpt-5",
    model_options: ["gpt-5"],
    permission_override: "inherit",
    capabilities: [],
    status: "online",
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
  };

  const sess: AcpSession = {
    id: "s-preview",
    workspace_id: "w1",
    agent_id: "a1",
    title: "文件预览验证",
    selected_model: "gpt-5",
    thinking_level: "高",
    permission_policy: "ask",
    acp_session_key: null,
    running: false,
    created_at: "2026-11-20T00:00:00Z",
    updated_at: "2026-11-20T00:00:00Z",
    diffs: [],
    rpc_logs: [],
    turns: [
      {
        id: "u1",
        session_id: "s-preview",
        seq: 1,
        role: "user",
        status: "done",
        payload: {
          text: "请结合附件排查问题",
          file_refs: [],
          attachments: [
            {
              id: "att-1",
              file_name: "spec.md",
              mime: "text/markdown",
              size: 28,
            },
          ],
        },
        created_at: "2026-11-20T00:00:00Z",
        updated_at: "2026-11-20T00:00:00Z",
      },
      {
        id: "m1",
        session_id: "s-preview",
        seq: 2,
        role: "agent_text",
        status: "done",
        payload: {
          text: "问题定位在 `src/engine.rs:2`，请点击查看。",
        },
        created_at: "2026-11-20T00:00:01Z",
        updated_at: "2026-11-20T00:00:01Z",
      },
    ],
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [sampleHost],
        acpAgents: [agent],
        acpWorkspaces: [sampleWorkspace],
        acpSessions: [sess],
        activeAcpHostId: sampleHost.id,
        activeAcpWorkspaceId: sampleWorkspace.id,
        activeAcpSessionId: sess.id,
      });
      root.render(<AcpWorkspaceView />);
    });

    // 1) 点击用户已发送的附件 -> 桌面端右侧边栏打开并展示 Markdown 内容
    const sentAttBtn = Array.from(container.querySelectorAll(".acp-turn-user button")).find((b) =>
      b.textContent?.includes("spec.md"),
    ) as HTMLButtonElement;
    expect(sentAttBtn).not.toBeUndefined();
    await act(async () => {
      sentAttBtn.click();
    });

    await vi.waitFor(() => {
      expect(
        container.querySelector('[data-testid="acp-preview-sidebar"]')?.textContent,
      ).toContain("已发送规格说明");
    });
    expect(fetchBlobSpy).toHaveBeenCalledWith("att-1");

    // 2) 在输入框输入待发送 @src/config.ts 引用并点击预览
    const textarea = container.querySelector(".acp-composer textarea") as HTMLTextAreaElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(textarea, "请修改 @src/config.ts 端口");
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const pendingRefChip = container.querySelector(
      '[data-testid="acp-pending-file-ref"]',
    ) as HTMLButtonElement;
    expect(pendingRefChip).not.toBeNull();
    await act(async () => {
      pendingRefChip.click();
    });

    await vi.waitFor(() => {
      const sidebar = container.querySelector('[data-testid="acp-preview-sidebar"]');
      expect(sidebar?.textContent).toContain("PORT = 8080");
      expect(sidebar?.querySelector(".pv-badge")?.textContent).toBe("待发送");
    });
    expect(readFileSpy).toHaveBeenCalledWith("h1", "src/config.ts", sampleWorkspace.cwd);

    // 3) 点击模型回复中的文件地址 `src/engine.rs:2` -> 定位并高亮第 2 行
    const modelFileBtn = container.querySelector(".acp-md-file-code-btn") as HTMLButtonElement;
    expect(modelFileBtn).not.toBeNull();
    await act(async () => {
      modelFileBtn.click();
    });

    await vi.waitFor(() => {
      const hlRow = container.querySelector(
        '[data-testid="acp-preview-sidebar"] .pv-code-row.is-hl',
      );
      expect(hlRow?.getAttribute("data-line")).toBe("2");
      expect(hlRow?.textContent).toContain("pub fn run()");
    });

    // 4) 切换到窄屏移动端视口 -> 自动切换为右侧弹出覆盖层且支持收起
    act(() => {
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        writable: true,
        value: 390,
      });
      window.dispatchEvent(new Event("resize"));
    });

    expect(container.querySelector('[data-testid="acp-preview-sidebar"]')).toBeNull();
    expect(container.querySelector('[data-testid="pv-mobile-drawer"]')).not.toBeNull();
  } finally {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: 1024,
    });
    act(() => root.unmount());
    container.remove();
    setState(resetSessionState());
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  }
});
