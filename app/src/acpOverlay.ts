import type {
  AcpAgent,
  AcpPermissionPolicy,
  AcpSession,
  AcpSettings,
  AcpThinkingLevel,
  AcpWorkspace,
} from "./types";

export interface PendingAgentPatch {
  name?: string;
  cmd?: string;
  default_model?: string;
  model_options?: string[];
  permission_override?: "inherit" | AcpPermissionPolicy;
}

export interface PendingSessionPatch {
  title?: string;
  selected_model?: string;
  thinking_level?: AcpThinkingLevel;
  permission_policy?: AcpPermissionPolicy;
}

export interface PendingWorkspacePatch {
  branch?: string;
  branches?: string[];
}

interface OverlayStore {
  settings: AcpSettings | null;
  agents: Record<string, PendingAgentPatch>;
  sessions: Record<string, PendingSessionPatch>;
  workspaces: Record<string, PendingWorkspacePatch>;
}

const STORAGE_KEY = "chat_acp_pending_overlay_v1";

function loadOverlayStore(): OverlayStore {
  if (typeof localStorage === "undefined") {
    return { settings: null, agents: {}, sessions: {}, workspaces: {} };
  }
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { settings: null, agents: {}, sessions: {}, workspaces: {} };
    const parsed = JSON.parse(raw) as Partial<OverlayStore>;
    return {
      settings: parsed.settings ?? null,
      agents: parsed.agents ?? {},
      sessions: parsed.sessions ?? {},
      workspaces: parsed.workspaces ?? {},
    };
  } catch {
    return { settings: null, agents: {}, sessions: {}, workspaces: {} };
  }
}

let mem: OverlayStore = loadOverlayStore();

function persist() {
  if (typeof localStorage === "undefined") return;
  try {
    const empty =
      !mem.settings &&
      Object.keys(mem.agents).length === 0 &&
      Object.keys(mem.sessions).length === 0 &&
      Object.keys(mem.workspaces).length === 0;
    if (empty) {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(mem));
    }
  } catch {}
}

export function setPendingSettingsPatch(settings: AcpSettings | null) {
  mem = { ...mem, settings };
  persist();
}

export function getPendingSettingsPatch(): AcpSettings | null {
  return mem.settings;
}

export function mergePendingAgentPatch(id: string, patch: PendingAgentPatch): PendingAgentPatch {
  const next = { ...(mem.agents[id] ?? {}), ...patch };
  mem = { ...mem, agents: { ...mem.agents, [id]: next } };
  persist();
  return next;
}

export function clearPendingAgentPatch(id: string) {
  if (!(id in mem.agents)) return;
  const nextAgents = { ...mem.agents };
  delete nextAgents[id];
  mem = { ...mem, agents: nextAgents };
  persist();
}

export function getPendingAgentPatches(): Record<string, PendingAgentPatch> {
  return mem.agents;
}

export function mergePendingSessionPatch(
  id: string,
  patch: PendingSessionPatch,
): PendingSessionPatch {
  const next = { ...(mem.sessions[id] ?? {}), ...patch };
  mem = { ...mem, sessions: { ...mem.sessions, [id]: next } };
  persist();
  return next;
}

export function clearPendingSessionPatch(id: string) {
  if (!(id in mem.sessions)) return;
  const nextSessions = { ...mem.sessions };
  delete nextSessions[id];
  mem = { ...mem, sessions: nextSessions };
  persist();
}

export function getPendingSessionPatches(): Record<string, PendingSessionPatch> {
  return mem.sessions;
}

export function mergePendingWorkspacePatch(
  id: string,
  patch: PendingWorkspacePatch,
): PendingWorkspacePatch {
  const next = { ...(mem.workspaces[id] ?? {}), ...patch };
  mem = { ...mem, workspaces: { ...mem.workspaces, [id]: next } };
  persist();
  return next;
}

export function clearPendingWorkspacePatch(id: string) {
  if (!(id in mem.workspaces)) return;
  const nextWs = { ...mem.workspaces };
  delete nextWs[id];
  mem = { ...mem, workspaces: nextWs };
  persist();
}

export function applySettingsOverlay(settings: AcpSettings): AcpSettings {
  return mem.settings ? { ...settings, ...mem.settings } : settings;
}

export function applyAgentOverlay(agent: AcpAgent): AcpAgent {
  const patch = mem.agents[agent.id];
  return patch ? { ...agent, ...patch } : agent;
}

export function applySessionOverlay(session: AcpSession): AcpSession {
  const patch = mem.sessions[session.id];
  return patch ? { ...session, ...patch } : session;
}

export function applyWorkspaceOverlay(ws: AcpWorkspace): AcpWorkspace {
  const patch = mem.workspaces[ws.id];
  return patch ? { ...ws, ...patch } : ws;
}

export function resetAcpOverlayForTests() {
  mem = { settings: null, agents: {}, sessions: {}, workspaces: {} };
  persist();
}
