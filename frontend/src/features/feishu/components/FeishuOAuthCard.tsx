import { useEffect, useState } from "react";
import { Button } from "@/shared/ui/button";
import { useToast } from "@/shared/lib/toast";
import { useFeishuOAuth } from "../api";

/// unix 秒 → 本地可读时间。
function formatUnixSeconds(value: number) {
  return new Date(value * 1000).toLocaleString();
}

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

/**
 * 飞书用户身份（OAuth）授权卡片。
 *
 * 为什么要有这张卡片：审批派发的「发起人姓名」要经 `contact/v3/users/search` 解析，
 * 而该接口**只接受 user_access_token**。没授权时带发起人的派发必然失败，且失败发生在
 * 工作流那一侧、控制台看不见——把「有没有授权」摆到台面上是这张卡片唯一的职责。
 *
 * 授权在**新窗口**里完成（飞书授权页只能由浏览器打开）：本窗口重新获得焦点时刷新一次
 * 状态，省得管理员手动刷新页面。
 */
export function FeishuOAuthCard() {
  const toast = useToast();
  const { canRead, canWrite, status, authorizeUrl } = useFeishuOAuth();
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
    <section className="rounded-lg border p-4" aria-label="飞书用户身份授权">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium">飞书用户身份</h2>
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
              尚未授权。未授权时，带「发起人」的审批派发无法解析姓名，不会创建实例。
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
              访问凭证由系统自动续期；只有换账号或到期时才需要重新授权。
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
