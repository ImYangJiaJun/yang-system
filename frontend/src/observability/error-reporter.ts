import { activeAccessToken } from "src/api/auth-session";
import { ApiError } from "src/api/errors";
import { apiBase } from "src/api/http";

export type FrontendErrorKind =
  "api" | "contract" | "network" | "promise" | "runtime" | "vue";

export interface FrontendErrorReport {
  event_id: string;
  kind: FrontendErrorKind;
  route: string;
  fingerprint: string;
  operation?: string;
  related_request_id?: string;
  status?: number;
  error_code?: number;
}

export interface FrontendErrorContext {
  kind: FrontendErrorKind;
  operation?: string;
  relatedRequestId?: string;
}

interface FrontendErrorReporterOptions {
  accessToken?: () => string | undefined;
  routeName: () => string;
  eventId?: () => string;
  now?: () => number;
  send?: (report: FrontendErrorReport, token: string) => Promise<void>;
}

const REPORT_PATH = "/api/v1/observability/frontend-errors";
const DEDUPLICATION_WINDOW_MS = 10_000;
const MAX_DEDUPLICATION_ENTRIES = 256;
const REQUEST_ID_PATTERN = /^[0-9a-f]{32}$/i;
const ROUTE_PATTERN = /^[A-Za-z0-9_.:-]{1,64}$/;
const OPERATION_PATTERN = /^[a-z][a-z0-9_.-]{0,127}$/;
const MASK_64 = 0xffff_ffff_ffff_ffffn;
const SAFE_ERROR_NAMES = new Set([
  "AggregateError",
  "ApiError",
  "ContractError",
  "Error",
  "EvalError",
  "RangeError",
  "ReferenceError",
  "SyntaxError",
  "TypeError",
  "URIError",
]);

let activeReporter: ReturnType<typeof createFrontendErrorReporter> | undefined;

export function createFrontendErrorReporter(
  options: FrontendErrorReporterOptions,
) {
  const accessToken = options.accessToken ?? activeAccessToken;
  const eventId = options.eventId ?? randomEventId;
  const now = options.now ?? Date.now;
  const send = options.send ?? sendReport;
  const lastSentAt = new Map<string, number>();

  async function capture(
    cause: unknown,
    context: FrontendErrorContext,
  ): Promise<boolean> {
    const token = accessToken()?.trim();
    if (!token) return false;

    const report = normalizeReport(
      cause,
      context,
      options.routeName(),
      eventId(),
    );
    const deduplicationKey = [
      report.kind,
      report.route,
      report.operation ?? "",
      report.related_request_id ?? "",
      report.fingerprint,
    ].join(":");
    const timestamp = now();
    for (const [key, sentAt] of lastSentAt) {
      if (timestamp - sentAt > DEDUPLICATION_WINDOW_MS) {
        lastSentAt.delete(key);
      }
    }
    const previous = lastSentAt.get(deduplicationKey);
    if (
      previous !== undefined &&
      timestamp - previous <= DEDUPLICATION_WINDOW_MS
    ) {
      return false;
    }
    if (
      previous === undefined &&
      lastSentAt.size >= MAX_DEDUPLICATION_ENTRIES
    ) {
      const oldest = lastSentAt.keys().next().value;
      if (oldest !== undefined) lastSentAt.delete(oldest);
    }
    lastSentAt.set(deduplicationKey, timestamp);

    try {
      await send(report, token);
      return true;
    } catch {
      return false;
    }
  }

  return { capture };
}

export function installFrontendErrorReporter(routeName: () => string) {
  const reporter = createFrontendErrorReporter({ routeName });
  activeReporter = reporter;
  const reportRuntimeError = (event: ErrorEvent) => {
    void reporter.capture(event.error ?? new Error(event.message), {
      kind: "runtime",
    });
  };
  const reportRejectedPromise = (event: PromiseRejectionEvent) => {
    void reporter.capture(event.reason, { kind: "promise" });
  };
  window.addEventListener("error", reportRuntimeError);
  window.addEventListener("unhandledrejection", reportRejectedPromise);

  return () => {
    window.removeEventListener("error", reportRuntimeError);
    window.removeEventListener("unhandledrejection", reportRejectedPromise);
    if (activeReporter === reporter) activeReporter = undefined;
  };
}

export function captureFrontendError(
  cause: unknown,
  context: FrontendErrorContext,
) {
  void activeReporter?.capture(cause, context);
}

function normalizeReport(
  cause: unknown,
  context: FrontendErrorContext,
  rawRoute: string,
  eventId: string,
): FrontendErrorReport {
  const errorName =
    cause instanceof Error
      ? SAFE_ERROR_NAMES.has(cause.name)
        ? cause.name
        : "Error"
      : `NonError:${safeErrorType(cause)}`;
  const apiError = cause instanceof ApiError ? cause : undefined;
  const route = ROUTE_PATTERN.test(rawRoute) ? rawRoute : "unknown";
  const operation =
    context.operation && OPERATION_PATTERN.test(context.operation)
      ? context.operation
      : undefined;
  const relatedRequestId =
    apiError?.requestId && REQUEST_ID_PATTERN.test(apiError.requestId)
      ? apiError.requestId.toLowerCase()
      : context.relatedRequestId &&
          REQUEST_ID_PATTERN.test(context.relatedRequestId)
        ? context.relatedRequestId.toLowerCase()
        : undefined;
  const status =
    apiError &&
    Number.isInteger(apiError.status) &&
    apiError.status >= 100 &&
    apiError.status <= 599
      ? apiError.status
      : undefined;
  const errorCode =
    apiError?.code !== undefined &&
    Number.isInteger(apiError.code) &&
    apiError.code >= 0 &&
    apiError.code <= 999_999
      ? apiError.code
      : undefined;
  const fingerprint = fingerprintFor([
    errorName,
    context.kind,
    operation ?? "",
    status?.toString() ?? "",
    errorCode?.toString() ?? "",
  ]);

  return {
    event_id: eventId,
    kind: context.kind,
    route,
    fingerprint,
    ...(operation ? { operation } : {}),
    ...(relatedRequestId ? { related_request_id: relatedRequestId } : {}),
    ...(status !== undefined ? { status } : {}),
    ...(errorCode !== undefined ? { error_code: errorCode } : {}),
  };
}

function fingerprintFor(parts: string[]) {
  let hash = 0xcbf2_9ce4_8422_2325n;
  for (const character of parts.join("\u001f")) {
    hash ^= BigInt(character.codePointAt(0) ?? 0);
    hash = (hash * 0x0000_0100_0000_01b3n) & MASK_64;
  }
  return hash.toString(16).padStart(16, "0");
}

function safeErrorType(cause: unknown) {
  if (cause === null) return "null";
  if (Array.isArray(cause)) return "array";
  return typeof cause;
}

function randomEventId() {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
  return [
    hex.slice(0, 4).join(""),
    hex.slice(4, 6).join(""),
    hex.slice(6, 8).join(""),
    hex.slice(8, 10).join(""),
    hex.slice(10, 16).join(""),
  ].join("-");
}

async function sendReport(report: FrontendErrorReport, token: string) {
  const response = await fetch(`${apiBase}${REPORT_PATH}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(report),
    credentials: "same-origin",
    keepalive: true,
  });
  if (!response.ok) {
    throw new Error(`frontend error report rejected: HTTP ${response.status}`);
  }
}
