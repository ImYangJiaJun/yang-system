import { useEffect, useState } from "react";
import { Button } from "@/shared/ui/button";
import { useToast } from "@/shared/lib/toast";
import { formatProductDateTime } from "@/shared/lib/product-locale";
import { useFeishuOAuth } from "../api";
import type { FeishuOAuthPurpose } from "../types";

/// unix 秒 → 产品 locale 的可读时间。
function formatUnixSeconds(value: number) {
  return formatProductDateTime(value * 1000);
}

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

/**
 * 飞书用户身份（OAuth）授权卡片。
 *
 * 派发与回写是两条独立链路，各绑一个飞书用户身份：`purpose="dispatch"` 解析发起人姓名，
 * `purpose="comment"` 作为审批评论作者。`GET /open-apis/search/v1/user` 只接受
 * user_access_token；没授权时带发起人的派发不会创建实例，评论也不会发出——把「有没有
 * 授权」摆到台面上是这张卡片唯一的职责。
 *
 * 授权在**新窗口**里完成（飞书授权页只能由浏览器打开）：本窗口重新获得焦点时刷新一次
 * 状态，省得管理员手动刷新页面。
 */
export function FeishuOAuthCard({ purpose }: { purpose: FeishuOAuthPurpose }) {
  const toast = useToast();
  const label = purpose === "dispatch" ? "审批派发" : "审批回写";
  const { canRead, canWrite, status, authorizeUrl } = useFeishuOAuth(purpose);
  const [pending, setPending] = useState(false);
  const refetch = status.refetch;

  useEffect(() => {
    const onFocus = () => void refetch();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refetch]);

  // 目录里没有这一粒（无读权限）时整张卡片不渲染：权限就是「目录里有没有」。
  if (!canRead) return null;

  async function authorize() {
    setPending(true);
    try {
      const url = await authorizeUrl();
      window.open(url, "_blank", "noopener,noreferrer");
      toast.success("已打开飞书授权页；完成后回到本页即可看到状态更新。");
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setPending(false);
    }
  }

  const data = status.data;
  const authorized = data?.authorized === true;
  const reauthDue = data?.reauthDue === true;

  return (
    <section
      className="rounded-lg border p-4"
      aria-label={`飞书用户身份授权（${label}）`}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium">飞书用户身份 · {label}</h2>
          {status.isPending ? (
            <p className="mt-1 text-sm text-muted-foreground" role="status">
              正在读取授权状态…
            </p>
          ) : status.isError ? (
            <p className="mt-1 text-sm text-destructive" role="alert">
              读取授权状态失败：{messageOf(status.error)}
            </p>
          ) : authorized ? (
            <p className="mt-1 text-sm text-muted-foreground">
              已授权：{data?.name ?? "（未记录姓名）"}
              {data?.authorizedAt
                ? ` · ${formatUnixSeconds(data.authorizedAt)}`
                : ""}
            </p>
          ) : (
            <p className="mt-1 text-sm text-muted-foreground">
              {purpose === "dispatch"
                ? "尚未授权。审批派发的发起人解析依赖这个身份；未授权时，带「发起人」的派发不会创建实例。"
                : "尚未授权。审批评论的作者依赖这个身份；未授权时，审批评论不会发出。"}
            </p>
          )}
          {reauthDue && (
            <p className="mt-2 text-sm text-destructive">
              授权已满 350 天：飞书要求 365
              天内重新授权，否则凭证失效。系统每天会发提醒邮件，直到重新授权。
            </p>
          )}
          {authorized && !reauthDue && (
            <p className="mt-2 text-xs text-muted-foreground">
              该身份只用于{label}
              。访问凭证由系统自动续期；只有换账号或到期时才需要重新授权。
            </p>
          )}
        </div>
        {canWrite && (
          <Button
            variant={authorized && !reauthDue ? "outline" : "default"}
            disabled={pending || status.isPending}
            onClick={() => void authorize()}
          >
            {authorized ? "重新授权" : "去授权"}
          </Button>
        )}
      </div>
    </section>
  );
}
