import type { ActionDemoSchema } from "src/contracts/ui-catalog";
import { ContractError } from "src/contracts/ui-catalog";
import { apiBase, contextHeaders } from "./http";
import type { SessionContext } from "./types";

function stringValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined || value === null) return "";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
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

export function buildActionRequest(
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
