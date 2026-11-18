import { convs, isDesktopShell } from "./api";

let unlistenTicket: (() => void) | null = null;
let startedServer: string | null = null;

/** 桌面端登录成功后自动拉起本机 ACP 宿主守护协程，向服务端建立 role=host 长连接。 */
export async function startAcpHost(serverUrl: string): Promise<void> {
  if (!isDesktopShell() || !serverUrl) return;
  if (startedServer === serverUrl && unlistenTicket) return;

  try {
    const [{ invoke }, { listen }] = await Promise.all([
      import("@tauri-apps/api/core"),
      import("@tauri-apps/api/event"),
    ]);

    if (unlistenTicket) {
      unlistenTicket();
      unlistenTicket = null;
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
  if (!isDesktopShell()) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("acp_host_stop");
  } catch {}
}
