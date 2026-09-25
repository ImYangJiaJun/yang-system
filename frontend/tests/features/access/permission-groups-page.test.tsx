/**
 * 权限组管理页（走真实路由 `/access/groups`，含 lazy 的 `.default` 约定与真实 api.ts）。
 *
 * 桩只替换 `fetch`：权限按身份投影这一点用目录本身模拟——不给某粒 Action，
 * 就等于这个身份没有那粒权限位。
 *
 * 这里断言的是**只有页面能证明的事**：内置全权组没有条目矩阵但有目录计数、
 * 孤儿条目被单独标出来、勾选/取消真的发出写请求并回读、无写权限时写入口不渲染。
 * 断言「渲染了一个 div」没有意义，所以每条用例都末了核对发出去的请求。
 */

import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UiCatalog } from "@/engine/contracts/ui-catalog";
import { clearStoredSession } from "@/engine/session/auth-session";
import { canManageGroups } from "@/features/access/api";
import { renderTestApp } from "@test/helpers/render-app";

/*
 * 预热本页模块（路由表里是 `lazy: () => import(...)`）。
 *
 * 用例里第一次渲染该路由时，Vite 才去转换并求值这一整棵依赖图（页面 + Radix Checkbox
 * 等），这笔**每个测试文件一次**的冷启动开销会落在该文件第一个用例的 `findBy*` 预算里。
 * 在夹具加载期先 import 一次，运行时照旧走自己的 `lazy: () => import(...)`（命中模块
 * 缓存），懒加载 + Suspense 那条路径仍然被覆盖。
 */
await import("@/features/access/views/PermissionGroupsPage");

const LIST_PATH = "/api/v1/access/groups";
const ITEMS_PATH = `${LIST_PATH}/items`;
const ITEMS_REMOVE_PATH = `${ITEMS_PATH}/remove`;
const MEMBERS_PATH = `${LIST_PATH}/members`;
const MEMBERS_REMOVE_PATH = `${MEMBERS_PATH}/remove`;
const DETAIL_PATH = /\/api\/v1\/access\/groups\/\d+$/;

type RecordedCall = {
  url: string;
  method: string;
  body: Record<string, unknown> | undefined;
};

type Handler = (
  body: Record<string, unknown>,
) => unknown | Response | Promise<unknown | Response>;

export function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function envelope(data: unknown, message = "成功"): Response {
  return jsonResponse({ code: 0, message, data });
}

type ParamSource = "body" | "query" | "path" | "header";

function param(name: string, source: ParamSource, required = true) {
  return { name, source, required, title: name, description: "" };
}

function action(
  operationId: string,
  method: string,
  path: string,
  params: ReturnType<typeof param>[] = [],
) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method,
    path,
    params,
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

/// 服务端 `access.groups` 模块注册的九个 Action（`actions/mod.rs` 的注册表顺序）。
const READ_ACTIONS = [
  action("access.groups.list_groups", "GET", LIST_PATH),
  action("access.groups.get_group", "GET", `${LIST_PATH}/{group_id}`, [
    param("group_id", "path"),
  ]),
];

const WRITE_ACTIONS = [
  action("access.groups.create_group", "POST", LIST_PATH, [
    param("group_key", "body"),
    param("title", "body"),
    param("description", "body", false),
  ]),
  action("access.groups.update_group", "POST", `${LIST_PATH}/update`, [
    param("group_id", "body"),
    param("title", "body"),
    param("description", "body", false),
  ]),
  action("access.groups.delete_group", "POST", `${LIST_PATH}/delete`, [
    param("group_id", "body"),
  ]),
  action("access.groups.add_group_item", "POST", ITEMS_PATH, [
    param("group_id", "body"),
    param("permission", "body"),
  ]),
  action("access.groups.remove_group_item", "POST", ITEMS_REMOVE_PATH, [
    param("group_id", "body"),
    param("permission", "body"),
  ]),
  action("access.groups.add_group_member", "POST", MEMBERS_PATH, [
    param("group_id", "body"),
    param("user_id", "body"),
  ]),
  action("access.groups.remove_group_member", "POST", MEMBERS_REMOVE_PATH, [
    param("group_id", "body"),
    param("user_id", "body"),
  ]),
];

type StubOptions = {
  /// 目录里有没有 `access.groups.read` / `access.groups.write`。
  read?: boolean;
  write?: boolean;
  groupList?: Handler;
  groupDetail?: Handler;
  createGroup?: Handler;
  renameGroup?: Handler;
  deleteGroup?: Handler;
  addItem?: Handler;
  removeItem?: Handler;
  addMember?: Handler;
  removeMember?: Handler;
};

/// **本文件所有用例共用一个可见 Action 集合**：内置组的目录计数要按它算。
function visibleActions(options: StubOptions) {
  const actions = [];
  if (options.read ?? true) actions.push(...READ_ACTIONS);
  if (options.write ?? true) actions.push(...WRITE_ACTIONS);
  return actions;
}

function catalogFor(options: StubOptions): Response {
  const actions = visibleActions(options);
  // revision 必须随权限组合变化：引擎的 `CatalogCache.accept` 在 revision 相同时会
  // 直接复用上一份目录对象，同文件内的多个测试就会互相串味（它只能是 64 位十六进制）。
  const flags = [
    (options.read ?? true) ? "1" : "0",
    (options.write ?? true) ? "1" : "0",
  ].join("");
  return jsonResponse({
    code: 0,
    message: "成功",
    data: {
      schema_version: "2.3",
      revision: `${flags}${"a".repeat(64)}`.slice(0, 64),
      actions,
      table_views: [],
      modules: [],
    },
  });
}

const ME = {
  id: 7,
  username: "alice",
  email: "alice@example.com",
  email_verified_at: 1000,
  status: "active",
  created_at: 500,
  updated_at: 600,
};

export function groupWire(overrides: Record<string, unknown> = {}) {
  return {
    id: 2,
    group_key: "ops",
    title: "运维",
    description: null,
    member_count: 0,
    item_count: 0,
    is_builtin: false,
    orphan_item_count: 0,
    ...overrides,
  };
}

export function groupDetailWire(overrides: Record<string, unknown> = {}) {
  return {
    id: 2,
    group_key: "ops",
    title: "运维",
    description: null,
    effective_all: false,
    items: [],
    members: [],
    ...overrides,
  };
}

async function respond(
  handler: Handler | undefined,
  body: Record<string, unknown>,
  fallback: unknown,
): Promise<Response> {
  const value = handler ? await handler(body) : fallback;
  return value instanceof Response ? value : envelope(value);
}

/// 装上 fetch 桩；没覆盖到的请求直接抛错，避免测试悄悄走一条没人管的路径。
function stubAccessApi(options: StubOptions = {}): RecordedCall[] {
  const calls: RecordedCall[] = [];

  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      let body: Record<string, unknown> | undefined;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body) as Record<string, unknown>;
        } catch {
          body = undefined;
        }
      }
      calls.push({ url, method, body });
      const payload = body ?? {};

      if (url.endsWith("/.well-known/yang/ui-catalog"))
        return catalogFor(options);
      if (url.endsWith("/api/v1/users/me")) return envelope(ME);
      if (url.endsWith(ITEMS_REMOVE_PATH))
        return respond(options.removeItem, payload, {
          group_id: payload.group_id,
          permission: payload.permission,
          changed: true,
        });
      if (url.endsWith(ITEMS_PATH))
        return respond(options.addItem, payload, {
          group_id: payload.group_id,
          permission: payload.permission,
          changed: true,
        });
      if (url.endsWith(MEMBERS_REMOVE_PATH))
        return respond(options.removeMember, payload, {
          group_id: payload.group_id,
          user_id: payload.user_id,
          changed: true,
        });
      if (url.endsWith(MEMBERS_PATH))
        return respond(options.addMember, payload, {
          group_id: payload.group_id,
          user_id: payload.user_id,
          changed: true,
        });
      if (url.endsWith(`${LIST_PATH}/update`))
        return respond(options.renameGroup, payload, {
          id: payload.group_id,
          title: payload.title,
          description: payload.description ?? null,
        });
      if (url.endsWith(`${LIST_PATH}/delete`))
        return respond(options.deleteGroup, payload, {
          group_key: "ops",
        });
      if (DETAIL_PATH.test(url)) {
        // 详情是 GET，`group_id` 在**路径**里——请求体是空的，所以这里必须从 URL
        // 把主键取出来交给替身，否则「按主键定位」这件事在测试里根本没被覆盖。
        const detailId = Number(url.slice(url.lastIndexOf("/") + 1));
        return respond(
          options.groupDetail,
          { group_id: detailId },
          groupDetailWire({ id: detailId }),
        );
      }
      if (url.endsWith(LIST_PATH)) {
        if (method === "POST")
          return respond(options.createGroup, payload, {
            id: 9,
            group_key: payload.group_key,
            title: payload.title,
            description: payload.description ?? null,
          });
        return respond(options.groupList, payload, { groups: [] });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
    }),
  );

  return calls;
}

function callsTo(calls: RecordedCall[], path: string) {
  return calls.filter((call) => call.url.endsWith(path));
}

function detailCalls(calls: RecordedCall[]) {
  return calls.filter((call) => DETAIL_PATH.test(call.url));
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderPage() {
  return renderTestApp({ path: "/access/groups", authenticated: true });
}

/* ------------------------------- 权限门控 -------------------------------- */

describe("canManageGroups", () => {
  it("requires the write operation to be visible in the catalog", () => {
    const readOnly = {
      modules: [],
      actions: [{ operation_id: "access.groups.list_groups" }],
    } as unknown as UiCatalog;
    expect(canManageGroups(readOnly)).toBe(false);
  });

  it("is true when the write operation is present", () => {
    const writable = {
      modules: [],
      actions: [
        { operation_id: "access.groups.list_groups" },
        { operation_id: "access.groups.create_group" },
      ],
    } as unknown as UiCatalog;
    expect(canManageGroups(writable)).toBe(true);
  });
});

/* --------------------------------- 页面 ---------------------------------- */

describe("权限组管理页", () => {
  it("组列表带内置与孤儿徽标，并显示成员数/权限数", async () => {
    stubAccessApi({
      groupList: () => ({
        groups: [
          groupWire({
            id: 1,
            group_key: "system_admin",
            title: "系统管理员",
            member_count: 1,
            is_builtin: true,
          }),
          groupWire({
            id: 2,
            group_key: "ops",
            title: "运维",
            member_count: 3,
            item_count: 4,
            orphan_item_count: 2,
          }),
        ],
      }),
      groupDetail: () => groupDetailWire({ id: 1, members: [7] }),
    });

    renderPage();

    const list = await screen.findByRole("list", { name: "权限组列表" });
    const builtin = within(list).getByRole("button", { name: /系统管理员/ });
    expect(within(builtin).getByText("内置")).toBeInTheDocument();
    expect(builtin).toHaveTextContent("成员 1");
    // 内置组没有条目表，权限数按 0 计——徽标与计数都不许凭空造一个数
    expect(builtin).toHaveTextContent("权限 0");

    const ops = within(list).getByRole("button", { name: /运维/ });
    expect(within(ops).getByText("孤儿 2")).toBeInTheDocument();
    expect(ops).toHaveTextContent("成员 3");
    expect(ops).toHaveTextContent("权限 4");
  });

  it("内置全权组不渲染条目矩阵，改为一句话说明权限由权限目录实时计算", async () => {
    const user = userEvent.setup();
    stubAccessApi({
      groupList: () => ({
        groups: [
          groupWire({ id: 2, group_key: "ops", title: "运维" }),
          groupWire({
            id: 1,
            group_key: "system_admin",
            title: "系统管理员",
            is_builtin: true,
          }),
        ],
      }),
      groupDetail: ({ group_id }) =>
        group_id === 1
          ? groupDetailWire({
              id: 1,
              group_key: "system_admin",
              title: "系统管理员",
              effective_all: true,
              // 内置组的条目表里没有授权事实，服务端即使回了行也不该被当成矩阵
              items: [],
              members: [7],
            })
          : groupDetailWire({ id: 2 }),
    });

    renderPage();

    const list = await screen.findByRole("list", { name: "权限组列表" });
    await user.click(within(list).getByRole("button", { name: /系统管理员/ }));

    // 目录里这个身份看得到 2 个读 Action + 7 个写 Action。
    expect(
      await screen.findByText(
        "该组的权限由权限目录实时计算，共 9 项，不在此处逐条列出。",
      ),
    ).toBeInTheDocument();
    // 矩阵整块不渲染：一个复选框都没有
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByRole("list", { name: "组权限条目" })).toBeNull();
    // 成员那一块照常渲染（全权组也有成员）
    expect(screen.getByRole("list", { name: "组成员" })).toHaveTextContent(
      "用户 #7",
    );
  });

  it("孤儿条目以警示样式渲染，并说明可以安全移除", async () => {
    stubAccessApi({
      groupList: () => ({
        groups: [groupWire({ id: 2, orphan_item_count: 1 })],
      }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2 }),
        items: [
          { permission: "account.users.read", is_orphan: false },
          { permission: "demo.notes.read", is_orphan: true },
        ],
      }),
    });

    renderPage();

    const items = await screen.findByRole("list", { name: "组权限条目" });
    const orphanRow = within(items).getByText("demo.notes.read").closest("li");
    const normalRow = within(items)
      .getByText("account.users.read")
      .closest("li");
    expect(orphanRow).not.toBeNull();
    expect(normalRow).not.toBeNull();
    if (orphanRow === null || normalRow === null) return;

    expect(orphanRow).toHaveAttribute("data-orphan", "true");
    expect(orphanRow.className).toContain("destructive");
    expect(orphanRow).toHaveTextContent("该权限已不在权限目录中，可安全移除");

    // 正常条目不许被同一套警示样式染上
    expect(normalRow).toHaveAttribute("data-orphan", "false");
    expect(normalRow.className).not.toContain("destructive");
    expect(normalRow).not.toHaveTextContent(
      "该权限已不在权限目录中，可安全移除",
    );
  });

  it("取消勾选一条权限会发出移除请求，并回读详情", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2, item_count: 1 })] }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2 }),
        items: [{ permission: "account.users.read", is_orphan: false }],
      }),
    });

    renderPage();

    const items = await screen.findByRole("list", { name: "组权限条目" });
    const checkbox = within(items).getByRole("checkbox", {
      name: "account.users.read 权限",
    });
    expect(checkbox).toBeChecked();

    await user.click(checkbox);

    await waitFor(() => {
      expect(callsTo(calls, ITEMS_REMOVE_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, ITEMS_REMOVE_PATH)[0]?.body).toEqual({
      group_id: 2,
      permission: "account.users.read",
    });
    // 回读：详情被重新拉了一次（写接口只回计数，最新条目只能靠回读）
    await waitFor(() => {
      expect(detailCalls(calls).length).toBeGreaterThanOrEqual(2);
    });
  });

  it("加入成员会发出加成员请求，并回读详情", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2, members: [7] }),
    });

    renderPage();

    const members = await screen.findByRole("list", { name: "组成员" });
    expect(members).toHaveTextContent("用户 #7");

    await user.type(screen.getByLabelText("要加入的用户 ID"), "8");
    await user.click(screen.getByRole("button", { name: "加入成员" }));

    await waitFor(() => {
      expect(callsTo(calls, MEMBERS_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, MEMBERS_PATH)[0]?.body).toEqual({
      group_id: 2,
      user_id: 8,
    });
    await waitFor(() => {
      expect(detailCalls(calls).length).toBeGreaterThanOrEqual(2);
    });
  });

  it("只有读权限时仍能看列表与详情，但一个写入口都不渲染", async () => {
    stubAccessApi({
      write: false,
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2, members: [7] }),
        items: [{ permission: "account.users.read", is_orphan: false }],
      }),
    });

    renderPage();

    // 读侧照常：列表、条目、成员都在
    expect(
      await screen.findByRole("list", { name: "权限组列表" }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("list", { name: "组权限条目" }),
    ).toHaveTextContent("account.users.read");
    expect(screen.getByRole("list", { name: "组成员" })).toHaveTextContent(
      "用户 #7",
    );

    // 写侧整块不渲染：复选框、加入表单、新建/改名/删除都不在
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByRole("button", { name: "加入成员" })).toBeNull();
    expect(screen.queryByRole("button", { name: "加入权限" })).toBeNull();
    expect(screen.queryByRole("button", { name: "新建权限组" })).toBeNull();
    expect(screen.queryByRole("button", { name: "删除该组" })).toBeNull();
  });

  it("没有读权限时不发列表请求，只说明联系管理员", async () => {
    const calls = stubAccessApi({ read: false });

    renderPage();

    expect(
      await screen.findByText(
        "当前身份没有查看权限组的权限，请联系运维管理员开通。",
      ),
    ).toBeInTheDocument();
    expect(calls.some((call) => call.url.endsWith(LIST_PATH))).toBe(false);
  });
});
