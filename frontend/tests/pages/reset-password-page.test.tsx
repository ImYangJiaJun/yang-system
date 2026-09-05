import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/api/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

/// 重置密码页：query token 预填 + 成功后清空会话 + 跳登录页。

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

describe("ResetPasswordPage", () => {
  it("从链接 query 预填重置凭证", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse({ code: 40102, message: "无会话" }, 401)),
    );
    renderTestApp({
      path: "/reset-password?token=abc123",
      authenticated: false,
    });

    await screen.findByRole("heading", { name: "重置密码" });
    expect(screen.getByLabelText("重置凭证")).toHaveValue("abc123");
  });

  it("成功重置后清空会话、广播凭据变更并跳登录页", async () => {
    const token = "a".repeat(64);
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/refresh")) {
          return jsonResponse({ code: 40102, message: "无会话" }, 401);
        }
        expect(url).toBe("/api/v1/users/reset-password");
        expect(JSON.parse(String(init?.body))).toEqual({
          reset_token: token,
          new_password: "replacement-password",
        });
        return jsonResponse({
          code: 0,
          message: "密码已重置",
          data: { relogin_required: true },
        });
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    const { controller, router } = renderTestApp({
      path: `/reset-password?token=${token}`,
      authenticated: false,
    });

    await screen.findByRole("heading", { name: "重置密码" });
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("新密码"), "replacement-password");
    await user.type(
      screen.getByLabelText("确认新密码"),
      "replacement-password",
    );
    await user.click(screen.getByRole("button", { name: "重置密码" }));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/login");
    });
    expect(controller.getSnapshot()).toMatchObject({
      loggedIn: false,
      sessionEndReason: "credentials-changed",
    });
    // 登录页展示凭据变更提示。
    expect(
      await screen.findByText("凭据已变更，请使用新密码重新登录"),
    ).toBeInTheDocument();
  });
});
