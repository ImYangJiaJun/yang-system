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
 *
 * 「加密返回 / 默认语言」**编辑态也要能改**：`update_datasource` 收这两个字段，
 * 默认语言选错会让该数据源在所有语言下都取不到文案——控制台必须留一条修复路径。
 * 两条规矩管着它们：
 * - 初值只能来自真实记录。**取不到现值就不渲染对应控件**（见 `encryptEditable` /
 *   `localeEditable`），拿一个猜的值当现状再写回去，等于把用户没说过的设置改掉；
 * - 现值认不出（`default_locale` 后端零校验，可能是 `zh-CN`）就**留空**并说明原因，
 *   改动只能由用户显式选一项产生——静默归一到 `zh_cn` 是未经确认地改写用户数据。
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
import {
  DEFAULT_LOCALE,
  DEFAULT_LOCALE_OPTIONS,
  asDefaultLocale,
} from "../types";

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
      /// 编辑态的两项：**只有对话框渲染了对应控件时才带上**（没渲染就是不知道现值，
      /// 省略 = 保持原值，绝不拿一个猜的值写回去）。
      encryptEnabled?: boolean;
      defaultLocale?: DefaultLocale;
    };

export type DatasourceFormDialogProps = {
  open: boolean;
  mode: DatasourceFormMode;
  /// 编辑态初值（基本类型，便于安全地作为 effect 依赖）。
  initialSourceKey?: string;
  initialTitle?: string;
  /// 编辑态「加密返回」的现值。**取不到就别给**：给了才渲染这个控件
  /// （能改的前提是界面知道现状），不给就不渲染。
  initialEncryptEnabled?: boolean;
  /// 编辑态「默认语言」的现值，**原样**给。后端对它零校验，所以它可能是取值域以外的值
  /// （例如 `zh-CN`）——认得出来就作为三选一的选中项，认不出就留空让用户显式改，
  /// 界面对它既不猜也不替换。完全取不到（回执入口没找到那一行）时不给，控件不渲染。
  initialDefaultLocale?: string;
  pending?: boolean;
  /// 服务端拒绝类错误：**直接回显后端原文**（例如「数据源不存在」）。
  serverError?: string | null;
  onSubmit: (submission: DatasourceFormSubmission) => void;
  onCancel: () => void;
};

/// 「默认语言」在打开时该选中哪一项。
///
/// 新建恒从 `zh_cn` 起；编辑则只认**认得出来**的值——认不出就留空（Radix 会显示
/// 占位符），让用户显式选一项来修好。静默替换成 `zh_cn` 是另一回事：那是未经确认地
/// 改写用户数据，而那个值恰恰就是「控件在所有语言下都取不到文案」的元凶。
function localeSelection(
  mode: DatasourceFormMode,
  raw: string | undefined,
): DefaultLocale | null {
  if (mode === "create") return DEFAULT_LOCALE;
  return raw === undefined ? null : asDefaultLocale(raw);
}

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
  initialEncryptEnabled,
  initialDefaultLocale,
  pending = false,
  serverError = null,
  onSubmit,
  onCancel,
}: DatasourceFormDialogProps) {
  const fieldId = useId();
  const [sourceKey, setSourceKey] = useState(initialSourceKey);
  const [title, setTitle] = useState(initialTitle);
  const [token, setToken] = useState("");
  const [encryptEnabled, setEncryptEnabled] = useState(
    initialEncryptEnabled ?? false,
  );
  const [defaultLocale, setDefaultLocale] = useState<DefaultLocale | null>(() =>
    localeSelection(mode, initialDefaultLocale),
  );
  const [submitted, setSubmitted] = useState(false);

  // 打开时按初值重置。依赖全是基本类型——传对象字面量会让 effect 每次渲染都跑。
  useEffect(() => {
    if (!open) return;
    setSourceKey(initialSourceKey);
    setTitle(initialTitle);
    setToken("");
    setEncryptEnabled(initialEncryptEnabled ?? false);
    setDefaultLocale(localeSelection(mode, initialDefaultLocale));
    setSubmitted(false);
  }, [
    open,
    mode,
    initialSourceKey,
    initialTitle,
    initialEncryptEnabled,
    initialDefaultLocale,
  ]);

  const errors = validate(mode, { sourceKey, title, token });
  const hasErrors = Object.keys(errors).length > 0;
  const showSourceKeyError = submitted && errors.sourceKey !== undefined;
  const showTitleError =
    (submitted || title !== "") && errors.title !== undefined;
  const showTokenError = submitted && errors.token !== undefined;
  // 两项各自开关：知道现状才渲染（见 props 上的说明），互不牵连——
  // 默认语言是取值域外的值，不该连带把「加密返回」也锁住。
  const encryptEditable =
    mode === "create" || initialEncryptEnabled !== undefined;
  const localeEditable =
    mode === "create" || initialDefaultLocale !== undefined;
  // 库里存着一个取值域以外的默认语言：这不会让控件报错，只会让它在所有语言下都取不到文案。
  const offDomainLocale =
    mode === "rename" &&
    initialDefaultLocale !== undefined &&
    asDefaultLocale(initialDefaultLocale) === null;

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
        // 新建时恒有选中项（`localeSelection` 从 zh_cn 起）
        defaultLocale: defaultLocale ?? DEFAULT_LOCALE,
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
      // 没渲染（或没选）就不发：省略 = 保持原值，正好对上「不知道现状就别动它」
      ...(encryptEditable ? { encryptEnabled } : {}),
      ...(defaultLocale === null ? {} : { defaultLocale }),
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
              : encryptEditable && localeEditable
                ? "数据源标识是主键，创建后不可修改；名称、Token、加密返回与默认语言都可以改。"
                : "数据源标识是主键，创建后不可修改；这里可以改名称与 Token。"}
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

          {encryptEditable ? (
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
                <p className="text-xs text-muted-foreground">{ENCRYPT_HELP}</p>
              </div>
            </div>
          ) : null}

          {localeEditable ? (
            <div className="space-y-1.5">
              <Label htmlFor={`${fieldId}-locale`}>默认语言</Label>
              <Select
                // 选中项可能是「无」（现值认不出时）：空串会让 Radix 显示占位符，
                // 而省略 value 会把它变成非受控——两者都不改动的语义要分清。
                value={defaultLocale ?? ""}
                onValueChange={(value) =>
                  setDefaultLocale(value as DefaultLocale)
                }
              >
                <SelectTrigger id={`${fieldId}-locale`} className="w-40">
                  <SelectValue placeholder="未选择（不修改）" />
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
              {offDomainLocale ? (
                <p className="text-xs text-destructive">
                  这个数据源当前存的默认语言是「{initialDefaultLocale}」，
                  不在取值域内——飞书侧控件在所有语言下都会取不到文案。选一项保存即可修好。
                </p>
              ) : null}
            </div>
          ) : null}

          {mode === "rename" && !encryptEditable && !localeEditable ? (
            <p className="text-xs text-muted-foreground">
              读不到这个数据源当前的「加密返回 / 默认语言」，这两项这次不显示——
              从列表页那一行的「重命名」入口进来就能改。
            </p>
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
