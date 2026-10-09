/**
 * 权限展示元数据的纯函数映射：中文名取自声明 Action、模块名取自 catalog.modules
 * （identity 兜底）、分组按模块前缀、命不中回退权限字符串本身。
 */

import { describe, expect, it } from "vitest";

import type { UiCatalog } from "@/engine";
import type { PermissionCatalogEntry } from "@/features/access/api";
import {
  buildPermissionMeta,
  groupPermissionMeta,
} from "@/features/access/permission-meta";

function modulePresentation(
  moduleId: string,
  title: string,
  identityId = "user",
): UiCatalog["modules"][number] {
  return {
    module_id: moduleId,
    identity: {
      id: identityId,
      title: identityId === "user" ? "个人账户" : "身份名",
      icon: "person",
      order: 1,
    },
    title,
    description: "",
    icon: "box",
    order: 1,
    primary_action: null,
    actions: [],
    action_presentations: [],
    views: [],
  };
}

function catalogWith(modules: UiCatalog["modules"]): UiCatalog {
  return {
    schema_version: "2.3",
    revision: "a".repeat(64),
    actions: [
      {
        operation_id: "access.grants.list_permissions",
        title: "权限目录",
        description: "查询全部 Action 声明的权限集合",
        method: "GET",
        path: "/api/v1/access/permissions",
        params: [],
        input_schema: {},
        output_schema: {},
        request_media_type: "json",
        response_kind: "json",
        requires_auth: true,
      },
      {
        operation_id: "account.user.list_users",
        title: "用户列表",
        description: "管理端分页列出用户",
        method: "GET",
        path: "/api/v1/users",
        params: [],
        input_schema: {},
        output_schema: {},
        request_media_type: "json",
        response_kind: "json",
        requires_auth: true,
      },
    ],
    table_views: [],
    modules,
  };
}

const CATALOG = catalogWith([
  modulePresentation("access.grants", "权限管理"),
  modulePresentation("feishu.datasource", "飞书数据源"),
  modulePresentation("account.user", "个人账户"),
]);

/// 权限目录条目（对齐 `list_permissions.rs` 的 PermissionEntry + 新增 reason）。
const ENTRIES: PermissionCatalogEntry[] = [
  {
    permission: "access.grants.read",
    declaredBy: [
      "access.grants.list_permissions",
      "access.grants.list_user_grants",
    ],
    adminEquivalent: false,
    reason: null,
  },
  {
    permission: "access.grants.write",
    declaredBy: [],
    adminEquivalent: true,
    reason: "授予后可以修改任何用户的权限",
  },
  {
    // 模块前缀 account.users 与目录里的模块 account.user 不同源：模块名命不中，
    // 展示回退到权限字符串，分组回退到前缀
    permission: "account.users.read",
    declaredBy: ["account.user.list_users"],
    adminEquivalent: false,
    reason: null,
  },
  {
    permission: "feishu.datasource.read",
    declaredBy: ["feishu.datasource.list_datasources"],
    adminEquivalent: false,
    reason: null,
  },
];

describe("buildPermissionMeta 条目映射", () => {
  it("中文名取第一条声明 Action 的 title，模块名取 module_id 命中的 title", () => {
    const meta = buildPermissionMeta(ENTRIES, CATALOG);

    const read = meta.get("access.grants.read");
    expect(read).toBeDefined();
    expect(read?.title).toBe("权限目录");
    expect(read?.description).toBe("查询全部 Action 声明的权限集合");
    expect(read?.moduleTitle).toBe("权限管理");
    expect(read?.modulePrefix).toBe("access.grants");
    expect(read?.declaredBy).toEqual([
      "access.grants.list_permissions",
      "access.grants.list_user_grants",
    ]);
    expect(read?.adminEquivalent).toBe(false);
    expect(read?.reason).toBeNull();
  });

  it("没有可用的声明 Action 时回退权限字符串本身", () => {
    const meta = buildPermissionMeta(ENTRIES, CATALOG);

    // declaredBy 空：没有任何 Action 可借
    const write = meta.get("access.grants.write");
    expect(write?.title).toBe("access.grants.write");
    expect(write?.description).toBeUndefined();
    expect(write?.moduleTitle).toBe("权限管理");
    expect(write?.adminEquivalent).toBe(true);
    expect(write?.reason).toBe("授予后可以修改任何用户的权限");

    // declaredBy 有值但目录里不存在：同样回退
    const datasource = meta.get("feishu.datasource.read");
    expect(datasource?.title).toBe("feishu.datasource.read");
    expect(datasource?.moduleTitle).toBe("飞书数据源");
  });

  it("模块前缀命不中 module_id 时回退 identity.title，都命不中则为 undefined", () => {
    const meta = buildPermissionMeta(ENTRIES, CATALOG);

    // account.users 对不上 account.user：identity.id 也不叫 account.users → undefined
    const account = meta.get("account.users.read");
    expect(account?.moduleTitle).toBeUndefined();

    // identity 兜底：前缀与 identity.id 相同时用身份标题
    const identityHit = buildPermissionMeta(
      [
        {
          permission: "user.read",
          declaredBy: [],
          adminEquivalent: false,
          reason: null,
        },
      ],
      CATALOG,
    ).get("user.read");
    expect(identityHit?.moduleTitle).toBe("个人账户");
  });

  it("以权限字符串为键，条目可直查", () => {
    const meta = buildPermissionMeta(ENTRIES, CATALOG);
    expect([...meta.keys()]).toEqual([
      "access.grants.read",
      "access.grants.write",
      "account.users.read",
      "feishu.datasource.read",
    ]);
  });

  it("空目录/空条目都返回空 Map", () => {
    expect(buildPermissionMeta([], CATALOG).size).toBe(0);
    expect(buildPermissionMeta(ENTRIES, undefined).size).toBe(ENTRIES.length);
  });
});

describe("groupPermissionMeta 分组", () => {
  it("按 moduleTitle 分组，命不中的组回退到模块前缀，保持目录顺序", () => {
    const meta = buildPermissionMeta(ENTRIES, CATALOG);
    const groups = groupPermissionMeta([...meta.values()]);

    expect(groups.map((group) => group.title)).toEqual([
      "权限管理",
      "account.users",
      "飞书数据源",
    ]);
    expect(groups[0].permissions.map((item) => item.permission)).toEqual([
      "access.grants.read",
      "access.grants.write",
    ]);
    expect(groups[1].permissions.map((item) => item.permission)).toEqual([
      "account.users.read",
    ]);
  });

  it("空列表返回空分组", () => {
    expect(groupPermissionMeta([])).toEqual([]);
  });
});
