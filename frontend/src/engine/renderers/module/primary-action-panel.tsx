import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { RefreshCw, Search } from "lucide-react";

import { invokeAction } from "@/engine/http/client";
import { asJsonSchema, effectiveSchema } from "@/engine/contracts/json-schema";
import { useSessionCredentials } from "@/engine/session/use-session";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import { Skeleton } from "@/shared/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";
import type { ModulePageDefinition } from "@/engine/catalog/module-pages";
import type { ActionDemoSchema } from "@/engine/contracts/ui-catalog";
import {
  ActionDialog,
  ConfirmActionDialog,
} from "@/engine/renderers/action/ActionDialog";
import { usePresentedActions } from "@/engine/renderers/action/use-presented-actions";
import { formatCell } from "@/engine/renderers/table/table-view-model";
import { BusinessCell } from "@/engine/renderers/table/BusinessCell";
import {
  buildPrimaryActionValues,
  inputFields,
  isRecord,
  manualPrimaryParams,
  outputProperties,
  schemaColumn,
} from "./primary-model";
import { useToast } from "@/shared/lib/toast";

const PAGE_SIZE = 20;

/**
 * 无视图模块的 primaryAction 数据卡片（旧 ModulePage.vue 回退分支的 React 版）：
 * 列表（items 数组）→ 通用表格；单记录 → 字段明细；否则空态。
 */
export function PrimaryActionPanel({
  page,
  actions,
}: {
  page: ModulePageDefinition;
  actions: ActionDemoSchema[];
}) {
  const session = useSessionCredentials();
  const action = page.primaryAction;
  const toast = useToast();
  const [data, setData] = useState<unknown>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [pageNumber, setPageNumber] = useState(1);
  const [refreshTick, setRefreshTick] = useState(0);
  /// 主 Action 需要人工填写的必填参数（如直授查询的 user_id）：面板补不齐
  /// （不是 page/limit/search、也没有默认值），这类 Action 不能自动加载。
  const manualParams = useMemo(() => manualPrimaryParams(action), [action]);
  const [manualValues, setManualValues] = useState<Record<string, unknown>>({});
  const abortRef = useRef<AbortController | undefined>(undefined);
  const manualComplete = manualParams.every((name) => {
    const value = manualValues[name];
    return value !== undefined && value !== null && value !== "";
  });

  /// 主数据 Action 的一次调用：自动加载（防抖后）与人工查询（点「查询」）共用。
  const invokePrimary = useCallback(
    (values: Record<string, unknown>) => {
      if (!action) return;
      abortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;
      setLoading(true);
      setError("");
      invokeAction(action, values, session, controller.signal)
        .then((result) => {
          if (result.kind !== "json") {
            throw new Error("模块主数据 Action 必须返回 JSON");
          }
          setData(result.data);
        })
        .catch((cause: unknown) => {
          if (cause instanceof Error && cause.name === "AbortError") return;
          setData(undefined);
          setError(cause instanceof Error ? cause.message : String(cause));
        })
        .finally(() => setLoading(false));
    },
    [action, session],
  );

  useEffect(() => {
    // 有人工必填参数时**不自动调用**：自动发出的请求注定缺少参数、每次进页都红框
    // （2026-10-05 回归：access.grants 模块的 list_user_grants）。改由查询条件条触发。
    if (!action || manualParams.length > 0) return;
    const timer = window.setTimeout(
      () =>
        invokePrimary(
          buildPrimaryActionValues(action, {
            page: pageNumber,
            pageSize: PAGE_SIZE,
            search,
          }),
        ),
      // 搜索输入防抖（旧实现 debounce=250ms）；翻页立即生效由 pageNumber 触发。
      250,
    );
    return () => {
      window.clearTimeout(timer);
      abortRef.current?.abort();
    };
  }, [action, invokePrimary, manualParams, pageNumber, search, refreshTick]);

  const record = isRecord(data) ? data : undefined;
  const rows = useMemo(() => {
    const items = record?.items;
    return Array.isArray(items) ? items.filter(isRecord) : [];
  }, [record]);
  const detail = record && !Array.isArray(record.items) ? record : undefined;
  const total =
    typeof record?.total === "number" && Number.isFinite(record.total)
      ? record.total
      : 0;
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  const rowSchemaProperties = outputProperties(action, true);
  const detailSchemaProperties = outputProperties(action, false);
  const columnFields = [...new Set(rows.flatMap((row) => Object.keys(row)))];
  const supportsSearch = action
    ? inputFields(action).includes("search")
    : false;

  // 无视图模块的模块级 Action（工具栏），与旧 ModulePage.vue 的 usePresentedActions 一致。
  const presented = usePresentedActions({
    presentations: page.actionPresentations,
    businessFields: [],
    actions,
    selectedRows: [],
    reload: () => setRefreshTick((prev) => prev + 1),
    onNotice: (notice) => {
      if (notice.type === "positive") {
        toast.success(notice.message);
      } else if (notice.type === "negative") {
        toast.error(notice.message);
      } else {
        toast.warning(notice.message);
      }
    },
  });

  if (!action && presented.directToolbarActions.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        该模块未声明主数据 Action，通用模块页无法渲染。
      </p>
    );
  }

  return (
    <div className="rounded-lg border border-border">
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <p className="text-sm font-medium">{action?.title ?? page.title}</p>
          {action?.description && (
            <p className="text-xs text-muted-foreground">
              {action.description}
            </p>
          )}
        </div>
        <div className="ml-auto flex items-center gap-2">
          {presented.directToolbarActions.map((presentation) => (
            <Button
              key={presentation.operation_id}
              variant="outline"
              size="sm"
              disabled={presentation.availability?.state === "disabled"}
              title={presentation.availability?.reason}
              onClick={() => presented.openAction(presentation)}
            >
              {presentation.title || presentation.operation_id}
            </Button>
          ))}
          {supportsSearch && (
            <div className="relative">
              <Search className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
              <Input
                className="w-56 pl-9"
                placeholder="搜索"
                aria-label="搜索"
                value={search}
                onChange={(event) => {
                  setSearch(event.target.value);
                  setPageNumber(1);
                }}
              />
            </div>
          )}
          <Button
            variant="ghost"
            size="icon"
            aria-label="刷新页面"
            onClick={() => setRefreshTick((prev) => prev + 1)}
          >
            <RefreshCw className="size-4" />
          </Button>
        </div>
      </div>

      {action && manualParams.length > 0 && (
        <div className="flex flex-wrap items-end gap-2 border-b border-border px-4 py-3">
          {manualParams.map((name) => (
            <ManualParamInput
              key={name}
              action={action}
              name={name}
              value={manualValues[name]}
              onChange={(value) =>
                setManualValues((prev) => ({ ...prev, [name]: value }))
              }
            />
          ))}
          <Button
            onClick={() =>
              invokePrimary({
                ...manualValues,
                ...buildPrimaryActionValues(action, {
                  page: 1,
                  pageSize: PAGE_SIZE,
                  search,
                }),
              })
            }
            disabled={loading || !manualComplete}
          >
            查询
          </Button>
        </div>
      )}

      {error && (
        <p
          role="alert"
          className="border-b border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
        >
          {error}
        </p>
      )}

      {loading && rows.length === 0 ? (
        <div className="space-y-2 p-4" aria-label="数据加载中">
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </div>
      ) : rows.length > 0 ? (
        <Table>
          <TableHeader>
            <TableRow>
              {columnFields.map((field) => (
                <TableHead key={field}>
                  {schemaColumn(field, rowSchemaProperties[field]).title}
                </TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, index) => (
              <TableRow key={index}>
                {columnFields.map((field) => (
                  <TableCell key={field}>
                    <BusinessCell
                      column={schemaColumn(field, rowSchemaProperties[field])}
                      value={row[field]}
                    />
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      ) : detail ? (
        <dl className="divide-y divide-border">
          {Object.entries(detail).map(([field, value]) => (
            <div key={field} className="flex gap-4 px-4 py-2 text-sm">
              <dt className="w-40 shrink-0 text-muted-foreground">
                {schemaColumn(field, detailSchemaProperties[field]).title}
              </dt>
              <dd className="min-w-0 break-all">{formatCell(value)}</dd>
            </div>
          ))}
        </dl>
      ) : action && manualParams.length > 0 ? (
        <div className="flex flex-col items-center gap-2 py-12 text-sm text-muted-foreground">
          输入上方查询条件后点「查询」查看数据。
        </div>
      ) : (
        <div className="flex flex-col items-center gap-2 py-12 text-sm text-muted-foreground">
          当前模块暂无数据
        </div>
      )}

      {rows.length > 0 && totalPages > 1 && (
        <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-2 text-sm">
          <span className="text-muted-foreground">
            第 {pageNumber} / {totalPages} 页
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={pageNumber <= 1}
            onClick={() => setPageNumber((prev) => prev - 1)}
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={pageNumber >= totalPages}
            onClick={() => setPageNumber((prev) => prev + 1)}
          >
            下一页
          </Button>
        </div>
      )}

      <ActionDialog
        state={presented.dialog}
        businessFields={[]}
        actions={actions}
        submitting={presented.submitting}
        onClose={presented.closeDialog}
        onSubmit={presented.submitDialog}
      />
      <ConfirmActionDialog
        presentation={presented.confirmation}
        onSettle={presented.settleConfirmation}
      />
    </div>
  );
}

/// 主 Action 人工必填参数的单行输入：整数走数字输入（与 SchemaField 同一语义：
/// 只提交可解析的有限数值，空输入视为未填），其余按文本。
function ManualParamInput({
  action,
  name,
  value,
  onChange,
}: {
  action: ActionDemoSchema;
  name: string;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  const id = useId();
  const parameter = action.params?.find((candidate) => candidate.name === name);
  const root = asJsonSchema(action.input_schema);
  const property = effectiveSchema(root, root).properties?.[name];
  const type = Array.isArray(property?.type)
    ? property.type.find((candidate) => candidate !== "null")
    : property?.type;
  const isInteger = type === "integer" || type === "number";
  const title = parameter?.title || property?.title || name;
  return (
    <div className="space-y-1">
      <Label htmlFor={id}>{title}</Label>
      <Input
        id={id}
        inputMode={isInteger ? "numeric" : "text"}
        value={
          isInteger
            ? typeof value === "number"
              ? value
              : ""
            : typeof value === "string"
              ? value
              : ""
        }
        onChange={(event) => {
          const raw = event.target.value;
          if (isInteger) {
            if (!raw) {
              onChange(undefined);
              return;
            }
            const parsed = Number(raw);
            if (Number.isFinite(parsed)) onChange(parsed);
          } else {
            onChange(raw);
          }
        }}
      />
    </div>
  );
}
