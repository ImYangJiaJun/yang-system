import { describe, expect, it } from "vitest";
import type {
  ActionDemoSchema,
  TableViewSchema,
  UiCatalog,
} from "src/contracts/ui-catalog";
import { buildAccountModulePages, moduleView } from "./module-pages";

function action(operationId: string): ActionDemoSchema {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method: "GET",
    path: `/api/v1/${operationId.replaceAll(".", "/")}`,
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function view(viewId: string, dataAction: string): TableViewSchema {
  return {
    view_id: viewId,
    title: viewId,
    table: viewId,
    data_action: dataAction,
    columns: [],
    form: { fields: [] },
    query: {
      search_fields: [],
      filter_fields: [],
      default_sort: [],
      default_page_size: 20,
      max_page_size: 100,
    },
    actions: [],
    action_presentations: [],
  };
}

function catalog(): UiCatalog {
  return {
    schema_version: "2.3",
    revision: "d".repeat(64),
    actions: [
      action("account.user.me"),
      action("account.user.logout"),
      action("admin.user.list"),
      action("admin.user.add"),
      action("org.tenant.list"),
      action("org.tenant.create"),
      action("org.org.list"),
      action("org.user.select"),
    ],
    table_views: [view("org.user.list", "org.user.select")],
    modules: [
      module("account.user", "user", {
        primary_action: "account.user.me",
        actions: ["account.user.logout"],
      }),
      module("admin.user", "admin", {
        primary_action: "admin.user.list",
        actions: ["admin.user.add"],
      }),
      module("org.tenant", "org", {
        primary_action: "org.tenant.list",
        actions: ["org.tenant.create"],
        order: 10,
      }),
      module("org.org", "org", {
        primary_action: "org.org.list",
        order: 20,
      }),
      module("org.user", "org", { views: ["org.user.list"], order: 30 }),
    ],
  };
}

function module(
  moduleId: string,
  identity: string,
  overrides: Partial<UiCatalog["modules"][number]> = {},
): UiCatalog["modules"][number] {
  return {
    module_id: moduleId,
    identity: {
      id: identity,
      title: identity,
      icon: identity === "org" ? "organization" : "person",
      order: 10,
    },
    title: moduleId === "admin.user" ? "平台账号" : moduleId,
    description: "",
    icon: "account",
    order: 10,
    actions: [],
    action_presentations: [],
    views: [],
    ...overrides,
  };
}

describe("module pages", () => {
  it("每个账号后端 Module 都生成且只生成一个前端页面", () => {
    const pages = buildAccountModulePages(catalog());

    expect(pages.map((page) => page.id)).toEqual([
      "account.user",
      "admin.user",
      "org.tenant",
      "org.org",
      "org.user",
    ]);
    expect(
      pages.find((page) => page.id === "admin.user")?.actions,
    ).toHaveLength(1);
    expect(
      pages.find((page) => page.id === "admin.user")?.primaryAction
        ?.operation_id,
    ).toBe("admin.user.list");
    expect(pages.find((page) => page.id === "org.user")?.views).toHaveLength(1);
  });

  it("Action-only Module 仍然是页面，而不是空账号空间", () => {
    const pages = buildAccountModulePages({
      ...catalog(),
      actions: [action("admin.user.list")],
      table_views: [],
      modules: [
        module("admin.user", "admin", {
          primary_action: "admin.user.list",
        }),
      ],
    });

    expect(pages).toHaveLength(1);
    expect(pages[0]).toMatchObject({
      id: "admin.user",
      identity: "admin",
      title: "平台账号",
    });
  });

  it("模块 Action 只接受模块显式授权的引用，并合并到当前 View", () => {
    const source = catalog();
    source.actions.push(action("org.user.export"), action("other.action"));
    source.modules = [
      module("org.user", "org", {
        actions: ["org.user.export"],
        action_presentations: [
          {
            operation_id: "org.user.export",
            title: "导出",
            placement: "toolbar",
            interaction: "download",
          },
          {
            operation_id: "other.action",
            title: "越权引用",
            placement: "toolbar",
            interaction: "invoke",
          },
        ],
        views: ["org.user.list"],
      }),
    ];

    const page = buildAccountModulePages(source)[0]!;
    const effectiveView = moduleView(page, "org.user.list");

    expect(page.actionPresentations.map((item) => item.operation_id)).toEqual([
      "org.user.export",
    ]);
    expect(
      effectiveView?.action_presentations.map((item) => item.operation_id),
    ).toEqual(["org.user.export"]);
    expect(effectiveView?.actions).toEqual(["org.user.export"]);
  });
});
