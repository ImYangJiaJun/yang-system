import { describe, expect, it } from "vitest";

import { serializeExportCondition } from "@/features/feishu/api";
import {
  exportMarkStatusLabel,
  exportWriteStatusLabel,
  isExportRunForceRecreatable,
  isExportRunRetryable,
} from "@/features/feishu/types";

describe("审批回写条件 DSL", () => {
  it("空条件不落库（恒真）", () => {
    expect(serializeExportCondition(null)).toBeNull();
    expect(serializeExportCondition({ match: "all", items: [] })).toBeNull();
  });

  it("实例与明细条件按后端 DSL 序列化", () => {
    const text = serializeExportCondition({
      match: "all",
      items: [
        { scope: "instance", widgetId: "total", op: "gte", value: "1000" },
        {
          scope: "detail",
          detailWidgetId: "detail",
          match: "none",
          items: [{ widgetId: "money", op: "between", value: ["100", "200"] }],
        },
      ],
    });
    expect(text).not.toBeNull();
    expect(JSON.parse(text as string)).toEqual({
      match: "all",
      items: [
        { scope: "instance", widget_id: "total", op: "gte", value: "1000" },
        {
          scope: "detail",
          detail_widget_id: "detail",
          match: "none",
          items: [{ widget_id: "money", op: "between", value: ["100", "200"] }],
        },
      ],
    });
  });

  it("is_empty 不携带操作数", () => {
    const text = serializeExportCondition({
      match: "any",
      items: [
        { scope: "instance", widgetId: "note", op: "is_empty", value: null },
      ],
    });
    expect(JSON.parse(text as string).items[0]).toEqual({
      scope: "instance",
      widget_id: "note",
      op: "is_empty",
    });
  });
});

describe("回写运行状态判据", () => {
  it("只有可恢复状态能人工重试", () => {
    for (const status of ["pending", "retry", "review"])
      expect(isExportRunRetryable(status)).toBe(true);
    for (const status of ["writing", "written", "terminal", "skipped"])
      expect(isExportRunRetryable(status)).toBe(false);
  });

  it("只有已写入且有目标记录的行能强制重建", () => {
    expect(
      isExportRunForceRecreatable({
        rowKey: "row:0",
        writeStatus: "written",
        targetRecordId: "rec1",
      }),
    ).toBe(true);
    expect(
      isExportRunForceRecreatable({
        rowKey: "__rule__",
        writeStatus: "written",
        targetRecordId: "rec1",
      }),
    ).toBe(false);
    expect(
      isExportRunForceRecreatable({
        rowKey: "row:0",
        writeStatus: "written",
        targetRecordId: null,
      }),
    ).toBe(false);
    expect(
      isExportRunForceRecreatable({
        rowKey: "row:0",
        writeStatus: "writing",
        targetRecordId: "rec1",
      }),
    ).toBe(false);
  });

  it("认不出的状态原样回显，不猜成成功态", () => {
    expect(exportWriteStatusLabel("future_status")).toBe("future_status");
    expect(exportMarkStatusLabel("future_mark")).toBe("future_mark");
  });
});
