import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

/// 注册页：用户名 + 密码（本地校验 / 提交契约 / 成功跳转）。
/// 邮箱验证码那条流程已经不在页面上（注册改成用户名密码），对应用例随之删掉。

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
  it("注册成功跳 /login?registered=1 并展示提示文案", async () => {
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/refresh")) {
          return jsonResponse({ code: 40102, message: "无会话" }, 401);
        }
        expect(url).toBe("/api/v1/users/register");
        // 帐号按 trim 后提交（首尾空格不属于帐号），密码原样。
        expect(JSON.parse(String(init?.body))).toEqual({
          username: "alice",
          password: "correct-password",
        });
        return jsonResponse(
          { code: 0, message: "成功", data: { id: 42, username: "alice" } },
          201,
        );
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    const { router } = renderRegister();

    await screen.findByRole("heading", { name: "创建账号" });
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "  alice  ");
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
