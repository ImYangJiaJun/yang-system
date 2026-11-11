import {
  ApiError,
  apiBase,
  parseJson,
  requestWithTokenRefresh,
  withRequestDeadline,
} from "@/engine";
import { compareProductText } from "@/shared/lib/product-locale";

/**
 * 批量审批（飞书用户态）的数据访问层。
 *
 * 两条硬约束：
 * 1. 所有请求都是**当前用户的系统会话 Bearer**；飞书 token 只在服务端使用，
 *    前端拿不到也不应该拿到。
 * 2. 本文件不 import 其他 features 域：账号身份重新授权只调用同一个 HTTP 端点，
 *    不引用 `features/account`（域间禁止互相 import）。
 *
 * 端点（设计 §6.2）：
 *   GET  /api/v1/feishu/batch-approval/tasks
 *   GET  /api/v1/feishu/batch-approval/tasks/{task_id}?instance_code=...
 *   GET  /api/v1/feishu/batch-approval/users/search?q=...
 *   POST /api/v1/feishu/batch-approval/jobs
 *   GET  /api/v1/feishu/batch-approval/jobs/{id}
 *   POST /api/v1/feishu/batch-approval/jobs/{id}/cancel
 */

export type BatchApprovalAction =
  "pass" | "refuse" | "forward" | "rollback" | "add_sign";

export const BATCH_ACTION_LABELS: Record<BatchApprovalAction, string> = {
  pass: "同意",
  refuse: "拒绝",
  forward: "转交",
  rollback: "退回",
  add_sign: "加签",
};

/// `/tasks` 的任务状态（topic/status）：数字语义来自审批查询文档
/// （approval-search/query.md）：1 待办、2 已办、17 未读、18 已读、33 处理中、34 撤回。
const FEISHU_TASK_STATUS_LABELS: Record<string, string> = {
  "1": "待办",
  "2": "已办",
  "17": "未读",
  "18": "已读",
  "33": "处理中",
  "34": "撤回",
  PENDING: "待办",
  RUNNING: "处理中",
  TODO: "待办",
  DONE: "已办",
};

/// 审批实例状态（process_status）：0 无流程状态、1 流转中、2 已通过、
/// 3 已拒绝、4 已撤销、5 已终止（approval-search/query.md）。
const FEISHU_PROCESS_STATUS_LABELS: Record<string, string> = {
  "0": "无流程状态",
  "1": "审批中",
  "2": "已通过",
  "3": "已拒绝",
  "4": "已撤销",
  "5": "已终止",
  PENDING: "审批中",
  RUNNING: "审批中",
  APPROVED: "已通过",
  REJECTED: "已拒绝",
  CANCELED: "已撤销",
  CANCELLED: "已撤销",
  DELETED: "已删除",
  TERMINATED: "已终止",
  TRANSFERRED: "已转交",
  COMPLETED: "已完成",
  DONE: "已完成",
  HIDDEN: "状态隐藏",
};

function mappedStatus(
  value: string | null | undefined,
  labels: Record<string, string>,
): string {
  const raw = value?.trim();
  if (!raw) return "—";
  const key = raw.toUpperCase();
  return labels[key] ?? FEISHU_PROCESS_STATUS_LABELS[key] ?? `状态 ${raw}`;
}

export function feishuTaskStatusLabel(
  value: string | null | undefined,
): string {
  return mappedStatus(value, FEISHU_TASK_STATUS_LABELS);
}

export function feishuProcessStatusLabel(
  value: string | null | undefined,
): string {
  return mappedStatus(value, FEISHU_PROCESS_STATUS_LABELS);
}

/// 兼容详情接口里的字符串状态；数字优先按实例状态解释。
export function feishuStatusLabel(value: string | null | undefined): string {
  return mappedStatus(value, {});
}

/// 列表列「实例状态」：优先实例状态；没有时退回任务分组/任务状态。
export function taskStatusLabel(task: BatchTask): string {
  return task.instanceStatus
    ? feishuProcessStatusLabel(task.instanceStatus)
    : feishuTaskStatusLabel(task.topic ?? task.status);
}

export const FEISHU_STATUS_FILTER_OPTIONS = [
  { value: "all", label: "全部状态" },
  { value: "待办", label: "待办" },
  { value: "已办", label: "已办" },
  { value: "未读", label: "未读" },
  { value: "已读", label: "已读" },
  { value: "处理中", label: "处理中" },
  { value: "撤回", label: "撤回" },
  { value: "审批中", label: "审批中" },
  { value: "已通过", label: "已通过" },
  { value: "已拒绝", label: "已拒绝" },
  { value: "已撤销", label: "已撤销" },
  { value: "已终止", label: "已终止" },
  { value: "已完成", label: "已完成" },
] as const;

export type BatchTask = {
  taskId: string;
  instanceCode: string;
  definitionCode: string;
  definitionName: string;
  title: string;
  initiatorName: string | null;
  summaries: string[];
  supportApiOperate: boolean;
  /// 外部审批（instance_external_id / task_external_id 非空）不能在用户态 API 处理。
  external: boolean;
  instanceExternalId: string | null;
  taskExternalId: string | null;
  /// 任务分组（飞书 `/tasks` 的 `topic`：1=待办，2=已办，17/18=通知）。
  topic: string | null;
  /// 审批实例状态（飞书通常返回 Running/Approved/Rejected 等）。
  instanceStatus: string | null;
  /// 任务状态（飞书可能返回 Todo/Done 或 PENDING 等）。
  status: string;
  link: string | null;
  /// 列表时间（毫秒）；后端任务模型没有稳定时间字段时可能为 null。
  createdAt: number | null;
};

export type BatchTaskGroup = {
  key: string;
  code: string;
  name: string;
  tasks: BatchTask[];
};

export type BatchTaskFormField = {
  name: string;
  value: unknown;
};

export type BatchTaskTimelineEntry = {
  title: string;
  status: string | null;
  userName: string | null;
  time: number | null;
  comment: string | null;
};

export type BatchTaskDetail = {
  taskId: string;
  instanceCode: string;
  title: string;
  definitionName: string;
  initiatorName: string | null;
  status: string | null;
  form: BatchTaskFormField[];
  timeline: BatchTaskTimelineEntry[];
  currentNodeNames: string[];
};

export type FeishuUser = {
  openId: string;
  name: string;
  department: string | null;
};

export type BatchJobItem = {
  taskId: string;
  instanceCode: string;
  title: string | null;
  status: string;
  resultCode: number | null;
  message: string | null;
  beforeStatus: string | null;
  afterStatus: string | null;
};

export type BatchJob = {
  id: string;
  status: string;
  total: number;
  succeeded: number;
  failed: number;
  unknown: number;
  items: BatchJobItem[];
};

export type BatchJobItemInput = {
  taskId: string;
  instanceCode: string;
};

export type BatchJobParams = {
  comment?: string;
  transferOpenId?: string;
  addSignOpenIds?: string[];
  addSignType?: number;
  approvalMethod?: number;
  rollbackMode?: "initiator" | "previous";
};

export type BatchJobRequest = BatchJobParams & {
  action: BatchApprovalAction;
  items: BatchJobItemInput[];
  idempotencyKey: string;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

function recordOf(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function textOf(value: unknown): string | null {
  if (typeof value === "string") return value.trim() ? value.trim() : null;
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return null;
}

function firstText(
  data: Record<string, unknown> | undefined,
  ...keys: string[]
): string | null {
  for (const key of keys) {
    const value = textOf(data?.[key]);
    if (value !== null) return value;
  }
  return null;
}

function numberField(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && /^-?\d+$/.test(value.trim())) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

/// 飞书时间字段可能是秒、毫秒或 ISO 串；统一成毫秒给产品 locale 格式化。
function timestampOf(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value < 1e12 ? value * 1000 : value;
  }
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (/^\d+$/.test(trimmed)) return timestampOf(Number(trimmed));
    const parsed = Date.parse(trimmed);
    return Number.isNaN(parsed) ? null : parsed;
  }
  return null;
}

function stringArrayOf(value: unknown): string[] {
  if (!Array.isArray(value)) {
    const single = textOf(value);
    return single ? [single] : [];
  }
  return value
    .map((entry) => {
      if (typeof entry === "string") return entry.trim();
      const record = recordOf(entry);
      return (
        firstText(record, "text", "value", "name", "title", "summary") ?? ""
      );
    })
    .filter((entry) => entry.length > 0);
}

async function request(
  path: string,
  options: { method: "GET" | "POST" | "DELETE"; body?: unknown },
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<unknown> {
  return withRequestDeadline(async (signal) => {
    const hasBody = options.body !== undefined;
    const response = await requestWithTokenRefresh(
      accessToken,
      (token) =>
        fetch(`${apiBase}${path}`, {
          method: options.method,
          headers: {
            Accept: "application/json",
            ...(hasBody ? { "Content-Type": "application/json" } : {}),
            ...(token ? { Authorization: `Bearer ${token}` } : {}),
          },
          ...(hasBody ? { body: JSON.stringify(options.body) } : {}),
          credentials: "include",
          signal,
        }),
      signal,
    );
    const requestId = response.headers.get("x-request-id") ?? undefined;
    const payload = (await parseJson(response)) as ApiEnvelope | undefined;
    if (!response.ok || payload?.code !== 0) {
      throw new ApiError(payload?.message ?? `HTTP ${response.status}`, {
        status: response.status,
        code: payload?.code,
        requestId,
        details: payload,
      });
    }
    return payload.data;
  }, signal);
}

function parseTask(raw: unknown): BatchTask | null {
  const record = recordOf(raw);
  const taskId = firstText(record, "task_id", "id");
  const instanceCode = firstText(record, "instance_code");
  if (!taskId || !instanceCode) return null;
  const instanceExternalId = firstText(record, "instance_external_id");
  const taskExternalId = firstText(record, "task_external_id");
  return {
    taskId,
    instanceCode,
    definitionCode: firstText(record, "definition_code") ?? "",
    definitionName:
      firstText(record, "definition_name", "approval_name") ?? "未命名审批",
    title: firstText(record, "title", "instance_title") ?? "（无标题）",
    initiatorName: firstText(record, "initiator_name", "start_user_name"),
    summaries: stringArrayOf(record?.summaries),
    // 字段缺失按可操作处理（§6.3）；写失败时后端会给出逐条错误。
    supportApiOperate: record?.support_api_operate !== false,
    external: Boolean(instanceExternalId || taskExternalId),
    instanceExternalId,
    taskExternalId,
    topic: firstText(record, "topic"),
    instanceStatus: firstText(record, "instance_status"),
    status: firstText(record, "status") ?? "PENDING",
    link: firstText(record, "link"),
    createdAt: timestampOf(
      record?.create_time ??
        record?.created_at ??
        record?.update_time ??
        record?.update_at,
    ),
  };
}

/// 拉取当前用户的全部待办。服务端聚合分页；超过上限时 `truncated=true`。
export async function fetchBatchTasks(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<{ tasks: BatchTask[]; truncated: boolean }> {
  const data = await request(
    "/api/v1/feishu/batch-approval/tasks",
    { method: "GET" },
    accessToken,
    signal,
  );
  const container = recordOf(data);
  const rawTasks = container?.tasks;
  const rawItems = container?.items;
  const rows = Array.isArray(data)
    ? data
    : Array.isArray(rawTasks)
      ? rawTasks
      : Array.isArray(rawItems)
        ? rawItems
        : [];
  const tasks = rows
    .map(parseTask)
    .filter((task): task is BatchTask => task !== null);
  return { tasks, truncated: container?.truncated === true };
}

/// 定义分组键：Code + Name。Code 缺失时仍按 Name 分组，避免整页堆在一起。
export function definitionKey(task: BatchTask): string {
  return `${task.definitionCode}\u0000${task.definitionName}`;
}

export function groupBatchTasks(tasks: BatchTask[]): BatchTaskGroup[] {
  const groups = new Map<string, BatchTaskGroup>();
  for (const task of tasks) {
    const key = definitionKey(task);
    const existing = groups.get(key);
    if (existing) {
      existing.tasks.push(task);
    } else {
      groups.set(key, {
        key,
        code: task.definitionCode,
        name: task.definitionName,
        tasks: [task],
      });
    }
  }
  return [...groups.values()].sort((left, right) =>
    compareProductText(left.name, right.name),
  );
}

export function taskOperability(task: BatchTask): {
  operable: boolean;
  reason: string | null;
} {
  if (task.external) {
    return { operable: false, reason: "外部审批，需在飞书侧处理" };
  }
  if (!task.supportApiOperate) {
    return { operable: false, reason: "该审批不支持 API 操作" };
  }
  if (task.status.toUpperCase() !== "PENDING") {
    return {
      operable: false,
      reason: `任务状态为 ${task.status}，已不在待办中`,
    };
  }
  return { operable: true, reason: null };
}

function parseDetailForm(value: unknown): BatchTaskFormField[] {
  let rows = Array.isArray(value) ? value : [];
  if (typeof value === "string") {
    try {
      const parsed = JSON.parse(value) as unknown;
      if (Array.isArray(parsed)) rows = parsed;
    } catch {
      rows = [];
    }
  }
  return rows.map((raw, index) => {
    const record = recordOf(raw);
    return {
      name:
        firstText(record, "name", "label", "custom_id", "title") ??
        `字段 ${index + 1}`,
      value:
        record && "value" in record
          ? record.value
          : record && "field_value" in record
            ? record.field_value
            : raw,
    };
  });
}

const TIMELINE_TYPE_LABELS: Record<string, string> = {
  START: "发起审批",
  PASS: "同意",
  APPROVE: "同意",
  REJECT: "拒绝",
  REFUSE: "拒绝",
  TRANSFER: "转交",
  TRANSFERRED: "已转交",
  ADD_SIGN: "加签",
  ROLLBACK: "退回",
  CANCEL: "撤销",
  CANCELED: "撤销",
  DELETE: "删除",
  CC: "抄送",
};

function parseTimelineEntry(raw: unknown): BatchTaskTimelineEntry {
  const record = recordOf(raw);
  const type = firstText(record, "type", "node_type");
  const typeLabel = type
    ? (TIMELINE_TYPE_LABELS[type.toUpperCase()] ?? type)
    : null;
  return {
    title:
      firstText(record, "node_name", "task_name", "name") ??
      typeLabel ??
      "审批节点",
    status: firstText(record, "status", "state") ?? typeLabel,
    userName: firstText(record, "user_name", "user_id", "open_id"),
    time: timestampOf(
      record?.create_time ??
        record?.end_time ??
        record?.start_time ??
        record?.timestamp,
    ),
    comment: firstText(record, "comment", "remark", "message"),
  };
}

/// 实例详情：仅用于抽查/批量确认前展开，不做逐条预拉。
export async function fetchBatchTaskDetail(
  taskId: string,
  instanceCode: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<BatchTaskDetail> {
  const data = await request(
    `/api/v1/feishu/batch-approval/tasks/${encodeURIComponent(taskId)}?instance_code=${encodeURIComponent(instanceCode)}`,
    { method: "GET" },
    accessToken,
    signal,
  );
  const record = recordOf(data);
  // 实例详情接口（GET /instances/{code}）返回 task_list；旧的 detail 接口返回
  // timeline/operation_records。两者都可用于时间线，优先用官方的 task_list。
  const rawTimeline =
    record?.timeline ?? record?.operation_records ?? record?.task_list;
  const timeline = Array.isArray(rawTimeline)
    ? rawTimeline.map(parseTimelineEntry)
    : [];
  const rawCurrentNodes = record?.current_nodes;
  const currentNodeNames = Array.isArray(rawCurrentNodes)
    ? rawCurrentNodes
        .map((node) => {
          const nodeRecord = recordOf(node);
          return firstText(nodeRecord, "name", "node_name", "node_id");
        })
        .filter((name): name is string => name !== null)
    : [];
  // 实例详情没有 current_nodes 时，task_list 中 PENDING 的任务就是当前待办节点。
  const pendingNodeNames = Array.isArray(record?.task_list)
    ? record.task_list
        .map((node) => {
          const nodeRecord = recordOf(node);
          const status = firstText(nodeRecord, "status")?.toUpperCase();
          return status === "PENDING"
            ? firstText(nodeRecord, "node_name", "name")
            : null;
        })
        .filter((name): name is string => name !== null)
    : [];
  const uniqueCurrentNodes = [
    ...new Set([...currentNodeNames, ...pendingNodeNames]),
  ];
  return {
    taskId,
    instanceCode,
    title:
      firstText(record, "title", "instance_title", "approval_name") ??
      firstText(record, "definition_name") ??
      "审批详情",
    definitionName:
      firstText(record, "definition_name", "approval_name") ?? "未命名审批",
    initiatorName: firstText(
      record,
      "initiator_name",
      "start_user_name",
      "user_name",
    ),
    status: firstText(record, "status", "instance_status"),
    form: parseDetailForm(record?.form),
    timeline,
    currentNodeNames: uniqueCurrentNodes,
  };
}

/// 转交/加签选人：飞书通讯录搜索（系统账号 UserPicker 不能用于这里）。
export async function searchFeishuUsers(
  query: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<FeishuUser[]> {
  const trimmed = query.trim();
  if (!trimmed) return [];
  const data = await request(
    `/api/v1/feishu/batch-approval/users/search?q=${encodeURIComponent(trimmed)}`,
    { method: "GET" },
    accessToken,
    signal,
  );
  const container = recordOf(data);
  const rawUsers = container?.users;
  const rawItems = container?.items;
  const rows = Array.isArray(data)
    ? data
    : Array.isArray(rawUsers)
      ? rawUsers
      : Array.isArray(rawItems)
        ? rawItems
        : [];
  return rows
    .map((raw) => {
      const record = recordOf(raw);
      const openId = firstText(record, "open_id", "user_id");
      const name = firstText(record, "name", "user_name");
      if (!openId || !name) return null;
      const departmentNames = record?.department_names;
      const department =
        firstText(record, "department", "department_name") ??
        (Array.isArray(departmentNames)
          ? stringArrayOf(departmentNames).join(" / ")
          : null);
      return { openId, name, department };
    })
    .filter((user): user is FeishuUser => user !== null);
}

export function newIdempotencyKey(): string {
  const cryptoApi = globalThis.crypto;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return cryptoApi.randomUUID();
  }
  // 老浏览器/非安全上下文的兜底：只要在 (user_id, idempotency_key) 内唯一即可。
  return `job-${Date.now()}-${Math.random().toString(36).slice(2, 12)}`;
}

/// 创建批量 job。items 必须同时带 task_id 与 instance_code（§6.5）。
export async function createBatchJob(
  input: BatchJobRequest,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<string> {
  const data = await request(
    "/api/v1/feishu/batch-approval/jobs",
    {
      method: "POST",
      body: {
        action: input.action,
        items: input.items.map((item) => ({
          task_id: item.taskId,
          instance_code: item.instanceCode,
        })),
        idempotency_key: input.idempotencyKey,
        ...(input.comment ? { comment: input.comment } : {}),
        ...(input.transferOpenId
          ? { transfer_open_id: input.transferOpenId }
          : {}),
        ...(input.addSignOpenIds && input.addSignOpenIds.length > 0
          ? { add_sign_open_ids: input.addSignOpenIds }
          : {}),
        ...(input.addSignType !== undefined
          ? { add_sign_type: input.addSignType }
          : {}),
        ...(input.approvalMethod !== undefined
          ? { approval_method: input.approvalMethod }
          : {}),
        ...(input.rollbackMode ? { rollback_mode: input.rollbackMode } : {}),
      },
    },
    accessToken,
    signal,
  );
  const record = recordOf(data);
  const jobId = firstText(record, "job_id", "id");
  if (!jobId) {
    throw new ApiError("创建批量任务响应缺少 job_id", { status: 200 });
  }
  return jobId;
}

function parseJobItem(raw: unknown): BatchJobItem | null {
  const record = recordOf(raw);
  const taskId = firstText(record, "task_id");
  const instanceCode = firstText(record, "instance_code");
  if (!taskId || !instanceCode) return null;
  return {
    taskId,
    instanceCode,
    title: firstText(record, "title", "definition_name"),
    status: firstText(record, "status") ?? "pending",
    resultCode: numberField(record?.result_code),
    message: firstText(record, "message"),
    beforeStatus: firstText(record, "before_status"),
    afterStatus: firstText(record, "after_status"),
  };
}

export async function fetchBatchJob(
  jobId: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<BatchJob> {
  const data = await request(
    `/api/v1/feishu/batch-approval/jobs/${encodeURIComponent(jobId)}`,
    { method: "GET" },
    accessToken,
    signal,
  );
  const record = recordOf(data);
  if (!record) {
    throw new ApiError("批量任务响应缺少任务信息", { status: 200 });
  }
  const rawItems = record.items;
  const items = Array.isArray(rawItems)
    ? rawItems
        .map(parseJobItem)
        .filter((item): item is BatchJobItem => item !== null)
    : [];
  const countOf = (key: string): number => {
    const value = numberField(record[key]);
    if (value !== null) return value;
    if (key === "total") return items.length;
    if (key === "succeeded") {
      return items.filter((item) => item.status === "succeeded").length;
    }
    if (key === "failed") {
      return items.filter((item) => item.status === "failed").length;
    }
    return items.filter((item) => item.status === "unknown").length;
  };
  return {
    id: firstText(record, "job_id", "id") ?? jobId,
    status: firstText(record, "status") ?? "pending",
    total: countOf("total"),
    succeeded: countOf("succeeded"),
    failed: countOf("failed"),
    unknown: countOf("unknown"),
    items,
  };
}

export async function cancelBatchJob(
  jobId: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<void> {
  await request(
    `/api/v1/feishu/batch-approval/jobs/${encodeURIComponent(jobId)}/cancel`,
    { method: "POST", body: {} },
    accessToken,
    signal,
  );
}

/// 当前用户资料（批量页是独立页，需要自行展示「当前用户」）。
export async function fetchCurrentUsername(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<string | null> {
  const data = await request(
    "/api/v1/users/me",
    { method: "GET" },
    accessToken,
    signal,
  );
  return firstText(recordOf(data), "username");
}

/// 重新授权飞书：返回授权页 URL。批量页在凭证过期/未绑定时使用。
export async function requestFeishuAuthorizationUrl(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<string> {
  const data = await request(
    "/api/v1/users/me/identities/feishu/authorize",
    { method: "POST", body: {} },
    accessToken,
    signal,
  );
  const url = firstText(recordOf(data), "authorize_url");
  if (!url) {
    throw new ApiError("重新授权响应缺少飞书授权地址", { status: 200 });
  }
  return url;
}

const REAUTH_CODES = new Set([20026, 20037, 20064, 20073, 99991679]);

export function isFeishuReauthError(cause: unknown): boolean {
  if (!(cause instanceof ApiError)) return false;
  if (cause.code !== undefined && REAUTH_CODES.has(cause.code)) return true;
  return /未绑定飞书|重新授权|授权已过期|reauth_required/i.test(cause.message);
}

export function isFeishuScopeError(cause: unknown): boolean {
  return cause instanceof ApiError && cause.code === 99991672;
}

export function describeBatchError(cause: unknown): string {
  if (isFeishuScopeError(cause)) {
    return "飞书应用未开通审批权限，请联系管理员在开放平台开通并发布版本。";
  }
  if (isFeishuReauthError(cause)) {
    return "飞书授权已过期或尚未绑定，请重新授权后再试。";
  }
  if (cause instanceof ApiError && cause.status === 428) {
    return "该操作需要二次身份验证，请重新登录后再试。";
  }
  return cause instanceof Error ? cause.message : "操作失败，请稍后重试";
}
