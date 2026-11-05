import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

/**
 * 用户管理页的停用/启用闭环：这两个 Action 一直在后端存在，但页面没有入口，
 * 管理员只能手调 API。这里钉住入口、确认框与状态回读。
 */

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function action(operationId: string) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method: "POST",
    path: "/api/v1/users/{id}/disable",
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function catalog(operations: string[]) {
  return {
    schema_version: "2.3",
    // revision 相同会被进程内 CatalogCache 当成同一份目录复用，
    // 所以它必须随操作集合变化，而不能只按数量变化。
    revision: String(operations.length).padEnd(64, "a"),
    actions: operations.map(action),
    table_views: [],
    modules: [],
  };
}

type Call = { url: string; method: string };

function stub(operations: string[]) {
  const calls: Call[] = [];
  let active = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = init?.method ?? "GET";
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: catalog(operations),
        });
      }
      if (url.includes("/api/v1/users/") && method === "POST") {
        calls.push({ url, method });
        const toggling = url.endsWith("/disable") ? "disable" : "enable";
        active = toggling !== "disable";
        return jsonResponse({ code: 0, message: "成功", data: {} });
      }
      if (url.includes("/api/v1/users?")) {
        return jsonResponse({
          code: 0,
          data: {
            users: [
              {
                id: 7,
                username: "alice",
                email: null,
                status: active ? "active" : "disabled",
              },
            ],
          },
        });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
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

describe("用户管理 · 停用与启用", () => {
  it("有管理权限时出现停用入口，确认后提交并回读状态", async () => {
    const calls = stub([
      "account.admin_users.admin_issue_password_reset",
      "account.admin_users.admin_disable_user",
    ]);
    const user = userEvent.setup();
    renderTestApp({ path: "/admin/users", authenticated: true });

    await screen.findByText("alice");
    await user.click(screen.getByRole("button", { name: "停用" }));

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/alice/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "停用" }));

    await waitFor(() =>
      expect(calls).toEqual([
        { url: "/api/v1/users/7/disable", method: "POST" },
      ]),
    );
    // 回读后行状态与按钮都翻转，不需要手工刷新。
    expect(await screen.findByText("停用")).toBeInTheDocument();
    expect(
      await screen.findByRole("button", { name: "启用" }),
    ).toBeInTheDocument();
  });

  it("没有管理权限时不渲染停用/启用入口", async () => {
    stub(["account.admin_users.admin_issue_password_reset"]);
    renderTestApp({ path: "/admin/users", authenticated: true });

    await screen.findByText("alice");
    expect(screen.queryByRole("button", { name: "停用" })).toBeNull();
    expect(screen.queryByRole("button", { name: "启用" })).toBeNull();
  });
});
