import { requestWithTokenRefresh } from "./auth-session";
import { ApiError } from "../http/errors";
import { apiBase, contextHeaders, parseJson } from "../http/http";
import type { SessionContext } from "../http/types";

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

export type StepUpProofResult = {
  proof: string;
  expiresIn: number;
};

export async function completeStepUp(
  challenge: string,
  credentials: { username: string; password: string },
  context: SessionContext,
  signal?: AbortSignal,
): Promise<StepUpProofResult> {
  const response = await requestWithTokenRefresh(context.token, (token) => {
    const headers = contextHeaders({ ...context, token });
    headers.set("Content-Type", "application/json");
    return fetch(`${apiBase}/api/v1/users/step-up/complete`, {
      method: "POST",
      headers,
      body: JSON.stringify({ challenge, credentials }),
      credentials: "include",
      signal,
    });
  });
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = (await parseJson(response)) as ApiEnvelope | undefined;
  const data =
    payload?.data !== null &&
    typeof payload?.data === "object" &&
    !Array.isArray(payload.data)
      ? (payload.data as Record<string, unknown>)
      : undefined;
  const proof = data?.proof;
  const expiresIn = data?.expires_in;
  if (
    !response.ok ||
    payload?.code !== 0 ||
    typeof proof !== "string" ||
    proof.length === 0 ||
    typeof expiresIn !== "number" ||
    !Number.isInteger(expiresIn) ||
    expiresIn <= 0 ||
    expiresIn > 600
  ) {
    throw new ApiError(
      response.ok && payload?.code === 0
        ? "Step-up 响应缺少有效 proof"
        : (payload?.message ?? `HTTP ${response.status}`),
      {
        status: response.status,
        code: payload?.code,
        requestId,
      },
    );
  }
  return { proof, expiresIn };
}
