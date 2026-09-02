import { ref } from "vue";
import { defineStore } from "pinia";
import type { AccountIdentity } from "src/module-pages";

const IDENTITY_KEY = "yang.account-identity";

function storedIdentity(): AccountIdentity | undefined {
  if (typeof sessionStorage === "undefined") return undefined;
  const identity = sessionStorage.getItem(IDENTITY_KEY)?.trim();
  // 身份取值由后端 Catalog 投影（module.identity.id）决定，前端不维护硬编码清单；
  // 下游 module-pages 的可见性过滤会忽略 Catalog 中不存在的身份
  return identity ? identity : undefined;
}

export const useIdentityStore = defineStore("identity", () => {
  const accountIdentity = ref<AccountIdentity | undefined>(storedIdentity());

  function select(identity: AccountIdentity) {
    accountIdentity.value = identity;
    sessionStorage.setItem(IDENTITY_KEY, identity);
  }

  function clear() {
    accountIdentity.value = undefined;
    if (typeof sessionStorage !== "undefined") {
      sessionStorage.removeItem(IDENTITY_KEY);
    }
  }

  return {
    accountIdentity,
    select,
    clear,
  };
});
