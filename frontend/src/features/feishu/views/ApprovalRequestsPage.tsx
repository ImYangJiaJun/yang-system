/**
 * 派发记录页（路由 `/feishu/approval/requests`，路由级 lazy → 必须 default 导出）。
 *
 * 列表列：时间 / 请求人 / 坐标 / record_id / outcome 色标 / message；
 * 行展开：请求体与返回体 JSON 原文（`pre` 标签，含 serial_number）；
 * 批量行（outcome=accepted）提供「查看任务」下钻 `list_tasks`（按 config_id 过滤，
 * 展示 record_id / state / instance_code / serial_number / last_error）；
 * 筛选：base_token / table_id / outcome + 分页（复用 ListPagination）。
 *
 * 空态口径同配置页：「total 为 0」才有资格说没有记录。
 */

import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Search } from "lucide-react";

import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableFooter,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import {
  useApprovalActions,
  useApprovalRequestList,
  useApprovalTasks,
} from "../api";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge, type ToneName } from "../components/StatusBadge";
import { useDebouncedValue } from "../list-query";
import type {
  ApprovalOutcome,
  ApprovalRequestItem,
  ApprovalRequestsQuery,
  ApprovalTaskItem,
  OrderByClause,
} from "../types";
import {
  APPROVAL_OUTCOME_OPTIONS,
  approvalOutcomeLabel,
  formatUnixSeconds,
} from "../types";

/// outcome → 色标（设计：succeeded 绿 / waiting 黄 / accepted 蓝 / failed 红）。
const OUTCOME_TONE: Record<string, ToneName> = {
  succeeded: "positive",
  waiting: "warning",
  accepted: "info",
  failed: "danger",
};

function toneOf(outcome: string): ToneName {
  return OUTCOME_TONE[outcome] ?? "neutral";
}

/// 展开态：`id` 为展开的请求行，`configId` 给「查看任务」下钻用。
type ExpandedState = {
  id: number;
  configId: number | null;
};

/// 记录页默认排序：请求时间倒序 + 主键收尾（后端未给排序时的兜底也是同一对键）。
const REQUESTS_ORDER_BY: OrderByClause[] = [
  { field: "created_at", direction: "Desc" },
  { field: "id", direction: "Asc" },
];

export default function ApprovalRequestsPage() {
  const [baseToken, setBaseToken] = useState("");
  const [tableId, setTableId] = useState("");
  // 两个坐标筛选是等值过滤（非搜索），逐键发请求没意义，去抖 300ms。
  const debouncedBaseToken = useDebouncedValue(baseToken, 300);
  const debouncedTableId = useDebouncedValue(tableId, 300);
  const [outcome, setOutcome] = useState<ApprovalOutcome | "all">("all");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(10);

  const [expanded, setExpanded] = useState<ExpandedState | null>(null);

  const query = useMemo<ApprovalRequestsQuery>(
    () => ({
      page,
      pageSize,
      baseToken: debouncedBaseToken,
      tableId: debouncedTableId,
      outcome,
      orderBy: REQUESTS_ORDER_BY,
    }),
    [page, pageSize, debouncedBaseToken, debouncedTableId, outcome],
  );

  const actions = useApprovalActions();
  const listQuery = useApprovalRequestList(query);
  // 「查看任务」下钻：只在展开行是批量受理（configId 非空）时才发请求。
  const tasksQuery = useApprovalTasks(expanded?.configId ?? null);

  const items = useMemo(() => listQuery.data?.items ?? [], [listQuery.data]);
  const total = listQuery.data?.total ?? null;
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  const settled =
    !listQuery.isPending && !listQuery.isError && !listQuery.isPlaceholderData;
  const emptyPage = settled && items.length === 0;
  const pageOutOfRange = emptyPage && lastPage !== null && page > lastPage;

  useEffect(() => {
    if (pageOutOfRange && lastPage !== null) setPage(lastPage);
  }, [pageOutOfRange, lastPage, setPage]);

  function toggleExpanded(item: ApprovalRequestItem) {
    setExpanded((current) =>
      current?.id === item.id
        ? null
        : {
            id: item.id,
            // 只有批量行有「查看任务」，其余行展开纯看 JSON，不触发任务查询。
            configId: item.outcome === "accepted" ? item.configId : null,
          },
    );
  }

  if (!actions.canRead) {
    return (
      <div className="space-y-4 p-6">
        <h1 className="text-lg font-semibold">派发记录</h1>
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          当前身份没有查看派发记录的权限，请联系运维管理员开通。
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4 p-6">
      <div>
        <h1 className="text-lg font-semibold">派发记录</h1>
        <p className="text-sm text-muted-foreground">
          每一次派发入口请求一行：成功 / 等待 / 批量受理 /
          失败四桶，行内展开看请求与返回原文。
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <div className="relative">
          <Search
            className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            value={baseToken}
            onChange={(event) => {
              setBaseToken(event.target.value);
              setPage(1);
            }}
            placeholder="按 Base Token 筛选"
            aria-label="按 Base Token 筛选"
            className="h-8 w-48 pl-7"
            autoComplete="off"
          />
        </div>
        <Input
          value={tableId}
          onChange={(event) => {
            setTableId(event.target.value);
            setPage(1);
          }}
          placeholder="按 table_id 筛选"
          aria-label="按 table_id 筛选"
          className="h-8 w-40"
          autoComplete="off"
        />
        <Select
          value={outcome}
          onValueChange={(value) => {
            setOutcome(value as ApprovalOutcome | "all");
            setPage(1);
          }}
        >
          <SelectTrigger size="sm" aria-label="结果筛选" className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {APPROVAL_OUTCOME_OPTIONS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {listQuery.isPending && !settled ? (
        <p className="text-sm text-muted-foreground">加载中…</p>
      ) : listQuery.isError ? (
        <p className="text-sm text-destructive">
          {listQuery.error instanceof Error
            ? listQuery.error.message
            : String(listQuery.error)}
        </p>
      ) : items.length === 0 && total === 0 ? (
        <p className="text-sm text-muted-foreground">
          还没有派发记录。记录会随每一次派发请求产生（含失败请求）。
        </p>
      ) : (
        <div className="space-y-3">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8" />
                <TableHead>时间</TableHead>
                <TableHead>请求人</TableHead>
                <TableHead>坐标</TableHead>
                <TableHead>record_id</TableHead>
                <TableHead>结果</TableHead>
                <TableHead>说明</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item) => {
                const open = expanded?.id === item.id;
                return (
                  <FragmentRow
                    key={item.id}
                    item={item}
                    open={open}
                    tasksQuery={tasksQuery}
                    onToggle={() => toggleExpanded(item)}
                  />
                );
              })}
            </TableBody>
          </Table>
          <ListPagination
            page={page}
            pageSize={pageSize}
            total={total}
            pending={listQuery.isPending}
            onPageChange={setPage}
            onPageSizeChange={(next) => {
              setPageSize(next);
              setPage(1);
            }}
          />
        </div>
      )}
    </div>
  );
}

function FragmentRow({
  item,
  open,
  tasksQuery,
  onToggle,
}: {
  item: ApprovalRequestItem;
  open: boolean;
  tasksQuery: ReturnType<typeof useApprovalTasks>;
  onToggle: () => void;
}) {
  return (
    <>
      <TableRow>
        <TableCell>
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={open}
            aria-label={open ? "收起详情" : "展开详情"}
            onClick={onToggle}
            className="size-7 p-0"
          >
            {open ? (
              <ChevronDown aria-hidden="true" />
            ) : (
              <ChevronRight aria-hidden="true" />
            )}
          </Button>
        </TableCell>
        <TableCell className="tabular-nums">
          {formatUnixSeconds(item.createdAt)}
        </TableCell>
        <TableCell>{item.requestedBy ?? "—"}</TableCell>
        <TableCell className="font-mono text-xs">
          {item.baseToken} / {item.tableId}
        </TableCell>
        <TableCell className="font-mono text-xs">
          {item.recordId ?? "—"}
        </TableCell>
        <TableCell>
          <StatusBadge tone={toneOf(item.outcome)}>
            {approvalOutcomeLabel(item.outcome)}
          </StatusBadge>
        </TableCell>
        <TableCell
          className="max-w-64 truncate text-xs text-muted-foreground"
          title={item.message}
        >
          {item.message}
        </TableCell>
      </TableRow>
      {open ? (
        <TableRow>
          <TableCell colSpan={7} className="bg-muted/30 py-2">
            <div className="px-4">
              <JsonBlock title="请求体" text={item.requestBody} />
              <JsonBlock title="返回体" text={item.responseBody} />
              {item.serialNumber ? (
                <p className="pb-2 text-xs">
                  审批单编号：
                  <span className="font-mono">{item.serialNumber}</span>
                </p>
              ) : null}
              {item.outcome === "accepted" && item.configId !== null ? (
                <div className="pb-2">
                  <p className="pb-1 text-xs font-medium text-muted-foreground">
                    任务（该配置）
                  </p>
                  {tasksQuery.isPending ? (
                    <p className="text-xs text-muted-foreground">加载中…</p>
                  ) : tasksQuery.isError ? (
                    <p className="text-xs text-destructive">
                      {tasksQuery.error instanceof Error
                        ? tasksQuery.error.message
                        : String(tasksQuery.error)}
                    </p>
                  ) : tasksQuery.data !== undefined &&
                    tasksQuery.data.items.length === 0 ? (
                    <p className="text-xs text-muted-foreground">
                      该配置下没有任务。
                    </p>
                  ) : tasksQuery.data !== undefined ? (
                    <TasksTable
                      tasks={tasksQuery.data.items}
                      total={tasksQuery.data.total}
                    />
                  ) : null}
                </div>
              ) : null}
            </div>
          </TableCell>
        </TableRow>
      ) : null}
    </>
  );
}

function JsonBlock({ title, text }: { title: string; text: string | null }) {
  return (
    <div className="pb-2">
      <p className="pb-1 text-xs font-medium text-muted-foreground">{title}</p>
      {text === null ? (
        <p className="text-xs text-muted-foreground">（无）</p>
      ) : (
        <pre className="max-h-48 overflow-auto rounded-md border border-border bg-background p-2 font-mono text-xs">
          {text}
        </pre>
      )}
    </div>
  );
}

function TasksTable({
  tasks,
  total,
}: {
  tasks: ApprovalTaskItem[];
  total: number | null;
}) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>record_id</TableHead>
          <TableHead>状态</TableHead>
          <TableHead>实例 Code</TableHead>
          <TableHead>单号</TableHead>
          <TableHead>最近错误</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {tasks.map((task) => (
          <TableRow key={task.id}>
            <TableCell className="font-mono text-xs">{task.recordId}</TableCell>
            <TableCell>{task.state}</TableCell>
            <TableCell className="font-mono text-xs">
              {task.instanceCode ?? "—"}
            </TableCell>
            <TableCell className="font-mono text-xs">
              {task.serialNumber ?? "—"}
            </TableCell>
            <TableCell
              className="max-w-48 truncate text-xs text-muted-foreground"
              title={task.lastError ?? undefined}
            >
              {task.lastError ?? "—"}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
      {(total ?? 0) > tasks.length ? (
        <TableFooter className="text-xs text-muted-foreground">
          共 {total} 条任务，仅显示最近 {tasks.length} 条（批量受理可产生大量
          任务，完整列表走配置页任务查询）。
        </TableFooter>
      ) : null}
    </Table>
  );
}
