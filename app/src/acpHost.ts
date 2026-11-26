import {
  activateLocalVault,
  addLocalVaultAgent,
  addLocalVaultSession,
  appendLocalUserTurnAndStart,
  applyLocalRunnerEvent,
  deactivateAndTakeLocalVault,
  ensureLocalOnlyHost,
  getLocalVault,
  getLocalVaultRawEnv,
  isLocalOnlyActive,
  isLocalVaultAgent,
  isLocalVaultHost,
  isLocalVaultSession,
  isLocalVaultWorkspace,
  patchLocalVaultAgent,
  patchLocalVaultSession,
  patchLocalVaultWorkspace,
  removeLocalVaultAgent,
  removeLocalVaultSession,
  removeLocalVaultWorkspace,
  renameLocalVaultHost,
  resolveLocalPermissionTurn,
  settleLocalSessionOnCancel,
  getStagedLocalAttachment,
  stageLocalAttachment,
  takeStagedLocalAttachments,
  upsertLocalVaultWorkspace,
} from "./acpLocalVault";
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
import { acp, convs, getServer, isDesktopShell, serverScope } from "./api";
import { applyAcpBootstrap, applyAcpEvent, type AppState, getState, setState } from "./store";
import type {
  AcpAgent,
  AcpAttachment,
  AcpDirListing,
  AcpHost,
  AcpHostFileContent,
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

/**
 * 切换「本机允许被云端控制」开关：
 * - 关闭时：立即断开本机宿主 WS，将本机当前配置转入纯本地存储（保持本机正常使用），并调用 deleteHost 从云端彻底删除本机全部数据且停止一切同步；
 * - 开启时：恢复本机宿主 WS 连接，并将纯本地存储中的 Agent、工作区与会话同步上云。
 */
export async function setCloudControlEnabled(enabled: boolean): Promise<LocalPolicyView> {
  if (!isDesktopShell()) {
    return { allow_all_local: false, cloud_control_enabled: true, approved_count: 0 };
  }
  await ensureLocalEventBridge();
  const meta = await fetchLocalHostMeta();

  if (!enabled) {
    const cur = getState();
    const cloudHost =
      cur.acpHosts.find((h) => h.id === (memLocalHostId ?? readStoredLocalHostId())) ??
      cur.acpHosts.find(
        (h) => h.ip === "Client-Daemon" && (!meta.os || h.os === meta.os),
      ) ??
      null;
    const cloudHostId = cloudHost?.id ?? memLocalHostId ?? readStoredLocalHostId();

    const policy = await tauriInvoke<LocalPolicyView>("acp_host_set_cloud_control", {
      enabled: false,
    });
    startedServer = null;
    setLocalAcpHostId(null);

    const localHostId = meta.install_id;
    const baseHost: AcpHost = cloudHost
      ? { ...cloudHost, id: localHostId }
      : {
          id: localHostId,
          name: meta.name,
          os: meta.os,
          ip: "Local-Only",
          roots: meta.roots,
          online: true,
          latency_ms: 0,
          last_seen: new Date().toISOString(),
        };

    const hostAgents = cloudHostId
      ? cur.acpAgents.filter((a) => a.host_id === cloudHostId)
      : [];
    const hostWorkspaces = cloudHostId
      ? cur.acpWorkspaces.filter((w) => w.host_id === cloudHostId)
      : [];
    const wsIds = new Set(hostWorkspaces.map((w) => w.id));
    const hostSessions = cur.acpSessions.filter((s) => wsIds.has(s.workspace_id));

    for (const a of hostAgents) clearPendingAgentPatch(a.id);
    for (const w of hostWorkspaces) clearPendingWorkspacePatch(w.id);
    for (const s of hostSessions) clearPendingSessionPatch(s.id);

    const v = activateLocalVault({
      host: baseHost,
      agents: hostAgents,
      workspaces: hostWorkspaces,
      sessions: hostSessions,
    });

    setState((st) => {
      const otherHosts = st.acpHosts.filter(
        (h) => h.id !== cloudHostId && h.id !== v.host!.id,
      );
      const otherAgents = st.acpAgents.filter(
        (a) => a.host_id !== cloudHostId && a.host_id !== v.host!.id,
      );
      const otherWorkspaces = st.acpWorkspaces.filter(
        (w) => w.host_id !== cloudHostId && w.host_id !== v.host!.id,
      );
      const otherSessions = st.acpSessions.filter((s) => !wsIds.has(s.workspace_id));
      return {
        acpHosts: [v.host!, ...otherHosts],
        acpAgents: [...v.agents, ...otherAgents],
        acpWorkspaces: [...v.workspaces, ...otherWorkspaces],
        acpSessions: [...v.sessions, ...otherSessions],
        activeAcpHostId:
          st.activeAcpHostId === cloudHostId ? v.host!.id : (st.activeAcpHostId ?? v.host!.id),
      };
    });

    if (cloudHostId && cloudHost?.ip !== "Local-Only") {
      await acp.deleteHost(cloudHostId);
    }
    return policy;
  }

  // 重新开启云端控制：恢复 WS 连接并将纯本地数据上云
  const policy = await tauriInvoke<LocalPolicyView>("acp_host_set_cloud_control", {
    enabled: true,
  });
  const localSnap = deactivateAndTakeLocalVault();
  const srv = getServer() || (typeof window !== "undefined" ? window.location.origin : "");

  const waitHostId = new Promise<string | null>((resolve) => {
    const timer = setTimeout(() => {
      hostReadyWaiters.delete(onReady);
      resolve(getLocalAcpHostId());
    }, 6000);
    const onReady = (hid: string) => {
      clearTimeout(timer);
      hostReadyWaiters.delete(onReady);
      resolve(hid);
    };
    hostReadyWaiters.add(onReady);
  });

  await startAcpHost(srv);
  const newCloudHostId = (await waitHostId) ?? getLocalAcpHostId();

  if (newCloudHostId && localSnap.host) {
    if (localSnap.host.name && localSnap.host.name !== meta.name) {
      await acp.renameHost(newCloudHostId, localSnap.host.name).catch(() => {});
    }
    const agentIdMap = new Map<string, string>();
    for (const ag of localSnap.agents) {
      try {
        const rawEnv = localSnap.agentSecrets[ag.id];
        const created = await acp.createAgent({
          host_id: newCloudHostId,
          preset: ag.preset,
          name: ag.name,
          adapter: ag.adapter,
          cmd: ag.cmd,
          env: rawEnv && Object.keys(rawEnv).length ? rawEnv : undefined,
          local_probed: true,
          status: ag.status,
          default_model: ag.default_model,
          model_options: ag.model_options,
        });
        agentIdMap.set(ag.id, created.id);
        if (ag.permission_override !== "inherit") {
          await acp
            .updateAgent(created.id, { permission_override: ag.permission_override })
            .catch(() => {});
        }
      } catch {}
    }

    const wsIdMap = new Map<string, string>();
    for (const ws of localSnap.workspaces) {
      try {
        const createdWs = await acp.createWorkspace({
          host_id: newCloudHostId,
          cwd: ws.cwd,
          name: ws.name,
          local_probed: true,
          branch: ws.branch,
          branches: ws.branches,
        });
        wsIdMap.set(ws.id, createdWs.id);
      } catch {}
    }

    for (const sess of localSnap.sessions) {
      const mappedWs = wsIdMap.get(sess.workspace_id);
      const mappedAg = agentIdMap.get(sess.agent_id);
      if (!mappedWs || !mappedAg) continue;
      try {
        const createdSess = await acp.createSession({
          workspace_id: mappedWs,
          agent_id: mappedAg,
          title: sess.title,
          skip_model_probe: true,
        });
        await acp
          .updateSession(createdSess.id, {
            selected_model: sess.selected_model,
            thinking_level: sess.thinking_level,
            permission_policy: sess.permission_policy,
          })
          .catch(() => {});
      } catch {}
    }
  }

  try {
    const bs = await acp.bootstrap();
    setState((cur) => applyAcpBootstrap(cur, bs));
  } catch {}

  return policy;
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

function base64ToBlob(b64: string, mime: string): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return new Blob([bytes], { type: mime || "application/octet-stream" });
}

export async function uploadAttachmentLocalFirst(
  file: File,
  sessionId: string,
): Promise<AcpAttachment> {
  const att = isLocalVaultSession(sessionId)
    ? await stageLocalAttachment(file)
    : await acp.uploadFile(file, sessionId);
  rememberAcpBlob(att.id, file);
  return att;
}

export async function fetchAcpAttachmentBlobLocalFirst(
  att: AcpAttachment,
): Promise<Blob> {
  const mem = recentUploadedAcpBlobs.get(att.id);
  if (mem) return mem;
  const staged = getStagedLocalAttachment(att.id);
  if (staged) {
    const blob = base64ToBlob(staged.data_base64, staged.mime);
    rememberAcpBlob(att.id, blob);
    return blob;
  }
  const fetched = await acp.fetchFileBlob(att.id);
  rememberAcpBlob(att.id, fetched);
  return fetched;
}

export async function downloadAcpAttachmentLocalFirst(
  id: string,
  fileName: string,
): Promise<void> {
  const localBlob =
    recentUploadedAcpBlobs.get(id) ??
    (() => {
      const staged = getStagedLocalAttachment(id);
      return staged ? base64ToBlob(staged.data_base64, staged.mime) : null;
    })();
  if (localBlob && isLocalOnlyActive()) {
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
    const staged = takeStagedLocalAttachments(body.attachments.map((a) => a.id));
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
          acp_session_key: sess.acp_session_key,
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
      const rolled = patchLocalVaultSession(sess.id, { running: false });
      setState((cur) => {
        const nextSess = cur.acpSessions.map((x) =>
          x.id === sess.id ? (rolled ?? { ...x, running: false }) : x,
        );
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
  setState((cur) =>
    applyAcpEvent(cur, "session_activity", {
      session_id: sess.id,
      phase: "connecting",
      detail: "等待 Agent 响应…",
    }),
  );
  return { localHandled: false, turn };
}

export async function cancelSessionLocalFirst(sess: AcpSession): Promise<AcpSession> {
  if (isLocalVaultSession(sess.id)) {
    await tauriInvoke("acp_host_local_prompt_cancel", { sessionId: sess.id });
    const updated = settleLocalSessionOnCancel(sess.id) ?? {
      ...sess,
      running: false,
    };
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
  return acp.cancelSession(sess.id);
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
    const v = ensureLocalOnlyHost(meta);
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
