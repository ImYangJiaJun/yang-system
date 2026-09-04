import { describe, expect, it } from "vitest";

import openapiDocument from "../../contracts/openapi.json";
import { compileDynamicSchema } from "./ajv";

/**
 * ADR-4 §2.2 单向覆盖断言：后端能下发的全部输入 Schema（每个 operation 的
 * requestBody 与 parameters），前端动态校验轨必须全部能编译。
 *
 * 反向方向“前端白名单拒绝的后端必拒绝”不在此断言：后端下发 Schema 由 Rust 侧
 * Catalog 投影生成并以后端自身校验测试为准，本测试只锁定前向兼容性。
 */

const HTTP_METHODS = ["get", "post", "put", "patch", "delete"] as const;

type JsonSchemaObject = Record<string, unknown>;

type Operation = {
  parameters?: Array<{ name?: string; schema?: JsonSchemaObject }>;
  requestBody?: { content?: Record<string, { schema?: JsonSchemaObject }> };
};

type InputSchemaCase = {
  operation: string;
  source: string;
  schema: JsonSchemaObject;
};

function collectInputSchemas(): {
  operationCount: number;
  cases: InputSchemaCase[];
} {
  const cases: InputSchemaCase[] = [];
  let operationCount = 0;
  for (const [path, pathItem] of Object.entries(openapiDocument.paths)) {
    for (const method of HTTP_METHODS) {
      const operation = (pathItem as Record<string, Operation | undefined>)[
        method
      ];
      if (!operation) continue;
      operationCount += 1;
      const label = `${method.toUpperCase()} ${path}`;
      for (const parameter of operation.parameters ?? []) {
        if (parameter.schema) {
          cases.push({
            operation: label,
            source: `parameter ${parameter.name ?? "<unnamed>"}`,
            schema: parameter.schema,
          });
        }
      }
      for (const [mediaType, media] of Object.entries(
        operation.requestBody?.content ?? {},
      )) {
        if (media.schema) {
          cases.push({
            operation: label,
            source: `requestBody ${mediaType}`,
            schema: media.schema,
          });
        }
      }
    }
  }
  return { operationCount, cases };
}

describe("OpenAPI 输入 Schema 前端可编译性", () => {
  const { operationCount, cases } = collectInputSchemas();

  it("契约快照覆盖后端全部 18 个业务 endpoint", () => {
    expect(operationCount).toBe(18);
    expect(cases.length).toBeGreaterThanOrEqual(operationCount);
  });

  it.each(
    cases.map((entry) => [entry.operation, entry.source, entry] as const),
  )("%s（%s）的输入 Schema 通过白名单编译", (_operation, _source, entry) => {
    expect(() => compileDynamicSchema(entry.schema)).not.toThrow();
  });
});
