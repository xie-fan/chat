// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { admin } from "../../api";
import { listenForNativeDrops } from "../../fileDrop";
import BackupPanel from "./BackupPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../fileDrop", () => ({
  isLinuxDesktop: () => true,
  listenForNativeDrops: vi.fn(),
}));

it("selects a native backup only when dropped onto the restore file input", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.spyOn(admin, "backups").mockResolvedValue({ items: [], in_progress: false });
  const restore = vi.spyOn(admin, "restoreDroppedBackup").mockRejectedValue(new Error("test"));
  vi.mocked(invoke).mockResolvedValue("selected-backup");
  let drop: Parameters<typeof listenForNativeDrops>[0] = () => {};
  vi.mocked(listenForNativeDrops).mockImplementation(async (handler) => {
    drop = handler;
    return () => {};
  });
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  try {
    await act(async () => root.render(<BackupPanel />));
    const input = host.querySelector<HTMLInputElement>('input[type="file"]')!;
    const confirm = host.querySelector<HTMLInputElement>(".bk-confirm input")!;
    const button = host.querySelector<HTMLButtonElement>(".bk-confirm button")!;
    const path = "/home/user/chat-backup.tar.gz";
    await act(async () => drop([path], confirm));
    expect(invoke).not.toHaveBeenCalled();
    await act(async () => drop([path], input));
    expect(invoke).toHaveBeenCalledWith("select_dropped_backup", { path });
    expect(host.textContent).toContain("chat-backup.tar.gz");
    expect(button.disabled).toBe(true);
    expect(restore).not.toHaveBeenCalled();
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(confirm, "RESTORE");
      confirm.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(button.disabled).toBe(false);
    await act(async () => button.click());
    expect(restore).toHaveBeenCalledWith("selected-backup", expect.any(Function));
  } finally {
    await act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  }
});
