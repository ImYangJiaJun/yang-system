/**
 * 审批派发配置的四步建配置向导。
 *
 * ① 坐标：Base Token + 数据表（`list_bitable_tables`）；表列表为空时允许手填
 *    `table_id`。`list_bitable_*` 声明 `feishu.datasource.write`，所以本向导实际
 *    需要**同时**持有 `feishu.approval.write` 与 `feishu.datasource.write`
 *    （或系统管理员）——工具提示在第一步说明。
 * ② 审批 Code + 控件预览（`list_widgets`，展示 id / 名称 / 类型 / 必填）。
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
import type { BitableField, BitableTable } from "../types";

type Step = 1 | 2 | 3 | 4;

/// Select 不接受空串作为 item value，手填态给一个哨兵值。
const MANUAL = "__manual__";

/// 向导权限提示（设计 §5.3）：建配置的坐标/字段端点归 `feishu.datasource.write`。
const PERMISSION_NOTE =
  "建配置向导需同时持有 feishu.approval.write 与 feishu.datasource.write（或为系统管理员）：坐标与字段列表走数据源侧的 list_bitable_* 端点。";

const DEFAULT_TIMEZONE = "Asia/Shanghai";

export type ApprovalConfigDialogProps = {
  client: ApprovalWizardClient;
  onCancel: () => void;
  onSubmitted?: () => void;
  open?: boolean;
};

export function ApprovalConfigDialog({
  client,
  onCancel,
  onSubmitted,
  open = true,
}: ApprovalConfigDialogProps) {
  const [step, setStep] = useState<Step>(1);

  // ① 坐标
  const [baseToken, setBaseToken] = useState("");
  const [tables, setTables] = useState<BitableTable[]>([]);
  const [tablesPending, setTablesPending] = useState(false);
  const [tableId, setTableId] = useState("");
  const [tableManual, setTableManual] = useState(false);

  // ① 字段（第三步的候选列；表选定后一次拉回）
  const [fields, setFields] = useState<BitableField[]>([]);
  const [fieldsPending, setFieldsPending] = useState(false);

  // ② 审批 Code + 控件预览
  const [approvalCode, setApprovalCode] = useState("");
  const [widgets, setWidgets] = useState<Awaited<
    ReturnType<ApprovalWizardClient["listWidgets"]>
  > | null>(null);
  const [widgetsPending, setWidgetsPending] = useState(false);

  // ③ 列 + 时区
  const [applicantField, setApplicantField] = useState("");
  const [applicantManual, setApplicantManual] = useState(false);
  const [backfillField, setBackfillField] = useState("");
  const [backfillManual, setBackfillManual] = useState(false);
  const [timezone, setTimezone] = useState(DEFAULT_TIMEZONE);

  // ④ 提交
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // ① 拉表失败（缺 datasource 权限等）：切手填并在此展示原因。
  const [stepOneError, setStepOneError] = useState<string | null>(null);

  const step1Ready = baseToken.trim() !== "" && tableId.trim() !== "";
  const step2Ready = approvalCode.trim() !== "";
  const step3Ready =
    applicantField.trim() !== "" &&
    backfillField.trim() !== "" &&
    timezone.trim() !== "";

  async function loadTables() {
    setTablesPending(true);
    try {
      const loaded = await client.listTables(baseToken.trim());
      setTables(loaded);
      setTableId("");
      // 表列表为空时允许手填（坐标是自由文本，不该被拉取结果卡死）。
      setTableManual(loaded.length === 0);
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

  async function loadFields(table: string) {
    setFieldsPending(true);
    try {
      const loaded = await client.listFields(baseToken.trim(), table);
      setFields(loaded);
    } catch {
      // 字段拉取失败不阻塞向导：第三步允许手填 field_id/列名。
      setFields([]);
    } finally {
      setFieldsPending(false);
    }
  }

  async function loadWidgets() {
    setWidgetsPending(true);
    setSubmitError(null);
    try {
      setWidgets(await client.listWidgets(approvalCode.trim()));
    } catch (cause) {
      setSubmitError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWidgetsPending(false);
    }
  }

  async function submit() {
    setSubmitting(true);
    setSubmitError(null);
    const input: CreateApprovalConfigInput = {
      baseToken,
      tableId,
      approvalCode,
      applicantField,
      backfillField,
      baseTimezone: timezone,
    };
    try {
      await client.createConfig(input);
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
          <DialogTitle>新建审批派发配置 · 第 {step} / 4 步</DialogTitle>
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
                  onValueChange={(value) => {
                    setTableId(value);
                    void loadFields(value);
                  }}
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
            {fieldsPending ? (
              <p className="text-xs text-muted-foreground">正在拉取字段列表…</p>
            ) : null}
          </div>
        ) : null}

        {step === 2 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="approval-code">审批定义 Code</Label>
              <Input
                id="approval-code"
                value={approvalCode}
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
            {widgets !== null ? (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>控件 ID</TableHead>
                    <TableHead>名称</TableHead>
                    <TableHead>类型</TableHead>
                    <TableHead>必填</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {widgets.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={4} className="text-muted-foreground">
                        这个审批定义没有可配控件。
                      </TableCell>
                    </TableRow>
                  ) : (
                    widgets.map((widget) => (
                      <TableRow key={widget.id}>
                        <TableCell className="font-mono text-xs">
                          {widget.id}
                        </TableCell>
                        <TableCell>{widget.name}</TableCell>
                        <TableCell>{widget.type}</TableCell>
                        <TableCell>
                          {widget.required ? "必填" : "可选"}
                        </TableCell>
                      </TableRow>
                    ))
                  )}
                </TableBody>
              </Table>
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
            />
            <FieldPick
              label="回填字段"
              description="审批单创建成功后，把单号回填到的列（同上）。"
              fields={fields}
              value={backfillField}
              manual={backfillManual}
              onManualChange={setBackfillManual}
              onChange={setBackfillField}
            />
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
            <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
              <SummaryRow label="Base Token" value={baseToken} />
              <SummaryRow label="数据表" value={tableId} />
              <SummaryRow label="审批 Code" value={approvalCode} />
              <SummaryRow label="申请人员字段" value={applicantField} />
              <SummaryRow label="回填字段" value={backfillField} />
              <SummaryRow label="时区" value={timezone} />
            </dl>
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
              disabled={submitting}
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
            <Button disabled={submitting} onClick={() => void submit()}>
              {submitting ? "提交中…" : "创建配置"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
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
}: {
  label: string;
  description: string;
  fields: BitableField[];
  value: string;
  manual: boolean;
  onManualChange: (manual: boolean) => void;
  onChange: (value: string) => void;
}) {
  return (
    <div className="space-y-1.5">
      <Label>{label}</Label>
      {!manual && fields.length > 0 ? (
        <Select
          value={value}
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
        >
          {manual ? "回到下拉选择" : "列表没有想要的列？手填"}
        </Button>
      )}
    </div>
  );
}
