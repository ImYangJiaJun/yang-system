import { createContext, useContext, useSyncExternalStore } from "react";

import type { AccountIdentity } from "@/engine/catalog/module-pages";
import type { IdentityStore } from "./identity";

/// 身份上下文的 React 绑定（纯逻辑在 identity.ts 的 IdentityStore；本文件不导出组件）。
export interface IdentityState {
  identity: AccountIdentity | undefined;
  select: (identity: AccountIdentity) => void;
  clear: () => void;
}

export const IdentityStoreContext = createContext<IdentityStore | null>(null);

export function useIdentityStore(): IdentityStore {
  const store = useContext(IdentityStoreContext);
  if (!store) throw new Error("IdentityStoreContext.Provider 缺失");
  return store;
}

export function useIdentity(): IdentityState {
  const store = useIdentityStore();
  const identity = useSyncExternalStore(store.subscribe, store.getSnapshot);
  return {
    identity,
    select: (next) => store.select(next),
    clear: () => store.clear(),
  };
}
