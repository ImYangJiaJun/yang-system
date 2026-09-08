import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, StepUpRequiredError } from "@/engine/http/errors";
import {
  activateTotp,
  changeEmail,
  changePassword,
  changeUsername,
  deactivateTotp,
  fetchCurrentUser,
  requestChangeEmail,
  setupTotp,
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
              totp_activated: true,
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
      totpActivated: true,
      createdAt: 500,
      updatedAt: 600,
    });
  });

  it("未启用 TOTP 时投影 totpActivated=false", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/users/me")) {
        return Promise.resolve(
          jsonResponse({
            code: 0,
            data: {
              id: 7,
              username: "alice",
              status: "active",
              totp_activated: false,
              created_at: 500,
              updated_at: 600,
            },
          }),
        );
      }
      return Promise.reject(new Error(`未覆盖请求: ${url}`));
    });

    const user = await fetchCurrentUser("tok-1");
    expect(user.totpActivated).toBe(false);
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

describe("requestChangeEmail", () => {
  it("POST change-email-verifications 携带新邮箱", async () => {
    let capturedBody: unknown;
    stubFetch((_url, init) => {
      capturedBody = JSON.parse(String(init.body));
      return Promise.resolve(
        jsonResponse({
          code: 0,
          message: "成功",
          data: { accepted: true, expires_in: 600, resend_after: 60 },
        }),
      );
    });
    const result = await requestChangeEmail("bob@example.com", "tok");
    expect(capturedBody).toEqual({ new_email: "bob@example.com" });
    expect(result).toEqual({ expiresIn: 600, resendAfter: 60 });
  });
});

describe("setupTotp", () => {
  it("POST mfa/totp/setup 并透传 Step-up proof，返回密钥与 URI", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    stubFetch((url, init) => {
      captured = { url, init };
      return Promise.resolve(
        jsonResponse({
          code: 0,
          message: "请在认证器应用中扫描二维码或手动输入密钥",
          data: {
            secret: "JBSWY3DPEHPK3PXP",
            otpauth_uri:
              "otpauth://totp/yang-system:alice?secret=JBSWY3DPEHPK3PXP",
            activated: false,
            digits: 6,
          },
        }),
      );
    });

    const result = await setupTotp("tok", undefined, "proof-x");
    expect(captured?.url).toContain("/api/v1/users/mfa/totp/setup");
    expect(
      (captured?.init.headers as Record<string, string>)["x-step-up-proof"],
    ).toBe("proof-x");
    expect(result).toEqual({
      secret: "JBSWY3DPEHPK3PXP",
      otpauthUri: "otpauth://totp/yang-system:alice?secret=JBSWY3DPEHPK3PXP",
      digits: 6,
    });
  });

  it("缺少密钥的成功响应被拒绝", async () => {
    stubFetch(() =>
      Promise.resolve(jsonResponse({ code: 0, data: { activated: false } })),
    );
    await expect(setupTotp("tok")).rejects.toThrow(/缺少有效密钥/);
  });

  it("服务端未配置 TOTP（404）转为可操作提示", async () => {
    stubFetch(() =>
      Promise.resolve(jsonResponse({ code: 40401, message: "Not Found" }, 404)),
    );
    const error = await setupTotp("tok").catch((cause) => cause);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).message).toContain("服务端未启用双重验证");
  });
});

describe("activateTotp", () => {
  it("POST mfa/totp/activate 提交密钥与验证码，返回一次性恢复码", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    stubFetch((url, init) => {
      captured = { url, init };
      return Promise.resolve(
        jsonResponse({
          code: 0,
          message: "TOTP 已启用，请妥善保存恢复码并重新登录",
          data: {
            totp_activated: true,
            recovery_codes: ["aaaa1111-bbbb2222-cccc3333-dddd4444"],
            immediate_convergence: true,
            relogin_required: true,
          },
        }),
      );
    });

    const result = await activateTotp("JBSWY3DPEHPK3PXP", "123456", "tok");
    expect(captured?.url).toContain("/api/v1/users/mfa/totp/activate");
    expect(JSON.parse(String(captured?.init.body))).toEqual({
      secret: "JBSWY3DPEHPK3PXP",
      code: "123456",
    });
    expect(result).toEqual({
      recoveryCodes: ["aaaa1111-bbbb2222-cccc3333-dddd4444"],
      immediateConvergence: true,
    });
  });

  it("缺少恢复码的激活响应被拒绝", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({
          code: 0,
          data: { totp_activated: true, relogin_required: true },
        }),
      ),
    );
    await expect(
      activateTotp("JBSWY3DPEHPK3PXP", "123456", "tok"),
    ).rejects.toThrow(/缺少恢复码/);
  });
});

describe("deactivateTotp", () => {
  it("POST mfa/totp/deactivate 透传 Step-up proof，返回停用确认", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    stubFetch((url, init) => {
      captured = { url, init };
      return Promise.resolve(
        jsonResponse({
          code: 0,
          message: "双重验证已关闭，请重新登录",
          data: {
            totp_activated: false,
            immediate_convergence: true,
            relogin_required: true,
          },
        }),
      );
    });

    const result = await deactivateTotp("tok", undefined, "proof-x");
    expect(captured?.url).toContain("/api/v1/users/mfa/totp/deactivate");
    expect(JSON.parse(String(captured?.init.body))).toEqual({});
    expect(new Headers(captured?.init.headers).get("x-step-up-proof")).toBe(
      "proof-x",
    );
    expect(result).toEqual({
      reloginRequired: true,
      immediateConvergence: true,
    });
  });

  it("缺少停用确认的成功响应被拒绝", async () => {
    stubFetch(() =>
      Promise.resolve(
        jsonResponse({
          code: 0,
          data: { totp_activated: true, relogin_required: true },
        }),
      ),
    );
    await expect(deactivateTotp("tok")).rejects.toThrow(/缺少确认/);
  });
});

describe("changeEmail", () => {
  it("POST change-email 提交新邮箱与验证码，透传 Step-up proof", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    stubFetch((url, init) => {
      captured = { url, init };
      return Promise.resolve(
        jsonResponse({
          code: 0,
          message: "成功",
          data: {
            email_changed: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        }),
      );
    });
    const result = await changeEmail(
      "bob@example.com",
      "123456",
      "tok",
      undefined,
      "proof-x",
    );
    expect(captured?.url).toContain("/api/v1/users/change-email");
    expect(JSON.parse(String(captured?.init.body))).toEqual({
      new_email: "bob@example.com",
      email_code: "123456",
    });
    expect(
      (captured?.init.headers as Record<string, string>)["x-step-up-proof"],
    ).toBe("proof-x");
    expect(result).toEqual({
      reloginRequired: true,
      immediateConvergence: true,
    });
  });
});
