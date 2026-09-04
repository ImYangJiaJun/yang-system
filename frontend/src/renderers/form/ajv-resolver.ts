import type { Resolver } from "react-hook-form";
import type { ErrorObject, ValidateFunction } from "ajv";

import { compileDynamicSchema } from "@/contracts/ajv";

/**
 * ADR-4：动态 JSON Schema 表单校验走 Ajv 白名单编译器（contracts/ajv.ts）。
 * react-hook-form 自定义 resolver：提交时对整个 values 跑一次 Ajv 校验，
 * 把错误按字段路径映射回 RHF 的 errors 结构。
 */

function fieldPathOf(error: ErrorObject): string {
  if (error.keyword === "required") {
    const missing = (error.params as { missingProperty?: string })
      .missingProperty;
    const base = error.instancePath.replaceAll("/", ".");
    const path = [base, missing].filter(Boolean).join(".");
    return path.replace(/^\.+/, "");
  }
  if (error.keyword === "additionalProperties") {
    const extra = (error.params as { additionalProperty?: string })
      .additionalProperty;
    if (extra) return extra;
  }
  return error.instancePath.replaceAll("/", ".").replace(/^\.+/, "");
}

export function mapAjvErrors(
  errors: ErrorObject[],
): Record<string, { type: string; message: string }> {
  const mapped: Record<string, { type: string; message: string }> = {};
  for (const error of errors) {
    const path = fieldPathOf(error);
    if (!path || mapped[path]) continue;
    mapped[path] = {
      type: error.keyword,
      message: error.message ?? `校验失败（${error.keyword}）`,
    };
  }
  return mapped;
}

export function ajvResolver(
  schema: unknown,
): Resolver<Record<string, unknown>> {
  let compiled: ValidateFunction | undefined;
  return (values) => {
    // 首次提交时才编译：白名单外关键词在此时抛出可识别错误（ADR-4 fail-closed）。
    compiled ??= compileDynamicSchema(schema);
    if (compiled(sanitizeForValidation(values))) return { values, errors: {} };
    return { values: {}, errors: mapAjvErrors(compiled.errors ?? []) };
  };
}

/// File 不是 JSON 值：校验前把文件替换为其文件名（string），类型/必填约束照常生效；
/// 文件的真实边界（数量/MIME/字节数）由 multipart 契约在请求构造时强制执行。
function sanitizeForValidation(
  values: Record<string, unknown>,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(values).map(([key, value]) => {
      if (typeof File !== "undefined" && value instanceof File) {
        return [key, value.name];
      }
      if (
        Array.isArray(value) &&
        value.length > 0 &&
        value.every(
          (item) => typeof File !== "undefined" && item instanceof File,
        )
      ) {
        return [key, value.map((item: File) => item.name)];
      }
      return [key, value];
    }),
  );
}
