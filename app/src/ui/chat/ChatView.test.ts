import { describe, expect, it } from "vitest";
import { ApiError } from "../../api";
import type { ConversationView, MessageView } from "../../types";
import {
  editRejected,
  ownsEditResult,
  prepareEditSubmission,
  uploadPendingAttachments,
} from "./ChatView";
import type { AttachmentView } from "../../types";

it("reuses successful uploads when a later attachment is retried", async () => {
  const first = new File(["a"], "a.txt");
  const second = new File(["b"], "b.txt");
  const uploaded = new Map<File, AttachmentView>();
  const keys = new Map<File, string>();
  const calls: string[] = [];
  let secondAttempt = false;
  await expect(uploadPendingAttachments(
    [first, second], uploaded, keys,
    async (file, key) => {
      calls.push(`${file.name}:${key}`);
      if (file === second && !secondAttempt) throw new Error("temporary upload failure");
      return {
        id: file === first ? "a" : "b.txt", file_name: file.name, mime: "text/plain", size: 1,
        width: null, height: null, has_thumb: false, duration_ms: null,
      };
    },
  )).rejects.toThrow("temporary upload failure");
  expect(uploaded.get(first)?.id).toBe("a");
  expect(calls).toHaveLength(2);
  const firstKey = keys.get(first);
  const secondKey = keys.get(second);
  expect(firstKey).toBeTruthy();
  expect(secondKey).toBeTruthy();
  secondAttempt = true;
  const retry = await uploadPendingAttachments(
    [first, second], uploaded, keys,
    async (file, key) => { calls.push(`${file.name}:${key}`); return {
      id: file.name, file_name: file.name, mime: "text/plain", size: 1,
      width: null, height: null, has_thumb: false, duration_ms: null,
    }; },
  );
  expect(retry.map((item) => item.id)).toEqual(["a", "b.txt"]);
  expect(calls).toHaveLength(3);
  expect(uploaded.get(first)?.id).toBe("a");
  expect(uploaded.get(second)?.id).toBe("b.txt");
  expect(keys.get(first)).toBe(firstKey);
  expect(keys.get(second)).toBe(secondKey);
  expect(calls[2]).toBe(`b.txt:${secondKey}`);
});

describe("edit submission callback ownership", () => {
  const token = {};
  const submitted = { token, rev: 2, messageId: "m1", convId: "c1", gen: 7 };

  it("accepts the result only for the same owner token, edit revision, message, conv and session", () => {
    const live = { owner: token, rev: 2, editingId: "m1", activeConv: "c1", gen: 7 };
    expect(ownsEditResult(submitted, live)).toBe(true);
  });

  it("rejects the result after cancel, switching to another message, or account/conv change", () => {
    // 取消编辑：editingId 清空且代次递增
    expect(ownsEditResult(submitted, { owner: token, rev: 3, editingId: null, activeConv: "c1", gen: 7 })).toBe(false);
    // 改编辑另一条消息
    expect(ownsEditResult(submitted, { owner: token, rev: 3, editingId: "m2", activeConv: "c1", gen: 7 })).toBe(false);
    // 会话切换/组件重挂：owner token 被替换
    expect(ownsEditResult(submitted, { owner: {}, rev: 2, editingId: "m1", activeConv: "c1", gen: 7 })).toBe(false);
    expect(ownsEditResult(submitted, { owner: null, rev: 2, editingId: "m1", activeConv: "c1", gen: 7 })).toBe(false);
    // 已切走会话或换账号
    expect(ownsEditResult(submitted, { owner: token, rev: 2, editingId: "m1", activeConv: "c2", gen: 7 })).toBe(false);
    expect(ownsEditResult(submitted, { owner: token, rev: 2, editingId: "m1", activeConv: "c1", gen: 8 })).toBe(false);
    // token 为空时永不相等
    expect(ownsEditResult({ ...submitted, token: null }, { owner: null, rev: 2, editingId: "m1", activeConv: "c1", gen: 7 })).toBe(false);
  });
});

describe("edit submission retention by outcome", () => {
  it("keeps the same op only when the outcome is uncertain", () => {
    // 结果未确认（网络错误/会话代次中断/5xx）：必须保留 clientOpId 供幂等重试
    expect(editRejected(new Error("network down"))).toBe(false);
    expect(editRejected(new ApiError(0, "session changed"))).toBe(false);
    expect(editRejected(new ApiError(500, "boom"))).toBe(false);
    // 服务端明确拒绝：内容或版本需变化，下一次提交必须是新 op
    for (const st of [400, 403, 404, 409, 422]) {
      expect(editRejected(new ApiError(st, "rejected"))).toBe(true);
    }
  });
});

describe("edit submission payload", () => {
  it("retains operation ID, body and expected version for an uncertain retry", () => {
    const first = prepareEditSubmission(null, "message", " edited ", 2);
    expect(prepareEditSubmission(first, "message", "edited", 3)).toBe(first);
    expect(first.expectedVersion).toBe(2);
    expect(first.content).toBe("edited");
    expect(prepareEditSubmission(first, "message", "new draft", 3).clientOpId).not.toBe(first.clientOpId);
    expect(prepareEditSubmission(null, "message", "edited", 3).clientOpId).not.toBe(first.clientOpId);
  });

  it("validates the Rust codepoint limit rather than UTF-16 units", () => {
    expect(prepareEditSubmission(null, "m", "𠀀".repeat(4000), 0).content).toHaveLength(8000);
    expect(() => prepareEditSubmission(null, "m", "𠀀".repeat(4001), 0)).toThrow();
  });
});

// ===== Agent A05 =====
// 专属会话里主人的附件消息显示「待交付」提示，被调用带走（attachment_ids）后消失。
