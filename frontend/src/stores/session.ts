import { computed, ref } from "vue";
import { defineStore } from "pinia";
import type { LoginResult } from "src/api/auth";
import { clearStoredSession, persistTokenPair } from "src/api/auth-session";

function sessionValue(key: string): string {
  return typeof sessionStorage === "undefined"
    ? ""
    : (sessionStorage.getItem(key) ?? "");
}

export const useSessionStore = defineStore("session", () => {
  const token = ref(sessionValue("yang.token"));
  const loggedIn = computed(() => Boolean(token.value.trim()));

  function setTokenPair(tokens: LoginResult) {
    token.value = tokens.accessToken;
    persistTokenPair(tokens);
  }

  function clear() {
    token.value = "";
    clearStoredSession();
  }

  return {
    token,
    loggedIn,
    setTokenPair,
    clear,
  };
});
