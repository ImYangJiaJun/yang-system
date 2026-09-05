import { describe, expect, it } from "vitest";

import type { TableViewSchema } from "@/contracts/ui-catalog";
import {
  buildListActionValues,
  hasActiveQuery,
  initialTableQueryState,
} from "@/renderers/table/table-query";

function view(overrides: Partial<TableViewSchema> = {}): TableViewSchema {
  return {
    view_id: "demo.items.main",
    title: "项目",
    table: "demo_items",
    data_action: "demo.items.list",
    columns: [
      {
        field: "name",
        title: "名称",
        description: "",
        widget: "text",
        required: false,
        searchable: true,
        filterable: true,
        sortable: true,
      },
    ],
    form: { fields: [] },
    query: {
      search_fields: ["name"],
      filter_fields: ["name"],
      default_sort: [{ field: "name", direction: "asc" }],
      default_page_size: 10,
      max_page_size: 100,
    },
    actions: [],
    action_presentations: [],
    ...overrides,
  };
}

describe("表格查询参数契约（对齐旧 useTableQuery）", () => {
  it("默认状态构造 page/page_size/search/where/order_by/count_total", () => {
    const v = view();
    expect(buildListActionValues(v, initialTableQueryState(v))).toEqual({
      page: 1,
      page_size: 10,
      search: null,
      where: undefined,
      order_by: [{ field: "name", direction: "Asc" }],
      count_total: true,
    });
  });

  it("搜索与筛选进入 where/search，方向枚举映射为 Asc/Desc", () => {
    const v = view();
    const state = {
      ...initialTableQueryState(v),
      page: 3,
      pageSize: 50,
      search: " 渲染器 ",
      filters: { name: { operator: "contains" as const, value: "渲染器" } },
      orderBy: [{ field: "name", direction: "desc" as const }],
    };
    expect(buildListActionValues(v, state)).toEqual({
      page: 3,
      page_size: 50,
      search: "渲染器",
      where: { type: "like", field: "name", pattern: "%渲染器%" },
      order_by: [{ field: "name", direction: "Desc" }],
      count_total: true,
    });
  });

  it("树视图无查询条件时整树拉取，有查询条件时回退分页", () => {
    const v = view({
      tree: {
        id_field: "id",
        parent_field: "parent_id",
        label_field: "name",
        max_nodes: 100,
      },
    });
    const base = initialTableQueryState(v);
    expect(hasActiveQuery(base)).toBe(false);
    expect(buildListActionValues(v, base)).toMatchObject({
      page: 1,
      page_size: 100,
    });
    const searching = { ...base, search: "关键字" };
    expect(hasActiveQuery(searching)).toBe(true);
    expect(buildListActionValues(v, searching)).toMatchObject({
      page: 1,
      page_size: 10,
    });
  });
});
