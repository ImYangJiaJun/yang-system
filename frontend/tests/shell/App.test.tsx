import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "@/shell/App";
import { renderTestApp } from "@test/helpers/render-app";

function jsonResponse(payload: unknown, status: number) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
});

describe("App", () => {
  it("只有 Action 没有 Module 时，不生成硬编码业务菜单", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(
          {
            code: 0,
            message: "成功",
            data: {
              schema_version: "2.4",
              revision: "c".repeat(64),
              modules: [],
              table_views: [],
              actions: [
                "access.groups.list_groups",
                "access.grants.list_user_grants",
                "feishu.datasource.list_datasources",
                "feishu.approval.list_configs",
                "feishu.approval.list_requests",
              ].map((operation_id) => ({
                operation_id,
                title: "",
                description: "",
                method: "GET",
                path: "/api/v1/test",
                params: [],
                input_schema: {},
                output_schema: {},
                request_media_type: "json",
                response_kind: "json",
                requires_auth: true,
              })),
            },
          },
          200,
        ),
      ),
    );
    renderTestApp({ path: "/" });
    await screen.findByRole("heading", { name: "应用中心" });
    expect(
      within(screen.getByRole("navigation")).queryAllByRole("link"),
    ).toHaveLength(0);
  });
  it("导航与首页卡片仅消费 Catalog 落点", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(
          {
            code: 0,
            message: "成功",
            data: {
              schema_version: "2.4",
              revision: "a".repeat(64),
              actions: [],
              table_views: [],
              modules: [
                {
                  module_id: "access.grants",
                  identity: {
                    id: "admin",
                    title: "系统管理",
                    icon: "access",
                    order: 20,
                  },
                  title: "权限管理",
                  description: "",
                  icon: "access",
                  order: 20,
                  app_route: "/access/workspace",
                  actions: [],
                  action_presentations: [],
                  views: [],
                },
              ],
            },
          },
          200,
        ),
      ),
    );
    renderTestApp({ path: "/", identity: "admin" });
    const nav = within(screen.getByRole("navigation"));
    expect(await nav.findByRole("link", { name: "权限管理" })).toHaveAttribute(
      "href",
      "/access/workspace",
    );
    expect(nav.getAllByRole("link")).toHaveLength(1);
    expect(screen.getByTestId("module-card-access.grants")).toHaveAttribute(
      "href",
      "/access/workspace",
    );
  });
  it("匿名启动经会话恢复门控落到登录页", async () => {
    // 无 Refresh Cookie：恢复失败 → anonymous → 登录页。
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 40102, message: "刷新会话 Cookie 缺失" }, 401),
      ),
    );

    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "用户登录" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "YANG System" }),
    ).toBeInTheDocument();
  });
});
