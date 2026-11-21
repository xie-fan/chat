export interface ParsedFileRef {
  raw: string;
  path: string;
  lineStart?: number;
  lineEnd?: number;
  display: string;
}

const NON_FILE_SCHEME_RE =
  /^(?:https?|wss?|mailto|tel|javascript|vbscript|data|blob|chat|about|chrome|edge):/i;

const ALL_KNOWN_EXTS = new Set([
  "ts",
  "tsx",
  "js",
  "jsx",
  "mjs",
  "cjs",
  "json",
  "jsonc",
  "json5",
  "jsonl",
  "md",
  "mdx",
  "markdown",
  "txt",
  "log",
  "rs",
  "toml",
  "lock",
  "py",
  "pyi",
  "go",
  "mod",
  "sum",
  "java",
  "kt",
  "kts",
  "swift",
  "c",
  "h",
  "cc",
  "cpp",
  "hpp",
  "cs",
  "rb",
  "php",
  "lua",
  "zig",
  "sh",
  "bash",
  "zsh",
  "fish",
  "ps1",
  "bat",
  "cmd",
  "sql",
  "graphql",
  "gql",
  "proto",
  "yaml",
  "yml",
  "xml",
  "html",
  "htm",
  "css",
  "scss",
  "sass",
  "less",
  "vue",
  "svelte",
  "astro",
  "csv",
  "tsv",
  "ini",
  "cfg",
  "conf",
  "env",
  "dockerfile",
  "gradle",
  "plist",
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "svg",
  "bmp",
  "ico",
  "avif",
  "pdf",
  "mp3",
  "wav",
  "ogg",
  "m4a",
  "aac",
  "flac",
  "mp4",
  "webm",
  "mov",
]);

const BARE_FILENAME_EXTS = new Set([
  "ts",
  "tsx",
  "js",
  "jsx",
  "mjs",
  "cjs",
  "json",
  "jsonc",
  "md",
  "mdx",
  "markdown",
  "rs",
  "toml",
  "lock",
  "py",
  "go",
  "java",
  "kt",
  "swift",
  "cpp",
  "hpp",
  "cs",
  "rb",
  "php",
  "sh",
  "ps1",
  "sql",
  "yaml",
  "yml",
  "xml",
  "html",
  "css",
  "scss",
  "vue",
  "svelte",
  "csv",
  "tsv",
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "svg",
  "pdf",
  "mp3",
  "wav",
  "ogg",
  "m4a",
  "mp4",
  "webm",
  "mov",
]);

const EXACT_BASENAMES = new Set([
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

const NON_FILE_BARE_PREFIXES = new Set([
  "window",
  "document",
  "navigator",
  "location",
  "history",
  "localstorage",
  "sessionstorage",
  "console",
  "math",
  "json",
  "object",
  "array",
  "promise",
  "number",
  "string",
  "boolean",
  "date",
  "regexp",
  "error",
  "process",
  "globalthis",
  "import",
  "this",
  "self",
  "req",
  "res",
  "ev",
  "event",
  "err",
  "node",
  "vue",
  "next",
]);

function safeDecodeURIComponent(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

function decodeFileUri(uri: string): string | null {
  try {
    const u = new URL(uri);
    if (u.protocol.toLowerCase() !== "file:") return null;
    let p = safeDecodeURIComponent(u.pathname);
    if (/^\/[A-Za-z]:\//.test(p)) {
      p = p.slice(1);
    } else if (u.hostname && u.hostname !== "localhost") {
      p = `//${u.hostname}${p}`;
    }
    return p || null;
  } catch {
    const rest = uri.replace(/^file:\/\/(?:localhost)?/i, "");
    if (!rest) return null;
    const decoded = safeDecodeURIComponent(rest);
    return /^\/[A-Za-z]:\//.test(decoded) ? decoded.slice(1) : decoded;
  }
}

function extractLineAnchor(input: string): {
  core: string;
  lineStart?: number;
  lineEnd?: number;
} {
  const hashMatch = /^(.*?)#L(\d+)(?:-L?(\d+))?$/i.exec(input);
  if (hashMatch && hashMatch[1]) {
    const start = Number(hashMatch[2]);
    const end = hashMatch[3] ? Number(hashMatch[3]) : undefined;
    return {
      core: hashMatch[1],
      lineStart: start > 0 ? start : undefined,
      lineEnd: end && end >= start ? end : undefined,
    };
  }

  const colonMatch = /^(.*?):(\d+)(?::(\d+))?$/.exec(input);
  if (colonMatch && colonMatch[1].length > 1 && !/^file:$/i.test(colonMatch[1])) {
    const start = Number(colonMatch[2]);
    return {
      core: colonMatch[1],
      lineStart: start > 0 ? start : undefined,
    };
  }

  return { core: input };
}

function splitNameAndExt(basename: string): { stem: string; ext: string } | null {
  const dot = basename.lastIndexOf(".");
  if (dot <= 0 || dot === basename.length - 1) return null;
  return {
    stem: basename.slice(0, dot),
    ext: basename.slice(dot + 1).toLowerCase(),
  };
}

/**
 * 识别模型回复（行内代码、Markdown 链接、裸 file:// 链接）或工具输出中的文件地址。
 * 同时解析可选的 #L10-L20 / :42 行号定位。
 */
export function parseFileReference(
  rawInput: string,
  opts?: { fromMarkdownLink?: boolean },
): ParsedFileRef | null {
  const raw = rawInput.trim();
  if (!raw || raw.length > 1024 || /[\r\n\0]/.test(raw)) return null;

  if (/^file:\/\//i.test(raw)) {
    const { core, lineStart, lineEnd } = extractLineAnchor(raw);
    const decoded = decodeFileUri(core);
    if (!decoded || decoded === "/" || /^[A-Za-z]:[\\/]?$/.test(decoded)) {
      return null;
    }
    return {
      raw,
      path: decoded,
      lineStart,
      lineEnd,
      display: raw,
    };
  }

  if (NON_FILE_SCHEME_RE.test(raw)) return null;

  const { core, lineStart, lineEnd } = extractLineAnchor(raw);
  if (!core || core.startsWith("-")) return null;

  // 排除代码表达式与命令行运算符
  if (/[;{}()=<>|&$!*?"'`]/.test(core)) return null;

  const isWinAbs = /^[A-Za-z]:[\\/]\S/.test(core);
  const isPosixCommonAbs = /^\/(?:Users|home|tmp|var|opt|etc|mnt|Volumes|workspace|workspaces|root|app|srv)\//i.test(
    core,
  );
  if (/\s/.test(core) && !opts?.fromMarkdownLink && !isWinAbs && !isPosixCommonAbs) {
    return null;
  }

  const hasSep = core.includes("/") || core.includes("\\");
  if (hasSep) {
    if (core.endsWith("/") || core.endsWith("\\")) return null;
    if (
      !/^(?:~[\\/]|(?:\.{1,2}[\\/])+|[A-Za-z]:[\\/]|\/|[A-Za-z0-9_@.-]+[\\/])/.test(
        core,
      )
    ) {
      return null;
    }
    const segs = core.split(/[\\/]/).filter(Boolean);
    if (segs.length === 0) return null;
    const base = segs[segs.length - 1];
    const baseLow = base.toLowerCase();
    if (EXACT_BASENAMES.has(baseLow)) {
      return { raw, path: core, lineStart, lineEnd, display: raw };
    }
    const parts = splitNameAndExt(base);
    if (parts && ALL_KNOWN_EXTS.has(parts.ext)) {
      return { raw, path: core, lineStart, lineEnd, display: raw };
    }
    if (
      opts?.fromMarkdownLink &&
      /^(?:(?:\.{1,2}[\\/])+|[A-Za-z]:[\\/]|\/)/.test(core) &&
      parts &&
      /^[a-z0-9]{1,10}$/.test(parts.ext)
    ) {
      return { raw, path: core, lineStart, lineEnd, display: raw };
    }
    return null;
  }

  // 无目录分隔符的单文件名
  const low = core.toLowerCase();
  if (EXACT_BASENAMES.has(low)) {
    return { raw, path: core, lineStart, lineEnd, display: raw };
  }

  const parts = splitNameAndExt(core);
  if (!parts) return null;
  if (/^v?\d+(?:\.\d+)*$/i.test(parts.stem)) return null;
  if (NON_FILE_BARE_PREFIXES.has(parts.stem.toLowerCase())) return null;
  if (!/^[A-Za-z0-9_.-]+$/.test(parts.stem)) return null;

  if (lineStart !== undefined && ALL_KNOWN_EXTS.has(parts.ext)) {
    return { raw, path: core, lineStart, lineEnd, display: raw };
  }
  if (BARE_FILENAME_EXTS.has(parts.ext)) {
    return { raw, path: core, lineStart, lineEnd, display: raw };
  }

  return null;
}
