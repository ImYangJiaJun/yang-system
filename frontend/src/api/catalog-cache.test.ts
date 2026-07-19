import { describe, expect, it } from "vitest";
import { CatalogCache } from "./catalog-cache";
import type { UiCatalog } from "@/contracts/ui-catalog";

function catalog(revision: string): UiCatalog {
  return {
    schema_version: "2.2",
    revision,
    actions: [],
    table_views: [],
  };
}

describe("CatalogCache", () => {
  it("同身份租户 revision 不变时复用，身份或租户切换严格隔离", () => {
    const cache = new CatalogCache();
    const first = catalog("a".repeat(64));
    expect(cache.accept({ token: "a", tenantId: "1" }, first)).toBe(first);
    expect(
      cache.accept({ token: "a", tenantId: "1" }, catalog("a".repeat(64))),
    ).toBe(first);
    expect(cache.get({ token: "a", tenantId: "2" })).toBeUndefined();
    expect(cache.get({ token: "b", tenantId: "1" })).toBeUndefined();
    const changed = catalog("b".repeat(64));
    expect(cache.accept({ token: "a", tenantId: "1" }, changed)).toBe(changed);
  });
});
