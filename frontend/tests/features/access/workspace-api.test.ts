/**
 * 权限工作台数据层契约：参数落位逐字对齐目录声明、响应解析 fail-closed
 * （丢行不猜值）、reason 缺失兼容。
 *
 * `lookup` 与 `list_holders` 是钉住的契约（后端并行开发中），这里的替身目录
 * **写字面量不写常量**：常量本身拼错一个字母时两边一起错，测试照样绿——
 * 而真实后果是 `requireGroupAction` 抛「目录里找不到 Action」。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import type { ActionDemoSchema, UiCatalog } from "@/engine";
import { listPermissions } from "@/features/access/api";
import {
  accessWorkspaceQueryKeys,
  fetchAllUserPages,
  listHolders,
  listUserGrants,
  listUserLookup,
  WORKSPACE_OPERATION_IDS,
} from "@/features/access/workspace-api";

type ParamSource = "body" | "query" | "path" | "header";

function param(
  name: string,
  source: ParamSource,
  required = false,
): {
  name: string;
  source: ParamSource;
  required: boolean;
  title: string;
  description: string;
} {
  return { name, source, required, title: name, description: "" };
}

function action(
  operationId: string,
  method: string,
  path: string,
  params: ReturnType<typeof param>[] = [],
  title?: string,
): ActionDemoSchema {
  return {
    operation_id: operationId,
    title: title ?? operationId,
    description: "",
    method: method as ActionDemoSchema["method"],
    path,
    params,
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

/// 部署里真实存在/钉住的那一套：三个工作台 Action + 权限目录读接口。
const DEPLOYED_ACTIONS: ActionDemoSchema[] = [
  action("account.user.lookup", "GET", "/api/v1/users", [
    param("q", "query"),
    param("page", "query"),
    param("page_size", "query"),
  ]),
  action(
    "access.grants.list_user_grants",
    "GET",
    "/api/v1/access/users/{user_id}/grants",
    [param("user_id", "path", true)],
  ),
  action(
    "access.grants.list_holders",
    "GET",
    "/api/v1/access/permissions/holders",
    [param("permission", "query", true)],
  ),
  action("access.grants.list_permissions", "GET", "/api/v1/access/permissions"),
];

function catalogWith(
  actions: ActionDemoSchema[] = DEPLOYED_ACTIONS,
): UiCatalog {
  return {
    schema_version: "2.3",
    revision: "a".repeat(64),
    actions,
    table_views: [],
    modules: [],
  };
}

const deps = { catalog: catalogWith(), session: { token: "tok-1" } };

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/// 字面信封：引擎要求 `code: 0`，裸 payload 会被当成业务失败（ApiError: HTTP 200）。
function envelope(data: unknown) {
  return jsonResponse({ code: 0, message: "成功", data });
}

/// stub fetch，并把每次请求的 url + method 记下来（同一 data 每次调用都回）。
function stubFetch(data: unknown) {
  const calls: Array<{ url: string; method: string }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      calls.push({ url, method: init?.method ?? "GET" });
      return Promise.resolve(envelope(data));
    }),
  );
  return calls;
}

/// 逐次返回不同 data 的 stub（翻页测试用）：调用次数超过 data 数时重复最后一个。
function stubFetchSequence(payloads: unknown[]) {
  const calls: Array<{ url: string }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      calls.push({ url });
      const payload = payloads[Math.min(calls.length - 1, payloads.length - 1)];
      return Promise.resolve(envelope(payload));
    }),
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("listUserLookup 用户查找", () => {
  it("q/page/page_size 按目录声明放进查询串，响应 users 逐行解析", async () => {
    const calls = stubFetch({
      users: [
        {
          id: 7,
          username: "alice",
          email: "alice@example.com",
          status: "active",
        },
        { id: 8, username: "bob", email: null, status: "disabled" },
        { id: 9, username: "carol", email: "", status: "deleted" },
        // 没有 id 的行定位不了，必须被丢
        { username: "ghost", email: "ghost@example.com", status: "active" },
      ],
    });

    const users = await listUserLookup(deps, "ali", undefined);

    expect(calls).toHaveLength(1);
    const url = new URL(calls[0].url, "http://test");
    expect(url.pathname).toBe("/api/v1/users");
    expect(url.searchParams.get("q")).toBe("ali");
    expect(url.searchParams.get("page")).toBe("1");
    expect(url.searchParams.get("page_size")).toBe("50");
    expect(calls[0].method).toBe("GET");

    expect(users).toEqual([
      {
        id: 7,
        username: "alice",
        email: "alice@example.com",
        status: "active",
      },
      { id: 8, username: "bob", email: null, status: "disabled" },
      // 空串 email 折成 null（与「有值但不可见」的空白格同一纪律）
      { id: 9, username: "carol", email: null, status: "deleted" },
    ]);
  });

  it("目录里找不到 lookup 时抛明确错误，不静默发请求", async () => {
    const calls = stubFetch({ users: [] });
    const missing = {
      catalog: catalogWith([
        action(
          "access.grants.list_permissions",
          "GET",
          "/api/v1/access/permissions",
        ),
      ]),
      session: { token: "tok-1" },
    };
    await expect(listUserLookup(missing, "ali", undefined)).rejects.toThrow(
      "UI 目录里找不到 Action",
    );
    expect(calls).toHaveLength(0);
  });
});

describe("fetchAllUserPages 全量翻页", () => {
  it("按 50 一页逐页取，不足一页即收尾", async () => {
    const fullPage = Array.from({ length: 50 }, (_, index) => ({
      id: index + 1,
      username: `u${index + 1}`,
      email: null,
      status: "active",
    }));
    const calls = stubFetchSequence([
      { users: fullPage },
      { users: [{ id: 51, username: "u51", email: null, status: "active" }] },
    ]);

    const users = await fetchAllUserPages(deps, undefined);

    expect(users).toHaveLength(51);
    expect(users[50]).toEqual({
      id: 51,
      username: "u51",
      email: null,
      status: "active",
    });
    expect(calls).toHaveLength(2);
    expect(new URL(calls[0].url, "http://test").searchParams.get("page")).toBe(
      "1",
    );
    expect(new URL(calls[1].url, "http://test").searchParams.get("page")).toBe(
      "2",
    );
    // 满页后多打一页空页收尾：不猜总数，靠「不足一页」判终
    expect(
      new URL(calls[1].url, "http://test").searchParams.get("page_size"),
    ).toBe("50");
  });
});

describe("listUserGrants 用户直授列表", () => {
  it("user_id 走路径，响应 grants 逐行解析（永久 null、过期标记、丢无 id 行）", async () => {
    const calls = stubFetch({
      user_id: 7,
      grants: [
        {
          id: 1,
          permission: "access.grants.read",
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: null,
          expired: false,
        },
        {
          id: 2,
          permission: "access.grants.write",
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: 1_700_000_000,
          expired: true,
        },
        // 缺 id：撤销定位不了，丢
        {
          permission: "demo.notes.read",
          granted_by: 9,
          occurred_at: 0,
          expires_at: null,
          expired: false,
        },
      ],
    });

    const result = await listUserGrants(7, deps, undefined);

    expect(calls).toHaveLength(1);
    const url = new URL(calls[0].url, "http://test");
    expect(url.pathname).toBe("/api/v1/access/users/7/grants");
    expect(url.search).toBe("");

    expect(result).toEqual({
      userId: 7,
      grants: [
        {
          id: 1,
          permission: "access.grants.read",
          grantedBy: 9,
          occurredAt: 1_690_000_000,
          expiresAt: null,
          expired: false,
        },
        {
          id: 2,
          permission: "access.grants.write",
          grantedBy: 9,
          occurredAt: 1_690_000_000,
          expiresAt: 1_700_000_000,
          expired: true,
        },
      ],
    });
  });

  it("响应缺 user_id 时按请求目标回填", async () => {
    stubFetch({ grants: [] });
    const result = await listUserGrants(7, deps, undefined);
    expect(result.userId).toBe(7);
  });
});

describe("listHolders 权限持有者", () => {
  it("direct/groups 两段形状分别解析，缺身份的直授行与缺 key 的组被丢", async () => {
    const calls = stubFetch({
      direct: [
        {
          user_id: 7,
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: null,
          expired: false,
        },
        {
          user_id: 8,
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: 1_700_000_000,
          expired: true,
        },
        { granted_by: 9, occurred_at: 0, expires_at: null, expired: false },
      ],
      groups: [
        { id: 3, group_key: "ops", title: "运维", member_count: 4 },
        { id: 4, group_key: "", title: "无名组", member_count: 1 },
      ],
    });

    const result = await listHolders("access.grants.write", deps, undefined);

    expect(calls).toHaveLength(1);
    const url = new URL(calls[0].url, "http://test");
    expect(url.pathname).toBe("/api/v1/access/permissions/holders");
    expect(url.searchParams.get("permission")).toBe("access.grants.write");

    expect(result).toEqual({
      direct: [
        {
          user_id: 7,
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: null,
          expired: false,
        },
        {
          user_id: 8,
          granted_by: 9,
          occurred_at: 1_690_000_000,
          expires_at: 1_700_000_000,
          expired: true,
        },
      ],
      groups: [{ id: 3, group_key: "ops", title: "运维", member_count: 4 }],
    });
  });

  it("响应缺 direct/groups 段时按空数组处理", async () => {
    stubFetch({});
    const result = await listHolders("access.grants.read", deps, undefined);
    expect(result).toEqual({ direct: [], groups: [] });
  });
});

describe("listPermissions reason 字段", () => {
  it("有 reason 解析为字符串，空串与缺失一律折成 null（老 mock 载荷兼容）", async () => {
    stubFetch({
      permissions: [
        {
          permission: "access.grants.write",
          declared_by: ["access.grants.grant_permission"],
          admin_equivalent: true,
          reason: "授予后可以修改任何用户的权限",
        },
        // 老载荷：没有 reason 字段
        {
          permission: "access.grants.read",
          declared_by: ["access.grants.list_permissions"],
          admin_equivalent: false,
        },
        // 空串也算没有理由
        {
          permission: "account.users.read",
          declared_by: ["account.user.list_users"],
          admin_equivalent: false,
          reason: "",
        },
      ],
    });

    const entries = await listPermissions(deps, undefined);

    expect(entries).toEqual([
      {
        permission: "access.grants.write",
        declaredBy: ["access.grants.grant_permission"],
        adminEquivalent: true,
        reason: "授予后可以修改任何用户的权限",
      },
      {
        permission: "access.grants.read",
        declaredBy: ["access.grants.list_permissions"],
        adminEquivalent: false,
        reason: null,
      },
      {
        permission: "account.users.read",
        declaredBy: ["account.user.list_users"],
        adminEquivalent: false,
        reason: null,
      },
    ]);
  });
});

describe("accessWorkspaceQueryKeys", () => {
  it("首段是 access，工作台各查询键挂在同一前缀下（随权限组页一次失效）", () => {
    expect(accessWorkspaceQueryKeys.userDirectory()[0]).toBe("access");
    expect(accessWorkspaceQueryKeys.userLookup("ali")).toEqual([
      "access",
      "user-lookup",
      "ali",
    ]);
    expect(accessWorkspaceQueryKeys.userGrants(7)).toEqual([
      "access",
      "user-grants",
      7,
    ]);
    expect(accessWorkspaceQueryKeys.holders("access.grants.read")).toEqual([
      "access",
      "holders",
      "access.grants.read",
    ]);
  });

  it("常量与钉住的契约逐字一致", () => {
    expect(WORKSPACE_OPERATION_IDS).toEqual({
      lookup: "account.user.lookup",
      listUserGrants: "access.grants.list_user_grants",
      listHolders: "access.grants.list_holders",
      grantPermission: "access.grants.grant_permission",
      revokePermission: "access.grants.revoke_permission",
    });
  });
});
