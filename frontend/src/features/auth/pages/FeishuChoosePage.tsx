import { useEffect, useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";

import { useSessionController } from "@/engine/session/use-session";
import { SecondFactorRequiredError } from "@/engine/http/errors";
import {
  bindFeishuChoose,
  createFeishuChoose,
  fetchFeishuChooseProfile,
  requestMfaEmailCode,
  type FeishuAuthResult,
  type FeishuChooseProfile,
} from "@/features/auth/api";
import { safeRedirectTarget } from "@/features/auth/redirect";
import { MfaChallengeDialog } from "@/features/auth/components/MfaChallengeDialog";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import { Skeleton } from "@/shared/ui/skeleton";

/// 首次飞书登录的账号选择页（公开路由 `/login/feishu/choose`）。
///
/// 页面只读展示飞书身份；两个写操作都必须带 URL 上的 pending token + 后端种下的
/// nonce Cookie。token 失效时后端拒绝，前端只负责把「重新点飞书登录」说清楚。
type ChoiceMode = "bind" | "create";
type Submitting = "bind" | "bind-mfa" | "create" | null;

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : "操作失败，请稍后重试";
}

export default function FeishuChoosePage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const token = searchParams.get("token") ?? "";
  const redirectParam = safeRedirectTarget(searchParams.get("redirect"));

  const [profile, setProfile] = useState<FeishuChooseProfile | null>(null);
  const [profileError, setProfileError] = useState("");
  const [loading, setLoading] = useState(Boolean(token));

  const [mode, setMode] = useState<ChoiceMode>("bind");
  const [bindUsername, setBindUsername] = useState("");
  const [bindPassword, setBindPassword] = useState("");
  const [createUsername, setCreateUsername] = useState("");
  const [createPassword, setCreatePassword] = useState("");
  const [createConfirm, setCreateConfirm] = useState("");
  const [submitting, setSubmitting] = useState<Submitting>(null);
  const [errorMessage, setErrorMessage] = useState("");

  const [mfaOpen, setMfaOpen] = useState(false);
  const [mfaError, setMfaError] = useState("");
  // 绑定重试必须复用同一组凭据：MFA 对话框只提供验证码。
  const [pendingBind, setPendingBind] = useState<{
    username: string;
    password: string;
  } | null>(null);

  useEffect(() => {
    if (!token) return;
    const abort = new AbortController();
    setLoading(true);
    setProfileError("");
    void fetchFeishuChooseProfile(token, abort.signal)
      .then((next) => {
        if (abort.signal.aborted) return;
        setProfile(next);
        // allow_create=false 时页面根本没有第二个选项，默认停在绑定。
        if (!next.allowCreate) setMode("bind");
      })
      .catch((cause: unknown) => {
        if (abort.signal.aborted) return;
        setProfileError(messageOf(cause));
      })
      .finally(() => {
        if (!abort.signal.aborted) setLoading(false);
      });
    return () => abort.abort();
  }, [token]);

  const finish = (result: FeishuAuthResult) => {
    controller.beginSession({ accessToken: result.accessToken });
    // 选择页 URL 不带 redirect（后端把它存进 pending token）；优先用响应/选择页
    // 数据里的值，URL 参数只作旧链接兼容的兜底。
    const target = safeRedirectTarget(
      result.redirect ?? profile?.redirect ?? redirectParam,
    );
    navigate(target, { replace: true });
  };

  const submitBind = async (mfaCode?: string) => {
    if (submitting) return;
    const username = (pendingBind?.username ?? bindUsername).trim();
    const password = pendingBind?.password ?? bindPassword;
    if (!username) {
      setErrorMessage("请输入用户名或已验证邮箱");
      return;
    }
    if (!password) {
      setErrorMessage("请输入密码");
      return;
    }
    setErrorMessage("");
    setMfaError("");
    setSubmitting(mfaCode ? "bind-mfa" : "bind");
    try {
      const result = await bindFeishuChoose({
        token,
        username,
        password,
        ...(mfaCode ? { mfaCode } : {}),
      });
      finish(result);
    } catch (cause) {
      if (cause instanceof SecondFactorRequiredError) {
        setPendingBind({ username, password });
        setMfaOpen(true);
      } else if (mfaCode) {
        setMfaError(messageOf(cause));
      } else {
        // 后端对「账号不存在 / 密码错 / 停用」使用同构响应；不要在此追加任何
        // 「用户不存在」之类的前端推断。
        setErrorMessage(messageOf(cause));
      }
    } finally {
      setSubmitting(null);
    }
  };

  const submitCreate = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    const username = createUsername.trim();
    if (username.length < 3 || username.length > 64) {
      setErrorMessage("用户名长度必须为 3 到 64 个字符");
      return;
    }
    if (createPassword.length < 10) {
      setErrorMessage("密码至少 10 个字符");
      return;
    }
    if (createPassword !== createConfirm) {
      setErrorMessage("两次输入的密码不一致");
      return;
    }
    if (
      !window.confirm(
        `将创建系统账号「${username}」并绑定当前飞书身份，创建后可用用户名和密码登录。确认继续？`,
      )
    ) {
      return;
    }
    setErrorMessage("");
    setSubmitting("create");
    try {
      const result = await createFeishuChoose({
        token,
        username,
        password: createPassword,
      });
      finish(result);
    } catch (cause) {
      setErrorMessage(messageOf(cause));
    } finally {
      setSubmitting(null);
    }
  };

  if (!token) {
    return (
      <main className="flex min-h-svh items-center justify-center bg-muted/40 px-4 py-8">
        <div className="w-full max-w-md rounded-xl border border-border bg-card p-8 shadow-sm">
          <h1 className="text-xl font-semibold">飞书登录</h1>
          <p role="alert" className="mt-3 text-sm text-destructive">
            登录链接不完整或已过期，请重新点「飞书登录」。
          </p>
          <Button className="mt-4 w-full" asChild>
            <Link to="/login">返回登录</Link>
          </Button>
        </div>
      </main>
    );
  }

  return (
    <main className="flex min-h-svh items-center justify-center bg-muted/40 px-4 py-8">
      <div className="w-full max-w-lg rounded-xl border border-border bg-card p-8 shadow-sm">
        <div className="mb-6 space-y-1">
          <h1 className="text-xl font-semibold">选择账号方式</h1>
          <p className="text-sm text-muted-foreground">
            这是你第一次用飞书登录本系统，请绑定已有账号或新建账号。
          </p>
        </div>

        {loading ? (
          <div className="space-y-3" role="status" aria-label="飞书身份加载中">
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : profileError ? (
          <div className="space-y-4">
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {profileError}
            </p>
            <p className="text-sm text-muted-foreground">
              登录链接可能已过期。请重新点「飞书登录」；如果反复失败，请联系管理员。
            </p>
            <Button className="w-full" asChild>
              <Link to="/login">重新登录</Link>
            </Button>
          </div>
        ) : profile ? (
          <>
            <section
              aria-label="飞书身份"
              className="mb-5 rounded-lg border border-border bg-muted/40 p-4 text-sm"
            >
              <p>
                <span className="text-muted-foreground">飞书账号：</span>
                <span className="font-medium">{profile.name}</span>
              </p>
              <p className="mt-1">
                <span className="text-muted-foreground">邮箱：</span>
                {profile.maskedEmail ?? "未提供"}
              </p>
              <p className="mt-1 text-muted-foreground">
                {profile.emailMatched
                  ? "检测到该邮箱已对应系统账号，建议绑定已有账号。"
                  : "未检测到与该邮箱对应的系统账号，也可选择新建账号。"}
              </p>
            </section>

            <div
              role="group"
              aria-label="账号方式"
              className="mb-4 grid grid-cols-2 gap-1 rounded-lg border border-border bg-muted/40 p-1"
            >
              <button
                type="button"
                aria-pressed={mode === "bind"}
                className={
                  mode === "bind"
                    ? "rounded-md bg-card px-3 py-1.5 text-sm font-medium shadow-sm"
                    : "rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
                }
                onClick={() => {
                  setMode("bind");
                  setErrorMessage("");
                }}
              >
                绑定已有账号
              </button>
              {profile.allowCreate && (
                <button
                  type="button"
                  aria-pressed={mode === "create"}
                  className={
                    mode === "create"
                      ? "rounded-md bg-card px-3 py-1.5 text-sm font-medium shadow-sm"
                      : "rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
                  }
                  onClick={() => {
                    setMode("create");
                    setErrorMessage("");
                  }}
                >
                  新建账号
                </button>
              )}
            </div>

            {!profile.allowCreate && (
              <p className="mb-4 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
                当前部署不允许自助新建账号，请绑定已有账号；如需新建请联系管理员。
              </p>
            )}

            {mode === "bind" ? (
              <form
                className="space-y-4"
                noValidate
                onSubmit={(event) => {
                  event.preventDefault();
                  void submitBind();
                }}
              >
                <div className="space-y-1.5">
                  <Label htmlFor="feishu-bind-username">
                    用户名或已验证邮箱
                  </Label>
                  <Input
                    id="feishu-bind-username"
                    autoComplete="username"
                    autoFocus
                    value={bindUsername}
                    disabled={submitting !== null}
                    onChange={(event) => setBindUsername(event.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="feishu-bind-password">密码</Label>
                  <Input
                    id="feishu-bind-password"
                    type="password"
                    autoComplete="current-password"
                    value={bindPassword}
                    disabled={submitting !== null}
                    onChange={(event) => setBindPassword(event.target.value)}
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

                <Button
                  type="submit"
                  className="w-full"
                  disabled={submitting !== null}
                >
                  {submitting === "bind" ? "验证中…" : "绑定并登录"}
                </Button>
              </form>
            ) : (
              <form className="space-y-4" noValidate onSubmit={submitCreate}>
                <div className="space-y-1.5">
                  <Label htmlFor="feishu-create-username">用户名</Label>
                  <Input
                    id="feishu-create-username"
                    autoComplete="username"
                    autoFocus
                    value={createUsername}
                    disabled={submitting !== null}
                    onChange={(event) => setCreateUsername(event.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="feishu-create-password">密码</Label>
                  <Input
                    id="feishu-create-password"
                    type="password"
                    autoComplete="new-password"
                    value={createPassword}
                    disabled={submitting !== null}
                    onChange={(event) => setCreatePassword(event.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="feishu-create-confirm">确认密码</Label>
                  <Input
                    id="feishu-create-confirm"
                    type="password"
                    autoComplete="new-password"
                    value={createConfirm}
                    disabled={submitting !== null}
                    onChange={(event) => setCreateConfirm(event.target.value)}
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

                <Button
                  type="submit"
                  className="w-full"
                  disabled={submitting !== null}
                >
                  {submitting === "create" ? "创建中…" : "新建并登录"}
                </Button>
                <p className="text-xs text-muted-foreground">
                  新建账号只获得基础登录权限，不会成为系统管理员；飞书邮箱不会写入
                  系统邮箱字段。
                </p>
              </form>
            )}
          </>
        ) : null}

        <p className="mt-6 text-center text-sm">
          <Link
            to="/login"
            className="text-muted-foreground hover:text-foreground"
          >
            返回登录
          </Link>
        </p>
      </div>

      <MfaChallengeDialog
        open={mfaOpen}
        submitting={submitting === "bind-mfa"}
        errorMessage={mfaError}
        onSubmit={(code) => void submitBind(code)}
        onCancel={() => {
          setMfaOpen(false);
          setMfaError("");
          setPendingBind(null);
        }}
        // 与密码登录一致：认证器不可用时可向已验证邮箱发一次性验证码。
        // 请求复用现有端点（需要账号标识 + 密码，正好是绑定表单里的凭据）。
        onSendEmailCode={
          pendingBind
            ? () =>
                requestMfaEmailCode(pendingBind.username, pendingBind.password)
            : undefined
        }
      />
    </main>
  );
}
