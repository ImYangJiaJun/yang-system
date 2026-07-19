import {
  ContractError,
  parseUiCatalog,
  type ActionDemoSchema,
  type UiCatalog,
} from "src/contracts/ui-catalog";

export type SessionContext = {
  token?: string;
  tenantId?: string;
};

export type InvocationResult = {
  kind: "json" | "download" | "preview" | "redirect";
  status: number;
  durationMs: number;
  requestId?: string;
  message?: string;
  data?: unknown;
  blobUrl?: string;
  filename?: string;
  location?: string;
};

export class ApiError extends Error {
  readonly status: number;
  readonly code?: number;
  readonly requestId?: string;
  readonly details?: unknown;

  constructor(
    message: string,
    options: {
      status: number;
      code?: number;
      requestId?: string;
      details?: unknown;
    },
  ) {
    super(message);
    this.name = "ApiError";
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.details = options.details;
  }
}

const apiBase = (import.meta.env.VITE_API_BASE_URL ?? "").replace(/\/$/, "");

function contextHeaders(context: SessionContext): Headers {
  const headers = new Headers({ Accept: "application/json" });
  if (context.token?.trim())
    headers.set("Authorization", `Bearer ${context.token.trim()}`);
  if (context.tenantId?.trim())
    headers.set("x-tenant-id", context.tenantId.trim());
  return headers;
}

async function parseJson(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new ContractError("服务端返回了无效 JSON", [
      error instanceof Error ? error.message : String(error),
    ]);
  }
}

export async function fetchUiCatalog(
  context: SessionContext,
  signal?: AbortSignal,
): Promise<UiCatalog> {
  const response = await fetch(`${apiBase}/.well-known/yang/ui-catalog`, {
    method: "GET",
    headers: contextHeaders(context),
    signal,
  });
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

function stringValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined || value === null) return "";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

function filenameFromDisposition(
  disposition: string | null,
): string | undefined {
  if (!disposition) return undefined;
  const utf8 = disposition.match(/filename\*=UTF-8''([^;]+)/i)?.[1];
  if (utf8) return decodeURIComponent(utf8);
  return disposition.match(/filename="?([^";]+)"?/i)?.[1];
}

function appendMultipart(
  body: Record<string, unknown>,
  spec: NonNullable<ActionDemoSchema["multipart"]>,
): FormData {
  const form = new FormData();
  const encoder = new TextEncoder();
  let fieldCount = 0;
  let fileCount = 0;
  let totalBytes = 0;

  const appendFile = (name: string, file: File) => {
    fileCount += 1;
    if (fileCount > spec.max_files)
      throw new ContractError(`文件数量超过上限 ${spec.max_files}`);
    if (file.size > spec.max_file_bytes)
      throw new ContractError(
        `文件 ${file.name} 超过单文件上限 ${spec.max_file_bytes} bytes`,
      );
    if (!spec.allowed_content_types.includes(file.type))
      throw new ContractError(
        `不允许的文件类型 ${file.type || "unknown"}，允许：${spec.allowed_content_types.join(", ")}`,
      );
    totalBytes += file.size;
    form.append(name, file, file.name);
  };

  for (const [name, value] of Object.entries(body)) {
    if (value === undefined || value === null || value === "") continue;
    if (value instanceof File) {
      appendFile(name, value);
      continue;
    }
    if (Array.isArray(value) && value.every((item) => item instanceof File)) {
      for (const file of value) appendFile(name, file);
      continue;
    }
    fieldCount += 1;
    if (fieldCount > spec.max_fields)
      throw new ContractError(`文本字段数量超过上限 ${spec.max_fields}`);
    const text = typeof value === "string" ? value : JSON.stringify(value);
    const bytes = encoder.encode(text).byteLength;
    if (bytes > spec.max_text_field_bytes)
      throw new ContractError(
        `字段 ${name} 超过文本上限 ${spec.max_text_field_bytes} bytes`,
      );
    totalBytes += bytes;
    form.append(name, text);
  }
  if (totalBytes > spec.max_total_bytes)
    throw new ContractError(
      `multipart 内容超过总上限 ${spec.max_total_bytes} bytes`,
    );
  return form;
}

function buildRequest(
  action: ActionDemoSchema,
  values: Record<string, unknown>,
  context: SessionContext,
): { url: string; init: RequestInit } {
  let path = action.path;
  const query = new URLSearchParams();
  const headers = contextHeaders(context);
  const body: Record<string, unknown> = {};
  const declaredParameters = new Set(
    action.params.map((parameter) => parameter.name),
  );

  for (const parameter of action.params) {
    const value = values[parameter.name];
    const missing = value === undefined || value === null || value === "";
    if (parameter.required && missing) {
      throw new ContractError(
        `缺少必填参数：${parameter.title || parameter.name}`,
      );
    }
    if (missing) continue;
    switch (parameter.source) {
      case "path": {
        const encoded = encodeURIComponent(stringValue(value));
        const braced = `{${parameter.name}}`;
        const colon = `:${parameter.name}`;
        if (path.includes(braced)) path = path.replaceAll(braced, encoded);
        else if (path.includes(colon)) path = path.replaceAll(colon, encoded);
        else throw new ContractError(`路径模板没有参数 ${parameter.name}`);
        break;
      }
      case "query":
        query.set(parameter.name, stringValue(value));
        break;
      case "header":
        headers.set(parameter.name, stringValue(value));
        break;
      case "body":
        body[parameter.name] = value;
        break;
    }
  }

  if (!["GET", "HEAD"].includes(action.method)) {
    for (const [name, value] of Object.entries(values)) {
      if (!declaredParameters.has(name) && value !== undefined)
        body[name] = value;
    }
  }

  if (/\{[^}]+\}|:[A-Za-z_][A-Za-z0-9_]*/.test(path)) {
    throw new ContractError(`路径仍有未填写参数：${path}`);
  }

  const queryString = query.toString();
  const init: RequestInit = {
    method: action.method,
    headers,
    redirect: action.response_kind === "redirect" ? "manual" : "follow",
  };
  if (
    !["GET", "HEAD"].includes(action.method) &&
    Object.keys(body).length > 0
  ) {
    if (action.request_media_type === "multipart") {
      if (!action.multipart)
        throw new ContractError("multipart Action 缺少资源限制契约");
      init.body = appendMultipart(body, action.multipart);
    } else {
      headers.set("Content-Type", "application/json");
      init.body = JSON.stringify(body);
    }
  }
  return {
    url: `${apiBase}${path}${queryString ? `?${queryString}` : ""}`,
    init,
  };
}

export async function invokeAction(
  action: ActionDemoSchema,
  values: Record<string, unknown>,
  context: SessionContext,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  const startedAt = performance.now();
  const request = buildRequest(action, values, context);
  const response = await fetch(request.url, { ...request.init, signal });
  const durationMs = Math.round((performance.now() - startedAt) * 10) / 10;
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
