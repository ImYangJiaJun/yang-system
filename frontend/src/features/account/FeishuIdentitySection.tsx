import { useCallback, useEffect, useState } from "react";
import { useSearchParams } from "react-router";

import { formatProductDateTime } from "@/shared/lib/product-locale";
import { useToast } from "@/shared/lib/toast";
import { Button } from "@/shared/ui/button";

import {
  describeIdentityError,
  fetchFeishuBindingAvailability,
  fetchMyIdentities,
  requestFeishuBindAuthorizeUrl,
  unbindFeishuIdentity,
  type UserIdentity,
} from "./feishu-identity-api";

/// OAuth 回调只带短码（§5.2），这里补齐账号页会遇到的几种。
const CALLBACK_ERROR_TEXT: Record<string, string> = {
  bind_failed: "绑定失败：该飞书账号或当前账号可能已绑定其他身份，请确认后重试",
  already_bound: "该飞书账号已绑定其他系统账号",
  already_linked: "当前账号已绑定其他飞书账号",
  access_denied: "已取消飞书授权",
  login_expired: "授权已过期，请重新发起绑定",
  state_expired: "授权已过期，请重新发起绑定",
  invalid_state: "授权已过期，请重新发起绑定",
  login_disabled: "飞书登录未启用，请联系管理员",
  tenant_rejected: "当前飞书租户不允许绑定本系统",
  tenant_not_allowed: "当前飞书租户不允许绑定本系统",
  app_scope: "飞书应用未开通通讯录/审批权限，请联系管理员",
  20027: "飞书应用未开通通讯录/审批权限，请联系管理员",
  missing_scope: "授权缺少必要权限，请重新授权并在授权页同意全部权限",
  99991679: "授权缺少必要权限，请重新授权并在授权页同意全部权限",
};

function callbackErrorText(code: string): string {
  return CALLBACK_ERROR_TEXT[code] ?? "飞书绑定失败，请重试";
}

function formatUnixSeconds(value: number | null): string {
  return value === null ? "—" : formatProductDateTime(value * 1000);
}

/// 账号设置页的「飞书账号」区块。
///
/// - 读取登录/绑定总开关与当前用户的身份绑定；
/// - 绑定/重新授权：取授权 URL 后**整页跳转**（OAuth 不能走 fetch）；
/// - 解绑：二次确认 + 后端 Step-up（428 时给出可行动提示）。
export function FeishuIdentitySection({
  accessToken,
}: {
  accessToken: string | undefined;
}) {
  const toast = useToast();
  const [searchParams, setSearchParams] = useSearchParams();
  const [enabled, setEnabled] = useState(false);
  const [identities, setIdentities] = useState<UserIdentity[]>([]);
  const [loading, setLoading] = useState(true);
  const [errorMessage, setErrorMessage] = useState("");
  const [busy, setBusy] = useState<"bind" | "unbind" | null>(null);

  const load = useCallback(
    async (signal?: AbortSignal) => {
      setErrorMessage("");
      try {
        const found = await fetchMyIdentities(accessToken, signal);
        if (signal?.aborted) return;
        setIdentities(found);
      } catch (cause) {
        if (signal?.aborted) return;
        setErrorMessage(describeIdentityError(cause));
      }
      try {
        const availability = await fetchFeishuBindingAvailability(signal);
        if (signal?.aborted) return;
        setEnabled(availability.enabled);
      } catch {
        // 入口开关读不到就不展示绑定入口；已有绑定仍照常显示与解绑。
        if (!signal?.aborted) setEnabled(false);
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [accessToken],
  );

  useEffect(() => {
    const abort = new AbortController();
    void load(abort.signal);
    return () => abort.abort();
  }, [load]);

  // OAuth 回调落在 /account?feishu=bound 或 ?feishu_error=...；提示一次后清掉参数，
  // 避免刷新/前进后退反复弹同一条 toast。
  useEffect(() => {
    const bound = searchParams.get("feishu");
    const errorCode = searchParams.get("feishu_error");
    if (!bound && !errorCode) return;
    if (bound === "bound") toast.success("飞书账号已绑定");
    if (errorCode) toast.error(callbackErrorText(errorCode));
    const next = new URLSearchParams(searchParams);
    next.delete("feishu");
    next.delete("feishu_error");
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams, toast]);

  const feishuIdentity =
    identities.find(
      (identity) =>
        identity.provider === "feishu" && identity.status !== "revoked",
    ) ?? null;

  const startBinding = async () => {
    if (busy) return;
    if (
      !window.confirm(
        "确定绑定飞书账号吗？绑定后可用飞书登录本账号，并授权批量审批读取你的飞书待办。",
      )
    ) {
      return;
    }
    const password = window.prompt("请输入当前账号密码以确认身份");
    if (!password) return;
    setErrorMessage("");
    setBusy("bind");
    try {
      const url = await requestFeishuBindAuthorizeUrl(accessToken, password);
      // 整页跳转：授权码换 Token 必须发生在服务端。
      window.location.assign(url);
    } catch (cause) {
      setErrorMessage(describeIdentityError(cause));
      setBusy(null);
    }
  };

  const unbind = async () => {
    if (busy) return;
    if (
      !window.confirm(
        "确定解绑飞书账号吗？解绑后将无法再用飞书登录本账号，批量审批也会停止；账号本身与其他登录方式不受影响。",
      )
    ) {
      return;
    }
    const password = window.prompt("请输入当前账号密码以确认身份");
    if (!password) return;
    setErrorMessage("");
    setBusy("unbind");
    try {
      await unbindFeishuIdentity(accessToken, password);
      toast.success("飞书账号已解绑");
      await load();
    } catch (cause) {
      setErrorMessage(describeIdentityError(cause));
    } finally {
      setBusy(null);
    }
  };

  // 总开关关闭且没有既有绑定时整块不渲染：不给一个必然失败的入口。
  if (!loading && !enabled && !feishuIdentity && !errorMessage) return null;

  return (
    <section
      aria-label="飞书账号"
      className="rounded-xl border border-border bg-card p-5"
    >
      <h2 className="text-base font-medium">飞书账号</h2>
      <p className="mt-1 text-sm text-muted-foreground">
        绑定后可用飞书身份登录本系统，并进入批量审批处理你本人的飞书待办。
        飞书邮箱只用于身份匹配提示，不会写入系统邮箱。
      </p>

      {loading ? (
        <p className="mt-3 text-sm text-muted-foreground" role="status">
          正在读取绑定状态…
        </p>
      ) : feishuIdentity ? (
        <div className="mt-3 space-y-3">
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">飞书姓名</dt>
            <dd>{feishuIdentity.externalName ?? "（未记录）"}</dd>
            <dt className="text-muted-foreground">飞书邮箱</dt>
            <dd>{feishuIdentity.externalEmail ?? "（未提供）"}</dd>
            <dt className="text-muted-foreground">绑定时间</dt>
            <dd>{formatUnixSeconds(feishuIdentity.boundAt)}</dd>
            <dt className="text-muted-foreground">最近飞书登录</dt>
            <dd>{formatUnixSeconds(feishuIdentity.lastLoginAt)}</dd>
          </dl>
          <div className="flex flex-wrap gap-2">
            {enabled && (
              <Button
                type="button"
                variant="outline"
                disabled={busy !== null}
                onClick={() => void startBinding()}
              >
                {busy === "bind" ? "正在跳转…" : "重新授权飞书"}
              </Button>
            )}
            <Button
              type="button"
              variant="outline"
              className="text-destructive"
              disabled={busy !== null}
              onClick={() => void unbind()}
            >
              {busy === "unbind" ? "解绑中…" : "解绑飞书账号"}
            </Button>
          </div>
        </div>
      ) : (
        <div className="mt-3 space-y-3">
          <p className="text-sm text-muted-foreground">尚未绑定飞书账号。</p>
          {enabled && (
            <Button
              type="button"
              disabled={busy !== null}
              onClick={() => void startBinding()}
            >
              {busy === "bind" ? "正在跳转…" : "绑定飞书账号"}
            </Button>
          )}
        </div>
      )}

      {errorMessage && (
        <p role="alert" className="mt-3 text-sm text-destructive">
          {errorMessage}
        </p>
      )}
    </section>
  );
}
