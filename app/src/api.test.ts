import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  auth,
  browserDeviceLabel,
  clearSession,
  convs,
  deviceLabel,
  files,
  getServer,
  loadSession,
  removeServer,
  saveSession,
  savedAccounts,
  serverScope,
  serverInfo,
  setServer,
  switchToAccount,
  watchIdentity,
} from "./api";
import { accountIdOf, authKey, encServer, kickReasonKey, readAuth, upsertSavedAccount, writeAuth } from "./servers";
import { memStorage } from "./testStorage";
import type { AuthResponse } from "./types";

const login = (name: string): AuthResponse => ({ access_token: `${name}-access`, refresh_token: `${name}-refresh`, access_expires_in: 900, user: { id: name, username: name, display_name: name, role: "user", last_seen_at: null } });
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
};

// T5：凭据分键到 chat_auth:<enc(server)>，「当前服务器」钉在 sessionStorage 里
const TEST_SRV = "http://test.local";
const curAuth = () => readAuth(localStorage, serverScope());
const authAt = (srv: string) => readAuth(localStorage, encServer(srv));

beforeEach(() => {
  vi.stubGlobal("localStorage", memStorage());
  vi.stubGlobal("sessionStorage", memStorage());
  vi.stubGlobal("window", new EventTarget());
  vi.stubGlobal("location", { origin: "http://localhost", protocol: "http:" });
  setServer(TEST_SRV); // 钉住本「标签页」的当前服务器
  sessionStorage.setItem("chat_cur_server", TEST_SRV); // 再 import 的模块也要钉在同一台
  saveSession(login("old"));
});
afterEach(() => { clearSession(); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("server-info native FCM availability", () => {
  it.each([true, false, undefined, "true"])("parses %s strictly", async (available) => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      registration_mode: "open", android_fcm_available: available,
    }))));
    expect((await serverInfo()).android_fcm_available).toBe(available === true);
  });
});

describe("session-owned HTTP requests", () => {
  it.each([200, 204, 401])("rejects an old-session %s response without refresh or retry", async (status) => {
    const response = deferred<Response>();
    const fetcher = vi.fn(() => response.promise);
    vi.stubGlobal("fetch", fetcher);
    const request = convs.leave("old-conv");
    const rejected = expect(request).rejects.toThrow();
    saveSession(login("new"));
    setServer("https://new.example");
    response.resolve(new Response(status === 204 ? null : "{}", { status }));
    await rejected;
    expect(fetcher).toHaveBeenCalledTimes(1);
    // 新会话写在当时的 TEST_SRV 键下；作废的旧响应不得污染它
    expect(authAt(TEST_SRV)?.access).toBe("new-access");
  });

  it("keeps refresh promises and finalizers isolated across generations", async () => {
    vi.stubGlobal("navigator", {}); // 无 Web Locks 环境（A25 起真实环境会串行排队刷新）
    vi.useFakeTimers();
    const oldRefresh = deferred<Response>();
    const newRefresh = deferred<Response>();
    const refreshTokens: string[] = [];
    vi.stubGlobal("fetch", vi.fn((url: string, init?: RequestInit) => {
      if (url.endsWith("/auth/refresh")) {
        const token = JSON.parse(String(init?.body)).refresh_token;
        refreshTokens.push(token);
        return token === "old-refresh" ? oldRefresh.promise : newRefresh.promise;
      }
      const headers = init?.headers as Record<string, string>;
      return Promise.resolve(new Response(headers.authorization === "Bearer renewed-access" ? JSON.stringify(login("new").user) : "{}", { status: headers.authorization === "Bearer renewed-access" ? 200 : 401 }));
    }));
    const old = auth.me();
    const oldRejected = expect(old).rejects.toThrow();
    await vi.waitFor(() => expect(refreshTokens).toEqual(["old-refresh"]));
    saveSession(login("new"));
    const next = auth.me();
    await vi.waitFor(() => expect(refreshTokens).toEqual(["old-refresh", "new-refresh"]));
    oldRefresh.resolve(Response.json(login("old-renewed")));
    await oldRejected;
    await vi.runOnlyPendingTimersAsync();
    const parallel = auth.me();
    await Promise.resolve();
    newRefresh.resolve(Response.json({ ...login("new"), access_token: "renewed-access" }));
    expect(await next).toEqual(login("new").user);
    expect(await parallel).toEqual(login("new").user);
    expect(refreshTokens).toEqual(["old-refresh", "new-refresh"]);
  });
});

describe("authenticated attachment access", () => {
  it("sends the account credential only in the ticket request header", async () => {
    const fetcher = vi.fn(async () => Response.json({ ticket: "file.ticket.signature", expires_in: 600 }));
    vi.stubGlobal("fetch", fetcher);
    const { url } = await files.access("image", { thumb: true });
    expect(fetcher).toHaveBeenCalledWith(`${TEST_SRV}/v1/files/image/ticket`, expect.objectContaining({
      headers: { authorization: "Bearer old-access" },
    }));
    expect(url).toBe(`${TEST_SRV}/v1/files/image?thumb=1&ticket=file.ticket.signature`);
  });

  it("uses legacy URLs only when the ticket route is unsupported", async () => {
    const fetcher = vi.fn()
      .mockResolvedValueOnce(Response.json({ error: "unknown_api" }, { status: 404 }))
      .mockResolvedValueOnce(Response.json({ error: "file not found" }, { status: 404 }));
    vi.stubGlobal("fetch", fetcher);
    expect((await files.access("legacy")).url).toBe(`${TEST_SRV}/v1/files/legacy`);
    await expect(files.access("removed")).rejects.toMatchObject({ status: 404, message: "file not found" });
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it("discards an in-flight attachment ticket from a previous sign-in", async () => {
    const ticket = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => ticket.promise));
    const result = files.access("private");
    const rejected = expect(result).rejects.toThrow("session changed");
    saveSession(login("other-user"));
    ticket.resolve(Response.json({ ticket: "old.file.signature", expires_in: 600 }));
    await rejected;
  });
});

// ===== V2 P1-5 =====
describe("device labels", () => {
  const UA = {
    chrome: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    edge: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
    safari: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
    firefox: "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0",
    android: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
  };
  it.each([
    [UA.chrome, "Chrome · Windows"],
    [UA.edge, "Edge · Windows"],
    [UA.safari, "Safari · macOS"],
    [UA.firefox, "Firefox · Linux"],
    [UA.android, "Chrome · Android"],
    ["", "浏览器 · 未知系统"],
  ])("browserDeviceLabel(%j) → %s", (ua, want) => {
    expect(browserDeviceLabel(ua)).toBe(want);
  });

  it("uses Tauri labels inside the shell", () => {
    vi.stubGlobal("navigator", { userAgent: UA.chrome });
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    expect(deviceLabel()).toBe("Chat 桌面端 (Windows)");
    vi.stubGlobal("navigator", { userAgent: UA.android });
    expect(deviceLabel()).toBe("Chat Android");
  });

  it("falls back to UA parsing outside Tauri", () => {
    vi.stubGlobal("navigator", { userAgent: UA.safari });
    expect(deviceLabel()).toBe("Safari · macOS");
  });

  it("login/register carry the generated device_label", async () => {
    vi.stubGlobal("navigator", { userAgent: UA.chrome });
    const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => Response.json(login("u")));
    vi.stubGlobal("fetch", fetcher);
    await auth.login("u", "secret123");
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body)).device_label).toBe("Chrome · Windows");
    await auth.register("u", "secret123");
    expect(JSON.parse(String(fetcher.mock.calls[1][1]?.body)).device_label).toBe("Chrome · Windows");
  });
});

// ===== Agent A25 =====
describe("cross-tab refresh", () => {
  // 刷新换的是同一账号（beforeEach 登录的 old）的令牌
  const creds = (access: string, refresh: string): AuthResponse => ({
    ...login("old"),
    access_token: access,
    refresh_token: refresh,
  });
  /** 刷新响应手动放行的假后端：非刷新接口仅认 Bearer a2，其余 401 */
  const fakeBackend = () => {
    const refreshes: { token: string; respond: (r: Response) => void }[] = [];
    const authorized: string[] = [];
    const fetcher = vi.fn((url: string, init?: RequestInit) => {
      if (url.endsWith("/v1/auth/refresh")) {
        const d = deferred<Response>();
        refreshes.push({ token: JSON.parse(String(init?.body)).refresh_token, respond: d.resolve });
        return d.promise;
      }
      const at = (init?.headers as Record<string, string>).authorization;
      authorized.push(at);
      return Promise.resolve(
        at === "Bearer a2" ? Response.json(login("u").user) : new Response("{}", { status: 401 }),
      );
    });
    return { fetcher, refreshes, authorized };
  };
  /** 两个共享同一 localStorage 的独立模块实例，模拟两个标签页 */
  const freshPair = async () => {
    vi.resetModules();
    const a = await import("./api");
    vi.resetModules();
    const b = await import("./api");
    return [a, b];
  };

  it("keeps the rotated tokens when this tab's refresh is rejected", async () => {
    vi.stubGlobal("navigator", {}); // 无 Web Locks → 走「令牌已被换掉」退路
    const { fetcher, refreshes, authorized } = fakeBackend();
    vi.stubGlobal("fetch", fetcher);
    const [A, B] = await freshPair();
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);

    const ra = A.auth.me();
    const rb = B.auth.me();
    await vi.waitFor(() => expect(refreshes.length).toBe(2));
    // 先到的标签页刷新成功并写回 r2；后到的拿旧令牌被服务端拒绝
    refreshes[0].respond(Response.json(creds("a2", "r2")));
    await vi.waitFor(() => expect(curAuth()?.refresh).toBe("r2"));
    refreshes[1].respond(new Response("{}", { status: 401 }));
    expect(await ra).toEqual(login("u").user);
    expect(await rb).toEqual(login("u").user);
    expect(refreshes.map((r) => r.token)).toEqual(["old-refresh", "old-refresh"]);
    expect(curAuth()?.access).toBe("a2");
    expect(authorized.filter((a) => a === "Bearer a2").length).toBe(2);
    expect(logout).not.toHaveBeenCalled();
  });

  it("serializes refreshes through Web Locks so the server sees one request", async () => {
    let tail: Promise<unknown> = Promise.resolve();
    const request = vi.fn((_name: string, cb: () => Promise<boolean>) => {
      const p = tail.then(cb);
      tail = p.then(() => undefined, () => undefined);
      return p;
    });
    vi.stubGlobal("navigator", { locks: { request } });
    const { fetcher, refreshes } = fakeBackend();
    vi.stubGlobal("fetch", fetcher);
    const [A, B] = await freshPair();
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);

    const ra = A.auth.me();
    const rb = B.auth.me();
    // 第二个标签页拿到锁时发现令牌已换，不再请求服务端
    await vi.waitFor(() => expect(refreshes.length).toBe(1));
    refreshes[0].respond(Response.json(creds("a2", "r2")));
    expect(await ra).toEqual(login("u").user);
    expect(await rb).toEqual(login("u").user);
    expect(request).toHaveBeenCalledTimes(2);
    expect(refreshes.length).toBe(1);
    expect(curAuth()?.access).toBe("a2");
    expect(logout).not.toHaveBeenCalled();
  });

  it("clears the session and emits chat:logout when the token is untouched and the server rejects", async () => {
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("fetch", vi.fn((url: string) =>
      url.endsWith("/auth/refresh")
        ? Promise.resolve(Response.json({ error: "invalid refresh token" }, { status: 401 }))
        : Promise.resolve(new Response("{}", { status: 401 }))));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);

    await expect(auth.me()).rejects.toMatchObject({ status: 401 });
    expect(curAuth()).toBeNull();
    expect(logout).toHaveBeenCalledTimes(1);
  });
});

// 另一个标签页直接改写共享的 localStorage（不经过本模块），改的是本页服务器的凭据键
const otherTabLogin = (name: string) => {
  const r = login(name);
  writeAuth(localStorage, serverScope(), {
    access: r.access_token,
    refresh: r.refresh_token,
    user: r.user,
  });
};

describe("cross-tab identity", () => {
  it("never sends another tab's account token from this tab", async () => {
    const fetcher = vi.fn(() => Promise.resolve(new Response("{}", { status: 200 })));
    vi.stubGlobal("fetch", fetcher);
    otherTabLogin("bob");
    await expect(convs.rename("g1", "x")).rejects.toThrow("session changed");
    expect(fetcher).not.toHaveBeenCalled();
    // 也不能替别的标签页登出
    auth.logout();
    expect(curAuth()?.refresh).toBe("bob-refresh");
  });

  it("token rotation of the same account keeps working", async () => {
    const fetcher = vi.fn((_url: string, _init?: RequestInit) => Promise.resolve(new Response("{}", { status: 200 })));
    vi.stubGlobal("fetch", fetcher);
    otherTabLogin("old"); // 同一用户 id，只是换了令牌
    writeAuth(localStorage, serverScope(), {
      access: "rotated-access",
      refresh: "old-refresh",
      user: login("old").user,
    });
    await convs.rename("g1", "x");
    const headers = fetcher.mock.calls[0][1]?.headers as Record<string, string>;
    expect(headers.authorization).toBe("Bearer rotated-access");
  });

  it("notifies when another tab switches account on this server, ignores other servers' keys", () => {
    const onChange = vi.fn();
    watchIdentity(onChange);
    const storageEvent = (key: string) => Object.assign(new Event("storage"), { key });
    // 别的服务器的凭据键动了：与本页无关
    localStorage.setItem(authKey(encServer("http://other")), "x");
    window.dispatchEvent(storageEvent(authKey(encServer("http://other"))));
    // 本服务器的键被改但仍是同一账号（令牌轮换）→ 不通知
    writeAuth(localStorage, serverScope(), {
      access: "rotated",
      refresh: "old-refresh",
      user: login("old").user,
    });
    window.dispatchEvent(storageEvent(authKey(serverScope())));
    expect(onChange).not.toHaveBeenCalled();
    otherTabLogin("bob");
    window.dispatchEvent(storageEvent(authKey(serverScope())));
    expect(onChange).toHaveBeenCalledTimes(1);
  });
});

// ===== T5：凭据按服务器隔离 =====
describe("per-server credentials", () => {
  const A = "http://a.example";
  const B = "http://b.example";

  it("keeps credentials isolated per server: switching back needs no re-login", () => {
    setServer(A);
    saveSession(login("alice"));
    setServer(B);
    expect(loadSession()).toBeNull(); // B 上还没有凭据
    saveSession(login("bob"));
    expect(loadSession()?.user.id).toBe("bob");
    // 切回 A：凭据还在，直接读回
    setServer(A);
    expect(loadSession()?.user.id).toBe("alice");
    expect(authAt(B)?.user.id).toBe("bob");
    setServer(TEST_SRV);
  });

  it("denied refresh clears only this server's credentials, not the other server's", async () => {
    vi.stubGlobal("navigator", {});
    writeAuth(localStorage, encServer(A), {
      access: "a-a",
      refresh: "r-a",
      user: login("alice").user,
    });
    writeAuth(localStorage, encServer(B), {
      access: "a-b",
      refresh: "r-b",
      user: login("bob").user,
    });
    setServer(A);
    vi.stubGlobal("fetch", vi.fn((url: string) =>
      url.endsWith("/auth/refresh")
        ? Promise.resolve(Response.json({ error: "invalid refresh token" }, { status: 401 }))
        : Promise.resolve(new Response("{}", { status: 401 }))));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.me()).rejects.toMatchObject({ status: 401 });
    expect(authAt(A)).toBeNull(); // 只有 A 被清
    expect(authAt(B)?.refresh).toBe("r-b");
    expect(sessionStorage.getItem(kickReasonKey(encServer(A)))).toBe("登录已失效，请重新登录");
    expect(logout).toHaveBeenCalledTimes(1);
    setServer(TEST_SRV);
  });

  it("clearSession only removes this server's auth and cache", () => {
    writeAuth(localStorage, encServer(B), {
      access: "a-b",
      refresh: "r-b",
      user: login("bob").user,
    });
    localStorage.setItem(`chat_cache:${encServer(B)}:bob`, "snap-b");
    localStorage.setItem(`chat_cache:${serverScope()}:old`, "snap-a");
    clearSession();
    expect(curAuth()).toBeNull();
    expect(localStorage.getItem(`chat_cache:${serverScope()}:old`)).toBeNull();
    expect(authAt(B)?.refresh).toBe("r-b");
    expect(localStorage.getItem(`chat_cache:${encServer(B)}:bob`)).toBe("snap-b");
  });
});

describe("refresh failures", () => {
  it.each([
    ["503", () => Promise.resolve(new Response("{}", { status: 503 }))],
    ["network error", () => Promise.reject(new TypeError("Failed to fetch"))],
    ["429", () => Promise.resolve(Response.json({ error: "too many requests" }, { status: 429 }))],
    ["404 (旧服务端/代理)", () => Promise.resolve(Response.json({ error: "unknown_api" }, { status: 404 }))],
    ["代理 401（非 JSON）", () => Promise.resolve(new Response("Unauthorized", { status: 401 }))],
    ["陌生 JSON 401", () => Promise.resolve(Response.json({ error: "sso required" }, { status: 401 }))],
    ["其他 403", () => Promise.resolve(Response.json({ error: "forbidden" }, { status: 403 }))],
  ])("keeps the session when refresh hits %s", async (_label, refreshResponse) => {
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("fetch", vi.fn((url: string) =>
      url.endsWith("/auth/refresh") ? refreshResponse() : Promise.resolve(new Response("{}", { status: 401 }))));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.me()).rejects.toMatchObject({ status: 503 });
    expect(curAuth()?.refresh).toBe("old-refresh");
    expect(logout).not.toHaveBeenCalled();
  });

  it.each([
    ["invalid refresh token", 401, "登录已失效，请重新登录"],
    ["session expired, please login again", 401, "登录已失效，请重新登录"],
    ["账号已被禁用", 403, "账号已被禁用，请联系管理员"],
  ])("logs out with a hint when refresh is explicitly rejected: %s", async (err, status, reason) => {
    vi.stubGlobal("navigator", {});
    const mem = new Map<string, string>();
    vi.stubGlobal("sessionStorage", {
      getItem: (k: string) => mem.get(k) ?? null,
      setItem: (k: string, v: string) => void mem.set(k, v),
      removeItem: (k: string) => void mem.delete(k),
    });
    vi.stubGlobal("fetch", vi.fn((url: string) =>
      url.endsWith("/auth/refresh")
        ? Promise.resolve(Response.json({ error: err }, { status }))
        : Promise.resolve(new Response("{}", { status: 401 }))));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.me()).rejects.toMatchObject({ status: 401 });
    expect(curAuth()).toBeNull();
    expect(logout).toHaveBeenCalledTimes(1);
    expect(mem.get(kickReasonKey(serverScope()))).toBe(reason);
  });

  it("logs out when the server rejects the refresh token", async () => {
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("fetch", vi.fn((url: string) =>
      url.endsWith("/auth/refresh")
        ? Promise.resolve(Response.json({ error: "invalid refresh token" }, { status: 401 }))
        : Promise.resolve(new Response("{}", { status: 401 }))));
    await expect(auth.me()).rejects.toMatchObject({ status: 401 });
    expect(curAuth()).toBeNull();
  });

  it("keeps the session when a middlebox 401s the retried request after a successful refresh", async () => {
    vi.stubGlobal("navigator", {});
    let calls = 0;
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url.endsWith("/auth/refresh"))
        return Promise.resolve(Response.json({ ...login("old"), access_token: "a2", refresh_token: "r2" }));
      calls++;
      // 初次 401 触发 refresh；换新 token 重试时被代理拦下（非本服务 JSON 报文）
      return Promise.resolve(new Response("Unauthorized", { status: 401 }));
    }));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.me()).rejects.toMatchObject({ status: 503 });
    expect(calls).toBe(2);
    expect(curAuth()?.refresh).toBe("r2");
    expect(logout).not.toHaveBeenCalled();
  });

  it("logs out when the retried request is rejected by the server after a successful refresh", async () => {
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      if (url.endsWith("/auth/refresh"))
        return Promise.resolve(Response.json({ ...login("old"), access_token: "a2", refresh_token: "r2" }));
      return Promise.resolve(
        Response.json({ error: "session expired, please login again" }, { status: 401 }),
      );
    }));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.me()).rejects.toMatchObject({ status: 401 });
    expect(curAuth()).toBeNull();
    expect(logout).toHaveBeenCalledTimes(1);
  });

  it("logs out with the disabled hint when any endpoint answers 「账号已被禁用」", async () => {
    vi.stubGlobal("navigator", {});
    const mem = new Map<string, string>();
    vi.stubGlobal("sessionStorage", {
      getItem: (k: string) => mem.get(k) ?? null,
      setItem: (k: string, v: string) => void mem.set(k, v),
      removeItem: (k: string) => void mem.delete(k),
    });
    vi.stubGlobal("fetch", vi.fn(() =>
      Promise.resolve(Response.json({ error: "账号已被禁用" }, { status: 403 }))));
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(convs.list()).rejects.toMatchObject({ status: 403 });
    expect(curAuth()).toBeNull();
    expect(logout).toHaveBeenCalledTimes(1);
    expect(mem.get(kickReasonKey(serverScope()))).toBe("账号已被禁用，请联系管理员");
  });
});

// ===== T1：登录/注册错误不触发刷新与登出 =====
describe("login/register errors", () => {
  it("maps a wrong-password 401 to 「用户名或密码错误」 without refreshing or clearing the session", async () => {
    const fetcher = vi.fn((_url: string, _init?: RequestInit) =>
      Promise.resolve(Response.json({ error: "invalid credentials" }, { status: 401 })));
    vi.stubGlobal("fetch", fetcher);
    const logout = vi.fn();
    window.addEventListener("chat:logout", logout);
    await expect(auth.login("old", "wrong")).rejects.toMatchObject({
      status: 401,
      message: "用户名或密码错误",
    });
    // 登录接口返回 401 时不得尝试刷新令牌（只调了一次接口）
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(String(fetcher.mock.calls[0][0])).toContain("/auth/login");
    // 已有会话凭据原样保留
    expect(curAuth()?.refresh).toBe("old-refresh");
    expect(logout).not.toHaveBeenCalled();
  });

  it("surfaces register errors as-is without refreshing", async () => {
    const fetcher = vi.fn((_url: string, _init?: RequestInit) =>
      Promise.resolve(Response.json({ error: "username already taken" }, { status: 409 })));
    vi.stubGlobal("fetch", fetcher);
    await expect(auth.register("old", "secret123")).rejects.toMatchObject({
      status: 409,
      message: "username already taken",
    });
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(curAuth()?.refresh).toBe("old-refresh");
  });
});


// ===== P0-1：成员资格错误中文化 =====
describe("membership error mapping", () => {
  it("maps a 「not a member」403 to a Chinese message with a stable not_member code", async () => {
    vi.stubGlobal("fetch", vi.fn(() =>
      Promise.resolve(Response.json({ error: "not a member of this conversation" }, { status: 403 }))));
    await expect(convs.leave("g1")).rejects.toMatchObject({
      status: 403,
      code: "not_member",
      message: "你已不在该群",
    });
  });

  it("leaves other error bodies untouched", async () => {
    vi.stubGlobal("fetch", vi.fn(() =>
      Promise.resolve(Response.json({ error: "only the owner can dissolve" }, { status: 403 }))));
    await expect(convs.dissolve("g1")).rejects.toMatchObject({
      status: 403,
      message: "only the owner can dissolve",
    });
  });
});

// ===== T1：请求超时，不可达时不挂在「加载中…」 =====
describe("request timeout", () => {
  it("aborts a hanging request and rejects as transient (status 0)", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn((_url: string, init?: RequestInit) =>
      new Promise((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
      }),
    ));
    const request = auth.me();
    const rejected = expect(request).rejects.toMatchObject({ status: 0 });
    await vi.advanceTimersByTimeAsync(10_000);
    await rejected;
  });
});

// ===== Multi-account isolation & cleanup =====
describe("multi-account switch, logout, and server removal", () => {
  it("switchToAccount preserves sibling account cache/drafts and emits chat:before-switch-account before changing server", () => {
    const alice = login("alice");
    const bob = login("bob");
    const scopeTest = encServer(TEST_SRV);
    const events: Array<{ type: string; server: string }> = [];
    window.addEventListener("chat:before-switch-account", () => {
      events.push({ type: "before", server: getServer() });
    });
    window.addEventListener("chat:switch-account", () => {
      events.push({ type: "after", server: getServer() });
    });

    setServer(TEST_SRV);
    saveSession(alice);
    localStorage.setItem(`chat_cache:${scopeTest}:alice`, "alice-snap");
    localStorage.setItem(`chat_drafts:${scopeTest}:alice`, JSON.stringify({ c1: "alice draft" }));

    const OTHER_SRV = "https://other.example";
    const scopeOther = encServer(OTHER_SRV);
    upsertSavedAccount(localStorage, {
      serverUrl: OTHER_SRV,
      access: bob.access_token,
      refresh: bob.refresh_token,
      user: bob.user,
    });
    localStorage.setItem(`chat_cache:${scopeOther}:bob`, "bob-snap");

    // Also store a sibling user on TEST_SRV with rotated tokens in authKey
    upsertSavedAccount(localStorage, {
      serverUrl: TEST_SRV,
      access: "alice-stale",
      refresh: "alice-stale-rt",
      user: alice.user,
    });

    const bobAccId = accountIdOf(OTHER_SRV, "bob");
    const switched = switchToAccount(bobAccId);
    expect(switched?.user.id).toBe("bob");
    expect(getServer()).toBe(OTHER_SRV);
    expect(events).toEqual([
      { type: "before", server: TEST_SRV },
      { type: "after", server: OTHER_SRV },
    ]);
    // Outgoing rotated tokens from TEST_SRV authKey were synced back to savedAccounts
    const savedAlice = savedAccounts().find((a) => a.id === accountIdOf(TEST_SRV, "alice"));
    expect(savedAlice?.access).toBe("alice-access");
    expect(savedAlice?.refresh).toBe("alice-refresh");
    expect(localStorage.getItem(`chat_cache:${scopeTest}:alice`)).toBe("alice-snap");

    // Switching back to alice on TEST_SRV preserves bob cache too
    switchToAccount(accountIdOf(TEST_SRV, "alice"));
    expect(localStorage.getItem(`chat_cache:${scopeOther}:bob`)).toBe("bob-snap");
  });

  it("clearSession only cleans active user data and leaves sibling saved account on same server intact", () => {
    const scope = serverScope();
    const bob = login("bob");
    upsertSavedAccount(localStorage, {
      serverUrl: TEST_SRV,
      access: bob.access_token,
      refresh: bob.refresh_token,
      user: bob.user,
    });
    localStorage.setItem(`chat_cache:${scope}:old`, "old-cache");
    localStorage.setItem(`chat_reedit:${scope}:old`, "old-reedit");
    localStorage.setItem(
      `chat_drafts:${scope}:old`,
      JSON.stringify({
        c1: { text: "keep text", reuse: { attachment_id: "att1", file_name: "a", mime: "text/plain", size: 1, kind: "file", conversation_id: "c1" } },
      }),
    );

    localStorage.setItem(`chat_cache:${scope}:bob`, "bob-cache");
    localStorage.setItem(`chat_reedit:${scope}:bob`, "bob-reedit");
    localStorage.setItem(
      `chat_drafts:${scope}:bob`,
      JSON.stringify({
        c2: { text: "bob text", reuse: { attachment_id: "att2", file_name: "b", mime: "text/plain", size: 2, kind: "file", conversation_id: "c2" } },
      }),
    );

    clearSession();

    expect(curAuth()).toBeNull();
    expect(localStorage.getItem(`chat_cache:${scope}:old`)).toBeNull();
    expect(localStorage.getItem(`chat_reedit:${scope}:old`)).toBeNull();
    expect(JSON.parse(localStorage.getItem(`chat_drafts:${scope}:old`)!)).toEqual({ c1: "keep text" });

    // Sibling saved account on same server remains intact
    expect(savedAccounts().some((a) => a.user.id === "bob")).toBe(true);
    expect(localStorage.getItem(`chat_cache:${scope}:bob`)).toBe("bob-cache");
    expect(localStorage.getItem(`chat_reedit:${scope}:bob`)).toBe("bob-reedit");
    expect(JSON.parse(localStorage.getItem(`chat_drafts:${scope}:bob`)!).c2.reuse.attachment_id).toBe("att2");
  });

  it("removeServer revokes refresh tokens for all saved accounts on that server and removes them", () => {
    const SRV_A = "http://srv-a.example";
    const alice = login("alice");
    const bob = login("bob");
    writeAuth(localStorage, encServer(SRV_A), {
      access: alice.access_token,
      refresh: alice.refresh_token,
      user: alice.user,
    });
    upsertSavedAccount(localStorage, {
      serverUrl: SRV_A,
      access: alice.access_token,
      refresh: alice.refresh_token,
      user: alice.user,
    });
    upsertSavedAccount(localStorage, {
      serverUrl: SRV_A,
      access: bob.access_token,
      refresh: bob.refresh_token,
      user: bob.user,
    });

    const fetcher = vi.fn((_url?: string, _init?: RequestInit) =>
      Promise.resolve(new Response(null, { status: 204 })),
    );
    vi.stubGlobal("fetch", fetcher);

    removeServer(SRV_A);

    const loggedOutTokens = fetcher.mock.calls
      .filter((c) => String(c[0]).startsWith(`${SRV_A}/v1/auth/logout`))
      .map((c) => JSON.parse(String((c[1] as RequestInit | undefined)?.body)).refresh_token);
    expect(loggedOutTokens.sort()).toEqual(["alice-refresh", "bob-refresh"]);
    expect(savedAccounts().some((a) => a.serverUrl === SRV_A)).toBe(false);
    expect(localStorage.getItem(authKey(encServer(SRV_A)))).toBeNull();
  });
});
