import { convs, isDesktopShell } from "./api";
import { confirmDialog } from "./ui/Modal";

let unlistenTicket: (() => void) | null = null;
let unlistenConfirm: (() => void) | null = null;
let startedServer: string | null = null;

export interface LocalPolicyView {
  allow_all_local: boolean;
  approved_count: number;
}

export async function getLocalAllowAll(): Promise<boolean> {
  if (!isDesktopShell()) return false;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const res = await invoke<LocalPolicyView>("acp_host_local_policy");
    return !!res?.allow_all_local;
  } catch {
    return false;
  }
}

export async function setLocalAllowAll(enabled: boolean): Promise<LocalPolicyView> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<LocalPolicyView>("acp_host_set_local_allow_all", { enabled });
}

/** 桌面端登录成功后自动拉起本机 ACP 宿主守护协程，向服务端建立 role=host 长连接。 */
export async function startAcpHost(serverUrl: string): Promise<void> {
  if (!isDesktopShell() || !serverUrl) return;
  if (startedServer === serverUrl && unlistenTicket && unlistenConfirm) return;

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
  } catch (e) {
    console.warn("acp host start skipped", e);
  }
}

export async function stopAcpHost(): Promise<void> {
  startedServer = null;
  if (unlistenTicket) {
    unlistenTicket();
    unlistenTicket = null;
  }
  if (unlistenConfirm) {
    unlistenConfirm();
    unlistenConfirm = null;
  }
  if (!isDesktopShell()) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("acp_host_stop");
  } catch {}
}
