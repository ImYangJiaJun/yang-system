import type { UiCatalog } from "src/contracts/ui-catalog";

/// 每次请求仍会到达服务端完成重新授权；这里只复用最近一次内容相同的不可变目录
/// 对象。缓存不保存 bearer token，也不会随会话数量无界增长。
export class CatalogCache {
  private current: UiCatalog | undefined;

  accept(catalog: UiCatalog): UiCatalog {
    if (this.current?.revision === catalog.revision) return this.current;
    this.current = catalog;
    return catalog;
  }
}
