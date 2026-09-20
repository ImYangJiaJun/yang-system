import { screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import catalogFixture from "@test/fixtures/ui-catalog.json";

/**
 * 失效传播（能力 11）：业务请求 401 → 刷新被拒 → yang:session-expired
 * → SessionBridge 清空会话并跳 /login，登录页展示对应提示（reason 经快照传递）。
 */

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function renderAppAuthenticated(path: string) {
  return renderTestApp({ path, authenticated: true });
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("会话失效传播", () => {
  it("业务 401 且刷新被拒 → 清空会话并跳登录页（reason 经快照传递）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(catalogFixture);
        }
        if (url.includes("/api/v1/users/refresh")) {
          return jsonResponse(
            { code: 40102, message: "刷新会话 Cookie 缺失" },
            401,
          );
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse({ code: 40102, message: "Token 已过期" }, 401);
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    const { controller, router } = renderAppAuthenticated("/m/demo.items.main");

    // 等待失效传播完成：落到登录页并展示过期提示（reason 经会话快照传递）。
    // 全量套件并行时环境较慢，放宽超时。
    await waitFor(
      () => {
        expect(screen.getByRole("heading", { name: "用户登录" })).toBeTruthy();
      },
      { timeout: 5000 },
    );
    await waitFor(
      () => {
        expect(router.state.location.pathname).toBe("/login");
      },
      { timeout: 5000 },
    );
    expect(
      await screen.findByRole("alert", undefined, { timeout: 5000 }),
    ).toHaveTextContent("登录状态已过期，请重新登录");
    expect(controller.getSnapshot()).toMatchObject({
      token: "",
      restoreState: "anonymous",
      loggedIn: false,
      sessionEndReason: "session-expired",
    });
  });

  /**
   * 会话边界必须清空查询缓存（审计 R2-H6）。
   *
   * 表数据查询键 `["table-data", view_id, state]` 不含会话维度：同一标签页登出 A 后
   * 登录 B，B 打开与 A 相同的视图会命中 A 的缓存并先渲染 A 的行；B 取数失败
   * （403 等）时 React Query 保留上一次成功的 data，这些行会一直留在屏幕上。
   */
  it("清空会话时级联清空查询缓存（下一会话不得读到上一会话的数据）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(catalogFixture);
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse({ code: 40301, message: "无权限" }, 403);
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    const { controller, queryClient } =
      renderAppAuthenticated("/m/demo.items.main");

    await waitFor(
      () => {
        expect(queryClient.getQueryCache().getAll().length).toBeGreaterThan(0);
      },
      { timeout: 5000 },
    );

    controller.clearSession();

    await waitFor(() => {
      expect(queryClient.getQueryCache().getAll()).toHaveLength(0);
    });
  });
});
