/**
 * 审批派发控制台两个页面测试的请求桩。
 *
 * 页面走**真实**的 `shell/routes.tsx` 路由与 `api.ts`，这里只替换 `fetch`：
 * 权限按身份投影用目录本身模拟（不给某个 Action = 这个身份没有那粒权限位）。
 * 未覆盖的请求直接抛错，避免测试悄悄走一条没人管的路径。
 */

import { vi } from "vitest";

/*
 * 预热本域两个懒加载页面模块（同 datasource harness 的理由：冷启动开销移出用例
 * 的计时窗口，否则第一个用例的 findBy* 预算会被 Vite 转换吃掉）。
 */
await import("@/features/feishu/views/ApprovalConfigsPage");
await import("@/features/feishu/views/ApprovalRequestsPage");

export type RecordedCall = {
  url: string;
  method: string;
  body: Record<string, unknown> | undefined;
};

type HandlerResult = unknown | Response | Promise<unknown | Response>;
type Handler = (body: Record<string, unknown>) => HandlerResult;

export type ApprovalApiStubOptions = {
  /// 目录里有没有这四粒权限位（不给了就等于「这个身份没有」）。
  approvalRead?: boolean;
  approvalWrite?: boolean;
  /// 向导的坐标/字段端点（list_bitable_*）归 `feishu.datasource.write`。
  datasourceWrite?: boolean;
  /// 各端点的响应。返回 Response 时原样使用（构造错误态）。
  configList?: Handler;
  createConfig?: Handler;
  updateConfig?: Handler;
  deleteConfig?: Handler;
  widgets?: Handler;
  requestList?: Handler;
  taskList?: Handler;
  bitableTables?: Handler;
  bitableFields?: Handler;
};

export function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function envelope(data: unknown, message = "成功"): Response {
  return jsonResponse({ code: 0, message, data });
}

function action(operationId: string, method: string, path: string) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method,
    path,
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function catalogFor(options: ApprovalApiStubOptions) {
  // revision 必须随权限组合变化：引擎的 CatalogCache.accept 在 revision 相同时
  // 直接复用上一份目录对象（同文件内多个测试会互相串味）。
  const flags = [
    (options.approvalRead ?? true) ? "1" : "0",
    (options.approvalWrite ?? true) ? "1" : "0",
    (options.datasourceWrite ?? true) ? "1" : "0",
  ].join("");
  const revision = `${flags}${"a".repeat(64)}`.slice(0, 64);
  const actions = [];
  if (options.approvalRead ?? true) {
    actions.push(
      action(
        "feishu.approval.list_configs",
        "POST",
        "/api/v1/feishu/approval/configs/query",
      ),
      action(
        "feishu.approval.list_requests",
        "POST",
        "/api/v1/feishu/approval/requests/query",
      ),
      action(
        "feishu.approval.list_tasks",
        "POST",
        "/api/v1/feishu/approval/tasks/query",
      ),
    );
  }
  if (options.approvalWrite ?? true) {
    actions.push(
      action(
        "feishu.approval.create_config",
        "POST",
        "/api/v1/feishu/approval/configs/create",
      ),
      action(
        "feishu.approval.update_config",
        "POST",
        "/api/v1/feishu/approval/configs/update",
      ),
      action(
        "feishu.approval.delete_config",
        "POST",
        "/api/v1/feishu/approval/configs/delete",
      ),
      action(
        "feishu.approval.list_widgets",
        "POST",
        "/api/v1/feishu/approval/definitions/widgets",
      ),
    );
  }
  if (options.datasourceWrite ?? true) {
    actions.push(
      action(
        "feishu.datasource.list_bitable_tables",
        "POST",
        "/api/v1/feishu/datasources/bitable-tables",
      ),
      action(
        "feishu.datasource.list_bitable_fields",
        "POST",
        "/api/v1/feishu/datasources/bitable-fields",
      ),
    );
  }
  return {
    code: 0,
    message: "成功",
    data: {
      schema_version: "2.3",
      revision,
      actions,
      table_views: [],
      modules: [],
    },
  };
}

const ME = {
  id: 7,
  username: "alice",
  email: "alice@example.com",
  email_verified_at: 1000,
  status: "active",
  created_at: 500,
  updated_at: 600,
};

function emptyPage() {
  return { items: [], page: 1, page_size: 10, total: 0 };
}

export function listPage(
  items: unknown[],
  overrides: { page?: number; pageSize?: number; total?: number | null } = {},
) {
  return {
    items,
    page: overrides.page ?? 1,
    page_size: overrides.pageSize ?? 10,
    total: overrides.total === undefined ? items.length : overrides.total,
  };
}

/// 后端线格式的配置行。键集与 `list_configs.rs` 的 `CONFIG_ITEM_COLUMNS` 一致。
export function approvalConfigWire(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id: 1,
    title: "差旅报销",
    base_token: "appbcbWCzen6",
    table_id: "tblsRc9GRRX",
    approval_code: "CODE-TRAVEL",
    applicant_field: "fldApp",
    backfill_field: "fldBack",
    base_timezone: "Asia/Shanghai",
    enabled: true,
    form_snapshot_at: 1758000000,
    updated_at: 1758000000,
    maps: [],
    ...overrides,
  };
}

/// 后端线格式的请求记录行（`list_requests.rs` 的 `REQUEST_ITEM_COLUMNS`）。
export function requestWire(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id: 1,
    requested_by: "张三",
    base_token: "appbcbWCzen6",
    table_id: "tblsRc9GRRX",
    config_id: 1,
    record_id: "recABC",
    request_body: JSON.stringify({
      base_token: "appbcbWCzen6",
      table_id: "tblsRc9GRRX",
      record_id: "recABC",
      requested_by: "张三",
    }),
    outcome: "succeeded",
    message: "审批单创建成功",
    serial_number: "SN-2026-0001",
    response_body: JSON.stringify({ instance_code: "inst-1" }),
    created_at: 1758000000,
    ...overrides,
  };
}

/// 后端线格式的任务行（`list_tasks.rs` 的 `TASK_ITEM_COLUMNS`）。
export function taskWire(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id: 1,
    config_id: 1,
    record_id: "recABC",
    state: "backfilled",
    instance_code: "inst-1",
    serial_number: "SN-2026-0001",
    attempts: 1,
    last_error: null,
    created_at: 1758000000,
    updated_at: 1758000000,
    ...overrides,
  };
}

async function respond(
  handler: Handler | undefined,
  body: Record<string, unknown>,
  fallback: unknown,
): Promise<Response> {
  const value = handler ? await handler(body) : fallback;
  if (value instanceof Response) return value;
  return envelope(value);
}

export function stubApprovalApi(
  options: ApprovalApiStubOptions = {},
): RecordedCall[] {
  const calls: RecordedCall[] = [];

  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = init?.method ?? "GET";
      let body: Record<string, unknown> | undefined;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body) as Record<string, unknown>;
        } catch {
          body = undefined;
        }
      }
      calls.push({ url, method, body });
      const payload = body ?? {};

      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse(catalogFor(options));
      }
      if (url.endsWith("/api/v1/users/me")) {
        return envelope(ME);
      }
      if (url.endsWith("/api/v1/feishu/approval/configs/query")) {
        return respond(options.configList, payload, emptyPage());
      }
      if (url.endsWith("/api/v1/feishu/approval/configs/create")) {
        return respond(options.createConfig, payload, { config_id: 7 });
      }
      if (url.endsWith("/api/v1/feishu/approval/configs/update")) {
        return respond(options.updateConfig, payload, { config_id: 1 });
      }
      if (url.endsWith("/api/v1/feishu/approval/configs/delete")) {
        return respond(options.deleteConfig, payload, {
          config_id: 1,
          deleted_field_maps: 2,
          deleted_pending_tasks: 1,
        });
      }
      if (url.endsWith("/api/v1/feishu/approval/definitions/widgets")) {
        return respond(options.widgets, payload, { widgets: [] });
      }
      if (url.endsWith("/api/v1/feishu/approval/requests/query")) {
        return respond(options.requestList, payload, emptyPage());
      }
      if (url.endsWith("/api/v1/feishu/approval/tasks/query")) {
        return respond(options.taskList, payload, emptyPage());
      }
      if (url.endsWith("/api/v1/feishu/datasources/bitable-tables")) {
        return respond(options.bitableTables, payload, { tables: [] });
      }
      if (url.endsWith("/api/v1/feishu/datasources/bitable-fields")) {
        return respond(options.bitableFields, payload, { fields: [] });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
    }),
  );

  return calls;
}

/// 某条端点被打了多少次。
export function countCalls(calls: RecordedCall[], suffix: string): number {
  return calls.filter((call) => call.url.endsWith(suffix)).length;
}

/// 某条端点的请求体序列。
export function bodiesOf(
  calls: RecordedCall[],
  suffix: string,
): Array<Record<string, unknown> | undefined> {
  return calls
    .filter((call) => call.url.endsWith(suffix))
    .map((call) => call.body);
}
