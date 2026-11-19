import { isDesktopShell, osOfUa } from "./api";

export function isLinuxDesktop(): boolean {
  return isDesktopShell() && osOfUa(navigator.userAgent) === "Linux";
}

export async function listenForNativeDrops(onDrop: (paths: string[], x: number, y: number) => void) {
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent(({ payload }) => {
    if (payload.type === "drop") {
      // Wry forwards GTK widget coordinates in logical pixels on Linux.
      onDrop(payload.paths, payload.position.x, payload.position.y);
    }
  });
}

export async function readNativeDropFile(path: string): Promise<File> {
  const { invoke } = await import("@tauri-apps/api/core");
  const bytes = new Uint8Array(await invoke<ArrayBuffer>("read_dropped_file", { path }));
  const separator = bytes.indexOf(0);
  const type = new TextDecoder().decode(bytes.subarray(0, separator));
  return new File([bytes.subarray(separator + 1)], path.slice(path.lastIndexOf("/") + 1), { type });
}

export function isManagedFileDrag({ target, dataTransfer }: Pick<DragEvent, "target" | "dataTransfer">): boolean {
  if (!dataTransfer || (!dataTransfer.types.includes("Files") && !dataTransfer.files.length)) {
    return false;
  }
  const element = target instanceof Element ? target : target instanceof Node ? target.parentElement : null;
  return !element?.closest('input[type="file"]');
}

export function installFileDropGuard(target: Window): () => void {
  const preventFileDropDefault = (event: DragEvent) => {
    if (isManagedFileDrag(event)) event.preventDefault();
  };
  target.addEventListener("dragover", preventFileDropDefault, true);
  target.addEventListener("drop", preventFileDropDefault, true);
  return () => {
    target.removeEventListener("dragover", preventFileDropDefault, true);
    target.removeEventListener("drop", preventFileDropDefault, true);
  };
}
