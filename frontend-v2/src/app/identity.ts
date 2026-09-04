import type { AccountIdentity } from "@/catalog/module-pages";

/**
 * 身份选择状态（旧 stores/identity.ts 语义平移）：
 * sessionStorage 持久化，key 与旧实现一致；身份取值由后端 Catalog 投影决定，
 * 前端不维护硬编码清单，下游可见性过滤会忽略 Catalog 中不存在的身份。
 *
 * 实现为进程内小型外置 store（subscribe/getSnapshot），React 侧经
 * useSyncExternalStore 订阅（app/use-identity.ts）——同步更新语义保证
 * “选择身份后立即导航”不会被未提交的 useState 批次拦截。
 */

export const IDENTITY_STORAGE_KEY = "yang.account-identity";

export function loadStoredIdentity(): AccountIdentity | undefined {
  if (typeof sessionStorage === "undefined") return undefined;
  const identity = sessionStorage.getItem(IDENTITY_STORAGE_KEY)?.trim();
  return identity ? identity : undefined;
}

export function storeIdentity(identity: AccountIdentity): void {
  if (typeof sessionStorage === "undefined") return;
  sessionStorage.setItem(IDENTITY_STORAGE_KEY, identity);
}

export function clearStoredIdentity(): void {
  if (typeof sessionStorage === "undefined") return;
  sessionStorage.removeItem(IDENTITY_STORAGE_KEY);
}

export class IdentityStore {
  private current: AccountIdentity | undefined = loadStoredIdentity();
  private readonly listeners = new Set<() => void>();

  /// 箭头函数属性保证引用稳定，可直接供 useSyncExternalStore。
  readonly getSnapshot = (): AccountIdentity | undefined => this.current;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  select(identity: AccountIdentity): void {
    storeIdentity(identity);
    this.current = identity;
    this.emit();
  }

  clear(): void {
    clearStoredIdentity();
    this.current = undefined;
    this.emit();
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }
}

export function createIdentityStore(): IdentityStore {
  return new IdentityStore();
}

export type IdentityLanding =
  | { kind: "direct"; identity: AccountIdentity }
  | { kind: "select" }
  | { kind: "none" };

/// 登录后的身份落点：已存且仍可见 → 直接进入；单身份 → 直接；多身份 → 选择页；零身份 → 空态。
export function resolveIdentityLanding(
  identities: ReadonlyArray<{ id: AccountIdentity }>,
  stored: AccountIdentity | undefined,
): IdentityLanding {
  if (stored && identities.some((candidate) => candidate.id === stored)) {
    return { kind: "direct", identity: stored };
  }
  if (identities.length === 1 && identities[0]) {
    return { kind: "direct", identity: identities[0].id };
  }
  if (identities.length === 0) return { kind: "none" };
  return { kind: "select" };
}
