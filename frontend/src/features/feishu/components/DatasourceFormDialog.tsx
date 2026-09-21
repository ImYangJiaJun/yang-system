/**
 * 新建 / 重命名共用的**手写表单**对话框（决策 5）。
 *
 * 不走通用 `JsonSchemaForm`：那条路上 `#[schemars(title)]` 对可选字段不生效
 * （`effectiveSchema` 取 `anyOf` 的非 null 分支后直接返回，丢掉外层 title，且是静默的），
 * 而这里每个字段都要带自己的帮助文字与实时校验。表单一旦手写，`type="password"`
 * 与中文 Label 也就只是自己的一行。
 *
 * 三条后端约束决定了两处细节：
 * - `source_key` 是主键，**创建后不可改**；
 * - `default_locale` 是**三选一**（`zh_cn` / `en_us` / `ja_jp`，下划线形式）。
 *   后端对它零校验，而它会被原样当作回给飞书的 `locale` 键——写成 `zh-CN` 会让该
 *   数据源在飞书侧所有语言下都取不到文案，所以绝不能是自由文本；
 * - 重命名时 **留空 Token = 不轮换**：省略该字段实现，传空串会被后端拒绝。
 */

import { useEffect, useId, useState } from "react";

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

import type { DefaultLocale } from "../types";
import { DEFAULT_LOCALE, DEFAULT_LOCALE_OPTIONS } from "../types";

export type DatasourceFormMode = "create" | "rename";

/// 提交物是判别联合：重命名时没填 Token 就**不带** token 这个键（省略 = 不轮换）。
export type DatasourceFormSubmission =
  | {
      mode: "create";
      sourceKey: string;
      title: string;
      token: string;
      encryptEnabled: boolean;
      defaultLocale: DefaultLocale;
    }
  | {
      mode: "rename";
      sourceKey: string;
      title: string;
      token?: string;
    };

export type DatasourceFormDialogProps = {
  open: boolean;
  mode: DatasourceFormMode;
  /// 编辑态初值（基本类型，便于安全地作为 effect 依赖）。
  initialSourceKey?: string;
  initialTitle?: string;
  pending?: boolean;
  /// 服务端拒绝类错误：**直接回显后端原文**（例如「数据源不存在」）。
  serverError?: string | null;
  onSubmit: (submission: DatasourceFormSubmission) => void;
  onCancel: () => void;
};

const SOURCE_KEY_PATTERN = /^[a-z][a-z0-9_]{0,63}$/;

const SOURCE_KEY_HELP =
  "小写字母开头，只能包含小写字母、数字与下划线，最长 64 字节。它会进接口 URL，创建后不可修改。";

const TOKEN_CREATE_HELP =
  "服务端只保存它的 SHA-256 摘要，之后无法回显——请先复制到安全的地方。";

const TOKEN_RENAME_HELP = "留空 = 不轮换 Token。";

const ENCRYPT_HELP =
  "返回给飞书的选项内容加密传输；需服务端已配置加密密钥。未配置时飞书会收到 50002 服务端未配置加密密钥，控件取不到任何选项。";

type FieldErrors = {
  sourceKey?: string;
  title?: string;
  token?: string;
};

function validate(
  mode: DatasourceFormMode,
  values: { sourceKey: string; title: string; token: string },
): FieldErrors {
  const errors: FieldErrors = {};
  const title = values.title.trim();
  if (mode === "create" && !SOURCE_KEY_PATTERN.test(values.sourceKey.trim())) {
    errors.sourceKey = "标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]";
  }
  if (title.length === 0 || title.length > 100) {
    errors.title = "名称必须在 1..=100 字符";
  }
  if (mode === "create" && values.token.trim() === "") {
    errors.token = "接口 Token 不能为空";
  }
  return errors;
}

export function DatasourceFormDialog({
  open,
  mode,
  initialSourceKey = "",
  initialTitle = "",
  pending = false,
  serverError = null,
  onSubmit,
  onCancel,
}: DatasourceFormDialogProps) {
  const fieldId = useId();
  const [sourceKey, setSourceKey] = useState(initialSourceKey);
  const [title, setTitle] = useState(initialTitle);
  const [token, setToken] = useState("");
  const [encryptEnabled, setEncryptEnabled] = useState(false);
  const [defaultLocale, setDefaultLocale] =
    useState<DefaultLocale>(DEFAULT_LOCALE);
  const [submitted, setSubmitted] = useState(false);

  // 打开时按初值重置。依赖全是基本类型——传对象字面量会让 effect 每次渲染都跑。
  useEffect(() => {
    if (!open) return;
    setSourceKey(initialSourceKey);
    setTitle(initialTitle);
    setToken("");
    setEncryptEnabled(false);
    setDefaultLocale(DEFAULT_LOCALE);
    setSubmitted(false);
  }, [open, mode, initialSourceKey, initialTitle]);

  const errors = validate(mode, { sourceKey, title, token });
  const hasErrors = Object.keys(errors).length > 0;
  const showSourceKeyError = submitted && errors.sourceKey !== undefined;
  const showTitleError =
    (submitted || title !== "") && errors.title !== undefined;
  const showTokenError = submitted && errors.token !== undefined;

  function handleSubmit() {
    setSubmitted(true);
    if (hasErrors) return;
    const trimmedTitle = title.trim();
    if (mode === "create") {
      onSubmit({
        mode: "create",
        sourceKey: sourceKey.trim(),
        title: trimmedTitle,
        token: token.trim(),
        encryptEnabled,
        defaultLocale,
      });
      return;
    }
    const rotated = token.trim();
    onSubmit({
      mode: "rename",
      sourceKey: sourceKey.trim(),
      title: trimmedTitle,
      // 留空 = 不轮换：整个键都不出现，而不是传空串
      ...(rotated === "" ? {} : { token: rotated }),
    });
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) onCancel();
      }}
    >
      <DialogContent showCloseButton={!pending}>
        <DialogHeader>
          <DialogTitle>
            {mode === "create" ? "添加数据源" : "编辑数据源"}
          </DialogTitle>
          <DialogDescription>
            {mode === "create"
              ? "先在飞书审批后台配置好外部选项控件并记下自定义 Token，再在这里登记。"
              : "只有名称与 Token 可以改；数据源标识是主键，创建后不可修改。"}
          </DialogDescription>
        </DialogHeader>

        {serverError ? (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            {serverError}
          </p>
        ) : null}

        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor={`${fieldId}-source-key`}>数据源标识</Label>
            {mode === "create" ? (
              <Input
                id={`${fieldId}-source-key`}
                value={sourceKey}
                onChange={(event) => setSourceKey(event.target.value)}
                className="font-mono"
                aria-invalid={showSourceKeyError}
                aria-describedby={`${fieldId}-source-key-help`}
                autoComplete="off"
                spellCheck={false}
              />
            ) : (
              <p className="font-mono text-sm">{sourceKey}</p>
            )}
            <p
              id={`${fieldId}-source-key-help`}
              className="text-xs text-muted-foreground"
            >
              {SOURCE_KEY_HELP}
            </p>
            {showSourceKeyError ? (
              <p role="alert" className="text-xs text-destructive">
                {errors.sourceKey}
              </p>
            ) : null}
          </div>

          <div className="space-y-1.5">
            <Label htmlFor={`${fieldId}-title`}>名称</Label>
            <Input
              id={`${fieldId}-title`}
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              aria-invalid={showTitleError}
              aria-describedby={`${fieldId}-title-help`}
              autoComplete="off"
            />
            <p
              id={`${fieldId}-title-help`}
              className="text-xs text-muted-foreground"
            >
              展示用名称，1..=100 字符。
            </p>
            {showTitleError ? (
              <p role="alert" className="text-xs text-destructive">
                {errors.title}
              </p>
            ) : null}
          </div>

          <div className="space-y-1.5">
            <Label htmlFor={`${fieldId}-token`}>
              {mode === "create" ? "接口 Token" : "轮换 Token（可留空）"}
            </Label>
            <Input
              id={`${fieldId}-token`}
              type="password"
              value={token}
              onChange={(event) => setToken(event.target.value)}
              aria-invalid={showTokenError}
              aria-describedby={`${fieldId}-token-help`}
              autoComplete="new-password"
            />
            <p
              id={`${fieldId}-token-help`}
              className="text-xs text-muted-foreground"
            >
              {mode === "create" ? TOKEN_CREATE_HELP : TOKEN_RENAME_HELP}
            </p>
            {showTokenError ? (
              <p role="alert" className="text-xs text-destructive">
                {errors.token}
              </p>
            ) : null}
          </div>

          {mode === "create" ? (
            <>
              <div className="flex items-start gap-2">
                <Checkbox
                  id={`${fieldId}-encrypt`}
                  checked={encryptEnabled}
                  onCheckedChange={(checked) =>
                    setEncryptEnabled(checked === true)
                  }
                />
                <div className="space-y-1">
                  <Label htmlFor={`${fieldId}-encrypt`}>加密返回</Label>
                  <p className="text-xs text-muted-foreground">
                    {ENCRYPT_HELP}
                  </p>
                </div>
              </div>

              <div className="space-y-1.5">
                <Label htmlFor={`${fieldId}-locale`}>默认语言</Label>
                <Select
                  value={defaultLocale}
                  onValueChange={(value) =>
                    setDefaultLocale(value as DefaultLocale)
                  }
                >
                  <SelectTrigger id={`${fieldId}-locale`} className="w-40">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {DEFAULT_LOCALE_OPTIONS.map((option) => (
                      <SelectItem key={option.value} value={option.value}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  飞书侧控件按这个语言取文案，只有三选一。
                </p>
              </div>
            </>
          ) : null}
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={onCancel} disabled={pending}>
            取消
          </Button>
          <Button onClick={handleSubmit} disabled={pending}>
            {pending ? "提交中…" : mode === "create" ? "创建数据源" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
