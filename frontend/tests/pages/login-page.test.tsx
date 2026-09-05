import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createSessionController } from "@/api/session-controller";
import { clearStoredSession } from "@/api/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import catalogFixture from "@test/fixtures/ui-catalog.json";

/// 登录链路测试：LoginPage 提交 → SessionController.beginSession → 跳转 /。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function renderLogin() {
  const controller = createSessionController();
  const { router } = renderTestApp({
    path: "/login",
    authenticated: false,
    controller,
  });
  return { controller, router };
}

/// 会话恢复（refresh）失败 → anonymous，登录页可交互。
function stubRefreshFailure() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/v1/users/refresh")) {
        return jsonResponse(
          { code: 40102, message: "刷新会话 Cookie 缺失" },
          401,
        );
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  // 清空 auth-session 模块级内存 Token，避免用例间串扰。
  clearStoredSession();
});

describe("LoginPage", () => {
  it("提交只发送 username/password，成功后写入会话并跳转到 /", async () => {
    stubRefreshFailure();
    const { controller, router } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/login")) {
          expect(init?.method).toBe("POST");
          expect(JSON.parse(String(init?.body))).toEqual({
            username: "alice",
            password: "correct-password",
          });
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { access_token: "access-token" },
          });
        }
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(catalogFixture);
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { items: [], page: 1, page_size: 10, total: 0 },
          });
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      },
    );
    vi.stubGlobal("fetch", fetchMock);

    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "登录" }));

    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "access-token",
        restoreState: "authenticated",
        loggedIn: true,
      });
    });
    await waitFor(() => {
      expect(router.state.location.pathname).not.toBe("/login");
    });
    // 跳转后认证区接管：导航出现演示模块。
    expect(
      await screen.findByRole("link", { name: "项目目录" }),
    ).toBeInTheDocument();
  });

  it("后端 401 错误码映射为登录错误信息，会话保持匿名", async () => {
    stubRefreshFailure();
    const { controller } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 40101, message: "账号或密码错误" }, 401),
      ),
    );

    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("密码"), "wrong-password");
    await user.click(screen.getByRole("button", { name: "登录" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "账号或密码错误",
    );
    expect(controller.getSnapshot().loggedIn).toBe(false);
  });

  it("空帐号/空密码在本地拦截，不发起请求", async () => {
    stubRefreshFailure();
    renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "登录" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("请输入帐号");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
