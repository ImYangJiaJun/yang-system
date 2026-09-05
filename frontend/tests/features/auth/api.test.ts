import { afterEach, describe, expect, it, vi } from "vitest";

import {
  disableAccount,
  login,
  logout,
  refreshSession,
} from "@/engine/session/lifecycle";
import {
  register,
  requestRegistrationEmail,
  resetPassword,
} from "@/features/auth/api";
import { ApiError, StepUpRequiredError } from "@/engine/http/errors";

afterEach(() => vi.unstubAllGlobals());

describe("login", () => {
  it("只发送用户名和密码并只暴露 Access Token", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.method).toBe("POST");
      expect(new Headers(init.headers).get("content-type")).toBe(
        "application/json",
      );
      expect(init.body).toBe(
        JSON.stringify({ username: "alice", password: "correct-password" }),
      );
      expect(init.credentials).toBe("include");
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: {
            access_token: "access-token",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(login("alice", "correct-password")).resolves.toEqual({
      accessToken: "access-token",
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/users/login");
  });

  it("保留服务端登录失败信息", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ code: 40101, message: "账号或密码错误" }),
            {
              status: 401,
              headers: { "content-type": "application/json" },
            },
          ),
      ),
    );

    const error = await login("alice", "wrong-password").catch(
      (cause: unknown) => cause,
    );
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 401,
      code: 40101,
      message: "账号或密码错误",
    });
  });

  it("拒绝成功响应中的空 Token", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 0,
              message: "成功",
              data: { access_token: "" },
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    await expect(login("alice", "correct-password")).rejects.toThrow(
      "登录响应缺少有效 Token",
    );
  });
});

describe("registration email verification", () => {
  it("请求验证码只发送邮箱并验证通用 202 响应", async () => {
    const fetchMock = vi.fn(async (url: string, init: RequestInit) => {
      expect(url).toBe("/api/v1/users/registration-email-verifications");
      expect(init.method).toBe("POST");
      expect(init.credentials).toBe("include");
      expect(init.body).toBe(JSON.stringify({ email: "alice@example.com" }));
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: { accepted: true, expires_in: 600, resend_after: 60 },
        }),
        { status: 202, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      requestRegistrationEmail("alice@example.com"),
    ).resolves.toEqual({ expiresIn: 600, resendAfter: 60 });
  });

  it("注册提交邮箱所有权证明并拒绝畸形成功响应", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            code: 0,
            data: {
              id: 42,
              username: "alice",
              email: "alice@example.com",
              email_verified_at: 1_785_000_000,
            },
          }),
          { status: 201, headers: { "content-type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ code: 0, data: { id: 43 } }), {
          status: 201,
          headers: { "content-type": "application/json" },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      register("alice", "correct-password", "alice@example.com", "123456"),
    ).resolves.toEqual({
      id: 42,
      username: "alice",
      email: "alice@example.com",
      emailVerifiedAt: 1_785_000_000,
    });
    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(
      JSON.stringify({
        username: "alice",
        password: "correct-password",
        email: "alice@example.com",
        email_code: "123456",
      }),
    );
    await expect(
      register("bob", "correct-password", "bob@example.com", "654321"),
    ).rejects.toThrow("注册响应缺少已验证账户");
  });
});

describe("refreshSession", () => {
  it("由 HttpOnly Cookie 刷新且请求体不携带 Refresh Token", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.method).toBe("POST");
      expect(init.body).toBe("{}");
      expect(init.credentials).toBe("include");
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: {
            access_token: "access-new",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(refreshSession()).resolves.toEqual({
      accessToken: "access-new",
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/users/refresh");
  });
});

describe("logout", () => {
  it("把合法 428 固化为 StepUp challenge 且不泄露到 details", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 700010,
              message: "需要重认证",
              data: { challenge: "signed-challenge", expires_in: 120 },
            }),
            {
              status: 428,
              headers: {
                "content-type": "application/json",
                "x-request-id": "logout-request",
              },
            },
          ),
      ),
    );

    const error = await logout("access-token").catch((cause: unknown) => cause);
    expect(error).toBeInstanceOf(StepUpRequiredError);
    expect(error).toMatchObject({
      challenge: "signed-challenge",
      expiresIn: 120,
      details: undefined,
    });
  });

  it("proof 只进入本次请求头并验证全量撤销响应", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      const headers = new Headers(init.headers);
      expect(headers.get("authorization")).toBe("Bearer access-token");
      expect(headers.get("x-step-up-proof")).toBe("one-shot-proof");
      expect(init.body).toBe("{}");
      return new Response(
        JSON.stringify({
          code: 0,
          message: "已撤销全部会话",
          data: {
            revoked_all_sessions: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      logout("access-token", undefined, "one-shot-proof"),
    ).resolves.toEqual({ immediateConvergence: true });
    expect(sessionStorage.getItem("yang.step-up-proof")).toBeNull();
  });

  it("拒绝缺少全量撤销语义的畸形成功响应", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ code: 0, data: {} }), {
            status: 200,
            headers: { "content-type": "application/json" },
          }),
      ),
    );

    await expect(logout("access-token")).rejects.toBeInstanceOf(ApiError);
  });
});

describe("disableAccount", () => {
  it("proof 仅进入停用请求头并要求服务端确认账号已停用", async () => {
    const fetchMock = vi.fn(async (url: string, init: RequestInit) => {
      expect(url).toBe("/api/v1/users/disable");
      const headers = new Headers(init.headers);
      expect(headers.get("authorization")).toBe("Bearer access-token");
      expect(headers.get("x-step-up-proof")).toBe("one-shot-proof");
      return new Response(
        JSON.stringify({
          code: 0,
          data: {
            account_disabled: true,
            immediate_convergence: false,
            relogin_required: true,
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      disableAccount("access-token", undefined, "one-shot-proof"),
    ).resolves.toEqual({ immediateConvergence: false });
  });

  it("拒绝未确认账号停用的畸形成功响应", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 0,
              data: {
                revoked_all_sessions: true,
                immediate_convergence: true,
                relogin_required: true,
              },
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    await expect(disableAccount("access-token")).rejects.toBeInstanceOf(
      ApiError,
    );
  });
});

describe("resetPassword", () => {
  it("只提交一次性凭证和新密码，并要求服务端确认重新登录", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.method).toBe("POST");
      expect(init.credentials).toBe("include");
      expect(new Headers(init.headers).get("authorization")).toBeNull();
      expect(init.body).toBe(
        JSON.stringify({
          reset_token: "a".repeat(64),
          new_password: "replacement-password",
        }),
      );
      return new Response(
        JSON.stringify({
          code: 0,
          message: "密码已重置",
          data: { relogin_required: true },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      resetPassword("a".repeat(64), "replacement-password"),
    ).resolves.toBeUndefined();
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/users/reset-password");
  });

  it("拒绝缺少重新登录确认的畸形成功响应", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ code: 0, message: "成功", data: {} }), {
            status: 200,
            headers: { "content-type": "application/json" },
          }),
      ),
    );

    await expect(
      resetPassword("a".repeat(64), "replacement-password"),
    ).rejects.toThrow("密码重置响应缺少重新登录确认");
  });
});
