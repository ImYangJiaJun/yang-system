import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { renderTestApp } from "@test/helpers/render-app";

/// 账号设置页：资料加载、修改密码/用户名表单、Step-up 428 重放、停用账号入口。

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

function mePayload() {
  return {
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
  };
}

/// 默认 stub：me 成功；change-password/change-username 成功（relogin_required）。
function stubAccountApi(
  options: {
    changePassword428?: boolean;
    changeUsername428?: boolean;
  } = {},
) {
  const calls: Array<{ url: string; proof?: string }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const proof = (init?.headers as Record<string, string> | undefined)?.[
        "x-step-up-proof"
      ];
      calls.push({ url, proof });
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: {
            schema_version: "2.3",
            revision: "a".repeat(64),
            actions: [],
            table_views: [],
            modules: [],
          },
        });
      }
      if (url.endsWith("/api/v1/users/me")) {
        return jsonResponse(mePayload());
      }
      if (url.endsWith("/api/v1/users/change-password")) {
        if (options.changePassword428 && !proof) {
          return jsonResponse(
            {
              code: 700010,
              message: "敏感操作需要重新认证",
              data: { challenge: "challenge-1", expires_in: 120 },
            },
            428,
          );
        }
        return jsonResponse({
          code: 0,
          data: { relogin_required: true, immediate_convergence: true },
        });
      }
      if (url.endsWith("/api/v1/users/change-username")) {
        if (options.changeUsername428 && !proof) {
          return jsonResponse(
            {
              code: 700010,
              message: "敏感操作需要重新认证",
              data: { challenge: "challenge-2", expires_in: 120 },
            },
            428,
          );
        }
        return jsonResponse({
          code: 0,
          data: {
            username_changed: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      if (url.endsWith("/api/v1/users/step-up/complete")) {
        return jsonResponse({
          code: 0,
          data: { proof: "proof-ok", expires_in: 300 },
        });
      }
      if (
        url.endsWith("/api/v1/users/logout") ||
        url.endsWith("/api/v1/users/disable")
      ) {
        return jsonResponse({
          code: 0,
          data: {
            account_disabled: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("账号设置页", () => {
  it("加载并展示当前用户资料", async () => {
    stubAccountApi();
    renderTestApp({ path: "/account", authenticated: true });

    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    expect(screen.getByText("alice@example.com（已验证）")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "账号设置" }),
    ).toBeInTheDocument();
  });

  it("修改密码成功清空会话并跳转登录页", async () => {
    stubAccountApi();
    const { controller } = renderTestApp({
      path: "/account",
      authenticated: true,
    });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    await user.type(screen.getByLabelText("当前密码"), "old-pass");
    await user.type(
      screen.getByLabelText("新密码（至少 10 位）"),
      "new-password-1",
    );
    await user.type(screen.getByLabelText("确认新密码"), "new-password-1");
    await user.click(screen.getByRole("button", { name: "修改密码" }));

    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(false);
    });
  });

  it("修改密码 428 时经 requestStepUpProof 换 proof 后重放成功", async () => {
    stubAccountApi({ changePassword428: true });
    const requestStepUpProof = vi.fn(
      async () => "proof-ok" as string | undefined,
    );
    const controller = createSessionController({ requestStepUpProof });
    controller.beginSession({ accessToken: "test-access" });
    renderTestApp({ path: "/account", authenticated: true, controller });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    await user.type(screen.getByLabelText("当前密码"), "old-pass");
    await user.type(
      screen.getByLabelText("新密码（至少 10 位）"),
      "new-password-1",
    );
    await user.type(screen.getByLabelText("确认新密码"), "new-password-1");
    await user.click(screen.getByRole("button", { name: "修改密码" }));

    await waitFor(() => {
      expect(requestStepUpProof).toHaveBeenCalledWith("challenge-1", {
        token: "test-access",
      });
    });
    // 重放后凭据变更 → 会话被清空。
    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(false);
    });
  });

  it("修改用户名提交成功并清空会话", async () => {
    stubAccountApi();
    renderTestApp({ path: "/account", authenticated: true });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    const usernameInput = screen.getByLabelText("新用户名");
    await user.clear(usernameInput);
    await user.type(usernameInput, "alice2");
    await user.click(screen.getByRole("button", { name: "修改用户名" }));

    await waitFor(() => {
      expect(
        screen.queryByRole("button", { name: "修改用户名" }),
      ).not.toBeInTheDocument();
    });
  });

  it("停用账号入口经 SessionController.disableAccount 生效", async () => {
    stubAccountApi();
    const { controller } = renderTestApp({
      path: "/account",
      authenticated: true,
    });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await user.click(screen.getByRole("button", { name: "停用账号" }));

    await waitFor(() => expect(controller.getSnapshot().loggedIn).toBe(false));
  });
});
