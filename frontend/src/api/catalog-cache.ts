import type { SessionContext } from "./client";
import type { UiCatalog } from "@/contracts/ui-catalog";

function contextKey(context: SessionContext): string {
  return JSON.stringify([
    context.token?.trim() ?? "",
    context.tenantId?.trim() ?? "",
  ]);
}

/// 请求仍会到达服务端完成重新授权；这里只按 identity + tenant + revision 复用已经
/// 校验过的不可变目录对象，避免把旧身份目录误用于新上下文。
export class CatalogCache {
  private readonly entries = new Map<string, UiCatalog>();

  accept(context: SessionContext, catalog: UiCatalog): UiCatalog {
    const key = contextKey(context);
    const current = this.entries.get(key);
    if (current?.revision === catalog.revision) return current;
    this.entries.set(key, catalog);
    return catalog;
  }

  get(context: SessionContext): UiCatalog | undefined {
    return this.entries.get(contextKey(context));
  }
}
