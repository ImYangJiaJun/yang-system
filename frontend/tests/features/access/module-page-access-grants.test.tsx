/**
 * `access.grants` 模块页（`/m/access.grants`，无视图模块 → PrimaryActionPanel）垂直切片。
 *
 * 回归背景：模块的 `primary_action` 是 `list_user_grants`——它要求必填路径参数
 * `user_id`（目标用户），而通用模块页此前只会自动下发 page/limit/search，于是每次
 * 进页都自动调用注定失败的查询、红框「缺少必填参数：目标用户」，直授管理面整页不可用
 * （2026-10-05 修复：主 Action 带人工必填参数时不再自动调用，改为在数据卡片上方渲染
 * 查询条件条，填齐后点「查询」才发请求）。
 *
 * 这里断言的是**只有页面能证明的事**：带必填参数的主 Action 不自动发请求也不红框、
 * 查询条件条拿到目标用户后才真的查询并渲染结果、无必填参数的主 Action 照旧自动加载、
 * 工具栏授予/撤销入口可用且对话框带「目标用户」字段。
 */

import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

const CATALOG_PATH = "/.well-known/yang/ui-catalog";
const PERMISSIONS_PATH = "/api/v1/access/permissions";
const GRANT_PATH = "/api/v1/access/grants";
const USER_GRANTS_PATH = "/api/v1/access/users/";
const DATASOURCE_PATH = "/api/v1/feishu/datasources/query";

type CapturedRequest = { url: string; method: string; body: unknown };

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function envelope(data: unknown) {
  return jsonResponse({ code: 0, message: "成功", data });
}

type ParamSource = "body" | "query" | "path" | "header";

function param(
  name: string,
  source: ParamSource,
  required = true,
  title?: string,
) {
  return { name, source, required, title: title ?? name, description: "" };
}

function action(
  operationId: string,
  method: string,
  path: string,
  params: ReturnType<typeof param>[] = [],
  inputSchema: Record<string, unknown> = {},
  outputSchema: Record<string, unknown> = {},
) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method,
    path,
    params,
    input_schema: inputSchema,
    output_schema: outputSchema,
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function presentation(operationId: string, title: string) {
  return {
    operation_id: operationId,
    placement: "toolbar",
    interaction: "form",
    title,
    availability: null,
    confirmation: null,
    record_parameter: null,
    view_id: null,
  };
}

/// 对齐 `access.grants` 模块（grants/mod.rs）注册的 Action 与展示投影。
function accessGrantsModule() {
  const grantInputSchema = {
    type: "object",
    properties: {
      user_id: { type: "integer", title: "目标用户" },
      permission: { type: "string", title: "权限" },
      expires_at: { type: "integer", title: "过期时间" },
    },
    required: ["user_id", "permission"],
  };
  const userGrantsSchema = {
    type: "object",
    properties: { user_id: { type: "integer", title: "目标用户" } },
    required: ["user_id"],
  };
  const actions = [
    action("access.grants.list_permissions", "GET", PERMISSIONS_PATH),
    action(
      "access.grants.grant_permission",
      "POST",
      GRANT_PATH,
      [
        param("user_id", "body", true, "目标用户"),
        param("permission", "body", true, "权限"),
        param("expires_at", "body", false, "过期时间"),
      ],
      grantInputSchema,
    ),
    action(
      "access.grants.revoke_permission",
      "POST",
      `${GRANT_PATH}/revoke`,
      [
        param("user_id", "body", true, "目标用户"),
        param("permission", "body", true, "权限"),
      ],
      grantInputSchema,
    ),
    action(
      "access.grants.list_user_grants",
      "GET",
      `${USER_GRANTS_PATH}{user_id}/grants`,
      [param("user_id", "path", true, "目标用户")],
      userGrantsSchema,
    ),
  ];
  return {
    schema_version: "2.3",
    revision: `1${"a".repeat(63)}`,
    actions,
    table_views: [],
    modules: [
      {
        module_id: "access.grants",
        identity: { id: "user", title: "用户", icon: "person", order: 1 },
        title: "权限管理",
        description: "管理用户直授权限与权限目录",
        icon: "access",
        order: 20,
        primary_action: "access.grants.list_user_grants",
        actions: actions.map((item) => item.operation_id),
        action_presentations: [
          presentation("access.grants.grant_permission", "授予权限"),
          presentation("access.grants.revoke_permission", "撤销权限"),
        ],
        views: [],
      },
    ],
  };
}

/// 无必填参数主 Action 的普通模块（防回归：自动加载路径不能被查询条件条改坏）。
function listDatasourceModule() {
  const actions = [
    action(
      "feishu.datasource.list_datasources",
      "POST",
      DATASOURCE_PATH,
      [param("page", "query"), param("limit", "query")],
      {
        type: "object",
        properties: {
          page: { type: "integer" },
          limit: { type: "integer" },
        },
      },
      {
        type: "object",
        properties: {
          items: {
            type: "array",
            items: {
              type: "object",
              properties: {
                id: { type: "integer", title: "ID" },
                title: { type: "string", title: "标题" },
              },
            },
          },
        },
      },
    ),
  ];
  return {
    schema_version: "2.3",
    revision: `2${"a".repeat(63)}`,
    actions,
    table_views: [],
    modules: [
      {
        module_id: "feishu.datasource",
        identity: { id: "user", title: "用户", icon: "person", order: 1 },
        title: "飞书数据源",
        description: "",
        icon: "database",
        order: 10,
        primary_action: "feishu.datasource.list_datasources",
        actions: ["feishu.datasource.list_datasources"],
        action_presentations: [],
        views: [],
      },
    ],
  };
}

function installFetchMock(catalog: () => unknown) {
  const captured: CapturedRequest[] = [];
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.href
            : input.url;
      if (url.includes(CATALOG_PATH)) return envelope(catalog());
      if (url.includes(PERMISSIONS_PATH)) {
        return envelope({ permissions: [] });
      }
      if (url.includes(DATASOURCE_PATH)) {
        captured.push({
          url,
          method: init?.method ?? "GET",
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return envelope({
          items: [{ id: 1, title: "第一个数据源" }],
          page: 1,
          page_size: 20,
          total: 1,
        });
      }
      if (url.includes(USER_GRANTS_PATH)) {
        captured.push({
          url,
          method: init?.method ?? "GET",
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return envelope({
          user_id: 7,
          grants: [{ permission: "access.grants.read" }],
        });
      }
      if (url.includes(GRANT_PATH) && init?.method === "POST") {
        captured.push({
          url,
          method: "POST",
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return envelope({
          user_id: 7,
          permission: "access.grants.read",
          changed: true,
        });
      }
      throw new Error(`测试未覆盖的请求：${init?.method ?? "GET"} ${url}`);
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return { fetchMock, captured };
}

beforeEach(() => {
  // 屏蔽 jsdom 中 Radix 组件依赖但与本测试无关的 API。
  Element.prototype.scrollIntoView ??= () => undefined;
});

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("access.grants 模块页（主 Action 带必填参数）", () => {
  it("进页不自动调用必填参数主 Action，也不红框；查询条件条拿到目标用户后才查询", async () => {
    const { fetchMock, captured } = installFetchMock(accessGrantsModule);
    renderTestApp({ path: "/m/access.grants", authenticated: true });

    // 工具栏入口照常渲染（授予/撤销仍可用）。
    expect(
      await screen.findByRole("button", { name: "授予权限" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "撤销权限" }),
    ).toBeInTheDocument();

    // 查询条件条出现「目标用户」输入；等过自动加载的防抖窗口，确认没有自动发请求。
    const targetInput = screen.getByLabelText(/目标用户/);
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(
      fetchMock.mock.calls.some(([input]) =>
        String(input).includes(USER_GRANTS_PATH),
      ),
    ).toBe(false);
    expect(screen.queryByText(/缺少必填参数/)).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    // 填齐后点「查询」才真的发出请求，卡片渲染返回的直授记录。
    const user = userEvent.setup();
    await user.type(targetInput, "7");
    await user.click(screen.getByRole("button", { name: "查询" }));
    await waitFor(() =>
      expect(captured.some((call) => call.url.includes(USER_GRANTS_PATH))).toBe(
        true,
      ),
    );
    expect(
      captured.find((call) => call.url.includes(USER_GRANTS_PATH))?.url,
    ).toBe(`${USER_GRANTS_PATH}7/grants`);
    expect(
      await screen.findByText('[{"permission":"access.grants.read"}]'),
    ).toBeInTheDocument();
  });

  it("工具栏「授予权限」对话框带目标用户字段，提交请求对齐 grant_permission 契约", async () => {
    const { captured } = installFetchMock(accessGrantsModule);
    const user = userEvent.setup();
    renderTestApp({ path: "/m/access.grants", authenticated: true });

    await user.click(await screen.findByRole("button", { name: "授予权限" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByLabelText(/目标用户/)).toBeInTheDocument();

    await user.type(within(dialog).getByLabelText(/目标用户/), "7");
    await user.type(
      within(dialog).getByLabelText(/权限/),
      "access.grants.read",
    );
    await user.click(within(dialog).getByRole("button", { name: "提交" }));
    await waitFor(() =>
      expect(captured.some((call) => call.url.includes(GRANT_PATH))).toBe(true),
    );
    const grant = captured.find((call) => call.url.includes(GRANT_PATH));
    expect(grant?.method).toBe("POST");
    expect(grant?.body).toEqual({
      user_id: 7,
      permission: "access.grants.read",
    });
  });
});

describe("无必填参数主 Action 的模块", () => {
  it("进页照旧自动加载并渲染表格", async () => {
    const { captured } = installFetchMock(listDatasourceModule);
    renderTestApp({ path: "/m/feishu.datasource", authenticated: true });

    expect(await screen.findByText("第一个数据源")).toBeInTheDocument();
    expect(captured.some((call) => call.url.includes(DATASOURCE_PATH))).toBe(
      true,
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
