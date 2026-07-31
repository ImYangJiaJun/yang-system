import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./errors";
import { login, refreshSession, resetPassword } from "./auth";

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
