import { useMemo, useState } from "react";
import { MoreHorizontal, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { buildTreeRows } from "@/contracts/table-data";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
} from "@/contracts/ui-catalog";
import {
  ActionDialog,
  ConfirmActionDialog,
} from "@/renderers/action/ActionDialog";
import { usePresentedActions } from "@/renderers/action/use-presented-actions";
import { DataGrid } from "./DataGrid";
import { TableQueryPanel } from "./TableQueryPanel";
import { hasActiveQuery } from "./table-query";
import {
  createTableFilters,
  flattenDisplayRows,
  type SourceRow,
} from "./table-view-model";
import { useRelationOptions } from "./use-relation-options";
import { useTableQuery } from "./use-table-query";

function actionLabel(presentation: ActionPresentationSchema) {
  return presentation.title || presentation.operation_id;
}

/**
 * 通用 TableView 解释器（对齐旧 TableView.vue 编排）：
 * 查询面板 + TanStack Table 数据网格 + 行/批量/工具栏 Action + Action 对话框。
 */
export function TableView({
  view,
  actions,
}: {
  view: TableViewSchema;
  actions: ActionDemoSchema[];
}) {
  const dataAction = actions.find(
    (action) => action.operation_id === view.data_action,
  );
  const query = useTableQuery(view, dataAction);
  const [dense, setDense] = useState(false);
  const [selectedRows, setSelectedRows] = useState<SourceRow[]>([]);

  // 树视图在无查询条件时把扁平行构造为树；构造失败安全降级为普通表格。
  const treeResult = useMemo(() => {
    if (!view.tree || hasActiveQuery(query.state)) {
      return { rows: query.rows, warning: "" };
    }
    try {
      return { rows: buildTreeRows(query.rows, view.tree), warning: "" };
    } catch (cause) {
      return {
        rows: query.rows,
        warning: `${cause instanceof Error ? cause.message : String(cause)}；已安全降级为普通表格`,
      };
    }
  }, [view.tree, query.state, query.rows]);
  const displayRows = useMemo(
    () => flattenDisplayRows(treeResult.rows),
    [treeResult.rows],
  );

  const relation = useRelationOptions(view, actions, query.rows);

  const presented = usePresentedActions({
    presentations: view.action_presentations,
    businessFields: view.form.fields,
    actions,
    selectedRows,
    reload: query.reload,
  });

  const sort = {
    field: query.state.orderBy[0]?.field ?? null,
    descending: query.state.orderBy[0]?.direction === "desc",
  };

  return (
    <section aria-label={view.title || view.table}>
      <header className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2 className="text-lg font-semibold">{view.title || view.table}</h2>
          <p className="text-sm text-muted-foreground">
            共 {query.total} 项 · 支持搜索、筛选、排序和批量处理
          </p>
        </div>
        <div className="flex items-center gap-2">
          {presented.directToolbarActions.map((presentation) => (
            <Button
              key={presentation.operation_id}
              variant={
                presentation === presented.toolbarActionGroups.primary
                  ? "default"
                  : "outline"
              }
              size="sm"
              disabled={presentation.availability?.state === "disabled"}
              title={presentation.availability?.reason}
              onClick={() => presented.openAction(presentation)}
            >
              {actionLabel(presentation)}
            </Button>
          ))}
          {presented.toolbarActionGroups.overflow.length > 0 && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="sm" aria-label="更多工具操作">
                  <MoreHorizontal className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {presented.toolbarActionGroups.overflow.map((presentation) => (
                  <DropdownMenuItem
                    key={presentation.operation_id}
                    disabled={presentation.availability?.state === "disabled"}
                    onClick={() => presented.openAction(presentation)}
                  >
                    {actionLabel(presentation)}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          )}
          <Button
            variant="ghost"
            size="icon"
            aria-label="刷新数据"
            onClick={() => void query.reload()}
          >
            <RefreshCw className="size-4" />
          </Button>
        </div>
      </header>

      <TableQueryPanel
        view={view}
        state={query.state}
        dense={dense}
        onDenseChange={setDense}
        onStateChange={(patch) => {
          if (patch.filters) query.setFilters(patch.filters);
          if (patch.search !== undefined) query.setSearch(patch.search);
        }}
        onApply={() => {
          query.applyQuery();
          void query.reload();
        }}
        onReset={() => {
          query.setFilters(
            createTableFilters(
              view.columns.filter((column) =>
                view.query.filter_fields.includes(column.field),
              ),
            ),
          );
          query.setSearch("");
          query.applyQuery();
          void query.reload();
        }}
      />

      {treeResult.warning && (
        <p className="mb-3 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
          {treeResult.warning}
        </p>
      )}
      {query.error && (
        <p
          role="alert"
          className="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {query.error}
        </p>
      )}

      {presented.bulkActions.length > 0 && (
        <div className="mb-2 flex items-center gap-2 text-sm">
          <span className="text-muted-foreground">
            已选 {selectedRows.length} 项
          </span>
          {presented.bulkActions.map((presentation) => (
            <Button
              key={presentation.operation_id}
              variant="outline"
              size="sm"
              disabled={
                selectedRows.length === 0 ||
                presentation.availability?.state === "disabled"
              }
              title={presentation.availability?.reason}
              onClick={() => presented.openAction(presentation)}
            >
              {actionLabel(presentation)}
            </Button>
          ))}
        </div>
      )}

      <DataGrid
        view={view}
        rows={displayRows}
        loading={query.loading}
        dense={dense}
        sort={sort}
        onSortChange={query.changeSort}
        labelFor={relation.labelFor}
        selectionEnabled={presented.bulkActions.length > 0}
        onSelectionChange={setSelectedRows}
        rowActionGroups={presented.rowActionGroups}
        onOpenAction={presented.openAction}
      />

      <footer className="mt-3 flex flex-wrap items-center justify-end gap-2 text-sm">
        <span className="text-muted-foreground">
          第 {query.state.page} / {query.pageCount} 页 · 共 {query.total} 项
        </span>
        <select
          className="h-8 rounded-md border border-input bg-transparent px-2 text-sm"
          aria-label="每页条数"
          value={query.state.pageSize}
          onChange={(event) => query.changePageSize(Number(event.target.value))}
        >
          {query.pageSizeOptions.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        <Button
          variant="outline"
          size="sm"
          disabled={query.state.page <= 1}
          onClick={() => query.changePage(query.state.page - 1)}
        >
          上一页
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={query.state.page >= query.pageCount}
          onClick={() => query.changePage(query.state.page + 1)}
        >
          下一页
        </Button>
      </footer>

      {presented.notice && (
        <div
          role="status"
          className={
            presented.notice.type === "negative"
              ? "fixed right-4 bottom-4 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive shadow-lg"
              : "fixed right-4 bottom-4 rounded-md border border-border bg-background px-4 py-2 text-sm shadow-lg"
          }
        >
          {presented.notice.message}
          <button
            type="button"
            className="ml-3 text-muted-foreground hover:text-foreground"
            aria-label="关闭通知"
            onClick={presented.dismissNotice}
          >
            ✕
          </button>
        </div>
      )}

      <ActionDialog
        state={presented.dialog}
        businessFields={view.form.fields}
        actions={actions}
        submitting={presented.submitting}
        onClose={presented.closeDialog}
        onSubmit={presented.submitDialog}
      />
      <ConfirmActionDialog
        presentation={presented.confirmation}
        onSettle={presented.settleConfirmation}
      />
    </section>
  );
}
