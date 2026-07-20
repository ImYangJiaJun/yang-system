import type { AccountIdentity } from "src/module-pages";

export type AccessTarget = "login" | "role-selection" | "protected";

export interface AccessState {
  authenticated: boolean;
  accountIdentity: AccountIdentity | undefined;
}

function storedIdentity(value: string | null): AccountIdentity | undefined {
  return value === "user" || value === "admin" || value === "org"
    ? value
    : undefined;
}

export function readAccessState(): AccessState {
  if (typeof sessionStorage === "undefined") {
    return { authenticated: false, accountIdentity: undefined };
  }
  return {
    authenticated: Boolean(sessionStorage.getItem("yang.token")?.trim()),
    accountIdentity: storedIdentity(
      sessionStorage.getItem("yang.account-identity"),
    ),
  };
}

export function resolveAccessRedirect(
  target: AccessTarget,
  state: AccessState,
  targetIdentity?: AccountIdentity,
): string | undefined {
  if (!state.authenticated) {
    return target === "login" ? undefined : "/login";
  }
  if (target === "login") return "/roles";
  if (target === "role-selection") return undefined;
  if (
    !state.accountIdentity ||
    (targetIdentity !== undefined && targetIdentity !== state.accountIdentity)
  ) {
    return "/roles";
  }
  return undefined;
}
