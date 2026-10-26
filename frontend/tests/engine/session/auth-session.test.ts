import { afterEach, describe, expect, it, vi } from "vitest";
import {
  activeAccessToken,
  clearStoredSession,
  persistTokenPair,
  SESSION_EXPIRED_EVENT,
  SessionExpiredError,
  requestWithTokenRefresh,
  SESSION_REFRESHED_EVENT,
  restoreSessionFromCookie,
} from "@/engine/session/auth-session";

function tokenResponse(accessToken: string) {
  return new Response(
    JSON.stringify({
      code: 0,
      message: "成功",
      data: {
        access_token: accessToken,
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

afterEach(() => {
  vi.useRealTimers();
  clearStoredSession();
  vi.unstubAllGlobals();
});

describe("requestWithTokenRefresh", () => {
  it("旧刷新结束不能清掉新一代的单飞请求", async () => {
    const releases: ((response: Response) => void)[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            releases.push(resolve);
          }),
      ),
    );
    const old = restoreSessionFromCookie().catch((error: unknown) => error);
    await vi.waitFor(() => expect(releases).toHaveLength(1));
    clearStoredSession();
    const current = restoreSessionFromCookie();
    await vi.waitFor(() => expect(releases).toHaveLength(2));
    releases[0](tokenResponse("obsolete"));
    await old;
    const same = restoreSessionFromCookie();
    expect(releases).toHaveLength(2);
    releases[1](tokenResponse("current"));
    await expect(current).resolves.toEqual({ accessToken: "current" });
    await expect(same).resolves.toEqual({ accessToken: "current" });
  });
  it.each([200, 401, 503])(
    "旧 refresh 的 %s 响应不能覆盖新登录或发出会话事件",
    async (status) => {
      persistTokenPair({ accessToken: "old" });
      let release!: (response: Response) => void;
      vi.stubGlobal(
        "fetch",
        vi.fn(
          () =>
            new Promise<Response>((resolve) => {
              release = resolve;
            }),
        ),
      );
      const refreshed = vi.fn();
      const expired = vi.fn();
      window.addEventListener(SESSION_REFRESHED_EVENT, refreshed);
      window.addEventListener(SESSION_EXPIRED_EVENT, expired);
      const pending = requestWithTokenRefresh(
        "old",
        async () => new Response(null, { status: 401 }),
      ).catch((error: unknown) => error);
      await vi.waitFor(() => expect(release).toBeTypeOf("function"));
      persistTokenPair({ accessToken: "new-login" });
      release(
        status === 200
          ? tokenResponse("obsolete")
          : new Response("{}", { status }),
      );
      await pending;
      expect(activeAccessToken()).toBe("new-login");
      expect(refreshed).not.toHaveBeenCalled();
      expect(expired).not.toHaveBeenCalled();
      window.removeEventListener(SESSION_REFRESHED_EVENT, refreshed);
      window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
    },
  );

  it("刷新总预算包含 Web Locks 等待，取消后不会执行刷新", async () => {
    vi.useFakeTimers();
    let signal: AbortSignal | undefined;
    vi.stubGlobal("navigator", {
      locks: {
        request: (_name: string, options: LockOptions) => {
          signal = options.signal;
          return new Promise((_resolve, reject) =>
            signal?.addEventListener("abort", () => reject(signal?.reason), {
              once: true,
            }),
          );
        },
      },
    });
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const pending = restoreSessionFromCookie().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(10_001);
    expect(signal?.aborted).toBe(true);
    expect(await pending).toMatchObject({ name: "TimeoutError" });
    expect(fetchMock).not.toHaveBeenCalled();
    vi.useRealTimers();
  });
  it("访问令牌过期后用 HttpOnly Cookie 刷新并只重试一次", async () => {
    persistTokenPair({ accessToken: "access-old" });
    const requestLock = vi.fn(
      async (
        _name: string,
        _options: LockOptions,
        callback: (lock: object) => Promise<unknown>,
      ) => callback({ name: "yang.session.refresh", mode: "exclusive" }),
    );
    vi.stubGlobal("navigator", { locks: { request: requestLock } });
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.signal).toBeInstanceOf(AbortSignal);
      expect(init.credentials).toBe("include");
      expect(init.body).toBe("{}");
      return tokenResponse("access-new");
    });
    vi.stubGlobal("fetch", fetchMock);
    const request = vi.fn(async (accessToken?: string) =>
      accessToken === "access-old"
        ? new Response(undefined, { status: 401 })
        : new Response(undefined, { status: 200 }),
    );

    await expect(
      requestWithTokenRefresh("access-old", request),
    ).resolves.toMatchObject({ status: 200 });
    expect(request.mock.calls.map(([token]) => token)).toEqual([
      "access-old",
      "access-new",
    ]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(requestLock).toHaveBeenCalledOnce();
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
  });

  it("并发 401 共享同一次刷新", async () => {
    persistTokenPair({ accessToken: "access-old" });
    const fetchMock = vi.fn(async () => tokenResponse("access-new"));
    vi.stubGlobal("fetch", fetchMock);
    const request = vi.fn(
      async (accessToken?: string) =>
        new Response(undefined, {
          status: accessToken === "access-old" ? 401 : 200,
        }),
    );

    const responses = await Promise.all([
      requestWithTokenRefresh("access-old", request),
      requestWithTokenRefresh("access-old", request),
    ]);

    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("刷新被拒绝时清空会话并只发出一次过期事件", async () => {
    persistTokenPair({ accessToken: "access-old" });
    sessionStorage.setItem("yang.account-identity", "user");
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ code: 40102, message: "Token 已过期" }),
            {
              status: 401,
              headers: { "content-type": "application/json" },
            },
          ),
      ),
    );
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);

    await expect(
      requestWithTokenRefresh(
        "access-old",
        async () => new Response(undefined, { status: 401 }),
      ),
    ).rejects.toBeInstanceOf(SessionExpiredError);

    expect(expired).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("服务端判定 Refresh Cookie 缺失时结束会话", async () => {
    persistTokenPair({ accessToken: "access-without-cookie" });
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ code: 40102, message: "刷新会话 Cookie 缺失" }),
            {
              status: 401,
              headers: { "content-type": "application/json" },
            },
          ),
      ),
    );
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);

    await expect(
      requestWithTokenRefresh(
        "access-without-cookie",
        async () => new Response(undefined, { status: 401 }),
      ),
    ).rejects.toBeInstanceOf(SessionExpiredError);

    expect(expired).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("刷新成功但重试仍是业务 401 时不结束会话，交给调用方处理", async () => {
    persistTokenPair({ accessToken: "access-retry-old" });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => tokenResponse("access-retry-new")),
    );
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);
    const request = vi.fn(async () => new Response(undefined, { status: 401 }));

    await expect(
      requestWithTokenRefresh("access-retry-old", request),
    ).resolves.toMatchObject({ status: 401 });

    expect(request).toHaveBeenCalledTimes(2);
    expect(expired).not.toHaveBeenCalled();
    // 会话仍然有效：新 Token 已写入且没有清空本地状态。
    expect(activeAccessToken()).toBe("access-retry-new");
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("新 Token 再次过期时仍能再次刷新，不触发过期事件", async () => {
    persistTokenPair({ accessToken: "access-t0" });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(tokenResponse("access-t1"))
      .mockResolvedValueOnce(tokenResponse("access-t2"));
    vi.stubGlobal("fetch", fetchMock);
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);
    const seen: Array<string | undefined> = [];
    let attempt = 0;
    const request = vi.fn(async (accessToken?: string) => {
      seen.push(accessToken);
      attempt += 1;
      // 第 1 次（T0）与第 3 次（T1 的首次使用）401，其余放行。
      return new Response(undefined, {
        status: attempt === 1 || attempt === 3 ? 401 : 200,
      });
    });

    await expect(
      requestWithTokenRefresh("access-t0", request),
    ).resolves.toMatchObject({ status: 200 });
    await expect(
      requestWithTokenRefresh(activeAccessToken(), request),
    ).resolves.toMatchObject({ status: 200 });

    expect(seen).toEqual(["access-t0", "access-t1", "access-t1", "access-t2"]);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(expired).not.toHaveBeenCalled();
    expect(activeAccessToken()).toBe("access-t2");
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("刷新超时按可重试错误处理并保留会话", async () => {
    persistTokenPair({ accessToken: "access-old" });
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.signal).toBeInstanceOf(AbortSignal);
      throw new DOMException("The operation timed out.", "TimeoutError");
    });
    vi.stubGlobal("fetch", fetchMock);
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);

    await expect(
      requestWithTokenRefresh(
        "access-old",
        async () => new Response(undefined, { status: 401 }),
      ),
    ).rejects.toThrow("timed out");

    expect(expired).not.toHaveBeenCalled();
    expect(activeAccessToken()).toBe("access-old");
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("刷新网络失败时保留会话，允许稍后重试", async () => {
    persistTokenPair({ accessToken: "access-old" });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => Promise.reject(new Error("offline"))),
    );
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);

    await expect(
      requestWithTokenRefresh(
        "access-old",
        async () => new Response(undefined, { status: 401 }),
      ),
    ).rejects.toThrow("offline");

    expect(expired).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });
});
