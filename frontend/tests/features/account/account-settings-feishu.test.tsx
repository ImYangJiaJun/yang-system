import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

/// 账号设置页的「飞书账号」区块：展示绑定、绑定/重新授权、解绑与 428 提示。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/// 后端 `feishu_identities` 的真实形状：裸数组元素，字段是 name/email_masked。
const BOUND_IDENTITY = {
  provider: "feishu",
  name: "张三",
  email_masked: "z***@corp.com",
  bound_at: 1_700_000_000,
  last_login_at: 1_700_000_100,
};

type StubOptions = {
  enabled?: boolean;
  identities?: unknown[];
  unbindStatus?: number;
  unbindBody?: unknown;
  onUnbind?: () => void;
};

type Call = { method: string; path: string; body: unknown };

function stubAccountApi(options: StubOptions = {}): Call[] {
  const calls: Call[] = [];
  let identities = options.identities ?? [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const raw = typeof input === "string" ? input : input.toString();
      const url = new URL(raw, "http://localhost");
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      calls.push({ method, path: url.pathname, body });

      if (url.pathname === "/.well-known/yang/ui-catalog") {
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
      if (url.pathname === "/api/v1/users/me") {
        return jsonResponse({
          code: 0,
          data: {
            id: 7,
            username: "alice",
            email: null,
            email_verified_at: null,
            status: "active",
            totp_activated: false,
            created_at: 500,
            updated_at: 600,
          },
        });
      }
      if (url.pathname === "/api/v1/users/sessions") {
        return jsonResponse({ code: 0, data: { sessions: [] } });
      }
      if (url.pathname === "/api/v1/users/security-events") {
        return jsonResponse({ code: 0, data: { events: [] } });
      }
      if (url.pathname === "/api/v1/auth/feishu/status") {
        return jsonResponse({
          code: 0,
          data: { enabled: options.enabled ?? true },
        });
      }
      if (url.pathname === "/api/v1/users/me/identities" && method === "GET") {
        return jsonResponse({ code: 0, data: identities });
      }
      if (
        url.pathname === "/api/v1/users/me/identities/feishu/authorize" &&
        method === "POST"
      ) {
        return jsonResponse({
          code: 0,
          data: {
            authorize_url:
              "https://accounts.feishu.cn/open-apis/authen/v1/authorize?x=1",
          },
        });
      }
      if (
        url.pathname === "/api/v1/users/me/identities/feishu" &&
        method === "DELETE"
      ) {
        options.onUnbind?.();
        if (options.unbindStatus && options.unbindStatus !== 200) {
          return jsonResponse(
            options.unbindBody ?? {
              code: 42801,
              message: "需要二次身份验证",
            },
            options.unbindStatus,
          );
        }
        identities = [];
        return jsonResponse({ code: 0, data: { unbound: true } });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url.pathname}`);
    }),
  );
  return calls;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("账号设置 · 飞书账号", () => {
  it("展示已绑定姓名/邮箱/时间与重新授权、解绑入口", async () => {
    stubAccountApi({ identities: [BOUND_IDENTITY] });
    renderTestApp({ path: "/account", authenticated: true });

    const region = await screen.findByRole("region", { name: "飞书账号" });
    expect(await within(region).findByText("张三")).toBeInTheDocument();
    expect(within(region).getByText("z***@corp.com")).toBeInTheDocument();
    expect(
      await within(region).findByRole("button", { name: "重新授权飞书" }),
    ).toBeInTheDocument();
    expect(
      within(region).getByRole("button", { name: "解绑飞书账号" }),
    ).toBeInTheDocument();
  });

  it("绑定飞书：先取 authorize_url，再交给浏览器整页跳转", async () => {
    const calls = stubAccountApi({ identities: [] });
    // jsdom 不会真的导航；`window.location.assign` 是只读属性，无法 spy。
    // 这里验请求与「未落在页面错误态」，整页跳转本身由生产代码直接调用。
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.spyOn(window, "prompt").mockReturnValue("secret");
    renderTestApp({ path: "/account", authenticated: true });

    const region = await screen.findByRole("region", { name: "飞书账号" });
    await userEvent.click(
      await within(region).findByRole("button", { name: "绑定飞书账号" }),
    );

    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.method === "POST" &&
            call.path === "/api/v1/users/me/identities/feishu/authorize" &&
            (call.body as { password?: string }).password === "secret",
        ),
      ).toBe(true),
    );
    expect(within(region).queryByRole("alert")).not.toBeInTheDocument();
    consoleError.mockRestore();
  });

  it("登录/绑定总开关关闭且未绑定时不渲染区块", async () => {
    stubAccountApi({ enabled: false, identities: [] });
    renderTestApp({ path: "/account", authenticated: true });

    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    // 区块在读取期间会短暂出现；加载完成后，开关关闭且未绑定时必须消失。
    await waitFor(() =>
      expect(
        screen.queryByRole("region", { name: "飞书账号" }),
      ).not.toBeInTheDocument(),
    );
  });

  it("解绑：二次确认后调用 DELETE 并刷新为未绑定", async () => {
    const calls = stubAccountApi({ identities: [BOUND_IDENTITY] });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.spyOn(window, "prompt").mockReturnValue("secret");
    renderTestApp({ path: "/account", authenticated: true });

    const region = await screen.findByRole("region", { name: "飞书账号" });
    await userEvent.click(
      await within(region).findByRole("button", { name: "解绑飞书账号" }),
    );

    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.method === "DELETE" &&
            call.path === "/api/v1/users/me/identities/feishu",
        ),
      ).toBe(true),
    );
    expect(await screen.findByText("尚未绑定飞书账号。")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "绑定飞书账号" }),
    ).toBeInTheDocument();
  });

  it("解绑遇到 428 时显示可行动的二次验证提示", async () => {
    stubAccountApi({
      identities: [BOUND_IDENTITY],
      unbindStatus: 428,
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.spyOn(window, "prompt").mockReturnValue("secret");
    renderTestApp({ path: "/account", authenticated: true });

    const region = await screen.findByRole("region", { name: "飞书账号" });
    await userEvent.click(
      await within(region).findByRole("button", { name: "解绑飞书账号" }),
    );

    expect(await within(region).findByRole("alert")).toHaveTextContent(
      "二次身份验证",
    );
  });

  it("OAuth 回调 ?feishu=bound 只提示一次并清掉参数", async () => {
    stubAccountApi({ identities: [BOUND_IDENTITY] });
    const { router } = renderTestApp({
      path: "/account?feishu=bound",
      authenticated: true,
    });

    expect(await screen.findByText("飞书账号已绑定")).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.search).toBe(""));
  });
});
