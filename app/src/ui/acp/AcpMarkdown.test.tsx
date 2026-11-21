// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AcpMarkdown } from "./AcpMarkdown";

const openExternalMock = vi.fn();

vi.mock("../../richText", () => ({
  openExternal: (url: string) => openExternalMock(url),
}));

afterEach(() => {
  openExternalMock.mockReset();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

function renderMd(text: string, streaming = false) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => {
    root.render(<AcpMarkdown text={text} streaming={streaming} />);
  });
  return {
    host,
    unmount: () => {
      act(() => root.unmount());
      host.remove();
    },
  };
}

describe("AcpMarkdown", () => {
  it("renders headings, lists, blockquotes, tables, hr, inline formatting and safe links", () => {
    const md = [
      "# 主标题",
      "",
      "普通段落包含 **加粗**、*斜体*、~~删除线~~、以及 `tool_call_id` 标识符和 _边界斜体_。",
      "",
      "- 项目一",
      "- 项目二 [点击文档](https://example.com/docs)",
      "",
      "1. 第一步",
      "2. 第二步 https://example.org/run",
      "",
      "> 引用块说明",
      "",
      "---",
      "",
      "| 列A | 列B |",
      "| :--- | ---: |",
      "| foo | 42 |",
    ].join("\n");

    const { host, unmount } = renderMd(md);
    try {
      expect(host.querySelector("h1")?.textContent).toBe("主标题");
      expect(host.querySelector("strong")?.textContent).toBe("加粗");
      expect(host.querySelector("del")?.textContent).toBe("删除线");
      const ems = Array.from(host.querySelectorAll("em")).map((e) => e.textContent);
      expect(ems).toEqual(["斜体", "边界斜体"]);
      // tool_call_id 内部下划线不应被拆分成斜体
      expect(host.querySelector(".acp-md-inline-code")?.textContent).toBe("tool_call_id");
      expect(host.querySelectorAll("ul li")).toHaveLength(2);
      expect(host.querySelectorAll("ol li")).toHaveLength(2);
      expect(host.querySelector("blockquote")?.textContent).toContain("引用块说明");
      expect(host.querySelector("hr")).not.toBeNull();
      expect(host.querySelectorAll("table th").length).toBe(2);
      expect(host.querySelector("table td:last-child")?.textContent).toBe("42");

      const links = host.querySelectorAll("a.acp-md-link");
      expect(links).toHaveLength(2);
      act(() => {
        (links[0] as HTMLAnchorElement).click();
        (links[1] as HTMLAnchorElement).click();
      });
      expect(openExternalMock).toHaveBeenCalledWith("https://example.com/docs");
      expect(openExternalMock).toHaveBeenCalledWith("https://example.org/run");
    } finally {
      unmount();
    }
  });

  it("handles closed and unclosed streaming fenced code blocks and copies code", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    const { host, unmount } = renderMd(
      "前缀\n\n```rust\nfn main() {}\n```\n\n尾部\n\n```ts\nconst x = 1",
      true,
    );
    try {
      const codeBlocks = host.querySelectorAll(".acp-md-code-block");
      expect(codeBlocks).toHaveLength(2);
      expect(codeBlocks[0].querySelector(".acp-md-code-lang")?.textContent).toBe("rust");
      expect(codeBlocks[0].querySelector("code")?.textContent).toContain("fn main() {}");
      // 未闭合代码块在流式渲染期间也能正确成块并挂载光标
      expect(codeBlocks[1].querySelector(".acp-md-code-lang")?.textContent).toBe("ts");
      expect(codeBlocks[1].querySelector("code")?.textContent).toContain("const x = 1");
      expect(codeBlocks[1].querySelector(".acp-md-caret")).not.toBeNull();

      const copyBtn = codeBlocks[0].querySelector(".acp-md-copy-btn") as HTMLButtonElement;
      act(() => {
        copyBtn.click();
      });
      expect(writeText).toHaveBeenCalledWith("fn main() {}");
      expect(copyBtn.textContent).toContain("已复制");
    } finally {
      unmount();
    }
  });
});
