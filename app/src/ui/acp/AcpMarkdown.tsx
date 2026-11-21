import React, { memo, useState } from "react";
import { openExternal } from "../../richText";
import { parseFileReference, type ParsedFileRef } from "../filePathRef";
import { CheckIcon, CopyIcon } from "../icons";

export interface AcpMarkdownProps {
  text: string;
  streaming?: boolean;
  onOpenFilePath?: (ref: ParsedFileRef) => void;
}

type MdBlock =
  | { kind: "code"; lang: string; code: string; unclosed: boolean }
  | { kind: "heading"; level: number; text: string }
  | { kind: "hr" }
  | { kind: "quote"; lines: string[] }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "table"; headers: string[]; aligns: Array<"left" | "center" | "right">; rows: string[][] }
  | { kind: "paragraph"; lines: string[] };

const SAFE_URL_RE = /^https?:\/\/\S+$/i;

function isTableSeparator(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed.includes("-") || !trimmed.includes("|")) return false;
  const cells = splitTableRow(trimmed);
  if (cells.length === 0) return false;
  return cells.every((c) => /^:?-{3,}:?$/.test(c));
}

function splitTableRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) s = s.slice(0, -1);
  return s.split("|").map((c) => c.trim());
}

function parseTableAligns(line: string): Array<"left" | "center" | "right"> {
  return splitTableRow(line).map((cell) => {
    if (cell.startsWith(":") && cell.endsWith(":")) return "center";
    if (cell.endsWith(":")) return "right";
    return "left";
  });
}

function isBlockStart(lines: string[], idx: number): boolean {
  const raw = lines[idx];
  const trimmed = raw.trim();
  if (!trimmed) return true;
  if (/^(?:```|~~~)/.test(trimmed)) return true;
  if (/^#{1,6}\s+/.test(trimmed)) return true;
  if (/^(?:---+|___+|\*\*\*+)$/.test(trimmed)) return true;
  if (/^>\s?/.test(trimmed)) return true;
  if (/^\s*(?:[-*+]|\d+[.)])\s+/.test(raw)) return true;
  if (idx + 1 < lines.length && trimmed.includes("|") && isTableSeparator(lines[idx + 1])) {
    return true;
  }
  return false;
}

function parseMarkdownBlocks(rawText: string): MdBlock[] {
  const text = rawText.replace(/\r\n?/g, "\n");
  const lines = text.split("\n");
  const blocks: MdBlock[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();

    if (!trimmed) {
      i++;
      continue;
    }

    const fenceMatch = /^(`{3,}|~{3,})\s*([^\s`~]*)\s*$/.exec(trimmed);
    if (fenceMatch) {
      const fence = fenceMatch[1];
      const fenceChar = fence[0];
      const lang = fenceMatch[2] ?? "";
      const codeLines: string[] = [];
      let closed = false;
      i++;
      while (i < lines.length) {
        const curTrim = lines[i].trim();
        if (
          curTrim.startsWith(fenceChar.repeat(fence.length)) &&
          new RegExp(`^${fenceChar}{${fence.length},}\\s*$`).test(curTrim)
        ) {
          closed = true;
          i++;
          break;
        }
        codeLines.push(lines[i]);
        i++;
      }
      blocks.push({ kind: "code", lang, code: codeLines.join("\n"), unclosed: !closed });
      continue;
    }

    const headingMatch = /^(#{1,6})\s+(.+)$/.exec(trimmed);
    if (headingMatch) {
      blocks.push({
        kind: "heading",
        level: headingMatch[1].length,
        text: headingMatch[2].replace(/\s+#+\s*$/, ""),
      });
      i++;
      continue;
    }

    if (/^(?:---+|___+|\*\*\*+)$/.test(trimmed)) {
      blocks.push({ kind: "hr" });
      i++;
      continue;
    }

    if (trimmed.startsWith(">")) {
      const quoteLines: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith(">")) {
        quoteLines.push(lines[i].trim().replace(/^>\s?/, ""));
        i++;
      }
      blocks.push({ kind: "quote", lines: quoteLines });
      continue;
    }

    if (
      i + 1 < lines.length &&
      trimmed.includes("|") &&
      isTableSeparator(lines[i + 1])
    ) {
      const headers = splitTableRow(line);
      const aligns = parseTableAligns(lines[i + 1]);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length) {
        const rowTrim = lines[i].trim();
        if (!rowTrim || !rowTrim.includes("|")) break;
        rows.push(splitTableRow(lines[i]));
        i++;
      }
      blocks.push({ kind: "table", headers, aligns, rows });
      continue;
    }

    const ulMatch = /^\s*[-*+]\s+(.+)$/.exec(line);
    const olMatch = /^\s*\d+[.)]\s+(.+)$/.exec(line);
    if (ulMatch || olMatch) {
      const ordered = Boolean(olMatch);
      const items: string[] = [];
      const itemRe = ordered ? /^\s*\d+[.)]\s+(.+)$/ : /^\s*[-*+]\s+(.+)$/;
      while (i < lines.length) {
        const m = itemRe.exec(lines[i]);
        if (!m) break;
        items.push(m[1]);
        i++;
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    const paraLines: string[] = [line];
    i++;
    while (i < lines.length && !isBlockStart(lines, i)) {
      paraLines.push(lines[i]);
      i++;
    }
    blocks.push({ kind: "paragraph", lines: paraLines });
  }

  return blocks;
}

function renderInline(
  text: string,
  keyPrefix: string,
  onOpenFilePath?: (ref: ParsedFileRef) => void,
  depth = 0,
): React.ReactNode[] {
  if (!text || depth > 4) return [text];
  const out: React.ReactNode[] = [];
  let pos = 0;
  let tokenIdx = 0;

  // 匹配行内代码、Markdown 链接（含 https?://、file:// 与相对/绝对文件地址）、加粗、删除线、斜体或裸 HTTP(S)/file 链接；_斜体_ 要求两侧为非字母数字边界以避免拆散 snake_case
  const tokenRe =
    /`([^`\n]+)`|\[([^\]]+)\]\(([^\s)]+)\)|\*\*([^\n*](?:[\s\S]*?[^\n*])?)\*\*|~~([^\n~][\s\S]*?)~~|(?<![A-Za-z0-9_])_([^\n_][\s\S]*?)_(?![A-Za-z0-9_])|\*([^\n*][\s\S]*?)\*|((?:https?|file):\/\/[^\s<>"'`)\]]+)/g;

  let m: RegExpExecArray | null;
  while ((m = tokenRe.exec(text)) !== null) {
    if (m.index > pos) {
      out.push(text.slice(pos, m.index));
    }
    const k = `${keyPrefix}-${tokenIdx++}`;
    if (m[1] !== undefined) {
      const codeStr = m[1];
      const fileRef = onOpenFilePath ? parseFileReference(codeStr) : null;
      if (fileRef && onOpenFilePath) {
        out.push(
          <button
            key={k}
            type="button"
            className="acp-md-file-code-btn"
            title={`预览文件：${fileRef.path}${fileRef.lineStart ? ` (L${fileRef.lineStart})` : ""}`}
            onClick={(e) => {
              e.stopPropagation();
              onOpenFilePath(fileRef);
            }}
          >
            <code className="acp-md-inline-code acp-md-file-code">{codeStr}</code>
          </button>,
        );
      } else {
        out.push(
          <code key={k} className="acp-md-inline-code">
            {codeStr}
          </code>,
        );
      }
    } else if (m[2] !== undefined && m[3] !== undefined) {
      const url = m[3];
      if (SAFE_URL_RE.test(url)) {
        out.push(
          <a
            key={k}
            href={url}
            className="acp-md-link"
            onClick={(e) => {
              e.preventDefault();
              void openExternal(url);
            }}
          >
            {renderInline(m[2], k, onOpenFilePath, depth + 1)}
          </a>,
        );
      } else {
        const fileRef = onOpenFilePath
          ? parseFileReference(url, { fromMarkdownLink: true })
          : null;
        if (fileRef && onOpenFilePath) {
          out.push(
            <a
              key={k}
              href={`#file-preview:${encodeURIComponent(fileRef.path)}`}
              className="acp-md-link acp-md-file-link"
              title={`预览文件：${fileRef.path}${fileRef.lineStart ? ` (L${fileRef.lineStart})` : ""}`}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onOpenFilePath(fileRef);
              }}
            >
              {renderInline(m[2], k, onOpenFilePath, depth + 1)}
            </a>,
          );
        } else {
          out.push(m[0]);
        }
      }
    } else if (m[4] !== undefined) {
      out.push(<strong key={k}>{renderInline(m[4], k, onOpenFilePath, depth + 1)}</strong>);
    } else if (m[5] !== undefined) {
      out.push(<del key={k}>{renderInline(m[5], k, onOpenFilePath, depth + 1)}</del>);
    } else if (m[6] !== undefined) {
      out.push(<em key={k}>{renderInline(m[6], k, onOpenFilePath, depth + 1)}</em>);
    } else if (m[7] !== undefined) {
      out.push(<em key={k}>{renderInline(m[7], k, onOpenFilePath, depth + 1)}</em>);
    } else if (m[8] !== undefined) {
      const rawUrl = m[8];
      const cleaned = rawUrl.replace(/[.,;:!?。，；：！？、）》】」』]+$/, "");
      const trailing = rawUrl.slice(cleaned.length);
      if (/^file:\/\//i.test(cleaned)) {
        const fileRef = onOpenFilePath ? parseFileReference(cleaned) : null;
        if (fileRef && onOpenFilePath) {
          out.push(
            <a
              key={k}
              href={`#file-preview:${encodeURIComponent(fileRef.path)}`}
              className="acp-md-link acp-md-file-link"
              title={`预览文件：${fileRef.path}`}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onOpenFilePath(fileRef);
              }}
            >
              {cleaned}
            </a>,
          );
          if (trailing) out.push(trailing);
        } else {
          out.push(rawUrl);
        }
      } else {
        out.push(
          <a
            key={k}
            href={cleaned}
            className="acp-md-link"
            onClick={(e) => {
              e.preventDefault();
              void openExternal(cleaned);
            }}
          >
            {cleaned}
          </a>,
        );
        if (trailing) out.push(trailing);
      }
    }
    pos = m.index + m[0].length;
  }

  if (pos < text.length) {
    out.push(text.slice(pos));
  }
  return out;
}

function CodeBlockCard({
  lang,
  code,
  unclosed,
  streaming,
}: {
  lang: string;
  code: string;
  unclosed: boolean;
  streaming: boolean;
}) {
  const [copied, setCopied] = useState(false);

  const onCopy = () => {
    try {
      void navigator.clipboard?.writeText(code);
    } catch {}
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="acp-md-code-block">
      <div className="acp-md-code-head">
        <span className="acp-md-code-lang">{lang || "text"}</span>
        <button
          type="button"
          className="acp-md-copy-btn"
          onClick={onCopy}
          aria-label="复制代码"
        >
          {copied ? <CheckIcon size={12} /> : <CopyIcon />}
          <span>{copied ? "已复制" : "复制"}</span>
        </button>
      </div>
      <pre className="acp-md-code-pre">
        <code>
          {code}
          {unclosed && streaming ? <span className="acp-md-caret" aria-hidden="true" /> : null}
        </code>
      </pre>
    </div>
  );
}

export const AcpMarkdown = memo(function AcpMarkdown({
  text,
  streaming = false,
  onOpenFilePath,
}: AcpMarkdownProps) {
  const blocks = parseMarkdownBlocks(text);
  if (blocks.length === 0) {
    return streaming ? (
      <div className="acp-md">
        <span className="acp-md-caret" aria-hidden="true" />
      </div>
    ) : null;
  }

  const lastIdx = blocks.length - 1;
  const lastBlock = blocks[lastIdx];
  const caretInsideCode = lastBlock?.kind === "code" && lastBlock.unclosed;

  return (
    <div className="acp-md">
      {blocks.map((b, idx) => {
        const isLast = idx === lastIdx;
        const showTrailingCaret = streaming && isLast && !caretInsideCode;
        switch (b.kind) {
          case "code":
            return (
              <CodeBlockCard
                key={idx}
                lang={b.lang}
                code={b.code}
                unclosed={b.unclosed}
                streaming={streaming && isLast}
              />
            );
          case "heading": {
            const Tag = (`h${Math.min(Math.max(b.level, 1), 6)}` as
              | "h1"
              | "h2"
              | "h3"
              | "h4"
              | "h5"
              | "h6");
            return (
              <Tag key={idx} className={`acp-md-h acp-md-h${Math.min(b.level, 4)}`}>
                {renderInline(b.text, `h${idx}`, onOpenFilePath)}
                {showTrailingCaret ? <span className="acp-md-caret" aria-hidden="true" /> : null}
              </Tag>
            );
          }
          case "hr":
            return <hr key={idx} className="acp-md-hr" />;
          case "quote":
            return (
              <blockquote key={idx} className="acp-md-quote">
                {b.lines.map((line, li) => (
                  <p key={li}>
                    {renderInline(line, `q${idx}-${li}`, onOpenFilePath)}
                    {showTrailingCaret && li === b.lines.length - 1 ? (
                      <span className="acp-md-caret" aria-hidden="true" />
                    ) : null}
                  </p>
                ))}
              </blockquote>
            );
          case "list": {
            const ListTag = b.ordered ? "ol" : "ul";
            return (
              <ListTag key={idx} className="acp-md-list">
                {b.items.map((item, li) => (
                  <li key={li}>
                    {renderInline(item, `l${idx}-${li}`, onOpenFilePath)}
                    {showTrailingCaret && li === b.items.length - 1 ? (
                      <span className="acp-md-caret" aria-hidden="true" />
                    ) : null}
                  </li>
                ))}
              </ListTag>
            );
          }
          case "table":
            return (
              <div key={idx} className="acp-md-table-wrap">
                <table className="acp-md-table">
                  <thead>
                    <tr>
                      {b.headers.map((h, ci) => (
                        <th key={ci} style={{ textAlign: b.aligns[ci] ?? "left" }}>
                          {renderInline(h, `th${idx}-${ci}`, onOpenFilePath)}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {b.rows.map((row, ri) => (
                      <tr key={ri}>
                        {b.headers.map((_, ci) => (
                          <td key={ci} style={{ textAlign: b.aligns[ci] ?? "left" }}>
                            {renderInline(row[ci] ?? "", `td${idx}-${ri}-${ci}`, onOpenFilePath)}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case "paragraph":
            return (
              <p key={idx} className="acp-md-p">
                {b.lines.map((line, li) => (
                  <React.Fragment key={li}>
                    {li > 0 ? <br /> : null}
                    {renderInline(line, `p${idx}-${li}`, onOpenFilePath)}
                  </React.Fragment>
                ))}
                {showTrailingCaret ? <span className="acp-md-caret" aria-hidden="true" /> : null}
              </p>
            );
        }
      })}
    </div>
  );
});
