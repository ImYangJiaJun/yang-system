import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { renderTestApp } from "@test/helpers/render-app";

/// 飞书登录的两个公开页面：账号选择页（绑定/新建）与第二因子页。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function choosePath() {
  return "/login/feishu/choose?token=pending-token&redirect=%2Fbatch-approval";
}

const PROFILE = {
  name: "张三",
  email_masked: "z***@corp.com",
  email_matches_existing: true,
  allow_create: true,
  redirect: "/batch-approval",
};

/// 登录成功后会跳到 /batch-approval；把该页自己的两个请求也桩掉，避免它们
/// 变成「未覆盖请求」污染断言。
function batchPagePayload(url: string): Response | null {
  if (url.includes("/api/v1/feishu/batch-approval/tasks")) {
    return jsonResponse({
      code: 0,
      message: "成功",
      data: { tasks: [], truncated: false },
    });
  }
  if (url.endsWith("/api/v1/users/me")) {
    return jsonResponse({
      code: 0,
      message: "成功",
      data: { username: "alice" },
    });
  }
  return null;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("FeishuChoosePage", () => {
  it("展示飞书姓名/脱敏邮箱/邮箱命中提示，绑定已有账号后建立会话并回跳", async () => {
    const bindBodies: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/choose?")) {
          return jsonResponse({ code: 0, message: "成功", data: PROFILE });
        }
        if (url.includes("/api/v1/auth/feishu/choose/bind")) {
          bindBodies.push(JSON.parse(String(init?.body)));
          return jsonResponse({
            code: 0,
            message: "成功",
            data: {
              access_token: "feishu-access",
              redirect: "/batch-approval",
            },
          });
        }
        const batch = batchPagePayload(url);
        if (batch) return batch;
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    const controller = createSessionController();
    const { router } = renderTestApp({
      path: choosePath(),
      authenticated: false,
      controller,
    });

    expect(await screen.findByText("张三")).toBeInTheDocument();
    expect(screen.getByText("z***@corp.com")).toBeInTheDocument();
    expect(screen.getByText(/检测到该邮箱已对应系统账号/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.type(
      screen.getByLabelText("用户名或已验证邮箱"),
      "alice@example.com",
    );
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "绑定并登录" }));

    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "feishu-access",
        loggedIn: true,
      });
    });
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/batch-approval"),
    );
    expect(bindBodies).toEqual([
      {
        token: "pending-token",
        username: "alice@example.com",
        password: "correct-password",
      },
    ]);
  });

  it("绑定遇到 700012 时弹出第二因子，带 mfa_code 重试同一次绑定", async () => {
    const bindBodies: unknown[] = [];
    let attempts = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/choose?")) {
          return jsonResponse({ code: 0, message: "成功", data: PROFILE });
        }
        if (url.includes("/api/v1/auth/feishu/choose/bind")) {
          attempts += 1;
          bindBodies.push(JSON.parse(String(init?.body)));
          if (attempts === 1) {
            return jsonResponse(
              { code: 700012, message: "需要输入双重验证码" },
              401,
            );
          }
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { access_token: "feishu-access", redirect: "/" },
          });
        }
        const batch = batchPagePayload(url);
        if (batch) return batch;
        // redirect "/" 会进入 AppLayout 并拉目录；给一个空目录让它落定。
        if (url.includes("/.well-known/yang/ui-catalog")) {
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
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    const controller = createSessionController();
    renderTestApp({
      path: choosePath(),
      authenticated: false,
      controller,
    });

    const user = userEvent.setup();
    await screen.findByText("张三");
    await user.type(screen.getByLabelText("用户名或已验证邮箱"), "alice");
    await user.type(screen.getByLabelText("密码"), "correct-password");
    await user.click(screen.getByRole("button", { name: "绑定并登录" }));

    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("双重验证码"), "123456");

    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(true);
    });
    expect(bindBodies).toEqual([
      {
        token: "pending-token",
        username: "alice",
        password: "correct-password",
      },
      {
        token: "pending-token",
        username: "alice",
        password: "correct-password",
        mfa_code: "123456",
      },
    ]);
  });

  it("新建账号需要二次确认，成功后建立会话", async () => {
    const createBodies: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/choose?")) {
          return jsonResponse({ code: 0, message: "成功", data: PROFILE });
        }
        if (url.includes("/api/v1/auth/feishu/choose/create")) {
          createBodies.push(JSON.parse(String(init?.body)));
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { access_token: "new-access", redirect: "/batch-approval" },
          });
        }
        const batch = batchPagePayload(url);
        if (batch) return batch;
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);

    const controller = createSessionController();
    const { router } = renderTestApp({
      path: choosePath(),
      authenticated: false,
      controller,
    });

    const user = userEvent.setup();
    await screen.findByText("张三");
    await user.click(screen.getByRole("button", { name: "新建账号" }));
    await user.type(screen.getByLabelText("用户名"), "new-user");
    await user.type(screen.getByLabelText("密码"), "new-password-1");
    await user.type(screen.getByLabelText("确认密码"), "new-password-1");
    await user.click(screen.getByRole("button", { name: "新建并登录" }));

    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "new-access",
        loggedIn: true,
      });
    });
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(createBodies).toEqual([
      {
        token: "pending-token",
        username: "new-user",
        password: "new-password-1",
      },
    ]);
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/batch-approval"),
    );
  });

  it("allow_create=false 时不渲染新建账号入口", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/choose?")) {
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { ...PROFILE, allow_create: false },
          });
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    renderTestApp({
      path: choosePath(),
      authenticated: false,
      controller: createSessionController(),
    });

    expect(await screen.findByText("张三")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "新建账号" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/当前部署不允许自助新建账号/)).toBeInTheDocument();
  });

  it("pending token 失效时显示可行动的重新登录入口", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/choose?")) {
          return jsonResponse(
            { code: 41001, message: "登录已过期，请重新点飞书登录" },
            410,
          );
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    renderTestApp({
      path: choosePath(),
      authenticated: false,
      controller: createSessionController(),
    });

    expect(await screen.findByRole("alert")).toHaveTextContent("登录已过期");
    expect(screen.getByRole("link", { name: "重新登录" })).toBeInTheDocument();
  });
});

describe("FeishuMfaPage", () => {
  it("提交 token + mfa_code 后建立会话并回跳", async () => {
    let body: unknown;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/mfa")) {
          body = JSON.parse(String(init?.body));
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { access_token: "mfa-access", redirect: "/batch-approval" },
          });
        }
        const batch = batchPagePayload(url);
        if (batch) return batch;
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    const controller = createSessionController();
    const { router } = renderTestApp({
      path: "/login/feishu/mfa?token=pending-login&redirect=%2Fbatch-approval",
      authenticated: false,
      controller,
    });

    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("双重验证码"), "123456");
    await user.click(screen.getByRole("button", { name: "验证并登录" }));

    await waitFor(() => {
      expect(controller.getSnapshot()).toMatchObject({
        token: "mfa-access",
        loggedIn: true,
      });
    });
    expect(body).toEqual({ token: "pending-login", mfa_code: "123456" });
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/batch-approval"),
    );
  });

  it("验证失败时留在页面并显示错误", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/api/v1/auth/feishu/mfa")) {
          return jsonResponse(
            { code: 700005, message: "一次性验证码无效" },
            400,
          );
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );

    const controller = createSessionController();
    renderTestApp({
      path: "/login/feishu/mfa?token=pending-login",
      authenticated: false,
      controller,
    });

    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("双重验证码"), "000000");
    await user.click(screen.getByRole("button", { name: "验证并登录" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "一次性验证码无效",
    );
    expect(controller.getSnapshot().loggedIn).toBe(false);
  });

  it("缺少 token 时不发请求并提示重新登录", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    renderTestApp({
      path: "/login/feishu/mfa",
      authenticated: false,
      controller: createSessionController(),
    });

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "登录链接不完整或已过期",
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
