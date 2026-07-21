import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./errors";
import { login, refreshSession } from "./auth";

afterEach(() => vi.unstubAllGlobals());

describe("login", () => {
  it("只发送用户名和密码并返回 Token 对", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.method).toBe("POST");
      expect(new Headers(init.headers).get("content-type")).toBe(
        "application/json",
      );
      expect(init.body).toBe(
        JSON.stringify({ username: "alice", password: "correct-password" }),
      );
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: {
            access_token: "access-token",
            refresh_token: "refresh-token",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(login("alice", "correct-password")).resolves.toEqual({
      accessToken: "access-token",
      refreshToken: "refresh-token",
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
              data: { access_token: "", refresh_token: "refresh-token" },
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
  it("发送 Refresh Token 并返回轮换后的 Token 对", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.method).toBe("POST");
      expect(init.body).toBe(JSON.stringify({ refresh_token: "refresh-old" }));
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: {
            access_token: "access-new",
            refresh_token: "refresh-new",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(refreshSession("refresh-old")).resolves.toEqual({
      accessToken: "access-new",
      refreshToken: "refresh-new",
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/users/refresh");
  });
});
