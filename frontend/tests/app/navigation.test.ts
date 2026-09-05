import { describe, expect, it } from "vitest";

import type { TableViewSchema, UiCatalog } from "@/engine/contracts/ui-catalog";
import {
  buildNavigationPages,
  groupNavigationPages,
  WORKSPACE_IDENTITY,
} from "@/app/navigation";

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

function catalog(overrides: Partial<UiCatalog> = {}): UiCatalog {
  return {
    schema_version: "2.3",
    revision: "a".repeat(64),
    actions: [],
    table_views: [],
    modules: [],
    ...overrides,
  };
}

describe("导航投影", () => {
  it("未分配给 Module 的视图合成工作台分组下的单视图模块页", () => {
    const doc = catalog({
      table_views: [view("demo.items.main", "demo.items.list")],
    });

    const pages = buildNavigationPages(doc);

    expect(pages).toHaveLength(1);
    expect(pages[0]).toMatchObject({
      id: "demo.items.main",
      identity: WORKSPACE_IDENTITY,
      views: [expect.objectContaining({ view_id: "demo.items.main" })],
    });
    const groups = groupNavigationPages(pages, doc);
    expect(groups).toHaveLength(1);
    expect(groups[0]?.title).toBe("工作台");
  });

  it("已分配的视图不重复出现在工作台", () => {
    const doc = catalog({
      table_views: [view("account.user.main", "account.user.list")],
      modules: [
        {
          module_id: "account.user",
          identity: { id: "user", title: "账号", icon: "person", order: 10 },
          title: "账号",
          description: "",
          icon: "account",
          order: 10,
          actions: [],
          action_presentations: [],
          views: ["account.user.main"],
        },
      ],
    });

    const pages = buildNavigationPages(doc);

    expect(pages.map((page) => page.id)).toEqual(["account.user"]);
    const groups = groupNavigationPages(pages, doc);
    expect(groups[0]?.title).toBe("账号");
  });
});
