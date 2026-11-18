import { describe, expect, it, vi } from "vitest";
import {
  authKey,
  encServer,
  firstEmptyCredential,
  isImeEnter,
  kickReasonKey,
  listServers,
  migrateLegacyStorage,
  needsInsecureConfirm,
  normalizeServerUrl,
  planServerEnter,
  readAuth,
  requestFormSubmit,
  resolveServerBlur,
  resolveServerInput,
  removeServerData,
  removeServerEntry,
  removeUserServerData,
  updateSavedAccountTokens,
  upsertServer,
  writeAuth,
  accountIdOf,
  listSavedAccounts,
  upsertSavedAccount,
  removeSavedAccount,
  type ServerEntry,
} from "./servers";
import { memStorage } from "./testStorage";

const user = (id: string) => ({
  id,
  username: id,
  display_name: id,
  role: "user" as const,
  last_seen_at: null,
});

// ===== T5：服务器地址规范化 =====
describe("normalizeServerUrl", () => {
  it.each([
    ["https://chat.287152.xyz", "https://chat.287152.xyz"],
    ["  https://a.example/  ", "https://a.example"],
    ["chat.287152.xyz", "http://chat.287152.xyz"],
    ["192.168.1.5:28080", "http://192.168.1.5:28080"],
    ["localhost:29051", "http://localhost:29051"],
    ["HTTP://EXAMPLE.COM/", "http://example.com"],
    ["http://a:80", "http://a"],
    ["https://a:443", "https://a"],
    ["http://a:8080", "http://a:8080"],
    ["https://a/sub/path/", "https://a/sub/path"],
    ["https://a/", "https://a"],
    ["https://a///", "https://a"],
  ])("%j → %j", (input, want) => {
    expect(normalizeServerUrl(input)).toBe(want);
  });

  it("http 与 https 是不同条目；localhost 与 127.0.0.1 不合并", () => {
    expect(normalizeServerUrl("http://a")).not.toBe(normalizeServerUrl("https://a"));
    expect(normalizeServerUrl("http://localhost")).not.toBe(
      normalizeServerUrl("http://127.0.0.1"),
    );
  });

  it("规范化幂等：再喂一次结果不变", () => {
    const once = normalizeServerUrl("HTTPS://A.Example:443/x/");
    expect(normalizeServerUrl(once!)).toBe(once);
  });

  it.each(["", "   ", "ftp://a", "ws://a", "http://", "://a", "javascript:alert(1)"])(
    "非法输入 %j → null",
    (input) => {
      expect(normalizeServerUrl(input)).toBeNull();
    },
  );
});

// ===== T6：登录提交时解析地址框输入 =====
// 地址框里的输入在提交前不必先落盘：提交一律用「当前输入」解析，
// 保证改完地址直接回车/点登录就连新地址，不靠 blur 保存。
describe("resolveServerInput", () => {
  const web = { desktop: false, origin: "http://localhost:29061" };

  it("输入新地址 → 返回规范化后的新地址（未保存的改动也生效）", () => {
    expect(resolveServerInput("localhost:29062", web)).toEqual({
      ok: true,
      url: "http://localhost:29062",
    });
    expect(resolveServerInput("  HTTPS://B.Example:443/x/ ", web)).toEqual({
      ok: true,
      url: "https://b.example/x",
    });
  });

  it("非空但非法 → 报错，不返回地址", () => {
    for (const bad of ["http://[bad", "ftp://a", "://a", "javascript:x"]) {
      const r = resolveServerInput(bad, web);
      expect(r.ok, `input=${bad}`).toBe(false);
      if (!r.ok) expect(r.err).toContain("服务器地址");
    }
  });

  it("Web 端留空 → 同源地址；桌面端留空 → 提示必填", () => {
    expect(resolveServerInput("", web)).toEqual({ ok: true, url: "http://localhost:29061" });
    expect(resolveServerInput("   ", web)).toEqual({ ok: true, url: "http://localhost:29061" });
    const r = resolveServerInput("", { desktop: true });
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.err).toContain("请填写服务器地址");
  });
});

describe("needsInsecureConfirm", () => {
  it("仅 Web https 页面连 http 服务器时需要确认", () => {
    expect(needsInsecureConfirm(false, "https:", "http://a")).toBe(true);
    expect(needsInsecureConfirm(false, "https:", "https://a")).toBe(false);
    expect(needsInsecureConfirm(false, "http:", "http://a")).toBe(false);
    expect(needsInsecureConfirm(true, "https:", "http://a")).toBe(false);
  });
});

// ===== T6b：Opus 复审修复 =====

// F1：IME 输入法合成期间的 Enter 不算提交（keyCode 229 是旧浏览器兜底）
describe("isImeEnter", () => {
  it("isComposing 或 keyCode 229 → IME 合成中", () => {
    expect(isImeEnter({ isComposing: true })).toBe(true);
    expect(isImeEnter({ keyCode: 229 })).toBe(true);
    expect(isImeEnter({ isComposing: false, keyCode: 13 })).toBe(false);
    expect(isImeEnter({})).toBe(false);
  });
});

describe("firstEmptyCredential", () => {
  it("返回第一个空凭据字段（用户名优先）；都填了 → null", () => {
    expect(firstEmptyCredential("", "")).toBe("username");
    expect(firstEmptyCredential("  ", "x")).toBe("username");
    expect(firstEmptyCredential("u", "")).toBe("password");
    expect(firstEmptyCredential("u", "p")).toBeNull();
  });
});

// F6/F1/F5/F2：地址框回车的动作决策
describe("planServerEnter", () => {
  const base = {
    composing: false,
    busy: false,
    input: "localhost:29072",
    username: "alice",
    password: "secret123",
    desktop: false,
    origin: "http://localhost:29071",
  };

  it("F6 登录请求在飞（busy）→ ignore：不提交地址也不发请求", () => {
    expect(planServerEnter({ ...base, busy: true })).toEqual({ t: "ignore" });
  });

  it("F1 IME 合成中的 Enter → ignore：不提交不校验", () => {
    expect(planServerEnter({ ...base, composing: true, input: "zhongwen" })).toEqual({
      t: "ignore",
    });
  });

  it("F5 用户名空 → 只提交地址并聚焦用户名；密码空 → 聚焦密码", () => {
    expect(planServerEnter({ ...base, username: "  " })).toEqual({
      t: "focus",
      url: "http://localhost:29072",
      field: "username",
    });
    expect(planServerEnter({ ...base, password: "" })).toEqual({
      t: "focus",
      url: "http://localhost:29072",
      field: "password",
    });
  });

  it("地址非法 → err（面板保持展开）", () => {
    const p = planServerEnter({ ...base, input: "http://[bad" });
    expect(p.t).toBe("err");
    if (p.t === "err") expect(p.err).toContain("服务器地址");
  });

  it("桌面端地址留空 → err（必填）", () => {
    const p = planServerEnter({ ...base, input: "  ", desktop: true });
    expect(p.t).toBe("err");
  });

  it("凭据齐全 + 地址合法 → submit，返回规范化地址", () => {
    expect(planServerEnter(base)).toEqual({
      t: "submit",
      url: "http://localhost:29072",
    });
  });
});

// F2/F7：地址框失焦去向
describe("resolveServerBlur", () => {
  const web = { desktop: false, origin: "http://localhost:29071" };

  it("Web 端留空 → collapse（不动已保存地址）", () => {
    expect(resolveServerBlur("   ", web)).toEqual({ t: "collapse" });
  });

  it("F2 桌面端留空 → err（地址必填，面板不收起）", () => {
    const r = resolveServerBlur("", { desktop: true });
    expect(r.t).toBe("err");
    if (r.t === "err") expect(r.err).toContain("请填写服务器地址");
  });

  it("非法输入 → err（不藏起错误）", () => {
    const r = resolveServerBlur("http://[bad", web);
    expect(r.t).toBe("err");
  });

  it("F7 合法输入 → commit 规范化地址（调用方顺带清掉旧错误）", () => {
    expect(resolveServerBlur("LOCALHOST:29072/", web)).toEqual({
      t: "commit",
      url: "http://localhost:29072",
    });
    expect(resolveServerBlur("", { desktop: false, origin: "http://x" })).toEqual({
      t: "collapse",
    });
  });
});

// F4：老 WebKit 没有 requestSubmit → 派发可取消冒泡的 submit 事件
describe("requestFormSubmit", () => {
  it("有 requestSubmit 直接调用", () => {
    const form = { requestSubmit: vi.fn() };
    requestFormSubmit(form as unknown as HTMLFormElement);
    expect(form.requestSubmit).toHaveBeenCalledTimes(1);
  });

  it("没有 requestSubmit → dispatchEvent submit {cancelable,bubbles}", () => {
    const seen: Event[] = [];
    const form = {
      requestSubmit: undefined,
      dispatchEvent: (e: Event) => {
        seen.push(e);
        return true;
      },
    };
    requestFormSubmit(form as unknown as HTMLFormElement);
    expect(seen).toHaveLength(1);
    expect(seen[0].type).toBe("submit");
    expect(seen[0].cancelable).toBe(true);
    expect(seen[0].bubbles).toBe(true);
  });

  it("null 表单安全返回", () => {
    expect(() => requestFormSubmit(null)).not.toThrow();
  });
});

describe("encServer", () => {
  it("编码后不含 : 或 /，键名分段不会错位", () => {
    const e = encServer("http://a:8080/x");
    expect(e).not.toMatch(/[:/]/);
    expect(e).toBe(encodeURIComponent("http://a:8080/x"));
  });
});

// ===== T5：服务器列表 =====
describe("服务器列表 chat_servers", () => {
  it("新增条目进列表并持久化，最近使用在前", () => {
    const st = memStorage();
    upsertServer(st, "http://a", {});
    upsertServer(st, "http://b", {});
    const list = listServers(st);
    expect(list.map((s) => s.url)).toEqual(["http://b", "http://a"]);
    expect(JSON.parse(st.raw.get("chat_servers")!)).toHaveLength(2);
  });

  it("同一个地址 upsert 不重复，且把它顶到最前", () => {
    const st = memStorage();
    upsertServer(st, "http://a", {});
    upsertServer(st, "http://b", {});
    upsertServer(st, "http://a", { username: "u1" });
    const list = listServers(st);
    expect(list.map((s) => s.url)).toEqual(["http://a", "http://b"]);
    expect(list[0].username).toBe("u1");
    expect(list).toHaveLength(2);
  });

  it("removeServerEntry 删除条目", () => {
    const st = memStorage();
    upsertServer(st, "http://a", {});
    upsertServer(st, "http://b", {});
    removeServerEntry(st, "http://a");
    expect(listServers(st).map((s) => s.url)).toEqual(["http://b"]);
  });

  it("损坏 JSON / 非数组 → 空列表", () => {
    for (const bad of ["{", "null", '"str"', "42", '{"a":1}']) {
      const st = memStorage({ chat_servers: bad });
      expect(listServers(st), `input=${bad}`).toEqual([]);
    }
  });

  it("条目里 url 缺失/非法的被过滤", () => {
    const st = memStorage({
      chat_servers: JSON.stringify([
        { url: "http://a", lastUsedAt: 1 },
        { url: 42, lastUsedAt: 9 },
        { nope: true },
        null,
      ] satisfies unknown[]),
    });
    expect(listServers(st).map((s) => s.url)).toEqual(["http://a"]);
  });
});

// ===== T5：按服务器的登录凭据 =====
describe("chat_auth:<enc> 凭据", () => {
  it("写入→读回；不同服务器互不可见", () => {
    const st = memStorage();
    writeAuth(st, encServer("http://a"), {
      access: "aa",
      refresh: "ra",
      user: user("u-a"),
    });
    writeAuth(st, encServer("http://b"), {
      access: "ab",
      refresh: "rb",
      user: user("u-b"),
    });
    expect(readAuth(st, encServer("http://a"))?.refresh).toBe("ra");
    expect(readAuth(st, encServer("http://b"))?.refresh).toBe("rb");
    expect(readAuth(st, encServer("http://c"))).toBeNull();
    expect(st.raw.get(authKey(encServer("http://a")))).toBeTruthy();
  });

  it("损坏/缺字段 → null", () => {
    for (const bad of ["{", "null", "42", '"s"', "[]", '{"refresh":1}', '{"refresh":"r"}']) {
      const st = memStorage({ [authKey(encServer("http://a"))]: bad });
      expect(readAuth(st, encServer("http://a")), `input=${bad}`).toBeNull();
    }
  });
});

// ===== T5：删除服务器清掉它的全部本地数据 =====
describe("removeServerData", () => {
  it("清该服务器的 auth/cache/outbox/drafts/kick，精确按 enc 前缀，不误伤其它服务器", () => {
    const a = encServer("http://a");
    const b = encServer("http://ab"); // 前缀相近：a 是 ab 的前缀，靠末尾冒号区分
    const st = memStorage({
      [authKey(a)]: "{}",
      [`chat_cache:${a}:u1`]: "c1",
      [`chat_cache:${a}:u2`]: "c2",
      [`chat_outbox:${a}:u1`]: "o1",
      [`chat_drafts:${a}:u1`]: "d1",
      [authKey(b)]: "{}",
      [`chat_cache:${b}:u1`]: "cb",
      [`chat_outbox:${b}:u1`]: "ob",
      [`chat_drafts:${b}:u1`]: "db",
      "chat_servers": "[]",
      "unrelated": "x",
    });
    removeServerData(st, a);
    const left = [...st.raw.keys()];
    expect(left).toEqual(
      expect.arrayContaining([
        authKey(b),
        `chat_cache:${b}:u1`,
        `chat_outbox:${b}:u1`,
        `chat_drafts:${b}:u1`,
        "chat_servers",
        "unrelated",
      ]),
    );
    expect(left.some((k) => k.includes(`:${a}:`) || k === authKey(a))).toBe(false);
  });
});

// ===== T5：旧版键一次性迁移 =====
describe("migrateLegacyStorage", () => {
  const CUR = "http://cur.example";
  const CUR_ENC = encServer(CUR);

  it("旧版全局凭据迁到当前服务器名下，旧键删除", () => {
    const st = memStorage({
      chat_access: "a1",
      chat_refresh: "r1",
      chat_user: JSON.stringify(user("u1")),
      chat_server_url: "http://cur.example/",
    });
    migrateLegacyStorage(st, CUR);
    expect(readAuth(st, CUR_ENC)).toEqual({
      access: "a1",
      refresh: "r1",
      user: user("u1"),
    });
    for (const k of ["chat_access", "chat_refresh", "chat_user", "chat_server_url"]) {
      expect(st.raw.has(k)).toBe(false);
    }
  });

  it("旧版 chat_cache/chat_outbox 按其内嵌服务器重编码，未发出的 outbox 不丢", () => {
    const payload = JSON.stringify([{ clientMsgId: "m1" }]);
    const st = memStorage({
      "chat_cache:http://a:28080:u1": "cache-a",
      "chat_outbox:http://a:28080:u1": payload,
      "chat_cache:https://b.example:u2": "cache-b",
    });
    migrateLegacyStorage(st, CUR);
    const aEnc = encServer("http://a:28080");
    expect(st.raw.get(`chat_cache:${aEnc}:u1`)).toBe("cache-a");
    expect(st.raw.get(`chat_outbox:${aEnc}:u1`)).toBe(payload);
    expect(st.raw.get(`chat_cache:${encServer("https://b.example")}:u2`)).toBe("cache-b");
    expect(st.raw.has("chat_cache:http://a:28080:u1")).toBe(false);
    expect(st.raw.has("chat_outbox:http://a:28080:u1")).toBe(false);
    expect(st.raw.has("chat_cache:https://b.example:u2")).toBe(false);
  });

  it("旧版 chat_drafts:<uid>（无服务器）迁到当前服务器名下", () => {
    const st = memStorage({ "chat_drafts:u1": '{"c":"hi"}' });
    migrateLegacyStorage(st, CUR);
    expect(st.raw.get(`chat_drafts:${CUR_ENC}:u1`)).toBe('{"c":"hi"}');
    expect(st.raw.has("chat_drafts:u1")).toBe(false);
  });

  it("已是新格式的键原样保留，迁移幂等", () => {
    const st = memStorage({
      [`chat_cache:${CUR_ENC}:u1`]: "c",
      [authKey(CUR_ENC)]: JSON.stringify({ access: "a", refresh: "r", user: user("u1") }),
    });
    migrateLegacyStorage(st, CUR);
    migrateLegacyStorage(st, CUR); // 第二次不再产生变化
    expect(st.raw.get(`chat_cache:${CUR_ENC}:u1`)).toBe("c");
    expect(readAuth(st, CUR_ENC)?.refresh).toBe("r");
  });

  it("发现的旧服务器进入服务器列表，当前服务器在最前", () => {
    const st = memStorage({
      chat_server_url: CUR,
      "chat_cache:http://old.example:u1": "x",
    });
    migrateLegacyStorage(st, CUR);
    const list = listServers(st);
    expect(list[0].url).toBe(CUR);
    expect(list.map((s) => s.url)).toContain("http://old.example");
  });

  it("凭据残缺（缺 refresh 或 user）时只删旧键不生成 auth", () => {
    const st = memStorage({ chat_access: "a1", chat_user: "{bad" });
    migrateLegacyStorage(st, CUR);
    expect(readAuth(st, CUR_ENC)).toBeNull();
    expect(st.raw.has("chat_access")).toBe(false);
    expect(st.raw.has("chat_user")).toBe(false);
  });
});

describe("kickReasonKey", () => {
  it("按服务器分键", () => {
    expect(kickReasonKey(encServer("http://a"))).toBe(
      `chat_kick_reason:${encServer("http://a")}`,
    );
    expect(kickReasonKey(encServer("http://a"))).not.toBe(
      kickReasonKey(encServer("http://b")),
    );
  });
});

// ===== T5：列表里同地址已登录用户名的记录（saveSession 走这里） =====
describe("upsertServer 记录最近登录用户名", () => {
  it("patch username/name 并顶到最前，未提供的字段保留", () => {
    const st = memStorage();
    upsertServer(st, "http://a", { username: "u1" });
    upsertServer(st, "http://a", { name: "测试服" });
    const e: ServerEntry = listServers(st)[0];
    expect(e.username).toBe("u1");
    expect(e.name).toBe("测试服");
  });
});

// ===== Multi-Account Registry =====
describe("multi-account chat_accounts_v1", () => {
  it("accountIdOf normalizes empty and explicit server urls", () => {
    expect(accountIdOf("", "u1")).not.toBe(accountIdOf("https://chat.example.com", "u1"));
    expect(accountIdOf("https://a.example", "u1")).toBe(
      accountIdOf("https://a.example/", "u1"),
    );
  });

  it("upsertSavedAccount inserts, updates existing without duplicate, and removeSavedAccount deletes", () => {
    const st = memStorage();
    upsertSavedAccount(st, {
      serverUrl: "https://a.example",
      serverName: "A 站",
      access: "acc-1",
      refresh: "ref-1",
      user: user("u1"),
    });
    upsertSavedAccount(st, {
      serverUrl: "https://b.example",
      serverName: "B 站",
      access: "acc-2",
      refresh: "ref-2",
      user: user("u2"),
    });
    upsertSavedAccount(st, {
      serverUrl: "https://a.example",
      serverName: "A 站重命名",
      access: "acc-1b",
      refresh: "ref-1b",
      user: user("u1"),
    });

    const list = listSavedAccounts(st);
    expect(list).toHaveLength(2);
    expect(list[0].user.id).toBe("u1");
    expect(list[0].serverName).toBe("A 站重命名");
    expect(list[0].access).toBe("acc-1b");

    const after = removeSavedAccount(st, list[0].id);
    expect(after).toHaveLength(1);
    expect(after[0].user.id).toBe("u2");
  });

  it("ignores malformed account storage JSON", () => {
    const st = memStorage({ chat_accounts_v1: "{bad" });
    expect(listSavedAccounts(st)).toEqual([]);
  });

  it("updateSavedAccountTokens updates credentials in place without reordering", () => {
    const st = memStorage();
    upsertSavedAccount(st, {
      serverUrl: "https://a.example",
      access: "a1",
      refresh: "r1",
      user: user("u1"),
      updatedAt: 100,
    });
    upsertSavedAccount(st, {
      serverUrl: "https://a.example",
      access: "a2",
      refresh: "r2",
      user: user("u2"),
      updatedAt: 200,
    });
    updateSavedAccountTokens(st, accountIdOf("https://a.example", "u1"), {
      access: "a1-new",
      refresh: "r1-new",
    });
    const list = listSavedAccounts(st);
    expect(list.map((a) => a.user.id)).toEqual(["u2", "u1"]);
    expect(list[1].access).toBe("a1-new");
    expect(list[1].refresh).toBe("r1-new");
    expect(list[1].updatedAt).toBe(100);
  });
});

describe("removeUserServerData", () => {
  it("cleans target user cache/outbox/reedit and strips draft reuse without harming sibling users on same server", () => {
    const scope = encServer("https://a.example");
    const st = memStorage({
      [authKey(scope)]: "{}",
      [`chat_cache:${scope}:u1`]: "c1",
      [`chat_outbox:${scope}:u1`]: "o1",
      [`chat_reedit:${scope}:u1`]: "r1",
      [`chat_drafts:${scope}:u1`]: JSON.stringify({
        c1: { text: "keep plain", reuse: { attachment_id: "att1", file_name: "f", mime: "text/plain", size: 1, kind: "file", conversation_id: "c1" } },
        c2: { text: "", reuse: { attachment_id: "att2", file_name: "f2", mime: "text/plain", size: 2, kind: "file", conversation_id: "c2" } },
      }),
      [`chat_cache:${scope}:u2`]: "c2-sibling",
      [`chat_outbox:${scope}:u2`]: "o2-sibling",
      [`chat_reedit:${scope}:u2`]: "r2-sibling",
      [`chat_drafts:${scope}:u2`]: JSON.stringify({ c9: { text: "sib", reuse: { attachment_id: "att9", file_name: "s", mime: "text/plain", size: 3, kind: "file", conversation_id: "c9" } } }),
    });

    removeUserServerData(st, scope, "u1");

    expect(st.raw.get(`chat_cache:${scope}:u1`)).toBeUndefined();
    expect(st.raw.get(`chat_outbox:${scope}:u1`)).toBeUndefined();
    expect(st.raw.get(`chat_reedit:${scope}:u1`)).toBeUndefined();
    expect(JSON.parse(st.raw.get(`chat_drafts:${scope}:u1`)!)).toEqual({ c1: "keep plain" });

    // Sibling u2 data + reuse chip remain completely untouched
    expect(st.raw.get(`chat_cache:${scope}:u2`)).toBe("c2-sibling");
    expect(st.raw.get(`chat_outbox:${scope}:u2`)).toBe("o2-sibling");
    expect(st.raw.get(`chat_reedit:${scope}:u2`)).toBe("r2-sibling");
    expect(JSON.parse(st.raw.get(`chat_drafts:${scope}:u2`)!).c9.reuse.attachment_id).toBe("att9");
    expect(st.raw.get(authKey(scope))).toBe("{}");
  });
});
