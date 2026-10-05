/**
 * 权限工作台（走真实路由 `/access/workspace` 与旧 URL `/access/groups`，
 * 含 lazy 的 `.default` 约定与真实 api.ts / workspace-api.ts）。
 *
 * 桩只替换 `fetch`：权限按身份投影这一点用目录本身模拟——不给某粒 Action，
 * 就等于这个身份没有那粒权限位。
 *
 * 这里断言的是**只有页面能证明的事**：选中用户后三块总览（直授分组渲染与
 * 危险/过期/永久/临期徽标、所属组、有效并集剔除孤儿与过期）、授予弹窗提交
 * body 对齐 grant_permission 契约（user_id 数字 + permission，永久时省略
 * expires_at）、撤销走确认后真的发出 revoke、按功能视图目录下钻持有者两张表、
 * 按组视图添加条目多选逐条 add_group_item、成员 UserPicker 添加的 body 对齐
 * add_group_member 契约。
 */

import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

await import("@/features/access/views/PermissionWorkspacePage");

const LOOKUP_PATH = "/api/v1/users/lookup";
const PERMISSIONS_PATH = "/api/v1/access/permissions";
const GRANT_PATH = "/api/v1/access/grants";
const REVOKE_PATH = `${GRANT_PATH}/revoke`;
const HOLDERS_PATH = "/api/v1/access/grants/holders";
const USER_GRANTS_PATH = /\/api\/v1\/access\/users\/\d+\/grants$/;
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

function jsonResponse(payload: unknown, status = 200): Response {
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
  title = operationId,
) {
  return {
    operation_id: operationId,
    title,
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

const GROUP_READ_ACTIONS = [
  action("access.groups.list_groups", "GET", LIST_PATH),
  action("access.groups.get_group", "GET", `${LIST_PATH}/{group_id}`, [
    param("group_id", "path"),
  ]),
];

const GROUP_WRITE_ACTIONS = [
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

/// 工作台五个读/写 Action（lookup 在 account 模块，其余在 access.grants 模块）。
const WORKSPACE_ACTIONS = [
  action(
    "account.users.lookup",
    "GET",
    LOOKUP_PATH,
    [
      param("q", "query", false),
      param("page", "query", false),
      param("page_size", "query", false),
    ],
    "用户查找",
  ),
  action(
    "access.grants.list_permissions",
    "GET",
    PERMISSIONS_PATH,
    [],
    "权限目录",
  ),
  // 声明者（permission-meta 的展示名来源）；测试只借它的标题，不会真的发请求
  action(
    "account.users.reset_credentials",
    "POST",
    "/api/v1/users/reset-credentials",
    [param("user_id", "body")],
    "重置凭据",
  ),
  action("access.grants.list_holders", "GET", HOLDERS_PATH, [
    param("permission", "query"),
  ]),
  action(
    "access.grants.list_user_grants",
    "GET",
    "/api/v1/access/users/{user_id}/grants",
    [param("user_id", "path")],
  ),
  action("access.grants.grant_permission", "POST", GRANT_PATH, [
    param("user_id", "body"),
    param("permission", "body"),
    param("expires_at", "body", false),
  ]),
  action("access.grants.revoke_permission", "POST", REVOKE_PATH, [
    param("user_id", "body"),
    param("permission", "body"),
  ]),
];

/// 权限目录（权限展示元数据与授予候选的来源）。
/// `declared_by` 各指向目录里带独立标题的 Action，展示名才不会撞车
/// （reset_credentials 与 account.users.read 同模块，但声明者不同）。
const PERMISSIONS = [
  {
    permission: "access.grants.read",
    declared_by: ["access.grants.list_permissions"],
    admin_equivalent: false,
  },
  {
    permission: "account.users.read",
    declared_by: ["account.users.lookup"],
    admin_equivalent: false,
  },
  {
    permission: "account.users.reset_credentials",
    declared_by: ["account.users.reset_credentials"],
    admin_equivalent: true,
    reason: "可重置任意账号的凭据",
  },
];

function catalogFor(): Response {
  return jsonResponse({
    code: 0,
    message: "成功",
    data: {
      schema_version: "2.3",
      // revision 必须是 64 位十六进制：引擎的 CatalogCache.accept 在 revision
      // 相同会直接复用上一份目录对象（同文件内多个用例会串味）。
      revision: `1${"b".repeat(63)}`,
      actions: [
        ...WORKSPACE_ACTIONS,
        ...GROUP_READ_ACTIONS,
        ...GROUP_WRITE_ACTIONS,
      ],
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

function groupWire(overrides: Record<string, unknown> = {}) {
  return {
    id: 2,
    group_key: "ops",
    title: "运维",
    description: null,
    member_count: 0,
    item_count: 0,
    is_builtin: false,
    orphan_item_count: 0,
    can_manage: true,
    ...overrides,
  };
}

function groupDetailWire(overrides: Record<string, unknown> = {}) {
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

type StubOptions = {
  lookup?: Handler;
  userGrants?: Handler;
  holders?: Handler;
  grant?: Handler;
  revoke?: Handler;
  groupList?: Handler;
  groupDetail?: Handler;
  addItem?: Handler;
  removeItem?: Handler;
  addMember?: Handler;
  removeMember?: Handler;
};

async function respond(
  handler: Handler | undefined,
  body: Record<string, unknown>,
  fallback: unknown,
): Promise<Response> {
  const value = handler ? await handler(body) : fallback;
  return value instanceof Response ? value : envelope(value);
}

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
      const search = new URL(url, "http://localhost").searchParams;

      if (url.endsWith("/.well-known/yang/ui-catalog")) return catalogFor();
      if (url.endsWith("/api/v1/users/me")) return envelope(ME);
      if (url.includes(LOOKUP_PATH)) {
        // 目录全量拉取（q 为空）返回全部用户；搜索词命中用户名/邮箱则返回该行。
        const q = search.get("q") ?? "";
        const users = [
          {
            id: 7,
            username: "alice",
            email: "alice@example.com",
            status: "active",
          },
          {
            id: 8,
            username: "bob",
            email: "bob@example.com",
            status: "active",
          },
        ];
        return respond(
          options.lookup,
          { q },
          q === ""
            ? { users }
            : {
                users: users.filter(
                  (user) => user.username.includes(q) || user.email.includes(q),
                ),
              },
        );
      }
      if (url.endsWith(PERMISSIONS_PATH))
        return respond(options.grant ?? undefined, payload, {
          permissions: PERMISSIONS,
        });
      if (url.includes(HOLDERS_PATH)) {
        const permission = search.get("permission") ?? "";
        return respond(
          options.holders,
          { permission },
          {
            direct: [],
            groups: [],
          },
        );
      }
      if (USER_GRANTS_PATH.test(url)) {
        const userId = Number(url.match(/\/users\/(\d+)\/grants$/)?.[1]);
        return respond(
          options.userGrants,
          { user_id: userId },
          {
            user_id: userId,
            grants: [],
          },
        );
      }
      if (url.endsWith(REVOKE_PATH))
        return respond(options.revoke, payload, {
          user_id: payload.user_id,
          permission: payload.permission,
          changed: true,
        });
      if (url.endsWith(GRANT_PATH) && method === "POST")
        return respond(options.grant, payload, {
          user_id: payload.user_id,
          permission: payload.permission,
          changed: true,
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
      if (DETAIL_PATH.test(url)) {
        const detailId = Number(url.slice(url.lastIndexOf("/") + 1));
        return respond(
          options.groupDetail,
          { group_id: detailId },
          groupDetailWire({ id: detailId }),
        );
      }
      if (url.endsWith(LIST_PATH))
        return respond(options.groupList, payload, { groups: [] });
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
    }),
  );

  return calls;
}

function callsTo(calls: RecordedCall[], path: string) {
  return calls.filter((call) => call.url.endsWith(path));
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

async function pickUser(
  user: ReturnType<typeof userEvent.setup>,
  name: string,
) {
  // 目录加载完成前页面还是骨架：先等搜索框出现（首个异步等待点）
  await user.type(await screen.findByLabelText("搜索用户"), name);
  await user.click(
    await screen.findByRole("button", { name: new RegExp(name) }),
  );
}

/* ------------------------------- 视图一：按用户 ------------------------------- */

describe("权限工作台·按用户视图", () => {
  it("选中用户后渲染直授分组与徽标、所属权限组、有效并集；撤销走确认发出 revoke", async () => {
    const user = userEvent.setup();
    const now = Math.floor(Date.now() / 1000);
    const calls = stubAccessApi({
      userGrants: () => ({
        user_id: 7,
        grants: [
          {
            id: 1,
            permission: "access.grants.read",
            granted_by: 1,
            occurred_at: now - 86400 * 30,
            expires_at: null,
            expired: false,
          },
          {
            id: 2,
            permission: "account.users.reset_credentials",
            granted_by: 7,
            occurred_at: now - 86400 * 5,
            expires_at: now - 86400,
            expired: true,
          },
          {
            id: 3,
            permission: "account.users.read",
            granted_by: 1,
            occurred_at: now - 86400 * 2,
            expires_at: now + 86400 * 3,
            expired: false,
          },
        ],
      }),
      groupList: () => ({
        groups: [
          groupWire({
            id: 2,
            member_count: 1,
            item_count: 2,
          }),
        ],
      }),
      groupDetail: () => ({
        ...groupDetailWire({ id: 2 }),
        items: [
          { permission: "account.users.read", is_orphan: false },
          // 孤儿条目：不在权限目录里，并集必须剔除
          { permission: "ghost.perm", is_orphan: true },
        ],
        members: [7],
      }),
    });

    renderTestApp({ path: "/access/workspace", authenticated: true });
    await pickUser(user, "alice");

    // 直授权限分组渲染：三条直授都在，徽标各就各位（危险/已过期/永久/临期）
    const direct = await screen.findByRole("region", { name: "直授权限" });
    expect(within(direct).getByText("access.grants.read")).toBeInTheDocument();
    expect(within(direct).getByText("永久")).toBeInTheDocument();
    const dangerRow = within(direct)
      .getByText("account.users.reset_credentials")
      .closest("li");
    expect(dangerRow).not.toBeNull();
    if (dangerRow === null) return;
    expect(dangerRow).toHaveTextContent("管理员等价");
    expect(dangerRow).toHaveTextContent("已过期");
    expect(within(direct).getByText("3 天后过期")).toBeInTheDocument();
    // 两条直授都是用户 #1 授予的（目录为空时授予人名字回退 #id）
    expect(within(direct).getAllByText(/授予人 #1/).length).toBe(2);

    // 所属权限组：目标用户在 ops 组里
    const memberships = screen.getByRole("list", { name: "所属权限组" });
    expect(within(memberships).getByText("运维")).toBeInTheDocument();
    expect(within(memberships).getByText("ops")).toBeInTheDocument();

    // 有效权限并集：过期行与孤儿条目都剔除，来源徽标标注直授/来自 XX 组
    const union = screen.getByRole("list", { name: "有效权限并集" });
    expect(within(union).getByText("access.grants.read")).toBeInTheDocument();
    const usersReadRow = within(union)
      .getByText("account.users.read")
      .closest("li");
    expect(usersReadRow).not.toBeNull();
    if (usersReadRow === null) return;
    expect(usersReadRow).toHaveTextContent("直授");
    expect(usersReadRow).toHaveTextContent("来自 运维");
    expect(within(union).queryByText("ghost.perm")).toBeNull();
    expect(
      within(union).queryByText("account.users.reset_credentials"),
    ).toBeNull();

    // 撤销走确认：对话框里指认目标（权限 + 用户），确认后真的发出 revoke
    await user.click(
      within(direct).getByRole("button", {
        name: "撤销 account.users.reset_credentials",
      }),
    );
    const confirm = await screen.findByRole("dialog");
    expect(confirm).toHaveTextContent(
      "account.users.reset_credentials（alice）",
    );
    await user.click(within(confirm).getByRole("button", { name: "确认撤销" }));

    await waitFor(() => {
      expect(callsTo(calls, REVOKE_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, REVOKE_PATH)[0]?.body).toEqual({
      user_id: 7,
      permission: "account.users.reset_credentials",
    });
  });

  it("授予新权限弹窗提交对齐 grant_permission 契约；重新授予预选该权限", async () => {
    const user = userEvent.setup();
    const now = Math.floor(Date.now() / 1000);
    const calls = stubAccessApi({
      userGrants: () => ({
        user_id: 7,
        grants: [
          {
            id: 1,
            permission: "access.grants.read",
            granted_by: 1,
            occurred_at: now - 86400,
            expires_at: null,
            expired: false,
          },
        ],
      }),
    });

    renderTestApp({ path: "/access/workspace", authenticated: true });
    await pickUser(user, "alice");

    // 「授予新权限」：直授模式弹窗，勾选一条权限确认
    const direct = await screen.findByRole("region", { name: "直授权限" });
    await user.click(
      within(direct).getByRole("button", { name: "授予新权限" }),
    );
    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("checkbox", {
        name: "权限目录 权限",
      }),
    );
    await user.click(within(dialog).getByRole("button", { name: "确认授予" }));

    // body 对齐契约：user_id 数字 + permission；永久（默认）时 expires_at 省略
    await waitFor(() => {
      expect(callsTo(calls, GRANT_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, GRANT_PATH)[0]?.body).toEqual({
      user_id: 7,
      permission: "access.grants.read",
    });
    expect(
      await screen.findByText("已授予，对方刷新会话后生效"),
    ).toBeInTheDocument();

    // 重新授予：弹窗预选该权限，确认即提交（后端对过期行原地续期）
    await user.click(
      within(direct).getByRole("button", {
        name: "重新授予 access.grants.read",
      }),
    );
    const regrant = await screen.findByRole("dialog");
    expect(
      within(regrant).getByRole("checkbox", {
        name: "权限目录 权限",
      }),
    ).toBeChecked();
    await user.click(within(regrant).getByRole("button", { name: "确认授予" }));

    await waitFor(() => {
      expect(callsTo(calls, GRANT_PATH)).toHaveLength(2);
    });
    expect(callsTo(calls, GRANT_PATH)[1]?.body).toEqual({
      user_id: 7,
      permission: "access.grants.read",
    });
  });
});

/* ------------------------------- 视图二：按功能 ------------------------------- */

describe("权限工作台·按功能视图", () => {
  it("目录浏览下钻：持有者两张表渲染（直授含名字与过期徽标），给用户授权提交 grant", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      holders: () => ({
        direct: [
          {
            user_id: 7,
            granted_by: 1,
            occurred_at: 1_700_000_000,
            expires_at: null,
            expired: false,
          },
        ],
        groups: [{ id: 2, group_key: "ops", title: "运维", member_count: 3 }],
      }),
      groupList: () => ({
        groups: [groupWire({ id: 2, member_count: 3 })],
      }),
    });

    renderTestApp({ path: "/access/workspace", authenticated: true });
    // 目录加载完成前页面还是骨架：先等 tab 出现再切
    await user.click(await screen.findByRole("tab", { name: "按功能" }));

    // 目录浏览：点开一条权限
    await user.click(
      await screen.findByRole("button", {
        name: /权限目录.*access\.grants\.read/,
      }),
    );

    // 基本信息：字符串 + 声明 Action
    expect(
      await screen.findByText("声明于 Action：access.grants.list_permissions"),
    ).toBeInTheDocument();

    // 直授持有者表：名字来自用户目录、授予人、时间、过期徽标
    const directHolders = await screen.findByRole("list", {
      name: "直授持有者",
    });
    expect(within(directHolders).getByText("alice")).toBeInTheDocument();
    expect(within(directHolders).getByText(/授予人 #1/)).toBeInTheDocument();
    expect(within(directHolders).getByText("永久")).toBeInTheDocument();

    // 组持有者表：id/标识/标题/成员数
    const groupHolders = screen.getByRole("list", { name: "组持有者" });
    expect(within(groupHolders).getByText("运维")).toBeInTheDocument();
    expect(within(groupHolders).getByText("ops")).toBeInTheDocument();
    expect(within(groupHolders).getByText("成员 3")).toBeInTheDocument();

    // 给用户授权：目标选 UserPicker，提交对齐 grant_permission 契约
    await user.click(screen.getByRole("button", { name: "给用户 / 组授权" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("搜索用户"), "alice");
    await user.click(
      await within(dialog).findByRole("button", { name: /alice/ }),
    );
    await user.click(within(dialog).getByRole("button", { name: "确认授予" }));

    await waitFor(() => {
      expect(callsTo(calls, GRANT_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, GRANT_PATH)[0]?.body).toEqual({
      user_id: 7,
      permission: "access.grants.read",
    });
  });
});

/* ------------------------------- 视图三：按组 ------------------------------- */

describe("权限工作台·按组视图（旧 URL /access/groups）", () => {
  it("添加条目多选逐条 add_group_item；成员 UserPicker 添加对齐 add_group_member 契约", async () => {
    const user = userEvent.setup();
    const calls = stubAccessApi({
      groupList: () => ({ groups: [groupWire({ id: 2 })] }),
      groupDetail: () => groupDetailWire({ id: 2 }),
    });

    // 旧 URL 无 ?tab= 参数：按路径默认「按组」tab
    renderTestApp({ path: "/access/groups", authenticated: true });

    await screen.findByText(
      "该组还没有任何权限条目，成员加进来也不会得到权限。",
    );

    // 添加条目：多选两条，确认后逐条发 add_group_item
    await user.click(screen.getByRole("button", { name: "添加条目" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("checkbox", {
        name: "权限目录 权限",
      }),
    );
    await user.click(
      within(dialog).getByRole("checkbox", {
        name: "用户查找 权限",
      }),
    );
    await user.click(within(dialog).getByRole("button", { name: "确认加入" }));

    await waitFor(() => {
      expect(callsTo(calls, ITEMS_PATH)).toHaveLength(2);
    });
    expect(callsTo(calls, ITEMS_PATH)[0]?.body).toEqual({
      group_id: 2,
      permission: "access.grants.read",
    });
    expect(callsTo(calls, ITEMS_PATH)[1]?.body).toEqual({
      group_id: 2,
      permission: "account.users.read",
    });

    // 成员：UserPicker 多选添加，body 对齐（group_id + user_id 数字）
    await pickUser(user, "alice");
    await user.click(screen.getByRole("button", { name: "加入所选成员" }));

    await waitFor(() => {
      expect(callsTo(calls, MEMBERS_PATH)).toHaveLength(1);
    });
    expect(callsTo(calls, MEMBERS_PATH)[0]?.body).toEqual({
      group_id: 2,
      user_id: 7,
    });
  });
});
