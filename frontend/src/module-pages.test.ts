import { describe, expect, it } from "vitest";
import type {
  ActionDemoSchema,
  TableViewSchema,
  UiCatalog,
} from "src/contracts/ui-catalog";
import { buildAccountModulePages } from "./module-pages";

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
    schema_version: "2.2",
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
    ).toHaveLength(2);
    expect(pages.find((page) => page.id === "org.user")?.views).toHaveLength(1);
  });

  it("Action-only Module 仍然是页面，而不是空账号空间", () => {
    const pages = buildAccountModulePages({
      ...catalog(),
      actions: [action("admin.user.list")],
      table_views: [],
    });

    expect(pages).toHaveLength(1);
    expect(pages[0]).toMatchObject({
      id: "admin.user",
      identity: "admin",
      title: "平台账号",
    });
  });
});
