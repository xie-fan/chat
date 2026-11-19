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
