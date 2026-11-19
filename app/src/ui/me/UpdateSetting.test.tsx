// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import UpdateSetting from "./UpdateSetting";

(globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;

const APK_NAME = "Chat_0.7.1_arm64.apk";
const LATEST = {
  version: "0.7.1",
  notes: "",
  published_at: "2026-10-31T00:00:00Z",
  assets: [
    { name: "Chat_0.7.1_x64-setup.exe", size: 1 },
    { name: "Chat_0.7.1_x64-setup.exe.sig", size: 1 },
    { name: APK_NAME, size: 1 },
  ],
};

const { invoke, openUrl } = vi.hoisted(() => ({
  invoke: vi.fn(),
  openUrl: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.7.0") }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));

/** installError/downloadError 非空 → 对应 invoke 直接 reject；denied → 系统未授予 Install Permission */
function stubCommands(opts: {
  installError?: unknown;
  downloadError?: unknown;
  denied?: boolean;
} = {}) {
  invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "download_update_apk":
        if (opts.downloadError) throw opts.downloadError;
        return { path: `/data/cache/apk-update/${APK_NAME}`, name: APK_NAME };
      case "install_update_apk":
        if (opts.installError) throw opts.installError;
        return undefined;
      case "apk_download_progress":
        return { state: "downloading", receivedBytes: 1, totalBytes: 2, name: APK_NAME, error: "" };
      case "check_install_permission":
        return !opts.denied;
      case "open_install_permission_settings":
        return undefined;
      case "install_update": // 桌面路径
        return true;
      default:
        throw new Error(`unexpected invoke ${cmd}`);
    }
  });
}

const UA = {
  android:
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
  windows:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
  linux: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 Safari/605.1.15",
};

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  invoke.mockReset();
  openUrl.mockClear();
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => LATEST })));
  vi.stubGlobal("__TAURI_INTERNALS__", {});
  host = document.createElement("div");
  document.body.appendChild(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

const flush = async () => {
  for (let i = 0; i < 10; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
};

const renderOn = async (ua: string) => {
  vi.stubGlobal("navigator", { userAgent: ua });
  root = createRoot(host);
  act(() => root.render(<UpdateSetting />));
  await flush();
};

const btn = (label: string): HTMLButtonElement | undefined =>
  Array.from(host.querySelectorAll("button")).find((b) => b.textContent === label);
const link = (label: string): HTMLAnchorElement | undefined =>
  Array.from(host.querySelectorAll("a")).find((a) => a.textContent === label);
const click = (el: Element | undefined) =>
  act(() => {
    el!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
const callsOf = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);

describe("Android UpdateSetting → In-app APK Installer", () => {
  it("有新版：常驻卸载提示 + 浏览器兜底；「下载安装」→ 自动拉起系统安装器", async () => {
    stubCommands();
    await renderOn(UA.android);
    // Manual Update Check 前的常驻一次性卸载提示（0.6.2 / 签名不一致 0.7.0 设备要用）
    expect(host.textContent).toContain("若安装失败请先卸载旧版再装（仅此一次）");
    expect(link("用浏览器下载")).toBeTruthy();
    click(btn("下载安装"));
    await flush();
    expect(callsOf("download_update_apk")).toHaveLength(1);
    expect(callsOf("download_update_apk")[0][1]).toMatchObject({
      url: expect.stringContaining(`/v1/client/download/${APK_NAME}`),
    });
    // 下载完自动拉起系统安装器；取消后「安装」保留可再拉
    expect(callsOf("install_update_apk")).toHaveLength(1);
    expect(callsOf("install_update_apk")[0][1]).toEqual({ name: APK_NAME });
    expect(host.textContent).toContain("已打开系统安装器");
    expect(btn("安装")).toBeTruthy();
    click(btn("安装"));
    await flush();
    expect(callsOf("install_update_apk")).toHaveLength(2);
    expect(callsOf("download_update_apk")).toHaveLength(1); // 不重新下载
  });

  it("Install Permission 被拒：中文引导 + 去开启 + 安装/浏览器兜底仍在", async () => {
    stubCommands({ installError: new Error("PERMISSION_DENIED:未允许安装未知来源应用") });
    await renderOn(UA.android);
    click(btn("下载安装"));
    await flush();
    expect(host.textContent).toContain("未开启「安装未知应用」权限");
    expect(btn("去开启")).toBeTruthy();
    expect(btn("安装")).toBeTruthy();
    expect(link("用浏览器下载")).toBeTruthy();
    click(btn("去开启"));
    await flush();
    expect(callsOf("open_install_permission_settings")).toHaveLength(1);
  });

  it("预先检查到无 Install Permission：不白拉安装器，直接给引导", async () => {
    stubCommands({ denied: true });
    await renderOn(UA.android);
    click(btn("下载安装"));
    await flush();
    expect(host.textContent).toContain("未开启「安装未知应用」权限");
    expect(callsOf("install_update_apk")).toHaveLength(0);
    expect(btn("安装")).toBeTruthy(); // 授权后回来直接装，不用重下
  });

  it("Signature Mismatch（-7）→ 卸载一次文案 + 浏览器兜底", async () => {
    stubCommands({
      installError: new Error("Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: -7]"),
    });
    await renderOn(UA.android);
    click(btn("下载安装"));
    await flush();
    expect(host.textContent).toContain("签名不一致");
    expect(host.textContent).toContain("请先卸载旧版再装（仅此一次）");
    expect(link("用浏览器下载")).toBeTruthy();
  });

  it("下载失败：回到可重试态 + 错误提示", async () => {
    stubCommands({ downloadError: new Error("下载超时") });
    await renderOn(UA.android);
    click(btn("下载安装"));
    await flush();
    expect(host.textContent).toContain("下载超时");
    expect(btn("下载安装")).toBeTruthy(); // 可重试
  });

  it("浏览器兜底：openUrl 打开同一个 APK 地址", async () => {
    stubCommands();
    await renderOn(UA.android);
    click(link("用浏览器下载"));
    await flush();
    expect(openUrl).toHaveBeenCalledWith(
      expect.stringContaining(`/v1/client/download/${APK_NAME}`),
    );
  });
});

describe("非 Android 平台不变", () => {
  it("Linux 没有更新时仍可检查，并可发现后来发布的安装包", async () => {
    const appImage = "Chat_0.7.1_amd64.AppImage";
    vi.mocked(fetch).mockResolvedValueOnce({
      ok: true,
      json: async () => ({ ...LATEST, version: "0.7.0" }),
    } as Response);
    await renderOn(UA.linux);
    expect(btn("更新并重启")).toBeUndefined();
    vi.mocked(fetch).mockResolvedValueOnce({
      ok: true,
      json: async () => ({
        ...LATEST,
        assets: [appImage, `${appImage}.sig`].map((name) => ({ name, size: 1 })),
      }),
    } as Response);
    click(btn("检查更新"));
    await flush();
    expect(btn("更新并重启")).toBeTruthy();
    expect(callsOf("install_update")).toHaveLength(0);
  });

  it("桌面端仍是「更新并重启」→ install_update", async () => {
    stubCommands();
    await renderOn(UA.windows);
    click(btn("更新并重启"));
    await flush();
    expect(callsOf("install_update")).toHaveLength(1);
    expect(callsOf("download_update_apk")).toHaveLength(0);
  });

  it("macOS 提供 DMG 手动下载而不是自动安装", async () => {
    const name = "Chat_0.7.1_universal.dmg";
    vi.mocked(fetch).mockResolvedValueOnce({
      ok: true,
      json: async () => ({ ...LATEST, assets: [{ name, size: 1 }] }),
    } as Response);
    await renderOn("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    expect(btn("更新并重启")).toBeUndefined();
    click(btn("下载 macOS 版"));
    await flush();
    expect(openUrl).toHaveBeenCalledWith(expect.stringContaining(`/v1/client/download/${name}`));
    expect(callsOf("install_update")).toHaveLength(0);
  });
});
