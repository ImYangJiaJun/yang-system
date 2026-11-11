import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";

import { useSessionController } from "@/engine/session/use-session";
import {
  completeFeishuMfa,
  requestFeishuMfaEmailCode,
} from "@/features/auth/api";
import { safeRedirectTarget } from "@/features/auth/redirect";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

/// 飞书登录的第二因子（公开路由 `/login/feishu/mfa`）。
///
/// 飞书身份只是第一因子：已激活 TOTP 的账号必须继续输入认证器动态码、恢复码
/// 或备用邮箱验证码。这里不提供「跳过」入口，也不接受任何客户端开关。
export default function FeishuMfaPage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const token = searchParams.get("token") ?? "";
  const redirectParam = safeRedirectTarget(searchParams.get("redirect"));

  const [code, setCode] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [sendingCode, setSendingCode] = useState(false);
  const [codeSent, setCodeSent] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");
  // token 缺失就是明确的失败闭合：不必等用户输入一次才告诉他链接失效。
  const visibleError =
    errorMessage ||
    (token ? "" : "登录链接不完整或已过期，请重新点「飞书登录」。");

  const sendEmailCode = async () => {
    if (sendingCode || submitting || !token) return;
    setSendingCode(true);
    setErrorMessage("");
    try {
      await requestFeishuMfaEmailCode(token);
      setCodeSent(true);
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "验证码发送失败，请稍后重试",
      );
    } finally {
      setSendingCode(false);
    }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    if (!token) {
      setErrorMessage("登录链接不完整或已过期，请重新点「飞书登录」。");
      return;
    }
    const trimmed = code.trim();
    if (!trimmed) {
      setErrorMessage("请输入双重验证码");
      return;
    }
    setErrorMessage("");
    setSubmitting(true);
    try {
      const result = await completeFeishuMfa({ token, mfaCode: trimmed });
      controller.beginSession({ accessToken: result.accessToken });
      navigate(safeRedirectTarget(result.redirect ?? redirectParam), {
        replace: true,
      });
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "验证失败，请稍后重试",
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <main className="flex min-h-svh items-center justify-center bg-muted/40 px-4 py-8">
      <div className="w-full max-w-md rounded-xl border border-border bg-card p-8 shadow-sm">
        <div className="mb-6 space-y-1">
          <h1 className="text-xl font-semibold">双重验证</h1>
          <p className="text-sm text-muted-foreground">
            该账号已启用双重验证。请输入认证器显示的 6
            位动态码；认证器不可用时， 也可输入一次性恢复码或备用邮箱验证码。
          </p>
        </div>

        <form className="space-y-4" noValidate onSubmit={submit}>
          <div className="space-y-1.5">
            <Label htmlFor="feishu-mfa-code">双重验证码</Label>
            <Input
              id="feishu-mfa-code"
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={64}
              autoFocus
              disabled={submitting}
              placeholder="动态码 / 恢复码 / 邮箱验证码"
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
          </div>

          {visibleError && (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {visibleError}
            </p>
          )}

          <Button type="submit" className="w-full" disabled={submitting}>
            {submitting ? "验证中…" : "验证并登录"}
          </Button>
          <Button
            type="button"
            variant="outline"
            className="w-full"
            disabled={submitting || sendingCode || !token}
            onClick={() => void sendEmailCode()}
          >
            {sendingCode
              ? "发送中…"
              : codeSent
                ? "重新发送备用邮箱验证码"
                : "发送备用邮箱验证码"}
          </Button>
          {codeSent && (
            <p className="text-center text-xs text-muted-foreground">
              验证码已发送到该账号的已验证邮箱。
            </p>
          )}
        </form>

        <p className="mt-6 text-center text-sm">
          <Link
            to="/login"
            className="text-muted-foreground hover:text-foreground"
          >
            返回登录
          </Link>
        </p>
      </div>
    </main>
  );
}
