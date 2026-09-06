import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, StepUpRequiredError } from "@/engine/http/errors";
import {
  changePassword,
  changeUsername,
  fetchCurrentUser,
} from "@/features/account/api";

/// account 账号中心 API 契约：路径、鉴权头、Step-up 428 重放、响应校验。

function jsonResponse(
  payload: unknown,
  status = 200,
  headers: Record<string, string> = {},
) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function stubFetch(
  handler: (url: string, init: RequestInit) => Promise<Response>,
) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      return handler(url, init ?? {});
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchCurrentUser", () => {
  it("携带 Bearer token 读取 /users/me 并投影为 CurrentUser", async () => {
    let capturedToken: string | undefined;
    stubFetch((url, init) => {
      capturedToken = (init.headers as Record<string, string>).Authorization;
      if (url.endsWith("/api/v1/users/me")) {
        return Promise.resolve(
          jsonResponse({
            code: 0,
            data: {
              id: 7,
              username: "alice",
              email: "alice@example.com",
              email_verified_at: 1000,
              status: "active",
              created_at: 500,
              updated_at: 600,
            },
          }),
        );
      }
      return Promise.reject(new Error(`未覆盖请求: ${url}`));
    });

    const user = await fetchCurrentUser("tok-1");
    expect(capturedToken).toBe("Bearer tok-1");
    expect(user).toEqual({
      id: 7,
      username: "alice",
      email: "alice@example.com",
      emailVerifiedAt: 1000,
      status: "active",
      createdAt: 500,
      updatedAt: 600,
    });
  });

  it("缺少用户名时拒绝响应", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/users/me")) {
        return Promise.resolve(jsonResponse({ code: 0, data: { id: 7 } }));
      }
      return Promise.reject(new Error(`未覆盖请求: ${url}`));
    });
    await expect(fetchCurrentUser("tok")).rejects.toThrow(/缺少有效资料/);
  });
});

describe("changePassword", () => {
  it("POST change-password 并透传 x-step-up-proof", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    stubFetch((url, init) => {
      captured = { url, init };
      return Promise.resolve(
        jsonResponse({
          code: 0,
          data: { relogin_required: true, immediate_convergence: true },
        }),
      );
    });

    const result = await changePassword(
      "old",
      "new-password-1",
      "tok",
      undefined,
      "proof-x",
    );
    expect(captured?.url).toContain("/api/v1/users/change-password");
    expect(JSON.parse(String(captured?.init.body))).toEqual({
      old_password: "old",
      new_password: "new-password-1",
    });
    expect(
      (captured?.init.headers as Record<string, string>)["x-step-up-proof"],
    ).toBe("proof-x");
    expect(result).toEqual({
      reloginRequired: true,
      immediateConvergence: true,
    });
  });

  it("428 响应抛 StepUpRequiredError（携带 challenge）", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse(
          {
            code: 700010,
            message: "敏感操作需要重新认证",
            data: { challenge: "signed-challenge", expires_in: 120 },
          },
          428,
        ),
      ),
    );
    const error = await changePassword("old", "new-password-1", "tok").catch(
      (cause) => cause,
    );
    expect(error).toBeInstanceOf(StepUpRequiredError);
    expect((error as StepUpRequiredError).challenge).toBe("signed-challenge");
    expect((error as StepUpRequiredError).expiresIn).toBe(120);
  });

  it("非 428 错误抛 ApiError", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({ code: 400001, message: "当前密码错误" }, 400),
      ),
    );
    const error = await changePassword("old", "new-password-1", "tok").catch(
      (cause) => cause,
    );
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).message).toContain("当前密码错误");
  });
});

describe("changeUsername", () => {
  it("POST change-username 提交新用户名", async () => {
    let capturedBody: unknown;
    stubFetch((_url, init) => {
      capturedBody = JSON.parse(String(init.body));
      return Promise.resolve(
        jsonResponse({
          code: 0,
          data: {
            username_changed: true,
            immediate_convergence: false,
            relogin_required: true,
          },
        }),
      );
    });
    const result = await changeUsername("alice2", "tok");
    expect(capturedBody).toEqual({ new_username: "alice2" });
    expect(result).toEqual({
      reloginRequired: true,
      immediateConvergence: false,
    });
  });
});
