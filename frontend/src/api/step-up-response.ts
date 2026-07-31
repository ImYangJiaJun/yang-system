import { StepUpRequiredError } from "./errors";

type StepUpEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

export function stepUpRequiredError(
  response: Response,
  payload: StepUpEnvelope | undefined,
): StepUpRequiredError | undefined {
  if (response.status !== 428) return undefined;
  const data =
    payload?.data !== null &&
    typeof payload?.data === "object" &&
    !Array.isArray(payload.data)
      ? (payload.data as Record<string, unknown>)
      : undefined;
  const challenge = data?.challenge;
  const expiresIn = data?.expires_in;
  if (
    typeof challenge !== "string" ||
    challenge.length === 0 ||
    typeof expiresIn !== "number" ||
    !Number.isInteger(expiresIn) ||
    expiresIn <= 0 ||
    expiresIn > 300
  ) {
    return undefined;
  }
  return new StepUpRequiredError(payload?.message ?? "敏感操作需要重新认证", {
    code: payload?.code,
    requestId: response.headers.get("x-request-id") ?? undefined,
    challenge,
    expiresIn,
  });
}
