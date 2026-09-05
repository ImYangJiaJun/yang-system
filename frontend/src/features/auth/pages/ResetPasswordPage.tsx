import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";

import { resetPassword } from "@/features/auth/api";
import { publishSessionEnd } from "@/engine/session/session-coordination";
import { useSessionController } from "@/engine/session/use-session";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

/// 重置密码（旧 ResetPasswordPage.vue 语义）：一次性凭证 + 新密码；
/// 凭证可由链接 query（?token=）预填。成功后清空会话并广播凭据变更。
export default function ResetPasswordPage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [resetToken, setResetToken] = useState(searchParams.get("token") ?? "");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");
  const [passwordVisible, setPasswordVisible] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setErrorMessage("");
    if (!resetToken.trim()) {
      setErrorMessage("请输入重置凭证");
      return;
    }
    if (newPassword.length < 10) {
      setErrorMessage("新密码至少 10 个字符");
      return;
    }
    if (newPassword !== confirmPassword) {
      setErrorMessage("两次输入的新密码不一致");
      return;
    }
    setSubmitting(true);
    try {
      await resetPassword(resetToken.trim(), newPassword);
      setResetToken("");
      setNewPassword("");
      setConfirmPassword("");
      controller.clearSession("credentials-changed");
      publishSessionEnd("credentials-changed");
      navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "密码重置失败，请稍后重试",
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <main className="flex min-h-svh items-center justify-center bg-muted/40 px-4 py-8">
      <div className="w-full max-w-md rounded-xl border border-border bg-card p-8 shadow-sm">
        <div className="mb-6 space-y-1">
          <h1 className="text-xl font-semibold">重置密码</h1>
          <p className="text-sm text-muted-foreground">
            输入管理员通过受控渠道交付的一次性凭证。凭证成功使用后立即失效。
          </p>
        </div>
        <form className="space-y-4" onSubmit={submit} noValidate>
          <div className="space-y-1.5">
            <Label htmlFor="reset-token">重置凭证</Label>
            <Input
              id="reset-token"
              type="password"
              autoComplete="one-time-code"
              value={resetToken}
              onChange={(event) => setResetToken(event.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="reset-new-password">新密码</Label>
            <div className="relative">
              <Input
                id="reset-new-password"
                type={passwordVisible ? "text" : "password"}
                autoComplete="new-password"
                className="pr-16"
                value={newPassword}
                onChange={(event) => setNewPassword(event.target.value)}
              />
              <button
                type="button"
                className="absolute top-2 right-3 text-xs text-muted-foreground hover:text-foreground"
                onClick={() => setPasswordVisible((prev) => !prev)}
              >
                {passwordVisible ? "隐藏" : "显示"}
              </button>
            </div>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="reset-confirm-password">确认新密码</Label>
            <Input
              id="reset-confirm-password"
              type={passwordVisible ? "text" : "password"}
              autoComplete="new-password"
              value={confirmPassword}
              onChange={(event) => setConfirmPassword(event.target.value)}
            />
          </div>

          {errorMessage && (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {errorMessage}
            </p>
          )}

          <Button type="submit" className="w-full" disabled={submitting}>
            {submitting ? "提交中…" : "重置密码"}
          </Button>
          <Button variant="ghost" className="w-full" asChild>
            <Link to="/login">返回登录</Link>
          </Button>
        </form>
      </div>
    </main>
  );
}
