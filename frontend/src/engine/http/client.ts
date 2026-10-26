import { withRequestDeadline } from "./deadline";
import {
  parseUiCatalog,
  type ActionDemoSchema,
  type UiCatalog,
} from "@/engine/contracts/ui-catalog";
import { buildActionRequest } from "./action-request";
import { parseActionResponse } from "./action-response";
import {
  requestWithTokenRefresh,
  requireCredentialRelogin,
} from "../session/auth-session";
import { ApiError } from "./errors";
import { apiBase, contextHeaders, parseJson } from "./http";
import type { InvocationResult, SessionContext } from "./types";

export { ApiError } from "./errors";
export type { InvocationResult, SessionContext } from "./types";

export async function fetchUiCatalog(
  context: SessionContext,
  signal?: AbortSignal,
  cached?: UiCatalog,
): Promise<UiCatalog> {
  return withRequestDeadline(async (signal) => {
    const response = await requestWithTokenRefresh(
      context.token,
      (token) => {
        const headers = contextHeaders({ ...context, token });
        if (cached) headers.set("If-None-Match", `"${cached.revision}"`);
        return fetch(`${apiBase}/.well-known/yang/ui-catalog`, {
          method: "GET",
          headers,
          signal,
        });
      },
      signal,
    );
    if (response.status === 304) {
      if (cached) return cached;
      throw new ApiError("UI catalog 返回 304，但本地没有可复用目录", {
        status: response.status,
      });
    }
    const requestId = response.headers.get("x-request-id") ?? undefined;
    const payload = await parseJson(response);
    if (!response.ok) {
      const envelope = payload as
        { code?: number; message?: string } | undefined;
      throw new ApiError(envelope?.message ?? `HTTP ${response.status}`, {
        status: response.status,
        code: envelope?.code,
        requestId,
        details: payload,
      });
    }
    return parseUiCatalog(payload);
  }, signal);
}

export async function invokeAction(
  action: ActionDemoSchema,
  values: Record<string, unknown>,
  context: SessionContext,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  return withRequestDeadline(async (signal) => {
    const startedAt = performance.now();
    const result = await requestWithTokenRefresh(
      context.token,
      async (token) => {
        const request = buildActionRequest(action, values, {
          ...context,
          token,
        });
        const response = await fetch(request.url, { ...request.init, signal });
        const durationMs =
          Math.round((performance.now() - startedAt) * 10) / 10;
        return parseActionResponse(action, response, durationMs);
      },
      signal,
    );
    if (requiresCredentialRelogin(result.data)) {
      requireCredentialRelogin();
    }
    return result;
  }, signal);
}

function requiresCredentialRelogin(data: unknown): boolean {
  return (
    data !== null &&
    typeof data === "object" &&
    !Array.isArray(data) &&
    (data as Record<string, unknown>).relogin_required === true
  );
}
