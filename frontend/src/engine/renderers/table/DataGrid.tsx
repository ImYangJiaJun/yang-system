import { useMemo, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  useReactTable,
  type ColumnDef,
} from "@tanstack/react-table";
import { ArrowDown, ArrowUp, ArrowUpDown, MoreHorizontal } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type {
  ActionPresentationSchema,
  TableViewSchema,
} from "@/engine/contracts/ui-catalog";
import { cn } from "@/lib/utils";
import { BusinessCell } from "./BusinessCell";
import type {
  DisplayRow,
  PresentedActionGroups,
  SourceRow,
} from "./table-view-model";

interface DataGridProps {
  view: TableViewSchema;
  rows: DisplayRow[];
  loading: boolean;
  sort: { field: string | null; descending: boolean };
  onSortChange: (field: string | null, descending: boolean) => void;
  labelFor: (operationId: string, value: unknown) => string | undefined;
  selectionEnabled: boolean;
  onSelectionChange: (rows: SourceRow[]) => void;
  rowActionGroups: PresentedActionGroups;
  onOpenAction: (
    presentation: ActionPresentationSchema,
    row?: SourceRow,
  ) => void;
}

function actionLabel(presentation: ActionPresentationSchema) {
  return presentation.title || presentation.operation_id;
}

export function DataGrid({
  view,
  rows,
  loading,
  sort,
  onSortChange,
  labelFor,
  selectionEnabled,
  onSelectionChange,
  rowActionGroups,
  onOpenAction,
}: DataGridProps) {
  const [rowSelection, setRowSelection] = useState<Record<string, boolean>>({});
  const hasRowActions =
    Boolean(rowActionGroups.primary) || rowActionGroups.overflow.length > 0;

  const columns = useMemo<ColumnDef<DisplayRow>[]>(() => {
    const defs: ColumnDef<DisplayRow>[] = [];
    if (selectionEnabled) {
      defs.push({
        id: "__selection",
        enableSorting: false,
        header: ({ table }) => (
          <Checkbox
            aria-label="全选"
            checked={table.getIsAllRowsSelected()}
            onCheckedChange={(checked) =>
              table.toggleAllRowsSelected(checked === true)
            }
          />
        ),
        cell: ({ row }) => (
          <Checkbox
            aria-label={`选择第 ${row.index + 1} 行`}
            checked={row.getIsSelected()}
            onCheckedChange={(checked) => row.toggleSelected(checked === true)}
          />
        ),
      });
    }
    for (const column of view.columns) {
      defs.push({
        id: column.field,
        accessorFn: (row) => row.data[column.field],
        enableSorting: column.sortable,
        header: column.title || column.field,
        cell: (info) => (
          <BusinessCell
            column={column}
            value={info.getValue()}
            relationLabel={
              column.relation
                ? labelFor(column.relation.operation_id, info.getValue())
                : undefined
            }
          />
        ),
      });
    }
    if (hasRowActions) {
      defs.push({
        id: "__actions",
        enableSorting: false,
        header: "操作",
        cell: ({ row }) => (
          <div className="flex items-center justify-end gap-1">
            {rowActionGroups.primary && (
              <Button
                variant="ghost"
                size="sm"
                disabled={
                  rowActionGroups.primary.availability?.state === "disabled"
                }
                title={rowActionGroups.primary.availability?.reason}
                onClick={() =>
                  onOpenAction(rowActionGroups.primary!, row.original.data)
                }
              >
                {actionLabel(rowActionGroups.primary)}
              </Button>
            )}
            {rowActionGroups.overflow.length > 0 && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="ghost" size="sm">
                    <MoreHorizontal className="size-4" />
                    更多操作
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {rowActionGroups.overflow.map((presentation) => (
                    <DropdownMenuItem
                      key={presentation.operation_id}
                      disabled={presentation.availability?.state === "disabled"}
                      onClick={() =>
                        onOpenAction(presentation, row.original.data)
                      }
                    >
                      {actionLabel(presentation)}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            )}
          </div>
        ),
      });
    }
    return defs;
  }, [
    view.columns,
    selectionEnabled,
    hasRowActions,
    rowActionGroups,
    onOpenAction,
    labelFor,
  ]);

  const table = useReactTable({
    data: rows,
    columns,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.key,
    manualPagination: true,
    manualSorting: true,
    state: { rowSelection },
    enableRowSelection: selectionEnabled,
    onRowSelectionChange: (updater) => {
      setRowSelection((prev) => {
        const next = typeof updater === "function" ? updater(prev) : updater;
        onSelectionChange(
          rows.filter((row) => next[row.key]).map((row) => row.data),
        );
        return next;
      });
    },
  });

  const cycleSort = (field: string) => {
    if (sort.field !== field) return onSortChange(field, false);
    if (!sort.descending) return onSortChange(field, true);
    return onSortChange(null, false);
  };

  return (
    <div className="rounded-md border border-border">
      <Table>
        <TableHeader>
          {table.getHeaderGroups().map((headerGroup) => (
            <TableRow key={headerGroup.id}>
              {headerGroup.headers.map((header) => {
                const column =
                  header.column.id !== "__selection" &&
                  header.column.id !== "__actions"
                    ? view.columns.find((c) => c.field === header.column.id)
                    : undefined;
                const sortable = column?.sortable;
                return (
                  <TableHead
                    key={header.id}
                    style={{
                      width: column?.display?.width
                        ? `${column.display.width}px`
                        : undefined,
                      minWidth: column?.display?.min_width
                        ? `${column.display.min_width}px`
                        : undefined,
                    }}
                    className={cn(
                      column?.display?.align === "right" && "text-right",
                      column?.display?.align === "center" && "text-center",
                    )}
                  >
                    {sortable ? (
                      <button
                        type="button"
                        className="inline-flex items-center gap-1 hover:text-foreground"
                        onClick={() => cycleSort(header.column.id)}
                        aria-label={`按 ${column.title || column.field} 排序`}
                      >
                        {flexRender(
                          header.column.columnDef.header,
                          header.getContext(),
                        )}
                        {sort.field === header.column.id ? (
                          sort.descending ? (
                            <ArrowDown className="size-3.5" />
                          ) : (
                            <ArrowUp className="size-3.5" />
                          )
                        ) : (
                          <ArrowUpDown className="size-3.5 opacity-40" />
                        )}
                      </button>
                    ) : (
                      flexRender(
                        header.column.columnDef.header,
                        header.getContext(),
                      )
                    )}
                  </TableHead>
                );
              })}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.length === 0 ? (
            <TableRow>
              <TableCell
                colSpan={columns.length}
                className="h-24 text-center text-muted-foreground"
              >
                {loading ? "加载中…" : "暂无数据"}
              </TableCell>
            </TableRow>
          ) : (
            table.getRowModel().rows.map((row) => (
              <TableRow
                key={row.id}
                data-state={row.getIsSelected() ? "selected" : undefined}
              >
                {row.getVisibleCells().map((cell, cellIndex) => (
                  <TableCell
                    key={cell.id}
                    style={
                      cellIndex === (selectionEnabled ? 1 : 0) &&
                      row.original.depth > 0
                        ? {
                            paddingLeft: `${row.original.depth * 1.25 + 0.5}rem`,
                          }
                        : undefined
                    }
                  >
                    {flexRender(cell.column.columnDef.cell, cell.getContext())}
                  </TableCell>
                ))}
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>
    </div>
  );
}
