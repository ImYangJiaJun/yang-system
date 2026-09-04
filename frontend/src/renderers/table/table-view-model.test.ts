import { describe, expect, it } from "vitest";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
} from "@/contracts/ui-catalog";
import {
  buildActionInitialValues,
  resolveDisplayRows,
  buildWhereClause,
  createTableFilters,
  flattenDisplayRows,
  formatCell,
  groupPresentedActions,
  pageSizeOptions,
} from "./table-view-model";

const action: ActionDemoSchema = {
  operation_id: "demo.items.edit",
  title: "编辑",
  description: "",
  method: "POST",
  path: "/api/v1/items/edit",
  params: [],
  input_schema: {
    type: "object",
    properties: {
      id: { type: "integer" },
      data: {
        type: "object",
        properties: { name: { type: "string" } },
      },
    },
    required: ["data"],
  },
  output_schema: {},
  request_media_type: "json",
  response_kind: "json",
  requires_auth: true,
};

const field = (name: string, writeOnly = false): FormFieldSchema => ({
  field: name,
  title: name,
  description: "",
  widget: "text",
  required: false,
  read_only: false,
  write_only: writeOnly,
});

describe("table view model", () => {
  it("稳定展平树行并保留深度和路径键", () => {
    expect(
      flattenDisplayRows([
        { id: 1, children: [{ id: 2 }, { id: 3 }] },
        { id: 4 },
      ]).map(({ data, depth, key }) => ({ id: data.id, depth, key })),
    ).toEqual([
      { id: 1, depth: 0, key: "root.0" },
      { id: 2, depth: 1, key: "root.0.0" },
      { id: 3, depth: 1, key: "root.0.1" },
      { id: 4, depth: 0, key: "root.1" },
    ]);
  });

  it("将精确筛选构造成类型化 where，并忽略空输入", () => {
    expect(
      buildWhereClause({
        enabled: { operator: "eq", value: "true" },
        count: { operator: "eq", value: " 3 " },
        empty: { operator: "eq", value: " " },
      }),
    ).toEqual({
      type: "and",
      conditions: [
        { type: "eq", field: "enabled", value: true },
        { type: "eq", field: "count", value: 3 },
      ],
    });
  });

  it("按后端契约生成包含、集合与区间条件", () => {
    expect(
      buildWhereClause({
        name: { operator: "contains", value: " 渲染器 " },
        status: { operator: "in", value: ["active", "paused"] },
        score: { operator: "range", value: ["10", "20"] },
      }),
    ).toEqual({
      type: "and",
      conditions: [
        { type: "like", field: "name", pattern: "%渲染器%" },
        {
          type: "in",
          field: "status",
          values: ["active", "paused"],
        },
        { type: "between", field: "score", lo: 10, hi: 20 },
      ],
    });
  });

  it("从列契约初始化默认操作符和区间值", () => {
    const filters = createTableFilters([
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
      {
        field: "score",
        title: "分数",
        description: "",
        widget: "integer",
        required: false,
        searchable: false,
        filterable: true,
        sortable: true,
        filter: { operators: ["range"], default_operator: "range" },
      },
    ]);
    expect(filters).toEqual({
      name: { operator: "eq", value: null },
      score: { operator: "range", value: [null, null] },
    });
    expect(buildWhereClause(filters)).toBeUndefined();
  });

  it("将危险和超额操作收纳到更多菜单", () => {
    const presentation = (
      operationId: string,
      confirmation = false,
    ): ActionPresentationSchema => ({
      operation_id: operationId,
      title: operationId,
      placement: "row",
      interaction: "invoke",
      confirmation: confirmation
        ? { title: "确认", message: "不可撤销" }
        : undefined,
    });
    const edit = presentation("edit");
    const inspect = presentation("inspect");
    const remove = presentation("remove", true);

    expect(groupPresentedActions([edit, inspect, remove], 1)).toEqual({
      primary: edit,
      secondary: [],
      overflow: [inspect, remove],
    });
  });

  it("只用可读行数据预填 Action，并隔离 write-only 字段", () => {
    expect(
      buildActionInitialValues(
        action,
        [field("id"), field("secret", true), field("name")],
        { id: 7, name: "A", secret: "hidden", extra: "kept-in-data" },
      ),
    ).toEqual({
      id: 7,
      data: { id: 7, name: "A", extra: "kept-in-data" },
    });
  });

  it("生成稳定分页选项并格式化单元格", () => {
    expect(pageSizeOptions(20)).toEqual([
      { label: "10 / 页", value: 10 },
      { label: "20 / 页", value: 20 },
      { label: "50 / 页", value: 50 },
    ]);
    expect(formatCell(null)).toBe("—");
    expect(formatCell(false)).toBe("否");
    expect(formatCell({ id: 1 })).toBe('{"id":1}');
  });
});

describe("树形表格安全降级", () => {
  const tree = {
    id_field: "id",
    parent_field: "parent_id",
    label_field: "name",
    max_nodes: 3,
  };

  it("无查询条件时构造树，超过 max_nodes 回退平铺并警告", () => {
    const rows = [
      { id: 1, parent_id: null, name: "root" },
      { id: 2, parent_id: 1, name: "child" },
    ];
    const nested = resolveDisplayRows({ tree }, rows, false);
    expect(nested.warning).toBe("");
    expect(nested.rows[0]?.children).toHaveLength(1);

    const overflow = [
      { id: 1, parent_id: null },
      { id: 2, parent_id: null },
      { id: 3, parent_id: null },
      { id: 4, parent_id: null },
    ];
    const degraded = resolveDisplayRows({ tree }, overflow, false);
    expect(degraded.warning).toContain("超过契约上限");
    expect(degraded.warning).toContain("已安全降级为普通表格");
    expect(degraded.rows).toBe(overflow);
  });

  it("循环父子关系回退平铺；有活动查询时始终平铺", () => {
    const cyclic = [
      { id: 1, parent_id: 2 },
      { id: 2, parent_id: 1 },
    ];
    const degraded = resolveDisplayRows({ tree }, cyclic, false);
    expect(degraded.warning).toContain("循环父子关系");
    expect(degraded.rows).toBe(cyclic);

    const queried = resolveDisplayRows({ tree }, cyclic, true);
    expect(queried.warning).toBe("");
    expect(queried.rows).toBe(cyclic);
  });
});
