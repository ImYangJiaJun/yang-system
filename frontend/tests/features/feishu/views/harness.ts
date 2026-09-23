/**
 * 两个页面测试共用的请求桩：把飞书控制台会打到的 6 类端点一次搭好。
 *
 * 页面走的是**真实**的 `shell/routes.tsx` 路由（含 lazy 的 `.default` 约定）与真实的
 * `api.ts`，所以这里只替换 `fetch`：权限按身份投影这一点也用目录本身模拟
 * （不给某个 Action，就等于这个身份没有那粒权限位）。
 */

import { vi } from "vitest";

/*
 * 预热本域两个**懒加载**页面模块（路由表里是 `lazy: () => import(...)`）。
 *
 * 用例里第一次渲染该路由时，Vite 才去转换并求值这一整棵依赖图（页面 + 表单对话框的
 * react-hook-form、台账的 @tanstack/react-table、若干 Radix 组件）。这笔**每个测试文件
 * 一次**的冷启动开销落在该文件**第一个用例**的 `findBy*` 预算（RTL 默认 1000ms）里：
 * 实测插桩（渲染后到「页面挂上 / 空态落定」的时刻）
 *   预热前：catalog=596ms mount=610ms empty=638ms；另一跑 mount=875ms empty=1659ms
 *   预热后：catalog=199..267ms mount=208..281ms empty=231..312ms
 * 全量跑多 worker 抢 CPU 时这笔开销会涨到 1s 以上——断言就在页面还停在骨架行的时刻
 * 超时（失败现场 DOM 里工具栏与 `data-slot="skeleton"` 都在，`isPending` 仍为真）。
 *
 * 在夹具**加载期**（任何用例开始之前）先 import 一次，这笔开销就移出了用例的计时窗口。
 * 运行时路由照旧走自己的 `lazy: () => import(...)`（命中模块缓存），
 * 懒加载 + Suspense 那条路径仍然被覆盖；这里只是不让它替用例的断言计费。
 */
await import("@/features/feishu/views/DatasourceListPage");
await import("@/features/feishu/views/DatasourceDetailPage");

export type RecordedCall = {
  url: string;
  method: string;
  body: Record<string, unknown> | undefined;
};

/// 桩的返回：给 data 载荷（自动包信封），或者直接给一个 Response（错误态用）。
type HandlerResult = unknown | Response | Promise<unknown | Response>;
type Handler = (body: Record<string, unknown>) => HandlerResult;

export type FeishuApiStubOptions = {
  /// 目录里有没有这三粒权限位（不给了就等于「这个身份没有」）。
  datasourceRead?: boolean;
  datasourceWrite?: boolean;
  optionRead?: boolean;
  /// 各端点的响应。返回 Response 时原样使用（用来构造错误态）。
  datasourceList?: Handler;
  optionList?: Handler;
  createDatasource?: Handler;
  updateDatasource?: Handler;
  deleteDatasource?: Handler;
  approvalOptions?: Handler;
  /// 「立即拉取」的受理结果。后端在**发信号之前**就会拒掉拉不动的源，
  /// 所以错误态也走这里（返回 `Response`）。
  pullNow?: Handler;
  /// 自动拉取排程。`next_run_at` 为 `null` 表示「正在拉取 / 还没跑过第一轮」。
  pullSchedule?: Handler;
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

function action(
  operationId: string,
  method: string,
  path: string,
  requiresAuth = true,
) {
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
    requires_auth: requiresAuth,
  };
}

/// 部署中的 8 个 Action 里控制台相关的 6 个；`approval_options` 是 public 端点，
/// 任何身份都能看到它（所以它不参与权限门控）。
const APPROVAL_OPTIONS_ACTION = action(
  "feishu.option.approval_options",
  "POST",
  "/api/v1/feishu/approval/options/{source_key}",
  false,
);

function catalogFor(options: FeishuApiStubOptions) {
  // revision 必须随权限组合变化：引擎的 `CatalogCache.accept` 在 revision 相同时
  // 直接复用上一份目录对象（它描述的是应用定义版本），同文件内的多个测试会互相串味。
  // 它只能是 64 位十六进制，所以把三个权限位编码进前 3 位。
  const flags = [
    (options.datasourceRead ?? true) ? "1" : "0",
    (options.datasourceWrite ?? true) ? "1" : "0",
    (options.optionRead ?? true) ? "1" : "0",
  ].join("");
  const revision = `${flags}${"a".repeat(64)}`.slice(0, 64);
  const actions = [APPROVAL_OPTIONS_ACTION];
  if (options.datasourceRead ?? true) {
    actions.push(
      action(
        "feishu.datasource.list_datasources",
        "POST",
        "/api/v1/feishu/datasources/query",
      ),
    );
  }
  if (options.datasourceWrite ?? true) {
    actions.push(
      action(
        "feishu.datasource.create_datasource",
        "POST",
        "/api/v1/feishu/datasources",
      ),
      action(
        "feishu.datasource.update_datasource",
        "PUT",
        "/api/v1/feishu/datasources",
      ),
      action(
        "feishu.datasource.delete_datasource",
        "DELETE",
        "/api/v1/feishu/datasources",
      ),
      // 服务端只在 can_pull()（出站凭证齐备）时才注册这一个。目录里没有它，
      // 就等于这个部署没开导出站拉取——页面据此不渲染按钮。
      action(
        "feishu.datasource.pull_now",
        "POST",
        "/api/v1/feishu/datasources/pull-now",
      ),
    );
  }
  if (options.datasourceRead ?? true) {
    actions.push(
      action(
        "feishu.datasource.pull_schedule",
        "POST",
        "/api/v1/feishu/datasources/pull-schedule",
      ),
    );
  }
  if (options.optionRead ?? true) {
    actions.push(
      action(
        "feishu.option.list_options",
        "POST",
        "/api/v1/feishu/options/query",
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

/// 空的一页：`total: 0` 表示服务端确实回了一个空的完整结果集。
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

/// 后端线格式的数据源行。
export function datasourceWire(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    source_key: "dept_sales",
    title: "部门",
    encrypt_enabled: false,
    default_locale: "zh_cn",
    status: "active",
    updated_at: 1758000000,
    ...overrides,
  };
}

/// 后端线格式的选项行。
export function optionWire(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    option_id: "travel",
    source_key: "expense_category",
    label: "差旅费",
    i18n: null,
    sort_order: 1,
    is_default: false,
    enabled: true,
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

/**
 * 装上 fetch 桩，返回记录下来的调用（url + method + 解析后的 body）。
 * 没覆盖到的请求直接抛错——避免测试悄悄走了一条没人管的路径。
 */
export function stubFeishuApi(
  options: FeishuApiStubOptions = {},
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
      if (url.endsWith("/api/v1/feishu/datasources/query")) {
        return respond(options.datasourceList, payload, emptyPage());
      }
      if (url.endsWith("/api/v1/feishu/datasources/pull-now")) {
        return respond(options.pullNow, payload, { accepted: true });
      }
      if (url.endsWith("/api/v1/feishu/datasources/pull-schedule")) {
        return respond(options.pullSchedule, payload, {
          interval_seconds: 900,
          next_run_at: null,
        });
      }
      if (url.endsWith("/api/v1/feishu/options/query")) {
        return respond(options.optionList, payload, emptyPage());
      }
      if (url.endsWith("/api/v1/feishu/datasources")) {
        if (method === "POST") {
          return respond(options.createDatasource, payload, {
            source_key: payload.source_key,
          });
        }
        if (method === "PUT") {
          return respond(options.updateDatasource, payload, { affected: 1 });
        }
        if (method === "DELETE") {
          return respond(options.deleteDatasource, payload, {
            deleted: 1,
            disabled_options: 0,
          });
        }
      }
      if (url.includes("/api/v1/feishu/approval/options/")) {
        return respond(options.approvalOptions, payload, {
          result: { options: [] },
        });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
    }),
  );

  return calls;
}

/// 某条端点被打了多少次（断言「切视图不重新发请求」这类行为用）。
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
