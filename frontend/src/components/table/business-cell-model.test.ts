import { describe, expect, it } from "vitest";
import type { TableColumnSchema } from "src/contracts/ui-catalog";
import {
  inferDisplayKind,
  resolveCellPresentation,
} from "./business-cell-model";

function column(overrides: Partial<TableColumnSchema> = {}): TableColumnSchema {
  return {
    field: "value",
    title: "值",
    description: "",
    widget: "text",
    required: false,
    searchable: false,
    filterable: false,
    sortable: false,
    ...overrides,
  };
}

describe("business cell model", () => {
  it("从稳定字段契约推导关系、数值和布尔展示", () => {
    expect(
      inferDisplayKind(
        column({
          relation: {
            operation_id: "demo.options",
            value_field: "id",
            label_fields: ["name"],
          },
        }),
      ),
    ).toBe("relation");
    expect(inferDisplayKind(column({ widget: "integer" }))).toBe("number");
    expect(inferDisplayKind(column({ widget: "switch" }))).toBe("boolean");
  });

  it("优先把关系值和枚举值翻译成业务标签", () => {
    expect(
      resolveCellPresentation(
        column({
          relation: {
            operation_id: "demo.options",
            value_field: "id",
            label_fields: ["name"],
          },
        }),
        2,
        "业务",
      ),
    ).toMatchObject({ kind: "relation", text: "业务", tooltip: "原始值：2" });
    expect(
      resolveCellPresentation(
        column({
          display: {
            kind: "status",
            options: [{ value: "active", label: "运行中", tone: "positive" }],
          },
        }),
        "active",
      ),
    ).toMatchObject({ kind: "status", text: "运行中", tone: "positive" });
  });

  it("关系标签缺失时保留原始值作为安全降级", () => {
    expect(
      resolveCellPresentation(
        column({
          display: { kind: "relation" },
        }),
        99,
      ).text,
    ).toBe("99");
  });
});
