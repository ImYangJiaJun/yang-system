import { describe, expect, it } from "vitest";
import { resolveCustomView } from "./registry";

describe("custom view registry", () => {
  it("只解析静态白名单 view_id", () => {
    expect(resolveCustomView("demo.items.insight")).toBeTypeOf("function");
    expect(resolveCustomView("unknown.view")).toBeUndefined();
    expect(resolveCustomView("../views/Admin.vue")).toBeUndefined();
  });
});
