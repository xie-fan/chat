import { currentAccountId, isDesktopShell, registerAndroidPush, session, type AndroidPushRegistration } from "./api";
import { notifyEnabled } from "./notify";

function android() {
  return "__TAURI_INTERNALS__" in window && !isDesktopShell();
}

async function registration(account: string | null, enabled: boolean) {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<AndroidPushRegistration | null>("android_push_registration", { account, enabled });
}

export function clearAndroidPush() {
  if (android()) void registration(null, false).catch(() => {});
}

export function watchAndroidPush(): (() => void) | undefined {
  if (!android()) return;
  let disposed = false;
  let request = 0;
  let last: AndroidPushRegistration | null = null;
  let sending = Promise.resolve();
  const sync = async () => {
    const current = ++request;
    const gen = session();
    const enabled = notifyEnabled();
    try {
      const device = await registration(currentAccountId(), enabled);
      if (disposed || current !== request || gen !== session()) return;
      if (!device) {
        last = null;
        return;
      }
      sending = sending.then(async () => {
        if (disposed || current !== request || gen !== session()) return;
        if (last?.token === device.token && last.binding === device.binding && last.project === device.project) return;
        await registerAndroidPush(device);
        if (!disposed && current === request && gen === session()) last = device;
      }).catch(() => {});
    } catch {}
  };
  const trigger = () => void sync();
  const interval = window.setInterval(trigger, 60_000);
  window.addEventListener("online", trigger);
  window.addEventListener("focus", trigger);
  window.addEventListener("chat:android-push-settings", trigger);
  trigger();
  return () => {
    disposed = true;
    window.clearInterval(interval);
    window.removeEventListener("online", trigger);
    window.removeEventListener("focus", trigger);
    window.removeEventListener("chat:android-push-settings", trigger);
  };
}
