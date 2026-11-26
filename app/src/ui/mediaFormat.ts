// 图片气泡尺寸与存储用量的格式化工具（V2 P2-1）。纯函数，便于 vitest 直测。

/** 图片气泡最长边（px）：大图等比缩到这个边长内，小图按原尺寸占位 */
export const BUBBLE_EDGE = 240;

/**
 * 附件图片的 <img> src（MM2 fix3）：
 * 撤回件复用期间使用上传者已取回的 object URL。普通附件使用鉴权后的媒体地址。
 * 两种地址仍在请求时返回 null，由调用方显示占位。
 */
export function attImgSrc(
  att: { recalled?: boolean },
  objectUrl: string | null | undefined,
  mediaUrl: string | null | undefined,
): string | null {
  return (att.recalled ? objectUrl : mediaUrl) ?? null;
}

/**
 * 原始像素尺寸 → 气泡展示尺寸。最长边缩到 BUBBLE_EDGE，比例保持；
 * 小图不放大；非法输入返回 0 尺寸（调用方回退无尺寸渲染）。
 */
export function imageBubbleSize(
  w: number,
  h: number,
): { width: number; height: number } {
  if (!Number.isFinite(w) || !Number.isFinite(h) || w <= 0 || h <= 0) {
    return { width: 0, height: 0 };
  }
  const edge = Math.max(w, h);
  if (edge <= BUBBLE_EDGE) return { width: w, height: h };
  const k = BUBBLE_EDGE / edge;
  // 极端细长图四舍五入后仍保底 1px，避免塌成 0
  return {
    width: Math.max(1, Math.round(w * k)),
    height: Math.max(1, Math.round(h * k)),
  };
}

/** 字节数 → 人类可读（B/KB/MB/GB/TB；读数达到 512 即进位到下一级单位，保持在三位内） */
export function fmtBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 512 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}

/** 「我的存储」一行文案：有限额 → 已用 X / 上限 Y；不限（null/0）→ 已用 X（不限额） */
export function storageText(usedBytes: number, quotaBytes: number | null): string {
  if (quotaBytes == null || quotaBytes <= 0) {
    return `已用 ${fmtBytes(usedBytes)}（不限额）`;
  }
  return `已用 ${fmtBytes(usedBytes)} / 上限 ${fmtBytes(quotaBytes)}`;
}
