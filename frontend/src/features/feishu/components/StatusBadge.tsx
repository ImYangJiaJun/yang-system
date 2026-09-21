/**
 * tone 语义徽标。
 *
 * **刻意不用 `shared/ui/badge` 的四个 variant**：它们是「主要 / 次要」语义，
 * 而这里要表达的是状态语气。底色一律来自 `index.css` 的 tone token
 * （`--tone-positive` / `--tone-warning` / `--tone-info`），组件里不写任何色值。
 *
 * 「加密返回」用 **info 而不是 positive**：它加密的是回给飞书的信封，与 Token 存储
 * 无关，且「开了但服务端没配密钥」在控制台内不可归因（那时它每次被飞书调用都返回
 * 50002）。把它画成健康色会得出一个与安全相关的错误结论。
 */

import type { CSSProperties, ReactNode } from "react";
import { Languages, Lock, CircleCheck, CircleSlash } from "lucide-react";

import { cn } from "@/shared/lib/utils";

import type { DatasourceStatus } from "../types";
import { localeLabel, statusLabel } from "../types";

export type ToneName = "positive" | "warning" | "info" | "neutral";

const TONE_CLASS: Record<ToneName, string> = {
  positive: "border-tone-positive/40 bg-tone-positive/10 text-tone-positive",
  warning: "border-tone-warning/40 bg-tone-warning/10 text-tone-warning",
  info: "border-tone-info/40 bg-tone-info/10 text-tone-info",
  neutral: "border-border bg-muted/50 text-muted-foreground",
};

export type StatusBadgeProps = {
  tone?: ToneName;
  icon?: ReactNode;
  /// 悬停说明：用来承载「这个标记的真实代价」这类一句话。
  title?: string;
  className?: string;
  children: ReactNode;
};

export function StatusBadge({
  tone = "neutral",
  icon,
  title,
  className,
  children,
}: StatusBadgeProps) {
  return (
    <span
      data-slot="status-badge"
      data-tone={tone}
      title={title}
      className={cn(
        "inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium",
        TONE_CLASS[tone],
        className,
      )}
    >
      {icon}
      {children}
    </span>
  );
}

/// 启用 / 已停用。
export function DatasourceStatusBadge({
  status,
}: {
  status: DatasourceStatus;
}) {
  const active = status === "active";
  return (
    <StatusBadge
      tone={active ? "positive" : "warning"}
      icon={
        active ? (
          <CircleCheck className="size-3.5" aria-hidden="true" />
        ) : (
          <CircleSlash className="size-3.5" aria-hidden="true" />
        )
      }
    >
      {statusLabel(status)}
    </StatusBadge>
  );
}

/// 「加密返回」。关掉时**不渲染任何东西**（不是渲染一个「未加密」徽标）。
export function EncryptBadge({ enabled }: { enabled: boolean }) {
  if (!enabled) return null;
  return (
    <StatusBadge
      tone="info"
      icon={<Lock className="size-3.5" aria-hidden="true" />}
      title="返回给飞书的选项内容加密传输。需要服务端已配置加密密钥，否则飞书会收到 50002 服务端未配置加密密钥。"
    >
      加密返回
    </StatusBadge>
  );
}

/// 默认语言：显示「简体中文 / English / 日本語」，不显示 `zh_cn`。
export function LocaleBadge({ locale }: { locale: string }) {
  return (
    <StatusBadge
      tone="neutral"
      icon={<Languages className="size-3.5" aria-hidden="true" />}
    >
      {localeLabel(locale)}
    </StatusBadge>
  );
}

/// 列表项上的分组徽标行：状态 + 加密返回 + 默认语言。
export function DatasourceBadgeRow({
  status,
  encryptEnabled,
  defaultLocale,
  style,
}: {
  status: DatasourceStatus;
  encryptEnabled: boolean;
  defaultLocale: string;
  style?: CSSProperties;
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5" style={style}>
      <DatasourceStatusBadge status={status} />
      <EncryptBadge enabled={encryptEnabled} />
      <LocaleBadge locale={defaultLocale} />
    </div>
  );
}
