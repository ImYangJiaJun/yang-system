import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

/// 注册页：邮箱验证码流程（冷却/提交契约/成功跳转）。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderRegister() {
  return renderTestApp({ path: "/register", authenticated: false });
}

describe("RegisterPage", () => {
  it("发送验证码只提交归一化邮箱，成功后开始重发冷却", async () => {
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/refresh")) {
          return jsonResponse({ code: 40102, message: "无会话" }, 401);
        }
        expect(url).toBe("/api/v1/users/registration-email-verifications");
        expect(JSON.parse(String(init?.body))).toEqual({
          email: "alice@example.com",
        });
        return jsonResponse(
          {
            code: 0,
            message: "成功",
            data: { accepted: true, expires_in: 600, resend_after: 60 },
          },
          202,
        );
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    renderRegister();

    await screen.findByRole("heading", { name: "创建账号" });
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("邮箱"), "  Alice@Example.COM ");
    await user.click(screen.getByRole("button", { name: "发送验证码" }));

    expect(
      await screen.findByText(/验证码将在 10 分钟内送达/),
    ).toBeInTheDocument();
    // 冷却开始：按钮进入倒计时禁用态。
    expect(
      screen.getByRole("button", { name: /重新发送（60s）/ }),
    ).toBeDisabled();
    // 邮箱被归一化回写。
    expect(screen.getByLabelText("邮箱")).toHaveValue("alice@example.com");
  });

  it("注册成功跳 /login?registered=1 并展示旧版提示文案", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/v1/users/refresh")) {
        return jsonResponse({ code: 40102, message: "无会话" }, 401);
      }
      if (url.includes("/api/v1/users/register")) {
        return jsonResponse(
          {
            code: 0,
            data: {
              id: 42,
              username: "alice",
              email: "alice@example.com",
              email_verified_at: 1_785_000_000,
            },
          },
          201,
        );
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const { router } = renderRegister();

    await screen.findByRole("heading", { name: "创建账号" });
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("邮箱"), "alice@example.com");
    await user.type(screen.getByLabelText("邮箱验证码"), "123456");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.type(screen.getByLabelText("确认密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "创建账号" }));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/login");
      expect(router.state.location.search).toContain("registered=1");
    });
    expect(await screen.findByText("账号已创建，请登录")).toBeInTheDocument();
  });

  it("本地校验拦截：密码不一致不发请求", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/v1/users/refresh")) {
        return jsonResponse({ code: 40102, message: "无会话" }, 401);
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    renderRegister();

    await screen.findByRole("heading", { name: "创建账号" });
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("邮箱验证码"), "123456");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.type(screen.getByLabelText("确认密码"), "different-password");
    await user.click(screen.getByRole("button", { name: "创建账号" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "两次输入的密码不一致",
    );
    const calls = fetchMock.mock.calls.filter(([input]) =>
      String(input).includes("/api/v1/users/register"),
    );
    expect(calls).toHaveLength(0);
  });
});
