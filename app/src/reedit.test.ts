import { describe, expect, it } from "vitest";
import {
  REEDIT_WINDOW_MS,
  clearReedits,
  patchReedit,
  purgeReedits,
  readReedits,
  reeditFresh,
  reeditKey,
  removeReedit,
  saveReedit,
  type ReeditEntry,
} from "./reedit";
import { removeServerData } from "./servers";
import { memStorage } from "./testStorage";

// ===== MM2 =====
const A = "enc-a";
const B = "enc-b";
const NOW = Date.parse("2026-06-01T12:00:00Z");

function entry(patch: Partial<ReeditEntry> = {}): ReeditEntry {
  return {
    conversation_id: "c1",
    message_id: "m1",
    text: "原文",
    savedAt: NOW,
    ...patch,
  };
}

describe("重新编辑暂存（chat_reedit:<enc>:<uid>）", () => {
  it("保存→读取往返；同 message_id 覆盖", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry());
    saveReedit(st, A, "u1", entry({ text: "覆盖后" }));
    expect(readReedits(st, A, "u1", NOW)).toEqual([entry({ text: "覆盖后" })]);
  });

  it("15 分钟窗口：过期条目读取时剔除并落盘", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry({ savedAt: NOW - REEDIT_WINDOW_MS }));
    saveReedit(st, A, "u1", entry({ message_id: "m2" }));
    expect(reeditFresh(entry({ savedAt: NOW - REEDIT_WINDOW_MS }), NOW)).toBe(false);
    const fresh = readReedits(st, A, "u1", NOW);
    expect(fresh.map((e) => e.message_id)).toEqual(["m2"]);
    // 过期条目已从存储剔除（不是只在内存里被过滤）
    expect(JSON.parse(st.raw.get(reeditKey(A, "u1"))!)).toHaveLength(1);
  });

  it("按服务器与用户隔离", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry());
    expect(readReedits(st, B, "u1", NOW)).toEqual([]);
    expect(readReedits(st, A, "u2", NOW)).toEqual([]);
  });

  it("patchReedit：撤回响应带回 recalled_attachment_id", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry());
    patchReedit(st, A, "u1", "m1", { recalled_attachment_id: "att9" });
    expect(readReedits(st, A, "u1", NOW)[0].recalled_attachment_id).toBe("att9");
    patchReedit(st, A, "u1", "ghost", { recalled_attachment_id: "x" }); // 不存在不炸
    expect(readReedits(st, A, "u1", NOW)).toHaveLength(1);
  });

  it("撤回失败 / 点击使用：removeReedit 立即删除（不留没成功的原文）", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry());
    removeReedit(st, A, "u1", "m1");
    expect(readReedits(st, A, "u1", NOW)).toEqual([]);
    expect(st.raw.has(reeditKey(A, "u1"))).toBe(false); // 空表清键
  });

  it("purgeReedits：启动时清过期、留新鲜", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry({ savedAt: NOW - REEDIT_WINDOW_MS }));
    saveReedit(st, A, "u1", entry({ message_id: "m2" }));
    purgeReedits(st, A, "u1", NOW);
    expect(readReedits(st, A, "u1", NOW).map((e) => e.message_id)).toEqual(["m2"]);
  });

  it("clearReedits：登出整键删除（新鲜条目也不留），其他账号/服务器不动", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry()); // 新鲜条目
    saveReedit(st, A, "u2", entry({ message_id: "m2" }));
    saveReedit(st, B, "u1", entry({ message_id: "m3" }));
    clearReedits(st, A, "u1");
    expect(st.raw.has(reeditKey(A, "u1"))).toBe(false); // 键整个没了
    expect(readReedits(st, A, "u1", NOW)).toEqual([]);
    expect(readReedits(st, A, "u2", NOW).map((e) => e.message_id)).toEqual(["m2"]);
    expect(readReedits(st, B, "u1", NOW).map((e) => e.message_id)).toEqual(["m3"]);
    // 空键/storage 抛错不炸
    expect(() => clearReedits(st, A, "u1")).not.toThrow();
    expect(() => clearReedits({ getItem: () => null, setItem: () => {}, removeItem: () => { throw new Error("denied"); } }, A, "u1")).not.toThrow();
  });

  it("删除服务器：chat_reedit 前缀随其余数据一起清掉", () => {
    const st = memStorage();
    saveReedit(st, A, "u1", entry());
    saveReedit(st, B, "u1", entry({ message_id: "m3" }));
    removeServerData(st, A);
    expect(readReedits(st, A, "u1", NOW)).toEqual([]);
    expect(readReedits(st, B, "u1", NOW).map((e) => e.message_id)).toEqual(["m3"]);
  });

  it("损坏数据按空处理，storage 抛错不炸", () => {
    const st = memStorage({ [reeditKey(A, "u1")]: "{broken" });
    expect(readReedits(st, A, "u1", NOW)).toEqual([]);
    const throwing = {
      getItem: () => { throw new Error("denied"); },
      setItem: () => { throw new Error("denied"); },
      removeItem: () => {},
    };
    expect(readReedits(throwing, A, "u1", NOW)).toEqual([]);
    expect(() => saveReedit(throwing, A, "u1", entry())).not.toThrow();
  });
});
