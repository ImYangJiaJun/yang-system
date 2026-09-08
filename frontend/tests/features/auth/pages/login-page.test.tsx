import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createSessionController } from "@/engine/session/session-controller";
import { clearStoredSession } from "@/engine/session/auth-session";
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

  it("启用 TOTP 的账号：密码通过后弹出双重验证框，满 6 位自动提交完成登录", async () => {
    stubRefreshFailure();
    const { controller } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    let loginCalls = 0;
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/login")) {
          loginCalls += 1;
          const body = JSON.parse(String(init?.body));
          if (loginCalls === 1) {
            // 第一阶段：只发账号密码；密码正确但未带码 → 700012。
            expect(body).toEqual({
              username: "alice",
              password: "correct-password",
            });
            return jsonResponse(
              { code: 700012, message: "需要输入双重验证码" },
              401,
            );
          }
          // 第二阶段：带原凭据 + extra.mfa_code 重放。
          expect(body).toEqual({
            username: "alice",
            password: "correct-password",
            extra: { mfa_code: "123456" },
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

    // 第一阶段通过后弹出双重验证对话框。
    const dialog = await screen.findByRole("dialog");
    const codeInput = within(dialog).getByLabelText("双重验证码");
    // 满 6 位自动提交，无需点击按钮。
    await user.type(codeInput, "123456");

    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "access-token",
        loggedIn: true,
      });
    });
    expect(loginCalls).toBe(2);
  });

  it("双重验证码错误：对话框内提示并清空输入，可重试", async () => {
    stubRefreshFailure();
    const { controller } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    let loginCalls = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/v1/users/login")) {
        loginCalls += 1;
        if (loginCalls === 1) {
          return jsonResponse(
            { code: 700012, message: "需要输入双重验证码" },
            401,
          );
        }
        if (loginCalls === 2) {
          return jsonResponse(
            {
              code: 700005,
              message: "参数无效 [mfa_code]: 双重验证码错误或已过期",
            },
            400,
          );
        }
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
    });
    vi.stubGlobal("fetch", fetchMock);

    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "登录" }));

    const dialog = await screen.findByRole("dialog");
    const codeInput = within(dialog).getByLabelText("双重验证码");
    await user.type(codeInput, "000000");

    // 错误提示出现在对话框内，输入框被清空。
    expect(
      await within(dialog).findByText(/双重验证码错误或已过期/),
    ).toBeInTheDocument();
    expect(codeInput).toHaveValue("");
    expect(controller.getSnapshot().loggedIn).toBe(false);

    // 重试正确验证码 → 登录成功。
    await user.type(codeInput, "123456");
    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(true);
    });
  });

  it("认证器不可用时可改用邮箱验证码：发码 → 满 6 位自动提交完成登录", async () => {
    stubRefreshFailure();
    const { controller } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    let loginCalls = 0;
    let emailCodeRequests = 0;
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/users/mfa/email-code")) {
          emailCodeRequests += 1;
          // 发码请求必须携带登录表单的账号密码（服务端等时重验防枚举）。
          expect(JSON.parse(String(init?.body))).toEqual({
            username: "alice",
            password: "correct-password",
          });
          return jsonResponse(
            {
              code: 0,
              message: "成功",
              data: { accepted: true, expires_in: 600, resend_after: 60 },
            },
            202,
          );
        }
        if (url.includes("/api/v1/users/login")) {
          loginCalls += 1;
          const body = JSON.parse(String(init?.body));
          if (loginCalls === 1) {
            return jsonResponse(
              { code: 700012, message: "需要输入双重验证码" },
              401,
            );
          }
          // 第二阶段：邮箱验证码作为 mfa_code 重放。
          expect(body).toEqual({
            username: "alice",
            password: "correct-password",
            extra: { mfa_code: "654321" },
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

    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("button", { name: /使用邮箱验证码/ }),
    );

    // 发码成功：提示已发送至注册邮箱，入口进入冷却倒计时。
    expect(
      await within(dialog).findByText(/验证码已发送至您的注册邮箱/),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: /重新发送邮箱验证码/ }),
    ).toBeDisabled();
    expect(emailCodeRequests).toBe(1);

    // 输入邮箱验证码满 6 位自动提交登录。
    await user.type(within(dialog).getByLabelText("双重验证码"), "654321");
    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "access-token",
        loggedIn: true,
      });
    });
  });

  it("邮箱验证码发码失败：对话框内提示，不消耗登录重试", async () => {
    stubRefreshFailure();
    const { controller } = renderLogin();
    await screen.findByRole("heading", { name: "用户登录" });

    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/v1/users/mfa/email-code")) {
        return jsonResponse(
          { code: 42901, message: "请求过于频繁，请 60 秒后重试" },
          429,
        );
      }
      if (url.includes("/api/v1/users/login")) {
        return jsonResponse(
          { code: 700012, message: "需要输入双重验证码" },
          401,
        );
      }
      if (url.includes("/.well-known/yang/ui-catalog")) {
        return jsonResponse(catalogFixture);
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const user = userEvent.setup();
    await user.type(screen.getByLabelText("帐号"), "alice");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "登录" }));

    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("button", { name: /使用邮箱验证码/ }),
    );

    expect(await within(dialog).findByText(/请求过于频繁/)).toBeInTheDocument();
    expect(controller.getSnapshot().loggedIn).toBe(false);
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
