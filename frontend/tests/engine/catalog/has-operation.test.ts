import { describe, expect, it } from "vitest";

import { hasOperation } from "@/engine/catalog/has-operation";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";

function catalogWith(operationIds: string[]): UiCatalog {
  return {
    modules: [],
    actions: operationIds.map((operation_id) => ({ operation_id })),
  } as unknown as UiCatalog;
}

describe("hasOperation", () => {
  it("returns true only for operation ids present in the catalog", () => {
    const catalog = catalogWith(["access.groups.create_group"]);
    expect(hasOperation(catalog, "access.groups.create_group")).toBe(true);
    expect(hasOperation(catalog, "access.groups.delete_group")).toBe(false);
  });

  it("does not throw on an empty catalog", () => {
    expect(
      hasOperation({ modules: [], actions: [] } as unknown as UiCatalog, "x.y"),
    ).toBe(false);
  });

  it("matches whole operation ids, not prefixes of them", () => {
    // 前缀命中的实现会把「目录里有 access.groups.list_groups」误判成
    // 「这个身份有 access.groups.list」——那粒权限根本不存在，
    // 按钮却会渲染出来，最后以服务端 403 收场。
    const catalog = catalogWith(["access.groups.list_groups"]);
    expect(hasOperation(catalog, "access.groups.list")).toBe(false);
    expect(hasOperation(catalog, "access.groups.list_groups")).toBe(true);
  });
});
