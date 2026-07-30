import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SESSION_EXPIRED_EVENT,
  SessionExpiredError,
  requestWithTokenRefresh,
} from "./auth-session";

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
  sessionStorage.clear();
  vi.unstubAllGlobals();
});

describe("requestWithTokenRefresh", () => {
  it("访问令牌过期后用 HttpOnly Cookie 刷新并只重试一次", async () => {
    sessionStorage.setItem("yang.token", "access-old");
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
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
    expect(sessionStorage.getItem("yang.token")).toBe("access-new");
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
  });

  it("并发 401 共享同一次刷新", async () => {
    sessionStorage.setItem("yang.token", "access-old");
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
    sessionStorage.setItem("yang.token", "access-old");
    sessionStorage.setItem("yang.tenant-id", "7");
    sessionStorage.setItem("yang.account-identity", "admin");
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
    expect(sessionStorage.getItem("yang.tenant-id")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("服务端判定 Refresh Cookie 缺失时结束会话", async () => {
    sessionStorage.setItem("yang.token", "access-without-cookie");
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

  it("刷新成功但重试仍为 401 时不进入刷新循环", async () => {
    sessionStorage.setItem("yang.token", "access-retry-old");
    const fetchMock = vi.fn(async () => tokenResponse("access-retry-new"));
    vi.stubGlobal("fetch", fetchMock);
    const request = vi.fn(async () => new Response(undefined, { status: 401 }));

    await expect(
      requestWithTokenRefresh("access-retry-old", request),
    ).rejects.toBeInstanceOf(SessionExpiredError);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(request).toHaveBeenCalledTimes(2);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
  });

  it("刷新网络失败时保留会话，允许稍后重试", async () => {
    sessionStorage.setItem("yang.token", "access-old");
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
    expect(sessionStorage.getItem("yang.token")).toBe("access-old");
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });
});
