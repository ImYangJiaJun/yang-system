import { useEffect, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronUp,
  GripVertical,
  Plus,
  Trash2,
} from "lucide-react";

import { useToast } from "@/shared/lib/toast";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";

import {
  BATCH_COMPUTED_AGGREGATE_LABELS,
  BATCH_COMPUTED_OPERATOR_LABELS,
  type BatchComputedAggregate,
  type BatchComputedOperand,
  type BatchComputedOperator,
  type BatchFieldCatalogEntry,
  type BatchViewComputedRule,
  type BatchViewConfig,
  fetchBatchFieldCatalog,
  fetchBatchViewConfig,
  resetBatchViewConfig,
  saveBatchViewConfig,
} from "../batch-approval-api";

/// 明细列在视图配置里的 key：`detail:<明细控件 ID>:<子控件 ID>`。
const DETAIL_COLUMN_PREFIX = "detail:";

function detailColumnKey(detailId: string, columnId: string): string {
  return `${DETAIL_COLUMN_PREFIX}${detailId}:${columnId}`;
}

/// 勾选、标题、当前节点、实例状态固定展示；这里只列出可配置的基础列。
const BASE_COLUMNS: Array<{ key: string; label: string }> = [
  { key: "base:definition", label: "审批定义" },
  { key: "base:instanceCode", label: "实例编号" },
  { key: "base:initiator", label: "发起人" },
  { key: "base:summary", label: "摘要" },
  { key: "base:createdAt", label: "提交时间" },
  { key: "base:apiOperate", label: "可批量处理" },
];

/// 技术定位字段默认不展示，需要时勾选即可。
const DEFAULT_HIDDEN_COLUMNS = new Set(["base:instanceCode"]);

const COLUMN_KIND_META = {
  instance: {
    label: "实例",
    className: "border-slate-300 bg-slate-50 text-slate-600",
  },
  form: {
    label: "表单",
    className: "border-sky-300 bg-sky-50 text-sky-700",
  },
  computed: {
    label: "计算",
    className: "border-violet-300 bg-violet-50 text-violet-700",
  },
  detail: {
    label: "明细",
    className: "border-amber-300 bg-amber-50 text-amber-700",
  },
} as const;

function columnKind(key: string): keyof typeof COLUMN_KIND_META {
  if (key.startsWith("computed:")) return "computed";
  if (key.startsWith(DETAIL_COLUMN_PREFIX)) return "detail";
  if (key.startsWith("form:")) return "form";
  return "instance";
}

const DESCRIPTION_TYPES = new Set([
  "text",
  "description",
  "explanation",
  "hint",
]);

type DraftColumn = {
  key: string;
  label: string;
  visible: boolean;
  /// 明细列的分组标题（父明细控件名）；普通列为空。
  group?: string;
};
type FieldOption = { id: string; name: string };
type DetailOption = { id: string; name: string; columns: FieldOption[] };
type CatalogOptions = {
  fields: FieldOption[];
  details: DetailOption[];
  /// 控件 ID → 名称，用于补全操作数显示名。
  names: Map<string, string>;
};

function newRuleId(): string {
  return `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

function catalogOptions(
  catalog: BatchFieldCatalogEntry[],
  fallback: Array<{ id: string | null; name: string }>,
): CatalogOptions {
  const byId = new Map(catalog.map((entry) => [entry.id, entry]));
  const names = new Map<string, string>();
  const fields: FieldOption[] = [];
  const details: DetailOption[] = [];
  for (const widget of catalog) {
    names.set(widget.id, widget.name);
    if (widget.parentId) continue;
    if (DESCRIPTION_TYPES.has((widget.type ?? "").toLowerCase())) continue;
    if (widget.children.length > 0) {
      const columns: FieldOption[] = [];
      for (const childId of widget.children) {
        const child = byId.get(childId);
        if (!child) continue;
        names.set(child.id, child.name);
        columns.push({ id: child.id, name: child.name });
      }
      details.push({ id: widget.id, name: widget.name, columns });
      continue;
    }
    fields.push({ id: widget.id, name: widget.name });
  }
  const known = new Set(catalog.map((entry) => entry.id));
  for (const field of fallback) {
    if (!field.id || known.has(field.id)) continue;
    fields.push({ id: field.id, name: field.name });
    names.set(field.id, field.name);
  }
  return { fields, details, names };
}

function operandLabel(operand: BatchComputedOperand): string {
  if (operand.kind === "field") {
    return operand.fieldName ?? operand.fieldId;
  }
  const detail = operand.detailFieldName ?? operand.detailFieldId;
  const column = operand.columnName ?? operand.columnId;
  return `${detail}.${column} ${BATCH_COMPUTED_AGGREGATE_LABELS[operand.aggregate]}`;
}

function defaultFieldOperand(options: CatalogOptions): BatchComputedOperand {
  const first = options.fields[0];
  return {
    kind: "field",
    fieldId: first?.id ?? "",
    fieldName: first?.name ?? null,
  };
}

function operandValid(operand: BatchComputedOperand | null): boolean {
  if (!operand) return false;
  if (operand.kind === "field") return operand.fieldId !== "";
  return (
    operand.detailFieldId !== "" &&
    operand.columnId !== "" &&
    operand.aggregate in BATCH_COMPUTED_AGGREGATE_LABELS
  );
}

/// 按保存的配置排序列；未配置的列（新控件/新规则）追加在末尾并保持可见。
function draftColumns(
  base: Array<{ key: string; label: string; group?: string }>,
  view: BatchViewConfig,
): DraftColumn[] {
  const visible = new Map(
    view.columns.map((column) => [column.key, column.visible]),
  );
  const byKey = new Map(base.map((entry) => [entry.key, entry]));
  const ordered: DraftColumn[] = [];
  const used = new Set<string>();
  for (const column of view.columns) {
    const entry = byKey.get(column.key);
    if (!entry || used.has(column.key)) continue;
    used.add(column.key);
    ordered.push({ ...entry, visible: column.visible });
  }
  for (const entry of base) {
    if (used.has(entry.key)) continue;
    used.add(entry.key);
    ordered.push({
      ...entry,
      visible: visible.get(entry.key) ?? !DEFAULT_HIDDEN_COLUMNS.has(entry.key),
    });
  }
  return ordered;
}

/// 计算字段变化时同步列：停用/删除的规则移除列，新增启用的规则追加列。
function syncComputedColumns(
  columns: DraftColumn[],
  rules: BatchViewComputedRule[],
): DraftColumn[] {
  const enabled = new Map(
    rules
      .filter((rule) => rule.enabled)
      .map((rule) => [`computed:${rule.id}`, rule.name]),
  );
  const kept = columns
    .filter(
      (column) =>
        !column.key.startsWith("computed:") || enabled.has(column.key),
    )
    .map((column) => {
      const name = enabled.get(column.key);
      return name === undefined ? column : { ...column, label: name };
    });
  const existing = new Set(kept.map((column) => column.key));
  for (const [key, name] of enabled) {
    if (!existing.has(key)) kept.push({ key, label: name, visible: true });
  }
  return kept;
}

/// 操作数选择器：顶层表单字段，或明细控件里某一列的行级聚合。
function OperandEditor({
  label,
  operand,
  options,
  onChange,
}: {
  label: string;
  operand: BatchComputedOperand;
  options: CatalogOptions;
  onChange: (operand: BatchComputedOperand) => void;
}) {
  const detail =
    (operand.kind === "detail"
      ? options.details.find((entry) => entry.id === operand.detailFieldId)
      : undefined) ?? options.details[0];
  const columns = detail?.columns ?? [];
  return (
    <div className="space-y-1">
      <Label>{label}</Label>
      <div className="grid gap-2 sm:grid-cols-4">
        <Select
          value={operand.kind}
          onValueChange={(value) => {
            if (value === "detail") {
              const first = options.details[0];
              onChange({
                kind: "detail",
                detailFieldId: first?.id ?? "",
                detailFieldName: first?.name ?? null,
                columnId: first?.columns[0]?.id ?? "",
                columnName: first?.columns[0]?.name ?? null,
                aggregate: "sum",
              });
              return;
            }
            onChange({
              kind: "field",
              fieldId: options.fields[0]?.id ?? "",
              fieldName: options.fields[0]?.name ?? null,
            });
          }}
        >
          <SelectTrigger aria-label={`${label}类型`}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="field">表单字段</SelectItem>
            <SelectItem value="detail">明细聚合</SelectItem>
          </SelectContent>
        </Select>
        {operand.kind === "field" ? (
          <div className="sm:col-span-3">
            <Select
              value={operand.fieldId}
              onValueChange={(value) =>
                onChange({
                  kind: "field",
                  fieldId: value,
                  fieldName: options.names.get(value) ?? null,
                })
              }
            >
              <SelectTrigger aria-label={`${label}字段`}>
                <SelectValue placeholder="选择字段" />
              </SelectTrigger>
              <SelectContent>
                {options.fields.map((field) => (
                  <SelectItem key={field.id} value={field.id}>
                    {field.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        ) : (
          <>
            <Select
              value={operand.detailFieldId}
              onValueChange={(value) => {
                const next = options.details.find(
                  (entry) => entry.id === value,
                );
                onChange({
                  ...operand,
                  detailFieldId: value,
                  detailFieldName: next?.name ?? null,
                  columnId: next?.columns[0]?.id ?? "",
                  columnName: next?.columns[0]?.name ?? null,
                });
              }}
            >
              <SelectTrigger aria-label={`${label}明细`}>
                <SelectValue placeholder="选择明细" />
              </SelectTrigger>
              <SelectContent>
                {options.details.map((entry) => (
                  <SelectItem key={entry.id} value={entry.id}>
                    {entry.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={operand.columnId}
              onValueChange={(value) =>
                onChange({
                  ...operand,
                  columnId: value,
                  columnName:
                    columns.find((column) => column.id === value)?.name ?? null,
                })
              }
            >
              <SelectTrigger aria-label={`${label}明细列`}>
                <SelectValue placeholder="选择列" />
              </SelectTrigger>
              <SelectContent>
                {columns.map((column) => (
                  <SelectItem key={column.id} value={column.id}>
                    {column.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={operand.aggregate}
              onValueChange={(value) =>
                onChange({
                  ...operand,
                  aggregate: value as BatchComputedAggregate,
                })
              }
            >
              <SelectTrigger aria-label={`${label}聚合方式`}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(
                  Object.keys(
                    BATCH_COMPUTED_AGGREGATE_LABELS,
                  ) as BatchComputedAggregate[]
                ).map((aggregate) => (
                  <SelectItem key={aggregate} value={aggregate}>
                    {BATCH_COMPUTED_AGGREGATE_LABELS[aggregate]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </>
        )}
      </div>
    </div>
  );
}

export function BatchViewSettingsDrawer({
  open,
  approvalCode,
  accessToken,
  fields,
  onClose,
  onSaved,
}: {
  open: boolean;
  approvalCode: string;
  accessToken: string | undefined;
  /// 当前定义下任务里实际出现的控件，作为目录拉取失败时的兜底。
  fields: Array<{ id: string | null; name: string }>;
  onClose: () => void;
  onSaved: () => void;
}) {
  const toast = useToast();
  const fieldsRef = useRef(fields);
  fieldsRef.current = fields;

  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [saving, setSaving] = useState(false);
  const [tab, setTab] = useState<"columns" | "computed">("columns");
  const [options, setOptions] = useState<CatalogOptions>({
    fields: [],
    details: [],
    names: new Map(),
  });
  const [columns, setColumns] = useState<DraftColumn[]>([]);
  const [rules, setRules] = useState<BatchViewComputedRule[]>([]);
  const [editing, setEditing] = useState<BatchViewComputedRule | null>(null);
  const [formError, setFormError] = useState("");

  useEffect(() => {
    if (!open || !approvalCode) return;
    let cancelled = false;
    setLoading(true);
    setLoadError("");
    setFormError("");
    setEditing(null);
    setTab("columns");
    void Promise.all([
      fetchBatchViewConfig(approvalCode, accessToken),
      fetchBatchFieldCatalog(approvalCode, accessToken).catch(() => []),
    ])
      .then(([view, catalog]) => {
        if (cancelled) return;
        const form = catalogOptions(catalog, fieldsRef.current);
        setOptions(form);
        setRules(view.computedFields);
        const base = [
          ...BASE_COLUMNS,
          ...form.fields.map((field) => ({
            key: `form:${field.id}`,
            label: field.name,
          })),
          ...form.details.map((detail) => ({
            key: `form:${detail.id}`,
            label: detail.name,
          })),
          ...form.details.flatMap((detail) =>
            detail.columns.map((column) => ({
              key: detailColumnKey(detail.id, column.id),
              label: `${detail.name} / ${column.name}`,
              group: detail.name,
            })),
          ),
          ...view.computedFields
            .filter((rule) => rule.enabled)
            .map((rule) => ({
              key: `computed:${rule.id}`,
              label: rule.name,
            })),
        ];
        setColumns(
          syncComputedColumns(draftColumns(base, view), view.computedFields),
        );
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setLoadError(
          cause instanceof Error ? cause.message : "读取字段配置失败",
        );
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, approvalCode, accessToken]);

  const updateRules = (next: BatchViewComputedRule[]) => {
    setRules(next);
    setColumns((current) => syncComputedColumns(current, next));
  };

  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const pointerYRef = useRef(0);
  const scrollFrameRef = useRef<number | null>(null);
  const stopAutoScroll = () => {
    if (scrollFrameRef.current !== null) {
      cancelAnimationFrame(scrollFrameRef.current);
      scrollFrameRef.current = null;
    }
  };
  // 拖动到列表上下边缘时持续滚动，否则长列表里无法把列拖到视口外。
  const startAutoScroll = () => {
    stopAutoScroll();
    if (typeof requestAnimationFrame !== "function") return;
    const step = () => {
      const list = listRef.current;
      if (list) {
        const rect = list.getBoundingClientRect();
        if (pointerYRef.current < rect.top + 48) list.scrollTop -= 14;
        else if (pointerYRef.current > rect.bottom - 48) list.scrollTop += 14;
      }
      scrollFrameRef.current = requestAnimationFrame(step);
    };
    scrollFrameRef.current = requestAnimationFrame(step);
  };
  useEffect(() => stopAutoScroll, []);
  const finishDrag = () => {
    stopAutoScroll();
    setDragIndex(null);
    setDropIndex(null);
  };
  const moveColumnTo = (from: number, to: number) => {
    setColumns((current) => {
      if (
        from === to ||
        from < 0 ||
        to < 0 ||
        from >= current.length ||
        to >= current.length
      ) {
        return current;
      }
      const next = [...current];
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  };
  const commitDrop = () => {
    if (dragIndex !== null && dropIndex !== null) {
      moveColumnTo(
        dragIndex,
        dropIndex > dragIndex ? dropIndex - 1 : dropIndex,
      );
    }
    finishDrag();
  };

  const startNewRule = () => {
    setFormError("");
    const left = defaultFieldOperand(options);
    const second = options.fields[1];
    setEditing({
      id: newRuleId(),
      name: "",
      left,
      operator: second ? "multiply" : "none",
      right: second
        ? { kind: "field", fieldId: second.id, fieldName: second.name }
        : null,
      precision: 2,
      enabled: true,
    });
  };

  const saveRule = () => {
    if (!editing) return;
    const name = editing.name.trim();
    if (!name) {
      setFormError("字段名不能为空");
      return;
    }
    if (!operandValid(editing.left)) {
      setFormError("请选择左侧的字段或明细列");
      return;
    }
    if (editing.operator !== "none" && !operandValid(editing.right)) {
      setFormError("请选择右侧的字段或明细列，或把运算改为「单值」");
      return;
    }
    if (rules.some((rule) => rule.id !== editing.id && rule.name === name)) {
      setFormError("字段名不能重复");
      return;
    }
    const next = {
      ...editing,
      name,
      right: editing.operator === "none" ? null : editing.right,
    };
    updateRules(
      rules.some((rule) => rule.id === next.id)
        ? rules.map((rule) => (rule.id === next.id ? next : rule))
        : [...rules, next],
    );
    setEditing(null);
    setFormError("");
  };

  const removeRule = (id: string) => {
    updateRules(rules.filter((rule) => rule.id !== id));
    if (editing?.id === id) setEditing(null);
  };

  const save = async () => {
    if (saving) return;
    setSaving(true);
    try {
      await saveBatchViewConfig(
        approvalCode,
        {
          computedFields: rules,
          columns: columns.map((column) => ({
            key: column.key,
            visible: column.visible,
          })),
        },
        accessToken,
      );
      toast.success("字段设置已保存");
      onSaved();
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : "保存字段设置失败");
    } finally {
      setSaving(false);
    }
  };

  const reset = async () => {
    if (saving) return;
    if (!window.confirm("重置为默认视图？计算字段规则也会一并清除。")) return;
    setSaving(true);
    try {
      await resetBatchViewConfig(approvalCode, accessToken);
      toast.success("已重置为默认视图");
      onSaved();
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : "重置失败");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="flex max-h-[85vh] flex-col sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>字段设置</DialogTitle>
          <DialogDescription>
            只影响你自己的批量审批视图（{approvalCode || "当前定义"}
            ），不会改变其他人的配置或审批定义本身。
          </DialogDescription>
        </DialogHeader>

        <div className="flex gap-2">
          <Button
            type="button"
            size="sm"
            variant={tab === "columns" ? "default" : "outline"}
            onClick={() => setTab("columns")}
          >
            显示字段
          </Button>
          <Button
            type="button"
            size="sm"
            variant={tab === "computed" ? "default" : "outline"}
            onClick={() => setTab("computed")}
          >
            计算字段
          </Button>
        </div>

        {loading ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            加载中…
          </p>
        ) : loadError ? (
          <p className="py-8 text-center text-sm text-destructive">
            {loadError}
          </p>
        ) : tab === "columns" ? (
          <div
            ref={listRef}
            className="min-h-0 flex-1 space-y-1 overflow-y-auto pr-1"
          >
            {columns.map((column, index) => {
              const previous = index > 0 ? columns[index - 1] : undefined;
              const groupStart =
                Boolean(column.group) && column.group !== previous?.group;
              return (
                <div key={column.key}>
                  {groupStart && (
                    <div className="pb-1 pt-2 text-xs font-medium text-muted-foreground">
                      明细内的列 · {column.group}
                    </div>
                  )}
                  {dragIndex !== null && dropIndex === index && (
                    <div
                      className="mb-1 h-0.5 rounded-full bg-primary"
                      data-slot="drop-indicator"
                    />
                  )}
                  <div
                    draggable
                    onDragStart={(event) => {
                      setDragIndex(index);
                      setDropIndex(index);
                      pointerYRef.current = event.clientY;
                      startAutoScroll();
                    }}
                    onDragOver={(event) => {
                      event.preventDefault();
                      pointerYRef.current = event.clientY;
                      const rect = event.currentTarget.getBoundingClientRect();
                      setDropIndex(
                        event.clientY > rect.top + rect.height / 2
                          ? index + 1
                          : index,
                      );
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      commitDrop();
                    }}
                    onDragEnd={finishDrag}
                    className={`flex cursor-grab items-center gap-2 rounded border px-2 py-1.5 ${
                      dragIndex === index ? "opacity-50" : ""
                    }`}
                  >
                    <GripVertical
                      className="size-4 shrink-0 text-muted-foreground"
                      aria-hidden="true"
                    />
                    <Checkbox
                      checked={column.visible}
                      aria-label={`显示 ${column.label}`}
                      onCheckedChange={(checked) =>
                        setColumns((current) =>
                          current.map((entry) =>
                            entry.key === column.key
                              ? { ...entry, visible: checked === true }
                              : entry,
                          ),
                        )
                      }
                    />
                    <span className="min-w-0 flex-1 truncate text-sm">
                      {column.label}
                    </span>
                    <span className="flex shrink-0 items-center">
                      <button
                        type="button"
                        aria-label={`上移 ${column.label}`}
                        title="上移"
                        disabled={index === 0}
                        onClick={() => moveColumnTo(index, index - 1)}
                        className="flex size-6 items-center justify-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
                      >
                        <ChevronUp className="size-3.5" />
                      </button>
                      <button
                        type="button"
                        aria-label={`下移 ${column.label}`}
                        title="下移"
                        disabled={index === columns.length - 1}
                        onClick={() => moveColumnTo(index, index + 1)}
                        className="flex size-6 items-center justify-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
                      >
                        <ChevronDown className="size-3.5" />
                      </button>
                    </span>
                    <span
                      className={`shrink-0 rounded border px-1 text-[10px] leading-4 ${
                        COLUMN_KIND_META[columnKind(column.key)].className
                      }`}
                    >
                      {COLUMN_KIND_META[columnKind(column.key)].label}
                    </span>
                  </div>
                </div>
              );
            })}
            {dragIndex !== null && dropIndex === columns.length && (
              <div
                className="h-0.5 rounded-full bg-primary"
                data-slot="drop-indicator"
              />
            )}
          </div>
        ) : (
          <div className="min-h-0 flex-1 space-y-2 overflow-y-auto pr-1">
            {rules.length === 0 && !editing && (
              <p className="py-4 text-center text-sm text-muted-foreground">
                还没有计算字段。计算字段把两个控件做四则运算，结果只读展示在表格里。
              </p>
            )}
            {rules.map((rule) => (
              <div
                key={rule.id}
                className="flex items-center gap-2 rounded border px-2 py-1.5"
              >
                <Checkbox
                  checked={rule.enabled}
                  aria-label={`启用 ${rule.name}`}
                  onCheckedChange={(checked) =>
                    updateRules(
                      rules.map((entry) =>
                        entry.id === rule.id
                          ? { ...entry, enabled: checked === true }
                          : entry,
                      ),
                    )
                  }
                />
                <span className="min-w-0 flex-1 truncate text-sm">
                  {rule.name}
                  <span className="ml-2 text-xs text-muted-foreground">
                    {operandLabel(rule.left)}{" "}
                    {rule.operator === "none"
                      ? ""
                      : BATCH_COMPUTED_OPERATOR_LABELS[rule.operator]}{" "}
                    {rule.right ? operandLabel(rule.right) : ""} ·{" "}
                    {rule.precision} 位小数
                  </span>
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    setFormError("");
                    setEditing(rule);
                  }}
                >
                  编辑
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  aria-label={`删除 ${rule.name}`}
                  onClick={() => removeRule(rule.id)}
                >
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
            ))}

            {editing ? (
              <div className="space-y-2 rounded border bg-muted/30 p-3">
                <div className="grid gap-2 sm:grid-cols-2">
                  <div className="space-y-1">
                    <Label htmlFor="view-rule-name">字段名</Label>
                    <Input
                      id="view-rule-name"
                      value={editing.name}
                      placeholder="例如：参考人民币金额"
                      onChange={(event) =>
                        setEditing({ ...editing, name: event.target.value })
                      }
                    />
                  </div>
                  <div className="space-y-1">
                    <Label>小数位</Label>
                    <Input
                      type="number"
                      min={0}
                      max={6}
                      value={editing.precision}
                      onChange={(event) => {
                        const precision = Number(event.target.value);
                        setEditing({
                          ...editing,
                          precision: Number.isFinite(precision)
                            ? Math.min(6, Math.max(0, Math.round(precision)))
                            : 0,
                        });
                      }}
                    />
                  </div>
                </div>
                <div className="space-y-1">
                  <Label>运算</Label>
                  <Select
                    value={editing.operator}
                    onValueChange={(value) => {
                      const operator = value as BatchComputedOperator;
                      setEditing({
                        ...editing,
                        operator,
                        right:
                          operator === "none"
                            ? null
                            : (editing.right ?? defaultFieldOperand(options)),
                      });
                    }}
                  >
                    <SelectTrigger aria-label="运算">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {(
                        Object.keys(
                          BATCH_COMPUTED_OPERATOR_LABELS,
                        ) as BatchComputedOperator[]
                      ).map((operator) => (
                        <SelectItem key={operator} value={operator}>
                          {BATCH_COMPUTED_OPERATOR_LABELS[operator]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <OperandEditor
                  label="左操作数"
                  operand={editing.left}
                  options={options}
                  onChange={(left) => setEditing({ ...editing, left })}
                />
                {editing.operator !== "none" && (
                  <OperandEditor
                    label="右操作数"
                    operand={editing.right ?? defaultFieldOperand(options)}
                    options={options}
                    onChange={(right) => setEditing({ ...editing, right })}
                  />
                )}
                <p className="text-xs text-muted-foreground">
                  明细聚合会先对明细里该列的每一行取值，再按所选方式汇总（空行跳过）。
                  汇率即使来自外部数据源、控件类型不是数字也可以选；计算时按数值解析，
                  解析不了的行显示为空。
                </p>
                {formError && (
                  <p className="text-xs text-destructive">{formError}</p>
                )}
                <div className="flex justify-end gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setEditing(null);
                      setFormError("");
                    }}
                  >
                    取消
                  </Button>
                  <Button type="button" size="sm" onClick={saveRule}>
                    确定
                  </Button>
                </div>
              </div>
            ) : (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={startNewRule}
              >
                <Plus className="size-3.5" />
                新增计算字段
              </Button>
            )}
          </div>
        )}

        <DialogFooter className="gap-2 sm:justify-between">
          <Button
            type="button"
            variant="ghost"
            onClick={() => void reset()}
            disabled={saving || loading}
          >
            重置默认
          </Button>
          <div className="flex gap-2">
            <Button type="button" variant="outline" onClick={onClose}>
              取消
            </Button>
            <Button
              type="button"
              onClick={() => void save()}
              disabled={
                saving || loading || Boolean(loadError) || editing !== null
              }
            >
              {saving ? "保存中…" : "保存"}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
