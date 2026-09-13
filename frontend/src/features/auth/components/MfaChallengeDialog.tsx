import { useEffect, useState } from "react";

import { Button } from "@/shared/ui/button";
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

/// 6 位动态码；恢复码形如 xxxxxxxx-xxxxxxxx-xxxxxxxx-xxxxxxxx。
const TOTP_CODE_PATTERN = /^\d{6}$/;
const RECOVERY_CODE_PATTERN = /^[0-9a-f]{8}(-[0-9a-f]{8}){3}$/i;

/// 登录第二因子对话框：第一因子校验通过后弹出。
/// 输入满 6 位数字自动提交（认证器动态码或邮箱验证码同为 6 位数字）；
/// 恢复码较长不自动提交，用「验证」按钮手动提交。
/// 认证器不可用时可切换为邮箱验证码：点击链接请求向注册邮箱发码
///（由父级携带登录表单的账号密码调后端，等时重验防枚举）。
/// 父级不传 onSendEmailCode 时隐藏邮箱验证码入口（如第一因子已是
/// 邮箱验证码的登录：备用邮箱通道被服务端禁用，同类不构成双因子）。
/// 验证失败由父级回传 errorMessage，对话框自动清空输入等待重试。
export function MfaChallengeDialog({
  open,
  submitting,
  errorMessage,
  onSubmit,
  onCancel,
  onSendEmailCode,
}: {
  open: boolean;
  submitting: boolean;
  errorMessage: string;
  onSubmit: (code: string) => void;
  onCancel: () => void;
  /// 请求发送邮箱验证码；返回验证码有效期与重发冷却（秒），失败抛错。
  /// 缺省时隐藏备用邮箱通道入口。
  onSendEmailCode?: () => Promise<{ expiresIn: number; resendAfter: number }>;
}) {
  const [code, setCode] = useState("");
  const [emailSending, setEmailSending] = useState(false);
  const [emailSent, setEmailSent] = useState(false);
  const [emailExpiresIn, setEmailExpiresIn] = useState(0);
  const [emailCooldown, setEmailCooldown] = useState(0);
  const [emailError, setEmailError] = useState("");

  // 打开时重置全部状态；父级回传错误时清空输入，便于立即重试。
  useEffect(() => {
    if (open) {
      setCode("");
      setEmailSending(false);
      setEmailSent(false);
      setEmailExpiresIn(0);
      setEmailCooldown(0);
      setEmailError("");
    }
  }, [open]);
  useEffect(() => {
    if (errorMessage) setCode("");
  }, [errorMessage]);
  // 重发冷却倒计时。
  useEffect(() => {
    if (emailCooldown <= 0) return;
    const timer = setInterval(() => {
      setEmailCooldown((prev) => Math.max(0, prev - 1));
    }, 1000);
    return () => clearInterval(timer);
  }, [emailCooldown > 0]);

  const valid =
    TOTP_CODE_PATTERN.test(code) || RECOVERY_CODE_PATTERN.test(code);

  const changeCode = (raw: string) => {
    const next = raw.replace(/[^\da-zA-Z-]/g, "").slice(0, 35);
    setCode(next);
    if (TOTP_CODE_PATTERN.test(next) && !submitting) onSubmit(next);
  };

  const sendEmailCode = async () => {
    if (!onSendEmailCode || submitting || emailSending || emailCooldown > 0)
      return;
    setEmailError("");
    setEmailSending(true);
    try {
      const challenge = await onSendEmailCode();
      setEmailSent(true);
      setEmailExpiresIn(challenge.expiresIn);
      setEmailCooldown(challenge.resendAfter);
    } catch (cause) {
      setEmailError(
        cause instanceof Error ? cause.message : "验证码发送失败，请稍后重试",
      );
    } finally {
      setEmailSending(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen && !submitting) onCancel();
      }}
    >
      <DialogContent
        aria-label="双重验证"
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>双重验证</DialogTitle>
          <DialogDescription>
            {onSendEmailCode
              ? "该账号已启用双重验证，请输入认证器显示的 6 位动态码；认证器不可用时也可输入恢复码或改用邮箱验证码。"
              : "该账号已启用双重验证，请输入认证器显示的 6 位动态码；认证器不可用时也可输入恢复码。"}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-1.5">
            <Label htmlFor="login-mfa-code">双重验证码</Label>
            <Input
              id="login-mfa-code"
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={35}
              autoFocus
              disabled={submitting}
              placeholder="6 位动态码或恢复码"
              value={code}
              onChange={(event) => changeCode(event.target.value)}
            />
          </div>
          {onSendEmailCode && (
            <div className="space-y-1.5">
              <button
                type="button"
                className="text-sm text-muted-foreground hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
                disabled={submitting || emailSending || emailCooldown > 0}
                onClick={() => void sendEmailCode()}
              >
                {emailCooldown > 0
                  ? `重新发送邮箱验证码（${emailCooldown}s）`
                  : emailSending
                    ? "发送中…"
                    : emailSent
                      ? "重新发送邮箱验证码"
                      : "认证器不可用？使用邮箱验证码"}
              </button>
              {emailSent && (
                <p className="text-xs text-muted-foreground">
                  验证码已发送至您的注册邮箱，{Math.ceil(emailExpiresIn / 60)}{" "}
                  分钟内有效；也可以继续输入认证器动态码或恢复码。
                </p>
              )}
              {emailError && (
                <p role="alert" className="text-sm text-destructive">
                  {emailError}
                </p>
              )}
            </div>
          )}
          {errorMessage && (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {errorMessage}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            disabled={submitting}
            onClick={onCancel}
          >
            取消
          </Button>
          <Button
            type="button"
            disabled={submitting || !valid}
            onClick={() => onSubmit(code)}
          >
            {submitting ? "验证中…" : "验证"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
