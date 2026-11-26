import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { saveAttachment } from "./download";
import { files, getServer } from "./api";
import { getState, setState } from "./store";
import type { AttachmentView } from "./types";
// 入口接线校验用源码（C6：所有下载入口必须共用 saveAttachment）
import chatView from "./ui/chat/ChatView.tsx?raw";
import convMedia from "./ui/chat/ConvMedia.tsx?raw";

// ===== MM4：saveAttachment 平台分发 + 全入口共用 =====

const ATT: AttachmentView = {
  id: "11111111-2222-3333-4444-555555555555",
  file_name: "报表.pdf",
  mime: "application/pdf",
  size: 1024,
};

function toasts() {
  return getState().toasts;
}

const downloadUrl = () => `${getServer()}/v1/files/${ATT.id}?dl=1&ticket=file.ticket.signature`;

describe("saveAttachment：平台分发", () => {
  beforeEach(() => {
    setState({ toasts: [] });
    vi.spyOn(files, "access").mockImplementation(async () => ({ url: downloadUrl(), expiresAt: Infinity }));
  });
  afterEach(() => vi.restoreAllMocks());

  it("Web：<a download> 使用限时附件地址，不弹 toast", async () => {
    const anchor = vi.fn();
    await saveAttachment(ATT, { tauri: false, anchor });
    expect(anchor).toHaveBeenCalledWith(
      downloadUrl(),
      "报表.pdf",
    );
    expect(toasts()).toHaveLength(0);
  });

  it("Tauri 桌面：invoke download_to_downloads，toast 下载中…→已保存到下载 并带 filePath", async () => {
    const invoke = vi.fn(async () => ({ path: "/home/u/Downloads/报表.pdf", name: "报表.pdf" }));
    await saveAttachment(ATT, { tauri: true, desktop: true, invoke });
    expect(invoke).toHaveBeenCalledWith(
      "download_to_downloads",
      expect.objectContaining({
        url: downloadUrl(),
        fileName: "报表.pdf",
        // C1：origin 白名单由前端服务器存储给出
        allowedOrigins: expect.arrayContaining([getServer()]),
      }),
    );
    const t = toasts();
    expect(t).toHaveLength(1);
    expect(t[0].title).toBe("已保存到下载：报表.pdf");
    expect(t[0].filePath).toBe("/home/u/Downloads/报表.pdf");
  });

  it("Tauri Android（非桌面）：成功 toast 不带 filePath（无点击动作）", async () => {
    const invoke = vi.fn(async () => ({ path: "content://dl/1", name: "报表.pdf" }));
    await saveAttachment(ATT, { tauri: true, desktop: false, invoke });
    const t = toasts();
    expect(t[0].title).toBe("已保存到下载：报表.pdf");
    expect(t[0].filePath).toBeUndefined();
  });

  it("Tauri 失败：toast 更新为「下载失败」+ 错误文案", async () => {
    const invoke = vi.fn(async () => {
      throw new Error("文件已撤回或不存在");
    });
    await saveAttachment(ATT, { tauri: true, desktop: true, invoke });
    const t = toasts();
    expect(t[0].title).toBe("下载失败");
    expect(t[0].text).toContain("文件已撤回或不存在");
    expect(t[0].filePath).toBeUndefined();
  });

  it("An unavailable attachment never starts a browser download", async () => {
    vi.mocked(files.access).mockRejectedValue(new Error("file not found"));
    const anchor = vi.fn();
    await saveAttachment(ATT, { tauri: false, anchor });
    expect(anchor).not.toHaveBeenCalled();
    expect(toasts()[0].title).toBe("下载失败");
  });
});

// C6：所有下载入口（消息菜单/动作面板、预览工具栏、文件与图片面板）必须走同一个 helper
describe("saveAttachment：全入口共用", () => {
  it("ChatView：菜单/动作面板 dispatch 与 viewer-download 均调 saveAttachment", () => {
    expect(chatView).toContain('from "../../download"');
    // 消息菜单 & 动作面板共用同一个 dispatch
    expect(chatView).toContain("saveAttachment(m.attachment)");
    // 预览/查看器工具栏
    expect(chatView).toContain("saveAttachment(preview.att)");
    // 不再保留 <a download> 老路径
    expect(chatView).not.toContain('document.createElement("a")');
  });

  it("ConvMedia：文件列表行内下载调 saveAttachment，不再是 <a download>", () => {
    expect(convMedia).toContain("saveAttachment(m.attachment!)");
    expect(convMedia).not.toContain("download={");
  });
});
