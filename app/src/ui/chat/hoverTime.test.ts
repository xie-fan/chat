// @vitest-environment happy-dom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

// HS1：悬停时间必须是脱离文档流的绝对定位覆盖层，
// 悬停显隐永远不改变消息行/列表的布局高度。
const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
const chatView = readFileSync(resolve(process.cwd(), "src/ui/chat/ChatView.tsx"), "utf8");

const mountRow = (mine = false) => {
  document.body.innerHTML = `
    <div class="msg-row${mine ? " mine" : ""}">
      <div class="msg-body">
        <div class="bubble-wrap">
          <div class="msg-actions" data-testid="msg-hover-actions"></div>
          <div class="bubble">hi</div>
          <div class="msg-time" data-testid="msg-hover-time">12:00</div>
        </div>
      </div>
    </div>`;
};

// 所有选择器命中 .msg-time 的规则（含 :hover / .mine 变体；先剥注释）
const msgTimeRules = () =>
  [...css.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{([^{}]+)\}/g)]
    .map(([, sel, body]) => ({ sel: sel.trim(), body }))
    .filter((r) => r.sel.split(",").some((s) => /\.msg-time\b/.test(s.trim())));

beforeAll(() => {
  const style = document.createElement("style");
  style.textContent = css;
  document.head.appendChild(style);
});

describe("悬停时间零布局位移（HS1）", () => {
  it(".msg-time 绝对定位于气泡外侧下方，默认隐藏", () => {
    mountRow();
    const t = document.querySelector(".msg-time")!;
    const cs = getComputedStyle(t);
    expect(cs.position).toBe("absolute");
    expect(cs.top).toBe("100%");
    expect(cs.display).toBe("none");
    // 覆盖层悬在下一行上方时不抢指针事件
    expect(cs.pointerEvents).toBe("none");
  });

  it("自己的消息时间贴气泡右缘（与他人左侧镜像）", () => {
    mountRow(true);
    const cs = getComputedStyle(document.querySelector(".msg-time")!);
    expect(cs.position).toBe("absolute");
    expect(cs.right).toBe("0px");
    expect(cs.left).toBe("auto");
  });

  it("悬停动作条仍在文档流外", () => {
    mountRow();
    expect(getComputedStyle(document.querySelector(".msg-actions")!).position).toBe("absolute");
  });

  it("bubble-wrap 提供定位上下文", () => {
    mountRow();
    expect(getComputedStyle(document.querySelector(".bubble-wrap")!).position).toBe("relative");
  });

  it("显示状态下（悬停 display:block 的效果）仍保持 absolute", () => {
    mountRow();
    const t = document.querySelector<HTMLElement>(".msg-time")!;
    t.style.display = "block"; // 悬停规则只切 display，这里直接验证显示态
    expect(getComputedStyle(t).position).toBe("absolute");
  });

  it("样式表存在悬停显示规则，且所有 .msg-time 规则都不把它放回文档流", () => {
    const rules = msgTimeRules();
    expect(rules.length).toBeGreaterThan(0);
    // 悬停规则负责显示（display:block）
    expect(
      rules.some((r) => r.sel.includes(":hover") && /display\s*:\s*block/.test(r.body)),
    ).toBe(true);
    // 任何命中规则都不得声明会把元素放回文档流的 position 值
    for (const r of rules) {
      const pos = /position\s*:\s*(\w+)/.exec(r.body)?.[1];
      if (pos) expect(pos).toBe("absolute");
    }
  });

  it("真实 ChatView 里 .msg-time 仍在 .bubble-wrap 内（定位上下文契约）", () => {
    expect(/className="bubble-wrap"[\s\S]*?data-testid="msg-hover-time"/.test(chatView)).toBe(true);
    expect(/className="msg-time"[\s\S]*?data-testid="msg-hover-time"/.test(chatView)).toBe(true);
  });
});
