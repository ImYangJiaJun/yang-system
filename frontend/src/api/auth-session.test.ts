import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SESSION_EXPIRED_EVENT,
  SessionExpiredError,
  requestWithTokenRefresh,
} from "./auth-session";

function tokenResponse(accessToken: string, refreshToken: string) {
  return new Response(
    JSON.stringify({
      code: 0,
      message: "成功",
      data: {
        access_token: accessToken,
        refresh_token: refreshToken,
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
  it("访问令牌过期后刷新 Token 对并只重试一次", async () => {
    sessionStorage.setItem("yang.token", "access-old");
    sessionStorage.setItem("yang.refresh-token", "refresh-old");
    const fetchMock = vi.fn(async () =>
      tokenResponse("access-new", "refresh-new"),
    );
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
    expect(sessionStorage.getItem("yang.refresh-token")).toBe("refresh-new");
  });

  it("并发 401 共享同一次刷新", async () => {
    sessionStorage.setItem("yang.token", "access-old");
    sessionStorage.setItem("yang.refresh-token", "refresh-old");
    const fetchMock = vi.fn(async () =>
      tokenResponse("access-new", "refresh-new"),
    );
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
    sessionStorage.setItem("yang.refresh-token", "refresh-invalid");
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

  it("缺少 Refresh Token 时直接结束不完整会话", async () => {
    sessionStorage.setItem("yang.token", "access-without-refresh");
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);

    await expect(
      requestWithTokenRefresh(
        "access-without-refresh",
        async () => new Response(undefined, { status: 401 }),
      ),
    ).rejects.toBeInstanceOf(SessionExpiredError);

    expect(expired).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });

  it("刷新成功但重试仍为 401 时不进入刷新循环", async () => {
    sessionStorage.setItem("yang.token", "access-retry-old");
    sessionStorage.setItem("yang.refresh-token", "refresh-retry-old");
    const fetchMock = vi.fn(async () =>
      tokenResponse("access-retry-new", "refresh-retry-new"),
    );
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
    sessionStorage.setItem("yang.refresh-token", "refresh-old");
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
    expect(sessionStorage.getItem("yang.refresh-token")).toBe("refresh-old");
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });
});
