/**
 * 门户创建 / 编辑对话框。
 *
 * 与 `DatasourceTableWizard` 不同，这里刻意用 react-hook-form + zod：本表单是一张
 * **平铺的配置表**（标题、查询键、开放列、访问模式、口令、短链、限速、展示名），
 * 字段之间只有两条交叉规则（查询键不得进开放列、口令模式口令必填），zod 的
 * `superRefine` 正好一处写清。多步向导那种「每一步都有独立帮助文字」的形态才适合
 * 用 `useState` 手写。
 *
 * `@hookform/resolvers` 没装，所以这里用一个**十几行的内联 resolver** 把 zod 的
 * `issues` 翻成 RHF 的 `errors`——为一条映射再引一个依赖不值。
 */

import { useMemo, useState } from "react";
import { useForm, type FieldErrors, type Resolver } from "react-hook-form";
import { z } from "zod";

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

import type { PortalActions } from "../portal-api";
import type {
  DatasourceFieldBinding,
  Portal,
  PortalAccessMode,
} from "../types";
import {
  CUSTOM_SLUG_PATTERN,
  parsePortalColumns,
  parsePortalLabels,
} from "../types";

const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

type PortalFormValues = {
  title: string;
  query_field_id: string;
  exposed_columns: string[];
  access_mode: PortalAccessMode;
  passphrase: string;
  custom_slug: string;
  empty_message: string;
  rate_limit_per_min: number;
  fuzzy_enabled: boolean;
  fuzzy_min_digits: number;
  result_labels: Record<string, string>;
  /// 口令有效期（天）。空 = 不过期。只在设置了口令时才有意义。
  passphrase_expires_days: string;
};

/// 有效天数 → unix 秒；空/非法回 `undefined`（= 不过期）。
function parseExpiryDays(raw: string): number | undefined {
  const days = Number(raw);
  if (!Number.isInteger(days) || days <= 0) return undefined;
  return Math.floor(Date.now() / 1000) + days * 86_400;
}

const ACCESS_MODE_OPTIONS: Array<{
  value: PortalAccessMode;
  label: string;
}> = [
  { value: "public", label: "公开（任何人可查）" },
  { value: "passphrase", label: "口令" },
  { value: "login", label: "需登录" },
];

/// 表单 schema。`isEdit` 只影响口令那一列：**创建**时口令模式必填且 ≥8 位；
/// **编辑**时留空表示不改口令，填了才校验长度。
function buildSchema(isEdit: boolean) {
  return z
    .object({
      title: z.string().trim().min(1, "标题必填").max(100, "标题最多 100 字"),
      query_field_id: z.string().min(1, "请选择查询键"),
      exposed_columns: z.array(z.string()),
      access_mode: z.enum(["public", "passphrase", "login"]),
      passphrase: z.string(),
      custom_slug: z.string(),
      empty_message: z.string().max(500, "提示最多 500 字"),
      rate_limit_per_min: z
        .number()
        .int("限速必须是整数")
        .min(1, "限速最小 1")
        .max(120, "限速最大 120"),
      fuzzy_enabled: z.boolean(),
      fuzzy_min_digits: z
        .number()
        .int("位数必须是整数")
        .min(2, "最低 2 位")
        .max(16, "最高 16 位"),
      result_labels: z.record(z.string(), z.string()),
      passphrase_expires_days: z.string(),
    })
    .superRefine((values, ctx) => {
      if (values.exposed_columns.includes(values.query_field_id)) {
        ctx.addIssue({
          code: "custom",
          path: ["exposed_columns"],
          message: "查询键不能出现在开放列中（查询键只用来查，不展示）",
        });
      }
      if (values.passphrase_expires_days !== "") {
        const days = Number(values.passphrase_expires_days);
        if (!Number.isInteger(days) || days <= 0) {
          ctx.addIssue({
            code: "custom",
            path: ["passphrase_expires_days"],
            message: "有效期必须是正整数天",
          });
        }
      }
      if (values.access_mode === "passphrase") {
        const required = !isEdit;
        if (
          (required || values.passphrase !== "") &&
          values.passphrase.length < 8
        ) {
          ctx.addIssue({
            code: "custom",
            path: ["passphrase"],
            message: "口令至少 8 位",
          });
        }
      }
      if (
        values.custom_slug !== "" &&
        !CUSTOM_SLUG_PATTERN.test(values.custom_slug)
      ) {
        ctx.addIssue({
          code: "custom",
          path: ["custom_slug"],
          message: "只能用 小写字母/数字/连字符/下划线，最长 64 位",
        });
      }
    });
}

function buildResolver(isEdit: boolean): Resolver<PortalFormValues> {
  const schema = buildSchema(isEdit);
  return (values) => {
    const parsed = schema.safeParse(values);
    if (parsed.success) return { values: parsed.data, errors: {} };
    const errors: Record<string, { type: string; message: string }> = {};
    for (const issue of parsed.error.issues) {
      const key = issue.path.map(String).join(".");
      if (key !== "" && errors[key] === undefined) {
        errors[key] = { type: "validation", message: issue.message };
      }
    }
    return { values: {}, errors: errors as FieldErrors<PortalFormValues> };
  };
}

function bindingLabel(binding: DatasourceFieldBinding): string {
  return binding.fieldName ?? binding.sourceKey;
}

function defaultValuesFor(portal: Portal | undefined): PortalFormValues {
  if (!portal) {
    return {
      title: "",
      query_field_id: "",
      exposed_columns: [],
      access_mode: "public",
      passphrase: "",
      custom_slug: "",
      empty_message: "",
      rate_limit_per_min: 30,
      fuzzy_enabled: true,
      fuzzy_min_digits: 4,
      result_labels: {},
      passphrase_expires_days: "",
    };
  }
  return {
    title: portal.title,
    query_field_id: portal.query_field_id,
    exposed_columns: parsePortalColumns(portal.exposed_columns),
    access_mode: portal.access_mode,
    passphrase: "",
    custom_slug: portal.custom_slug ?? "",
    empty_message: portal.empty_message ?? "",
    rate_limit_per_min: portal.rate_limit_per_min,
    fuzzy_enabled: portal.fuzzy_enabled,
    fuzzy_min_digits: portal.fuzzy_min_digits,
    result_labels: parsePortalLabels(portal.result_labels),
    passphrase_expires_days: "",
  };
}

export default function PortalEditDialog({
  open,
  onOpenChange,
  datasourceId,
  fields,
  portal,
  actions,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  datasourceId: number;
  /// 该数据源**启用中**的字段绑定（查询键与开放列的候选项）。
  fields: DatasourceFieldBinding[];
  /// 编辑目标；缺省 = 创建。
  portal?: Portal;
  actions: PortalActions;
  onSaved: () => void;
}) {
  const isEdit = portal !== undefined;
  const resolver = useMemo(() => buildResolver(isEdit), [isEdit]);
  const {
    register,
    handleSubmit,
    watch,
    setValue,
    formState: { errors },
  } = useForm<PortalFormValues>({
    resolver,
    defaultValues: defaultValuesFor(portal),
  });
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const accessMode = watch("access_mode");
  const queryFieldId = watch("query_field_id");
  const exposedColumns = watch("exposed_columns");
  const resultLabels = watch("result_labels");

  function toggleExposed(fieldId: string, checked: boolean) {
    const next = checked
      ? [...exposedColumns, fieldId]
      : exposedColumns.filter((id) => id !== fieldId);
    setValue("exposed_columns", next, { shouldValidate: true });
  }

  function setLabel(fieldId: string, label: string) {
    setValue("result_labels", { ...resultLabels, [fieldId]: label });
  }

  const onSubmit = handleSubmit(async (values) => {
    setSubmitError(null);
    setSubmitting(true);
    try {
      const emptyMessage = values.empty_message.trim();
      const customSlug = values.custom_slug.trim();
      const expiresAt = parseExpiryDays(values.passphrase_expires_days);
      if (portal) {
        await actions.updatePortal({
          portal_id: portal.id,
          title: values.title.trim(),
          query_field_id: values.query_field_id,
          exposed_columns: values.exposed_columns,
          access_mode: values.access_mode,
          result_labels: values.result_labels,
          empty_message: emptyMessage,
          rate_limit_per_min: values.rate_limit_per_min,
          fuzzy_enabled: values.fuzzy_enabled,
          fuzzy_min_digits: values.fuzzy_min_digits,
          custom_slug: customSlug,
        });
        // 口令是**独立端点**（`update_portal` 不收它）：只在填了的时候换一次，
        // 留空表示不动现有口令。有效期也只有换口令时才有意义。
        if (values.access_mode === "passphrase" && values.passphrase !== "") {
          await actions.setPassphrase(portal.id, values.passphrase, expiresAt);
        }
      } else {
        const created = await actions.createPortal({
          datasource_id: datasourceId,
          title: values.title.trim(),
          query_field_id: values.query_field_id,
          exposed_columns: values.exposed_columns,
          access_mode: values.access_mode,
          passphrase:
            values.access_mode === "passphrase" ? values.passphrase : undefined,
          result_labels: values.result_labels,
          empty_message: emptyMessage,
          rate_limit_per_min: values.rate_limit_per_min,
          fuzzy_enabled: values.fuzzy_enabled,
          fuzzy_min_digits: values.fuzzy_min_digits,
          custom_slug: customSlug,
        });
        // 有效期不在 `create_portal` 的入参里（后端只给了 `set_passphrase`），
        // 所以创建时填了有效期就补一次独立调用。
        if (values.access_mode === "passphrase" && expiresAt !== undefined) {
          await actions.setPassphrase(
            created.portalId,
            values.passphrase,
            expiresAt,
          );
        }
      }
      onSaved();
      onOpenChange(false);
    } catch (cause) {
      setSubmitError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSubmitting(false);
    }
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{isEdit ? "编辑门户" : "创建门户"}</DialogTitle>
          <DialogDescription>
            查询键只用来精确匹配、不展示；开放列决定结果表格里出现哪些字段。
          </DialogDescription>
        </DialogHeader>

        <form className="space-y-4" onSubmit={onSubmit}>
          <div className="space-y-2">
            <Label htmlFor="portal-title">标题</Label>
            <Input id="portal-title" {...register("title")} />
            {errors.title ? (
              <p className="text-sm text-destructive">{errors.title.message}</p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="portal-query-field">查询键</Label>
            <Select
              value={queryFieldId}
              onValueChange={(next) =>
                setValue("query_field_id", next, { shouldValidate: true })
              }
            >
              <SelectTrigger id="portal-query-field" className="w-full">
                <SelectValue placeholder="选择用来精确匹配的字段" />
              </SelectTrigger>
              <SelectContent>
                {fields.map((binding) => (
                  <SelectItem key={binding.fieldId} value={binding.fieldId}>
                    {bindingLabel(binding)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {errors.query_field_id ? (
              <p className="text-sm text-destructive">
                {errors.query_field_id.message}
              </p>
            ) : null}
          </div>

          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">开放列</legend>
            <div className="space-y-1.5">
              {fields.map((binding) => {
                const isQueryKey = binding.fieldId === queryFieldId;
                return (
                  <label
                    key={binding.fieldId}
                    className="flex items-center gap-2 text-sm"
                  >
                    <Checkbox
                      checked={exposedColumns.includes(binding.fieldId)}
                      disabled={isQueryKey}
                      onCheckedChange={(checked) =>
                        toggleExposed(binding.fieldId, checked === true)
                      }
                    />
                    <span>
                      {bindingLabel(binding)}
                      {isQueryKey ? (
                        <span className="ml-1 text-xs text-muted-foreground">
                          （查询键，不展示）
                        </span>
                      ) : null}
                    </span>
                  </label>
                );
              })}
            </div>
            {errors.exposed_columns ? (
              <p className="text-sm text-destructive">
                {errors.exposed_columns.message}
              </p>
            ) : null}
          </fieldset>

          <div className="space-y-2">
            <Label htmlFor="portal-access-mode">访问模式</Label>
            <Select
              value={accessMode}
              onValueChange={(next) =>
                setValue("access_mode", next as PortalAccessMode, {
                  shouldValidate: true,
                })
              }
            >
              <SelectTrigger id="portal-access-mode" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {ACCESS_MODE_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {accessMode === "passphrase" ? (
            <div className="space-y-2">
              <Label htmlFor="portal-passphrase">
                口令{isEdit ? "（留空表示不修改）" : ""}
              </Label>
              <Input
                id="portal-passphrase"
                type="password"
                autoComplete="new-password"
                {...register("passphrase")}
              />
              {errors.passphrase ? (
                <p className="text-sm text-destructive">
                  {errors.passphrase.message}
                </p>
              ) : null}
            </div>
          ) : null}

          {accessMode === "passphrase" ? (
            <div className="space-y-2">
              <Label htmlFor="portal-passphrase-expires">
                口令有效期（天，可选）
              </Label>
              <Input
                id="portal-passphrase-expires"
                type="number"
                min={1}
                placeholder="留空表示不过期"
                {...register("passphrase_expires_days")}
              />
              {errors.passphrase_expires_days ? (
                <p className="text-sm text-destructive">
                  {errors.passphrase_expires_days.message}
                </p>
              ) : null}
            </div>
          ) : null}

          <div className="space-y-2">
            <Label htmlFor="portal-custom-slug">自定义短链（可选）</Label>
            <Input
              id="portal-custom-slug"
              placeholder="如 bank-card"
              {...register("custom_slug")}
            />
            {errors.custom_slug ? (
              <p className="text-sm text-destructive">
                {errors.custom_slug.message}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="portal-empty-message">未找到提示（可选）</Label>
            <Input
              id="portal-empty-message"
              placeholder="如 未查询到该卡号"
              {...register("empty_message")}
            />
            {errors.empty_message ? (
              <p className="text-sm text-destructive">
                {errors.empty_message.message}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="portal-rate-limit">限速（次 / 分钟）</Label>
            <Input
              id="portal-rate-limit"
              type="number"
              min={1}
              max={120}
              {...register("rate_limit_per_min", { valueAsNumber: true })}
            />
            {errors.rate_limit_per_min ? (
              <p className="text-sm text-destructive">
                {errors.rate_limit_per_min.message}
              </p>
            ) : null}
          </div>

          <div className="flex items-center gap-2">
            <Checkbox
              id="portal-fuzzy-enabled"
              checked={watch("fuzzy_enabled")}
              onCheckedChange={(checked) =>
                setValue("fuzzy_enabled", checked === true)
              }
            />
            <Label htmlFor="portal-fuzzy-enabled">启用模糊匹配</Label>
          </div>

          <div className="space-y-2">
            <Label htmlFor="portal-fuzzy-min-digits">模糊匹配最低位数</Label>
            <Input
              id="portal-fuzzy-min-digits"
              type="number"
              min={2}
              max={16}
              {...register("fuzzy_min_digits", { valueAsNumber: true })}
            />
            {errors.fuzzy_min_digits ? (
              <p className="text-sm text-destructive">
                {errors.fuzzy_min_digits.message}
              </p>
            ) : (
              <p className="text-xs text-muted-foreground">
                输入中至少包含这么多位连续数字时才会触发模糊查询
              </p>
            )}
          </div>

          {exposedColumns.length > 0 ? (
            <fieldset className="space-y-2">
              <legend className="text-sm font-medium">
                展示名映射（可选）
              </legend>
              <div className="space-y-2">
                {fields
                  .filter((binding) => exposedColumns.includes(binding.fieldId))
                  .map((binding) => (
                    <div
                      key={binding.fieldId}
                      className="flex items-center gap-2"
                    >
                      <span className="w-32 shrink-0 truncate text-sm text-muted-foreground">
                        {bindingLabel(binding)}
                      </span>
                      <Input
                        value={resultLabels[binding.fieldId] ?? ""}
                        placeholder="展示名"
                        onChange={(event) =>
                          setLabel(binding.fieldId, event.target.value)
                        }
                      />
                    </div>
                  ))}
              </div>
            </fieldset>
          ) : null}

          {submitError !== null ? (
            <p role="alert" className={ERROR_BAR}>
              {submitError}
            </p>
          ) : null}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={submitting}
            >
              取消
            </Button>
            <Button type="submit" disabled={submitting}>
              {submitting ? "提交中…" : isEdit ? "保存" : "创建"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
