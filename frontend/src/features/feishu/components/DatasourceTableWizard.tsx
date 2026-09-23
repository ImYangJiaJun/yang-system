/**
 * 表级数据源的配置向导：选表 → 选视图 → 勾字段 → 配标识与父列。
 *
 * # 四步各自为什么存在
 *
 * 1. **选表**：坐标里最外层的一段（`app_token`）。填它之前什么都拉不到。
 * 2. **选视图**：它决定**拉取哪些行**（`search` 的 `view_id`）。这一步最容易讲错，
 *    见下面 `VIEW_SCOPE_NOTE`。
 * 3. **勾字段**：全表字段都列出来，由运维判断哪些当外部选项。
 * 4. **配 `source_key` 与父列**：`source_key` 进 URL、全局唯一、创建后不可改；
 *    父列决定级联（`parent_field_id`），父必须是**同表里也勾了的**另一列。
 *
 * # 数据从哪来
 *
 * 一律走 `client`（真实实现 `api.ts::useTableWizardClient`）→ 目录里声明的 Action。
 * 组件本身不摸会话与目录，所以它可以脱离整棵应用壳被渲染与测试。
 *
 * # 表单状态为什么是 `useState`
 *
 * 与已退役的字段级表单对话框同一取舍（它随退役的字段级可写入口一起删掉了）：
 * 每个字段都有自己的帮助文字与实时校验，走通用 schema 表单或引 react-hook-form
 * 都只会多一层需要同步的状态。
 */

import { useId, useMemo, useState } from "react";

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

import type {
  CreatedTable,
  CreateTableSubmission,
  TableWizardClient,
} from "../api";
import type { BitableField, BitableTable, BitableView } from "../types";
import {
  fieldTypeLabel,
  isValidSourceKey,
  sourceKeyFromFieldId,
} from "../types";
import { FieldPickerTable } from "./FieldPickerTable";

/// **这一步最容易被讲成另一件事。**
///
/// 2026-09-23 对目标表实测：`列出字段` 的 `view_id` 参数不生效（带与不带各调一次，
/// 返回**完全相同的 30 个字段**，`field_id` 集合与顺序均一致）。视图真正的职责是
/// 决定**拉取哪些行**（《查询记录》的 `view_id`）。把这两件事讲成一件事，运维会
/// 以为「换个视图能换出一批字段」，然后白折腾一轮。
const VIEW_SCOPE_NOTE =
  "视图只决定拉取哪些行，不决定能勾哪些字段——下面第三步的字段列表是整表的，换视图不会让它变。";

/// Radix Select 不接受空串作为 item 的 value，所以「无父」另给一个哨兵值。
const NO_PARENT = "__none__";
/// 视图留空 = 取全表，同样需要一个哨兵值。
const WHOLE_TABLE_VIEW = "__whole_table__";

const SOURCE_KEY_HELP =
  "进接口 URL 路径段，全局唯一、创建后不可修改。默认按字段 ID 派生，直接可用。";

const SOURCE_KEY_ERROR =
  "标识形状不对：必须是小写字母开头的 [a-z0-9_]（小写字母开头，只含小写字母、数字与下划线），最长 64 字节。";

/// 改 `source_key` 的真实代价（设计 §9.3）。
///
/// 「创建后不可修改」是一句**程序事实**；这一句才是**代价**：源标识进的是审批控件
/// 的外部选项地址，换掉它，那边已经配好的控件会立刻取不到选项——所以换标识**必须**
/// 回审批后台把那个地址一并改掉。只讲前一句，运维会以为「反正我记住新的就行」。
const SOURCE_KEY_WARNING =
  "改这一栏之前想清楚：源标识进的是那个审批控件的外部选项地址。换一个就等于换了地址——" +
  "必须回审批后台把控件里的外部选项地址一并改掉，否则它会立刻取不到任何选项。";

const THEAD = "text-xs text-muted-foreground";

type Step = 1 | 2 | 3 | 4;

/// 一条勾选行的可编辑配置。
type RowConfig = {
  sourceKey: string;
  parentFieldId: string | null;
};

export type DatasourceTableWizardProps = {
  client: TableWizardClient;
  /// 关掉向导（用户点了取消、或关掉对话框）。
  onCancel: () => void;
  /// 创建成功。调用方通常据此回读列表并关掉向导。
  ///
  /// 一并交回那次**提交物**：回执里要写的是用户刚填的名称，而它是向导的内部状态，
  /// 调用方从响应里读不到（响应只回 `datasource_id` 与逐字段凭据）。
  onSubmitted?: (
    created: CreatedTable,
    submission: CreateTableSubmission,
  ) => void;
  open?: boolean;
};

export function DatasourceTableWizard({
  client,
  onCancel,
  onSubmitted,
  open = true,
}: DatasourceTableWizardProps) {
  const fieldId = useId();
  const [step, setStep] = useState<Step>(1);

  const [title, setTitle] = useState("");
  const [appToken, setAppToken] = useState("");

  const [tables, setTables] = useState<BitableTable[]>([]);
  const [tablesPending, setTablesPending] = useState(false);

  const [tableId, setTableId] = useState("");

  const [views, setViews] = useState<BitableView[]>([]);
  const [viewsPending, setViewsPending] = useState(false);
  const [viewId, setViewId] = useState<string>(WHOLE_TABLE_VIEW);

  const [fields, setFields] = useState<BitableField[]>([]);
  const [fieldsPending, setFieldsPending] = useState(false);

  const [configs, setConfigs] = useState<Record<string, RowConfig>>({});
  /// 一次只留一条错误，避免同一件事在屏幕上出现两遍。
  const [loadError, setLoadError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  /// 勾选顺序无关紧要，展示与提交一律按**字段列表本身的顺序**——
  /// 界面看到的行序与落到后端的行序一致，排查时才不会对不上。
  const selected = useMemo(
    () => fields.filter((field) => configs[field.fieldId] !== undefined),
    [fields, configs],
  );

  /// 已有父列的行：可选父 = 其余已勾选字段。
  const candidatesFor = (selfId: string): BitableField[] =>
    selected.filter((field) => field.fieldId !== selfId);

  const sourceKeyErrors = useMemo<string[]>(() => {
    const seen = new Map<string, number>();
    for (const field of selected) {
      const key = configs[field.fieldId]?.sourceKey.trim() ?? "";
      seen.set(key, (seen.get(key) ?? 0) + 1);
    }
    const messages: string[] = [];
    for (const field of selected) {
      const key = configs[field.fieldId]?.sourceKey.trim() ?? "";
      if (!isValidSourceKey(key)) {
        messages.push(`「${key}」${SOURCE_KEY_ERROR}`);
      } else if ((seen.get(key) ?? 0) > 1) {
        // 唯一索引在库上，撞了会以 ParamInvalid 冒泡——与其让创建整个失败，
        // 不如在这里说清是哪一行。
        messages.push(`源标识重复：${key}（它进 URL 路径段，必须全局唯一）。`);
      }
    }
    return messages;
  }, [selected, configs]);

  const canSubmit = selected.length > 0 && sourceKeyErrors.length === 0;

  function toggle(field: BitableField) {
    setConfigs((previous) => {
      if (previous[field.fieldId] !== undefined) {
        const next = { ...previous };
        delete next[field.fieldId];
        // 取消勾选会把「以它为父」的行悬空：父不在勾选集合里就读不到它的文案，
        // 后端会拒（父必须是勾选集合内的列）。这里顺手清掉，别把错误留到提交。
        for (const [key, config] of Object.entries(next)) {
          if (config.parentFieldId === field.fieldId) {
            next[key] = { ...config, parentFieldId: null };
          }
        }
        return next;
      }
      return {
        ...previous,
        [field.fieldId]: {
          sourceKey: sourceKeyFromFieldId(field.fieldId),
          parentFieldId: null,
        },
      };
    });
  }

  async function loadTables() {
    setTablesPending(true);
    setLoadError(null);
    try {
      const loaded = await client.listTables(appToken.trim());
      setTables(loaded);
      setTableId("");
      if (loaded.length === 0) {
        setLoadError(
          "这个 App 下没有数据表。核对一下 Base Token 是不是另一张表。",
        );
      }
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setTablesPending(false);
    }
  }

  async function enterStep3() {
    setStep(3);
    setFieldsPending(true);
    setLoadError(null);
    try {
      setFields(await client.listFields(appToken.trim(), tableId));
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setFieldsPending(false);
    }
  }

  async function enterStep2() {
    setStep(2);
    setViewsPending(true);
    setLoadError(null);
    try {
      setViews(await client.listViews(appToken.trim(), tableId));
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setViewsPending(false);
    }
  }

  async function submit() {
    setSubmitting(true);
    setSubmitError(null);
    const submission: CreateTableSubmission = {
      title: title.trim(),
      appToken: appToken.trim(),
      tableId,
      viewId: viewId === WHOLE_TABLE_VIEW ? "" : viewId,
      fields: selected.map((field) => ({
        fieldId: field.fieldId,
        fieldName: field.fieldName,
        type: field.type,
        sourceKey: configs[field.fieldId]?.sourceKey.trim() ?? "",
        parentFieldId: configs[field.fieldId]?.parentFieldId ?? null,
      })),
    };
    try {
      const created = await client.createTable(submission);
      onSubmitted?.(created, submission);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  }

  const alert =
    submitError ??
    (sourceKeyErrors.length > 0 ? sourceKeyErrors.join("；") : null) ??
    loadError;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !submitting) onCancel();
      }}
    >
      <DialogContent className="sm:max-w-3xl" showCloseButton={!submitting}>
        <DialogHeader>
          <DialogTitle>配置表级数据源</DialogTitle>
          <DialogDescription>
            一次配好同一张表里的多列（含它们的级联关系），一轮拉取扫一次表。
          </DialogDescription>
        </DialogHeader>

        <p className="text-sm text-muted-foreground" aria-live="polite">
          {stepLabel(step)}
        </p>

        {alert === null ? null : (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            {alert}
          </p>
        )}

        {step === 1 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor={`${fieldId}-title`}>名称</Label>
              <Input
                id={`${fieldId}-title`}
                value={title}
                onChange={(event) => setTitle(event.target.value)}
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                展示用名称，1..=100 字符。
              </p>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor={`${fieldId}-app-token`}>
                Base Token（app_token）
              </Label>
              <Input
                id={`${fieldId}-app-token`}
                value={appToken}
                onChange={(event) => setAppToken(event.target.value)}
                className="font-mono"
                autoComplete="off"
                spellCheck={false}
                placeholder="feishu.cn/base/ 后面那一段"
              />
              <Button
                variant="outline"
                size="sm"
                disabled={appToken.trim() === "" || tablesPending}
                onClick={() => void loadTables()}
              >
                {tablesPending ? "正在拉取…" : "拉取数据表"}
              </Button>
            </div>

            {tables.length > 0 ? (
              <div className="space-y-1.5">
                <Label htmlFor={`${fieldId}-table`}>数据表</Label>
                <Select value={tableId} onValueChange={setTableId}>
                  <SelectTrigger id={`${fieldId}-table`} className="w-full">
                    <SelectValue placeholder="选择要接入的数据表" />
                  </SelectTrigger>
                  <SelectContent>
                    {tables.map((table) => (
                      <SelectItem key={table.tableId} value={table.tableId}>
                        {table.name === "" ? table.tableId : table.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            ) : null}
          </div>
        ) : null}

        {step === 2 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor={`${fieldId}-view`}>视图</Label>
              <Select value={viewId} onValueChange={setViewId}>
                <SelectTrigger id={`${fieldId}-view`} className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={WHOLE_TABLE_VIEW}>
                    不限视图（整表拉取）
                  </SelectItem>
                  {views.map((view) => (
                    <SelectItem key={view.viewId} value={view.viewId}>
                      {`${view.viewName === "" ? view.viewId : view.viewName}（${view.viewId}）`}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {viewsPending ? "正在拉取视图…" : VIEW_SCOPE_NOTE}
              </p>
            </div>
          </div>
        ) : null}

        {step === 3 ? (
          <div className="space-y-3">
            <p className="text-xs text-muted-foreground">
              整表的字段都在这里，不按类型过滤：哪些列适合当外部选项的取值来源，
              由你自己判断。类型码一并带出，认不出的码照实显示原始数字。
            </p>
            {fieldsPending ? (
              <p className="text-sm text-muted-foreground">正在拉取字段…</p>
            ) : (
              <FieldPickerTable
                fields={fields}
                selectedIds={new Set(Object.keys(configs))}
                onToggle={(id) => {
                  const field = fields.find((item) => item.fieldId === id);
                  if (field) toggle(field);
                }}
              />
            )}
          </div>
        ) : null}

        {step === 4 ? (
          <div className="space-y-3">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>字段</TableHead>
                  <TableHead className={THEAD}>类型</TableHead>
                  <TableHead>源标识</TableHead>
                  <TableHead>父列</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {selected.map((field) => {
                  const config = configs[field.fieldId];
                  return (
                    <TableRow key={field.fieldId} data-slot="binding-row">
                      <TableCell className="align-top">
                        {field.fieldName}
                      </TableCell>
                      <TableCell className={`align-top ${THEAD}`}>
                        {fieldTypeLabel(field.type)}
                      </TableCell>
                      <TableCell className="align-top">
                        <Input
                          aria-label="源标识"
                          value={config?.sourceKey ?? ""}
                          onChange={(event) =>
                            setConfigs((previous) => ({
                              ...previous,
                              [field.fieldId]: {
                                sourceKey: event.target.value,
                                parentFieldId:
                                  previous[field.fieldId]?.parentFieldId ??
                                  null,
                              },
                            }))
                          }
                          className="font-mono"
                          autoComplete="off"
                          spellCheck={false}
                        />
                      </TableCell>
                      <TableCell className="align-top">
                        <Select
                          value={config?.parentFieldId ?? NO_PARENT}
                          onValueChange={(value) =>
                            setConfigs((previous) => ({
                              ...previous,
                              [field.fieldId]: {
                                sourceKey:
                                  previous[field.fieldId]?.sourceKey ?? "",
                                parentFieldId:
                                  value === NO_PARENT ? null : value,
                              },
                            }))
                          }
                        >
                          <SelectTrigger aria-label="父列" className="w-full">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value={NO_PARENT}>
                              无父列（顶级）
                            </SelectItem>
                            {/* 只列**已勾选**的其它字段：父列不在勾选集合里就读不到
                                它的文案，后端也会拒。 */}
                            {candidatesFor(field.fieldId).map((candidate) => (
                              <SelectItem
                                key={candidate.fieldId}
                                value={candidate.fieldId}
                              >
                                {candidate.fieldName}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
            <p className="text-xs text-muted-foreground">{SOURCE_KEY_HELP}</p>
            <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-xs">
              {SOURCE_KEY_WARNING}
            </p>
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
              onClick={() =>
                setStep((current) =>
                  current === 4 ? 3 : current === 3 ? 2 : 1,
                )
              }
            >
              上一步
            </Button>
          )}

          {step === 1 ? (
            <Button
              disabled={title.trim() === "" || tableId === ""}
              onClick={() => void enterStep2()}
            >
              下一步
            </Button>
          ) : step === 2 ? (
            <Button onClick={() => void enterStep3()}>下一步</Button>
          ) : step === 3 ? (
            <Button disabled={selected.length === 0} onClick={() => setStep(4)}>
              下一步
            </Button>
          ) : (
            <Button
              disabled={!canSubmit || submitting}
              onClick={() => void submit()}
            >
              {submitting ? "创建中…" : "创建数据源"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 四步的当前进度。**写死在文案里**比画一个进度条更实在：
/// 每一步的名字就是它要问的那件事。
function stepLabel(step: Step): string {
  switch (step) {
    case 1:
      return "第 1 步／共 4 步：填名称与 Base Token，拉取这个 App 下的数据表。";
    case 2:
      return "第 2 步／共 4 步：选视图。";
    case 3:
      return "第 3 步／共 4 步：勾选要作为外部选项的字段。";
    default:
      return "第 4 步／共 4 步：给每个勾选的字段配源标识与父列，然后创建。";
  }
}
