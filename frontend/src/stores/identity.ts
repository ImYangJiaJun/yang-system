import { ref } from "vue";
import { defineStore } from "pinia";
import type { AccountIdentity } from "src/module-pages";

const IDENTITY_KEY = "yang.account-identity";

function storedIdentity(): AccountIdentity | undefined {
  if (typeof sessionStorage === "undefined") return undefined;
  const identity = sessionStorage.getItem(IDENTITY_KEY);
  return identity === "user" || identity === "admin" || identity === "org"
    ? identity
    : undefined;
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
