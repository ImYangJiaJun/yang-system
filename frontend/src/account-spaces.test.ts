import { describe, expect, it } from "vitest";
import type {
  ActionDemoSchema,
  TableViewSchema,
  UiCatalog,
} from "src/contracts/ui-catalog";
import {
  summarizeAccountSpaces,
  unassignedViews,
  visibleAccountSpaces,
} from "./account-spaces";

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

function catalog(
  actions: ActionDemoSchema[],
  tableViews: TableViewSchema[],
): UiCatalog {
  return {
    schema_version: "2.2",
    revision: "a".repeat(64),
    actions,
    table_views: tableViews,
  };
}

describe("account spaces", () => {
  it("个人账户始终可见，管理和企业空间由服务端目录授权决定", () => {
    expect(visibleAccountSpaces(undefined).map((space) => space.id)).toEqual([
      "user",
    ]);

    const summaries = visibleAccountSpaces(
      catalog(
        [action("account.user.me"), action("admin.user.list")],
        [view("org.user.list", "org.user.select")],
      ),
    );
    expect(summaries.map((space) => space.id)).toEqual([
      "user",
      "admin",
      "org",
    ]);
    expect(
      summaries.find((space) => space.id === "admin")?.actions,
    ).toHaveLength(1);
    expect(summaries.find((space) => space.id === "org")?.views).toHaveLength(
      1,
    );
  });

  it("不会把账号空间 View 重复放入通用业务入口", () => {
    const current = catalog(
      [],
      [
        view("org.user.list", "org.user.select"),
        view("demo.items.main", "demo.items.select"),
      ],
    );

    expect(unassignedViews(current).map((item) => item.view_id)).toEqual([
      "demo.items.main",
    ]);
    expect(
      summarizeAccountSpaces(current).find((space) => space.id === "org")
        ?.views,
    ).toHaveLength(1);
  });
});
