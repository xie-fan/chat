// ===== MF2 批量转发：请求构造 / 结果文案 / 旧服务端判定（纯函数，便于单测） =====

import { newClientId } from "../../clientId";
import { ApiError } from "../../api";
import type { ForwardBatchResp, ForwardResp } from "../../types";

export type ForwardMode = "separate" | "merged";

/** 一次用户确认的完整批量请求；client_op_id 只允许在「完全相同参数」重发时复用 */
export interface BatchPayload {
  conversation_id: string;
  /** 按气泡序的服务端消息 id */
  message_ids: string[];
  /** 去重排序后的目标会话 id（≤10） */
  targets: string[];
  mode: ForwardMode;
  client_op_id: string;
}

/** 本次确认要的请求载荷：源会话/消息集/目标/模式任一变化 → 新 op；完全相同 → 复用 */
export function prepareBatchSubmission(
  prev: BatchPayload | null,
  conversation_id: string,
  message_ids: string[],
  targets: string[],
  mode: ForwardMode,
): BatchPayload {
  const t = [...new Set(targets)].sort();
  if (
    prev &&
    prev.conversation_id === conversation_id &&
    prev.mode === mode &&
    prev.targets.join("\n") === t.join("\n") &&
    prev.message_ids.length === message_ids.length &&
    prev.message_ids.every((id, i) => message_ids[i] === id)
  ) {
    return prev;
  }
  return { conversation_id, message_ids: [...message_ids], targets: t, mode, client_op_id: newClientId() };
}

/**
 * MF2-C6 旧服务端判定：只在「这个路由根本不存在」时走兜底——
 * 405（方法不允许），或 404 且响应体不是 JSON 带 error 字段。
 * 404 + {"error":"..."}（如 unknown_api）、400、500 等一律走正常报错路径。
 */
export function isOldServerUnsupported(e: unknown): boolean {
  return (
    e instanceof ApiError &&
    (e.status === 405 || (e.status === 404 && !e.hasErrorField))
  );
}

export interface BatchToast {
  ok: number;
  fail: number;
  skipped: number;
  /** C5 文案（可能为空串：resp.results 为空） */
  text: string;
}

const firstLine = (s: string) => s.split("\n")[0];

/**
 * MF2-C5 结果文案：
 * 全成「已转发到 OK 个会话」；部分成「已转发到 OK 个会话，F 个失败：<首个原因>」（原因不一再加「等」）；
 * 全败「转发失败：<首个原因>」；K>0 末尾追加「，已跳过 K 条不支持的消息」。
 * K = 顶层 skipped 之和；顶层没有该字段才看第一目标的（不按目标数乘）。
 */
export function batchResultToast(resp: ForwardBatchResp): BatchToast {
  const ok = resp.results.filter((r) => r.ok).length;
  const fail = resp.results.length - ok;
  const errors = resp.results
    .filter((r) => !r.ok)
    .map((r) => r.error?.message || "转发失败");
  const distinct = new Set(errors);
  let text: string;
  if (fail === 0) text = `已转发到 ${ok} 个会话`;
  else if (ok === 0) text = `转发失败：${firstLine(errors[0] ?? "目标不可达")}`;
  else text = `已转发到 ${ok} 个会话，${fail} 个失败：${firstLine(errors[0])}${distinct.size > 1 ? " 等" : ""}`;
  const skippedMap =
    resp.skipped && Object.keys(resp.skipped).length > 0
      ? resp.skipped
      : (resp.results[0]?.skipped ?? {});
  const skipped = Object.values(skippedMap).reduce((a, b) => a + (b || 0), 0);
  if (skipped > 0) text += `，已跳过 ${skipped} 条不支持的消息`;
  return { ok, fail, skipped, text };
}

/**
 * 单条转发（含记录内附件条目）同一套 C5 文案——ForwardResp 的 error 是 code 字符串 +
 * message_text，归一化后复用同一套拼接规则。
 */
export function forwardRespToast(resp: ForwardResp): BatchToast {
  return batchResultToast({
    skipped: {},
    results: resp.results.map((r) => ({
      conversation_id: r.conversation_id,
      ok: r.ok,
      forwarded: r.message ? 1 : 0,
      error: r.ok ? undefined : { code: r.error ?? "", message: r.message_text || "转发失败" },
      skipped: {},
    })),
  });
}

/** MF2-C6：逐条兜底结果文案「服务端版本过低，已逐条转发 X/N 条」 */
export const fallbackSeparateToast = (sent: number, total: number) =>
  `服务端版本过低，已逐条转发 ${sent}/${total} 条`;
export const FALLBACK_MERGED_TOAST = "服务端版本过低，不支持合并转发";
/** 逐条兜底最多回放的消息数 */
export const FALLBACK_SEPARATE_CAP = 20;
