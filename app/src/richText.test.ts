import { describe, expect, it } from "vitest";
import { shortUrl, tokenize } from "./richText";

// ===== V2 P1-4 =====
describe("tokenize 链接", () => {
  it("http/https 识别为链接", () => {
    expect(tokenize("见 http://a.com 和 https://b.io/x")).toEqual([
      { t: "text", text: "见 " },
      { t: "link", url: "http://a.com" },
      { t: "text", text: " 和 " },
      { t: "link", url: "https://b.io/x" },
    ]);
  });

  it("中文标点/中文字符立即截断链接", () => {
    expect(tokenize("看这个https://a.com，好的")).toEqual([
      { t: "text", text: "看这个" },
      { t: "link", url: "https://a.com" },
      { t: "text", text: "，好的" },
    ]);
    expect(tokenize("https://a.com好的")).toEqual([
      { t: "link", url: "https://a.com" },
      { t: "text", text: "好的" },
    ]);
  });

  it("空白截断链接", () => {
    expect(tokenize("https://a.com/b c")).toEqual([
      { t: "link", url: "https://a.com/b" },
      { t: "text", text: " c" },
    ]);
  });

  it("末尾英文标点剥掉归普通文本", () => {
    for (const [input, tail] of [
      ["https://a.com.", "."],
      ["https://a.com?", "?"],
      ["https://a.com!", "!"],
      ["https://a.com,", ","],
      ["https://a.com;", ";"],
      ["https://a.com:", ":"],
      [`https://a.com'`, "'"],
      [`https://a.com"`, '"'],
    ] as const) {
      expect(tokenize(input)).toEqual([
        { t: "link", url: "https://a.com" },
        { t: "text", text: tail },
      ]);
    }
    // 连续末尾标点一起剥
    expect(tokenize("https://a.com?!")).toEqual([
      { t: "link", url: "https://a.com" },
      { t: "text", text: "?!" },
    ]);
  });

  it("末尾右括号仅在括号不配对时剥掉", () => {
    expect(tokenize("(见 https://a.com)")).toEqual([
      { t: "text", text: "(见 " },
      { t: "link", url: "https://a.com" },
      { t: "text", text: ")" },
    ]);
    // 维基式配对括号保留
    expect(tokenize("https://en.wikipedia.org/wiki/Foo_(bar)")).toEqual([
      { t: "link", url: "https://en.wikipedia.org/wiki/Foo_(bar)" },
    ]);
    // 配对括号后多出的仍剥
    expect(tokenize("https://a.com/f(x))")).toEqual([
      { t: "link", url: "https://a.com/f(x)" },
      { t: "text", text: ")" },
    ]);
  });

  it("多个链接", () => {
    expect(tokenize("https://a.com 到 https://b.com 再到 https://c.com")).toEqual([
      { t: "link", url: "https://a.com" },
      { t: "text", text: " 到 " },
      { t: "link", url: "https://b.com" },
      { t: "text", text: " 再到 " },
      { t: "link", url: "https://c.com" },
    ]);
  });

  it("链接紧贴 @提及", () => {
    expect(tokenize("@bob 看 https://a.com")).toEqual([
      { t: "mention", text: "@bob" },
      { t: "text", text: " 看 " },
      { t: "link", url: "https://a.com" },
    ]);
    // URL 内的 @ 不切成提及
    expect(tokenize("https://a.com/@bob")).toEqual([
      { t: "link", url: "https://a.com/@bob" },
    ]);
  });

  it("只高亮服务端确认的完整用户名", () => {
    expect(tokenize("@bo @bobby @bob", { mentions: new Set(["bob"]) })).toEqual([
      { t: "text", text: "@bo @bobby " },
      { t: "mention", text: "@bob" },
    ]);
  });

  it("非 http/https 协议不产生链接", () => {
    expect(tokenize("javascript:alert(1)")).toEqual([
      { t: "text", text: "javascript:alert(1)" },
    ]);
    expect(tokenize("看 data:text/html,x 和 ftp://h/f")).toEqual([
      { t: "text", text: "看 data:text/html,x 和 ftp://h/f" },
    ]);
  });

  it("裸协议头不算链接", () => {
    expect(tokenize("https://")).toEqual([{ t: "text", text: "https://" }]);
  });
});

describe("tokenize 行内代码", () => {
  it("`code` 渲染为 code，内部不识别链接和 @", () => {
    expect(tokenize("用 `https://a.com` 和 `@bob` 试")).toEqual([
      { t: "text", text: "用 " },
      { t: "code", text: "https://a.com" },
      { t: "text", text: " 和 " },
      { t: "code", text: "@bob" },
      { t: "text", text: " 试" },
    ]);
  });

  it("未闭合的反引号当普通文本", () => {
    expect(tokenize("这是 `code 没闭合")).toEqual([
      { t: "text", text: "这是 `code 没闭合" },
    ]);
  });
});

describe("tokenize 代码块", () => {
  it("带语言名", () => {
    expect(tokenize("```js\nconst a = 1;\n```")).toEqual([
      { t: "block", lang: "js", code: "const a = 1;" },
    ]);
  });

  it("不带语言名", () => {
    expect(tokenize("```\ncode\n```")).toEqual([
      { t: "block", lang: "", code: "code" },
    ]);
  });

  it("保留缩进与多行", () => {
    expect(tokenize("```\n  a\n    b\n```")).toEqual([
      { t: "block", lang: "", code: "  a\n    b" },
    ]);
  });

  it("未闭合的 ``` 当普通文本", () => {
    expect(tokenize("```js\nnever closed")).toEqual([
      { t: "text", text: "```js\nnever closed" },
    ]);
    expect(tokenize("只有 ``` 一行")).toEqual([
      { t: "text", text: "只有 ``` 一行" },
    ]);
  });

  it("代码块前后有普通文本", () => {
    expect(tokenize("前面\n```py\nx = 1\n```\n后面")).toEqual([
      { t: "text", text: "前面\n" },
      { t: "block", lang: "py", code: "x = 1" },
      { t: "text", text: "\n后面" },
    ]);
  });

  it("块内不识别链接、@、行内代码", () => {
    expect(tokenize("```\n看 https://a.com @bob `x`\n```")).toEqual([
      { t: "block", lang: "", code: "看 https://a.com @bob `x`" },
    ]);
  });

  it("行内代码先于普通文本解析但代码块优先", () => {
    // ``` 未配成块时才轮到行内代码/链接
    expect(tokenize("`code` https://a.com")).toEqual([
      { t: "code", text: "code" },
      { t: "text", text: " " },
      { t: "link", url: "https://a.com" },
    ]);
  });
});

describe("tokenize 边界", () => {
  it("空串", () => {
    expect(tokenize("")).toEqual([]);
  });

  it("纯空白", () => {
    expect(tokenize("  \n ")).toEqual([{ t: "text", text: "  \n " }]);
  });

  it("@提及保持原样（含 @）", () => {
    expect(tokenize("hi @bob_2 在吗")).toEqual([
      { t: "text", text: "hi " },
      { t: "mention", text: "@bob_2" },
      { t: "text", text: " 在吗" },
    ]);
  });
});

describe("shortUrl 显示缩短", () => {
  it("短链接原样", () => {
    expect(shortUrl("https://a.com")).toBe("https://a.com");
  });

  it("超过 60 字符中间省略", () => {
    const u = "https://example.com/" + "x".repeat(60);
    const s = shortUrl(u);
    expect(s.length).toBeLessThan(u.length);
    expect(s).toContain("…");
    expect(s.startsWith("https://example.com/")).toBe(true);
    expect(s.endsWith("xxxx")).toBe(true);
  });
});
