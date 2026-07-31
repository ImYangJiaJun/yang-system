import { computed, ref } from "vue";
import { defineStore } from "pinia";
import type { LoginResult } from "src/api/auth";
import {
  clearStoredSession,
  discardLegacyStoredCredentials,
  persistTokenPair,
  restoreSessionFromCookie,
} from "src/api/auth-session";

export const useSessionStore = defineStore("session", () => {
  discardLegacyStoredCredentials();
  const token = ref("");
  const restoreState = ref<"pending" | "authenticated" | "anonymous">(
    "pending",
  );
  let activeRestore: Promise<boolean> | undefined;
  const loggedIn = computed(() => Boolean(token.value.trim()));

  function setTokenPair(tokens: LoginResult) {
    token.value = tokens.accessToken;
    restoreState.value = "authenticated";
    persistTokenPair(tokens);
  }

  function clear() {
    token.value = "";
    restoreState.value = "anonymous";
    clearStoredSession();
  }

  async function restoreFromCookie(): Promise<boolean> {
    if (loggedIn.value) return true;
    if (restoreState.value === "anonymous") return false;
    if (!activeRestore) {
      activeRestore = restoreSessionFromCookie()
        .then((tokens) => {
          if (!tokens) {
            restoreState.value = "anonymous";
            return false;
          }
          setTokenPair(tokens);
          return true;
        })
        .catch(() => {
          restoreState.value = "anonymous";
          return false;
        })
        .finally(() => {
          activeRestore = undefined;
        });
    }
    return activeRestore;
  }

  return {
    token,
    loggedIn,
    restoreState,
    setTokenPair,
    restoreFromCookie,
    clear,
  };
});
