import {
  ContractError,
  parseUiCatalog,
  type ActionDemoSchema,
  type UiCatalog,
} from "src/contracts/ui-catalog";
import { captureFrontendError } from "src/observability/error-reporter";
import { buildActionRequest } from "./action-request";
import { parseActionResponse } from "./action-response";
import {
  requestWithTokenRefresh,
  requireCredentialRelogin,
} from "./auth-session";
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
  let relatedRequestId: string | undefined;
  try {
    const response = await requestWithTokenRefresh(context.token, (token) => {
      const headers = contextHeaders({ ...context, token });
      if (cached) headers.set("If-None-Match", `"${cached.revision}"`);
      return fetch(`${apiBase}/.well-known/yang/ui-catalog`, {
        method: "GET",
        headers,
        signal,
      });
    });
    if (response.status === 304) {
      if (cached) return cached;
      throw new ApiError("UI catalog 返回 304，但本地没有可复用目录", {
        status: response.status,
      });
    }
    const requestId = response.headers.get("x-request-id") ?? undefined;
    relatedRequestId = requestId;
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
  } catch (cause) {
    captureFrontendError(cause, {
      kind: failureKind(cause),
      operation: "account.user.ui_catalog",
      relatedRequestId,
    });
    throw cause;
  }
}

export async function invokeAction(
  action: ActionDemoSchema,
  values: Record<string, unknown>,
  context: SessionContext,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  let relatedRequestId: string | undefined;
  try {
    const startedAt = performance.now();
    const response = await requestWithTokenRefresh(context.token, (token) => {
      const request = buildActionRequest(action, values, { ...context, token });
      return fetch(request.url, { ...request.init, signal });
    });
    relatedRequestId = response.headers.get("x-request-id") ?? undefined;
    const durationMs = Math.round((performance.now() - startedAt) * 10) / 10;
    const result = await parseActionResponse(action, response, durationMs);
    if (requiresCredentialRelogin(result.data)) {
      requireCredentialRelogin();
    }
    return result;
  } catch (cause) {
    if (!(cause instanceof Error && cause.name === "AbortError")) {
      captureFrontendError(cause, {
        kind: failureKind(cause),
        operation: action.operation_id,
        relatedRequestId,
      });
    }
    throw cause;
  }
}

function requiresCredentialRelogin(data: unknown): boolean {
  return (
    data !== null &&
    typeof data === "object" &&
    !Array.isArray(data) &&
    (data as Record<string, unknown>).relogin_required === true
  );
}

function failureKind(cause: unknown): "api" | "contract" | "network" {
  if (cause instanceof ApiError) return "api";
  if (cause instanceof ContractError) return "contract";
  return "network";
}
