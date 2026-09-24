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
 *
 * # 复制失败时必须说话（2026-09-24 修）
 *
 * 明文 HTTP 部署下 `navigator.clipboard` 不存在，原来的空 `catch` 让这里变成
 * 「点了没反应」。现在失败会渲染一条 `role="alert"`，并指向那条已被 CSS 截断的
 * 文字——它带 `select-all`，点一下就是全选，退路不需要剪贴板接口。
 */

import { Check, Copy } from "lucide-react";
import { useState } from "react";

import { copyText } from "@/shared/lib/clipboard";
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
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");

  async function copy() {
    // 谎报「已复制」比不响应更糟——用户会去别处粘一个空的。
    // 所以这里按 copyText 的真实结果分流，失败时把退路摆出来。
    setState((await copyText(value)) === "copied" ? "copied" : "failed");
  }

  return (
    <div className="space-y-2 rounded-md border border-border p-3">
      {label === undefined ? null : <p className="text-sm">{label}</p>}
      <div className="flex items-center gap-2">
        <code
          // `select-all`：点一下就是全选，不依赖剪贴板接口。这段文字被 CSS 截断显示，
          // 靠鼠标拖着选只能选到看得见的部分，所以一键全选是必要的退路。
          className="min-w-0 flex-1 truncate rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-sm select-all"
          title={value}
        >
          {value}
        </code>
        <Button variant="outline" size="sm" onClick={() => void copy()}>
          {state === "copied" ? (
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
      {state === "failed" ? (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {
            // 只说「这次没写成」这个已知事实，再点出最常见的原因——
            // 断言「就是明文 HTTP 干的」在 HTTPS 上（例如权限被拒）就是假话。
          }
          浏览器这次没允许写剪贴板，明文 HTTP 页面最常见的原因是这个。
          上面的文字点一下即可全选，再按 Ctrl/Cmd + C 复制。
        </p>
      ) : null}
      {hint === undefined ? null : (
        <p className="text-xs text-muted-foreground">{hint}</p>
      )}
    </div>
  );
}
