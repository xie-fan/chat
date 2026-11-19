export function isFileDrag(data: DataTransfer | null): boolean {
  return !!data && (data.types.includes("Files") || data.files.length > 0);
}

export function installFileDropGuard(target: Window): () => void {
  const preventFileDropDefault = (event: DragEvent) => {
    if (isFileDrag(event.dataTransfer)) event.preventDefault();
  };
  target.addEventListener("dragover", preventFileDropDefault, true);
  target.addEventListener("drop", preventFileDropDefault, true);
  return () => {
    target.removeEventListener("dragover", preventFileDropDefault, true);
    target.removeEventListener("drop", preventFileDropDefault, true);
  };
}
