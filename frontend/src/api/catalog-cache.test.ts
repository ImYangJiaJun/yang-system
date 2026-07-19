import { describe, expect, it } from "vitest";
import { CatalogCache } from "./catalog-cache";
import type { UiCatalog } from "src/contracts/ui-catalog";

function catalog(revision: string): UiCatalog {
  return {
    schema_version: "2.2",
    revision,
    actions: [],
    table_views: [],
  };
}

describe("CatalogCache", () => {
  it("只复用最近一次相同 revision，不保存会话凭证", () => {
    const cache = new CatalogCache();
    const first = catalog("a".repeat(64));
    expect(cache.accept(first)).toBe(first);
    expect(cache.accept(catalog("a".repeat(64)))).toBe(first);
    const changed = catalog("b".repeat(64));
    expect(cache.accept(changed)).toBe(changed);
  });
});
