import { Fragment, useRef, useState } from "react";

// ===== V2 P1-4 =====
// 消息正文富文本渲染：代码块 → 行内代码 → 链接/@ 三层切分，全部产出 React 元素。
export type Token =
  | { t: "text"; text: string }
  | { t: "link"; url: string }
  | { t: "code"; text: string }
  | { t: "block"; lang: string; code: string }
  | { t: "mention"; text: string };

/** 链接只由 ASCII 可见字符组成；空白、中文/中文标点天然终止匹配 */
const RE_LINK = /https?:\/\/[\x21-\x7e]+/g;
/** 与旧 renderContent 一致的 @ 提及规则 */
const RE_MENTION = /@[A-Za-z0-9_]+/g;
/** 链接末尾固定剥掉的英文标点 */
const TRIM_TAIL = new Set([...".,;:!?'\""]);

const countChar = (s: string, c: string) => s.split(c).length - 1;

/** 剥掉链接末尾标点；')' 只在链接内括号不配对时才剥 */
function trimUrl(u: string): string {
  while (u.length) {
    const last = u[u.length - 1];
    if (TRIM_TAIL.has(last)) u = u.slice(0, -1);
    else if (last === ")" && countChar(u, ")") > countChar(u, "(")) u = u.slice(0, -1);
    else break;
  }
  return u;
}

/** 显示文本过长时中间省略；title 属性仍放完整地址 */
export function shortUrl(u: string): string {
  if (u.length <= 60) return u;
  return `${u.slice(0, 36)}…${u.slice(-20)}`;
}

/** 先切代码块：``` 起、同一行余下为语言名、下一对 ``` 收尾；未闭合当普通文本 */
function splitBlocks(text: string): ({ t: "raw"; text: string } | Extract<Token, { t: "block" }>)[] {
  const out: ({ t: "raw"; text: string } | Extract<Token, { t: "block" }>)[] = [];
  let i = 0;
  let buf = 0;
  while (i < text.length) {
    if (text.startsWith("```", i)) {
      const nl = text.indexOf("\n", i + 3);
      if (nl !== -1) {
        const close = text.indexOf("```", nl + 1);
        if (close !== -1) {
          if (i > buf) out.push({ t: "raw", text: text.slice(buf, i) });
          out.push({
            t: "block",
            lang: text.slice(i + 3, nl).trim(),
            code: text.slice(nl + 1, close).replace(/\n$/, ""),
          });
          i = buf = close + 3;
          continue;
        }
      }
    }
    i++;
  }
  if (buf < text.length) out.push({ t: "raw", text: text.slice(buf) });
  return out;
}

/** 再切行内代码：`x` 非空内容才生效；残留的 ``` 保持字面量 */
function splitInline(text: string): ({ t: "raw"; text: string } | Extract<Token, { t: "code" }>)[] {
  const out: ({ t: "raw"; text: string } | Extract<Token, { t: "code" }>)[] = [];
  let i = 0;
  let buf = 0;
  while (i < text.length) {
    if (text[i] === "`" && !text.startsWith("```", i)) {
      const end = text.indexOf("`", i + 1);
      if (end > i + 1) {
        if (i > buf) out.push({ t: "raw", text: text.slice(buf, i) });
        out.push({ t: "code", text: text.slice(i + 1, end) });
        i = buf = end + 1;
        continue;
      }
    }
    i++;
  }
  if (buf < text.length) out.push({ t: "raw", text: text.slice(buf) });
  return out;
}

type MentionNames = boolean | ReadonlySet<string>;

/** 最后切链接与 @（仅普通文本段）；mentions=false 时 @ 按纯文本处理（MF2 记录/转发文本） */
function splitRich(text: string, mentions: MentionNames): Token[] {
  const out: Token[] = [];
  const pushMentions = (s: string) => {
    if (mentions === false) {
      if (s) out.push({ t: "text", text: s });
      return;
    }
    let p = 0;
    for (const m of s.matchAll(RE_MENTION)) {
      if (mentions !== true && !mentions.has(m[0].slice(1))) continue;
      if (m.index > p) out.push({ t: "text", text: s.slice(p, m.index) });
      out.push({ t: "mention", text: m[0] });
      p = m.index + m[0].length;
    }
    if (p < s.length) out.push({ t: "text", text: s.slice(p) });
  };
  let pos = 0;
  for (const m of text.matchAll(RE_LINK)) {
    const start = m.index;
    pushMentions(text.slice(pos, start));
    const url = trimUrl(m[0]);
    const schemeLen = url.startsWith("https:") ? 8 : 7;
    if (url.length > schemeLen) {
      out.push({ t: "link", url });
      pos = start + url.length; // 剥掉的末尾标点留给下一段普通文本
    } else {
      out.push({ t: "text", text: m[0] });
      pos = start + m[0].length;
    }
  }
  pushMentions(text.slice(pos));
  return out;
}

export function tokenize(text: string, opts: { mentions?: MentionNames } = {}): Token[] {
  const out: Token[] = [];
  const push = (t: Token) => {
    const last = out[out.length - 1];
    if (t.t === "text") {
      if (t.text === "") return;
      if (last?.t === "text") last.text += t.text;
      else out.push(t);
    } else {
      out.push(t);
    }
  };
  for (const seg of splitBlocks(text)) {
    if (seg.t === "block") {
      push(seg);
      continue;
    }
    for (const s2 of splitInline(seg.text)) {
      if (s2.t === "code") {
        push(s2);
        continue;
      }
      for (const s3 of splitRich(s2.text, opts.mentions ?? true)) push(s3);
    }
  }
  return out;
}

// ---------- 渲染 ----------

const isTauri = () => "__TAURI_INTERNALS__" in window;

/** Tauri 壳内用系统浏览器打开外链（WebView 不跳转），插件失败再退回 window.open */
export function openExternal(url: string) {
  void import("@tauri-apps/plugin-opener")
    .then((m) => m.openUrl(url))
    .catch(() => window.open(url, "_blank", "noopener,noreferrer"));
}

function CodeBlock({ lang, code }: { lang: string; code: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const copy = async () => {
    let ok = true;
    try {
      await navigator.clipboard.writeText(code);
    } catch {
      // 非安全上下文（LAN http）退化到 execCommand
      const ta = document.createElement("textarea");
      ta.value = code;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        ok = document.execCommand("copy");
      } catch {
        ok = false;
      }
      ta.remove();
    }
    if (!ok) return;
    setCopied(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(false), 1500);
  };
  return (
    <div className="rt-block">
      <div className="rt-block-head">
        {lang && <span className="rt-lang">{lang}</span>}
        <button type="button" className="rt-copy" onClick={() => void copy()}>
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  );
}

export default function RichText({ text, mentions = false }: { text: string; mentions?: MentionNames }) {
  return (
    <>
      {tokenize(text, { mentions }).map((tok, i) => {
        switch (tok.t) {
          case "link":
            return (
              <a
                key={i}
                className="rt-link"
                href={tok.url}
                title={tok.url}
                target="_blank"
                rel="noopener noreferrer"
                onClick={(e) => {
                  if (!isTauri()) return;
                  e.preventDefault();
                  openExternal(tok.url);
                }}
              >
                {shortUrl(tok.url)}
              </a>
            );
          case "code":
            return (
              <code key={i} className="rt-code">
                {tok.text}
              </code>
            );
          case "block":
            return <CodeBlock key={i} lang={tok.lang} code={tok.code} />;
          case "mention":
            return (
              <span key={i} className="mt">
                {tok.text}
              </span>
            );
          default:
            return <Fragment key={i}>{tok.text}</Fragment>;
        }
      })}
    </>
  );
}
