import { isDesktopShell, osOfUa } from "./api";

export function isWebKitFileDrag(dataTransfer: DataTransfer): boolean {
  const { types, files } = dataTransfer;
  if (!isDesktopShell() || osOfUa(navigator.userAgent) !== "Linux" ||
      !types.includes("text/uri-list") || types.includes("Files") || files.length) {
    return false;
  }
  const uris = dataTransfer.getData("text/uri-list").split(/\r?\n/)
    .map((line) => line.trim()).filter((line) => line && !line.startsWith("#"));
  if (uris.length) return uris.every((uri) => /^file:\/\//i.test(uri));
  // Protected WebKitGTK drags hide the URI; links advertise text/plain, Dolphin files don't.
  return !types.includes("text/plain");
}

export function isManagedFileDrag({ target, dataTransfer }: Pick<DragEvent, "target" | "dataTransfer">): boolean {
  if (!dataTransfer || (!dataTransfer.types.includes("Files") && !dataTransfer.files.length && !isWebKitFileDrag(dataTransfer))) {
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
