import { describe, expect, it } from "vitest";
import { resolveFormControl } from "@/engine/renderers/form/form-control";
import {
  fromDateTimeLocalValue,
  toDateTimeLocalValue,
} from "@/engine/renderers/form/date-time-value";

describe("resolveFormControl", () => {
  it.each([
    ["text", "text"],
    ["textarea", "textarea"],
    ["password", "password"],
    ["email", "email"],
    ["url", "url"],
    ["color", "color"],
    ["editor", "textarea"],
    ["integer", "number"],
    ["decimal", "number"],
    ["switch", "toggle"],
    ["radio", "enum"],
    ["relation_select", "relation"],
    ["tree_select", "relation"],
    ["date_time", "date_time"],
    ["json", "json"],
  ] as const)("为 %s 提供显式 renderer 或安全降级", (widget, expected) => {
    expect(resolveFormControl(widget, "string", false, undefined)).toBe(
      expected,
    );
  });

  it("没有业务 hint 时按 JSON Schema 选择通用控件", () => {
    expect(resolveFormControl(undefined, "array", false, undefined)).toBe(
      "json",
    );
    expect(resolveFormControl(undefined, "boolean", false, undefined)).toBe(
      "toggle",
    );
    expect(resolveFormControl(undefined, "string", true, undefined)).toBe(
      "enum",
    );
    expect(resolveFormControl(undefined, "string", false, "date-time")).toBe(
      "date_time",
    );
  });
});

describe("date_time 控件的秒 ↔ 本地墙钟桥", () => {
  it("Unix 秒按本地时区还原成 datetime-local 值，提交时再换回秒", () => {
    const seconds = Math.floor(Date.now() / 1000);
    const local = toDateTimeLocalValue(seconds);
    expect(local).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
    // 换算必须可逆：日期选择器改一秒，回到后端仍是同一时刻。
    expect(fromDateTimeLocalValue(local)).toBe(seconds - (seconds % 60));
  });

  it("已经是 datetime-local 形态的字符串原样保留", () => {
    expect(toDateTimeLocalValue("2026-11-05T08:30")).toBe("2026-11-05T08:30");
  });

  it("空值与非法值不给控件喂垃圾", () => {
    expect(toDateTimeLocalValue(undefined)).toBe("");
    expect(toDateTimeLocalValue("not-a-date")).toBe("");
    expect(fromDateTimeLocalValue("")).toBeUndefined();
  });
});
