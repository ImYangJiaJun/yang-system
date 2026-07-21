import {
  parseUiCatalog,
  type ActionDemoSchema,
  type UiCatalog,
} from "src/contracts/ui-catalog";
import { buildActionRequest } from "./action-request";
import { parseActionResponse } from "./action-response";
import { requestWithTokenRefresh } from "./auth-session";
import { ApiError } from "./errors";
import { apiBase, contextHeaders, parseJson } from "./http";
import type { InvocationResult, SessionContext } from "./types";

export { ApiError } from "./errors";
export type { InvocationResult, SessionContext } from "./types";

export async function fetchUiCatalog(
  context: SessionContext,
  signal?: AbortSignal,
): Promise<UiCatalog> {
  const response = await requestWithTokenRefresh(context.token, (token) =>
    fetch(`${apiBase}/.well-known/yang/ui-catalog`, {
      method: "GET",
      headers: contextHeaders({ ...context, token }),
      signal,
    }),
  );
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = await parseJson(response);
  if (!response.ok) {
    const envelope = payload as { code?: number; message?: string } | undefined;
    throw new ApiError(envelope?.message ?? `HTTP ${response.status}`, {
      status: response.status,
      code: envelope?.code,
      requestId,
      details: payload,
    });
  }
  return parseUiCatalog(payload);
}

export async function invokeAction(
  action: ActionDemoSchema,
  values: Record<string, unknown>,
  context: SessionContext,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  const startedAt = performance.now();
  const response = await requestWithTokenRefresh(context.token, (token) => {
    const request = buildActionRequest(action, values, { ...context, token });
    return fetch(request.url, { ...request.init, signal });
  });
  const durationMs = Math.round((performance.now() - startedAt) * 10) / 10;
  return parseActionResponse(action, response, durationMs);
}
