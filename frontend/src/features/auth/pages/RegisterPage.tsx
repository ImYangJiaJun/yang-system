import { useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router";

import { register } from "@/features/auth/api";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

/// 创建账号：用户名加密码即可。
export default function RegisterPage() {
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [passwordVisible, setPasswordVisible] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setErrorMessage("");
    if (username.trim().length < 3 || username.trim().length > 64) {
      setErrorMessage("帐号长度必须为 3 到 64 个字符");
      return;
    }
    if (password.length < 10) {
      setErrorMessage("密码至少 10 个字符");
      return;
    }
    if (password !== confirmPassword) {
      setErrorMessage("两次输入的密码不一致");
      return;
    }
    setSubmitting(true);
    try {
      await register(username.trim(), password);
      navigate("/login?registered=1", { replace: true });
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "注册失败，请稍后重试",
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <main className="flex min-h-svh items-center justify-center bg-muted/40 px-4 py-8">
      <div className="w-full max-w-lg rounded-xl border border-border bg-card p-8 shadow-sm">
        <div className="mb-6 space-y-1">
          <h1 className="text-xl font-semibold">创建账号</h1>
          <p className="text-sm text-muted-foreground">
            使用用户名和密码创建账号。
          </p>
        </div>
        <form className="space-y-4" onSubmit={submit} noValidate>
          <div className="space-y-1.5">
            <Label htmlFor="register-username">帐号</Label>
            <Input
              id="register-username"
              autoComplete="username"
              autoFocus
              value={username}
              onChange={(event) => setUsername(event.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="register-password">密码</Label>
            <div className="relative">
              <Input
                id="register-password"
                type={passwordVisible ? "text" : "password"}
                autoComplete="new-password"
                className="pr-16"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
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
            <Label htmlFor="register-confirm-password">确认密码</Label>
            <Input
              id="register-confirm-password"
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
            {submitting ? "创建中…" : "创建账号"}
          </Button>
          <Button variant="ghost" className="w-full" asChild>
            <Link to="/login">返回登录</Link>
          </Button>
        </form>
      </div>
    </main>
  );
}
