import type { ActionDemoSchema } from "src/contracts/ui-catalog";
import { ApiError, StepUpRequiredError } from "./errors";
import { parseJson } from "./http";
import type { InvocationResult } from "./types";

function filenameFromDisposition(
  disposition: string | null,
): string | undefined {
  if (!disposition) return undefined;
  const utf8 = disposition.match(/filename\*=UTF-8''([^;]+)/i)?.[1];
  if (utf8) return decodeURIComponent(utf8);
  return disposition.match(/filename="?([^";]+)"?/i)?.[1];
}

export async function parseActionResponse(
  action: ActionDemoSchema,
  response: Response,
  durationMs: number,
): Promise<InvocationResult> {
  const requestId = response.headers.get("x-request-id") ?? undefined;

  if (
    action.response_kind === "redirect" &&
    (response.type === "opaqueredirect" ||
      (response.status >= 300 && response.status < 400))
  ) {
    return {
      kind: "redirect",
      status: response.status,
      durationMs,
      requestId,
      location: response.headers.get("location") ?? undefined,
    };
  }

  if (
    action.response_kind === "download" ||
    action.response_kind === "preview"
  ) {
    if (!response.ok) {
      const payload = await parseJson(response);
      const envelope = payload as
        { code?: number; message?: string } | undefined;
      throw new ApiError(envelope?.message ?? `HTTP ${response.status}`, {
        status: response.status,
        code: envelope?.code,
        requestId,
        details: payload,
      });
    }
    const blob = await response.blob();
    return {
      kind: action.response_kind,
      status: response.status,
      durationMs,
      requestId,
      blobUrl: URL.createObjectURL(blob),
      filename: filenameFromDisposition(
        response.headers.get("content-disposition"),
      ),
    };
  }

  const payload = await parseJson(response);
  const envelope = payload as
    { code?: number; message?: string; data?: unknown } | undefined;
  if (response.status === 428) {
    const data = envelope?.data;
    const challenge =
      data !== null && typeof data === "object" && !Array.isArray(data)
        ? (data as Record<string, unknown>).challenge
        : undefined;
    const expiresIn =
      data !== null && typeof data === "object" && !Array.isArray(data)
        ? (data as Record<string, unknown>).expires_in
        : undefined;
    if (
      typeof challenge === "string" &&
      challenge.length > 0 &&
      typeof expiresIn === "number" &&
      Number.isInteger(expiresIn) &&
      expiresIn > 0 &&
      expiresIn <= 300
    ) {
      throw new StepUpRequiredError(
        envelope?.message ?? "敏感操作需要重新认证",
        {
          code: envelope?.code,
          requestId,
          challenge,
          expiresIn,
        },
      );
    }
  }
  if (!response.ok || envelope?.code !== 0) {
    throw new ApiError(envelope?.message ?? `HTTP ${response.status}`, {
      status: response.status,
      code: envelope?.code,
      requestId,
      details: payload,
    });
  }
  return {
    kind: "json",
    status: response.status,
    durationMs,
    requestId,
    message: envelope.message,
    data: envelope.data,
  };
}
