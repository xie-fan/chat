const NPM_ADAPTER_PACKAGES: Record<string, string> = {
  "codex-acp": "@agentclientprotocol/codex-acp",
  "claude-agent-acp": "@agentclientprotocol/claude-agent-acp",
};

function firstCommandToken(raw: string): string {
  const s = raw.trim();
  if (!s) return "";
  const first = s[0];
  if (first === '"' || first === "'") {
    const end = s.indexOf(first, 1);
    return end > 1 ? s.slice(1, end) : s.slice(1);
  }
  const match = /^\S+/.exec(s);
  return match ? match[0] : "";
}

export function autoInstallPackageForCmd(cmd: string): string | null {
  const prog = firstCommandToken(cmd).trim();
  if (!prog || prog.includes("/") || prog.includes("\\")) return null;
  const normalized = prog.toLowerCase().replace(/\.(cmd|exe|bat)$/i, "");
  return NPM_ADAPTER_PACKAGES[normalized] ?? null;
}

export function agentStatusLabel(status: string): {
  text: string;
  tone: "online" | "installing" | "muted";
} {
  switch (status) {
    case "online":
      return { text: "就绪", tone: "online" };
    case "installing":
      return { text: "正在通过 npm 安装适配器…", tone: "installing" };
    case "offline":
      return { text: "离线", tone: "muted" };
    default:
      return { text: "待命", tone: "muted" };
  }
}
