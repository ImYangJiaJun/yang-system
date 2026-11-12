import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Plus, Trash2 } from "lucide-react";

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
  BATCH_COMPUTED_OPERATOR_LABELS,
  type BatchComputedOperator,
  type BatchFieldCatalogEntry,
  type BatchViewComputedRule,
  type BatchViewConfig,
  fetchBatchFieldCatalog,
  fetchBatchViewConfig,
  resetBatchViewConfig,
  saveBatchViewConfig,
} from "../batch-approval-api";

/// 勾选、标题、当前节点、实例状态固定展示；这里只列出可配置的基础列。
const BASE_COLUMNS: Array<{ key: string; label: string }> = [
  { key: "base:definition", label: "审批定义" },
  { key: "base:instanceCode", label: "实例编号" },
  { key: "base:initiator", label: "发起人" },
  { key: "base:summary", label: "摘要" },
  { key: "base:createdAt", label: "提交时间" },
  { key: "base:apiOperate", label: "API 操作" },
];

const DESCRIPTION_TYPES = new Set([
  "text",
  "description",
  "explanation",
  "hint",
]);

type DraftColumn = { key: string; label: string; visible: boolean };
type FieldOption = { id: string; name: string };

function newRuleId(): string {
  return `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

function formOptions(
  catalog: BatchFieldCatalogEntry[],
  fallback: Array<{ id: string | null; name: string }>,
): FieldOption[] {
  const seen = new Set<string>();
  const options: FieldOption[] = [];
  for (const widget of catalog) {
    if (DESCRIPTION_TYPES.has((widget.type ?? "").toLowerCase())) continue;
    if (seen.has(widget.id)) continue;
    seen.add(widget.id);
    options.push({ id: widget.id, name: widget.name });
  }
  for (const field of fallback) {
    if (!field.id || seen.has(field.id)) continue;
    seen.add(field.id);
    options.push({ id: field.id, name: field.name });
  }
  return options;
}

/// 按保存的配置排序列；未配置的列（新控件/新规则）追加在末尾并保持可见。
function draftColumns(
  base: Array<{ key: string; label: string }>,
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
    ordered.push({ ...entry, visible: visible.get(entry.key) !== false });
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
      return name === undefined
        ? column
        : { ...column, label: name, visible: true };
    });
  const existing = new Set(kept.map((column) => column.key));
  for (const [key, name] of enabled) {
    if (!existing.has(key)) kept.push({ key, label: name, visible: true });
  }
  return kept;
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
  const [options, setOptions] = useState<FieldOption[]>([]);
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
        const form = formOptions(catalog, fieldsRef.current);
        setOptions(form);
        setRules(view.computedFields);
        const base = [
          ...BASE_COLUMNS,
          ...form.map((field) => ({
            key: `form:${field.id}`,
            label: field.name,
          })),
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

  const moveColumn = (index: number, delta: number) => {
    setColumns((current) => {
      const target = index + delta;
      if (target < 0 || target >= current.length) return current;
      const next = [...current];
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  };

  const startNewRule = () => {
    setFormError("");
    setEditing({
      id: newRuleId(),
      name: "",
      leftFieldId: options[0]?.id ?? "",
      leftFieldName: options[0]?.name ?? null,
      operator: "multiply",
      rightFieldId: options[1]?.id ?? options[0]?.id ?? "",
      rightFieldName: options[1]?.name ?? options[0]?.name ?? null,
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
    if (!editing.leftFieldId || !editing.rightFieldId) {
      setFormError("请选择左右两侧的控件");
      return;
    }
    if (rules.some((rule) => rule.id !== editing.id && rule.name === name)) {
      setFormError("字段名不能重复");
      return;
    }
    const next = {
      ...editing,
      name,
      leftFieldName:
        options.find((option) => option.id === editing.leftFieldId)?.name ??
        null,
      rightFieldName:
        options.find((option) => option.id === editing.rightFieldId)?.name ??
        null,
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

  const selectOptions = useMemo(
    () => options.map((option) => ({ value: option.id, label: option.name })),
    [options],
  );

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
          <div className="min-h-0 flex-1 space-y-1 overflow-y-auto pr-1">
            {columns.map((column, index) => (
              <div
                key={column.key}
                className="flex items-center gap-2 rounded border px-2 py-1.5"
              >
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
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  aria-label={`上移 ${column.label}`}
                  disabled={index === 0}
                  onClick={() => moveColumn(index, -1)}
                >
                  <ArrowUp className="size-3.5" />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  aria-label={`下移 ${column.label}`}
                  disabled={index === columns.length - 1}
                  onClick={() => moveColumn(index, 1)}
                >
                  <ArrowDown className="size-3.5" />
                </Button>
              </div>
            ))}
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
                    {rule.leftFieldName ?? rule.leftFieldId}{" "}
                    {BATCH_COMPUTED_OPERATOR_LABELS[rule.operator]}{" "}
                    {rule.rightFieldName ?? rule.rightFieldId} ·{" "}
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
                <div className="grid gap-2 sm:grid-cols-3">
                  <div className="space-y-1">
                    <Label>左控件</Label>
                    <Select
                      value={editing.leftFieldId}
                      onValueChange={(value) =>
                        setEditing({ ...editing, leftFieldId: value })
                      }
                    >
                      <SelectTrigger aria-label="左控件">
                        <SelectValue placeholder="选择控件" />
                      </SelectTrigger>
                      <SelectContent>
                        {selectOptions.map((option) => (
                          <SelectItem key={option.value} value={option.value}>
                            {option.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1">
                    <Label>运算</Label>
                    <Select
                      value={editing.operator}
                      onValueChange={(value) =>
                        setEditing({
                          ...editing,
                          operator: value as BatchComputedOperator,
                        })
                      }
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
                  <div className="space-y-1">
                    <Label>右控件</Label>
                    <Select
                      value={editing.rightFieldId}
                      onValueChange={(value) =>
                        setEditing({ ...editing, rightFieldId: value })
                      }
                    >
                      <SelectTrigger aria-label="右控件">
                        <SelectValue placeholder="选择控件" />
                      </SelectTrigger>
                      <SelectContent>
                        {selectOptions.map((option) => (
                          <SelectItem key={option.value} value={option.value}>
                            {option.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </div>
                <p className="text-xs text-muted-foreground">
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
