import { describe, expect, it } from "vitest";
import type { TableColumnSchema } from "@/engine/contracts/ui-catalog";
import { PRODUCT_LOCALE } from "@/shared/lib/product-locale";
import {
  inferDisplayKind,
  resolveCellPresentation,
} from "@/engine/renderers/table/business-cell-model";

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

  it("date_time 把 Unix 秒按时间戳渲染，不按日期字符串解析", () => {
    const seconds = 1720000000;
    const expected = new Intl.DateTimeFormat(PRODUCT_LOCALE, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).format(new Date(seconds * 1000));

    const numeric = resolveCellPresentation(
      column({ widget: "date_time" }),
      seconds,
    );
    expect(numeric).toMatchObject({ kind: "date_time", text: expected });
    expect(numeric.text).not.toBe(String(seconds));

    // 后端 JSON 可能把 Timestamp 序列化成数字字符串，同样按秒处理。
    expect(
      resolveCellPresentation(column({ widget: "date_time" }), String(seconds))
        .text,
    ).toBe(expected);
  });
});
