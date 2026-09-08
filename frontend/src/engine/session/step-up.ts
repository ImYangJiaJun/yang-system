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
  credentials: { username: string; password: string; mfaCode?: string },
  context: SessionContext,
  signal?: AbortSignal,
): Promise<StepUpProofResult> {
  // 账号启用 TOTP 时 Step-up 不得降级为单因子：mfa_code 随凭据一并提交（缺省 null）。
  const body = {
    challenge,
    credentials: {
      username: credentials.username,
      password: credentials.password,
      mfa_code: credentials.mfaCode?.trim() || null,
    },
  };
  const response = await requestWithTokenRefresh(context.token, (token) => {
    const headers = contextHeaders({ ...context, token });
    headers.set("Content-Type", "application/json");
    return fetch(`${apiBase}/api/v1/users/step-up/complete`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
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
