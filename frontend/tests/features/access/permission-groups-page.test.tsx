/**
 * 权限组管理（走真实路由 `/access/groups`——工作台「按组」tab，旧 URL 兼容；
 * 含 lazy 的 `.default` 约定与真实 api.ts）。
 *
 * 桩只替换 `fetch`：权限按身份投影这一点用目录本身模拟——不给某粒 Action，
 * 就等于这个身份没有那粒权限位。
 *
 * 这里断言的是**只有页面能证明的事**：内置全权组没有条目矩阵但有目录计数、
 * 孤儿条目被单独标出来、勾选/取消真的发出写请求并回读、无写权限时写入口不渲染、
 * 加权限走候选目录弹窗（多选、管理员等价徽标、候选排除已在组的权限）、
 * `session-refreshed` 后整块回读。
 * 断言「渲染了一个 div」没有意义，所以每条用例都末了核对发出去的请求。
 */

import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UiCatalog } from "@/engine/contracts/ui-catalog";
import {
  clearStoredSession,
  SESSION_REFRESHED_EVENT,
} from "@/engine/session/auth-session";
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
await import("@/features/access/views/PermissionWorkspacePage");

const LIST_PATH = "/api/v1/access/groups";
const LOOKUP_PATH = "/api/v1/users/lookup";
const ITEMS_PATH = `${LIST_PATH}/items`;
const ITEMS_REMOVE_PATH = `${ITEMS_PATH}/remove`;
const MEMBERS_PATH = `${LIST_PATH}/members`;
const MEMBERS_REMOVE_PATH = `${MEMBERS_PATH}/remove`;
const DETAIL_PATH = /\/api\/v1\/access\/groups\/\d+$/;
const PERMISSIONS_PATH = "/api/v1/access/permissions";

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

/// 权限目录读接口（注册在 `access.grants` 模块，权限位 `access.grants.read`）。
const PERMISSION_ACTIONS = [
  action("access.grants.list_permissions", "GET", PERMISSIONS_PATH),
];

/// 工作台用户查找（成员名映射与成员添加的 UserPicker 都靠它；
/// 目录里有它才会发 lookup 请求，没有时名字回退「用户 #id」）。
const LOOKUP_ACTIONS = [
  action("account.users.lookup", "GET", LOOKUP_PATH, [
    param("q", "query", false),
    param("page", "query", false),
    param("page_size", "query", false),
  ]),
];

type StubOptions = {
  /// 目录里有没有 `access.groups.read` / `access.groups.write` /
  /// `access.grants.list_permissions`（三粒独立权限位）。
  read?: boolean;
  write?: boolean;
  permissions?: boolean;
  groupList?: Handler;
  groupDetail?: Handler;
  createGroup?: Handler;
  renameGroup?: Handler;
  deleteGroup?: Handler;
  addItem?: Handler;
  removeItem?: Handler;
  addMember?: Handler;
  removeMember?: Handler;
  permissionsList?: Handler;
  lookup?: Handler;
};

/// **本文件所有用例共用一个可见 Action 集合**：内置组的目录计数要按它算。
function visibleActions(options: StubOptions) {
  const actions = [...LOOKUP_ACTIONS];
  if (options.read ?? true) actions.push(...READ_ACTIONS);
  if (options.write ?? true) actions.push(...WRITE_ACTIONS);
  if (options.permissions ?? true) actions.push(...PERMISSION_ACTIONS);
  return actions;
}

function catalogFor(options: StubOptions): Response {
  const actions = visibleActions(options);
  // revision 必须随权限组合变化：引擎的 `CatalogCache.accept` 在 revision 相同时会
  // 直接复用上一份目录对象，同文件内的多个测试就会互相串味（它只能是 64 位十六进制）。
  const flags = [
    (options.read ?? true) ? "1" : "0",
    (options.write ?? true) ? "1" : "0",
    (options.permissions ?? true) ? "1" : "0",
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
    // 默认「当前身份可管理该组」（对齐真实后端：列表按可见性过滤，
    // 能看见的组绝大多数是所有者/成员，管理按钮由 can_manage 驱动）。
    can_manage: true,
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
    can_manage: true,
    ...overrides,
  };
}

/// 权限目录的测试投影：普通权限与管理员等价权限各若干条
/// （`admin_equivalent` 标记对齐 `sensitive_permissions.rs` 的清单口径）。
function permissionWire(overrides: Record<string, unknown> = {}) {
  return {
    permission: "account.users.read",
    declared_by: ["account.users.list_users"],
    admin_equivalent: false,
    ...overrides,
  };
}

const PERMISSION_CATALOG = [
  permissionWire(),
  permissionWire({
    permission: "demo.notes.read",
    declared_by: ["demo.notes.list_notes"],
  }),
  permissionWire({
    permission: "demo.notes.write",
    declared_by: ["demo.notes.create_note"],
  }),
  permissionWire({
    permission: "account.users.reset_credentials",
    declared_by: ["account.users.reset_credentials"],
    admin_equivalent: true,
  }),
];

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
      if (url.includes(LOOKUP_PATH)) {
        // 目录全量拉取（q 为空）默认返回空：成员名回退「用户 #id」；
        // 搜索词非空时返回 bob（id 8），供成员添加的 UserPicker 用。
        const q = new URL(url, "http://localhost").searchParams.get("q") ?? "";
        return respond(
          options.lookup,
          { q },
          q === ""
            ? { users: [] }
            : {
                users: [
                  {
                    id: 8,
                    username: "bob",
                    email: "bob@example.com",
                    status: "active",
                  },
                ],
              },
        );
      }
      if (url.endsWith(PERMISSIONS_PATH))
        return respond(options.permissionsList, payload, {
          permissions: PERMISSION_CATALOG,
        });
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

    // 目录里这个身份看得到 1 个 lookup + 2 个读 Action + 7 个写 Action
    // + 1 个权限目录读接口（内置组的计数按整个 UI 目录的 Action 数算）。
    expect(
      await screen.findByText(
        "该组的权限由权限目录实时计算，共 11 项，不在此处逐条列出。",
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
    // 目录全量拉取为空时名字回退「用户 #id」
    expect(members).toHaveTextContent("用户 #7");

    // 成员添加走 UserPicker：搜索出 bob（id 8），勾选后一次提交
    await user.type(screen.getByLabelText("搜索用户"), "bob");
    await user.click(await screen.findByRole("button", { name: /bob/ }));
    await user.click(screen.getByRole("button", { name: "加入所选成员" }));

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

  it("不是组所有者也没有全局写权限时能看列表与详情，但一个管理入口都不渲染", async () => {
    stubAccessApi({
      write: false,
      groupList: () => ({ groups: [groupWire({ id: 2, can_manage: false })] }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2, members: [7], can_manage: false }),
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

    // 管理侧整块不渲染：复选框、添加/移出入口、改名/删除都不在
    // （管理按钮由后端算好的组级 can_manage 驱动，不再看全局写权限位）
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByRole("button", { name: "添加条目" })).toBeNull();
    expect(screen.queryByRole("button", { name: "加入所选成员" })).toBeNull();
    expect(screen.queryByLabelText("搜索用户")).toBeNull();
    expect(screen.queryByRole("button", { name: "新建权限组" })).toBeNull();
    expect(screen.queryByRole("button", { name: "删除该组" })).toBeNull();
    expect(screen.queryByRole("button", { name: "保存" })).toBeNull();
  });

  it("组所有者（can_manage=true）即使没有全局写权限也能管理该组", async () => {
    stubAccessApi({
      // 目录里没有 create_group（写侧权限位全缺），但后端对该组回了
      // can_manage: true（本组所有者）——管理按钮按组级判据渲染。
      write: false,
      groupList: () => ({ groups: [groupWire({ id: 2, can_manage: true })] }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2, members: [7], can_manage: true }),
        items: [{ permission: "account.users.read", is_orphan: false }],
      }),
    });

    renderPage();

    // 组级管理入口都在：改名、删除、条目复选框、添加条目、成员添加
    await screen.findByRole("button", { name: "删除该组" });
    expect(screen.getByRole("button", { name: "保存" })).toBeInTheDocument();
    expect(
      await screen.findByRole("checkbox", { name: "account.users.read 权限" }),
    ).toBeChecked();
    expect(
      screen.getByRole("button", { name: "添加条目" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("搜索用户")).toBeInTheDocument();
    // 只有建组按钮看登录态权限位：目录里没有 create_group 时不渲染
    expect(screen.queryByRole("button", { name: "新建权限组" })).toBeNull();
  });

  it("没有读权限时不发列表请求，只提示登录", async () => {
    const calls = stubAccessApi({ read: false });

    renderPage();

    // authenticated-only 后，登录用户的目录恒含 list_groups；这个分支是防御性的
    // （未登录/目录未就绪），提示语不再提「开通权限」。
    expect(
      await screen.findByText("查看权限组需要先登录。"),
    ).toBeInTheDocument();
    expect(calls.some((call) => call.url.endsWith(LIST_PATH))).toBe(false);
  });

  it("新建权限组会发出建组 POST 并刷新列表", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [] }),
    });

    renderPage();

    expect(
      await screen.findByText("还没有权限组。用下面的表单建第一个。"),
    ).toBeInTheDocument();

    await user.type(screen.getByLabelText("组标识"), "ops");
    await user.type(screen.getByLabelText("展示名"), "运维组");
    await user.click(screen.getByRole("button", { name: "新建权限组" }));

    await waitFor(() => {
      const posts = callsTo(calls, LIST_PATH).filter(
        (c) => c.method === "POST",
      );
      expect(posts).toHaveLength(1);
      expect(posts[0]?.body).toEqual({
        group_key: "ops",
        title: "运维组",
        description: undefined,
      });
    });
    // 列表回读
    await waitFor(() => {
      expect(
        callsTo(calls, LIST_PATH).filter((c) => c.method === "GET").length,
      ).toBeGreaterThanOrEqual(2);
    });
  });

  it("修改组展示名会发出改名 POST 并刷新详情", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2, title: "运维" })] }),
      groupDetail: () => groupDetailWire({ id: 2, title: "运维" }),
    });

    renderPage();

    // 选中组后改名表单出现在详情面板里
    await screen.findByRole("button", { name: "删除该组" });
    const detailHeading = screen.getByRole("heading", { name: "运维" });
    const detailPanel = detailHeading.closest("header");
    expect(detailPanel).not.toBeNull();
    const renameTitle = within(detailPanel!).getByLabelText("展示名");
    await user.clear(renameTitle);
    await user.type(renameTitle, "生产运维");
    await user.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(callsTo(calls, `${LIST_PATH}/update`)).toHaveLength(1);
    });
    expect(callsTo(calls, `${LIST_PATH}/update`)[0]?.body).toEqual({
      group_id: 2,
      title: "生产运维",
      description: undefined,
    });
    // 详情回读
    await waitFor(() => {
      expect(detailCalls(calls).length).toBeGreaterThanOrEqual(2);
    });
  });

  it("删除组会发出删除 POST 并刷新列表", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2, title: "运维" })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    renderPage();

    await screen.findByRole("button", { name: "删除该组" });
    await user.click(screen.getByRole("button", { name: "删除该组" }));

    await waitFor(() => {
      expect(callsTo(calls, `${LIST_PATH}/delete`)).toHaveLength(1);
    });
    expect(callsTo(calls, `${LIST_PATH}/delete`)[0]?.body).toEqual({
      group_id: 2,
    });
    // 列表回读
    await waitFor(() => {
      expect(
        callsTo(calls, LIST_PATH).filter((c) => c.method === "GET").length,
      ).toBeGreaterThanOrEqual(2);
    });
  });

  it("从候选目录弹窗选一条权限会发出加权限 POST 并刷新详情", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    renderPage();

    await screen.findByText(
      "该组还没有任何权限条目，成员加进来也不会得到权限。",
    );

    // 候选是弹窗勾选，不是自由文本：打开「添加条目」，勾选「account.users.read」提交
    await user.click(screen.getByRole("button", { name: "添加条目" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("checkbox", {
        name: "account.users.read 权限",
      }),
    );
    await user.click(within(dialog).getByRole("button", { name: "确认加入" }));

    await waitFor(() => {
      expect(callsTo(calls, ITEMS_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, ITEMS_PATH)[0]?.body).toEqual({
      group_id: 2,
      permission: "account.users.read",
    });
    // 详情回读
    await waitFor(() => {
      expect(detailCalls(calls).length).toBeGreaterThanOrEqual(2);
    });
  });

  it("候选里标出管理员等价权限的危害面徽标", async () => {
    const user = userEvent.setup();
    stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    renderPage();

    await screen.findByText(
      "该组还没有任何权限条目，成员加进来也不会得到权限。",
    );

    await user.click(screen.getByRole("button", { name: "添加条目" }));
    const dialog = await screen.findByRole("dialog");
    const sensitive = within(dialog).getByRole("checkbox", {
      name: "account.users.reset_credentials 权限",
    });
    expect(sensitive.closest("label")).toHaveTextContent("管理员等价");
    // 普通权限不背同一块徽标
    const normal = within(dialog).getByRole("checkbox", {
      name: "account.users.read 权限",
    });
    expect(normal.closest("label")).not.toHaveTextContent("管理员等价");
  });

  it("候选列表不含组里已有的权限", async () => {
    const user = userEvent.setup();
    stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2, item_count: 1 })] }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2 }),
        items: [{ permission: "account.users.read", is_orphan: false }],
      }),
    });

    renderPage();

    await screen.findByRole("list", { name: "组权限条目" });

    await user.click(screen.getByRole("button", { name: "添加条目" }));
    const dialog = await screen.findByRole("dialog");
    // 目录已加载（出现别的选项），但已在组里的那条不在候选里
    await within(dialog).findByRole("checkbox", {
      name: /reset_credentials/,
    });
    expect(
      within(dialog).queryByRole("checkbox", {
        name: "account.users.read 权限",
      }),
    ).toBeNull();
  });

  it("能管理组但看不到权限目录时，加权限入口换成说明且不发目录请求", async () => {
    const calls = stubAccessApi({
      permissions: false,
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    renderPage();

    await screen.findByText(
      "该组还没有任何权限条目，成员加进来也不会得到权限。",
    );
    expect(screen.queryByRole("button", { name: "加入权限" })).toBeNull();
    expect(screen.getByText(/看不到权限目录/)).toBeInTheDocument();
    // 不渲染入口的同时也不许发注定 403 的目录请求
    expect(callsTo(calls, PERMISSIONS_PATH)).toHaveLength(0);
  });

  it("权限目录查询失败时，换成错误说明并给出重试入口；重试后可正常添加", async () => {
    const user = userEvent.setup();
    let failed = true;
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
      // 目录第一次返回 500：失败态必须和「加载出了空目录」的合法空态区分开
      permissionsList: () =>
        failed
          ? jsonResponse({ code: 50001, message: "服务内部错误" }, 500)
          : { permissions: PERMISSION_CATALOG },
    });

    renderPage();

    await screen.findByText(
      "该组还没有任何权限条目，成员加进来也不会得到权限。",
    );

    // 失败态：先等目录查询失败 settle（ItemPanel 挂载时才发目录请求），
    // 再断言「添加条目」按钮不渲染、换成「加载失败」说明
    expect(
      await screen.findByText("权限目录加载失败，请重试；移除不受影响。"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "添加条目" })).toBeNull();

    // 重试入口真实可用：失败一次后点重试，目录重新拉到，添加入口回来
    failed = false;
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => {
      expect(callsTo(calls, PERMISSIONS_PATH).length).toBeGreaterThanOrEqual(2);
    });
    await screen.findByRole("button", { name: "添加条目" });
  });

  it("收到 session-refreshed 事件后把 access 前缀查询一起作废重拉", async () => {
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    renderPage();

    await screen.findByRole("list", { name: "权限组列表" });
    const listGets = () =>
      callsTo(calls, LIST_PATH).filter((call) => call.method === "GET").length;
    // 等详情与候选目录都加载完再派发事件：`invalidateQueries` 只重拉**活跃**查询，
    // 事件若在详情（及其挂载的 ItemPanel）就绪前到达，候选目录查询还没挂载、
    // 不会被这次失效覆盖——那测的就不是「整块回读」了。
    await waitFor(() => {
      expect(detailCalls(calls).length).toBe(1);
    });
    await waitFor(() => {
      expect(callsTo(calls, PERMISSIONS_PATH).length).toBe(1);
    });
    expect(listGets()).toBe(1);

    // 别处对当前账号的授权改动（如权限授予）要刷新会话才生效：
    // 事件一到，页面把列表、详情、候选目录整块作废重拉。
    window.dispatchEvent(new CustomEvent(SESSION_REFRESHED_EVENT));

    await waitFor(() => {
      expect(listGets()).toBeGreaterThanOrEqual(2);
    });
    await waitFor(() => {
      expect(detailCalls(calls).length).toBeGreaterThanOrEqual(2);
    });
    await waitFor(() => {
      expect(callsTo(calls, PERMISSIONS_PATH).length).toBeGreaterThanOrEqual(2);
    });
  });
});
