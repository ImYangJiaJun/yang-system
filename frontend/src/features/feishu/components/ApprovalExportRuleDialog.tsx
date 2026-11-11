/**
 * 审批回写规则的六步向导。
 *
 * ① 来源审批：规则名 + 审批定义 Code（可粘贴后台地址）→ 拉取控件与节点，
 *    选定付款明细控件（`fieldList`）。
 * ② 触发节点：指定节点通过 / 最终通过；节点复选框来自定义 `node_list`，
 *    并允许手填补充（真机样本里 `node_list` 不含自动通过节点）。
 * ③ 条件：实例级 / 明细行级条件构建器（AND/OR）。
 * ④ 目标表 / 状态列：Bitable 坐标 + 状态标记列。
 * ⑤ 字段映射：明细 / 顶层 / 系统 / 常量 → 目标列 + 转换器。
 * ⑥ 保存并试算：**先保存再试算**——试算端点只接受已保存的规则 id。
 *
 * 数据访问一律走注入的 `client`（真实实现 `api.ts::useApprovalExportWizardClient`）。
 */

import { useMemo, useRef, useState } from "react";

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
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import type {
  ApprovalDefinitionPreview,
  ApprovalExportRuleInput,
  ApprovalExportWizardClient,
} from "../api";
import { serializeExportCondition } from "../api";
import {
  EXPORT_CONVERTER_OPTIONS,
  EXPORT_SYSTEM_KEY_OPTIONS,
  EXPORT_WRITABLE_FIELD_TYPES,
  defaultApprovalCommentConfig,
  parseApprovalDefinitionInput,
  parseBitableLink,
  type ApprovalCommentConfig,
  type ApprovalExportCondition,
  type ApprovalExportConditionItem,
  type ApprovalExportConditionOp,
  type ApprovalExportPreview,
  type ApprovalExportRule,
  type ApprovalWidget,
  type BitableField,
  type BitableTable,
} from "../types";

type Step = 1 | 2 | 3 | 4 | 5 | 6;

/// Select 不接受空串作为 item value，未选择态给哨兵值。
const NONE = "__none__";

const CONDITION_OPS: ReadonlyArray<{
  value: ApprovalExportConditionOp;
  label: string;
}> = [
  { value: "eq", label: "等于" },
  { value: "ne", label: "不等于" },
  { value: "in", label: "属于（逗号分隔）" },
  { value: "gt", label: "大于" },
  { value: "gte", label: "大于等于" },
  { value: "lt", label: "小于" },
  { value: "lte", label: "小于等于" },
  { value: "contains", label: "包含" },
  { value: "between", label: "介于（两个值）" },
  { value: "is_empty", label: "为空" },
];

const TRIGGER_STATE_LABELS: Record<string, string> = {
  passed: "已通过",
  waiting_node: "等待触发节点",
  waiting_final: "等待最终通过",
  not_passed: "未通过",
  rolled_back: "已退回",
};

/// 一条映射的编辑态。`optionMapText` 是「审批文案=目标选项」逐行文本。
type MapDraft = {
  sourceKind: "detail" | "top" | "system" | "constant";
  sourceWidgetId: string;
  sourceSystemKey: string;
  constantValue: string;
  targetFieldId: string;
  converter: string;
  optionMapText: string;
  required: boolean;
};

type LeafDraft = {
  widgetId: string;
  op: ApprovalExportConditionOp;
  valueText: string;
};

type ConditionItemDraft =
  | {
      scope: "instance";
      widgetId: string;
      op: ApprovalExportConditionOp;
      valueText: string;
    }
  | {
      scope: "detail";
      match: "all" | "any" | "none";
      leaves: LeafDraft[];
    };

type ConditionDraft = {
  match: "all" | "any";
  items: ConditionItemDraft[];
};

function emptyMapDraft(): MapDraft {
  return {
    sourceKind: "detail",
    sourceWidgetId: "",
    sourceSystemKey: "instance_code",
    constantValue: "",
    targetFieldId: "",
    converter: "text",
    optionMapText: "",
    required: false,
  };
}

function emptyConditionDraft(): ConditionDraft {
  return { match: "all", items: [] };
}

/// 线上值 → 编辑框文本。数组（`in` / `between`）按逗号拼回去。
function valueToText(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (Array.isArray(value)) return value.map(String).join("，");
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  return JSON.stringify(value);
}

/// 编辑框文本 → 线上值。数字保持字符串（后端用定点整数解析，不经 f64）。
function textToValue(op: ApprovalExportConditionOp, text: string): unknown {
  const trimmed = text.trim();
  if (op === "is_empty") return undefined;
  if (op === "in" || op === "between") {
    return trimmed
      .split(/[,，]/)
      .map((part) => part.trim())
      .filter((part) => part !== "");
  }
  return trimmed;
}

function conditionToDraft(
  condition: ApprovalExportCondition | null,
): ConditionDraft {
  if (!condition) return emptyConditionDraft();
  return {
    match: condition.match,
    items: condition.items.map((item): ConditionItemDraft => {
      if (item.scope === "instance") {
        return {
          scope: "instance",
          widgetId: item.widgetId,
          op: item.op,
          valueText: valueToText(item.value),
        };
      }
      return {
        scope: "detail",
        match: item.match,
        leaves: item.items.map((leaf) => ({
          widgetId: leaf.widgetId,
          op: leaf.op,
          valueText: valueToText(leaf.value),
        })),
      };
    }),
  };
}

function draftToCondition(
  draft: ConditionDraft,
): ApprovalExportCondition | null {
  const items: ApprovalExportConditionItem[] = [];
  for (const item of draft.items) {
    if (item.scope === "instance") {
      if (item.widgetId === "") continue;
      items.push({
        scope: "instance",
        widgetId: item.widgetId,
        op: item.op,
        value: textToValue(item.op, item.valueText),
      });
      continue;
    }
    const leaves = item.leaves
      .filter((leaf) => leaf.widgetId !== "")
      .map((leaf) => ({
        widgetId: leaf.widgetId,
        op: leaf.op,
        value: textToValue(leaf.op, leaf.valueText),
      }));
    if (leaves.length === 0) continue;
    items.push({
      scope: "detail",
      detailWidgetId: "",
      match: item.match,
      items: leaves,
    });
  }
  if (items.length === 0) return null;
  return { match: draft.match, items };
}

/// 把草稿里的明细条件补上规则选定的明细控件 id（草稿本身不持有它，
/// 这样切换明细控件时不会留下两份互相矛盾的 id）。
function conditionWithDetail(
  condition: ApprovalExportCondition | null,
  detailWidgetId: string,
): ApprovalExportCondition | null {
  if (!condition) return null;
  return {
    ...condition,
    items: condition.items.map((item) =>
      item.scope === "detail" ? { ...item, detailWidgetId } : item,
    ),
  };
}

function parseOptionMapText(text: string): Record<string, string> {
  const map: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const [rawKey, ...rest] = line.split("=");
    const key = rawKey?.trim() ?? "";
    const value = rest.join("=").trim();
    if (key !== "" && value !== "") map[key] = value;
  }
  return map;
}

function optionMapToText(map: Record<string, string>): string {
  return Object.entries(map)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

function mapDraftFromRule(map: ApprovalExportRule["maps"][number]): MapDraft {
  return {
    sourceKind: map.sourceKind,
    sourceWidgetId: map.sourceWidgetId,
    sourceSystemKey: map.sourceSystemKey || "instance_code",
    constantValue: map.constantValue ?? "",
    targetFieldId: map.targetFieldId,
    converter: map.converter || "text",
    optionMapText: optionMapToText(map.optionMap),
    required: map.required,
  };
}

function mapDraftReady(map: MapDraft): boolean {
  if (map.targetFieldId.trim() === "") return false;
  if (map.sourceKind === "detail" || map.sourceKind === "top") {
    return map.sourceWidgetId !== "";
  }
  if (map.sourceKind === "system") return map.sourceSystemKey !== "";
  return map.constantValue.trim() !== "";
}

function widgetLabel(widget: ApprovalWidget): string {
  return widget.qualifiedName?.trim() || widget.name?.trim() || widget.id;
}

function leafReady(leaf: LeafDraft): boolean {
  if (leaf.widgetId === "") return false;
  if (leaf.op === "is_empty") return true;
  if (leaf.op === "in" || leaf.op === "between") {
    const count = leaf.valueText
      .split(/[,，]/)
      .map((part) => part.trim())
      .filter((part) => part !== "").length;
    return leaf.op === "between" ? count === 2 : count > 0;
  }
  return leaf.valueText.trim() !== "";
}

function conditionReady(draft: ConditionDraft): boolean {
  return draft.items.every((item) =>
    item.scope === "instance"
      ? item.widgetId !== "" &&
        leafReady({
          widgetId: item.widgetId,
          op: item.op,
          valueText: item.valueText,
        })
      : item.leaves.length > 0 && item.leaves.every(leafReady),
  );
}

export type ApprovalExportRuleDialogProps = {
  client: ApprovalExportWizardClient;
  onCancel: () => void;
  onSubmitted?: () => void;
  open?: boolean;
  initialRule?: ApprovalExportRule;
};

export function ApprovalExportRuleDialog({
  client,
  onCancel,
  onSubmitted,
  open = true,
  initialRule,
}: ApprovalExportRuleDialogProps) {
  const [step, setStep] = useState<Step>(1);

  // ① 来源审批
  const [title, setTitle] = useState(initialRule?.title ?? "");
  const [approvalInput, setApprovalInput] = useState(
    initialRule?.approvalCode ?? "",
  );
  const [definition, setDefinition] =
    useState<ApprovalDefinitionPreview | null>(null);
  const [definitionPending, setDefinitionPending] = useState(false);
  const [definitionError, setDefinitionError] = useState<string | null>(null);
  const [detailWidgetId, setDetailWidgetId] = useState(
    initialRule?.detailWidgetId ?? "",
  );

  // ② 触发节点
  const [triggerMode, setTriggerMode] = useState(
    initialRule?.triggerMode === "final_approved"
      ? "final_approved"
      : "node_passed",
  );
  const [requireFinalApproval, setRequireFinalApproval] = useState(
    initialRule?.requireFinalApproval ?? true,
  );
  const [selectedNodeIds, setSelectedNodeIds] = useState<string[]>(
    initialRule?.triggerNodeIds ?? [],
  );
  const [extraNodeInput, setExtraNodeInput] = useState("");

  // ③ 条件
  const [rowFilter, setRowFilter] = useState(
    initialRule?.rowFilter === "all_rows" ? "all_rows" : "matched_rows",
  );
  const [condition, setCondition] = useState<ConditionDraft>(() =>
    conditionToDraft(initialRule?.condition ?? null),
  );

  // ④ 目标表 / 状态列
  // 只剩**一个** token 状态：手填与「拉取数据表」解析后的值共用它，否则没有
  // `feishu.datasource.write` 的身份手填完也无法通过第 4 步。
  const [baseToken, setBaseToken] = useState(
    initialRule?.targetBaseToken ?? "",
  );
  const [tableId, setTableId] = useState(initialRule?.targetTableId ?? "");
  const [tables, setTables] = useState<BitableTable[]>([]);
  const [tablesPending, setTablesPending] = useState(false);
  const [tablesMessage, setTablesMessage] = useState<string | null>(null);
  const [tableManual, setTableManual] = useState(!client.canPickBitable);
  const [coordinateError, setCoordinateError] = useState<string | null>(null);
  const [fields, setFields] = useState<BitableField[]>([]);
  const [fieldsPending, setFieldsPending] = useState(false);
  const [fieldsError, setFieldsError] = useState<string | null>(null);
  const [markEnabled, setMarkEnabled] = useState(
    initialRule?.markEnabled ?? true,
  );
  const [statusField, setStatusField] = useState(
    initialRule?.statusField ?? "",
  );
  // 支付评论配置。解析失败时后端给 null 但保留原串：界面从默认值起步并提示覆盖。
  const [commentConfig, setCommentConfig] = useState<ApprovalCommentConfig>(
    () => initialRule?.commentConfig ?? defaultApprovalCommentConfig(),
  );

  // ⑤ 字段映射
  const [maps, setMaps] = useState<MapDraft[]>(() =>
    initialRule && initialRule.maps.length > 0
      ? initialRule.maps.map(mapDraftFromRule)
      : [emptyMapDraft()],
  );

  // ⑥ 保存与试算
  const [enabled, setEnabled] = useState(initialRule?.enabled ?? true);
  const [savedRuleId, setSavedRuleId] = useState<number | null>(
    initialRule?.id ?? null,
  );
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [previewInstance, setPreviewInstance] = useState("");
  const [previewPending, setPreviewPending] = useState(false);
  const [preview, setPreview] = useState<ApprovalExportPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const savedRef = useRef(false);

  const approvalCode = parseApprovalDefinitionInput(approvalInput).approvalCode;
  const widgets = definition?.widgets ?? [];
  const nodes = definition?.nodes ?? [];

  const detailWidget = widgets.find((widget) => widget.id === detailWidgetId);
  const detailPrefix = detailWidget
    ? `${detailWidget.qualifiedName?.trim() || detailWidget.name?.trim()}_`
    : "";
  // 明细子控件的限定名以「明细名_」开头（`preview_widgets` 的拼法）；控件组
  // 的子控件在实例表单里是顶层节点，所以顶层候选排除明细子树即可。
  const detailChildren = detailWidget
    ? widgets.filter(
        (widget) =>
          widget.id !== detailWidget.id &&
          (widget.qualifiedName ?? widget.name ?? "").startsWith(detailPrefix),
      )
    : [];
  // 其他明细控件的子树同样不是顶层控件（后端 `collect_top` 也跳过所有
  // fieldList 子树）；只按所选明细的前缀排除会漏掉它们。
  const otherDetailPrefixes = widgets
    .filter(
      (widget) => widget.type === "fieldList" && widget.id !== detailWidgetId,
    )
    .map((widget) => {
      const name = widget.qualifiedName?.trim() || widget.name?.trim();
      return name ? `${name}_` : "";
    })
    .filter((prefix) => prefix !== "");
  const topWidgets = widgets.filter((widget) => {
    if (widget.type === "fieldList") return false;
    const qualified = widget.qualifiedName ?? widget.name ?? "";
    if (detailPrefix !== "" && qualified.startsWith(detailPrefix)) return false;
    return !otherDetailPrefixes.some((prefix) => qualified.startsWith(prefix));
  });
  const detailWidgetOptions = widgets.filter(
    (widget) => widget.type === "fieldList",
  );

  // 手动补充的节点 id（不在定义 node_list 里，但仍被保存过 / 用户手填）。
  const knownNodeIds = new Set(
    nodes.map((node) => node.nodeId).filter((id) => id !== ""),
  );
  const manualNodeIds = selectedNodeIds.filter(
    (nodeId) => !knownNodeIds.has(nodeId),
  );

  const conditionJson = useMemo(() => {
    const built = conditionWithDetail(
      draftToCondition(condition),
      detailWidgetId,
    );
    return serializeExportCondition(built);
  }, [condition, detailWidgetId]);

  // 坐标可以是裸 token / table_id，也可以是一整条多维表格链接。手填路径没有
  // 「拉取数据表」按钮，所以保存时也要能解析链接，而不是把整条 URL 当 token 发出去。
  const parsedCoordinate = parseBitableLink(baseToken);
  const effectiveBaseToken = (parsedCoordinate?.baseToken ?? baseToken).trim();
  const effectiveTableId = (
    tableId.trim() !== "" ? tableId : (parsedCoordinate?.tableId ?? "")
  ).trim();

  function patchCommentConfig(patch: Partial<ApprovalCommentConfig>) {
    setCommentConfig((current) => ({ ...current, ...patch }));
  }

  function commentFieldLabel(fieldId: string): string {
    return (
      commentConfig.fieldNames[fieldId]?.trim() ||
      fields.find((field) => field.fieldId === fieldId)?.fieldName?.trim() ||
      fieldId
    );
  }

  /// 勾选评论展示字段：同步维护缓存列名，并把不属于集合的必填 / 触发引用剪掉。
  function toggleCommentField(fieldId: string, checked: boolean) {
    setCommentConfig((current) => {
      const fieldIds = checked
        ? Array.from(new Set([...current.fieldIds, fieldId]))
        : current.fieldIds.filter((id) => id !== fieldId);
      const field = fields.find((item) => item.fieldId === fieldId);
      const fieldNames = { ...current.fieldNames };
      if (checked && field) fieldNames[fieldId] = field.fieldName;
      if (!checked) delete fieldNames[fieldId];
      const triggerFieldId =
        current.triggerFieldId !== null &&
        fieldIds.includes(current.triggerFieldId)
          ? current.triggerFieldId
          : null;
      return {
        ...current,
        fieldIds,
        fieldNames,
        requiredFieldIds: current.requiredFieldIds.filter((id) =>
          fieldIds.includes(id),
        ),
        triggerFieldId,
        triggerValues: triggerFieldId === null ? [] : current.triggerValues,
      };
    });
  }

  function toggleRequiredField(fieldId: string, checked: boolean) {
    setCommentConfig((current) => ({
      ...current,
      requiredFieldIds: checked
        ? [...current.requiredFieldIds, fieldId]
        : current.requiredFieldIds.filter((id) => id !== fieldId),
    }));
  }

  /// 字段列表未拉取时的兜底：逗号 / 空格分隔的 field_id 手填。
  function setCommentFieldIdsFromText(text: string) {
    const fieldIds = Array.from(
      new Set(
        text
          .split(/[,，\s]+/)
          .map((part) => part.trim())
          .filter((part) => part !== ""),
      ),
    );
    setCommentConfig((current) => {
      const triggerFieldId =
        current.triggerFieldId !== null &&
        fieldIds.includes(current.triggerFieldId)
          ? current.triggerFieldId
          : null;
      return {
        ...current,
        fieldIds,
        fieldNames: Object.fromEntries(
          Object.entries(current.fieldNames).filter(([id]) =>
            fieldIds.includes(id),
          ),
        ),
        requiredFieldIds: current.requiredFieldIds.filter((id) =>
          fieldIds.includes(id),
        ),
        triggerFieldId,
        triggerValues: triggerFieldId === null ? [] : current.triggerValues,
      };
    });
  }

  function setTriggerValuesFromText(text: string) {
    patchCommentConfig({
      triggerValues: text
        .split(/\r?\n/)
        .map((part) => part.trim())
        .filter((part) => part !== ""),
    });
  }

  const commentFieldsMissing = commentConfig.fieldIds.filter(
    (fieldId) => !fields.some((field) => field.fieldId === fieldId),
  );
  const commentConfigReady =
    !commentConfig.enabled ||
    (commentConfig.fieldIds.length > 0 &&
      commentConfig.requiredFieldIds.every((id) =>
        commentConfig.fieldIds.includes(id),
      ) &&
      (commentConfig.triggerFieldId === null ||
        (commentConfig.fieldIds.includes(commentConfig.triggerFieldId) &&
          commentConfig.triggerValues.length > 0)) &&
      commentConfig.statusFieldId.trim() !== "" &&
      commentConfig.authorUserId.trim() !== "" &&
      commentConfig.statusLabels.commented.trim() !== "" &&
      commentConfig.statusLabels.failed.trim() !== "" &&
      commentConfig.prefix.trim() !== "" &&
      ["{base_token}", "{table_id}", "{record_id}"].every((placeholder) =>
        commentConfig.recordUrlTemplate.includes(placeholder),
      ));

  const stepReady: Record<Step, boolean> = {
    1:
      title.trim() !== "" &&
      approvalCode.trim() !== "" &&
      definition !== null &&
      detailWidgetId !== "",
    2: triggerMode === "final_approved" || selectedNodeIds.length > 0,
    3: conditionReady(condition),
    4:
      effectiveBaseToken !== "" &&
      effectiveTableId !== "" &&
      (!markEnabled || statusField.trim() !== "") &&
      commentConfigReady,
    5: maps.length > 0 && maps.every(mapDraftReady),
    6: true,
  };
  const allReady =
    stepReady[1] &&
    stepReady[2] &&
    stepReady[3] &&
    stepReady[4] &&
    stepReady[5];

  async function loadDefinition() {
    const code = approvalCode.trim();
    if (code === "") return;
    setDefinitionPending(true);
    setDefinitionError(null);
    try {
      const loaded = await client.listDefinition(code);
      if (loaded.widgets.length === 0)
        throw new Error("审批定义没有解析出控件，请确认定义 Code 与权限。");
      setDefinition(loaded);
      const detailIds = loaded.widgets
        .filter((widget) => widget.type === "fieldList")
        .map((widget) => widget.id);
      setDetailWidgetId((current) =>
        current !== "" && detailIds.includes(current)
          ? current
          : (detailIds[0] ?? ""),
      );
    } catch (cause) {
      setDefinition(null);
      setDefinitionError(
        cause instanceof Error ? cause.message : String(cause),
      );
    } finally {
      setDefinitionPending(false);
    }
  }

  async function loadTables() {
    setTablesPending(true);
    setTablesMessage(null);
    setCoordinateError(null);
    try {
      const raw = baseToken.trim();
      const parsed = parseBitableLink(raw);
      const token = parsed?.baseToken ?? raw;
      if (parsed && raw.includes("://")) {
        setBaseToken(parsed.baseToken);
        setTableId(parsed.tableId);
      }
      const loaded = await client.listTables(token);
      setTables(loaded);
      setTableManual(loaded.length === 0);
      setTablesMessage(
        loaded.length === 0
          ? "未查询到可用的数据表，可手填数据表 ID。"
          : `已拉取 ${loaded.length} 张数据表，请在下方选择。`,
      );
    } catch (cause) {
      setTableManual(true);
      setCoordinateError(
        cause instanceof Error ? cause.message : String(cause),
      );
    } finally {
      setTablesPending(false);
    }
  }

  async function loadFields() {
    setFieldsPending(true);
    setFieldsError(null);
    try {
      const loaded = await client.listFields(
        effectiveBaseToken,
        effectiveTableId,
      );
      if (loaded.length === 0)
        throw new Error("多维表格没有可用列，请确认表和读取权限后重试。");
      setFields(loaded);
    } catch (cause) {
      setFields([]);
      setFieldsError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setFieldsPending(false);
    }
  }

  function toggleNode(nodeId: string, checked: boolean) {
    setSelectedNodeIds((current) =>
      checked
        ? current.includes(nodeId)
          ? current
          : [...current, nodeId]
        : current.filter((id) => id !== nodeId),
    );
  }

  function addExtraNodes() {
    const extra = extraNodeInput
      .split(/[,，\s]+/)
      .map((part) => part.trim())
      .filter((part) => part !== "");
    if (extra.length === 0) return;
    setSelectedNodeIds((current) => [
      ...current,
      ...extra.filter((id) => !current.includes(id)),
    ]);
    setExtraNodeInput("");
  }

  function buildInput(): ApprovalExportRuleInput {
    return {
      title,
      approvalCode,
      triggerMode,
      triggerNodeIds: selectedNodeIds,
      requireFinalApproval,
      detailWidgetId,
      rowFilter,
      condition: conditionWithDetail(
        draftToCondition(condition),
        detailWidgetId,
      ),
      targetBaseToken: effectiveBaseToken,
      targetTableId: effectiveTableId,
      statusField: markEnabled ? statusField : null,
      markEnabled,
      enabled,
      // 新规则未启用评论时不写该列；已有配置（含停用 / 解析失败）始终回写，
      // 避免一次无关编辑把原配置悄悄清掉。
      commentConfig:
        commentConfig.enabled ||
        (initialRule?.commentConfigJson ?? null) !== null
          ? {
              ...commentConfig,
              triggerFieldId: commentConfig.triggerFieldId?.trim() || null,
            }
          : null,
      maps: maps.map((map) => ({
        sourceKind: map.sourceKind,
        sourceWidgetId:
          map.sourceKind === "detail" || map.sourceKind === "top"
            ? map.sourceWidgetId
            : "",
        sourceSystemKey: map.sourceKind === "system" ? map.sourceSystemKey : "",
        constantValue: map.sourceKind === "constant" ? map.constantValue : null,
        targetFieldId: map.targetFieldId,
        converter: map.converter,
        optionMap: parseOptionMapText(map.optionMapText),
        required: map.required,
      })),
    };
  }

  async function saveAndPreview() {
    if (!allReady) return;
    setSubmitting(true);
    setSubmitError(null);
    setPreviewError(null);
    try {
      const input = buildInput();
      let ruleId = savedRuleId;
      if (ruleId === null) {
        ruleId = await client.createRule(input);
        if (ruleId <= 0) throw new Error("服务端未返回新规则主键");
        setSavedRuleId(ruleId);
      } else {
        await client.updateRule(ruleId, input);
      }
      savedRef.current = true;
      onSubmitted?.();
      const instance = previewInstance.trim();
      if (instance !== "") {
        setPreviewPending(true);
        try {
          setPreview(await client.preview(ruleId, instance));
        } catch (cause) {
          setPreview(null);
          setPreviewError(
            cause instanceof Error ? cause.message : String(cause),
          );
        } finally {
          setPreviewPending(false);
        }
      }
    } catch (cause) {
      // 保存期的校验原因一次报全（定义控件 / 目标列 / 条件引用）。
      setSubmitError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSubmitting(false);
    }
  }

  function close() {
    if (submitting) return;
    if (savedRef.current) onSubmitted?.();
    onCancel();
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !submitting) close();
      }}
    >
      <DialogContent className="sm:max-w-4xl" showCloseButton={!submitting}>
        <DialogHeader>
          <DialogTitle>
            {initialRule ? "编辑" : "新建"}回写规则 · 第 {step} / 6 步
          </DialogTitle>
          <DialogDescription>
            {step === 1
              ? "选择来源审批与付款明细控件。"
              : step === 2
                ? "选择触发方式；指定节点通过时可多选，节点列表不含自动通过节点时可手填补充。"
                : step === 3
                  ? "条件为空表示恒真；行级条件按明细行逐行求值。"
                  : step === 4
                    ? "选择目标 Base / 数据表、状态标记列与支付评论配置。"
                    : step === 5
                      ? "每条映射把审批里的一个来源写到目标表的一列。"
                      : "先保存，再用一个真实实例 Code 试算。试算只读，可反复执行。"}
          </DialogDescription>
        </DialogHeader>

        {step === 1 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="export-title">规则名称</Label>
              <Input
                id="export-title"
                value={title}
                onChange={(event) => setTitle(event.target.value)}
                placeholder="例如：付款申请 → 付款明细台账"
                autoComplete="off"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="export-approval-code">审批定义 Code</Label>
              <Input
                id="export-approval-code"
                value={approvalInput}
                onChange={(event) => {
                  setApprovalInput(event.target.value);
                  setDefinition(null);
                }}
                placeholder="审批定义 Code，或粘贴审批后台地址"
                autoComplete="off"
              />
            </div>
            <Button
              variant="outline"
              disabled={approvalCode.trim() === "" || definitionPending}
              onClick={() => void loadDefinition()}
            >
              {definitionPending ? "拉取中…" : "拉取控件与节点"}
            </Button>
            {definitionError !== null ? (
              <p role="alert" className="text-sm text-destructive">
                {definitionError}
              </p>
            ) : null}
            {definition ? (
              <>
                <div className="space-y-1.5">
                  <Label>付款明细控件（fieldList）</Label>
                  {detailWidgetOptions.length === 0 ? (
                    <p role="alert" className="text-sm text-destructive">
                      该审批定义没有 fieldList 明细控件，无法按明细行回写。
                    </p>
                  ) : (
                    <Select
                      value={detailWidgetId}
                      onValueChange={setDetailWidgetId}
                    >
                      <SelectTrigger
                        aria-label="付款明细控件"
                        className="w-full"
                      >
                        <SelectValue placeholder="选择明细控件" />
                      </SelectTrigger>
                      <SelectContent>
                        {detailWidgetOptions.map((widget) => (
                          <SelectItem key={widget.id} value={widget.id}>
                            {widgetLabel(widget)}（{widget.id}）
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                </div>
                <p role="status" className="text-xs text-muted-foreground">
                  已解析 {widgets.length} 个控件、{nodes.length} 个节点。
                </p>
              </>
            ) : null}
          </div>
        ) : null}

        {step === 2 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label>触发方式</Label>
              <Select
                value={triggerMode}
                onValueChange={(value) => setTriggerMode(value)}
              >
                <SelectTrigger aria-label="触发方式" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="node_passed">指定节点通过</SelectItem>
                  <SelectItem value="final_approved">最终通过</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <label className="flex items-center gap-2 text-sm">
              <Checkbox
                checked={requireFinalApproval}
                onCheckedChange={(checked) =>
                  setRequireFinalApproval(checked === true)
                }
              />
              节点通过后还要求实例最终通过
            </label>
            {triggerMode === "node_passed" ? (
              <div className="space-y-2">
                <Label>触发节点</Label>
                {nodes.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    审批定义没有返回节点列表，请手填节点 ID（以实例详情里的
                    task_list.node_id 为准）。
                  </p>
                ) : (
                  <div className="max-h-56 space-y-1 overflow-auto rounded-md border p-2">
                    {nodes.map((node, nodeIndex) => (
                      <label
                        key={`${node.nodeId}-${nodeIndex}`}
                        className="flex items-center gap-2 rounded px-1 py-1 text-sm hover:bg-muted/50"
                      >
                        <Checkbox
                          checked={selectedNodeIds.includes(node.nodeId)}
                          onCheckedChange={(checked) =>
                            toggleNode(node.nodeId, checked === true)
                          }
                        />
                        <span>{node.name?.trim() || node.nodeId}</span>
                        <span className="text-xs text-muted-foreground">
                          {node.nodeId}
                          {node.nodeType ? ` · ${node.nodeType}` : ""}
                        </span>
                      </label>
                    ))}
                  </div>
                )}
                {manualNodeIds.length > 0 ? (
                  <div className="flex flex-wrap gap-2">
                    {manualNodeIds.map((nodeId) => (
                      <span
                        key={nodeId}
                        className="inline-flex items-center gap-1 rounded-md border bg-muted/50 px-2 py-0.5 text-xs"
                      >
                        手填节点 {nodeId}
                        <button
                          type="button"
                          className="text-muted-foreground hover:text-destructive"
                          aria-label={`移除节点 ${nodeId}`}
                          onClick={() => toggleNode(nodeId, false)}
                        >
                          ×
                        </button>
                      </span>
                    ))}
                  </div>
                ) : null}
                <div className="flex gap-2">
                  <Input
                    aria-label="补充节点 ID"
                    value={extraNodeInput}
                    onChange={(event) => setExtraNodeInput(event.target.value)}
                    placeholder="补充节点 ID，逗号分隔"
                    autoComplete="off"
                  />
                  <Button variant="outline" onClick={addExtraNodes}>
                    添加
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">
                  触发节点以任务的 node_id 判定；定义 node_list
                  可能不含自动通过节点，可手填补充。
                </p>
              </div>
            ) : null}
          </div>
        ) : null}

        {step === 3 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label>行筛选</Label>
              <Select
                value={rowFilter}
                onValueChange={(value) => setRowFilter(value)}
              >
                <SelectTrigger aria-label="行筛选" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="matched_rows">
                    仅写入满足行条件的明细行
                  </SelectItem>
                  <SelectItem value="all_rows">
                    实例条件命中后写入全部明细行
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            {initialRule &&
            initialRule.conditionJson !== null &&
            initialRule.condition === null ? (
              <p role="alert" className="text-sm text-destructive">
                原条件 JSON 无法解析，当前按空条件展示；保存会覆盖它。
              </p>
            ) : null}
            <ConditionEditor
              draft={condition}
              topWidgets={topWidgets}
              detailChildren={detailChildren}
              onChange={setCondition}
            />
          </div>
        ) : null}

        {step === 4 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="export-base-token">
                目标多维表格链接 / Base Token
              </Label>
              <Input
                id="export-base-token"
                value={baseToken}
                disabled={tablesPending}
                onChange={(event) => {
                  const value = event.target.value;
                  setBaseToken(value);
                  // 完整表格链接自带 ?table=；直接带出数据表 ID，手填用户
                  // 不必再从链接里抄一遍。
                  const parsed = parseBitableLink(value);
                  if (parsed?.tableId) setTableId(parsed.tableId);
                }}
                placeholder="粘贴完整数据表链接，或 appbcbWCzen6…"
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                支持完整链接（自动解析 Base Token 与数据表 ID），也支持旧的手填
                Base Token。
              </p>
            </div>
            {client.canPickBitable ? (
              <Button
                variant="outline"
                disabled={baseToken.trim() === "" || tablesPending}
                onClick={() => void loadTables()}
              >
                {tablesPending ? "拉取中…" : "拉取数据表"}
              </Button>
            ) : (
              <p className="text-xs text-muted-foreground">
                当前身份没有 feishu.datasource.write，不能拉取表 / 列；请手填
                Base Token、数据表 ID 与字段 ID。
              </p>
            )}
            {tablesMessage !== null ? (
              <p role="status" className="text-xs text-muted-foreground">
                {tablesMessage}
              </p>
            ) : null}
            {coordinateError !== null ? (
              <p role="alert" className="text-xs text-destructive">
                数据表拉取失败，已切换为手填：{coordinateError}
              </p>
            ) : null}
            {tableManual || tables.length === 0 ? (
              <div className="space-y-1.5">
                <Label htmlFor="export-table-id">数据表 ID（手填）</Label>
                <Input
                  id="export-table-id"
                  value={tableId}
                  onChange={(event) => setTableId(event.target.value)}
                  placeholder="tbl…"
                  autoComplete="off"
                />
              </div>
            ) : (
              <div className="space-y-1.5">
                <Label>数据表</Label>
                <Select value={tableId} onValueChange={setTableId}>
                  <SelectTrigger aria-label="数据表" className="w-full">
                    <SelectValue placeholder="选择数据表" />
                  </SelectTrigger>
                  <SelectContent>
                    {tables.map((table) => (
                      <SelectItem key={table.tableId} value={table.tableId}>
                        {table.name}（{table.tableId}）
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
            {client.canPickBitable ? (
              <Button
                variant="outline"
                disabled={effectiveTableId === "" || fieldsPending}
                onClick={() => void loadFields()}
              >
                {fieldsPending ? "拉取中…" : "拉取字段"}
              </Button>
            ) : null}
            {fieldsError !== null ? (
              <p role="alert" className="text-xs text-destructive">
                字段拉取失败：{fieldsError}
              </p>
            ) : null}
            <label className="flex items-center gap-2 text-sm">
              <Checkbox
                checked={markEnabled}
                onCheckedChange={(checked) => setMarkEnabled(checked === true)}
              />
              维护目标状态列（实例拒绝 / 撤回 / 退回时标记）
            </label>
            {markEnabled ? (
              <div className="space-y-1.5">
                <Label>目标状态列（文本或单选）</Label>
                {fields.length > 0 ? (
                  <Select
                    value={statusField}
                    onValueChange={(value) =>
                      setStatusField(value === NONE ? "" : value)
                    }
                  >
                    <SelectTrigger aria-label="目标状态列" className="w-full">
                      <SelectValue placeholder="选择状态列" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={NONE}>未选择</SelectItem>
                      {statusField !== "" &&
                      !fields.some((field) => field.fieldId === statusField) ? (
                        <SelectItem value={statusField}>
                          已不在表中（{statusField}）
                        </SelectItem>
                      ) : null}
                      {fields
                        .filter((field) => field.type === 1 || field.type === 3)
                        .map((field) => (
                          <SelectItem key={field.fieldId} value={field.fieldId}>
                            {field.fieldName}（{field.fieldId}）
                          </SelectItem>
                        ))}
                    </SelectContent>
                  </Select>
                ) : (
                  <Input
                    aria-label="目标状态列 field_id"
                    value={statusField}
                    onChange={(event) => setStatusField(event.target.value)}
                    placeholder="状态列 field_id"
                    autoComplete="off"
                  />
                )}
                {fields.length > 0 &&
                statusField !== "" &&
                !fields.some((field) => field.fieldId === statusField) ? (
                  <p className="text-xs text-destructive">
                    状态列不在当前表字段中。
                  </p>
                ) : null}
              </div>
            ) : null}

            <div className="space-y-3 rounded-md border p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <p className="text-sm font-medium">支付评论</p>
                  <p className="text-xs text-muted-foreground">
                    回写成功后按目标表字段生成一条审批评论，并把状态写回状态列。
                  </p>
                </div>
                <label className="flex items-center gap-2 text-sm">
                  <Checkbox
                    checked={commentConfig.enabled}
                    onCheckedChange={(checked) =>
                      patchCommentConfig({ enabled: checked === true })
                    }
                  />
                  启用支付评论
                </label>
              </div>
              {initialRule?.commentConfig === null &&
              initialRule.commentConfigJson !== null ? (
                <p role="alert" className="text-xs text-destructive">
                  原评论配置 JSON 无法解析，当前按默认值展示；保存会覆盖它。
                </p>
              ) : null}
              {commentConfig.enabled ? (
                <div className="space-y-4">
                  <div className="space-y-1.5">
                    <Label>评论展示字段（至少一个）</Label>
                    {fields.length > 0 ? (
                      <div className="max-h-48 space-y-1 overflow-auto rounded-md border p-2">
                        {fields.map((field) => (
                          <label
                            key={field.fieldId}
                            className="flex items-center gap-2 rounded px-1 py-1 text-sm hover:bg-muted/50"
                          >
                            <Checkbox
                              checked={commentConfig.fieldIds.includes(
                                field.fieldId,
                              )}
                              onCheckedChange={(checked) =>
                                toggleCommentField(
                                  field.fieldId,
                                  checked === true,
                                )
                              }
                            />
                            <span>{field.fieldName}</span>
                            <span className="text-xs text-muted-foreground">
                              {field.fieldId}
                            </span>
                          </label>
                        ))}
                        {commentFieldsMissing.map((fieldId) => (
                          <label
                            key={fieldId}
                            className="flex items-center gap-2 rounded px-1 py-1 text-sm"
                          >
                            <Checkbox
                              checked
                              onCheckedChange={(checked) =>
                                toggleCommentField(fieldId, checked === true)
                              }
                            />
                            <span>{commentFieldLabel(fieldId)}</span>
                            <span className="text-xs text-destructive">
                              不在当前字段列表
                            </span>
                          </label>
                        ))}
                      </div>
                    ) : (
                      <div className="space-y-1">
                        <Input
                          aria-label="评论展示字段 field_id"
                          value={commentConfig.fieldIds.join(", ")}
                          onChange={(event) =>
                            setCommentFieldIdsFromText(event.target.value)
                          }
                          placeholder="fld…，逗号分隔"
                          autoComplete="off"
                        />
                        <p className="text-xs text-muted-foreground">
                          先点上方「拉取字段」可从列表勾选；也可手填 field_id。
                        </p>
                      </div>
                    )}
                  </div>

                  {commentConfig.fieldIds.length > 0 ? (
                    <>
                      <div className="space-y-1.5">
                        <Label>必填字段（任一为空则本条评论跳过）</Label>
                        <div className="flex flex-wrap gap-x-4 gap-y-1">
                          {commentConfig.fieldIds.map((fieldId) => (
                            <label
                              key={fieldId}
                              className="flex items-center gap-2 text-sm"
                            >
                              <Checkbox
                                checked={commentConfig.requiredFieldIds.includes(
                                  fieldId,
                                )}
                                onCheckedChange={(checked) =>
                                  toggleRequiredField(fieldId, checked === true)
                                }
                              />
                              {commentFieldLabel(fieldId)}
                            </label>
                          ))}
                        </div>
                      </div>

                      <div className="grid gap-3 sm:grid-cols-2">
                        <div className="space-y-1.5">
                          <Label>触发字段（可选）</Label>
                          <Select
                            value={commentConfig.triggerFieldId ?? NONE}
                            onValueChange={(value) =>
                              patchCommentConfig({
                                triggerFieldId: value === NONE ? null : value,
                                triggerValues:
                                  value === NONE
                                    ? []
                                    : commentConfig.triggerValues,
                              })
                            }
                          >
                            <SelectTrigger
                              aria-label="评论触发字段"
                              className="w-full"
                            >
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value={NONE}>不设触发字段</SelectItem>
                              {commentConfig.fieldIds.map((fieldId) => (
                                <SelectItem key={fieldId} value={fieldId}>
                                  {commentFieldLabel(fieldId)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-trigger-values">
                            触发值（每行一个，可多个）
                          </Label>
                          <textarea
                            id="export-comment-trigger-values"
                            className="min-h-16 w-full rounded-md border bg-transparent px-2 py-1 text-sm"
                            value={commentConfig.triggerValues.join("\n")}
                            disabled={commentConfig.triggerFieldId === null}
                            onChange={(event) =>
                              setTriggerValuesFromText(event.target.value)
                            }
                            placeholder={"例如：\n支付成功"}
                          />
                        </div>
                      </div>

                      <div className="grid gap-3 sm:grid-cols-2">
                        <div className="space-y-1.5">
                          <Label>审批评论状态列（单选）</Label>
                          {fields.length > 0 ? (
                            <Select
                              value={
                                commentConfig.statusFieldId === ""
                                  ? NONE
                                  : commentConfig.statusFieldId
                              }
                              onValueChange={(value) => {
                                const fieldId = value === NONE ? "" : value;
                                const field = fields.find(
                                  (item) => item.fieldId === fieldId,
                                );
                                patchCommentConfig({
                                  statusFieldId: fieldId,
                                  statusFieldName: field?.fieldName ?? "",
                                });
                              }}
                            >
                              <SelectTrigger
                                aria-label="审批评论状态列"
                                className="w-full"
                              >
                                <SelectValue placeholder="选择状态列" />
                              </SelectTrigger>
                              <SelectContent>
                                <SelectItem value={NONE}>未选择</SelectItem>
                                {commentConfig.statusFieldId !== "" &&
                                !fields.some(
                                  (field) =>
                                    field.fieldId ===
                                      commentConfig.statusFieldId &&
                                    field.type === 3,
                                ) ? (
                                  <SelectItem
                                    value={commentConfig.statusFieldId}
                                  >
                                    已不在单选列中（
                                    {commentConfig.statusFieldId}）
                                  </SelectItem>
                                ) : null}
                                {fields
                                  .filter((field) => field.type === 3)
                                  .map((field) => (
                                    <SelectItem
                                      key={field.fieldId}
                                      value={field.fieldId}
                                    >
                                      {field.fieldName}（{field.fieldId}）
                                    </SelectItem>
                                  ))}
                              </SelectContent>
                            </Select>
                          ) : (
                            <Input
                              aria-label="审批评论状态列 field_id"
                              value={commentConfig.statusFieldId}
                              onChange={(event) =>
                                patchCommentConfig({
                                  statusFieldId: event.target.value,
                                })
                              }
                              placeholder="单选列 field_id"
                              autoComplete="off"
                            />
                          )}
                        </div>
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-author">
                            评论作者 open_id
                          </Label>
                          <Input
                            id="export-comment-author"
                            value={commentConfig.authorUserId}
                            onChange={(event) =>
                              patchCommentConfig({
                                authorUserId: event.target.value,
                              })
                            }
                            placeholder="ou_…"
                            autoComplete="off"
                          />
                        </div>
                      </div>

                      <div className="grid gap-3 sm:grid-cols-2">
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-label-commented">
                            已评论文案
                          </Label>
                          <Input
                            id="export-comment-label-commented"
                            value={commentConfig.statusLabels.commented}
                            onChange={(event) =>
                              patchCommentConfig({
                                statusLabels: {
                                  ...commentConfig.statusLabels,
                                  commented: event.target.value,
                                },
                              })
                            }
                            autoComplete="off"
                          />
                        </div>
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-label-failed">
                            评论失败文案
                          </Label>
                          <Input
                            id="export-comment-label-failed"
                            value={commentConfig.statusLabels.failed}
                            onChange={(event) =>
                              patchCommentConfig({
                                statusLabels: {
                                  ...commentConfig.statusLabels,
                                  failed: event.target.value,
                                },
                              })
                            }
                            autoComplete="off"
                          />
                        </div>
                      </div>

                      <div className="grid gap-3 sm:grid-cols-2">
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-prefix">
                            评论前缀
                          </Label>
                          <Input
                            id="export-comment-prefix"
                            value={commentConfig.prefix}
                            onChange={(event) =>
                              patchCommentConfig({
                                prefix: event.target.value,
                              })
                            }
                            placeholder="【支付回填】"
                            autoComplete="off"
                          />
                        </div>
                        <div className="space-y-1.5">
                          <Label htmlFor="export-comment-url-template">
                            记录链接模板
                          </Label>
                          <Input
                            id="export-comment-url-template"
                            value={commentConfig.recordUrlTemplate}
                            onChange={(event) =>
                              patchCommentConfig({
                                recordUrlTemplate: event.target.value,
                              })
                            }
                            placeholder="https://租户域名/base/{base_token}?table={table_id}&record={record_id}"
                            autoComplete="off"
                          />
                          <p className="text-xs text-muted-foreground">
                            必须含 {"{base_token}"} / {"{table_id}"} /{" "}
                            {"{record_id}"} 三个占位符。
                          </p>
                        </div>
                      </div>

                      <label className="flex items-center gap-2 text-sm">
                        <Checkbox
                          checked={commentConfig.includeAttachments}
                          onCheckedChange={(checked) =>
                            patchCommentConfig({
                              includeAttachments: checked === true,
                            })
                          }
                        />
                        把展示字段中的附件加入评论
                      </label>
                    </>
                  ) : null}
                </div>
              ) : null}
            </div>
          </div>
        ) : null}

        {step === 5 ? (
          <div className="space-y-3">
            <div className="max-h-[45vh] space-y-3 overflow-auto">
              {maps.map((map, index) => {
                const fieldOptions =
                  map.sourceKind === "detail"
                    ? detailChildren
                    : map.sourceKind === "top"
                      ? topWidgets
                      : [];
                const selectedWidgetExists = fieldOptions.some(
                  (widget) => widget.id === map.sourceWidgetId,
                );
                return (
                  <div key={index} className="space-y-2 rounded-md border p-3">
                    <div className="flex items-center justify-between">
                      <p className="text-sm font-medium">映射 #{index + 1}</p>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={maps.length === 1}
                        onClick={() =>
                          setMaps((current) =>
                            current.filter((_, at) => at !== index),
                          )
                        }
                      >
                        移除
                      </Button>
                    </div>
                    <div className="grid gap-2 sm:grid-cols-2">
                      <div className="space-y-1">
                        <Label>来源</Label>
                        <Select
                          value={map.sourceKind}
                          onValueChange={(value) =>
                            setMaps((current) =>
                              current.map((row, at) =>
                                at === index
                                  ? {
                                      ...row,
                                      sourceKind:
                                        value as MapDraft["sourceKind"],
                                      sourceWidgetId: "",
                                    }
                                  : row,
                              ),
                            )
                          }
                        >
                          <SelectTrigger
                            aria-label={`映射 ${index + 1} 来源`}
                            className="w-full"
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value="detail">明细子控件</SelectItem>
                            <SelectItem value="top">顶层控件</SelectItem>
                            <SelectItem value="system">系统字段</SelectItem>
                            <SelectItem value="constant">常量</SelectItem>
                          </SelectContent>
                        </Select>
                      </div>
                      <div className="space-y-1">
                        <Label>目标列</Label>
                        {fields.length > 0 ? (
                          <Select
                            value={map.targetFieldId}
                            onValueChange={(value) =>
                              setMaps((current) =>
                                current.map((row, at) =>
                                  at === index
                                    ? {
                                        ...row,
                                        targetFieldId:
                                          value === NONE ? "" : value,
                                      }
                                    : row,
                                ),
                              )
                            }
                          >
                            <SelectTrigger
                              aria-label={`映射 ${index + 1} 目标列`}
                              className="w-full"
                            >
                              <SelectValue placeholder="选择目标列" />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value={NONE}>未选择</SelectItem>
                              {map.targetFieldId !== "" &&
                              !fields.some(
                                (field) =>
                                  field.fieldId === map.targetFieldId &&
                                  EXPORT_WRITABLE_FIELD_TYPES.includes(
                                    field.type,
                                  ),
                              ) ? (
                                <SelectItem value={map.targetFieldId}>
                                  {fields.some(
                                    (field) =>
                                      field.fieldId === map.targetFieldId,
                                  )
                                    ? `该列不可写（${map.targetFieldId}）`
                                    : `已不在表中（${map.targetFieldId}）`}
                                </SelectItem>
                              ) : null}
                              {fields
                                .filter((field) =>
                                  EXPORT_WRITABLE_FIELD_TYPES.includes(
                                    field.type,
                                  ),
                                )
                                .map((field) => (
                                  <SelectItem
                                    key={field.fieldId}
                                    value={field.fieldId}
                                  >
                                    {field.fieldName}（{field.fieldId}）
                                  </SelectItem>
                                ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <Input
                            aria-label={`映射 ${index + 1} 目标列 field_id`}
                            value={map.targetFieldId}
                            onChange={(event) =>
                              setMaps((current) =>
                                current.map((row, at) =>
                                  at === index
                                    ? {
                                        ...row,
                                        targetFieldId: event.target.value,
                                      }
                                    : row,
                                ),
                              )
                            }
                            placeholder="目标列 field_id"
                            autoComplete="off"
                          />
                        )}
                      </div>
                    </div>
                    <div className="grid gap-2 sm:grid-cols-2">
                      <div className="space-y-1">
                        <Label>来源控件 / 键</Label>
                        {map.sourceKind === "detail" ||
                        map.sourceKind === "top" ? (
                          <Select
                            value={map.sourceWidgetId}
                            onValueChange={(value) =>
                              setMaps((current) =>
                                current.map((row, at) =>
                                  at === index
                                    ? { ...row, sourceWidgetId: value }
                                    : row,
                                ),
                              )
                            }
                          >
                            <SelectTrigger
                              aria-label={`映射 ${index + 1} 来源控件`}
                              className="w-full"
                            >
                              <SelectValue placeholder="选择控件" />
                            </SelectTrigger>
                            <SelectContent>
                              {map.sourceWidgetId !== "" &&
                              !selectedWidgetExists ? (
                                <SelectItem value={map.sourceWidgetId}>
                                  已不在定义中（{map.sourceWidgetId}）
                                </SelectItem>
                              ) : null}
                              {fieldOptions.map((widget) => (
                                <SelectItem key={widget.id} value={widget.id}>
                                  {widgetLabel(widget)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : map.sourceKind === "system" ? (
                          <Select
                            value={map.sourceSystemKey}
                            onValueChange={(value) =>
                              setMaps((current) =>
                                current.map((row, at) =>
                                  at === index
                                    ? { ...row, sourceSystemKey: value }
                                    : row,
                                ),
                              )
                            }
                          >
                            <SelectTrigger
                              aria-label={`映射 ${index + 1} 系统字段`}
                              className="w-full"
                            >
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {EXPORT_SYSTEM_KEY_OPTIONS.map((option) => (
                                <SelectItem
                                  key={option.value}
                                  value={option.value}
                                >
                                  {option.label}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <Input
                            aria-label={`映射 ${index + 1} 常量值`}
                            value={map.constantValue}
                            onChange={(event) =>
                              setMaps((current) =>
                                current.map((row, at) =>
                                  at === index
                                    ? {
                                        ...row,
                                        constantValue: event.target.value,
                                      }
                                    : row,
                                ),
                              )
                            }
                            placeholder="固定写入的文本"
                            autoComplete="off"
                          />
                        )}
                      </div>
                      <div className="space-y-1">
                        <Label>转换器</Label>
                        <Select
                          value={map.converter}
                          onValueChange={(value) =>
                            setMaps((current) =>
                              current.map((row, at) =>
                                at === index
                                  ? { ...row, converter: value }
                                  : row,
                              ),
                            )
                          }
                        >
                          <SelectTrigger
                            aria-label={`映射 ${index + 1} 转换器`}
                            className="w-full"
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {EXPORT_CONVERTER_OPTIONS.map((option) => (
                              <SelectItem
                                key={option.value}
                                value={option.value}
                              >
                                {option.label}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </div>
                    </div>
                    {map.converter === "option_text" ||
                    map.converter === "multi_option" ? (
                      <div className="space-y-1">
                        <Label>选项文案映射（可空）</Label>
                        <textarea
                          className="min-h-16 w-full rounded-md border bg-transparent px-2 py-1 text-sm"
                          aria-label={`映射 ${index + 1} 选项映射`}
                          value={map.optionMapText}
                          onChange={(event) =>
                            setMaps((current) =>
                              current.map((row, at) =>
                                at === index
                                  ? {
                                      ...row,
                                      optionMapText: event.target.value,
                                    }
                                  : row,
                              ),
                            )
                          }
                          placeholder={"审批文案=目标选项名，每行一条"}
                        />
                      </div>
                    ) : null}
                    <label className="flex items-center gap-2 text-sm">
                      <Checkbox
                        checked={map.required}
                        onCheckedChange={(checked) =>
                          setMaps((current) =>
                            current.map((row, at) =>
                              at === index
                                ? { ...row, required: checked === true }
                                : row,
                            ),
                          )
                        }
                      />
                      目标列必填
                    </label>
                  </div>
                );
              })}
            </div>
            <Button
              variant="outline"
              onClick={() =>
                setMaps((current) => [...current, emptyMapDraft()])
              }
            >
              添加映射
            </Button>
          </div>
        ) : null}

        {step === 6 ? (
          <div className="space-y-4">
            <div className="space-y-1 rounded-md border p-3 text-sm">
              <p>
                规则：{title}（{approvalCode}）· {detailWidgetId}
              </p>
              <p>
                触发：
                {triggerMode === "final_approved" ? "最终通过" : "指定节点通过"}
                {selectedNodeIds.length > 0
                  ? `（${selectedNodeIds.join("、")}）`
                  : ""}
                {requireFinalApproval ? " · 还要求最终通过" : ""}
              </p>
              <p>
                条件：{conditionJson ?? "无条件（恒真）"} · 行筛选：
                {rowFilter === "all_rows" ? "全部明细行" : "仅满足条件的明细行"}
              </p>
              <p>
                目标：{effectiveBaseToken} / {effectiveTableId}
                {markEnabled ? ` · 状态列 ${statusField}` : " · 不维护状态列"}
              </p>
              <p>
                支付评论：
                {commentConfig.enabled
                  ? `已启用 · 状态列 ${
                      commentConfig.statusFieldName ||
                      commentConfig.statusFieldId ||
                      "未选择"
                    } · ${commentConfig.fieldIds.length} 个展示字段`
                  : "未启用"}
              </p>
              <p>映射：{maps.length} 条</p>
            </div>
            <label className="flex items-center gap-2 text-sm">
              <Checkbox
                checked={enabled}
                onCheckedChange={(checked) => setEnabled(checked === true)}
              />
              启用该规则
            </label>
            <div className="space-y-1.5">
              <Label htmlFor="export-preview-instance">
                试算实例 Code（可选）
              </Label>
              <Input
                id="export-preview-instance"
                value={previewInstance}
                onChange={(event) => setPreviewInstance(event.target.value)}
                placeholder="实例 Code；填写后保存成功即自动试算"
                autoComplete="off"
              />
            </div>
            {savedRuleId !== null ? (
              <p role="status" className="text-xs text-muted-foreground">
                规则 #{savedRuleId} 已保存；再次点击保存会更新它。
              </p>
            ) : null}
            {submitError !== null ? (
              <p role="alert" className="text-sm text-destructive">
                {submitError}
              </p>
            ) : null}
            {previewPending ? <p role="status">试算中…</p> : null}
            {previewError !== null ? (
              <p role="alert" className="text-sm text-destructive">
                试算失败：{previewError}
              </p>
            ) : null}
            {preview ? <PreviewPanel preview={preview} /> : null}
          </div>
        ) : null}

        <DialogFooter>
          <Button
            variant="ghost"
            disabled={submitting || step === 1}
            onClick={() => setStep((current) => (current - 1) as Step)}
          >
            上一步
          </Button>
          <span className="flex-1" />
          <Button variant="outline" disabled={submitting} onClick={close}>
            关闭
          </Button>
          {step < 6 ? (
            <Button
              disabled={!stepReady[step]}
              onClick={() => setStep((current) => (current + 1) as Step)}
            >
              下一步
            </Button>
          ) : (
            <Button
              disabled={submitting || !allReady}
              onClick={() => void saveAndPreview()}
            >
              {submitting ? "保存中…" : "保存并试算"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/* ----------------------------- 条件构建器 ----------------------------- */

function ConditionEditor({
  draft,
  topWidgets,
  detailChildren,
  onChange,
}: {
  draft: ConditionDraft;
  topWidgets: ApprovalWidget[];
  detailChildren: ApprovalWidget[];
  onChange: (draft: ConditionDraft) => void;
}) {
  const patchItem = (index: number, next: ConditionItemDraft) =>
    onChange({
      ...draft,
      items: draft.items.map((item, at) => (at === index ? next : item)),
    });
  const removeItem = (index: number) =>
    onChange({
      ...draft,
      items: draft.items.filter((_, at) => at !== index),
    });
  const addInstance = () =>
    onChange({
      ...draft,
      items: [
        ...draft.items,
        { scope: "instance", widgetId: "", op: "eq", valueText: "" },
      ],
    });
  const addDetail = () =>
    onChange({
      ...draft,
      items: [
        ...draft.items,
        {
          scope: "detail",
          match: "all",
          leaves: [{ widgetId: "", op: "eq", valueText: "" }],
        },
      ],
    });

  return (
    <div className="space-y-3 rounded-md border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <Label>条件组合</Label>
        <Select
          value={draft.match}
          onValueChange={(value) =>
            onChange({ ...draft, match: value === "any" ? "any" : "all" })
          }
        >
          <SelectTrigger aria-label="条件组合方式" className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">全部满足（AND）</SelectItem>
            <SelectItem value="any">任一满足（OR）</SelectItem>
          </SelectContent>
        </Select>
        <Button size="sm" variant="outline" onClick={addInstance}>
          添加实例条件
        </Button>
        <Button size="sm" variant="outline" onClick={addDetail}>
          添加明细行条件
        </Button>
      </div>
      {draft.items.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          没有条件：实例与明细行都会命中。添加条件后，缺失控件按不命中处理并留下
          drift 提示。
        </p>
      ) : null}
      {draft.items.map((item, index) =>
        item.scope === "instance" ? (
          <div
            key={`instance-${index}`}
            className="grid items-end gap-2 rounded-md bg-muted/40 p-2 sm:grid-cols-[1fr_10rem_1fr_auto]"
          >
            <div className="space-y-1">
              <Label>顶层控件</Label>
              <Select
                value={item.widgetId}
                onValueChange={(value) =>
                  patchItem(index, { ...item, widgetId: value })
                }
              >
                <SelectTrigger
                  aria-label={`实例条件 ${index + 1} 控件`}
                  className="w-full"
                >
                  <SelectValue placeholder="选择控件" />
                </SelectTrigger>
                <SelectContent>
                  {item.widgetId !== "" &&
                  !topWidgets.some((widget) => widget.id === item.widgetId) ? (
                    <SelectItem value={item.widgetId}>
                      已不在定义中（{item.widgetId}）
                    </SelectItem>
                  ) : null}
                  {topWidgets.map((widget) => (
                    <SelectItem key={widget.id} value={widget.id}>
                      {widgetLabel(widget)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <OpSelect
              value={item.op}
              onChange={(op) => patchItem(index, { ...item, op })}
              label={`实例条件 ${index + 1} 操作符`}
            />
            <ValueInput
              op={item.op}
              value={item.valueText}
              onChange={(valueText) => patchItem(index, { ...item, valueText })}
              label={`实例条件 ${index + 1} 值`}
            />
            <Button size="sm" variant="ghost" onClick={() => removeItem(index)}>
              移除
            </Button>
          </div>
        ) : (
          <div
            key={`detail-${index}`}
            className="space-y-2 rounded-md bg-muted/40 p-2"
          >
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-sm font-medium">明细行条件 #{index + 1}</p>
              <Select
                value={item.match}
                onValueChange={(value) =>
                  patchItem(index, {
                    ...item,
                    match:
                      value === "any"
                        ? "any"
                        : value === "none"
                          ? "none"
                          : "all",
                  })
                }
              >
                <SelectTrigger
                  aria-label={`明细条件 ${index + 1} 组合方式`}
                  className="w-36"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">行内全部满足</SelectItem>
                  <SelectItem value="any">行内任一满足</SelectItem>
                  <SelectItem value="none">无一行满足</SelectItem>
                </SelectContent>
              </Select>
              <Button
                size="sm"
                variant="outline"
                onClick={() =>
                  patchItem(index, {
                    ...item,
                    leaves: [
                      ...item.leaves,
                      { widgetId: "", op: "eq", valueText: "" },
                    ],
                  })
                }
              >
                添加行内条件
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => removeItem(index)}
              >
                移除
              </Button>
            </div>
            {item.leaves.map((leaf, leafIndex) => (
              <div
                key={`leaf-${leafIndex}`}
                className="grid items-end gap-2 sm:grid-cols-[1fr_10rem_1fr_auto]"
              >
                <div className="space-y-1">
                  <Label>明细子控件</Label>
                  <Select
                    value={leaf.widgetId}
                    onValueChange={(value) =>
                      patchItem(index, {
                        ...item,
                        leaves: item.leaves.map((current, at) =>
                          at === leafIndex
                            ? { ...current, widgetId: value }
                            : current,
                        ),
                      })
                    }
                  >
                    <SelectTrigger
                      aria-label={`明细条件 ${index + 1} 子控件 ${leafIndex + 1}`}
                      className="w-full"
                    >
                      <SelectValue placeholder="选择子控件" />
                    </SelectTrigger>
                    <SelectContent>
                      {leaf.widgetId !== "" &&
                      !detailChildren.some(
                        (widget) => widget.id === leaf.widgetId,
                      ) ? (
                        <SelectItem value={leaf.widgetId}>
                          已不在明细中（{leaf.widgetId}）
                        </SelectItem>
                      ) : null}
                      {detailChildren.map((widget) => (
                        <SelectItem key={widget.id} value={widget.id}>
                          {widgetLabel(widget)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <OpSelect
                  value={leaf.op}
                  onChange={(op) =>
                    patchItem(index, {
                      ...item,
                      leaves: item.leaves.map((current, at) =>
                        at === leafIndex ? { ...current, op } : current,
                      ),
                    })
                  }
                  label={`明细条件 ${index + 1} 子条件 ${leafIndex + 1} 操作符`}
                />
                <ValueInput
                  op={leaf.op}
                  value={leaf.valueText}
                  onChange={(valueText) =>
                    patchItem(index, {
                      ...item,
                      leaves: item.leaves.map((current, at) =>
                        at === leafIndex ? { ...current, valueText } : current,
                      ),
                    })
                  }
                  label={`明细条件 ${index + 1} 子条件 ${leafIndex + 1} 值`}
                />
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={item.leaves.length === 1}
                  onClick={() =>
                    patchItem(index, {
                      ...item,
                      leaves: item.leaves.filter((_, at) => at !== leafIndex),
                    })
                  }
                >
                  移除
                </Button>
              </div>
            ))}
          </div>
        ),
      )}
    </div>
  );
}

function OpSelect({
  value,
  onChange,
  label,
}: {
  value: ApprovalExportConditionOp;
  onChange: (op: ApprovalExportConditionOp) => void;
  label: string;
}) {
  return (
    <div className="space-y-1">
      <Label>操作符</Label>
      <Select
        value={value}
        onValueChange={(next) =>
          onChange(
            CONDITION_OPS.find((option) => option.value === next)?.value ??
              "eq",
          )
        }
      >
        <SelectTrigger aria-label={label} className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {CONDITION_OPS.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

function ValueInput({
  op,
  value,
  onChange,
  label,
}: {
  op: ApprovalExportConditionOp;
  value: string;
  onChange: (value: string) => void;
  label: string;
}) {
  const disabled = op === "is_empty";
  return (
    <div className="space-y-1">
      <Label>值</Label>
      <Input
        aria-label={label}
        value={disabled ? "" : value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        placeholder={
          disabled
            ? "无需填写"
            : op === "in"
              ? "逗号分隔的多个值"
              : op === "between"
                ? "下限,上限（两个数字）"
                : "条件值"
        }
        autoComplete="off"
      />
    </div>
  );
}

/* ------------------------------- 试算结果 ------------------------------ */

function PreviewPanel({ preview }: { preview: ApprovalExportPreview }) {
  return (
    <div className="space-y-3 rounded-md border p-3 text-sm">
      <p>
        触发：
        {TRIGGER_STATE_LABELS[preview.trigger.state] ?? preview.trigger.state}
        {preview.trigger.nodeId ? `（节点 ${preview.trigger.nodeId}）` : ""}
      </p>
      <p>实例条件：{preview.condition.matched ? "命中" : "未命中"}</p>
      <p>将写入 {preview.rows.length} 行</p>
      {preview.error ? (
        <p role="alert" className="text-destructive">
          试算错误：{preview.error}
        </p>
      ) : null}
      {preview.drifts.length > 0 ? (
        <div>
          <p className="font-medium">定义漂移</p>
          <ul className="list-disc pl-5 text-xs text-destructive">
            {preview.drifts.map((drift, index) => (
              <li key={index}>{drift}</li>
            ))}
          </ul>
        </div>
      ) : null}
      {preview.reviews.length > 0 ? (
        <div>
          <p className="font-medium">需人工核查</p>
          <ul className="list-disc pl-5 text-xs text-amber-700">
            {preview.reviews.map((review) => (
              <li key={review.rowKey}>
                {review.rowKey}：{review.reason}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {preview.rows.length > 0 ? (
        <div className="max-h-48 overflow-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>行键</TableHead>
                <TableHead>目标字段值</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {preview.rows.map((row) => (
                <TableRow key={row.rowKey}>
                  <TableCell className="align-top font-mono text-xs">
                    {row.rowKey}
                  </TableCell>
                  <TableCell className="align-top">
                    <pre className="whitespace-pre-wrap break-all text-xs">
                      {JSON.stringify(row.fields, null, 2)}
                    </pre>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      ) : null}
      {preview.rows[0]?.payloadHash ? (
        <p className="text-xs text-muted-foreground">
          首行内容哈希：{preview.rows[0].payloadHash}
        </p>
      ) : null}
    </div>
  );
}
