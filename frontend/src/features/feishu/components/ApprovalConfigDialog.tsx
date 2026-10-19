/**
 * 审批派发配置的四步建配置向导。
 *
 * ① 坐标：Base Token + 数据表（`list_bitable_tables`）；表列表为空时允许手填
 *    `table_id`。`list_bitable_*` 声明 `feishu.datasource.write`，所以本向导实际
 *    需要**同时**持有 `feishu.approval.write` 与 `feishu.datasource.write`
 *    （或系统管理员）——工具提示在第一步说明。
 * ② 审批 Code + 逐控件选列：首次按名称预选，重复预览保留手动映射。
 * ③ 申请人列 / 回填列（从 `list_bitable_fields` 选，列表为空时手填 field_id
 *    或列名——后端两者都接受）+ Base 时区（默认 Asia/Shanghai）。
 * ④ 提交（`create_config`）：失败时服务端校验原因一次报全，展示在最后一步。
 *
 * 数据访问一律走注入的 `client`（真实实现 `api.ts::useApprovalWizardClient`），
 * 组件不摸会话与目录，可脱离应用壳渲染与测试。
 */

import { useState } from "react";

import { Button } from "@/shared/ui/button";
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

import type { ApprovalWizardClient, CreateApprovalConfigInput } from "../api";
import {
  parseBitableLink,
  type ApprovalConfigItem,
  type ApprovalWidget,
  type BitableField,
  type BitableTable,
} from "../types";

type Step = 1 | 2 | 3 | 4;

/// Select 不接受空串作为 item value，手填态给一个哨兵值。
const MANUAL = "__manual__";

/// 向导权限提示（设计 §5.3）：建配置的坐标/字段端点归 `feishu.datasource.write`。
const PERMISSION_NOTE =
  "建配置向导需同时持有 feishu.approval.write 与 feishu.datasource.write（或为系统管理员）：坐标与字段列表走数据源侧的 list_bitable_* 端点。";

const DEFAULT_TIMEZONE = "Asia/Shanghai";

/// 第三步六个可选回填扩展列（设计 §5.1）：与 `CreateApprovalConfigInput` 同键名，
/// 空 = 不回填该列。键名同时用作表单与提交 payload 的字段名。
const EXTENDED_BACKFILL_FIELDS = [
  { key: "statusField", label: "申请状态回填列" },
  { key: "flowField", label: "审批流程回填列" },
  { key: "startTimeField", label: "发起时间回填列" },
  { key: "endTimeField", label: "完成时间回填列" },
  { key: "departmentField", label: "发起人部门回填列" },
  { key: "handlerField", label: "当前处理人回填列" },
] as const;

export type ApprovalConfigDialogProps = {
  client: ApprovalWizardClient;
  onCancel: () => void;
  onSubmitted?: () => void;
  open?: boolean;
  initialConfig?: ApprovalConfigItem;
};

export function ApprovalConfigDialog({
  client,
  onCancel,
  onSubmitted,
  open = true,
  initialConfig,
}: ApprovalConfigDialogProps) {
  const [step, setStep] = useState<Step>(initialConfig ? 2 : 1);

  // ① 坐标
  const [baseToken, setBaseToken] = useState(initialConfig?.baseToken ?? "");
  const [tables, setTables] = useState<BitableTable[]>([]);
  const [tablesPending, setTablesPending] = useState(false);
  const [tablesMessage, setTablesMessage] = useState<string | null>(null);
  const [tableId, setTableId] = useState(initialConfig?.tableId ?? "");
  const [tableManual, setTableManual] = useState(false);

  // ② 预览同时拉取控件和列，供映射和第三步使用。
  const [fields, setFields] = useState<BitableField[]>([]);

  // ② 审批 Code + 控件预览
  const [approvalCode, setApprovalCode] = useState(
    initialConfig?.approvalCode ?? "",
  );
  const [mappingKey, setMappingKey] = useState<string | null>(null);
  const [maps, setMaps] = useState<Record<string, string>>(
    Object.fromEntries(
      initialConfig?.maps.map((map) => [map.widgetId, map.bitableField]) ?? [],
    ),
  );
  const [optionSources, setOptionSources] = useState<Record<string, string>>(
    Object.fromEntries(
      initialConfig?.maps.map((map) => [
        map.widgetId,
        map.optionSourceKey ?? "",
      ]) ?? [],
    ),
  );
  const [optionDatasources, setOptionDatasources] = useState<
    Record<string, number>
  >({});
  const [widgets, setWidgets] = useState<Awaited<
    ReturnType<ApprovalWizardClient["listWidgets"]>
  > | null>(null);
  const [widgetsPending, setWidgetsPending] = useState(false);
  const [showTechnicalInfo, setShowTechnicalInfo] = useState(false);

  // ③ 列 + 时区
  const [applicantField, setApplicantField] = useState(
    initialConfig?.applicantField ?? "",
  );
  const [applicantManual, setApplicantManual] = useState(false);
  const [backfillField, setBackfillField] = useState(
    initialConfig?.backfillField ?? "",
  );
  const [backfillManual, setBackfillManual] = useState(false);
  // 六个可选回填扩展列：空串 = 不回填。
  const [extendedFields, setExtendedFields] = useState<Record<string, string>>(
    () => ({
      statusField: initialConfig?.statusField ?? "",
      flowField: initialConfig?.flowField ?? "",
      startTimeField: initialConfig?.startTimeField ?? "",
      endTimeField: initialConfig?.endTimeField ?? "",
      departmentField: initialConfig?.departmentField ?? "",
      handlerField: initialConfig?.handlerField ?? "",
    }),
  );
  const [extendedManual, setExtendedManual] = useState<Record<string, boolean>>(
    {},
  );
  const [timezone, setTimezone] = useState(
    initialConfig?.baseTimezone ?? DEFAULT_TIMEZONE,
  );

  const [title, setTitle] = useState(initialConfig?.title ?? "");

  // ④ 提交
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // ① 拉表失败（缺 datasource 权限等）：切手填并在此展示原因。
  const [stepOneError, setStepOneError] = useState<string | null>(null);

  const step1Ready = baseToken.trim() !== "" && tableId.trim() !== "";
  const currentKey = JSON.stringify([
    baseToken.trim(),
    tableId.trim(),
    approvalCode.trim(),
  ]);
  const mappingWidgets =
    widgets?.filter((widget) => widget.type !== "fieldList") ?? [];
  const step2Ready =
    mappingKey === currentKey &&
    widgets !== null &&
    Object.values(maps).some(Boolean) &&
    mappingWidgets.every(
      (widget) => !widget.required || Boolean(maps[widget.id]),
    ) &&
    mappingWidgets.every(
      (widget) =>
        !maps[widget.id] ||
        !widget.usesExternalOptions ||
        widget.optionSources?.some(
          (source) => source.sourceKey === optionSources[widget.id],
        ),
    ) &&
    Object.entries(maps).every(
      ([id, field]) =>
        !field ||
        (mappingWidgets.some((w) => w.id === id) &&
          fields.some((f) => f.fieldId === field)),
    );
  const step3Ready =
    applicantField.trim() !== "" &&
    backfillField.trim() !== "" &&
    timezone.trim() !== "";

  async function loadTables() {
    setTablesPending(true);
    setTablesMessage(null);
    setStepOneError(null);
    try {
      const rawToken = baseToken.trim();
      const parsed = parseBitableLink(rawToken);
      const token = parsed?.baseToken ?? rawToken;
      if (parsed && rawToken.includes("://")) {
        setBaseToken(parsed.baseToken);
        setTableId(parsed.tableId);
      }
      const loaded = await client.listTables(token);
      setTables(loaded);
      setTableId((current) => current.trim());
      // 表列表为空时允许手填（坐标是自由文本，不该被拉取结果卡死）。
      setTableManual(loaded.length === 0);
      setTablesMessage(
        loaded.length === 0
          ? "未查询到可用的数据表，可手填数据表 ID。"
          : `已拉取 ${loaded.length} 张数据表，请在下方选择。`,
      );
    } catch (cause) {
      // 拉取失败（典型场景=身份缺 feishu.datasource.write，目录里没有该端点）
      // 同样切手填并保留可见错误——只报错不切模式会让向导卡死在第一步。
      const message = cause instanceof Error ? cause.message : String(cause);
      setTableManual(true);
      setStepOneError(message);
    } finally {
      setTablesPending(false);
    }
  }

  async function loadWidgets() {
    setWidgetsPending(true);
    setSubmitError(null);
    try {
      const [loadedWidgets, loadedFields] = await Promise.all([
        client.listWidgets(approvalCode.trim()),
        client.listFields(baseToken.trim(), tableId.trim()),
      ]);
      if (loadedFields.length === 0)
        throw new Error("多维表格没有可用列，请确认表和读取权限后重试。");
      setFields(loadedFields);
      setWidgets(loadedWidgets);
      if (mappingKey !== currentKey) {
        const keepSavedSources = Boolean(initialConfig) && mappingKey === null;
        setOptionSources((previous) =>
          Object.fromEntries(
            loadedWidgets
              .filter((widget) => widget.usesExternalOptions)
              .map((widget) => {
                const qualified = (widget.qualifiedName ?? widget.name).trim();
                let candidates =
                  widget.optionSources?.filter(
                    (source) => source.fieldName.trim() === qualified,
                  ) ?? [];
                if (candidates.length === 0) {
                  candidates =
                    widget.optionSources?.filter(
                      (source) =>
                        source.fieldName.trim() === widget.name.trim(),
                    ) ?? [];
                }
                return [
                  widget.id,
                  (keepSavedSources && previous[widget.id]) ||
                    (candidates.length === 1 ? candidates[0].sourceKey : ""),
                ];
              }),
          ),
        );
        setOptionDatasources({});
        if (!initialConfig) {
          const owned = new Set<string>();
          setMaps(
            Object.fromEntries(
              loadedWidgets
                .filter((widget) => widget.type !== "fieldList")
                .map((widget) => {
                  const qualified = widget.qualifiedName ?? widget.name;
                  let candidates = loadedFields.filter(
                    (field) => field.fieldName.trim() === qualified.trim(),
                  );
                  if (candidates.length === 0)
                    candidates = loadedFields.filter(
                      (field) => field.fieldName.trim() === widget.name.trim(),
                    );
                  const column =
                    candidates.length === 1 && !owned.has(candidates[0].fieldId)
                      ? candidates[0].fieldId
                      : "";
                  if (column) owned.add(column);
                  return [widget.id, column];
                }),
            ),
          );
        }
        setMappingKey(currentKey);
      }
    } catch (cause) {
      setWidgets(null);
      setSubmitError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWidgetsPending(false);
    }
  }

  async function submit() {
    setSubmitting(true);
    setSubmitError(null);
    // 有值才带、空串归 None（与后端「空串 = 不回填」口径一致）。
    const extended: Partial<CreateApprovalConfigInput> = {};
    for (const { key } of EXTENDED_BACKFILL_FIELDS) {
      const value = extendedFields[key]?.trim();
      if (value) extended[key] = value;
    }
    const input: CreateApprovalConfigInput = {
      baseToken,
      tableId,
      approvalCode,
      applicantField,
      backfillField,
      baseTimezone: timezone,
      ...extended,
      maps: Object.entries(maps)
        .filter(([, field]) => field !== "")
        .map(([widgetId, bitableField]) => ({
          widgetId,
          bitableField,
          ...(mappingWidgets.find((widget) => widget.id === widgetId)
            ?.usesExternalOptions && optionSources[widgetId]
            ? { optionSourceKey: optionSources[widgetId] }
            : {}),
        })),
    };
    try {
      if (initialConfig) {
        await client.updateConfig(initialConfig.id, {
          maps: input.maps,
          baseTimezone: timezone,
          ...(title !== initialConfig.title ? { title } : {}),
          ...extended,
        });
      } else {
        await client.createConfig(input);
      }
      onSubmitted?.();
    } catch (cause) {
      // 服务端一次报全校验原因（含唯一冲突「该多维表格已配置」），原样展示。
      setSubmitError(cause instanceof Error ? cause.message : String(cause));
      setStep(4);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !submitting) onCancel();
      }}
    >
      <DialogContent className="sm:max-w-3xl" showCloseButton={!submitting}>
        <DialogHeader>
          <DialogTitle>
            {initialConfig ? "编辑" : "新建"}审批派发配置 · 第 {step} / 4 步
          </DialogTitle>
          <DialogDescription>
            {step === 1
              ? PERMISSION_NOTE
              : step === 4
                ? "确认无误后提交——服务端会再次全量校验，原因一次报全。"
                : "填写本步内容，可随时返回上一步修改。"}
          </DialogDescription>
        </DialogHeader>

        {step === 1 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="approval-base-token">多维表格 Base Token</Label>
              <Input
                id="approval-base-token"
                value={baseToken}
                disabled={Boolean(initialConfig) || tablesPending}
                onChange={(event) => setBaseToken(event.target.value)}
                placeholder="appbcbWCzen6…"
                autoComplete="off"
              />
            </div>
            <Button
              variant="outline"
              disabled={baseToken.trim() === "" || tablesPending}
              onClick={() => void loadTables()}
            >
              {tablesPending ? "拉取中…" : "拉取数据表"}
            </Button>
            {tablesMessage !== null ? (
              <p role="status" className="text-xs text-muted-foreground">
                {tablesMessage}
              </p>
            ) : null}
            {tableManual ? (
              <div className="space-y-1.5">
                <Label htmlFor="approval-table-id">数据表 ID（手填）</Label>
                <Input
                  id="approval-table-id"
                  value={tableId}
                  onChange={(event) => setTableId(event.target.value)}
                  placeholder="tbl…"
                  autoComplete="off"
                />
                <p className="text-xs text-muted-foreground">
                  Base Token 下没有拉到数据表，手填 table_id（多维表格 URL 里的
                  tbl 段）。
                </p>
                {stepOneError !== null ? (
                  <p role="alert" className="text-xs text-destructive">
                    数据表拉取失败，已切换为手填：{stepOneError}
                  </p>
                ) : null}
              </div>
            ) : (
              <div className="space-y-1.5">
                <Label>数据表</Label>
                <Select
                  value={tableId}
                  disabled={tablesPending || tables.length === 0}
                  onValueChange={setTableId}
                >
                  <SelectTrigger aria-label="数据表" className="w-full">
                    <SelectValue
                      placeholder={tablesPending ? "拉取中…" : "选择数据表"}
                    />
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
          </div>
        ) : null}

        {step === 2 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="approval-code">审批定义 Code</Label>
              <Input
                id="approval-code"
                value={approvalCode}
                disabled={Boolean(initialConfig) || widgetsPending}
                onChange={(event) => setApprovalCode(event.target.value)}
                placeholder="approval code（审批后台「审批定义」里复制）"
                autoComplete="off"
              />
            </div>
            <Button
              variant="outline"
              disabled={approvalCode.trim() === "" || widgetsPending}
              onClick={() => void loadWidgets()}
            >
              {widgetsPending ? "拉取中…" : "预览控件"}
            </Button>
            <p className="text-xs text-muted-foreground">
              首次预览按列名和控件名预选；明细子控件优先匹配“明细名称_子控件名称”，没有对应列时再匹配子控件名称。可逐项修改，重新预览保留手动选择；明细父级仅展示分组，必填子控件必须选列。
            </p>
            <Button
              variant="link"
              className="h-auto p-0 text-xs text-muted-foreground"
              onClick={() => setShowTechnicalInfo((visible) => !visible)}
            >
              {showTechnicalInfo ? "隐藏技术信息" : "显示技术信息"}
            </Button>
            {submitError !== null ? (
              <p role="alert" className="text-sm text-destructive">
                {submitError}
              </p>
            ) : null}
            {widgets !== null
              ? Object.entries(maps)
                  .filter(
                    ([id, field]) =>
                      field && !mappingWidgets.some((w) => w.id === id),
                  )
                  .map(([id]) => (
                    <p
                      key={id}
                      role="alert"
                      className="text-sm text-destructive"
                    >
                      控件{" "}
                      {initialConfig?.maps.find((m) => m.widgetId === id)
                        ?.widgetName ?? id}{" "}
                      已不在可映射控件中。
                      <Button
                        variant="link"
                        onClick={() =>
                          setMaps((previous) => ({ ...previous, [id]: "" }))
                        }
                      >
                        清除旧映射
                      </Button>
                    </p>
                  ))
              : null}
            {widgets !== null ? (
              <div className="max-h-[45vh] overflow-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>审批控件</TableHead>
                      <TableHead>对应多维表格字段</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {widgets.length === 0 ? (
                      <TableRow>
                        <TableCell
                          colSpan={2}
                          className="text-muted-foreground"
                        >
                          这个审批定义没有可配控件。
                        </TableCell>
                      </TableRow>
                    ) : (
                      widgets.map((widget) =>
                        widget.type === "fieldList" ? (
                          <TableRow key={widget.id} className="bg-muted/50">
                            <TableCell colSpan={2} className="py-2">
                              <div className="flex items-center gap-2 text-sm font-medium">
                                <span>{widget.name}</span>
                                <span className="text-xs font-normal text-muted-foreground">
                                  明细分组，无需选列
                                </span>
                              </div>
                              {showTechnicalInfo ? (
                                <div className="mt-1 flex gap-3 font-mono text-xs text-muted-foreground">
                                  <span>
                                    控件 ID：<span>{widget.id}</span>
                                  </span>
                                  <span>
                                    类型：<span>{widget.type}</span>
                                  </span>
                                </div>
                              ) : null}
                            </TableCell>
                          </TableRow>
                        ) : (
                          <TableRow key={widget.id}>
                            <TableCell className="align-top">
                              <div className="flex items-center gap-2">
                                <span className="font-medium">
                                  {widget.name}
                                </span>
                                <span
                                  className={
                                    widget.required
                                      ? "rounded-full bg-destructive/10 px-2 py-0.5 text-xs font-medium text-destructive"
                                      : "rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                                  }
                                >
                                  {widget.required ? "必填" : "可选"}
                                </span>
                                {widget.type === "connect" ? (
                                  <span className="rounded-full bg-violet-100 px-2 py-0.5 text-xs font-medium text-violet-700">
                                    关联审批
                                  </span>
                                ) : null}
                              </div>
                              {widget.qualifiedName &&
                              widget.qualifiedName !== widget.name ? (
                                <p className="mt-1 text-xs text-muted-foreground">
                                  明细路径：{widget.qualifiedName}
                                </p>
                              ) : null}
                              {showTechnicalInfo ? (
                                <div className="mt-1 flex gap-3 font-mono text-xs text-muted-foreground">
                                  <span>
                                    控件 ID：<span>{widget.id}</span>
                                  </span>
                                  <span>
                                    类型：<span>{widget.type}</span>
                                  </span>
                                </div>
                              ) : null}
                            </TableCell>
                            <TableCell className="align-top">
                              <Select
                                value={maps[widget.id] || "__none__"}
                                disabled={widgetsPending}
                                onValueChange={(value) =>
                                  setMaps((previous) => ({
                                    ...previous,
                                    [widget.id]:
                                      value === "__none__" ? "" : value,
                                  }))
                                }
                              >
                                <SelectTrigger
                                  aria-label={`${widget.qualifiedName ?? widget.name}对应的多维表格字段`}
                                  className="w-full min-w-48"
                                >
                                  <SelectValue />
                                </SelectTrigger>
                                <SelectContent>
                                  <SelectItem value="__none__">
                                    {widget.required
                                      ? "请选择字段（必填）"
                                      : "不映射"}
                                  </SelectItem>
                                  {maps[widget.id] &&
                                  !fields.some(
                                    (f) => f.fieldId === maps[widget.id],
                                  ) ? (
                                    <SelectItem value={maps[widget.id]}>
                                      字段已删除（{maps[widget.id]}）
                                    </SelectItem>
                                  ) : null}
                                  {fields.map((field) => (
                                    <SelectItem
                                      key={field.fieldId}
                                      value={field.fieldId}
                                    >
                                      {field.fieldName}（{field.fieldId}）
                                    </SelectItem>
                                  ))}
                                </SelectContent>
                              </Select>
                              {widget.usesExternalOptions ? (
                                <OptionSourcePicker
                                  widget={widget}
                                  sourceKey={optionSources[widget.id] ?? ""}
                                  datasourceId={optionDatasources[widget.id]}
                                  disabled={widgetsPending || !maps[widget.id]}
                                  onDatasourceChange={(datasourceId) => {
                                    setOptionDatasources((previous) => {
                                      const next = { ...previous };
                                      if (datasourceId === null) {
                                        delete next[widget.id];
                                      } else {
                                        next[widget.id] = datasourceId;
                                      }
                                      return next;
                                    });
                                    const belongs = widget.optionSources?.some(
                                      (source) =>
                                        source.sourceKey ===
                                          optionSources[widget.id] &&
                                        source.datasourceId === datasourceId,
                                    );
                                    if (!belongs) {
                                      setOptionSources((previous) => ({
                                        ...previous,
                                        [widget.id]: "",
                                      }));
                                    }
                                  }}
                                  onSourceChange={(sourceKey) => {
                                    const source = widget.optionSources?.find(
                                      (item) => item.sourceKey === sourceKey,
                                    );
                                    setOptionSources((previous) => ({
                                      ...previous,
                                      [widget.id]: sourceKey,
                                    }));
                                    if (source) {
                                      setOptionDatasources((previous) => ({
                                        ...previous,
                                        [widget.id]: source.datasourceId,
                                      }));
                                    }
                                  }}
                                />
                              ) : null}
                              {widget.type === "connect" ? (
                                <p className="text-muted-foreground mt-1 text-xs">
                                  关联审批：选择超链接列或实例 Code
                                  文本列，不能仅填申请编号。
                                </p>
                              ) : null}
                            </TableCell>
                          </TableRow>
                        ),
                      )
                    )}
                  </TableBody>
                </Table>
              </div>
            ) : null}
          </div>
        ) : null}

        {step === 3 ? (
          <div className="space-y-4">
            <FieldPick
              label="申请人员字段"
              description="审批单的申请人在多维表格里的列（field_id 或列名都接受，落库统一存 id）。"
              fields={fields}
              value={applicantField}
              manual={applicantManual}
              onManualChange={setApplicantManual}
              onChange={setApplicantField}
              disabled={Boolean(initialConfig)}
            />
            <FieldPick
              label="回填字段"
              description="审批单创建成功后，把单号回填到的列（同上）。"
              fields={fields}
              value={backfillField}
              manual={backfillManual}
              onManualChange={setBackfillManual}
              onChange={setBackfillField}
              disabled={Boolean(initialConfig)}
            />
            {EXTENDED_BACKFILL_FIELDS.map((field) => (
              <FieldPick
                key={field.key}
                label={field.label}
                description="留空则不回填该列。"
                fields={fields}
                value={extendedFields[field.key] ?? ""}
                manual={extendedManual[field.key] ?? false}
                onManualChange={(manual) =>
                  setExtendedManual((current) => ({
                    ...current,
                    [field.key]: manual,
                  }))
                }
                onChange={(value) =>
                  setExtendedFields((current) => ({
                    ...current,
                    [field.key]: value,
                  }))
                }
              />
            ))}
            <div className="space-y-1.5">
              <Label htmlFor="approval-timezone">Base 时区（IANA 名）</Label>
              <Input
                id="approval-timezone"
                value={timezone}
                onChange={(event) => setTimezone(event.target.value)}
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                多维表格日期是不带时区的毫秒时间戳，猜错会让审批里的时间整体偏移。
              </p>
            </div>
          </div>
        ) : null}

        {step === 4 ? (
          <div className="space-y-4">
            {initialConfig && (
              <div className="space-y-2">
                <Label htmlFor="approval-config-title">配置名称</Label>
                <Input
                  id="approval-config-title"
                  value={title}
                  maxLength={100}
                  onChange={(event) => setTitle(event.target.value)}
                />
              </div>
            )}
            <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
              <SummaryRow label="Base Token" value={baseToken} />
              <SummaryRow label="数据表" value={tableId} />
              <SummaryRow label="审批 Code" value={approvalCode} />
              <SummaryRow
                label="申请人员字段"
                value={
                  fields.find((f) => f.fieldId === applicantField)?.fieldName ??
                  applicantField
                }
              />
              <SummaryRow
                label="回填字段"
                value={
                  fields.find((f) => f.fieldId === backfillField)?.fieldName ??
                  backfillField
                }
              />
              {EXTENDED_BACKFILL_FIELDS.map((field) => {
                const value = extendedFields[field.key] ?? "";
                return value.trim() ? (
                  <SummaryRow
                    key={field.key}
                    label={field.label}
                    value={
                      fields.find((f) => f.fieldId === value)?.fieldName ??
                      value
                    }
                  />
                ) : null;
              })}
              <SummaryRow label="时区" value={timezone} />
            </dl>
            <div className="max-h-60 overflow-auto space-y-2 text-sm">
              {widgets?.map((widget) => (
                <p
                  key={widget.id}
                  className={
                    widget.type === "fieldList" ? "font-medium" : undefined
                  }
                >
                  {widget.qualifiedName ?? widget.name}
                  {widget.type === "fieldList"
                    ? "（明细分组）"
                    : ` → ${fields.find((f) => f.fieldId === maps[widget.id])?.fieldName ?? "不映射"}`}
                  {maps[widget.id] && widget.usesExternalOptions ? (
                    <span className="block text-xs text-muted-foreground">
                      外部选项：
                      {
                        widget.optionSources?.find(
                          (source) =>
                            source.sourceKey === optionSources[widget.id],
                        )?.datasourceTitle
                      }{" "}
                      ·{" "}
                      {widget.optionSources?.find(
                        (source) =>
                          source.sourceKey === optionSources[widget.id],
                      )?.fieldName || optionSources[widget.id]}
                    </span>
                  ) : null}
                </p>
              ))}
            </div>
            {submitError !== null ? (
              <div
                role="alert"
                className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              >
                {submitError}
              </div>
            ) : null}
          </div>
        ) : null}

        <DialogFooter>
          {step === 1 ? (
            <Button variant="ghost" disabled={submitting} onClick={onCancel}>
              取消
            </Button>
          ) : (
            <Button
              variant="ghost"
              disabled={submitting || (Boolean(initialConfig) && step === 2)}
              onClick={() => setStep((current) => (current - 1) as Step)}
            >
              上一步
            </Button>
          )}
          {step === 1 ? (
            <Button
              disabled={!step1Ready || tablesPending}
              onClick={() => setStep(2)}
            >
              下一步
            </Button>
          ) : step === 2 ? (
            <Button
              disabled={!step2Ready || widgetsPending}
              onClick={() => setStep(3)}
            >
              下一步
            </Button>
          ) : step === 3 ? (
            <Button disabled={!step3Ready} onClick={() => setStep(4)}>
              下一步
            </Button>
          ) : (
            <Button
              disabled={submitting || !step2Ready}
              onClick={() => void submit()}
            >
              {submitting ? "提交中…" : initialConfig ? "保存配置" : "创建配置"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 外部选项来源的两级选择：先选数据源，再选该数据源下的字段。
///
/// 已保存的 sourceKey 可能已经失效（来源停用/删除），此时数据源下拉显示
/// 「来源已失效」且字段下拉禁用，下一步由 `step2Ready` 拦住。
function OptionSourcePicker({
  widget,
  sourceKey,
  datasourceId,
  disabled,
  onDatasourceChange,
  onSourceChange,
}: {
  widget: ApprovalWidget;
  sourceKey: string;
  datasourceId: number | undefined;
  disabled: boolean;
  onDatasourceChange: (datasourceId: number | null) => void;
  onSourceChange: (sourceKey: string) => void;
}) {
  const sources = widget.optionSources ?? [];
  const saved = sources.find((source) => source.sourceKey === sourceKey);
  const invalid = sourceKey !== "" && !saved;
  const currentDatasource = datasourceId ?? saved?.datasourceId;
  const datasources = [
    ...new Map(sources.map((source) => [source.datasourceId, source])).values(),
  ];
  const fields = sources.filter(
    (source) => source.datasourceId === currentDatasource,
  );
  const datasourceValue = invalid
    ? "__invalid__"
    : currentDatasource === undefined
      ? "__none__"
      : String(currentDatasource);
  return (
    <div className="mt-2 space-y-1">
      <p className="text-xs text-muted-foreground">外部选项来源</p>
      <Select
        value={datasourceValue}
        disabled={disabled}
        onValueChange={(value) => {
          if (value === "__none__" || value === "__invalid__") {
            onDatasourceChange(null);
          } else {
            onDatasourceChange(Number(value));
          }
        }}
      >
        <SelectTrigger
          aria-label={`${widget.qualifiedName ?? widget.name}的外部选项数据源`}
          className="w-full min-w-48"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="__none__">请选择数据源</SelectItem>
          {invalid ? (
            <SelectItem value="__invalid__">
              来源已失效（{sourceKey}）
            </SelectItem>
          ) : null}
          {datasources.map((source) => (
            <SelectItem
              key={source.datasourceId}
              value={String(source.datasourceId)}
            >
              {source.datasourceTitle}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Select
        value={invalid ? "__none__" : sourceKey || "__none__"}
        disabled={disabled || currentDatasource === undefined || invalid}
        onValueChange={(value) => {
          if (value !== "__none__") onSourceChange(value);
        }}
      >
        <SelectTrigger
          aria-label={`${widget.qualifiedName ?? widget.name}的外部选项来源`}
          className="w-full min-w-48"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="__none__">请选择字段</SelectItem>
          {fields.map((source) => (
            <SelectItem key={source.sourceKey} value={source.sourceKey}>
              {source.fieldName || source.sourceKey}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {sources.length === 0 ? (
        <p className="text-xs text-destructive">
          没有可用的外部选项来源，请检查数据源及字段绑定是否启用。
        </p>
      ) : null}
    </div>
  );
}

function SummaryRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="font-mono text-xs break-all">{value}</dd>
    </div>
  );
}

/// 第三步的「字段选择或手填」：有字段列表时用下拉（值 = 列名（field_id），
/// 后端两者都接受）；列表为空或用户切到手填时用文本框。
function FieldPick({
  label,
  description,
  fields,
  value,
  manual,
  onManualChange,
  onChange,
  disabled = false,
}: {
  label: string;
  description: string;
  fields: BitableField[];
  value: string;
  manual: boolean;
  onManualChange: (manual: boolean) => void;
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <div className="space-y-1.5">
      <Label>{label}</Label>
      {!manual && fields.length > 0 ? (
        <Select
          value={value}
          disabled={disabled}
          onValueChange={(next) => {
            if (next === MANUAL) {
              onManualChange(true);
              return;
            }
            onChange(next);
          }}
        >
          <SelectTrigger aria-label={label} className="w-full">
            <SelectValue placeholder="选择列" />
          </SelectTrigger>
          <SelectContent>
            {fields.map((field) => (
              <SelectItem key={field.fieldId} value={field.fieldId}>
                {field.fieldName}（{field.fieldId}）
              </SelectItem>
            ))}
            <SelectItem value={MANUAL}>手填 field_id 或列名…</SelectItem>
          </SelectContent>
        </Select>
      ) : (
        <Input
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
          placeholder="field_id 或列名"
          autoComplete="off"
        />
      )}
      <p className="text-xs text-muted-foreground">{description}</p>
      {fields.length === 0 ? null : (
        <Button
          variant="link"
          size="sm"
          className="h-auto p-0 text-xs"
          onClick={() => onManualChange(!manual)}
          disabled={disabled}
        >
          {manual ? "回到下拉选择" : "列表没有想要的列？手填"}
        </Button>
      )}
    </div>
  );
}
