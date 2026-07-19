import { describe, expect, it } from "vitest";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "src/contracts/ui-catalog";
import {
  buildActionInitialValues,
  buildWhereClause,
  flattenDisplayRows,
  formatCell,
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

  it("将有效筛选构造成类型化 where，并忽略空输入", () => {
    expect(
      buildWhereClause({ enabled: "true", count: " 3 ", empty: " " }),
    ).toEqual({
      type: "and",
      conditions: [
        { type: "eq", field: "enabled", value: true },
        { type: "eq", field: "count", value: 3 },
      ],
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
