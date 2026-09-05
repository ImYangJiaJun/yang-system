import { useState, type FormEvent } from "react";
import { Eye, EyeOff, Lock, User } from "lucide-react";
import { useNavigate, useSearchParams, Link } from "react-router";

import { login } from "@/engine/session/auth";
import {
  useSessionController,
  useSessionSnapshot,
} from "@/engine/session/use-session";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

/// 登录页（对齐旧 LoginPage.vue 语义）：品牌面板 + 凭据表单 + 错误/提示横幅。
export default function LoginPage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const snapshot = useSessionSnapshot();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [passwordVisible, setPasswordVisible] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");
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

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    if (!username.trim()) {
      setErrorMessage("请输入帐号");
      return;
    }
    if (!password) {
      setErrorMessage("请输入密码");
      return;
    }
    setErrorMessage("");
    setSubmitting(true);
    try {
      const result = await login(username.trim(), password);
      controller.beginSession(result);
      navigate("/", { replace: true });
    } catch (cause) {
      // 后端错误（401/错误码 envelope）已由 api/auth 映射为 ApiError.message
      setErrorMessage(
        cause instanceof Error ? cause.message : "登录失败，请稍后重试",
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <main className="flex min-h-svh bg-background text-foreground">
      <section className="hidden flex-1 items-center justify-center bg-primary/5 md:flex">
        <div className="max-w-sm space-y-4 px-8 text-center">
          <div className="mx-auto flex size-20 items-center justify-center rounded-2xl bg-primary text-3xl font-bold text-primary-foreground">
            Y
          </div>
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

          <form className="space-y-4" onSubmit={submit} noValidate>
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
    </main>
  );
}
