// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { resetSessionState, setState } from "../../store";
import type { AcpAgent, AcpHost } from "../../types";
import AgentSettingsSection from "./AgentSettingsSection";

const singleHost: AcpHost = {
  id: "h1",
  name: "14X",
  os: "windows · x86_64",
  ip: "127.0.0.1",
  roots: ["C:\\work"],
  online: true,
  latency_ms: 731,
  last_seen: "2026-11-20T00:00:00Z",
};

const sampleAgent: AcpAgent = {
  id: "a1",
  host_id: "h1",
  preset: "gemini",
  name: "Gemini CLI",
  adapter: "gemini --acp",
  cmd: "gemini --acp",
  env: {},
  short_tag: "GM",
  badge_bg: "from-indigo-500 to-sky-500",
  default_model: "Gemini 2.5 Pro",
  model_options: ["Gemini 2.5 Pro", "Gemini 2.5 Flash"],
  permission_override: "inherit",
  capabilities: ["fs.read"],
  status: "online",
  created_at: "2026-11-20T00:00:00Z",
  updated_at: "2026-11-20T00:00:00Z",
};

it("仅有一个宿主节点时点击收起按钮可正常折叠与再展开", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    act(() => {
      setState({
        acpLoaded: true,
        acpHosts: [singleHost],
        acpAgents: [sampleAgent],
      });
      root.render(<AgentSettingsSection />);
    });

    const head = container.querySelector(".host-group-head") as HTMLElement;
    expect(head).not.toBeNull();
    expect(head.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".host-group-body")).not.toBeNull();
    expect(container.textContent).toContain("Gemini CLI");

    act(() => {
      head.click();
    });
    expect(head.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".host-group-body")).toBeNull();

    act(() => {
      head.click();
    });
    expect(head.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".host-group-body")).not.toBeNull();
    expect(container.textContent).toContain("Gemini CLI");
  } finally {
    act(() => root.unmount());
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});
