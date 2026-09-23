/**
 * 一段可一键复制的只读文本。
 *
 * # 为什么抽出来
 *
 * 这个交互在本域出现两次——预检回执里的 `source_key`、详情页里的取选项接口地址。
 * 它有一串容易在复制粘贴中漏掉的细节：剪贴板在非安全上下文会抛、失败时不能假装成功、
 * 显示被截断时 `title` 必须给出全文。两处各写一遍迟早会漂移成两种行为。
 *
 * 与它配对的是一条产品约定：**控制台从不显示 Token 明文**（服务端只存摘要），
 * 所以这里复制的永远是「地址 + 标识」这类可公开的值，不是凭据。
 */

import { Check, Copy } from "lucide-react";
import { useState } from "react";

import { Button } from "@/shared/ui/button";

export type CopyFieldProps = {
  /// 要复制的**完整**内容。显示时可能被截断，`title` 与剪贴板都用这个原值。
  value: string;
  /// 这一段是什么、粘到哪里去。
  label?: string;
  /// 补充说明，渲染在下方的小字。
  hint?: string;
  /// 复制按钮的无障碍名与文案。
  copyLabel?: string;
  /// 复制成功后的文案。
  copiedLabel?: string;
};

export function CopyField({
  value,
  label,
  hint,
  copyLabel = "复制",
  copiedLabel = "已复制",
}: CopyFieldProps) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
    } catch {
      // 剪贴板不可用（无权限 / 非安全上下文）时什么都不做：文本本身可选中，
      // 而谎报「已复制」比不响应更糟——用户会去别处粘一个空的。
    }
  }

  return (
    <div className="space-y-2 rounded-md border border-border p-3">
      {label === undefined ? null : <p className="text-sm">{label}</p>}
      <div className="flex items-center gap-2">
        <code
          className="min-w-0 flex-1 truncate rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-sm"
          title={value}
        >
          {value}
        </code>
        <Button variant="outline" size="sm" onClick={() => void copy()}>
          {copied ? (
            <>
              <Check aria-hidden="true" />
              {copiedLabel}
            </>
          ) : (
            <>
              <Copy aria-hidden="true" />
              {copyLabel}
            </>
          )}
        </Button>
      </div>
      {hint === undefined ? null : (
        <p className="text-xs text-muted-foreground">{hint}</p>
      )}
    </div>
  );
}
