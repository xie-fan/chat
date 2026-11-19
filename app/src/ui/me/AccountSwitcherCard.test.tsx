// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { resetSessionState, setState } from "../../store";
import type { UserPublic } from "../../types";
import AccountSwitcherCard from "./AccountSwitcherCard";

vi.mock("../../api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api")>()),
  currentAccountId: () => "active-account",
  savedAccounts: () => [
    { id: "active-account", serverUrl: "", user: { id: "u1", username: "alice", display_name: "旧昵称" } },
    { id: "inactive-account", serverUrl: "", user: { id: "u2", username: "bob", display_name: "另一个账号" } },
  ],
}));

it("个人资料更新后，当前账号行立即使用新昵称", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const host = document.createElement("div");
  const root = createRoot(host);
  const me: UserPublic = {
    id: "u1", username: "alice", display_name: "旧昵称", role: "user", last_seen_at: null,
  };
  try {
    act(() => {
      setState({ me });
      root.render(<AccountSwitcherCard />);
    });
    expect(host.textContent).toContain("旧昵称");
    act(() => setState({ me: { ...me, display_name: "新昵称" } }));
    expect(host.textContent).not.toContain("旧昵称");
    expect(host.textContent).toContain("新昵称");
    expect(host.textContent).toContain("另一个账号");
  } finally {
    act(() => root.unmount());
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});
