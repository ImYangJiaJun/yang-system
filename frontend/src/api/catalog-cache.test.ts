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

  it("相同 revision 的不同授权投影不能复用旧目录", () => {
    const cache = new CatalogCache();
    const anonymous = catalog("a".repeat(64));
    const authenticated: UiCatalog = {
      ...catalog("a".repeat(64)),
      actions: [
        {
          operation_id: "admin.user.list",
          title: "平台账号列表",
          description: "",
          method: "GET",
          path: "/api/v1/admin/users",
          params: [],
          input_schema: {},
          output_schema: {},
          request_media_type: "json",
          response_kind: "json",
          requires_auth: true,
        },
      ],
    };

    expect(cache.accept(anonymous)).toBe(anonymous);
    expect(cache.accept(authenticated)).toBe(authenticated);
  });
});
