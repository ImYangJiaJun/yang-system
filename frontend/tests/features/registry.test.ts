import { describe, expect, it } from "vitest";

import { resolveCustomView } from "@/features/registry";

describe("custom view registry", () => {
  it("只解析静态白名单 view_id", () => {
    expect(resolveCustomView("demo.items.insight")).toBeDefined();
    expect(resolveCustomView("unknown.view")).toBeUndefined();
    // 路径穿越字符串永不命中（禁止按后端字符串构造动态 import）。
    expect(resolveCustomView("../views/Admin")).toBeUndefined();
    expect(resolveCustomView(null)).toBeUndefined();
  });
});
