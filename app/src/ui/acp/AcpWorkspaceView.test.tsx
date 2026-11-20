// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { resetSessionState, setState } from "../../store";
import type { AcpHost, AcpWorkspace } from "../../types";
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
