import {
  activateLocalVault,
  addLocalVaultAgent,
  addLocalVaultSession,
  appendLocalUserTurnAndStart,
  applyLocalRunnerEvent,
  beginLocalVaultImport,
  completeLocalVaultImport,
  confirmLocalVaultCloudRemoval,
  ensureLocalOnlyHost,
  getLocalVault,
  getLocalVaultRawEnv,
  isLocalOnlyActive,
  isLocalVaultAgent,
  isLocalVaultHost,
  isLocalVaultSession,
  isLocalVaultWorkspace,
  localVaultOwnerMatches,
  patchLocalVaultAgent,
  patchLocalVaultSession,
  patchLocalVaultWorkspace,
  removeLocalVaultAgent,
  removeLocalVaultSession,
  removeLocalVaultWorkspace,
  renameLocalVaultHost,
  resolveLocalPermissionTurn,
  settleLocalSessionOnCancel,
  getLocalPromptAttachments,
  stageLocalAttachment,
  upsertLocalVaultWorkspace,
} from "./acpLocalVault";
import { readLocalAcpBlob, storeLocalAcpBlob } from "./acpAttachmentStore";
import {
  clearPendingAgentPatch,
  clearPendingSessionPatch,
  clearPendingWorkspacePatch,
  getPendingAgentPatches,
  getPendingSessionPatches,
  getPendingSettingsPatch,
  mergePendingAgentPatch,
  mergePendingSessionPatch,
  mergePendingWorkspacePatch,
  setPendingSettingsPatch,
  type PendingAgentPatch,
  type PendingSessionPatch,
} from "./acpOverlay";
import { acp, convs, getServer, isDesktopShell, serverScope, session } from "./api";
import { applyAcpBootstrap, applyAcpEvent, type AppState, getState, setState } from "./store";
import type {
  AcpAgent,
  AcpAttachment,
  AcpDirListing,
  AcpHost,
  AcpHostFileContent,
  AcpHostSnapshot,
  AcpPermissionPolicy,
  AcpSession,
  AcpThinkingLevel,
  AcpTurn,
  AcpWorkspace,
} from "./types";
import { confirmDialog } from "./ui/Modal";

let unlistenTicket: (() => void) | null = null;
let unlistenConfirm: (() => void) | null = null;
let unlistenStatus: (() => void) | null = null;
let unlistenLocalEvent: (() => void) | null = null;
let startedServer: string | null = null;
let cloudTransferRunning = false;

let memLocalHostId: string | null = null;
let memLocalMeta: { install_id: string; name: string; os: string; roots: string[] } | null = null;
const hostReadyWaiters = new Set<(hostId: string) => void>();

const LOCAL_HOST_KEY_PREFIX = "chat_acp_local_host_v1:";

function readStoredLocalHostId(): string | null {
  if (typeof localStorage === "undefined") return null;
  try {
    return localStorage.getItem(LOCAL_HOST_KEY_PREFIX + serverScope()) || null;
  } catch {
    return null;
  }
}

export function setLocalAcpHostId(hostId: string | null) {
  memLocalHostId = hostId;
  if (typeof localStorage === "undefined") return;
  try {
    const key = LOCAL_HOST_KEY_PREFIX + serverScope();
    if (hostId) {
      localStorage.setItem(key, hostId);
    } else {
      localStorage.removeItem(key);
    }
  } catch {}
}

export function getLocalAcpHostId(): string | null {
  if (isLocalOnlyActive()) {
    return getLocalVault().host?.id ?? null;
  }
  return memLocalHostId ?? readStoredLocalHostId();
}

export function isLocalAcpHost(hostId: string | null | undefined): boolean {
  if (!isDesktopShell() || !hostId) return false;
  if (isLocalVaultHost(hostId)) return true;
  const known = getLocalAcpHostId();
  if (known && hostId === known) return true;
  if (!known && memLocalMeta) {
    const h = getState().acpHosts.find((x) => x.id === hostId);
    if (
      h &&
      (h.ip === "Client-Daemon" || h.ip === "Local-Only") &&
      h.os === memLocalMeta.os &&
      h.name === memLocalMeta.name
    ) {
      if (h.ip === "Client-Daemon") setLocalAcpHostId(h.id);
      return true;
    }
  }
  return false;
}

export interface LocalPolicyView {
  allow_all_local: boolean;
  cloud_control_enabled: boolean;
  approved_count: number;
}

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export async function getLocalPolicy(): Promise<LocalPolicyView> {
  if (!isDesktopShell()) {
    return { allow_all_local: false, cloud_control_enabled: true, approved_count: 0 };
  }
  try {
    const res = await tauriInvoke<LocalPolicyView>("acp_host_local_policy");
    return {
      allow_all_local: Boolean(res?.allow_all_local),
      cloud_control_enabled: res?.cloud_control_enabled ?? true,
      approved_count: res?.approved_count ?? 0,
    };
  } catch {
    return { allow_all_local: false, cloud_control_enabled: true, approved_count: 0 };
  }
}

export async function getLocalAllowAll(): Promise<boolean> {
  const p = await getLocalPolicy();
  return p.allow_all_local;
}

export async function setLocalAllowAll(enabled: boolean): Promise<LocalPolicyView> {
  return tauriInvoke<LocalPolicyView>("acp_host_set_local_allow_all", { enabled });
}

async function fetchLocalHostMeta(): Promise<{
  host_id: string | null;
  install_id: string;
  name: string;
  os: string;
  roots: string[];
}> {
  try {
    const info = await tauriInvoke<{
      host_id?: string | null;
      install_id?: string;
      name?: string;
      os?: string;
      roots?: string[];
    }>("acp_host_info");
    const meta = {
      host_id: info?.host_id ?? null,
      install_id: info?.install_id || "local-host",
      name: info?.name || "Desktop",
      os: info?.os || "",
      roots: Array.isArray(info?.roots) ? info.roots : [],
    };
    memLocalMeta = {
      install_id: meta.install_id,
      name: meta.name,
      os: meta.os,
      roots: meta.roots,
    };
    return meta;
  } catch {
    return {
      host_id: null,
      install_id: "local-host",
      name: "Desktop",
      os: "",
      roots: [],
    };
  }
}

async function ensureLocalEventBridge(): Promise<void> {
  if (!isDesktopShell() || unlistenLocalEvent) return;
  try {
    const { listen } = await import("@tauri-apps/api/event");
    unlistenLocalEvent = await listen<Record<string, unknown>>(
      "acp-host:local-event",
      (ev) => {
        if (!ev.payload) return;
        const current = getState().me?.id
          ? JSON.stringify([getServer(), getState().me!.id])
          : null;
        if (!current || !localVaultOwnerMatches(current)) return;
        const delta = applyLocalRunnerEvent(ev.payload);
        if (delta.agent || delta.workspace || delta.session || delta.activity) {
          setState((cur) => {
            const patch: Partial<AppState> = {
              ...(delta.agent
                ? {
                    acpAgents: cur.acpAgents.map((a) =>
                      a.id === delta.agent!.id ? delta.agent! : a,
                    ),
                  }
                : {}),
              ...(delta.workspace
                ? {
                    acpWorkspaces: cur.acpWorkspaces.map((w) =>
                      w.id === delta.workspace!.id ? delta.workspace! : w,
                    ),
                  }
                : {}),
              ...(delta.session
                ? {
                    acpSessions: cur.acpSessions.map((s) =>
                      s.id === delta.session!.id ? delta.session! : s,
                    ),
                  }
                : {}),
            };
            const mergedCur: AppState = { ...cur, ...patch };
            if (delta.activity) {
              Object.assign(
                patch,
                applyAcpEvent(mergedCur, "session_activity", delta.activity),
              );
            } else if (delta.session && !delta.session.running) {
              Object.assign(
                patch,
                applyAcpEvent(mergedCur, "session_activity", {
                  session_id: delta.session.id,
                  phase: "idle",
                  detail: "",
                }),
              );
            }
            return patch;
          });
        }
      },
    );
  } catch {}
}

function assertCompleteSnapshot(snapshot: AcpHostSnapshot) {
  if (snapshot.sessions.some((item) => item.running)) {
    throw new Error("请先结束本机正在运行的 ACP 任务，再切换云端控制");
  }
  const fileIds = new Set(snapshot.attachments.map((att) => att.id));
  for (const item of snapshot.sessions) {
    for (const turn of item.turns ?? []) {
      if (!Array.isArray(turn.payload.attachments)) continue;
      for (const att of turn.payload.attachments) {
        if (!att || typeof att.id !== "string" || !fileIds.has(att.id)) {
          throw new Error("ACP 历史记录引用了缺失的附件，已保留原始数据");
        }
      }
    }
  }
}

function showLocalVault(vault: ReturnType<typeof getLocalVault>, cloudHostId: string | null) {
  const host = vault.host!;
  const workspaceIds = new Set(vault.workspaces.map((item) => item.id));
  setState((cur) => {
    const excluded = new Set(
      cur.acpWorkspaces.filter((item) =>
        item.host_id === cloudHostId || item.host_id === host.id,
      ).map((item) => item.id),
    );
    return {
      acpHosts: [host, ...cur.acpHosts.filter((item) => item.id !== cloudHostId && item.id !== host.id)],
      acpAgents: [...vault.agents, ...cur.acpAgents.filter((item) =>
        item.host_id !== cloudHostId && item.host_id !== host.id)],
      acpWorkspaces: [...vault.workspaces, ...cur.acpWorkspaces.filter((item) =>
        item.host_id !== cloudHostId && item.host_id !== host.id)],
      acpSessions: [...vault.sessions, ...cur.acpSessions.filter((item) =>
        !workspaceIds.has(item.workspace_id) && !excluded.has(item.workspace_id))],
      activeAcpHostId: cur.activeAcpHostId === cloudHostId ? host.id : (cur.activeAcpHostId ?? host.id),
    };
  });
}

function waitForCloudHost(): { ready: Promise<void>; stop: () => void } {
  let stop = () => {};
  const ready = new Promise<void>((resolve) => {
    const onReady = () => {
      stop();
      resolve();
    };
    const timeout = setTimeout(onReady, 6000);
    stop = () => {
      clearTimeout(timeout);
      hostReadyWaiters.delete(onReady);
    };
    hostReadyWaiters.add(onReady);
  });
  return { ready, stop };
}

export async function setCloudControlEnabled(enabled: boolean): Promise<LocalPolicyView> {
  if (!isDesktopShell()) {
    return { allow_all_local: false, cloud_control_enabled: true, approved_count: 0 };
  }
  if (cloudTransferRunning) throw new Error("已有 ACP 数据切换正在进行");
  cloudTransferRunning = true;
  try {
    return await transferCloudControl(enabled);
  } finally {
    cloudTransferRunning = false;
  }
}

async function transferCloudControl(enabled: boolean): Promise<LocalPolicyView> {
  const generation = session();
  const serverUrl = getServer();
  const owner = JSON.stringify([serverUrl, getState().me?.id]);
  if (getLocalVault().owner && getLocalVault().owner !== owner) {
    throw new Error("请切回原账号继续同步本地 ACP 数据");
  }
  const ensureCurrentAccount = () => {
    if (generation !== session() || serverUrl !== getServer()) {
      throw new Error("登录账号已变更，已中止 ACP 数据切换");
    }
  };
  await ensureLocalEventBridge();
  const meta = await fetchLocalHostMeta();
  ensureCurrentAccount();
  if (meta.install_id === "local-host") throw new Error("无法确认本机 ACP 宿主身份");

  async function deleteSavedCloudSource() {
    const source = getLocalVault().cloudSource;
    if (!source) return;
    if (source.owner !== owner) throw new Error("请切回原账号继续同步本地 ACP 数据");
    ensureCurrentAccount();
    await acp.deleteHostSnapshot(meta.install_id, source.revision);
    ensureCurrentAccount();
    confirmLocalVaultCloudRemoval();
  }

  if (!enabled) {
    let cloudHostId = getLocalVault().cloudSource?.hostId ?? meta.host_id;
    if (!isLocalOnlyActive()) {
      const exported = await acp.exportHostSnapshot(meta.install_id);
      ensureCurrentAccount();
      const snapshot = exported?.snapshot;
      if (snapshot) {
        cloudHostId = snapshot.host.id;
        assertCompleteSnapshot(snapshot);
        for (const item of snapshot.attachments) {
          const blob = await acp.fetchFileBlob(item.id);
          ensureCurrentAccount();
          if (blob.size !== item.size) throw new Error(`云端附件不完整：${item.file_name}`);
          await storeLocalAcpBlob(item.id, blob);
        }
      }
      ensureCurrentAccount();
      const host: AcpHost = snapshot
        ? { ...snapshot.host, id: meta.install_id }
        : {
            id: meta.install_id,
            name: meta.name,
            os: meta.os,
            ip: "Local-Only",
            roots: meta.roots,
            online: true,
            latency_ms: 0,
            last_seen: new Date().toISOString(),
          };
      const sourceAgents = snapshot?.agents ?? [];
      activateLocalVault({
        host,
        agents: sourceAgents.map((agent) => ({
          ...agent,
          env: Object.fromEntries(Object.entries(agent.env).map(([key, value]) => [key, value ? "••••••••" : ""])),
        })),
        agentSecrets: Object.fromEntries(sourceAgents.map((agent) => [agent.id, agent.env])),
        workspaces: snapshot?.workspaces ?? [],
        sessions: snapshot?.sessions ?? [],
        attachments: snapshot?.attachments ?? [],
        owner,
        cloudSource: exported ? { owner, hostId: exported.snapshot.host.id, revision: exported.revision } : null,
      });
    }
    const policy = await tauriInvoke<LocalPolicyView>("acp_host_set_cloud_control", { enabled: false });
    ensureCurrentAccount();
    startedServer = null;
    setLocalAcpHostId(null);
    showLocalVault(getLocalVault(), cloudHostId);
    try {
      await deleteSavedCloudSource();
    } catch (error) {
      throw new Error(`本地快照已保存，云端副本仍待清理：${error instanceof Error ? error.message : String(error)}`);
    }
    return policy;
  }

  const currentVault = getLocalVault();
  if (!currentVault.active || !currentVault.host) {
    const policy = await tauriInvoke<LocalPolicyView>("acp_host_set_cloud_control", { enabled: true });
    ensureCurrentAccount();
    await startAcpHost(serverUrl);
    return policy;
  }
  await deleteSavedCloudSource();
  const active = getLocalVault();
  const snapshot: AcpHostSnapshot = {
    host: active.host!,
    agents: active.agents.map((agent) => ({ ...agent, env: active.agentSecrets[agent.id] ?? {} })),
    workspaces: active.workspaces,
    sessions: active.sessions,
    attachments: Object.values(active.attachments).filter((att) =>
      active.sessions.some((item) => item.id === att.session_id),
    ),
  };
  assertCompleteSnapshot(snapshot);
  const transferId = beginLocalVaultImport(owner);
  const watch = waitForCloudHost();
  try {
    const policy = await tauriInvoke<LocalPolicyView>("acp_host_set_cloud_control", { enabled: true });
    ensureCurrentAccount();
    await startAcpHost(serverUrl);
    ensureCurrentAccount();
    let target = await acp.exportHostSnapshot(meta.install_id);
    if (!target?.snapshot.host.online) {
      await watch.ready;
      ensureCurrentAccount();
      target = await acp.exportHostSnapshot(meta.install_id);
    }
    if (!target?.snapshot.host.online) throw new Error("本机还未连上云端，已保留本地快照");
    ensureCurrentAccount();

    if (target.import_id !== transferId) {
      if (target.snapshot.agents.length || target.snapshot.workspaces.length) {
        throw new Error("云端宿主已有数据，已保留本地快照");
      }
      for (const item of snapshot.attachments) {
        const blob = await readLocalAcpBlob(item.id);
        ensureCurrentAccount();
        if (!blob || blob.size !== item.size) throw new Error(`本地附件缺失或不完整：${item.file_name}`);
        const copied = await acp.uploadTransferFile(item, blob);
        if (copied.id !== item.id || copied.size !== item.size ||
            copied.file_name !== item.file_name || copied.mime !== item.mime) {
          throw new Error(`云端附件校验失败：${item.file_name}`);
        }
      }
      ensureCurrentAccount();
      if (getLocalVault().cloudImport?.id !== transferId) {
        throw new Error("本地数据在同步期间发生变化，已保留本地快照");
      }
      const receipt = await acp.importHostSnapshot(meta.install_id, transferId, snapshot);
      if (receipt.host_id !== target.snapshot.host.id || receipt.transfer_id !== transferId) {
        throw new Error("未收到正确的 ACP 导入确认，已保留本地快照");
      }
    } else {
      const copiedFiles = new Set(target.snapshot.attachments.map((att) => att.id));
      if (snapshot.attachments.some((att) => !copiedFiles.has(att.id))) {
        throw new Error("云端附件记录已发生变化，已保留本地快照");
      }
    }
    ensureCurrentAccount();
    const bootstrap = await acp.bootstrap();
    ensureCurrentAccount();
    const hostId = target.snapshot.host.id;
    const agentIds = new Set(bootstrap.agents.filter((item) => item.host_id === hostId).map((item) => item.id));
    const wsIds = new Set(bootstrap.workspaces.filter((item) => item.host_id === hostId).map((item) => item.id));
    const cloudSessions = new Map(bootstrap.sessions.filter((item) => wsIds.has(item.workspace_id)).map((item) => [item.id, item]));
    if (!bootstrap.hosts.some((item) => item.id === hostId)
      || snapshot.agents.some((item) => !agentIds.has(item.id))
      || snapshot.workspaces.some((item) => !wsIds.has(item.id))
      || snapshot.sessions.some((item) => {
        const remote = cloudSessions.get(item.id);
        const remoteTurnIds = new Set(remote?.turns?.map((turn) => turn.id));
        return !remote || item.turns?.some((turn) => !remoteTurnIds.has(turn.id));
      })) {
      throw new Error("云端快照校验失败，已保留本地快照");
    }
    if (getLocalVault().cloudImport?.id !== transferId) {
      throw new Error("本地数据在同步期间发生变化，已保留本地快照");
    }
    completeLocalVaultImport();
    setLocalAcpHostId(hostId);
    setState((cur) => applyAcpBootstrap(cur, bootstrap));
    return policy;
  } catch (error) {
    if (generation === session() && serverUrl === getServer()) {
      await tauriInvoke("acp_host_set_cloud_control", { enabled: false }).catch(() => {});
      startedServer = null;
      setLocalAcpHostId(null);
    }
    throw error;
  } finally {
    watch.stop();
  }
}


// ---------- 本地优先修改 + 后台合并异步同步到服务器（纯本地模式完全不同步） ----------

let settingsSyncSeq = 0;
let settingsSyncInFlight = false;

export function mutateSettingsLocalFirst(default_permission: AcpPermissionPolicy) {
  const next = { default_permission };
  setState({ acpSettings: next });
  if (isLocalOnlyActive()) return;
  setPendingSettingsPatch(next);
  settingsSyncSeq += 1;
  void flushSettingsSync();
}

async function flushSettingsSync() {
  if (settingsSyncInFlight) return;
  settingsSyncInFlight = true;
  try {
    while (true) {
      const pending = getPendingSettingsPatch();
      if (!pending) break;
      const snapSeq = settingsSyncSeq;
      try {
        const saved = await acp.updateSettings(pending.default_permission);
        if (settingsSyncSeq === snapSeq) {
          setPendingSettingsPatch(null);
          setState({ acpSettings: saved });
          break;
        }
      } catch {
        break;
      }
    }
  } finally {
    settingsSyncInFlight = false;
  }
}

const agentSyncSeq: Record<string, number> = {};
const agentSyncInFlight = new Set<string>();

export function mutateAgentLocalFirst(agentId: string, patch: PendingAgentPatch) {
  if (isLocalVaultAgent(agentId)) {
    const updated = patchLocalVaultAgent(agentId, patch);
    if (updated) {
      setState((cur) => ({
        acpAgents: cur.acpAgents.map((a) => (a.id === agentId ? updated : a)),
      }));
    }
    return;
  }
  mergePendingAgentPatch(agentId, patch);
  setState((cur) => ({
    acpAgents: cur.acpAgents.map((a) => (a.id === agentId ? { ...a, ...patch } : a)),
  }));
  agentSyncSeq[agentId] = (agentSyncSeq[agentId] ?? 0) + 1;
  void flushAgentSync(agentId);
}

async function flushAgentSync(agentId: string) {
  if (agentSyncInFlight.has(agentId)) return;
  agentSyncInFlight.add(agentId);
  try {
    while (true) {
      const pending = getPendingAgentPatches()[agentId];
      if (!pending) break;
      const snapSeq = agentSyncSeq[agentId] ?? 0;
      try {
        const saved = await acp.updateAgent(agentId, pending);
        if ((agentSyncSeq[agentId] ?? 0) === snapSeq) {
          clearPendingAgentPatch(agentId);
          setState((cur) => ({
            acpAgents: cur.acpAgents.map((a) => (a.id === saved.id ? saved : a)),
          }));
          break;
        }
      } catch {
        break;
      }
    }
  } finally {
    agentSyncInFlight.delete(agentId);
  }
}

const sessionSyncSeq: Record<string, number> = {};
const sessionSyncInFlight = new Set<string>();

export function mutateSessionLocalFirst(sessionId: string, patch: PendingSessionPatch) {
  if (isLocalVaultSession(sessionId)) {
    const updated = patchLocalVaultSession(sessionId, patch);
    if (updated) {
      setState((cur) => ({
        acpSessions: cur.acpSessions.map((x) => (x.id === sessionId ? updated : x)),
      }));
    }
    return;
  }
  mergePendingSessionPatch(sessionId, patch);
  setState((cur) => ({
    acpSessions: cur.acpSessions.map((x) => (x.id === sessionId ? { ...x, ...patch } : x)),
  }));
  sessionSyncSeq[sessionId] = (sessionSyncSeq[sessionId] ?? 0) + 1;
  void flushSessionSync(sessionId);
}

async function flushSessionSync(sessionId: string) {
  if (sessionSyncInFlight.has(sessionId)) return;
  sessionSyncInFlight.add(sessionId);
  try {
    while (true) {
      const pending = getPendingSessionPatches()[sessionId];
      if (!pending) break;
      const snapSeq = sessionSyncSeq[sessionId] ?? 0;
      try {
        const saved = await acp.updateSession(sessionId, pending);
        if ((sessionSyncSeq[sessionId] ?? 0) === snapSeq) {
          clearPendingSessionPatch(sessionId);
          setState((cur) => ({
            acpSessions: cur.acpSessions.map((x) =>
              x.id === saved.id ? { ...saved, turns: saved.turns ?? x.turns } : x,
            ),
          }));
          break;
        }
      } catch {
        break;
      }
    }
  } finally {
    sessionSyncInFlight.delete(sessionId);
  }
}

export function flushAllPendingAcpSyncs() {
  if (isLocalOnlyActive()) return;
  if (getPendingSettingsPatch()) void flushSettingsSync();
  for (const agentId of Object.keys(getPendingAgentPatches())) {
    if (!isLocalVaultAgent(agentId)) void flushAgentSync(agentId);
  }
  for (const sessionId of Object.keys(getPendingSessionPatches())) {
    if (!isLocalVaultSession(sessionId)) void flushSessionSync(sessionId);
  }
}

// ---------- 本机直连操作（目录、Git、Agent 探测、会话与纯本地执行） ----------

const PLACEHOLDER_MODELS = new Set([
  "",
  "default model",
  "default",
  "默认",
  "默认模型",
  "gpt-5 codex",
  "o3-pro",
  "gpt-4.1",
  "claude sonnet 4.5",
  "claude opus 4.1",
  "claude haiku 4.5",
  "gemini 2.5 pro",
  "gemini 2.5 flash",
]);

export function isPlaceholderOrLegacyModel(m: string | null | undefined): boolean {
  return PLACEHOLDER_MODELS.has((m ?? "").trim().toLowerCase());
}

function resolveProbedModelsClient(
  curDefault: string,
  probedDefault: string | null | undefined,
  probedOptions: string[] | undefined,
): { defaultModel: string; modelOptions: string[] } | null {
  const realOptions: string[] = [];
  for (const raw of probedOptions ?? []) {
    const trimmed = raw.trim().slice(0, 120);
    if (!trimmed || isPlaceholderOrLegacyModel(trimmed)) continue;
    if (!realOptions.includes(trimmed)) {
      realOptions.push(trimmed);
      if (realOptions.length >= 64) break;
    }
  }
  if (realOptions.length === 0) return null;

  const cur = curDefault.trim();
  let defaultModel: string;
  if (!isPlaceholderOrLegacyModel(cur) && realOptions.includes(cur)) {
    defaultModel = cur;
  } else if (probedDefault && realOptions.includes(probedDefault.trim())) {
    defaultModel = probedDefault.trim();
  } else {
    defaultModel = realOptions[0];
  }
  return { defaultModel, modelOptions: realOptions };
}

function hasMaskedSecrets(env: Record<string, string> | undefined): boolean {
  if (!env) return false;
  return Object.values(env).some((v) => v === "••••••••");
}

export async function renameHostLocalFirst(hostId: string, name: string): Promise<AcpHost> {
  if (isLocalVaultHost(hostId)) {
    const updated = renameLocalVaultHost(name);
    if (!updated) throw new Error("未找到本机节点");
    setState((cur) => ({
      acpHosts: cur.acpHosts.map((x) => (x.id === updated.id ? updated : x)),
    }));
    return updated;
  }
  const uh = await acp.renameHost(hostId, name);
  setState((cur) => ({
    acpHosts: cur.acpHosts.map((x) => (x.id === uh.id ? uh : x)),
  }));
  return uh;
}

export async function listDirsLocalFirst(hostId: string, path = ""): Promise<AcpDirListing> {
  if (isLocalAcpHost(hostId)) {
    try {
      return await tauriInvoke<AcpDirListing>("acp_host_list_dir", { path });
    } catch (e) {
      throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : "读取本机目录失败");
    }
  }
  return acp.listDirs(hostId, path);
}

export async function readHostFileLocalFirst(
  hostId: string,
  path: string,
  cwd?: string,
): Promise<AcpHostFileContent> {
  if (isLocalAcpHost(hostId)) {
    try {
      return await tauriInvoke<AcpHostFileContent>("acp_host_read_file", {
        path,
        cwd: cwd ?? null,
      });
    } catch (e) {
      throw new Error(
        typeof e === "string" ? e : e instanceof Error ? e.message : "读取本机文件失败",
      );
    }
  }
  return acp.readHostFile(hostId, path, cwd);
}

export async function createWorkspaceLocalFirst(body: {
  host_id: string;
  cwd: string;
  name?: string;
}): Promise<AcpWorkspace> {
  if (isLocalAcpHost(body.host_id)) {
    let probed: { cwd: string; branch: string; branches: string[] };
    try {
      probed = await tauriInvoke<{ cwd: string; branch: string; branches: string[] }>(
        "acp_host_probe_workspace",
        { cwd: body.cwd },
      );
    } catch (e) {
      throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : "校验本机工作目录失败");
    }
    if (isLocalVaultHost(body.host_id)) {
      const ws = upsertLocalVaultWorkspace({
        cwd: probed.cwd || body.cwd,
        name: body.name,
        branch: probed.branch,
        branches: probed.branches,
      });
      setState((cur) => ({
        acpWorkspaces: [ws, ...cur.acpWorkspaces.filter((x) => x.id !== ws.id)],
        activeAcpHostId: ws.host_id,
        activeAcpWorkspaceId: ws.id,
      }));
      return ws;
    }
    return acp.createWorkspace({
      host_id: body.host_id,
      cwd: probed.cwd || body.cwd,
      name: body.name,
      local_probed: true,
      branch: probed.branch,
      branches: probed.branches,
    });
  }
  return acp.createWorkspace(body);
}

export async function deleteWorkspaceLocalFirst(wsId: string): Promise<void> {
  if (isLocalVaultWorkspace(wsId)) {
    removeLocalVaultWorkspace(wsId);
    return;
  }
  await acp.deleteWorkspace(wsId);
}

export async function gitCheckoutLocalFirst(
  ws: AcpWorkspace,
  branch: string,
): Promise<AcpWorkspace> {
  if (isLocalAcpHost(ws.host_id)) {
    let res: { branch: string; branches: string[] };
    try {
      res = await tauriInvoke<{ branch: string; branches: string[] }>("acp_host_git_checkout", {
        cwd: ws.cwd,
        branch,
      });
    } catch (e) {
      throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : "切换本机分支失败");
    }
    const nextBranch = res.branch || branch;
    const nextBranches = res.branches?.length ? res.branches : ws.branches;

    if (isLocalVaultWorkspace(ws.id)) {
      const updated = patchLocalVaultWorkspace(ws.id, {
        branch: nextBranch,
        branches: nextBranches,
      })!;
      setState((cur) => ({
        acpWorkspaces: cur.acpWorkspaces.map((x) => (x.id === ws.id ? updated : x)),
      }));
      return updated;
    }

    mergePendingWorkspacePatch(ws.id, { branch: nextBranch, branches: nextBranches });
    const updated: AcpWorkspace = { ...ws, branch: nextBranch, branches: nextBranches };
    setState((cur) => ({
      acpWorkspaces: cur.acpWorkspaces.map((x) => (x.id === ws.id ? updated : x)),
    }));
    void acp
      .gitCheckout(ws.id, nextBranch, { local_probed: true, branches: nextBranches })
      .then((saved) => {
        clearPendingWorkspacePatch(ws.id);
        setState((cur) => ({
          acpWorkspaces: cur.acpWorkspaces.map((x) => (x.id === saved.id ? saved : x)),
        }));
      })
      .catch(() => {});
    return updated;
  }
  const saved = await acp.gitCheckout(ws.id, branch);
  setState((cur) => ({
    acpWorkspaces: cur.acpWorkspaces.map((x) => (x.id === saved.id ? saved : x)),
  }));
  return saved;
}

const inFlightModelProbes = new Map<string, Promise<AcpAgent>>();

export function probeAgentModelsLocalFirst(
  ag: AcpAgent,
  opts?: {
    sessionId?: string;
    cwd?: string;
    autoInstall?: boolean;
    rawEnv?: Record<string, string>;
    wait?: boolean;
  },
): Promise<AcpAgent> {
  const existing = inFlightModelProbes.get(ag.id);
  if (existing) return existing;

  const task = (async (): Promise<AcpAgent> => {
    const vaultRawEnv = isLocalVaultAgent(ag.id) ? getLocalVaultRawEnv(ag.id) : undefined;
    const effectiveRawEnv = opts?.rawEnv ?? vaultRawEnv;
    const canProbeLocally =
      isLocalAcpHost(ag.host_id) &&
      (effectiveRawEnv !== undefined || !hasMaskedSecrets(ag.env));

    if (canProbeLocally) {
      const envToUse = effectiveRawEnv ?? ag.env;
      const fallbackCwd =
        opts?.cwd ??
        getState().acpWorkspaces.find((w) => w.host_id === ag.host_id)?.cwd ??
        null;

      let localRes: {
        status?: string;
        default_model?: string | null;
        model_options?: string[];
      };
      try {
        localRes = await tauriInvoke("acp_host_probe_agent", {
          preset: ag.preset,
          cmd: ag.cmd,
          env: Object.keys(envToUse).length ? envToUse : null,
          cwd: fallbackCwd,
          autoInstall: Boolean(opts?.autoInstall),
          probeModels: true,
        });
      } catch (e) {
        throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : "本机探测模型失败");
      }

      const curAgent = getState().acpAgents.find((x) => x.id === ag.id) ?? ag;
      const resolved = resolveProbedModelsClient(
        curAgent.default_model,
        localRes.default_model,
        localRes.model_options,
      );
      const nextStatus = localRes.status || "online";

      if (!resolved) {
        if (curAgent.status !== nextStatus) {
          if (isLocalVaultAgent(ag.id)) {
            patchLocalVaultAgent(ag.id, { status: nextStatus });
          }
          const nextAg = { ...curAgent, status: nextStatus };
          setState((cur) => ({
            acpAgents: cur.acpAgents.map((x) => (x.id === ag.id ? nextAg : x)),
          }));
          return nextAg;
        }
        return curAgent;
      }

      const { defaultModel, modelOptions } = resolved;
      const updatedAgent: AcpAgent = {
        ...curAgent,
        status: nextStatus,
        default_model: defaultModel,
        model_options: modelOptions,
      };

      if (isLocalVaultAgent(ag.id)) {
        patchLocalVaultAgent(ag.id, {
          status: nextStatus,
          default_model: defaultModel,
          model_options: modelOptions,
        });
        setState((cur) => ({
          acpAgents: cur.acpAgents.map((x) => (x.id === ag.id ? updatedAgent : x)),
        }));
      } else {
        setState((cur) => ({
          acpAgents: cur.acpAgents.map((x) => (x.id === ag.id ? updatedAgent : x)),
        }));
        mutateAgentLocalFirst(ag.id, {
          default_model: defaultModel,
          model_options: modelOptions,
        });
      }

      const candidateSessions = getState().acpSessions.filter(
        (s) =>
          s.agent_id === ag.id &&
          !s.running &&
          !s.acp_session_key &&
          (s.turns ?? []).length === 0,
      );
      for (const s of candidateSessions) {
        const shouldSync =
          isPlaceholderOrLegacyModel(s.selected_model) ||
          s.selected_model === curAgent.default_model ||
          (opts?.sessionId === s.id && !modelOptions.includes(s.selected_model));
        if (shouldSync && s.selected_model !== defaultModel) {
          mutateSessionLocalFirst(s.id, { selected_model: defaultModel });
        }
      }

      return updatedAgent;
    }

    return acp.probeAgentModels(ag.id, {
      session_id: opts?.sessionId,
      wait: Boolean(opts?.wait),
    });
  })().finally(() => {
    inFlightModelProbes.delete(ag.id);
  });

  inFlightModelProbes.set(ag.id, task);
  return task;
}

export async function createAgentLocalFirst(body: {
  host_id: string;
  preset: string;
  name: string;
  cmd: string;
  env?: Record<string, string>;
}): Promise<AcpAgent> {
  if (isLocalAcpHost(body.host_id)) {
    let status = "standby";
    try {
      const quick = await tauriInvoke<{ status?: string }>("acp_host_probe_agent", {
        preset: body.preset,
        cmd: body.cmd,
        env: body.env ?? null,
        cwd: null,
        autoInstall: false,
        probeModels: false,
      });
      status = quick.status || "standby";
    } catch {}

    const created = isLocalVaultHost(body.host_id)
      ? addLocalVaultAgent({
          preset: body.preset,
          name: body.name,
          cmd: body.cmd,
          env: body.env,
          status,
        })
      : await acp.createAgent({
          ...body,
          local_probed: true,
          status,
        });

    setState((cur) => ({
      acpAgents: [...cur.acpAgents.filter((x) => x.id !== created.id), created],
      activeAcpHostId: body.host_id,
    }));

    if (status === "online" || status === "installing") {
      void probeAgentModelsLocalFirst(created, {
        autoInstall: status === "installing",
        rawEnv: body.env ?? {},
      }).catch(() => {});
    }
    return created;
  }

  const created = await acp.createAgent(body);
  setState((cur) => ({
    acpAgents: [...cur.acpAgents.filter((x) => x.id !== created.id), created],
    activeAcpHostId: body.host_id,
  }));
  return created;
}

export async function deleteAgentLocalFirst(agentId: string): Promise<void> {
  if (isLocalVaultAgent(agentId)) {
    removeLocalVaultAgent(agentId);
    return;
  }
  await acp.deleteAgent(agentId);
}

export async function createSessionLocalFirst(body: {
  workspace: AcpWorkspace;
  agent_id: string;
  title?: string;
}): Promise<AcpSession> {
  const local = isLocalAcpHost(body.workspace.host_id);
  const ag = getState().acpAgents.find((x) => x.id === body.agent_id);

  let sess: AcpSession;
  if (isLocalVaultHost(body.workspace.host_id)) {
    if (!ag) throw new Error("未找到所选 Agent");
    sess = addLocalVaultSession({
      workspace: body.workspace,
      agent: ag,
      defaultPermission: getState().acpSettings.default_permission,
      title: body.title,
    });
  } else {
    sess = await acp.createSession({
      workspace_id: body.workspace.id,
      agent_id: body.agent_id,
      title: body.title,
      skip_model_probe: local,
    });
  }

  setState((cur) => ({
    acpSessions: [sess, ...cur.acpSessions.filter((x) => x.id !== sess.id)],
    activeAcpHostId: body.workspace.host_id,
    activeAcpWorkspaceId: body.workspace.id,
    activeAcpSessionId: sess.id,
  }));

  if (local && ag) {
    void probeAgentModelsLocalFirst(ag, {
      sessionId: sess.id,
      cwd: body.workspace.cwd,
    }).catch(() => {});
  }
  return sess;
}

export async function deleteSessionLocalFirst(sessId: string): Promise<void> {
  if (isLocalVaultSession(sessId)) {
    await tauriInvoke("acp_host_local_prompt_cancel", { sessionId: sessId }).catch(() => {});
    removeLocalVaultSession(sessId);
    return;
  }
  await acp.deleteSession(sessId);
}

const recentUploadedAcpBlobs = new Map<string, Blob>();

function rememberAcpBlob(id: string, blob: Blob) {
  recentUploadedAcpBlobs.set(id, blob);
  if (recentUploadedAcpBlobs.size > 32) {
    const oldest = recentUploadedAcpBlobs.keys().next().value;
    if (oldest) recentUploadedAcpBlobs.delete(oldest);
  }
}

export async function uploadAttachmentLocalFirst(
  file: File,
  sessionId: string,
): Promise<AcpAttachment> {
  const att = isLocalVaultSession(sessionId)
    ? await stageLocalAttachment(file, sessionId)
    : await acp.uploadFile(file, sessionId);
  rememberAcpBlob(att.id, file);
  return att;
}

export async function fetchAcpAttachmentBlobLocalFirst(
  att: AcpAttachment,
): Promise<Blob> {
  if (getLocalVault().attachments[att.id]) {
    const blob = await readLocalAcpBlob(att.id);
    if (!blob || blob.size !== att.size) throw new Error(`本地附件缺失或不完整：${att.file_name}`);
    return blob;
  }
  const mem = recentUploadedAcpBlobs.get(att.id);
  if (mem) return mem;
  const fetched = await acp.fetchFileBlob(att.id);
  rememberAcpBlob(att.id, fetched);
  return fetched;
}

export async function downloadAcpAttachmentLocalFirst(
  id: string,
  fileName: string,
): Promise<void> {
  const local = getLocalVault().attachments[id];
  if (local) {
    const localBlob = await fetchAcpAttachmentBlobLocalFirst(local);
    const url = URL.createObjectURL(localBlob);
    const a = document.createElement("a");
    a.href = url;
    a.download = fileName || "attachment";
    a.style.display = "none";
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return;
  }
  await acp.downloadFile(id, fileName);
}

export async function sendPromptLocalFirst(
  sess: AcpSession,
  ws: AcpWorkspace | null,
  ag: AcpAgent | null,
  body: {
    text: string;
    files: string[];
    attachments: AcpAttachment[];
    model: string;
    thinking_level: AcpThinkingLevel;
    permission_policy: AcpPermissionPolicy;
  },
): Promise<{ localHandled: boolean; turn: AcpTurn }> {
  if (isLocalVaultSession(sess.id)) {
    if (!ws || !ag) throw new Error("工作区或 Agent 不存在");
    await ensureLocalEventBridge();
    const staged = await getLocalPromptAttachments(body.attachments);
    const claimed = appendLocalUserTurnAndStart(sess.id, body);
    if (!claimed) throw new Error("未找到本地会话");

    setState((cur) => {
      const nextSess = cur.acpSessions.map((x) => (x.id === sess.id ? claimed.session : x));
      return {
        acpSessions: nextSess,
        ...applyAcpEvent(
          { ...cur, acpSessions: nextSess },
          "session_activity",
          {
            session_id: sess.id,
            phase: "connecting",
            detail: "等待 Agent 响应…",
          },
        ),
      };
    });

    const rawEnv = getLocalVaultRawEnv(ag.id);
    try {
      await tauriInvoke("acp_host_local_prompt_start", {
        msg: {
          session_id: sess.id,
          run_id: claimed.session.run_id,
          acp_session_key: claimed.session.acp_session_key,
          cwd: ws.cwd,
          agent: {
            id: ag.id,
            preset: ag.preset,
            cmd: ag.cmd,
            env: rawEnv,
          },
          model: body.model,
          thinking_level: body.thinking_level,
          permission_policy: body.permission_policy,
          text: body.text,
          files: body.files,
          attachments: staged.map((s) => ({
            file_name: s.file_name,
            mime: s.mime,
            data_base64: s.data_base64,
          })),
        },
      });
    } catch (e) {
      const rolled = settleLocalSessionOnCancel(sess.id, claimed.session.run_id);
      if (!rolled || rolled.running) throw e;
      setState((cur) => {
        const nextSess = cur.acpSessions.map((x) => (x.id === sess.id ? rolled : x));
        return {
          acpSessions: nextSess,
          ...applyAcpEvent({ ...cur, acpSessions: nextSess }, "session_activity", {
            session_id: sess.id,
            phase: "idle",
            detail: "",
          }),
        };
      });
      throw e;
    }
    return { localHandled: true, turn: claimed.turn };
  }

  const turn = await acp.sendPrompt(sess.id, {
    text: body.text,
    files: body.files,
    attachment_ids: body.attachments.map((a) => a.id),
    model: body.model,
    thinking_level: body.thinking_level,
    permission_policy: body.permission_policy,
  });
  if (turn.payload.run_id === undefined) {
    // Older servers still need the optimistic activity until their running state arrives.
    setState((cur) =>
      applyAcpEvent(cur, "session_activity", {
        session_id: sess.id,
        phase: "connecting",
        detail: "等待 Agent 响应…",
      }),
    );
  }
  return { localHandled: false, turn };
}

export async function cancelSessionLocalFirst(sess: AcpSession): Promise<AcpSession> {
  if (isLocalVaultSession(sess.id)) {
    await tauriInvoke("acp_host_local_prompt_cancel", {
      sessionId: sess.id,
      runId: sess.run_id ?? null,
    });
    const updated = settleLocalSessionOnCancel(sess.id, sess.run_id) ?? {
      ...sess,
      running: false,
      run_id: null,
    };
    if (updated.running) return updated;
    setState((cur) => {
      const nextSess = cur.acpSessions.map((x) => (x.id === sess.id ? updated : x));
      return {
        acpSessions: nextSess,
        ...applyAcpEvent(
          { ...cur, acpSessions: nextSess },
          "session_activity",
          {
            session_id: sess.id,
            phase: "idle",
            detail: "",
          },
        ),
      };
    });
    return updated;
  }
  const updated = await acp.cancelSession(sess.id, sess.run_id);
  const current = getState().acpSessions.find((s) => s.id === sess.id);
  // Another device can already have started a new run by the time this HTTP reply arrives.
  if (current?.run_id && current.run_id !== sess.run_id && current.run_id !== updated.run_id) {
    return current;
  }
  return updated;
}

export async function replyPermissionLocalFirst(
  sessId: string,
  turnId: string,
  option_id: "allow_once" | "allow_always" | "reject_once" | "reject_always",
): Promise<AcpTurn> {
  if (isLocalVaultSession(sessId)) {
    const resolved = resolveLocalPermissionTurn(turnId, option_id);
    if (!resolved) throw new Error("权限请求已过期或不存在");
    await tauriInvoke("acp_host_local_permission_reply", {
      reqId: resolved.reqId,
      optionId: option_id,
    });
    setState((cur) => ({
      acpSessions: cur.acpSessions.map((x) => (x.id === sessId ? resolved.session : x)),
    }));
    return resolved.turn;
  }
  return acp.replyPermission(turnId, option_id);
}

/** 桌面端登录成功后初始化本机 ACP：若开启了云端控制则建立 role=host 长连接；若关闭则挂载纯本地节点，不向服务器同步任何数据。 */
export async function startAcpHost(serverUrl: string): Promise<void> {
  if (!isDesktopShell() || !serverUrl) return;
  await ensureLocalEventBridge();

  const [policy, meta] = await Promise.all([getLocalPolicy(), fetchLocalHostMeta()]);
  if (!policy.cloud_control_enabled) {
    const owner = getLocalVault().owner;
    if (owner && owner !== JSON.stringify([serverUrl, getState().me?.id])) return;
    const v = ensureLocalOnlyHost(meta, JSON.stringify([serverUrl, getState().me?.id]));
    setState((cur) => ({
      acpHosts: [v.host!, ...cur.acpHosts.filter((h) => h.id !== v.host!.id)],
      acpAgents: [
        ...v.agents,
        ...cur.acpAgents.filter((a) => a.host_id !== v.host!.id),
      ],
      acpWorkspaces: [
        ...v.workspaces,
        ...cur.acpWorkspaces.filter((w) => w.host_id !== v.host!.id),
      ],
      acpSessions: [
        ...v.sessions,
        ...cur.acpSessions.filter(
          (s) => !v.workspaces.some((w) => w.id === s.workspace_id),
        ),
      ],
      activeAcpHostId: cur.activeAcpHostId ?? v.host!.id,
    }));
    return;
  }

  flushAllPendingAcpSyncs();
  if (startedServer === serverUrl && unlistenTicket && unlistenConfirm && unlistenStatus) return;

  try {
    const [{ invoke }, { listen }] = await Promise.all([
      import("@tauri-apps/api/core"),
      import("@tauri-apps/api/event"),
    ]);

    if (unlistenTicket) {
      unlistenTicket();
      unlistenTicket = null;
    }
    if (unlistenConfirm) {
      unlistenConfirm();
      unlistenConfirm = null;
    }
    if (unlistenStatus) {
      unlistenStatus();
      unlistenStatus = null;
    }

    unlistenStatus = await listen<{ online?: boolean; host_id?: string | null }>(
      "acp-host:status",
      (ev) => {
        if (ev.payload?.host_id) {
          setLocalAcpHostId(ev.payload.host_id);
          for (const waiter of hostReadyWaiters) {
            waiter(ev.payload.host_id);
          }
        }
      },
    );

    unlistenTicket = await listen<{ req_id: string }>("acp-host:need-ticket", async (ev) => {
      const reqId = ev.payload?.req_id;
      if (!reqId) return;
      try {
        const res = await convs.ticket();
        await invoke("acp_host_ticket", { reqId, ticket: res.ticket, error: null });
      } catch (e) {
        const msg = e instanceof Error ? e.message : "取票失败";
        await invoke("acp_host_ticket", { reqId, ticket: null, error: msg }).catch(() => {});
      }
    });

    unlistenConfirm = await listen<{
      req_id: string;
      session_id: string;
      cwd: string;
      cmd: string;
      env_keys?: string[];
    }>("acp-host:confirm-exec", async (ev) => {
      const p = ev.payload;
      if (!p?.req_id) return;
      let allow = false;
      try {
        const envNote = p.env_keys?.length
          ? `\n环境变量键：${p.env_keys.join(", ")}`
          : "";
        allow = await confirmDialog(
          `远程请求在本机运行未授权的 Agent：\n目录：${p.cwd}\n命令：${p.cmd}${envNote}\n\n确认允许并记住此启动配置？`,
          { okText: "允许运行" },
        );
      } catch {
        allow = false;
      }
      await invoke("acp_host_confirm_exec", { reqId: p.req_id, allow }).catch(() => {});
    });

    startedServer = serverUrl;
    await invoke("acp_host_start", { serverUrl, hostName: null });
    if (meta.host_id) setLocalAcpHostId(meta.host_id);
  } catch (e) {
    console.warn("acp host start skipped", e);
  }
}

export async function stopAcpHost(): Promise<void> {
  startedServer = null;
  memLocalHostId = null;
  if (unlistenTicket) {
    unlistenTicket();
    unlistenTicket = null;
  }
  if (unlistenConfirm) {
    unlistenConfirm();
    unlistenConfirm = null;
  }
  if (unlistenStatus) {
    unlistenStatus();
    unlistenStatus = null;
  }
  if (unlistenLocalEvent) {
    unlistenLocalEvent();
    unlistenLocalEvent = null;
  }
  if (!isDesktopShell()) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("acp_host_stop");
  } catch {}
}
