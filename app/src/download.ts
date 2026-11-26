// ===== MM4：附件下载统一入口 =====
// Tauri 壳（桌面 + Android）：invoke Rust 命令 download_to_downloads 直存系统下载目录，
// 桌面端点击成功 toast 在文件管理器中显示文件；Android 走 MediaStore（无点击动作）。
// Web：保持 <a download> 浏览器自带下载。
import { allowedOrigins, files, inTauri, isDesktopShell } from "./api";
import { pushToast, updateToast } from "./store";
import type { AttachmentView } from "./types";

interface Downloaded {
  path: string;
  name: string;
}

type Invoke = (cmd: string, args: Record<string, unknown>) => Promise<Downloaded>;
type AnchorFn = (url: string, name: string) => void;

/** 测试注入点：平台判定 / Tauri invoke / Web 锚点 */
export interface SaveAttachmentDeps {
  tauri?: boolean;
  desktop?: boolean;
  invoke?: Invoke;
  anchor?: AnchorFn;
}

function browserAnchor(url: string, name: string) {
  const el = document.createElement("a");
  el.href = url;
  el.download = name;
  el.referrerPolicy = "no-referrer";
  document.body.appendChild(el);
  el.click();
  el.remove();
}

/**
 * 唯一的下载入口：消息菜单 / 动作面板的「下载」、预览工具栏 viewer-download、
 * 会话「文件与图片」面板的行内下载按钮都走这里。
 */
export async function saveAttachment(
  a: AttachmentView,
  deps: SaveAttachmentDeps = {},
): Promise<void> {
  if (!(deps.tauri ?? inTauri())) {
    try {
      const { url } = await files.access(a.id, { dl: true });
      (deps.anchor ?? browserAnchor)(url, a.file_name);
    } catch (e) {
      pushToast({ title: "下载失败", text: e instanceof Error ? e.message : String(e) });
    }
    return;
  }
  // 在途提示常驻直到被结果接替（大文件可能超过 5s）
  const toast = pushToast(
    { title: "下载中…", text: a.file_name },
    { timeoutMs: 0 },
  );
  try {
    const { url } = await files.access(a.id, { dl: true });
    const invoke: Invoke =
      deps.invoke ??
      (await import("@tauri-apps/api/core")).invoke as Invoke;
    const r = await invoke("download_to_downloads", {
      url,
      fileName: a.file_name,
      allowedOrigins: allowedOrigins(),
    });
    updateToast(toast, {
      title: `已保存到下载：${r.name}`,
      text: "",
      // 桌面端点击 toast → 在文件夹中显示；Android 没有点击动作
      filePath: (deps.desktop ?? isDesktopShell()) ? r.path : undefined,
    });
  } catch (e) {
    updateToast(toast, {
      title: "下载失败",
      text: e instanceof Error ? e.message : String(e),
    });
  }
}

/** 成功 toast 点击（桌面）：在文件管理器中显示；平台不支持/权限被拒就静默 */
export async function revealDownloaded(path: string): Promise<void> {
  try {
    const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
    await revealItemInDir(path);
  } catch {
    /* 不支持 reveal 的平台（如部分 Linux 桌面）不做任何事 */
  }
}
