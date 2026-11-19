// 在线更新：服务端转发最新 Release（GET /v1/client/latest）。
// 桌面端交给 Rust 的 install_update（tauri-plugin-updater，校验签名后安装重启）；
// Android 走 In-app APK Installer（下载进缓存 → 系统安装器），浏览器下载作兜底。
// 浏览器端每次加载都是服务器上的新版，不需要。

import { allowedOrigins, getServer } from "./api";

export interface ClientLatest {
  version: string;
  notes: string;
  published_at: string;
  assets: { name: string; size: number }[];
}

/** "1.2.10" > "1.2.9"；缺的段按 0，非数字段按 0 */
export function isNewer(latest: string, current: string): boolean {
  const p = (v: string) => v.split(/[.+-]/).slice(0, 3).map((x) => parseInt(x, 10) || 0);
  const a = p(latest);
  const b = p(current);
  for (let i = 0; i < 3; i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) > (b[i] ?? 0);
  }
  return false;
}

/** 没配置 / 没有 Release / 服务端太旧 → null */
export async function fetchLatest(): Promise<ClientLatest | null> {
  try {
    const res = await fetch(`${getServer()}/v1/client/latest`);
    return res.ok ? ((await res.json()) as ClientLatest) : null;
  } catch {
    return null;
  }
}

export function downloadUrl(name: string): string {
  return `${getServer()}/v1/client/download/${encodeURIComponent(name)}`;
}

export function apkUrl(latest: ClientLatest): string | null {
  const a = latest.assets.find((x) => x.name.endsWith(".apk"));
  return a ? downloadUrl(a.name) : null;
}

/** 安装包给人看的平台名；.sig 是更新签名，不给人下载 → null */
export function assetLabel(name: string): string | null {
  if (name.endsWith(".sig")) return null;
  if (name.endsWith("-setup.exe")) return "Windows";
  if (name.endsWith(".apk")) return "Android";
  if (name.endsWith(".deb")) return "Linux（deb）";
  if (name.endsWith(".AppImage")) return "Linux（AppImage）";
  if (name.endsWith(".dmg")) return "macOS";
  return name;
}

/** 按 osOfUa 的结果挑推荐安装包；没有对应平台 → undefined */
export function assetFor(os: string, names: string[]): string | undefined {
  const suffix = ({ Windows: "-setup.exe", Android: ".apk", Linux: ".AppImage", macOS: ".dmg" } as Record<string, string>)[os];
  return suffix ? names.find((n) => n.endsWith(suffix)) : undefined;
}

/** 本平台的安装包已上传（桌面还要有 .sig 才能在线更新）。Release 先建、各平台包陆续上传，
 *  没传完之前不提示新版本，免得点了装不上 */
export function hasInstaller(os: string, names: string[]): boolean {
  const pkg = assetFor(os, names);
  return !!pkg && (os === "Android" || names.includes(`${pkg}.sig`));
}

// ===== AS2：Android 应用内 APK 安装器（AS3 再把 UpdateSetting 接到这些命令上）=====
// Rust → Kotlin ApkInstallerPlugin：下载进应用私有缓存 apk-update/，FileProvider
// content URI 交系统安装器；与附件 MediaStore 下载是两条独立路径。

export interface ApkDownloaded {
  path: string;
  name: string;
}

/** state: idle | downloading | done | error；totalBytes = -1 表示服务器没给长度 */
export interface ApkDownloadProgress {
  state: string;
  receivedBytes: number;
  totalBytes: number;
  name: string;
  error: string;
}

const PERMISSION_DENIED = "PERMISSION_DENIED:";

/** invoke/异常 → 一行错误文案 */
export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Kotlin 侧权限拒绝带 PERMISSION_DENIED: 前缀；其余失败 = 网络/IO/安装器拉起失败 */
export function isApkPermissionDenied(e: unknown): boolean {
  return errText(e).startsWith(PERMISSION_DENIED);
}

/** Signature Mismatch 特征：系统错误 -7 = INSTALL_FAILED_UPDATE_INCOMPATIBLE（签名不一致）。
 *  命中 → 前端给「先卸载旧版再装（仅此一次）」文案；其余安装失败走通用文案。
 *  -7 前不能是数字/小数点（免得把版本号 1.2.3-7、HTTP -70 当成签名错误） */
export function isApkSignatureError(e: unknown): boolean {
  return /signatures?\b|签名|证书|update_incompatible|inconsistent_certificate|(?:^|[^\d.])-7(?!\d)/i.test(
    errText(e),
  );
}

/** 下载百分比：服务端没给 Content-Length（total<=0）→ null，UI 只报已收大小 */
export function apkProgressPercent(received: number, total: number): number | null {
  return total > 0 ? Math.min(100, Math.round((received / total) * 100)) : null;
}

/** 下载进度文案（In-app APK Installer 轮询快照 → 一行中文） */
export function apkProgressText(received: number, total: number): string {
  const mb = (n: number) => (n / 1024 / 1024).toFixed(1);
  const pct = apkProgressPercent(received, total);
  return pct === null
    ? `已下载 ${mb(received)} MB`
    : `已下载 ${mb(received)} / ${mb(total)} MB（${pct}%）`;
}

/** 下载 APK 到应用缓存（挂起直到完成/失败）；期间用 apkDownloadProgress 轮询进度 */
export async function downloadUpdateApk(url: string): Promise<ApkDownloaded> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke("download_update_apk", { url, allowedOrigins: allowedOrigins() });
}

/** 轮询当前下载进度快照（与 downloadUpdateApk 的挂起调用并发可用） */
export async function apkDownloadProgress(): Promise<ApkDownloadProgress> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke("apk_download_progress");
}

/** 是否已授予「安装未知应用」权限（API<26 看全局未知来源开关） */
export async function checkInstallPermission(): Promise<boolean> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke("check_install_permission");
}

/** 打开系统「安装未知应用」授权页；授权后重试 installUpdateApk */
export async function openInstallPermissionSettings(): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke("open_install_permission_settings");
}

/** 对已下载（downloadUpdateApk 返回的 name）的 APK 拉起系统安装器 */
export async function installUpdateApk(name: string): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke("install_update_apk", { name });
}
