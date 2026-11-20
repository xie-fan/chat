// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { auth, serverInfo } from "../../api";
import PushSettings from "./PushSettings";

vi.mock("../../api", () => ({
  auth: { push: vi.fn() },
  serverInfo: vi.fn(),
  osOfUa: (ua: string) => ua.includes("Android") ? "Android" : "Linux",
}));

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("__TAURI_INTERNALS__", {});
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Android");
  vi.mocked(auth.push).mockResolvedValue({ available: false, enabled: false, preview: false, devices: [] });
  vi.mocked(serverInfo).mockResolvedValue({ registration_mode: "open", android_fcm_available: true });
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("shows native FCM status on Android", async () => {
  await act(async () => root.render(<PushSettings />));
  expect(host.textContent).toContain("系统推送（FCM）已启用，无需额外配置");
  expect(host.textContent).not.toContain("管理员未配置推送服务");
});

it.each([false, undefined])("treats FCM availability %s as unavailable", async (available) => {
  vi.mocked(serverInfo).mockResolvedValue({ registration_mode: "open", android_fcm_available: available });
  await act(async () => root.render(<PushSettings />));
  expect(host.textContent).toContain("管理员未配置推送服务");
});

it.each(["web", "desktop"])("keeps %s behavior unchanged", async (platform) => {
  if (platform === "web") vi.stubGlobal("__TAURI_INTERNALS__", undefined);
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue(platform === "web" ? "Linux" : "Windows");
  await act(async () => root.render(<PushSettings />));
  expect(host.textContent).toContain("管理员未配置推送服务");
});

it("keeps ntfy controls reachable alongside FCM", async () => {
  vi.mocked(auth.push).mockResolvedValue({ available: true, enabled: true, preview: false, devices: [] });
  await act(async () => root.render(<PushSettings />));
  expect(host.textContent).toContain("系统推送（FCM）已启用");
  expect(host.textContent).toContain("额外推送（ntfy）");
  expect(host.textContent).toContain("添加手机");
});

it("preserves push loading errors", async () => {
  vi.mocked(auth.push).mockRejectedValue(new Error("加载失败"));
  await act(async () => root.render(<PushSettings />));
  expect(host.textContent).toContain("加载失败");
});

it("does not claim FCM is active while notifications are off on this device", async () => {
  localStorage.setItem("chat_notify_enabled", "0");
  try {
    await act(async () => root.render(<PushSettings />));
    expect(host.textContent).toContain("打开「桌面通知」后生效");
    expect(host.textContent).not.toContain("已启用");
    localStorage.setItem("chat_notify_enabled", "1");
    await act(async () => window.dispatchEvent(new Event("chat:android-push-settings")));
    expect(host.textContent).toContain("系统推送（FCM）已启用");
  } finally {
    localStorage.removeItem("chat_notify_enabled");
  }
});
