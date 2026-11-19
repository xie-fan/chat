import { describe, expect, it } from "vitest";
import {
  apkProgressPercent,
  apkProgressText,
  assetFor,
  assetLabel,
  hasInstaller,
  isApkPermissionDenied,
  isApkSignatureError,
  isNewer,
} from "./update";

describe("isNewer", () => {
  it.each([
    ["0.2.0", "0.1.0", true],
    ["1.2.10", "1.2.9", true],
    ["1.0.0", "1.0.0", false],
    ["0.9.9", "1.0.0", false],
    ["1.1", "1.0.5", true],
    ["1.0.0", "1.0", false],
  ])("isNewer(%s, %s) = %s", (a, b, want) => {
    expect(isNewer(a, b)).toBe(want);
  });
});

describe("安装包挑选", () => {
  const names = [
    "Chat_0.2.0_amd64.AppImage",
    "Chat_0.2.0_amd64.AppImage.sig",
    "Chat_0.2.0_amd64.deb",
    "Chat_0.2.0_arm64.apk",
    "Chat_0.2.0_x64-setup.exe",
    "Chat_0.2.0_x64-setup.exe.sig",
    "Chat_0.2.0_universal.dmg",
  ];
  it.each([
    ["Windows", "Chat_0.2.0_x64-setup.exe"],
    ["Android", "Chat_0.2.0_arm64.apk"],
    ["Linux", "Chat_0.2.0_amd64.AppImage"],
    ["macOS", "Chat_0.2.0_universal.dmg"],
    ["iOS", undefined],
  ])("%s → %s", (os, want) => {
    expect(assetFor(os, names)).toBe(want);
  });
  it("签名文件不列给人下载", () => {
    expect(names.map(assetLabel).filter(Boolean)).toEqual([
      "Linux（AppImage）",
      "Linux（deb）",
      "Android",
      "Windows",
      "macOS",
    ]);
  });
});

describe("hasInstaller", () => {
  it("本平台包传完（桌面带 .sig）才算有", () => {
    const linuxOnly = ["Chat_0.2.1_amd64.AppImage", "Chat_0.2.1_amd64.AppImage.sig"];
    expect(hasInstaller("Linux", linuxOnly)).toBe(true);
    expect(hasInstaller("Windows", linuxOnly)).toBe(false);
    expect(hasInstaller("Windows", ["Chat_0.2.1_x64-setup.exe"])).toBe(false);
    expect(hasInstaller("Android", ["Chat_0.2.1.apk"])).toBe(true);
    expect(hasInstaller("macOS", ["Chat_0.2.1_universal.dmg"])).toBe(false);
  });
});

describe("isApkPermissionDenied", () => {
  it.each([
    ["PERMISSION_DENIED:未允许安装未知来源应用", true],
    [new Error("PERMISSION_DENIED:x"), true],
    ["下载失败（HTTP 500）", false],
    ["INSTALL_FAILED:系统没有可用的安装器", false],
    [new Error("网络错误"), false],
    [null, false],
    [undefined, false],
  ])("%j → %s", (e, want) => {
    expect(isApkPermissionDenied(e)).toBe(want);
  });
});

// Signature Mismatch（系统错误 -7 = INSTALL_FAILED_UPDATE_INCOMPATIBLE）→ 卸载一次文案
describe("isApkSignatureError", () => {
  it.each([
    ["INSTALL_FAILED_UPDATE_INCOMPATIBLE", true],
    ["INSTALL_FAILED: signatures do not match the previously installed version", true],
    ["adb install 失败：Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: -7]", true],
    [new Error("签名不一致"), true],
    ["INSTALL_FAILED:系统没有可用的安装器", false],
    ["PERMISSION_DENIED:未允许安装未知来源应用", false],
    ["安装失败：安装包不存在", false],
    ["下载失败（HTTP 500）", false],
    ["design error in config", false],
    ["assigned to device slot", false],
    ["version 1.2.3-7 is old", false], // 版本号里的 -7 不算
    ["下载失败（HTTP -70）", false],
    [null, false],
    [undefined, false],
  ])("%j → %s", (e, want) => {
    expect(isApkSignatureError(e)).toBe(want);
  });
});

const MB = 1024 * 1024;

describe("apkProgressPercent", () => {
  it.each([
    [5 * MB, 20 * MB, 25],
    [0, 20 * MB, 0],
    [30 * MB, 20 * MB, 100], // 封顶
    [5 * MB, -1, null], // 服务器没给长度
    [5 * MB, 0, null],
  ])("(%d, %d) → %s", (received, total, want) => {
    expect(apkProgressPercent(received, total)).toBe(want);
  });
});

describe("apkProgressText", () => {
  it("已知总长度 → 百分比 + 已收/总量", () => {
    expect(apkProgressText(5 * MB, 20 * MB)).toBe("已下载 5.0 / 20.0 MB（25%）");
  });
  it("未知总长度 → 只报已收", () => {
    expect(apkProgressText(3 * MB, -1)).toBe("已下载 3.0 MB");
    expect(apkProgressText(0, -1)).toBe("已下载 0.0 MB");
  });
});
