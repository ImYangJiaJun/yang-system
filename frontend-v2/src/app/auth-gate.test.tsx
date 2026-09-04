import { screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/api/auth-session";
import { createSessionController } from "@/api/session-controller";
import { renderTestApp } from "@/test/render-app";

import catalogFixture from "@/test/fixtures/ui-catalog.json";

/// 路由门控测试：pending 骨架、anonymous → /login、authenticated 访问 /login → /。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function renderAt(path: string, controller = createSessionController()) {
  const authenticated = controller.getSnapshot().loggedIn;
  return renderTestApp({ path, authenticated, controller });
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("认证门控", () => {
  it("restoreState 为 pending 时渲染全屏骨架，恢复失败后离开骨架", async () => {
    // refresh 挂起 → 会话保持 pending；测试末尾放行，避免模块级 activeRefresh
    //  Promise 泄漏到后续用例（auth-session 的刷新去重是模块级单例）。
    let release: ((response: Response) => void) | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            release = resolve;
          }),
      ),
    );

    renderAt("/");

    expect(screen.getByLabelText("会话恢复中")).toBeInTheDocument();

    release?.(
      jsonResponse({ code: 40102, message: "刷新会话 Cookie 缺失" }, 401),
    );
    await waitFor(() => {
      expect(screen.queryByLabelText("会话恢复中")).toBeNull();
    });
  });

  it("anonymous 访问受保护路径重定向到 /login", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 40102, message: "刷新会话 Cookie 缺失" }, 401),
      ),
    );
    const { router } = renderAt("/m/demo.items.main");

    expect(
      await screen.findByRole("heading", { name: "用户登录" }),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/login");
  });

  it("authenticated 访问 /login 重定向回首页", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
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
      }),
    );
    const controller = createSessionController();
    controller.beginSession({ accessToken: "test-access" });
    const { router } = renderAt("/login", controller);

    await waitFor(() => {
      expect(router.state.location.pathname).not.toBe("/login");
    });
    // 首页是应用中心（Dashboard）。
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/");
    });
    expect(
      await screen.findByRole("heading", { name: "应用中心", level: 1 }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "用户登录" })).toBeNull();
  });

  it("anonymous 访问 /login 正常渲染登录页（不重定向死循环）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 40102, message: "刷新会话 Cookie 缺失" }, 401),
      ),
    );
    const { router } = renderAt("/login");

    expect(
      await screen.findByRole("heading", { name: "用户登录" }),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/login");
  });
});
