import { readLocalAcpBlob, removeLocalAcpBlob, storeLocalAcpBlob } from "./acpAttachmentStore";
import { newClientId as makeUuid } from "./clientId";
import type {
  AcpActivityPhase,
  AcpAgent,
  AcpAttachment,
  AcpBootstrap,
  AcpDiffItem,
  AcpHost,
  AcpPermissionPolicy,
  AcpRpcLogEntry,
  AcpSession,
  AcpSnapshotAttachment,
  AcpThinkingLevel,
  AcpToolDiff,
  AcpTurn,
  AcpUsageStats,
  AcpWorkspace,
} from "./types";

export interface LocalVaultData {
  active: boolean;
  host: AcpHost | null;
  agents: AcpAgent[];
  agentSecrets: Record<string, Record<string, string>>;
  workspaces: AcpWorkspace[];
  sessions: AcpSession[];
  attachments: Record<string, AcpSnapshotAttachment>;
  owner: string | null;
  cloudSource: { owner: string; hostId: string; revision: string } | null;
  cloudImport: { owner: string; id: string } | null;
}

export interface LocalAttachmentPayload {
  id: string;
  file_name: string;
  mime: string;
  size: number;
  data_base64: string;
}

const VAULT_STORAGE_KEY = "chat_acp_local_vault_v1";

function emptyVault(): LocalVaultData {
  return {
    active: false,
    host: null,
    agents: [],
    agentSecrets: {},
    workspaces: [],
    sessions: [],
    attachments: {},
    owner: null,
    cloudSource: null,
    cloudImport: null,
  };
}

function loadVault(): LocalVaultData {
  if (typeof localStorage === "undefined") return emptyVault();
  try {
    const raw = localStorage.getItem(VAULT_STORAGE_KEY);
    if (!raw) return emptyVault();
    const p = JSON.parse(raw) as Partial<LocalVaultData>;
    return {
      active: Boolean(p.active),
      host: p.host ?? null,
      agents: Array.isArray(p.agents) ? p.agents : [],
      agentSecrets: p.agentSecrets ?? {},
      workspaces: Array.isArray(p.workspaces) ? p.workspaces : [],
      sessions: Array.isArray(p.sessions)
        ? p.sessions.map((s) => ({ ...s, running: false, run_id: null }))
        : [],
      attachments: p.attachments ?? {},
      owner: p.owner ?? null,
      cloudSource: p.cloudSource ?? null,
      cloudImport: p.cloudImport ?? null,
    };
  } catch {
    return emptyVault();
  }
}

let vault: LocalVaultData = loadVault();

function writeVault(data: LocalVaultData) {
  if (!data.active && !data.host && data.agents.length === 0 && data.workspaces.length === 0) {
    localStorage.removeItem(VAULT_STORAGE_KEY);
  } else {
    localStorage.setItem(VAULT_STORAGE_KEY, JSON.stringify(data));
  }
}

function commitVault(data: LocalVaultData): LocalVaultData {
  // Transfers may destroy the source after this call. A failed write must
  // leave the current in-memory and persisted snapshots intact.
  writeVault(data);
  vault = data;
  return data;
}

function persistVault(keepImport = false) {
  if (vault.cloudImport && !keepImport) vault = { ...vault, cloudImport: null };
  if (typeof localStorage === "undefined") return;
  try {
    writeVault(vault);
  } catch {}
}

export function getLocalVault(): LocalVaultData {
  return vault;
}

export function isLocalOnlyActive(): boolean {
  return vault.active && Boolean(vault.host);
}

/** 本地事件桥接也要按服务器和账号隔离，防止切号后迟到事件修改旧账号 Vault。 */
export function localVaultOwnerMatches(owner: string): boolean {
  return !vault.owner || vault.owner === owner;
}

export function isLocalVaultHost(hostId: string | null | undefined): boolean {
  return Boolean(vault.active && vault.host && hostId === vault.host.id);
}

export function isLocalVaultAgent(agentId: string | null | undefined): boolean {
  return Boolean(vault.active && agentId && vault.agents.some((a) => a.id === agentId));
}

export function isLocalVaultWorkspace(wsId: string | null | undefined): boolean {
  return Boolean(vault.active && wsId && vault.workspaces.some((w) => w.id === wsId));
}

export function isLocalVaultSession(sessId: string | null | undefined): boolean {
  return Boolean(vault.active && sessId && vault.sessions.some((s) => s.id === sessId));
}

export function getLocalVaultRawEnv(agentId: string): Record<string, string> {
  return vault.agentSecrets[agentId] ?? {};
}

export function activateLocalVault(snapshot: {
  host: AcpHost;
  agents: AcpAgent[];
  workspaces: AcpWorkspace[];
  sessions: AcpSession[];
  attachments?: AcpSnapshotAttachment[];
  agentSecrets?: Record<string, Record<string, string>>;
  owner?: string;
  cloudSource?: LocalVaultData["cloudSource"];
}): LocalVaultData {
  const localHostId = snapshot.host.id;
  const host: AcpHost = {
    ...snapshot.host,
    id: localHostId,
    ip: "Local-Only",
    online: true,
    latency_ms: 0,
    last_seen: new Date().toISOString(),
  };
  return commitVault({
    active: true,
    host,
    agents: snapshot.agents.map((a) => ({ ...a, host_id: localHostId })),
    agentSecrets: snapshot.agentSecrets ?? vault.agentSecrets,
    workspaces: snapshot.workspaces.map((w) => ({ ...w, host_id: localHostId })),
    sessions: snapshot.sessions.map((s) => ({ ...s, turns: s.turns ?? [] })),
    attachments: Object.fromEntries((snapshot.attachments ?? []).map((a) => [a.id, a])),
    owner: snapshot.owner ?? null,
    cloudSource: snapshot.cloudSource ?? null,
    cloudImport: null,
  });
}

export function ensureLocalOnlyHost(info: {
  install_id: string;
  name: string;
  os: string;
  roots: string[];
}, owner?: string): LocalVaultData {
  const now = new Date().toISOString();
  const hostId = vault.host?.id || info.install_id || makeUuid();
  const host: AcpHost = {
    id: hostId,
    name: vault.host?.name || info.name || "Desktop",
    os: info.os || vault.host?.os || "",
    ip: "Local-Only",
    roots: info.roots?.length ? info.roots : (vault.host?.roots ?? []),
    online: true,
    latency_ms: 0,
    last_seen: now,
  };
  vault = {
    ...vault,
    active: true,
    host,
    owner: vault.owner ?? owner ?? null,
  };
  persistVault(true);
  return vault;
}

export function confirmLocalVaultCloudRemoval() {
  vault = { ...vault, cloudSource: null };
  persistVault(true);
}

export function beginLocalVaultImport(owner: string): string {
  if (vault.cloudImport?.owner === owner) return vault.cloudImport.id;
  if (vault.cloudImport) throw new Error("请切回原账号继续同步本地 ACP 数据");
  const id = makeUuid();
  commitVault({ ...vault, owner, cloudImport: { owner, id } });
  return id;
}

export function completeLocalVaultImport() {
  const attachmentIds = Object.keys(vault.attachments);
  commitVault(emptyVault());
  for (const id of attachmentIds) void removeLocalAcpBlob(id).catch(() => {});
}

export function mergeLocalVaultWithBootstrap(bs: AcpBootstrap, owner: string): AcpBootstrap {
  if (!vault.active || !vault.host || (vault.owner && vault.owner !== owner)) return bs;
  const lid = vault.host.id;
  const source = vault.cloudSource?.hostId;
  return {
    settings: bs.settings,
    hosts: [vault.host, ...bs.hosts.filter((h) => h.id !== lid && h.id !== source)],
    agents: [...vault.agents, ...bs.agents.filter((a) => a.host_id !== lid && a.host_id !== source)],
    workspaces: [...vault.workspaces, ...bs.workspaces.filter((w) => w.host_id !== lid && w.host_id !== source)],
    sessions: [
      ...vault.sessions,
      ...bs.sessions.filter(
        (s) => !vault.workspaces.some((w) => w.id === s.workspace_id),
      ),
    ],
  };
}

export function localPresetMeta(preset: string) {
  switch (preset.trim()) {
    case "claude":
      return {
        short_tag: "CC",
        badge_bg: "from-orange-500 to-amber-600",
        adapter: "claude-agent-acp",
        capabilities: ["fs.read", "fs.write", "terminal.run", "diff.stream"],
      };
    case "codex":
      return {
        short_tag: "CX",
        badge_bg: "from-emerald-500 to-teal-600",
        adapter: "codex-acp",
        capabilities: ["fs.read", "fs.write", "terminal.run"],
      };
    case "gemini":
      return {
        short_tag: "GM",
        badge_bg: "from-indigo-500 to-sky-500",
        adapter: "gemini --acp",
        capabilities: ["fs.read", "fs.write", "terminal.run"],
      };
    case "opencode":
      return {
        short_tag: "OC",
        badge_bg: "from-violet-500 to-purple-600",
        adapter: "opencode acp",
        capabilities: ["fs.read", "fs.write", "terminal.run"],
      };
    case "pi":
      return {
        short_tag: "PI",
        badge_bg: "from-amber-500 to-orange-600",
        adapter: "pi-rpc",
        capabilities: ["fs.read", "fs.write", "terminal.run", "diff.stream"],
      };
    default:
      return {
        short_tag: "AG",
        badge_bg: "from-indigo-500 to-sky-500",
        adapter: "acp-stdio",
        capabilities: ["fs.read", "fs.write", "terminal.run"],
      };
  }
}

export function renameLocalVaultHost(name: string): AcpHost | null {
  if (!vault.host) return null;
  const trimmed = name.trim().slice(0, 80);
  if (!trimmed) return vault.host;
  const nextHost: AcpHost = { ...vault.host, name: trimmed };
  vault = { ...vault, host: nextHost };
  persistVault();
  return nextHost;
}

export function addLocalVaultAgent(params: {
  preset: string;
  name: string;
  cmd: string;
  env?: Record<string, string>;
  status: string;
}): AcpAgent {
  const now = new Date().toISOString();
  const meta = localPresetMeta(params.preset);
  const id = makeUuid();
  const maskedEnv: Record<string, string> = {};
  const rawEnv: Record<string, string> = {};
  for (const [k, v] of Object.entries(params.env ?? {})) {
    if (k && v) {
      rawEnv[k] = v;
      maskedEnv[k] = "••••••••";
    }
  }
  const ag: AcpAgent = {
    id,
    host_id: vault.host?.id ?? "local",
    preset: params.preset,
    name: params.name.trim().slice(0, 80),
    adapter: meta.adapter,
    cmd: params.cmd.trim().slice(0, 500),
    env: maskedEnv,
    short_tag: meta.short_tag,
    badge_bg: meta.badge_bg,
    default_model: "Default Model",
    model_options: ["Default Model"],
    permission_override: "inherit",
    capabilities: meta.capabilities,
    status: params.status,
    created_at: now,
    updated_at: now,
  };
  vault = {
    ...vault,
    agents: [...vault.agents, ag],
    agentSecrets: { ...vault.agentSecrets, [id]: rawEnv },
  };
  persistVault();
  return ag;
}

export function patchLocalVaultAgent(id: string, patch: Partial<AcpAgent>): AcpAgent | null {
  let updated: AcpAgent | null = null;
  const now = new Date().toISOString();
  vault = {
    ...vault,
    agents: vault.agents.map((a) => {
      if (a.id !== id) return a;
      updated = { ...a, ...patch, updated_at: now };
      return updated;
    }),
  };
  if (updated) persistVault();
  return updated;
}

export function removeLocalVaultAgent(agentId: string) {
  const nextSecrets = { ...vault.agentSecrets };
  delete nextSecrets[agentId];
  vault = {
    ...vault,
    agents: vault.agents.filter((a) => a.id !== agentId),
    agentSecrets: nextSecrets,
    sessions: vault.sessions.filter((s) => s.agent_id !== agentId),
  };
  persistVault();
}

function deriveWsName(cwd: string, alias?: string): string {
  if (alias?.trim()) return alias.trim().slice(0, 120);
  const seg = cwd
    .trim()
    .replace(/[/\\]+$/, "")
    .split(/[/\\]/)
    .filter(Boolean)
    .pop();
  return (seg || "workspace").slice(0, 120);
}

export function upsertLocalVaultWorkspace(params: {
  cwd: string;
  name?: string;
  branch: string;
  branches: string[];
}): AcpWorkspace {
  const now = new Date().toISOString();
  const existing = vault.workspaces.find((w) => w.cwd === params.cwd);
  if (existing) {
    const updated: AcpWorkspace = {
      ...existing,
      name: params.name?.trim() ? deriveWsName(params.cwd, params.name) : existing.name,
      branch: params.branch || existing.branch,
      branches: params.branches.length ? params.branches : existing.branches,
      updated_at: now,
    };
    vault = {
      ...vault,
      workspaces: [updated, ...vault.workspaces.filter((w) => w.id !== existing.id)],
    };
    persistVault();
    return updated;
  }
  const ws: AcpWorkspace = {
    id: makeUuid(),
    host_id: vault.host?.id ?? "local",
    cwd: params.cwd,
    name: deriveWsName(params.cwd, params.name),
    branch: params.branch,
    branches: params.branches,
    created_at: now,
    updated_at: now,
  };
  vault = { ...vault, workspaces: [ws, ...vault.workspaces] };
  persistVault();
  return ws;
}

export function patchLocalVaultWorkspace(
  wsId: string,
  patch: Partial<AcpWorkspace>,
): AcpWorkspace | null {
  let updated: AcpWorkspace | null = null;
  const now = new Date().toISOString();
  vault = {
    ...vault,
    workspaces: vault.workspaces.map((w) => {
      if (w.id !== wsId) return w;
      updated = { ...w, ...patch, updated_at: now };
      return updated;
    }),
  };
  if (updated) persistVault();
  return updated;
}

export function removeLocalVaultWorkspace(wsId: string) {
  vault = {
    ...vault,
    workspaces: vault.workspaces.filter((w) => w.id !== wsId),
    sessions: vault.sessions.filter((s) => s.workspace_id !== wsId),
  };
  persistVault();
}

export function addLocalVaultSession(params: {
  workspace: AcpWorkspace;
  agent: AcpAgent;
  defaultPermission: AcpPermissionPolicy;
  title?: string;
}): AcpSession {
  const now = new Date().toISOString();
  const perm =
    params.agent.permission_override !== "inherit"
      ? params.agent.permission_override
      : params.defaultPermission;
  const title = (
    params.title?.trim() || `${params.agent.name} · ${params.workspace.name}`
  ).slice(0, 160);
  const sess: AcpSession = {
    id: makeUuid(),
    workspace_id: params.workspace.id,
    agent_id: params.agent.id,
    title,
    selected_model: params.agent.default_model,
    thinking_level: "中" as AcpThinkingLevel,
    permission_policy: perm,
    acp_session_key: null,
    running: false,
    diffs: [],
    rpc_logs: [],
    created_at: now,
    updated_at: now,
    turns: [],
  };
  vault = { ...vault, sessions: [sess, ...vault.sessions] };
  persistVault();
  return sess;
}

export function patchLocalVaultSession(
  sessId: string,
  patch: Partial<AcpSession>,
): AcpSession | null {
  let updated: AcpSession | null = null;
  const now = new Date().toISOString();
  vault = {
    ...vault,
    sessions: vault.sessions.map((s) => {
      if (s.id !== sessId) return s;
      updated = { ...s, ...patch, updated_at: now };
      return updated;
    }),
  };
  if (updated) persistVault();
  return updated;
}

export function removeLocalVaultSession(sessId: string) {
  vault = {
    ...vault,
    sessions: vault.sessions.filter((s) => s.id !== sessId),
  };
  persistVault();
}

export async function stageLocalAttachment(file: File, sessionId: string): Promise<AcpAttachment> {
  if (!file.size) throw new Error("不能上传空附件");
  const att = {
    id: makeUuid(),
    file_name: file.name,
    mime: file.type || "application/octet-stream",
    size: file.size,
  };
  await storeLocalAcpBlob(att.id, file);
  if (!isLocalVaultSession(sessionId)) {
    await removeLocalAcpBlob(att.id);
    throw new Error("未找到本地会话");
  }
  try {
    commitVault({
      ...vault,
      cloudImport: null,
      attachments: { ...vault.attachments, [att.id]: { ...att, session_id: sessionId } },
    });
  } catch (error) {
    await removeLocalAcpBlob(att.id).catch(() => {});
    throw error;
  }
  return att;
}

async function promptAttachment(att: AcpAttachment): Promise<LocalAttachmentPayload> {
  const blob = await readLocalAcpBlob(att.id);
  if (!blob || blob.size !== att.size) throw new Error(`本地附件缺失或不完整：${att.file_name}`);
  const buf = await blob.arrayBuffer();
  const bytes = new Uint8Array(buf);
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return { ...att, data_base64: btoa(binary) };
}

export function getLocalPromptAttachments(attachments: AcpAttachment[]) {
  return Promise.all(attachments.map(promptAttachment));
}

export function appendLocalUserTurnAndStart(
  sessId: string,
  payload: {
    text: string;
    files: string[];
    attachments: AcpAttachment[];
    model: string;
    thinking_level: AcpThinkingLevel;
    permission_policy: AcpPermissionPolicy;
  },
): { session: AcpSession; turn: AcpTurn } | null {
  const cur = vault.sessions.find((s) => s.id === sessId);
  if (!cur) return null;
  if (cur.running) throw new Error("该会话已有运行中的任务");
  const runId = makeUuid();
  const now = new Date().toISOString();
  const turns = cur.turns ?? [];
  const nextSeq = turns.reduce((m, t) => Math.max(m, t.seq), 0) + 1;
  const turn: AcpTurn = {
    id: makeUuid(),
    session_id: sessId,
    seq: nextSeq,
    role: "user",
    status: "done",
    payload: {
      text: payload.text,
      run_id: runId,
      files: payload.files,
      attachments: payload.attachments,
    },
    created_at: now,
    updated_at: now,
  };
  const updated: AcpSession = {
    ...cur,
    running: true,
    run_id: runId,
    selected_model: payload.model,
    thinking_level: payload.thinking_level,
    permission_policy: payload.permission_policy,
    updated_at: now,
    turns: [...turns, turn],
  };
  vault = {
    ...vault,
    sessions: vault.sessions.map((s) => (s.id === sessId ? updated : s)),
  };
  persistVault();
  return { session: updated, turn };
}

export function resolveLocalPermissionTurn(
  turnId: string,
  optionId: string,
): { session: AcpSession; turn: AcpTurn; reqId: string } | null {
  const now = new Date().toISOString();
  for (const s of vault.sessions) {
    const idx = (s.turns ?? []).findIndex((t) => t.id === turnId && t.role === "permission");
    if (idx < 0) continue;
    const prevTurn = s.turns![idx];
    if (!s.running || prevTurn.status !== "pending" || prevTurn.payload.run_id !== s.run_id) {
      return null;
    }
    const reqId = String(prevTurn.payload.req_id ?? "");
    const nextTurn: AcpTurn = {
      ...prevTurn,
      status: optionId.startsWith("allow") ? "approved" : "rejected",
      payload: { ...prevTurn.payload, chosen_option: optionId },
      updated_at: now,
    };
    const nextTurns = [...(s.turns ?? [])];
    nextTurns[idx] = nextTurn;
    const nextSession: AcpSession = { ...s, turns: nextTurns, updated_at: now };
    vault = {
      ...vault,
      sessions: vault.sessions.map((x) => (x.id === s.id ? nextSession : x)),
    };
    persistVault();
    return { session: nextSession, turn: nextTurn, reqId };
  }
  return null;
}

export interface LocalRunnerDelta {
  agent?: AcpAgent;
  workspace?: AcpWorkspace;
  session?: AcpSession;
  activity?: {
    session_id: string;
    phase: AcpActivityPhase;
    detail: string;
    usage?: AcpUsageStats;
  };
}

function updateLocalVaultSessionInMem(
  sessId: string,
  nextSession: AcpSession,
  persist: boolean,
): AcpSession {
  vault = {
    ...vault,
    sessions: vault.sessions.map((s) => (s.id === sessId ? nextSession : s)),
  };
  if (persist) persistVault();
  return nextSession;
}

function normalizeLocalToolStatus(raw: unknown): string | undefined {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (
    trimmed === "pending" ||
    trimmed === "in_progress" ||
    trimmed === "completed" ||
    trimmed === "failed"
  ) {
    return trimmed;
  }
  return undefined;
}

function mergeLocalToolCallPayload(
  existing: Record<string, unknown> | undefined,
  raw: Record<string, unknown>,
  toolCallId: string,
): Record<string, unknown> {
  const name =
    typeof raw.name === "string" && raw.name.trim()
      ? raw.name.trim().slice(0, 120)
      : String(existing?.name ?? "");

  const rawKind =
    typeof raw.tool_kind === "string"
      ? raw.tool_kind
      : typeof raw.kind === "string" && raw.kind !== "tool_call"
        ? raw.kind
        : "";
  const kind = rawKind.trim()
    ? rawKind.trim().slice(0, 40)
    : String(existing?.kind ?? "other");

  const title =
    typeof raw.title === "string" && raw.title.trim()
      ? raw.title.trim().slice(0, 300)
      : String(existing?.title ?? "");

  let input = String(existing?.input ?? "");
  if (typeof raw.input === "string" && raw.input.length > 0) {
    input = raw.input.slice(0, 16_384);
  } else if (raw.input && typeof raw.input === "object") {
    try {
      const s = JSON.stringify(raw.input, null, 2);
      if (s) input = s.slice(0, 16_384);
    } catch {}
  }

  const prevOutput = String(existing?.output ?? "");
  let output = prevOutput;
  if (typeof raw.output === "string" && raw.output.length > 0) {
    output = raw.output.slice(0, 65_536);
  } else if (typeof raw.output_delta === "string" && raw.output_delta.length > 0) {
    output = (prevOutput + raw.output_delta).slice(0, 65_536);
  }

  const locations =
    Array.isArray(raw.locations) && raw.locations.length > 0
      ? raw.locations
          .map((x) => String(x ?? "").trim())
          .filter(Boolean)
          .slice(0, 20)
      : Array.isArray(existing?.locations)
        ? (existing.locations as string[])
        : [];

  let diffs: AcpToolDiff[] = Array.isArray(existing?.diffs)
    ? (existing.diffs as AcpToolDiff[])
    : [];
  if (Array.isArray(raw.diffs) && raw.diffs.length > 0) {
    const parsed: AcpToolDiff[] = [];
    for (const d of raw.diffs.slice(0, 10)) {
      if (!d || typeof d !== "object") continue;
      const obj = d as Record<string, unknown>;
      const path = String(obj.path ?? "").trim();
      if (!path) continue;
      parsed.push({
        path: path.slice(0, 300),
        old_text:
          typeof obj.old_text === "string" ? obj.old_text.slice(0, 32_768) : null,
        new_text: String(obj.new_text ?? "").slice(0, 32_768),
      });
    }
    if (parsed.length > 0) diffs = parsed;
  }

  const duration_ms =
    typeof raw.duration_ms === "number" && Number.isFinite(raw.duration_ms)
      ? raw.duration_ms
      : typeof existing?.duration_ms === "number"
        ? existing.duration_ms
        : undefined;

  const statusNorm = normalizeLocalToolStatus(raw.status);
  const is_error =
    typeof raw.is_error === "boolean"
      ? raw.is_error
      : statusNorm === "failed"
        ? true
        : statusNorm === "completed"
          ? false
          : Boolean(existing?.is_error ?? false);

  return {
    tool_call_id: toolCallId,
    name,
    kind,
    title,
    input,
    output,
    locations,
    diffs,
    is_error,
    ...(duration_ms !== undefined ? { duration_ms } : {}),
  };
}

function upsertLocalStreamTurn(
  cur: AcpSession,
  sessId: string,
  turns: AcpTurn[],
  role: "agent_text" | "agent_thought",
  kind: string,
  raw: Record<string, unknown>,
  now: string,
): AcpSession {
  const turnKey = String(raw.turn_key ?? "");
  const delta = typeof raw.delta === "string" ? raw.delta : "";
  const fullText = typeof raw.text === "string" ? raw.text : null;
  const done =
    typeof raw.done === "boolean"
      ? raw.done
      : kind === "text" || kind === "thought";
  const status = done ? "done" : "streaming";
  const durationMs =
    typeof raw.duration_ms === "number" && Number.isFinite(raw.duration_ms)
      ? raw.duration_ms
      : undefined;
  const idx = turnKey
    ? turns.findIndex(
        (t) => t.role === role && t.payload.turn_key === turnKey && t.payload.run_id === cur.run_id,
      )
    : -1;
  const nextSeq = () => turns.reduce((m, t) => Math.max(m, t.seq), 0) + 1;

  if (idx >= 0) {
    const prevPayload = turns[idx].payload;
    const prevText = String(prevPayload.text ?? "");
    const nextDuration =
      durationMs !== undefined ? durationMs : prevPayload.duration_ms;
    turns[idx] = {
      ...turns[idx],
      status,
      payload: {
        turn_key: turnKey,
        run_id: cur.run_id,
        text: fullText !== null ? fullText : prevText + delta,
        ...(nextDuration !== undefined ? { duration_ms: nextDuration } : {}),
      },
      updated_at: now,
    };
  } else {
    turns.push({
      id: makeUuid(),
      session_id: sessId,
      seq: nextSeq(),
      role,
      status,
      payload: {
        turn_key: turnKey,
        run_id: cur.run_id,
        text: fullText ?? delta,
        ...(durationMs !== undefined ? { duration_ms: durationMs } : {}),
      },
      created_at: now,
      updated_at: now,
    });
  }

  // 流式增量不每次写满 localStorage，结束时再统一落盘
  const nextSession: AcpSession = { ...cur, turns, updated_at: now };
  return updateLocalVaultSessionInMem(sessId, nextSession, done);
}

function settleSessionTurns(
  turns: AcpTurn[],
  options: { hasError?: boolean; cancelPendingPermissions?: boolean },
  now: string,
): AcpTurn[] {
  return turns.map((t) => {
    if (
      (t.role === "agent_text" || t.role === "agent_thought") &&
      t.status === "streaming"
    ) {
      return { ...t, status: "done", updated_at: now };
    }
    if (
      t.role === "tool_call" &&
      (t.status === "pending" || t.status === "in_progress")
    ) {
      return {
        ...t,
        status: options.hasError ? "failed" : "completed",
        payload: options.hasError
          ? { ...t.payload, is_error: true }
          : t.payload,
        updated_at: now,
      };
    }
    if (
      options.cancelPendingPermissions &&
      t.role === "permission" &&
      t.status === "pending"
    ) {
      return {
        ...t,
        status: "rejected",
        payload: { ...t.payload, chosen_option: "cancelled" },
        updated_at: now,
      };
    }
    return t;
  });
}

export function settleLocalSessionOnCancel(
  sessId: string,
  expectedRunId?: string | null,
): AcpSession | null {
  const cur = vault.sessions.find((s) => s.id === sessId);
  if (!cur?.running || (expectedRunId != null && cur.run_id !== expectedRunId)) {
    return cur ?? null;
  }
  const now = new Date().toISOString();
  const settledTurns = settleSessionTurns(
    cur.turns ?? [],
    { hasError: true, cancelPendingPermissions: true },
    now,
  );
  return patchLocalVaultSession(sessId, {
    running: false,
    run_id: null,
    turns: settledTurns,
  });
}

export function applyLocalRunnerEvent(raw: Record<string, unknown>): LocalRunnerDelta {
  const type = String(raw.type ?? "");
  const sessId = typeof raw.session_id === "string" ? raw.session_id : "";
  const curSession = sessId ? vault.sessions.find((s) => s.id === sessId) : undefined;
  if (
    (type === "session_update" || raw.run_id != null) &&
    (!curSession?.running || !curSession.run_id || curSession.run_id !== raw.run_id)
  ) {
    return {};
  }
  const now = new Date().toISOString();

  if (type === "agent_status") {
    const agentId = String(raw.agent_id ?? "");
    const status = String(raw.status ?? "standby");
    const ag = patchLocalVaultAgent(agentId, { status });
    return ag ? { agent: ag } : {};
  }

  if (type === "agent_models") {
    const agentId = String(raw.agent_id ?? "");
    const opts = Array.isArray(raw.model_options)
      ? raw.model_options.map(String).filter(Boolean)
      : [];
    if (!opts.length) return {};
    const cur = vault.agents.find((a) => a.id === agentId);
    if (!cur) return {};
    const def =
      typeof raw.default_model === "string" && opts.includes(raw.default_model)
        ? raw.default_model
        : opts[0];
    const ag = patchLocalVaultAgent(agentId, {
      status: "online",
      default_model: def,
      model_options: opts,
    });
    return ag ? { agent: ag } : {};
  }

  if (type !== "session_update" || !curSession) return {};
  const cur = curSession;
  const kind = String(raw.kind ?? "");
  const turns = [...(cur.turns ?? [])];
  const nextSeq = () => turns.reduce((m, t) => Math.max(m, t.seq), 0) + 1;

  if (kind === "plan") {
    const runKey = String(raw.run_key ?? "");
    const payload = { run_key: runKey, run_id: cur.run_id, steps: raw.steps ?? [] };
    const idx = runKey
      ? turns.findIndex(
          (t) => t.role === "agent_plan" && t.payload.run_key === runKey && t.payload.run_id === cur.run_id,
        )
      : -1;
    if (idx >= 0) {
      turns[idx] = { ...turns[idx], payload, status: "done", updated_at: now };
    } else {
      turns.push({
        id: makeUuid(),
        session_id: sessId,
        seq: nextSeq(),
        role: "agent_plan",
        status: "done",
        payload,
        created_at: now,
        updated_at: now,
      });
    }
    const session = patchLocalVaultSession(sessId, { turns })!;
    return { session };
  }

  if (kind === "text_delta" || kind === "text") {
    return {
      session: upsertLocalStreamTurn(cur, sessId, turns, "agent_text", kind, raw, now),
    };
  }

  if (kind === "thought_delta" || kind === "thought") {
    return {
      session: upsertLocalStreamTurn(cur, sessId, turns, "agent_thought", kind, raw, now),
    };
  }

  if (kind === "tool_call") {
    const toolCallId =
      typeof raw.tool_call_id === "string" && raw.tool_call_id.trim()
        ? raw.tool_call_id.trim().slice(0, 128)
        : makeUuid();
    const idx = turns.findIndex(
      (t) => t.role === "tool_call" && t.payload.tool_call_id === toolCallId && t.payload.run_id === cur.run_id,
    );
    const prev = idx >= 0 ? turns[idx] : undefined;
    const status =
      normalizeLocalToolStatus(raw.status) ?? prev?.status ?? "in_progress";
    const payload = { ...mergeLocalToolCallPayload(prev?.payload, raw, toolCallId), run_id: cur.run_id };
    if (idx >= 0) {
      turns[idx] = { ...turns[idx], status, payload, updated_at: now };
    } else {
      turns.push({
        id: makeUuid(),
        session_id: sessId,
        seq: nextSeq(),
        role: "tool_call",
        status,
        payload,
        created_at: now,
        updated_at: now,
      });
    }
    const isTerminal = status === "completed" || status === "failed";
    const nextSession: AcpSession = { ...cur, turns, updated_at: now };
    return {
      session: updateLocalVaultSessionInMem(sessId, nextSession, isTerminal),
    };
  }

  if (kind === "activity") {
    const phase = String(raw.phase ?? "idle").trim() as AcpActivityPhase;
    const detail = String(raw.detail ?? "").trim();
    const usage =
      raw.usage && typeof raw.usage === "object" && !Array.isArray(raw.usage)
        ? (raw.usage as AcpUsageStats)
        : undefined;
    if (!phase && !usage) return {};
    return {
      activity: {
        session_id: sessId,
        phase: phase || "idle",
        detail,
        ...(usage ? { usage } : {}),
      },
    };
  }

  if (kind === "terminal_out") {
    turns.push({
      id: makeUuid(),
      session_id: sessId,
      seq: nextSeq(),
      role: "terminal_out",
      status: "done",
      payload: {
        command: String(raw.command ?? ""),
        output: String(raw.output ?? ""),
        exit_code: Number(raw.exit_code ?? 0),
      },
      created_at: now,
      updated_at: now,
    });
    const session = patchLocalVaultSession(sessId, { turns })!;
    return { session };
  }

  if (kind === "permission_request") {
    turns.push({
      id: makeUuid(),
      session_id: sessId,
      seq: nextSeq(),
      role: "permission",
      status: "pending",
      payload: {
        req_id: String(raw.req_id ?? ""),
        run_id: cur.run_id,
        tool_kind: String(raw.tool_kind ?? "tool"),
        command: String(raw.command ?? ""),
        cwd: String(raw.cwd ?? ""),
        reason: String(raw.reason ?? "请求执行需要确认的工具操作"),
        chosen_option: "",
      },
      created_at: now,
      updated_at: now,
    });
    const session = patchLocalVaultSession(sessId, { turns })!;
    return { session };
  }

  if (kind === "diffs") {
    const diffs = Array.isArray(raw.diffs) ? (raw.diffs as AcpDiffItem[]) : [];
    const session = patchLocalVaultSession(sessId, { diffs })!;
    return { session };
  }

  if (kind === "rpc_log") {
    const entry = raw.entry as AcpRpcLogEntry | undefined;
    if (!entry) return {};
    const rpc_logs = [...cur.rpc_logs, entry].slice(-50);
    const nextSession: AcpSession = { ...cur, rpc_logs, updated_at: now };
    vault = {
      ...vault,
      sessions: vault.sessions.map((s) => (s.id === sessId ? nextSession : s)),
    };
    return { session: nextSession };
  }

  if (kind === "prompt_done") {
    const acpKey =
      typeof raw.acp_session_key === "string" && raw.acp_session_key
        ? raw.acp_session_key
        : cur.acp_session_key;
    const errText = typeof raw.error === "string" ? raw.error.trim() : "";
    const settledTurns = settleSessionTurns(
      turns,
      { hasError: Boolean(errText), cancelPendingPermissions: true },
      now,
    );
    if (errText) {
      settledTurns.push({
        id: makeUuid(),
        session_id: sessId,
        seq: settledTurns.reduce((m, t) => Math.max(m, t.seq), 0) + 1,
        role: "agent_text",
        status: "error",
        payload: { text: `执行中断：${errText}` },
        created_at: now,
        updated_at: now,
      });
    }
    const session = patchLocalVaultSession(sessId, {
      running: false,
      run_id: null,
      acp_session_key: acpKey,
      turns: settledTurns,
    })!;

    let workspace: AcpWorkspace | undefined;
    const branch = typeof raw.branch === "string" ? raw.branch : "";
    const branches = Array.isArray(raw.branches) ? raw.branches.map(String) : [];
    if (branch || branches.length) {
      workspace =
        patchLocalVaultWorkspace(cur.workspace_id, {
          ...(branch ? { branch } : {}),
          ...(branches.length ? { branches } : {}),
        }) ?? undefined;
    }
    return {
      session,
      workspace,
      activity: { session_id: sessId, phase: "idle", detail: "" },
    };
  }

  return {};
}

export function resetLocalVaultForTests() {
  vault = emptyVault();
  persistVault();
}
