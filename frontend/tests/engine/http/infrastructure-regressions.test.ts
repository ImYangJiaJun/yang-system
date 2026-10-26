import { afterEach, describe, expect, it, vi } from "vitest";
import { parseJson } from "@/engine/http/http";
import { login } from "@/engine/session/lifecycle";
import {
  clearStoredSession,
  persistTokenPair,
} from "@/engine/session/auth-session";
import {
  changeUsername,
  fetchAvatar,
  listSessions,
} from "@/features/account/api";
import { issuePasswordReset, listUsers } from "@/features/account/admin-api";

afterEach(() => {
  clearStoredSession();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("基础设施请求边界", () => {
  it("无效 JSON 保留 HTTP 状态与请求编号，不泄露网关正文", async () => {
    const result = parseJson(
      new Response("<html>secret-token=private</html>", {
        status: 502,
        headers: { "x-request-id": "request-502" },
      }),
    );
    await expect(result).rejects.toMatchObject({
      status: 502,
      requestId: "request-502",
    });
    await expect(result).rejects.not.toThrow("private");
  });

  it("登录预算覆盖响应体读取，超时不重发未知结果的 POST", async () => {
    vi.useFakeTimers();
    let signal!: AbortSignal;
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      signal = init.signal as AbortSignal;
      return new Response(
        new ReadableStream({
          start(controller) {
            signal?.addEventListener(
              "abort",
              () => controller.error(signal.reason),
              { once: true },
            );
          },
        }),
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    const pending = login("user", "password").catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(60_001);
    expect(signal?.aborted).toBe(true);
    expect(await pending).toMatchObject({ name: "TimeoutError" });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it.each([
    [
      "修改用户名",
      () => changeUsername("next", "old"),
      { relogin_required: true, immediate_convergence: true },
    ],
    ["读取头像", () => fetchAvatar(1, "old"), { etag: null, data_url: null }],
    ["读取会话", () => listSessions("old"), { sessions: [] }],
    ["管理用户", () => listUsers("old", 1), { users: [] }],
    [
      "签发重置凭证",
      () => issuePasswordReset("old", 1),
      { reset_token: "one-time", expires_in: 60 },
    ],
  ] as const)("%s 共享401续期，保持一次重发边界", async (_name, run, data) => {
    persistTokenPair({ accessToken: "old" });
    const authorizations: (string | null)[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        if (url.endsWith("/refresh"))
          return new Response(
            JSON.stringify({ code: 0, data: { access_token: "fresh" } }),
          );
        const authorization = new Headers(init.headers).get("authorization");
        authorizations.push(authorization);
        return authorization === "Bearer old"
          ? new Response("{}", { status: 401 })
          : new Response(JSON.stringify({ code: 0, data }));
      }),
    );
    await run();
    expect(authorizations).toEqual(["Bearer old", "Bearer fresh"]);
  });
});
