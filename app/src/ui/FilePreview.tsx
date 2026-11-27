import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { copyPlainText } from "../clipboard";
import type { AcpAttachment, AcpHostFileContent } from "../types";
import { AcpMarkdown } from "./acp/AcpMarkdown";
import { Z } from "./chat/layers";
import {
  CheckIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CodeIcon,
  CopyIcon,
  DownloadIcon,
  FileTextIcon,
  ImageIcon,
  XIcon,
} from "./icons";

export type FilePreviewTarget =
  | {
      kind: "chat-attachment";
      key: string;
      fileName: string;
      mime: string;
      size?: number;
      badge?: string;
      /** 已准备好的媒体地址，例如撤回件草稿的 Object URL。 */
      url?: string;
      /** 预览打开时为会话附件签发地址。 */
      loadUrl?: () => Promise<string>;
      /** 需鉴权或本地异步获取的 Blob（如撤回保留件） */
      loadBlob?: () => Promise<Blob>;
    }
  | {
      kind: "local-file";
      key: string;
      file: File;
      badge?: string;
    }
  | {
      kind: "acp-attachment";
      key: string;
      att: AcpAttachment;
      badge?: string;
      loadBlob: () => Promise<Blob>;
      onDownload?: () => void;
    }
  | {
      kind: "host-path";
      key: string;
      path: string;
      cwd?: string;
      hostName?: string;
      lineStart?: number;
      lineEnd?: number;
      badge?: string;
      loadHostFile: () => Promise<AcpHostFileContent>;
    };

export type PreviewViewKind =
  | "image"
  | "markdown"
  | "csv"
  | "code"
  | "pdf"
  | "audio"
  | "video"
  | "unsupported";

export function fmtPreviewSize(n?: number | null): string {
  if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return "";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

const TEXT_EXT_RE =
  /\.(txt|md|mdx|markdown|log|json|jsonc|json5|jsonl|js|jsx|mjs|cjs|ts|tsx|py|pyi|rs|csv|tsv|ya?ml|xml|html?|css|scss|sass|less|vue|svelte|astro|sh|bash|zsh|fish|bat|cmd|ps1|c|h|cc|cpp|hpp|cs|java|kt|kts|swift|go|mod|sum|rb|php|lua|zig|toml|lock|ini|cfg|conf|env|sql|graphql|gql|proto|dockerfile|gradle|plist)$/i;

const EXACT_TEXT_NAMES = new Set([
  "dockerfile",
  "makefile",
  "gnumakefile",
  "justfile",
  "cmakelists.txt",
  ".gitignore",
  ".dockerignore",
  ".editorconfig",
  ".env",
  ".env.example",
  ".env.local",
]);

export function classifyPreviewKind(fileName: string, mime: string): PreviewViewKind {
  const m = (mime || "").toLowerCase().split(";")[0].trim();
  const lowName = (fileName || "").trim().toLowerCase();
  const baseName = lowName.split(/[\\/]/).pop() ?? lowName;

  if (m === "image/svg+xml" || lowName.endsWith(".svg")) return "image";
  if (m.startsWith("image/") || /\.(png|jpe?g|gif|webp|bmp|ico|avif)$/i.test(lowName)) {
    return "image";
  }
  if (m === "application/pdf" || lowName.endsWith(".pdf")) return "pdf";
  if (m.startsWith("audio/") || /\.(mp3|wav|ogg|m4a|aac|flac|opus)$/i.test(lowName)) {
    return "audio";
  }
  if (m.startsWith("video/") || /\.(mp4|webm|mov|m4v)$/i.test(lowName)) {
    return "video";
  }
  if (m === "text/markdown" || /\.(md|mdx|markdown)$/i.test(lowName)) {
    return "markdown";
  }
  if (
    m === "text/csv" ||
    m === "text/tab-separated-values" ||
    /\.(csv|tsv)$/i.test(lowName)
  ) {
    return "csv";
  }
  if (
    m.startsWith("text/") ||
    m === "application/json" ||
    m === "application/xml" ||
    m === "application/javascript" ||
    m === "application/typescript" ||
    TEXT_EXT_RE.test(lowName) ||
    EXACT_TEXT_NAMES.has(baseName)
  ) {
    return "code";
  }
  return "unsupported";
}

export function checkIsMobileViewport(win: Window = window): boolean {
  if (win.innerWidth > 0) {
    if (win.innerWidth <= 720) return true;
  } else if (typeof win.matchMedia === "function") {
    if (win.matchMedia("(max-width: 720px)").matches) return true;
  }
  if (typeof win.matchMedia === "function") {
    if (win.matchMedia("(max-height: 520px) and (pointer: coarse)").matches) {
      return true;
    }
  }
  return false;
}

export function useIsMobilePreview(): boolean {
  const [isMobile, setIsMobile] = useState(() =>
    typeof window !== "undefined" ? checkIsMobileViewport(window) : false,
  );

  useEffect(() => {
    if (typeof window === "undefined") return;
    const sync = () => setIsMobile(checkIsMobileViewport(window));
    sync();
    window.addEventListener("resize", sync);
    const mqWidth = window.matchMedia?.("(max-width: 720px)");
    const mqLand = window.matchMedia?.("(max-height: 520px) and (pointer: coarse)");
    mqWidth?.addEventListener?.("change", sync);
    mqLand?.addEventListener?.("change", sync);
    return () => {
      window.removeEventListener("resize", sync);
      mqWidth?.removeEventListener?.("change", sync);
      mqLand?.removeEventListener?.("change", sync);
    };
  }, []);

  return isMobile;
}

/** 移动端展开态右滑收起判定：满足位移阈值或快速右滑速度，且未反向左滑取消 */
export function shouldCollapseOnSwipeRight(
  dx: number,
  vx: number,
  drawerWidth: number,
): boolean {
  const w = drawerWidth > 0 ? drawerWidth : 320;
  if (vx < -0.25) return false;
  const threshold = Math.min(80, Math.max(36, w * 0.22));
  return dx >= threshold || (dx > 18 && vx >= 0.35);
}

/** 移动端收起态左滑展开判定：从右侧边缘向左滑动满足阈值或快速左滑速度 */
export function shouldExpandOnSwipeLeft(
  dx: number,
  vx: number,
  drawerWidth: number,
): boolean {
  const w = drawerWidth > 0 ? drawerWidth : 320;
  if (vx > 0.25) return false;
  const leftDist = -dx;
  const threshold = Math.min(56, Math.max(28, w * 0.16));
  return leftDist >= threshold || (leftDist > 16 && vx <= -0.3);
}

function parseDelimitedTable(raw: string, delimiter: "," | "\t", maxRows = 300): string[][] {
  const rows: string[][] = [];
  let curRow: string[] = [];
  let curCell = "";
  let inQuotes = false;
  let i = 0;

  while (i < raw.length && rows.length < maxRows) {
    const ch = raw[i];
    if (inQuotes) {
      if (ch === '"') {
        if (raw[i + 1] === '"') {
          curCell += '"';
          i += 2;
          continue;
        }
        inQuotes = false;
        i++;
        continue;
      }
      curCell += ch;
      i++;
      continue;
    }

    if (ch === '"' && curCell.length === 0) {
      inQuotes = true;
      i++;
      continue;
    }
    if (ch === delimiter) {
      curRow.push(curCell);
      curCell = "";
      i++;
      continue;
    }
    if (ch === "\r" || ch === "\n") {
      if (ch === "\r" && raw[i + 1] === "\n") i++;
      curRow.push(curCell);
      curCell = "";
      if (curRow.length > 1 || curRow[0] !== "") {
        rows.push(curRow);
      }
      curRow = [];
      i++;
      continue;
    }
    curCell += ch;
    i++;
  }
  if ((curCell.length > 0 || curRow.length > 0) && rows.length < maxRows) {
    curRow.push(curCell);
    rows.push(curRow);
  }
  return rows;
}

function base64ToBlob(b64: string, mime: string): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return new Blob([bytes], { type: mime || "application/octet-stream" });
}

function triggerBlobDownload(blob: Blob, fileName: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = fileName || "download";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function canScrollLeftInside(target: EventTarget | null, boundary: HTMLElement | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  let el: HTMLElement | null = target;
  while (el && el !== boundary) {
    if (el.scrollWidth > el.clientWidth + 2 && el.scrollLeft > 0) {
      return true;
    }
    el = el.parentElement;
  }
  return false;
}

interface LoadedData {
  status: "loading" | "ready" | "error";
  fileName: string;
  mime: string;
  size?: number;
  resolvedPath?: string;
  viewKind: PreviewViewKind;
  mediaUrl?: string;
  textContent?: string;
  downloadBlob?: Blob;
  truncated?: boolean;
  unsupportedReason?: string;
  errorMsg?: string;
}

const MAX_CLIENT_TEXT_BYTES = 512 * 1024;

async function readLimitedText(
  url: string,
  maxBytes: number,
  signal: AbortSignal,
  knownSize?: number,
): Promise<{ text: string; truncated: boolean }> {
  const canUseRange =
    knownSize !== 0 && !url.startsWith("blob:") && !url.startsWith("data:");
  let resp = await fetch(url, {
    cache: "no-store",
    referrerPolicy: "no-referrer",
    signal,
    ...(canUseRange ? { headers: { Range: `bytes=0-${maxBytes}` } } : {}),
  });

  if (resp.status === 416) {
    const contentRange = resp.headers.get("content-range") ?? "";
    if (knownSize === 0 || /bytes\s+\*\/0\b/i.test(contentRange)) {
      return { text: "", truncated: false };
    }
    resp = await fetch(url, {
      cache: "no-store",
      referrerPolicy: "no-referrer",
      signal,
    });
  }

  if (!resp.ok) {
    throw new Error(`加载失败 (${resp.status})`);
  }

  const crMatch = /\/(\d+)\s*$/.exec(resp.headers.get("content-range") ?? "");
  const contentTotal = crMatch ? Number(crMatch[1]) : undefined;

  let merged: Uint8Array;
  let exceeded = false;

  if (resp.body) {
    const reader = resp.body.getReader();
    const onAbort = () => {
      void reader.cancel().catch(() => {});
    };
    signal.addEventListener("abort", onAbort, { once: true });

    const chunks: Uint8Array[] = [];
    let kept = 0;
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!value || value.byteLength === 0) continue;
        if (kept + value.byteLength > maxBytes) {
          if (kept < maxBytes) {
            chunks.push(value.subarray(0, maxBytes - kept));
            kept = maxBytes;
          }
          exceeded = true;
          await reader.cancel().catch(() => {});
          break;
        }
        chunks.push(value);
        kept += value.byteLength;
      }
    } finally {
      signal.removeEventListener("abort", onAbort);
      reader.releaseLock();
    }

    merged = new Uint8Array(kept);
    let offset = 0;
    for (const chunk of chunks) {
      merged.set(chunk, offset);
      offset += chunk.byteLength;
    }
  } else {
    const raw = new Uint8Array(await resp.arrayBuffer());
    exceeded = raw.byteLength > maxBytes;
    merged = exceeded ? raw.subarray(0, maxBytes) : raw;
  }

  const truncated =
    exceeded ||
    (contentTotal !== undefined && contentTotal > maxBytes) ||
    (resp.status === 206 && knownSize !== undefined && knownSize > maxBytes);
  const text = new TextDecoder("utf-8").decode(merged, { stream: truncated });
  return { text, truncated };
}

export interface FilePreviewProps {
  variant: "chat" | "agent";
  target: FilePreviewTarget;
  /** 每次用户主动点击打开文件时递增，确保移动端处于收起态时重新展开 */
  openSeq?: number;
  onClose: () => void;
  /** 供外层追加业务按钮（如聊天中的 回复/转发/撤回/下载） */
  toolbarActions?: React.ReactNode;
  /** 聊天触屏幽灵点击拦截器：返回 true 时忽略刚打开瞬间的穿透点击 */
  shouldIgnoreClick?: () => boolean;
  /** 移动端收起/展开态变化回调（收起时不拦截全局返回键） */
  onMobileCollapsedChange?: (collapsed: boolean) => void;
}

export default function FilePreview({
  variant,
  target,
  openSeq = 0,
  onClose,
  toolbarActions,
  shouldIgnoreClick,
  onMobileCollapsedChange,
}: FilePreviewProps) {
  const isMobile = useIsMobilePreview();
  const layoutMode: "desktop-modal" | "desktop-sidebar" | "mobile-drawer" = isMobile
    ? "mobile-drawer"
    : variant === "agent"
      ? "desktop-sidebar"
      : "desktop-modal";

  const [reloadNonce, setReloadNonce] = useState(0);
  const [data, setData] = useState<LoadedData>(() => ({
    status: "loading",
    fileName:
      target.kind === "local-file"
        ? target.file.name
        : target.kind === "acp-attachment"
          ? target.att.file_name
          : target.kind === "host-path"
            ? target.path.split(/[\\/]/).pop() || target.path
            : target.fileName,
    mime:
      target.kind === "local-file"
        ? target.file.type
        : target.kind === "acp-attachment"
          ? target.att.mime
          : target.kind === "chat-attachment"
            ? target.mime
            : "",
    size:
      target.kind === "local-file"
        ? target.file.size
        : target.kind === "acp-attachment"
          ? target.att.size
          : target.kind === "chat-attachment"
            ? target.size
            : undefined,
    viewKind: "code",
  }));

  const [rawMode, setRawMode] = useState(false);
  const [imgZoom, setImgZoom] = useState(1);
  const [copied, setCopied] = useState(false);
  const [pathCopied, setPathCopied] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(440);

  // 移动端抽屉：false = 展开覆盖大部分保留最左侧；true = 右滑收起至最右侧边缘
  const [collapsed, setCollapsed] = useState(false);
  const [dragOffsetPx, setDragOffsetPx] = useState<number | null>(null);

  const sheetRef = useRef<HTMLDivElement>(null);
  const codeBodyRef = useRef<HTMLDivElement>(null);
  const pressedOutsideRef = useRef<boolean | null>(null);
  const gestureRef = useRef<{
    mode: "collapse" | "expand";
    source: "touch" | "pointer";
    pointerId?: number;
    startX: number;
    startY: number;
    lastX: number;
    lastTime: number;
    vx: number;
    width: number;
    axis: "undecided" | "x" | "y";
    blockedByInnerScroll: boolean;
  } | null>(null);

  const lineStart = target.kind === "host-path" ? target.lineStart : undefined;
  const lineEnd = target.kind === "host-path" ? (target.lineEnd ?? target.lineStart) : undefined;
  const badge = target.badge;

  // 主动点击打开新文件或再次点击同一文件时自动展开
  useEffect(() => {
    setCollapsed(false);
    setDragOffsetPx(null);
    setImgZoom(1);
    setCopied(false);
    setPathCopied(false);
    setRawMode(Boolean(lineStart));
  }, [target.key, openSeq, lineStart]);

  useEffect(() => {
    onMobileCollapsedChange?.(layoutMode === "mobile-drawer" ? collapsed : false);
  }, [layoutMode, collapsed, onMobileCollapsedChange]);

  // 加载预览内容并托管所有临时 ObjectURL 生命周期
  useEffect(() => {
    let alive = true;
    const abortController = new AbortController();
    let revokeUrl: string | null = null;

    const assignObjectUrl = (blob: Blob) => {
      if (revokeUrl) URL.revokeObjectURL(revokeUrl);
      revokeUrl = URL.createObjectURL(blob);
      return revokeUrl;
    };

    const showPdf = async (blob: Blob, file: Omit<LoadedData, "status" | "viewKind">) => {
      const header = await blob.slice(0, 1024).text();
      if (!alive) return;
      if (!/%PDF-[12]\.\d/.test(header)) {
        setData({
          ...file,
          status: "ready",
          viewKind: "unsupported",
          unsupportedReason: "文件不是可预览的 PDF",
        });
        return;
      }
      setData({
        ...file,
        status: "ready",
        viewKind: "pdf",
        mediaUrl: assignObjectUrl(blob.slice(0, blob.size, "application/pdf")),
      });
    };

    const load = async () => {
      try {
        if (target.kind === "local-file") {
          const f = target.file;
          const vk = classifyPreviewKind(f.name, f.type);
          setData({
            status: "loading",
            fileName: f.name,
            mime: f.type || "application/octet-stream",
            size: f.size,
            viewKind: vk,
          });
          if (vk === "markdown" || vk === "csv" || vk === "code") {
            const slice = f.slice(0, MAX_CLIENT_TEXT_BYTES);
            const txt = await slice.text();
            if (!alive) return;
            setData({
              status: "ready",
              fileName: f.name,
              mime: f.type || "text/plain",
              size: f.size,
              viewKind: vk,
              textContent: txt,
              downloadBlob: f,
              truncated: f.size > MAX_CLIENT_TEXT_BYTES,
            });
            return;
          }
          if (vk === "pdf") {
            await showPdf(f, { fileName: f.name, mime: f.type, size: f.size, downloadBlob: f });
            return;
          }
          if (vk === "image" || vk === "audio" || vk === "video") {
            const url = assignObjectUrl(f);
            let svgText: string | undefined;
            if (
              (f.type === "image/svg+xml" || f.name.toLowerCase().endsWith(".svg")) &&
              f.size <= MAX_CLIENT_TEXT_BYTES
            ) {
              svgText = await f.text().catch(() => undefined);
            }
            if (!alive) return;
            setData({
              status: "ready",
              fileName: f.name,
              mime: f.type || "application/octet-stream",
              size: f.size,
              viewKind: vk,
              mediaUrl: url,
              textContent: svgText,
              downloadBlob: f,
            });
            return;
          }
          if (!alive) return;
          setData({
            status: "ready",
            fileName: f.name,
            mime: f.type || "application/octet-stream",
            size: f.size,
            viewKind: "unsupported",
            downloadBlob: f,
          });
          return;
        }

        if (target.kind === "chat-attachment") {
          const vk = classifyPreviewKind(target.fileName, target.mime);
          setData({
            status: "loading",
            fileName: target.fileName,
            mime: target.mime,
            size: target.size,
            viewKind: vk,
          });

          if (vk === "pdf") {
            let blob: Blob;
            if (target.loadBlob) {
              blob = await target.loadBlob();
            } else {
              const url = target.url ?? await target.loadUrl?.();
              if (!alive) return;
              if (!url) throw new Error("找不到 PDF 附件");
              const resp = await fetch(url, {
                cache: "no-store",
                referrerPolicy: "no-referrer",
                signal: abortController.signal,
              });
              if (!resp.ok) throw new Error(`加载失败 (${resp.status})`);
              blob = await resp.blob();
            }
            await showPdf(blob, {
              fileName: target.fileName,
              mime: target.mime,
              size: target.size ?? blob.size,
            });
            return;
          }

          if (target.loadBlob) {
            const blob = await target.loadBlob();
            if (!alive) return;
            if (vk === "markdown" || vk === "csv" || vk === "code") {
              const txt = await blob.slice(0, MAX_CLIENT_TEXT_BYTES).text();
              if (!alive) return;
              setData({
                status: "ready",
                fileName: target.fileName,
                mime: target.mime,
                size: target.size ?? blob.size,
                viewKind: vk,
                textContent: txt,
                truncated: blob.size > MAX_CLIENT_TEXT_BYTES,
              });
              return;
            }
            const url = vk === "unsupported" ? undefined : assignObjectUrl(blob);
            setData({
              status: "ready",
              fileName: target.fileName,
              mime: target.mime,
              size: target.size ?? blob.size,
              viewKind: vk,
              mediaUrl: url,
            });
            return;
          }

          const url = target.url ?? (vk !== "unsupported" ? await target.loadUrl?.() : undefined);
          if (!alive || (!url && vk !== "unsupported")) {
            return;
          }

          if (url && (vk === "markdown" || vk === "csv" || vk === "code")) {
            const { text, truncated } = await readLimitedText(
              url,
              MAX_CLIENT_TEXT_BYTES,
              abortController.signal,
              target.size,
            );
            if (!alive || abortController.signal.aborted) return;
            setData({
              status: "ready",
              fileName: target.fileName,
              mime: target.mime,
              size: target.size,
              viewKind: vk,
              textContent: text,
              truncated,
            });
            return;
          }

          setData({
            status: "ready",
            fileName: target.fileName,
            mime: target.mime,
            size: target.size,
            viewKind: vk,
            mediaUrl: url,
          });
          return;
        }

        if (target.kind === "acp-attachment") {
          const { att } = target;
          const vk = classifyPreviewKind(att.file_name, att.mime);
          setData({
            status: "loading",
            fileName: att.file_name,
            mime: att.mime,
            size: att.size,
            viewKind: vk,
          });
          const blob = await target.loadBlob();
          if (!alive) return;
          if (vk === "markdown" || vk === "csv" || vk === "code") {
            const txt = await blob.slice(0, MAX_CLIENT_TEXT_BYTES).text();
            if (!alive) return;
            setData({
              status: "ready",
              fileName: att.file_name,
              mime: att.mime,
              size: att.size || blob.size,
              viewKind: vk,
              textContent: txt,
              downloadBlob: blob,
              truncated: blob.size > MAX_CLIENT_TEXT_BYTES,
            });
            return;
          }
          if (vk === "pdf") {
            await showPdf(blob, {
              fileName: att.file_name, mime: att.mime, size: att.size || blob.size, downloadBlob: blob,
            });
            return;
          }
          if (vk === "image" || vk === "audio" || vk === "video") {
            const url = assignObjectUrl(blob);
            let svgText: string | undefined;
            if (
              (att.mime === "image/svg+xml" || att.file_name.toLowerCase().endsWith(".svg")) &&
              blob.size <= MAX_CLIENT_TEXT_BYTES
            ) {
              svgText = await blob.text().catch(() => undefined);
            }
            if (!alive) return;
            setData({
              status: "ready",
              fileName: att.file_name,
              mime: att.mime,
              size: att.size || blob.size,
              viewKind: vk,
              mediaUrl: url,
              textContent: svgText,
              downloadBlob: blob,
            });
            return;
          }
          setData({
            status: "ready",
            fileName: att.file_name,
            mime: att.mime,
            size: att.size || blob.size,
            viewKind: "unsupported",
            downloadBlob: blob,
          });
          return;
        }

        // target.kind === "host-path"
        const guessName = target.path.split(/[\\/]/).pop() || target.path;
        setData({
          status: "loading",
          fileName: guessName,
          mime: "",
          resolvedPath: target.path,
          viewKind: classifyPreviewKind(guessName, ""),
        });
        const res = await target.loadHostFile();
        if (!alive) return;
        const vk = classifyPreviewKind(res.file_name, res.mime);

        if (vk === "pdf" && res.content !== null) {
          const blob = res.encoding === "base64"
            ? base64ToBlob(res.content, res.mime)
            : new Blob([res.content], { type: res.mime });
          await showPdf(blob, {
            fileName: res.file_name,
            mime: res.mime,
            size: res.size,
            resolvedPath: res.path,
            downloadBlob: res.truncated ? undefined : blob,
            truncated: res.truncated,
          });
          return;
        }

        if (res.encoding === "utf8" && res.content !== null) {
          const effectiveKind = vk === "unsupported" ? "code" : vk;
          let mediaUrl: string | undefined;
          const textBlob = new Blob([res.content], {
            type: res.mime || "text/plain;charset=utf-8",
          });
          if (effectiveKind === "image") {
            mediaUrl = assignObjectUrl(
              new Blob([res.content], { type: "image/svg+xml" }),
            );
          }
          setData({
            status: "ready",
            fileName: res.file_name,
            mime: res.mime,
            size: res.size,
            resolvedPath: res.path,
            viewKind: effectiveKind,
            mediaUrl,
            textContent: res.content,
            downloadBlob: !res.truncated ? textBlob : undefined,
            truncated: res.truncated,
          });
          return;
        }

        if (res.encoding === "base64" && res.content !== null) {
          const blob = base64ToBlob(res.content, res.mime);
          const url = assignObjectUrl(blob);
          setData({
            status: "ready",
            fileName: res.file_name,
            mime: res.mime,
            size: res.size,
            resolvedPath: res.path,
            viewKind: vk === "unsupported" ? "image" : vk,
            mediaUrl: url,
            downloadBlob: blob,
            truncated: res.truncated,
          });
          return;
        }

        setData({
          status: "ready",
          fileName: res.file_name,
          mime: res.mime,
          size: res.size,
          resolvedPath: res.path,
          viewKind: "unsupported",
          truncated: res.truncated,
          unsupportedReason:
            res.reason === "too_large"
              ? "文件体积超过在线预览上限（4 MB）"
              : "二进制格式文件暂不支持直接预览",
        });
      } catch (err) {
        if (!alive || abortController.signal.aborted) return;
        setData((prev) => ({
          ...prev,
          status: "error",
          errorMsg: err instanceof Error ? err.message : String(err),
        }));
      }
    };

    void load();
    return () => {
      alive = false;
      abortController.abort();
      if (revokeUrl) URL.revokeObjectURL(revokeUrl);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target.key, reloadNonce]);

  // 若指定了目标行号，内容加载完成后自动滚动定位并高亮
  useEffect(() => {
    if (data.status !== "ready" || !lineStart) return;
    const timer = window.setTimeout(() => {
      const row = codeBodyRef.current?.querySelector(`[data-line="${lineStart}"]`);
      row?.scrollIntoView({ block: "center" });
    }, 30);
    return () => window.clearTimeout(timer);
  }, [data.status, data.textContent, lineStart, rawMode]);

  // 桌面 Agent 右侧栏拖拽调宽
  const startSidebarResize = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarWidth;
    const onMove = (ev: PointerEvent) => {
      const delta = startX - ev.clientX;
      const maxW = Math.min(720, Math.floor(window.innerWidth * 0.56));
      setSidebarWidth(Math.max(300, Math.min(maxW, startW + delta)));
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  // ---------- 移动端右滑收起 / 左滑展开手势引擎 ----------
  const getDrawerWidth = () => {
    const measured = sheetRef.current?.offsetWidth ?? 0;
    if (measured > 0) return measured;
    const vw = typeof window !== "undefined" && window.innerWidth > 0 ? window.innerWidth : 390;
    return Math.min(560, Math.max(260, vw - 48));
  };

  const beginGesture = (
    mode: "collapse" | "expand",
    source: "touch" | "pointer",
    clientX: number,
    clientY: number,
    eventTarget: EventTarget | null,
    pointerId?: number,
  ) => {
    if (gestureRef.current && gestureRef.current.source !== source) return;
    const now = performance.now();
    gestureRef.current = {
      mode,
      source,
      pointerId,
      startX: clientX,
      startY: clientY,
      lastX: clientX,
      lastTime: now,
      vx: 0,
      width: getDrawerWidth(),
      axis: "undecided",
      blockedByInnerScroll:
        mode === "collapse" && canScrollLeftInside(eventTarget, sheetRef.current),
    };
  };

  const moveGesture = (source: "touch" | "pointer", clientX: number, clientY: number) => {
    const g = gestureRef.current;
    if (!g || g.source !== source) return;
    const dx = clientX - g.startX;
    const dy = clientY - g.startY;
    const now = performance.now();
    const dt = now - g.lastTime;
    if (dt > 0) {
      g.vx = (clientX - g.lastX) / dt;
      g.lastX = clientX;
      g.lastTime = now;
    }

    if (g.axis === "undecided") {
      if (Math.hypot(dx, dy) < 8) return;
      if (Math.abs(dx) > Math.abs(dy) * 1.12 && !(dx > 0 && g.blockedByInnerScroll)) {
        g.axis = "x";
      } else {
        g.axis = "y";
        return;
      }
    }
    if (g.axis !== "x") return;

    if (g.mode === "collapse") {
      setDragOffsetPx(Math.max(0, Math.min(g.width, dx)));
    } else {
      setDragOffsetPx(Math.max(0, Math.min(g.width, g.width + dx)));
    }
  };

  const finishGesture = (source: "touch" | "pointer", clientX?: number, canceled = false) => {
    const g = gestureRef.current;
    if (!g || g.source !== source) return;
    gestureRef.current = null;

    if (canceled || g.axis !== "x") {
      setDragOffsetPx(null);
      return;
    }
    const endX = clientX ?? g.lastX;
    const dx = endX - g.startX;
    if (g.mode === "collapse") {
      setCollapsed(shouldCollapseOnSwipeRight(dx, g.vx, g.width));
    } else {
      if (shouldExpandOnSwipeLeft(dx, g.vx, g.width)) {
        setCollapsed(false);
      } else {
        setCollapsed(true);
      }
    }
    setDragOffsetPx(null);
  };

  const handleClose = useCallback(() => {
    if (shouldIgnoreClick?.()) return;
    onClose();
  }, [onClose, shouldIgnoreClick]);

  const copyContent = async () => {
    if (!data.textContent) return;
    if (await copyPlainText(data.textContent)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    }
  };

  const copyPath = async () => {
    const p = data.resolvedPath || (target.kind === "host-path" ? target.path : "");
    if (!p) return;
    if (await copyPlainText(p)) {
      setPathCopied(true);
      window.setTimeout(() => setPathCopied(false), 1600);
    }
  };

  const hasToggleablePreview =
    data.status === "ready" &&
    Boolean(data.textContent) &&
    (data.viewKind === "markdown" ||
      data.viewKind === "csv" ||
      (data.viewKind === "image" &&
        (data.mime === "image/svg+xml" || data.fileName.toLowerCase().endsWith(".svg"))));

  const csvRows = useMemo(() => {
    if (data.status !== "ready" || data.viewKind !== "csv" || !data.textContent || rawMode) {
      return null;
    }
    const isTsv =
      data.mime.includes("tab-separated") || data.fileName.toLowerCase().endsWith(".tsv");
    return parseDelimitedTable(data.textContent, isTsv ? "\t" : ",");
  }, [data.status, data.viewKind, data.textContent, data.mime, data.fileName, rawMode]);

  const codeLines = useMemo(() => {
    if (data.status !== "ready" || data.textContent === undefined) return null;
    const showCode =
      data.viewKind === "code" ||
      (rawMode && (data.viewKind === "markdown" || data.viewKind === "csv" || data.viewKind === "image"));
    if (!showCode) return null;
    const split = data.textContent.replace(/\r\n?/g, "\n").split("\n");
    if (split.length > 4000 && !lineStart) return null;
    return split;
  }, [data.status, data.textContent, data.viewKind, rawMode, lineStart]);

  const defaultDownload =
    toolbarActions == null
      ? target.kind === "acp-attachment" && target.onDownload
        ? target.onDownload
        : data.downloadBlob
          ? () => triggerBlobDownload(data.downloadBlob!, data.fileName)
          : undefined
      : undefined;

  const sizeText = fmtPreviewSize(data.size);

  const renderHeader = () => (
    <div className="pv-head">
      <div className="pv-title-group">
        <div className="pv-title-line">
          <span className="pv-file-icon" aria-hidden="true">
            {data.viewKind === "image" ? (
              <ImageIcon size={15} />
            ) : data.viewKind === "code" ? (
              <CodeIcon size={15} />
            ) : (
              <FileTextIcon size={15} />
            )}
          </span>
          <span className="pv-name" title={data.fileName}>
            {data.fileName}
          </span>
          {badge && <span className="pv-badge">{badge}</span>}
          {lineStart && (
            <span className="pv-line-badge mono-code">
              L{lineStart}
              {lineEnd && lineEnd > lineStart ? `-${lineEnd}` : ""}
            </span>
          )}
          {sizeText && <span className="muted pv-size">{sizeText}</span>}
        </div>
        {data.resolvedPath && (
          <div className="pv-subpath-row">
            <span className="pv-subpath mono-code" title={data.resolvedPath}>
              {data.resolvedPath}
            </span>
            <button
              type="button"
              className="pv-mini-btn"
              title="复制文件路径"
              onClick={() => void copyPath()}
            >
              {pathCopied ? "已复制路径" : "复制路径"}
            </button>
          </div>
        )}
      </div>

      <span className="pv-toolbar">
        {hasToggleablePreview && (
          <button
            type="button"
            className="pv-act"
            data-testid="pv-toggle-raw"
            onClick={() => setRawMode((v) => !v)}
          >
            <CodeIcon size={14} />
            <span>{rawMode ? "渲染" : "源码"}</span>
          </button>
        )}

        {data.status === "ready" && data.viewKind === "image" && !rawMode && (
          <span className="pv-zoom-group" role="group" aria-label="图片缩放">
            <button
              type="button"
              className="pv-zoom-btn"
              aria-label="缩小"
              onClick={() => setImgZoom((z) => Math.max(0.5, Number((z - 0.25).toFixed(2))))}
            >
              −
            </button>
            <button
              type="button"
              className="pv-zoom-btn pv-zoom-reset"
              title="重置缩放"
              onClick={() => setImgZoom(1)}
            >
              {Math.round(imgZoom * 100)}%
            </button>
            <button
              type="button"
              className="pv-zoom-btn"
              aria-label="放大"
              onClick={() => setImgZoom((z) => Math.min(3, Number((z + 0.25).toFixed(2))))}
            >
              +
            </button>
          </span>
        )}

        {data.status === "ready" && data.textContent !== undefined && (
          <button
            type="button"
            className="pv-act"
            data-testid="pv-copy-content"
            onClick={() => void copyContent()}
          >
            {copied ? <CheckIcon size={14} /> : <CopyIcon />}
            <span>{copied ? "已复制" : "复制"}</span>
          </button>
        )}

        {toolbarActions}

        {defaultDownload && (
          <button
            type="button"
            className="pv-act"
            data-testid="viewer-download"
            onClick={() => {
              if (!shouldIgnoreClick?.()) defaultDownload();
            }}
          >
            <DownloadIcon />
            <span>下载</span>
          </button>
        )}
      </span>

      <div className="pv-win-btns">
        {layoutMode === "mobile-drawer" && (
          <button
            type="button"
            className="icon-btn pv-collapse-btn"
            data-testid="pv-mobile-collapse"
            title="右滑收起"
            aria-label="收起预览"
            onClick={() => {
              if (!shouldIgnoreClick?.()) setCollapsed(true);
            }}
          >
            <ChevronRightIcon size={18} />
          </button>
        )}
        <button
          type="button"
          className="icon-btn"
          data-testid="viewer-close"
          aria-label="关闭预览"
          title="关闭"
          onClick={handleClose}
        >
          <XIcon size={17} />
        </button>
      </div>
    </div>
  );

  const renderBody = () => {
    if (data.status === "loading") {
      return <div className="pv-status-box muted">加载文件内容中…</div>;
    }

    if (data.status === "error") {
      return (
        <div className="pv-status-box">
          <div className="pv-err-text">{data.errorMsg || "加载文件失败"}</div>
          <button
            type="button"
            className="btn-ghost"
            onClick={() => setReloadNonce((n) => n + 1)}
          >
            重试
          </button>
        </div>
      );
    }

    return (
      <>
        {data.truncated && data.viewKind !== "unsupported" && (
          <div className="pv-trunc-banner">
            文件较大，当前仅展示前 512 KB 内容预览
          </div>
        )}

        {data.viewKind === "image" && !rawMode && (
          <div className="pv-media-stage">
            {data.mediaUrl ? (
              <img
                src={data.mediaUrl}
                alt={data.fileName}
                style={
                  imgZoom !== 1
                    ? {
                        transform: `scale(${imgZoom})`,
                        transformOrigin: "center center",
                      }
                    : undefined
                }
              />
            ) : (
              <div className="pv-unsupported">附件加载中…</div>
            )}
          </div>
        )}

        {data.viewKind === "markdown" && !rawMode && data.textContent !== undefined && (
          <div className="pv-md-stage">
            <AcpMarkdown text={data.textContent} />
          </div>
        )}

        {data.viewKind === "csv" && !rawMode && csvRows && (
          <div className="pv-table-stage">
            {csvRows.length === 0 ? (
              <div className="pv-unsupported">空表格文件</div>
            ) : (
              <table className="pv-csv-table">
                <thead>
                  <tr>
                    <th className="pv-csv-idx">#</th>
                    {csvRows[0].map((h, ci) => (
                      <th key={ci}>{h}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {csvRows.slice(1).map((row, ri) => (
                    <tr key={ri}>
                      <td className="pv-csv-idx">{ri + 1}</td>
                      {csvRows[0].map((_, ci) => (
                        <td key={ci}>{row[ci] ?? ""}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}

        {(data.viewKind === "code" ||
          (rawMode &&
            (data.viewKind === "markdown" ||
              data.viewKind === "csv" ||
              data.viewKind === "image"))) &&
          data.textContent !== undefined && (
            <div className="pv-code-stage" ref={codeBodyRef}>
              {codeLines ? (
                <div className="pv-code-lines mono-code">
                  {codeLines.map((ln, idx) => {
                    const num = idx + 1;
                    const hl =
                      lineStart != null &&
                      num >= lineStart &&
                      num <= (lineEnd ?? lineStart);
                    return (
                      <div
                        key={num}
                        data-line={num}
                        className={`pv-code-row${hl ? " is-hl" : ""}`}
                      >
                        <span className="pv-code-num" aria-hidden="true">
                          {num}
                        </span>
                        <span className="pv-code-text">{ln || " "}</span>
                      </div>
                    );
                  })}
                </div>
              ) : (
                <pre className="pv-raw-pre mono-code">{data.textContent}</pre>
              )}
            </div>
          )}

        {data.viewKind === "pdf" && (
          <div className="pv-pdf-stage">
            {data.mediaUrl ? (
              <iframe src={data.mediaUrl} title={data.fileName} referrerPolicy="no-referrer" />
            ) : (
              <div className="pv-unsupported">PDF 加载中…</div>
            )}
          </div>
        )}

        {data.viewKind === "audio" && (
          <div className="pv-media-stage">
            {data.mediaUrl ? (
              <audio controls src={data.mediaUrl} />
            ) : (
              <div className="pv-unsupported">音频加载中…</div>
            )}
          </div>
        )}

        {data.viewKind === "video" && (
          <div className="pv-media-stage">
            {data.mediaUrl ? (
              <video controls playsInline src={data.mediaUrl} />
            ) : (
              <div className="pv-unsupported">视频加载中…</div>
            )}
          </div>
        )}

        {data.viewKind === "unsupported" && (
          <div className="pv-status-box">
            <div className="pv-unsupported">
              {data.unsupportedReason || "暂不支持预览此格式，请下载后查看"}
            </div>
          </div>
        )}
      </>
    );
  };

  // ==================== 模式 1：桌面端 Agent 右侧边栏 ====================
  if (layoutMode === "desktop-sidebar") {
    return (
      <aside
        className="pv-agent-sidebar"
        data-testid="acp-preview-sidebar"
        aria-label={`文件预览: ${data.fileName}`}
        style={{ width: sidebarWidth }}
      >
        <div
          className="pv-sidebar-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="调整预览侧栏宽度"
          onPointerDown={startSidebarResize}
        />
        {renderHeader()}
        <div className="pv-body is-sidebar">{renderBody()}</div>
      </aside>
    );
  }

  // ==================== 模式 2：移动端 & 移动 Web 右侧覆盖层（保留最左侧 + 右滑收起 / 左滑展开） ====================
  if (layoutMode === "mobile-drawer") {
    const w = getDrawerWidth();
    const activeOffset =
      dragOffsetPx !== null ? dragOffsetPx : collapsed ? w : 0;
    const openRatio = w > 0 ? Math.max(0, Math.min(1, 1 - activeOffset / w)) : collapsed ? 0 : 1;
    const isDragging = dragOffsetPx !== null;
    const sheetTransform =
      dragOffsetPx !== null
        ? `translate3d(${dragOffsetPx}px, 0, 0)`
        : collapsed
          ? "translate3d(100%, 0, 0)"
          : "translate3d(0, 0, 0)";

    return (
      <>
        <div
          className={`pv-mobile-layer${collapsed && !isDragging ? " is-collapsed" : ""}`}
          data-testid="pv-mobile-drawer"
          data-collapsed={collapsed ? "1" : "0"}
          style={{ zIndex: Z.modal }}
        >
          <div
            className="pv-mobile-mask"
            data-testid="pv-mobile-mask"
            style={{ opacity: openRatio }}
            onClick={() => {
              if (!collapsed && !shouldIgnoreClick?.()) {
                setCollapsed(true);
              }
            }}
          />
          <div
            ref={sheetRef}
            className={`pv-mobile-sheet${isDragging ? " is-dragging" : ""}`}
            role="dialog"
            aria-modal={!collapsed}
            aria-hidden={collapsed && !isDragging}
            aria-label={data.fileName}
            style={{ transform: sheetTransform }}
            onClick={(e) => e.stopPropagation()}
            onTouchStart={(e) => {
              const t = e.touches[0];
              if (!t || collapsed) return;
              beginGesture("collapse", "touch", t.clientX, t.clientY, e.target);
            }}
            onTouchMove={(e) => {
              const t = e.touches[0];
              if (!t) return;
              moveGesture("touch", t.clientX, t.clientY);
            }}
            onTouchEnd={(e) => {
              const t = e.changedTouches[0];
              finishGesture("touch", t?.clientX, false);
            }}
            onTouchCancel={() => finishGesture("touch", undefined, true)}
            onPointerDown={(e) => {
              if (e.pointerType === "mouse" && e.button !== 0) return;
              if (collapsed) return;
              beginGesture("collapse", "pointer", e.clientX, e.clientY, e.target, e.pointerId);
            }}
            onPointerMove={(e) => {
              moveGesture("pointer", e.clientX, e.clientY);
            }}
            onPointerUp={(e) => {
              finishGesture("pointer", e.clientX, false);
            }}
            onPointerCancel={() => finishGesture("pointer", undefined, true)}
          >
            <div className="pv-mobile-grabber" aria-hidden="true" />
            {renderHeader()}
            <div className="pv-body is-drawer">{renderBody()}</div>
          </div>
        </div>

        {collapsed && (
          <div
            className="pv-edge-zone"
            data-testid="pv-edge-expander"
            style={{ zIndex: Z.modal }}
            onTouchStart={(e) => {
              const t = e.touches[0];
              if (!t) return;
              beginGesture("expand", "touch", t.clientX, t.clientY, e.target);
            }}
            onTouchMove={(e) => {
              const t = e.touches[0];
              if (!t) return;
              moveGesture("touch", t.clientX, t.clientY);
            }}
            onTouchEnd={(e) => {
              const t = e.changedTouches[0];
              finishGesture("touch", t?.clientX, false);
            }}
            onTouchCancel={() => finishGesture("touch", undefined, true)}
            onPointerDown={(e) => {
              if (e.pointerType === "mouse" && e.button !== 0) return;
              beginGesture("expand", "pointer", e.clientX, e.clientY, e.target, e.pointerId);
            }}
            onPointerMove={(e) => {
              if (
                gestureRef.current?.source === "pointer" &&
                gestureRef.current.pointerId === e.pointerId &&
                Math.hypot(
                  e.clientX - gestureRef.current.startX,
                  e.clientY - gestureRef.current.startY,
                ) >= 8
              ) {
                try {
                  e.currentTarget.setPointerCapture?.(e.pointerId);
                } catch {}
              }
              moveGesture("pointer", e.clientX, e.clientY);
            }}
            onPointerUp={(e) => {
              finishGesture("pointer", e.clientX, false);
            }}
            onPointerCancel={() => finishGesture("pointer", undefined, true)}
          >
            <button
              type="button"
              className="pv-edge-pill"
              data-testid="pv-edge-pill"
              aria-label={`展开文件预览：${data.fileName}`}
              title={`左滑或点击展开：${data.fileName}`}
              onClick={() => setCollapsed(false)}
            >
              <ChevronLeftIcon size={15} />
              <span className="pv-edge-pill-text">{data.fileName}</span>
            </button>
          </div>
        )}
      </>
    );
  }

  // ==================== 模式 3：桌面端 & 桌面 Web 聊天中央弹窗 ====================
  return (
    <div
      className="modal-mask pv-modal-mask"
      data-testid="file-preview-modal"
      style={{ zIndex: Z.modal }}
      onPointerDown={(e) => {
        pressedOutsideRef.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        if (e.target !== e.currentTarget) return;
        if (pressedOutsideRef.current === false) {
          pressedOutsideRef.current = null;
          return;
        }
        pressedOutsideRef.current = null;
        handleClose();
      }}
    >
      <div
        className="pv-modal"
        role="dialog"
        aria-modal="true"
        aria-label={data.fileName}
        onClick={(e) => e.stopPropagation()}
      >
        {renderHeader()}
        <div className="pv-body">{renderBody()}</div>
      </div>
    </div>
  );
}
