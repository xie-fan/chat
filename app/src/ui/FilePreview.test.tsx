// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { resetSessionState, setState } from "../store";
import type { ConversationView, MessageView } from "../types";
import ChatView from "./chat/ChatView";
import FilePreview, {
  classifyPreviewKind,
  shouldCollapseOnSwipeRight,
  shouldExpandOnSwipeLeft,
  type FilePreviewTarget,
} from "./FilePreview";

function setViewportWidth(width: number, height = 800) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    writable: true,
    value: height,
  });
  window.dispatchEvent(new Event("resize"));
}

afterEach(() => {
  setViewportWidth(1024, 768);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

async function mountPreview(props: {
  variant: "chat" | "agent";
  target: FilePreviewTarget;
  openSeq?: number;
  onClose?: () => void;
}) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const onClose = props.onClose ?? vi.fn();
  await act(async () => {
    root.render(
      <FilePreview
        variant={props.variant}
        target={props.target}
        openSeq={props.openSeq}
        onClose={onClose}
      />,
    );
  });
  return {
    host,
    root,
    onClose,
    unmount: () => {
      act(() => root.unmount());
      host.remove();
    },
  };
}

describe("FilePreview", () => {
  it("classifies file types accurately", () => {
    expect(classifyPreviewKind("photo.webp", "application/octet-stream")).toBe("image");
    expect(classifyPreviewKind("README.md", "text/plain")).toBe("markdown");
    expect(classifyPreviewKind("report.csv", "text/csv")).toBe("csv");
    expect(classifyPreviewKind("FilePreview.tsx", "")).toBe("code");
    expect(classifyPreviewKind("manual.pdf", "application/pdf")).toBe("pdf");
    expect(classifyPreviewKind("archive.zip", "application/zip")).toBe("unsupported");
  });

  it("only embeds a PDF after checking its file header", async () => {
    const hadCreateObjectUrl = typeof URL.createObjectURL === "function";
    const previousCreateObjectUrl = URL.createObjectURL;
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      writable: true,
      value: vi.fn(() => "blob:validated-pdf"),
    });
    const { host, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "local-file",
        key: "local:document.pdf",
        file: new File(["%PDF-1.7\n1 0 obj\n"], "document.pdf", {
          type: "application/octet-stream",
        }),
      },
    });
    try {
      await vi.waitFor(() => {
        expect(host.querySelector('iframe[src="blob:validated-pdf"]')).not.toBeNull();
      });
      expect(host.querySelector(".pv-unsupported")).toBeNull();
    } finally {
      unmount();
      if (hadCreateObjectUrl) {
        Object.defineProperty(URL, "createObjectURL", {
          configurable: true,
          writable: true,
          value: previousCreateObjectUrl,
        });
      } else {
        Object.defineProperty(URL, "createObjectURL", {
          configurable: true,
          writable: true,
          value: undefined,
        });
      }
    }
  });

  it("does not send a PDF-looking HTML file to the embedded viewer", async () => {
    const { host, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "local-file",
        key: "local:fake.pdf",
        file: new File(["<!doctype html><script>alert(1)</script>"], "fake.pdf", {
          type: "application/pdf",
        }),
      },
    });
    try {
      await vi.waitFor(() => {
        expect(host.textContent).toContain("文件不是可预览的 PDF");
      });
      expect(host.querySelector("iframe")).toBeNull();
    } finally {
      unmount();
    }
  });

  it("evaluates swipe collapse / expand thresholds and velocity", () => {
    expect(shouldCollapseOnSwipeRight(96, 0.1, 340)).toBe(true);
    expect(shouldCollapseOnSwipeRight(28, 0.55, 340)).toBe(true);
    expect(shouldCollapseOnSwipeRight(120, -0.4, 340)).toBe(false);
    expect(shouldCollapseOnSwipeRight(14, 0.05, 340)).toBe(false);

    expect(shouldExpandOnSwipeLeft(-72, -0.1, 340)).toBe(true);
    expect(shouldExpandOnSwipeLeft(-24, -0.45, 340)).toBe(true);
    expect(shouldExpandOnSwipeLeft(-80, 0.4, 340)).toBe(false);
    expect(shouldExpandOnSwipeLeft(-10, -0.05, 340)).toBe(false);
  });

  it("renders centered modal on desktop chat for pending local files and guards against drag-release dismiss", async () => {
    setViewportWidth(1280, 800);
    const pendingFile = new File(["const answer = 42;\nexport default answer;\n"], "draft.ts", {
      type: "text/typescript",
    });
    const onClose = vi.fn();
    const { host, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "local-file",
        key: "pending:draft.ts",
        file: pendingFile,
        badge: "待发送",
      },
      onClose,
    });

    try {
      await vi.waitFor(() => {
        expect(host.textContent).toContain("const answer = 42;");
      });

      const mask = host.querySelector('[data-testid="file-preview-modal"]') as HTMLElement;
      const modal = host.querySelector(".pv-modal") as HTMLElement;
      expect(mask).not.toBeNull();
      expect(modal).not.toBeNull();
      expect(host.querySelector(".pv-badge")?.textContent).toBe("待发送");

      // 在弹窗内部按下、拖到背景遮罩松开（如框选文本），不应误关弹窗
      act(() => {
        modal.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
        mask.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      });
      expect(onClose).not.toHaveBeenCalled();

      // 在遮罩外部按下并点击才关闭弹窗
      act(() => {
        mask.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
        mask.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      });
      expect(onClose).toHaveBeenCalledTimes(1);
    } finally {
      unmount();
    }
  });

  it("renders docked right sidebar on desktop agent with line highlight and markdown/csv view toggles", async () => {
    setViewportWidth(1280, 800);
    const loadCode = vi.fn().mockResolvedValue({
      path: "C:/repo/src/main.ts",
      file_name: "main.ts",
      mime: "text/typescript",
      size: 48,
      encoding: "utf8",
      content: "line1\nline2-target\nline3-target\nline4",
      truncated: false,
    });

    const { host, root, unmount } = await mountPreview({
      variant: "agent",
      target: {
        kind: "host-path",
        key: "host:main.ts:2-3",
        path: "src/main.ts",
        lineStart: 2,
        lineEnd: 3,
        loadHostFile: loadCode,
      },
    });

    try {
      await vi.waitFor(() => {
        expect(host.textContent).toContain("line2-target");
      });

      const sidebar = host.querySelector('[data-testid="acp-preview-sidebar"]');
      expect(sidebar).not.toBeNull();
      expect(host.querySelector(".pv-line-badge")?.textContent).toBe("L2-3");

      const highlighted = Array.from(host.querySelectorAll(".pv-code-row.is-hl")).map((el) =>
        el.getAttribute("data-line"),
      );
      expect(highlighted).toEqual(["2", "3"]);

      // 切换为 CSV 文件，验证表格渲染与源码切换
      const loadCsv = vi.fn().mockResolvedValue({
        path: "C:/repo/metrics.csv",
        file_name: "metrics.csv",
        mime: "text/csv",
        size: 32,
        encoding: "utf8",
        content: "name,score\nalice,98\nbob,95\n",
        truncated: false,
      });

      await act(async () => {
        root.render(
          <FilePreview
            variant="agent"
            target={{
              kind: "host-path",
              key: "host:metrics.csv",
              path: "metrics.csv",
              loadHostFile: loadCsv,
            }}
            onClose={() => {}}
          />,
        );
      });

      await vi.waitFor(() => {
        expect(host.querySelector(".pv-csv-table")).not.toBeNull();
      });
      expect(host.querySelector(".pv-csv-table")?.textContent).toContain("alice");

      const rawBtn = host.querySelector('[data-testid="pv-toggle-raw"]') as HTMLButtonElement;
      expect(rawBtn).not.toBeNull();
      act(() => {
        rawBtn.click();
      });
      expect(host.querySelector(".pv-csv-table")).toBeNull();
      expect(host.querySelector(".pv-code-lines")?.textContent).toContain("name,score");
    } finally {
      unmount();
    }
  });

  it("renders right overlay on mobile (both chat & agent) and supports swipe-right collapse and swipe-left expand", async () => {
    setViewportWidth(390, 844);
    const onClose = vi.fn();
    const { host, unmount } = await mountPreview({
      variant: "agent",
      target: {
        kind: "local-file",
        key: "mobile:notes.md",
        file: new File(["# 移动端预览标题\n\n正文内容"], "notes.md", {
          type: "text/markdown",
        }),
      },
      onClose,
    });

    try {
      await vi.waitFor(() => {
        expect(host.querySelector(".pv-md-stage h1")?.textContent).toBe("移动端预览标题");
      });

      const drawer = host.querySelector('[data-testid="pv-mobile-drawer"]') as HTMLElement;
      const sheet = host.querySelector(".pv-mobile-sheet") as HTMLElement;
      expect(drawer).not.toBeNull();
      expect(drawer.getAttribute("data-collapsed")).toBe("0");
      expect(host.querySelector('[data-testid="pv-edge-expander"]')).toBeNull();

      // 模拟右滑手势 -> 收起至右侧边缘，保留会话区域可交互且不销毁预览状态
      act(() => {
        sheet.dispatchEvent(
          new PointerEvent("pointerdown", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 1,
            clientX: 80,
            clientY: 260,
          }),
        );
        sheet.dispatchEvent(
          new PointerEvent("pointermove", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 1,
            clientX: 220,
            clientY: 264,
          }),
        );
        sheet.dispatchEvent(
          new PointerEvent("pointerup", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 1,
            clientX: 250,
            clientY: 265,
          }),
        );
      });

      expect(drawer.getAttribute("data-collapsed")).toBe("1");
      expect(onClose).not.toHaveBeenCalled();

      const expander = host.querySelector('[data-testid="pv-edge-expander"]') as HTMLElement;
      expect(expander).not.toBeNull();

      // 模拟在右侧边缘左滑 -> 重新展开预览层
      act(() => {
        expander.dispatchEvent(
          new PointerEvent("pointerdown", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 2,
            clientX: 386,
            clientY: 320,
          }),
        );
        expander.dispatchEvent(
          new PointerEvent("pointermove", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 2,
            clientX: 260,
            clientY: 322,
          }),
        );
        expander.dispatchEvent(
          new PointerEvent("pointerup", {
            bubbles: true,
            pointerType: "touch",
            pointerId: 2,
            clientX: 240,
            clientY: 322,
          }),
        );
      });

      expect(drawer.getAttribute("data-collapsed")).toBe("0");
      expect(host.querySelector('[data-testid="pv-edge-expander"]')).toBeNull();

      // 点击关闭按钮彻底关闭预览
      const closeBtn = host.querySelector('[data-testid="viewer-close"]') as HTMLButtonElement;
      act(() => {
        closeBtn.click();
      });
      expect(onClose).toHaveBeenCalledTimes(1);
    } finally {
      unmount();
    }
  });

  it("integrates with ChatView to preview received/sent attachments and pending composer files", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    setViewportWidth(1280, 800);

    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: RequestInfo | URL) => {
        const s = String(url);
        if (s.endsWith("/v1/files/f-recv/ticket")) {
          return Response.json({ ticket: "file.ticket.signature", expires_in: 600 });
        }
        if (s.includes("/v1/files/f-recv")) {
          return new Response("hello from chat file\n", {
            status: 200,
            headers: { "content-type": "text/plain" },
          });
        }
        if (s.endsWith("/pins")) {
          return Response.json([]);
        }
        if (s.includes("/messages")) {
          return Response.json({ messages: [], has_more: false });
        }
        return Response.json({});
      }),
    );

    const conv: ConversationView = {
      id: "c1",
      kind: "self",
      name: "文件传输助手",
      owner_id: "u1",
      peer: null,
      member_count: 1,
      last_seq: 1,
      unread_count: 0,
      has_mention: false,
      last_message: null,
      created_at: "2026-11-20T00:00:00Z",
    };

    const msg: MessageView = {
      id: "m1",
      conversation_id: "c1",
      seq: 1,
      sender_id: "u1",
      sender_username: "me",
      sender_name: "我",
      kind: "file",
      content: "[文件] received.txt",
      attachment: {
        id: "f-recv",
        file_name: "received.txt",
        mime: "text/plain",
        size: 21,
        width: null,
        height: null,
        duration_ms: null,
        has_thumb: false,
      },
      mentions: [],
      reactions: [],
      poll: null,
      edit_version: 1,
      edited_at: null,
      deleted_at: null,
      created_at: "2026-11-20T00:00:00Z",
    };

    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    try {
      await act(async () => {
        setState({
          me: {
            id: "u1",
            username: "me",
            display_name: "我",
            role: "user",
            last_seen_at: null,
          },
          conversations: [conv],
          activeConv: "c1",
          messages: { c1: [msg] },
          hasMore: { c1: false },
          historyLoaded: { c1: true },
        });
        root.render(<ChatView />);
      });

      // 1) 点击收发文件气泡 -> 桌面端中央弹窗打开
      const fileBubble = host.querySelector(".msg-file") as HTMLElement;
      expect(fileBubble).not.toBeNull();
      await act(async () => {
        fileBubble.click();
      });

      await vi.waitFor(() => {
        expect(
          host.querySelector('[data-testid="file-preview-modal"]')?.textContent,
        ).toContain("hello from chat file");
      });

      // 2) 选择待发送文件并点击待发芯片 -> 切换预览待发文件
      const fileInput = host.querySelector('input[type="file"]') as HTMLInputElement;
      const pending = new File(["pending markdown body"], "todo.md", {
        type: "text/markdown",
      });
      Object.defineProperty(fileInput, "files", {
        configurable: true,
        value: [pending],
      });
      await act(async () => {
        fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      });

      const pendingChipBtn = host.querySelector(
        '[data-testid="pending-file-chip"] .pf-chip-main',
      ) as HTMLButtonElement;
      expect(pendingChipBtn).not.toBeNull();

      // 避开 380ms 幽灵点击保护窗口
      vi.spyOn(Date, "now").mockReturnValue(Date.now() + 1000);
      await act(async () => {
        pendingChipBtn.click();
      });

      await vi.waitFor(() => {
        const modal = host.querySelector('[data-testid="file-preview-modal"]');
        expect(modal?.textContent).toContain("pending markdown body");
        expect(modal?.querySelector(".pv-badge")?.textContent).toBe("待发送");
      });

      // 3) 切换为移动端视口 -> 聊天内同样自动变为右侧弹出层
      act(() => {
        setViewportWidth(390, 844);
      });
      expect(host.querySelector('[data-testid="file-preview-modal"]')).toBeNull();
      expect(host.querySelector('[data-testid="pv-mobile-drawer"]')).not.toBeNull();
    } finally {
      act(() => root.unmount());
      host.remove();
      setState(resetSessionState());
    }
  });

  it("streams and caps chat text preview at 512 KiB bytes and handles UTF-8 boundary cleanly", async () => {
    setViewportWidth(1280, 800);
    const chunkSize = 64 * 1024;
    const totalChunks = 20;
    let pulledChunks = 0;
    let streamCanceled = false;
    let seenRange: string | null = null;
    let seenSignal: AbortSignal | undefined;

    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
        seenRange = new Headers(init?.headers).get("Range");
        seenSignal = init?.signal ?? undefined;
        const stream = new ReadableStream<Uint8Array>({
          pull(controller) {
            if (pulledChunks >= totalChunks) {
              controller.close();
              return;
            }
            const buf = new Uint8Array(chunkSize).fill(0x61); // 'a'
            // 第 8 个块是字节 [458752, 524288)：把 3 字节汉字 '中' (E4 B8 AD) 放在 524287..524289 跨过 512 KiB 边界
            if (pulledChunks === 7) {
              buf[chunkSize - 1] = 0xe4;
            } else if (pulledChunks === 8) {
              buf[0] = 0xb8;
              buf[1] = 0xad;
            }
            pulledChunks++;
            controller.enqueue(buf);
          },
          cancel() {
            streamCanceled = true;
          },
        });
        return new Response(stream, {
          status: 200,
          headers: { "content-type": "text/plain; charset=utf-8" },
        });
      }),
    );

    const { host, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "chat-attachment",
        key: "chat:huge.log",
        fileName: "huge.log",
        mime: "text/plain",
        size: totalChunks * chunkSize,
        url: "https://example.invalid/v1/files/huge?ticket=abc",
      },
    });

    try {
      await vi.waitFor(() => {
        expect(host.querySelector(".pv-trunc-banner")?.textContent).toContain(
          "文件较大，当前仅展示前 512 KB 内容预览",
        );
      });
      expect(seenRange).toBe("bytes=0-524288");
      expect(seenSignal).toBeInstanceOf(AbortSignal);
      expect(streamCanceled).toBe(true);
      expect(pulledChunks).toBeLessThan(totalChunks);

      const rendered = host.querySelector(".pv-code-lines")?.textContent ?? "";
      expect(rendered.endsWith("\uFFFD")).toBe(false);
      expect(new TextEncoder().encode(rendered).byteLength).toBeLessThanOrEqual(512 * 1024);
    } finally {
      unmount();
    }
  });

  it("handles 206 Partial Content and empty file / 416 responses cleanly", async () => {
    setViewportWidth(1280, 800);
    const seenRanges: Array<string | null> = [];
    let mode: "partial" | "empty-known" | "empty-416" = "partial";

    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
        seenRanges.push(new Headers(init?.headers).get("Range"));
        if (mode === "partial") {
          return new Response("partial-body-preview", {
            status: 206,
            headers: {
              "content-type": "text/plain",
              "content-range": "bytes 0-524288/2097152",
            },
          });
        }
        if (mode === "empty-known") {
          return new Response("", {
            status: 200,
            headers: { "content-type": "text/plain" },
          });
        }
        return new Response(null, {
          status: 416,
          headers: { "content-range": "bytes */0" },
        });
      }),
    );

    const { host, root, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "chat-attachment",
        key: "chat:partial.txt",
        fileName: "partial.txt",
        mime: "text/plain",
        size: 2097152,
        url: "https://example.invalid/v1/files/partial",
      },
    });

    try {
      await vi.waitFor(() => {
        expect(host.querySelector(".pv-trunc-banner")).not.toBeNull();
      });
      expect(host.textContent).toContain("partial-body-preview");
      expect(seenRanges[0]).toBe("bytes=0-524288");

      mode = "empty-known";
      await act(async () => {
        root.render(
          <FilePreview
            variant="chat"
            target={{
              kind: "chat-attachment",
              key: "chat:empty-known.txt",
              fileName: "empty-known.txt",
              mime: "text/plain",
              size: 0,
              url: "https://example.invalid/v1/files/empty-known",
            }}
            onClose={() => {}}
          />,
        );
      });
      await vi.waitFor(() => {
        expect(host.querySelector(".pv-code-lines")).not.toBeNull();
      });
      expect(host.querySelector(".pv-trunc-banner")).toBeNull();
      expect(seenRanges[1]).toBeNull();

      mode = "empty-416";
      await act(async () => {
        root.render(
          <FilePreview
            variant="chat"
            target={{
              kind: "chat-attachment",
              key: "chat:empty-416.txt",
              fileName: "empty-416.txt",
              mime: "text/plain",
              url: "https://example.invalid/v1/files/empty-416",
            }}
            onClose={() => {}}
          />,
        );
      });
      await vi.waitFor(() => {
        expect(host.querySelector(".pv-code-lines")).not.toBeNull();
      });
      expect(host.querySelector(".pv-state-error")).toBeNull();
      expect(host.querySelector(".pv-trunc-banner")).toBeNull();
    } finally {
      unmount();
    }
  });

  it("aborts in-flight chat text preview fetch when switching target or closing", async () => {
    setViewportWidth(1280, 800);
    const signals: AbortSignal[] = [];

    vi.stubGlobal(
      "fetch",
      vi.fn((_url: RequestInfo | URL, init?: RequestInit) => {
        const sig = init?.signal as AbortSignal;
        signals.push(sig);
        return new Promise<Response>(() => {});
      }),
    );

    const { root, unmount } = await mountPreview({
      variant: "chat",
      target: {
        kind: "chat-attachment",
        key: "chat:slow-1.log",
        fileName: "slow-1.log",
        mime: "text/plain",
        size: 1024 * 1024,
        url: "https://example.invalid/v1/files/slow-1",
      },
    });

    try {
      await vi.waitFor(() => {
        expect(signals.length).toBe(1);
      });
      expect(signals[0].aborted).toBe(false);

      await act(async () => {
        root.render(
          <FilePreview
            variant="chat"
            target={{
              kind: "chat-attachment",
              key: "chat:slow-2.log",
              fileName: "slow-2.log",
              mime: "text/plain",
              size: 1024 * 1024,
              url: "https://example.invalid/v1/files/slow-2",
            }}
            onClose={() => {}}
          />,
        );
      });

      await vi.waitFor(() => {
        expect(signals.length).toBe(2);
      });
      expect(signals[0].aborted).toBe(true);
      expect(signals[1].aborted).toBe(false);

      unmount();
      expect(signals[1].aborted).toBe(true);
    } catch (e) {
      unmount();
      throw e;
    }
  });
});
