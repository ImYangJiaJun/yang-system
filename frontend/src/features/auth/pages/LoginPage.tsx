import { useEffect, useState, type FormEvent } from "react";
import { Eye, EyeOff, Lock, Mail, User } from "lucide-react";
import { useNavigate, useSearchParams, Link } from "react-router";

import { login } from "@/engine/session/lifecycle";
import { loginByEmailCode } from "@/engine";
import { SecondFactorRequiredError } from "@/engine/http/errors";
import {
  useSessionController,
  useSessionSnapshot,
} from "@/engine/session/use-session";
import {
  requestLoginEmailCode,
  requestMfaEmailCode,
} from "@/features/auth/api";
import { MfaChallengeDialog } from "@/features/auth/components/MfaChallengeDialog";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import logoDarkUrl from "@/shared/assets/logo-dark.png";
import logoLightUrl from "@/shared/assets/logo-light.png";

/// 登录页（对齐旧 LoginPage.vue 语义）：品牌面板 + 凭据表单 + 错误/提示横幅。
/// 两段式登录：账号启用 TOTP 时，第一因子校验通过（SecondFactorRequired）后
/// 弹出双重验证对话框，输入动态码/恢复码带原凭据重新提交。
/// 验证码登录模式：邮箱 + 一次性验证码免密登录；第二段重发同一验证码，
/// 备用邮箱通道不可用（第一因子已是邮箱持有，同类不构成双因子）。
type LoginMode = "password" | "email-code";

/// 仅做「明显非法」的前端基础提示，安全语义全在后端。
const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export default function LoginPage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const snapshot = useSessionSnapshot();
  const [mode, setMode] = useState<LoginMode>("password");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [passwordVisible, setPasswordVisible] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");
  // 验证码登录模式：发码冷却与送达提示。
  const [loginEmail, setLoginEmail] = useState("");
  const [emailCode, setEmailCode] = useState("");
  const [codeSent, setCodeSent] = useState(false);
  const [codeCooldown, setCodeCooldown] = useState(0);
  const [sendingCode, setSendingCode] = useState(false);
  const [infoMessage, setInfoMessage] = useState("");
  // 第二因子阶段：记录触发 MFA 的登录方式，第二段按原方式重发凭据。
  const [mfaMode, setMfaMode] = useState<LoginMode | null>(null);
  const [mfaError, setMfaError] = useState("");
  // 会话结束原因优先读控制器快照（失效传播），兼容外部链接的 ?reason= 参数。
  const endReason = snapshot.sessionEndReason ?? searchParams.get("reason");
  const reasonMessage =
    endReason === "credentials-changed"
      ? "凭据已变更，请使用新密码重新登录"
      : endReason === "session-expired"
        ? "登录状态已过期，请重新登录"
        : "";
  const successMessage =
    searchParams.get("registered") === "1" ? "账号已创建，请登录" : "";

  /// 统一的登录尝试：第一阶段（无码）或第二阶段（带 mfaCode）。
  const attemptLogin = async (mfaCode?: string) => {
    setErrorMessage("");
    setMfaError("");
    setSubmitting(true);
    try {
      const result = await login(username.trim(), password, mfaCode);
      controller.beginSession(result);
      navigate("/", { replace: true });
    } catch (cause) {
      if (cause instanceof SecondFactorRequiredError) {
        // 密码已通过：进入第二因子阶段。
        setMfaMode("password");
      } else if (mfaCode !== undefined) {
        // 第二阶段失败：留在对话框内提示，输入框由对话框自动清空。
        setMfaError(
          cause instanceof Error ? cause.message : "登录失败，请稍后重试",
        );
      } else {
        // 后端错误（401/错误码 envelope）已由 api/auth 映射为 ApiError.message
        setErrorMessage(
          cause instanceof Error ? cause.message : "登录失败，请稍后重试",
        );
      }
    } finally {
      setSubmitting(false);
    }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    if (mode === "email-code") {
      await submitEmailCode();
      return;
    }
    if (!username.trim()) {
      setErrorMessage("请输入帐号");
      return;
    }
    if (!password) {
      setErrorMessage("请输入密码");
      return;
    }
    await attemptLogin();
  };

  const switchMode = (next: LoginMode) => {
    if (next === mode) return;
    setMode(next);
    setErrorMessage("");
    setInfoMessage("");
    // 切换登录方式即放弃未完成的第二因子阶段。
    setMfaMode(null);
    setMfaError("");
  };

  const emailLooksValid = EMAIL_PATTERN.test(loginEmail.trim());

  /// 发送登录验证码：成功进入 resend_after 冷却，冷却结束才可重发。
  const sendLoginEmailCode = async () => {
    if (sendingCode || codeCooldown > 0) return;
    setErrorMessage("");
    setInfoMessage("");
    const email = loginEmail.trim();
    if (!email || !EMAIL_PATTERN.test(email)) {
      setErrorMessage("请输入有效的邮箱地址");
      return;
    }
    setSendingCode(true);
    try {
      const challenge = await requestLoginEmailCode(email);
      setCodeSent(true);
      setCodeCooldown(challenge.resendAfter);
      setInfoMessage("验证码已发送，请查收邮箱后输入完成登录");
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "发送失败，请稍后重试",
      );
    } finally {
      setSendingCode(false);
    }
  };

  useEffect(() => {
    if (codeCooldown <= 0) return;
    const timer = setInterval(() => {
      setCodeCooldown((prev) => Math.max(0, prev - 1));
    }, 1000);
    return () => clearInterval(timer);
  }, [codeCooldown > 0]);

  /// 验证码登录提交：第一段（无码）或第二段（带 mfaCode 重发同一验证码）。
  const attemptEmailCodeLogin = async (mfaCode?: string) => {
    setErrorMessage("");
    setInfoMessage("");
    setMfaError("");
    setSubmitting(true);
    try {
      const result = await loginByEmailCode(
        loginEmail.trim(),
        emailCode.trim(),
        mfaCode,
      );
      controller.beginSession(result);
      navigate("/", { replace: true });
    } catch (cause) {
      if (cause instanceof SecondFactorRequiredError) {
        // 验证码已通过（服务端只验不消费）：进入第二因子阶段。
        setMfaMode("email-code");
      } else if (mfaCode !== undefined) {
        // 第二阶段失败：留在对话框内提示，输入框由对话框自动清空。
        setMfaError(
          cause instanceof Error ? cause.message : "登录失败，请稍后重试",
        );
      } else {
        setErrorMessage(
          cause instanceof Error ? cause.message : "登录失败，请稍后重试",
        );
      }
    } finally {
      setSubmitting(false);
    }
  };

  const submitEmailCode = async () => {
    const email = loginEmail.trim();
    if (!email || !EMAIL_PATTERN.test(email)) {
      setErrorMessage("请输入有效的邮箱地址");
      return;
    }
    if (!emailCode.trim()) {
      setErrorMessage("请输入邮箱验证码");
      return;
    }
    await attemptEmailCodeLogin();
  };

  return (
    <main className="flex min-h-svh bg-background text-foreground">
      <section className="hidden flex-1 items-center justify-center bg-primary/5 md:flex">
        <div className="max-w-sm space-y-4 px-8 text-center">
          <img
            src={logoLightUrl}
            alt="YANG System 标识"
            className="mx-auto size-28 dark:hidden"
          />
          <img
            src={logoDarkUrl}
            alt=""
            aria-hidden="true"
            className="mx-auto hidden size-28 dark:block"
          />
          <h1 className="text-3xl font-bold tracking-tight">YANG System</h1>
          <p className="text-foreground/70">
            统一管理个人账号、平台账号与企业组织。
          </p>
        </div>
      </section>

      <aside className="flex flex-1 items-center justify-center px-6">
        <div className="w-full max-w-sm rounded-xl border border-border bg-card p-8 shadow-sm">
          <div className="mb-6 space-y-1">
            <h2 className="text-xl font-semibold">用户登录</h2>
            <p className="text-sm text-muted-foreground">
              使用 YANG 账号进入系统
            </p>
          </div>

          <div
            role="group"
            aria-label="登录方式"
            className="mb-4 grid grid-cols-2 gap-1 rounded-lg border border-border bg-muted/40 p-1"
          >
            <button
              type="button"
              aria-pressed={mode === "password"}
              className={
                mode === "password"
                  ? "rounded-md bg-card px-3 py-1.5 text-sm font-medium shadow-sm"
                  : "rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
              }
              onClick={() => switchMode("password")}
            >
              密码登录
            </button>
            <button
              type="button"
              aria-pressed={mode === "email-code"}
              className={
                mode === "email-code"
                  ? "rounded-md bg-card px-3 py-1.5 text-sm font-medium shadow-sm"
                  : "rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
              }
              onClick={() => switchMode("email-code")}
            >
              验证码登录
            </button>
          </div>

          <form className="space-y-4" onSubmit={submit} noValidate>
            {mode === "password" ? (
              <>
                <div className="space-y-1.5">
                  <Label htmlFor="login-username">帐号</Label>
                  <div className="relative">
                    <User className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
                    <Input
                      id="login-username"
                      name="username"
                      autoComplete="username"
                      autoFocus
                      className="pl-9"
                      value={username}
                      onChange={(event) => setUsername(event.target.value)}
                    />
                  </div>
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="login-password">密码</Label>
                  <div className="relative">
                    <Lock className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
                    <Input
                      id="login-password"
                      name="password"
                      type={passwordVisible ? "text" : "password"}
                      autoComplete="current-password"
                      className="pr-9 pl-9"
                      value={password}
                      onChange={(event) => setPassword(event.target.value)}
                    />
                    <button
                      type="button"
                      aria-label={passwordVisible ? "隐藏密码" : "显示密码"}
                      className="absolute top-2.5 right-3 text-muted-foreground hover:text-foreground"
                      onClick={() => setPasswordVisible((prev) => !prev)}
                    >
                      {passwordVisible ? (
                        <EyeOff className="size-4" />
                      ) : (
                        <Eye className="size-4" />
                      )}
                    </button>
                  </div>
                </div>
              </>
            ) : (
              <>
                <div className="space-y-1.5">
                  <Label htmlFor="login-email">邮箱</Label>
                  <div className="relative">
                    <Mail className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
                    <Input
                      id="login-email"
                      name="email"
                      type="email"
                      autoComplete="email"
                      autoFocus
                      className="pl-9"
                      value={loginEmail}
                      onChange={(event) => setLoginEmail(event.target.value)}
                    />
                  </div>
                </div>

                <div className="flex items-end gap-2">
                  <div className="flex-1 space-y-1.5">
                    <Label htmlFor="login-email-code">验证码</Label>
                    <Input
                      id="login-email-code"
                      inputMode="numeric"
                      maxLength={6}
                      autoComplete="one-time-code"
                      value={emailCode}
                      onChange={(event) => setEmailCode(event.target.value)}
                    />
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={
                      sendingCode ||
                      codeCooldown > 0 ||
                      submitting ||
                      !emailLooksValid
                    }
                    onClick={() => void sendLoginEmailCode()}
                  >
                    {codeCooldown > 0
                      ? `${codeCooldown}s 后重发`
                      : sendingCode
                        ? "发送中…"
                        : codeSent
                          ? "重新发送"
                          : "发送验证码"}
                  </Button>
                </div>
              </>
            )}

            {reasonMessage && !errorMessage && (
              <p
                role="alert"
                className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
              >
                {reasonMessage}
              </p>
            )}
            {errorMessage && (
              <p
                role="alert"
                className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              >
                {errorMessage}
              </p>
            )}
            {successMessage && (
              <p
                aria-live="polite"
                className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
              >
                {successMessage}
              </p>
            )}
            {infoMessage && (
              <p
                aria-live="polite"
                className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
              >
                {infoMessage}
              </p>
            )}

            <Button type="submit" className="w-full" disabled={submitting}>
              {submitting ? "登录中…" : "登录"}
            </Button>

            <div className="flex justify-between text-sm">
              <Link
                to="/reset-password"
                className="text-muted-foreground hover:text-foreground"
              >
                使用重置凭证
              </Link>
              <Link
                to="/register"
                className="text-muted-foreground hover:text-foreground"
              >
                创建账号
              </Link>
            </div>
          </form>

          <p className="mt-6 text-center text-xs text-muted-foreground">
            YANG 生态 · 契约驱动企业应用
          </p>
        </div>
      </aside>

      <MfaChallengeDialog
        open={mfaMode !== null}
        submitting={submitting}
        errorMessage={mfaError}
        onSubmit={(code) =>
          void (mfaMode === "email-code"
            ? attemptEmailCodeLogin(code)
            : attemptLogin(code))
        }
        onCancel={() => {
          setMfaMode(null);
          setMfaError("");
        }}
        onSendEmailCode={
          mfaMode === "email-code"
            ? undefined
            : () => requestMfaEmailCode(username.trim(), password)
        }
      />
    </main>
  );
}
