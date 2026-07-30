import type { UiCatalog } from "src/contracts/ui-catalog";

/// 每次请求仍会到达服务端完成重新授权；这里只复用最近一次内容完全相同的不可变
/// 目录对象。revision 描述应用定义版本，不代表请求级授权投影，因此不能单独作为
/// 缓存键。缓存不保存 bearer token，也不会随会话数量无界增长。
export class CatalogCache {
  private current: UiCatalog | undefined;

  get value(): UiCatalog | undefined {
    return this.current;
  }

  accept(catalog: UiCatalog): UiCatalog {
    if (this.current?.revision === catalog.revision) return this.current;
    this.current = catalog;
    return catalog;
  }
}
