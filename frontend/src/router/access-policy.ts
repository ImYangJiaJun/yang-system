import type { AccountIdentity } from "src/module-pages";

export type AccessTarget = "login" | "role-selection" | "protected";

export interface AccessState {
  authenticated: boolean;
  accountIdentity: AccountIdentity | undefined;
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
