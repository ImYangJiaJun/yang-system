import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "@/shell/App";
import { canReadAccessWorkspace } from "@/shell/access-workspace-permission";

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
  it("无工作台只读权限时隐藏入口，有任一只读权限时显示入口", () => {
    expect(
      canReadAccessWorkspace({
        schema_version: "2.3",
        revision: "test",
        actions: [],
        modules: [],
        table_views: [],
      }),
    ).toBe(false);
    expect(
      canReadAccessWorkspace({
        schema_version: "2.3",
        revision: "test",
        actions: [
          {
            operation_id: "access.grants.list_user_grants",
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
          },
        ],
        modules: [],
        table_views: [],
      }),
    ).toBe(true);
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
