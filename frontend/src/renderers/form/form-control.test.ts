import { describe, expect, it } from "vitest";
import { resolveFormControl } from "./form-control";

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
