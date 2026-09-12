import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
} from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";

import type { CurrentUser, SessionInfo, TotpSetupResult } from "./api";
import {
  activateTotp,
  changeEmail,
  changePassword,
  changeUsername,
  deactivateTotp,
  fetchCurrentUser,
  listSessions,
  requestChangeEmail,
  revokeSession,
  setupTotp,
  uploadAvatar,
} from "./api";
import { prepareAvatarFile } from "./lib/prepare-avatar";
import { TotpSetupDialog } from "./TotpSetupDialog";
import { UserAvatar } from "./UserAvatar";
import {
  useSessionController,
  useSessionSnapshot,
} from "@/engine/session/use-session";
import { StepUpRequiredError } from "@/engine/http/errors";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

/// 账号设置页：资料展示 + 修改密码 + 修改用户名 + 停用账号。
///
/// 受 Step-up 保护的动作（改密/改用户名/停用）在 428 时经
/// SessionController.requestStepUpProof 弹重认证对话框，换到一次性
/// proof 后重放原请求；用户取消则放弃本次操作。
export default function AccountSettingsPage() {
  const controller = useSessionController();
  const session = useSessionSnapshot();
  const token = session.token || undefined;
  const navigate = useNavigate();

  const [profile, setProfile] = useState<CurrentUser | null>(null);
  const [profileError, setProfileError] = useState("");

  // 修改密码表单
  const [oldPassword, setOldPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");

  // 修改用户名表单
  const [newUsername, setNewUsername] = useState("");

  // 更换邮箱表单
  const [newEmail, setNewEmail] = useState("");
  const [emailCode, setEmailCode] = useState("");
  const [emailCodeSent, setEmailCodeSent] = useState(false);
  const [emailCooldown, setEmailCooldown] = useState(0);

  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [errorMessage, setErrorMessage] = useState("");

  // 登录设备
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [sessionsError, setSessionsError] = useState("");

  // 头像：隐藏文件输入 + 上传进行态（复用 busy="avatar"）
  const queryClient = useQueryClient();
  const avatarInputRef = useRef<HTMLInputElement | null>(null);

  // TOTP 双重验证：setup（弹窗展示二维码/密钥待验码）→ activated（一次性回显恢复码）
  const [totpSetup, setTotpSetup] = useState<TotpSetupResult | null>(null);
  const [totpError, setTotpError] = useState("");
  const [totpRecoveryCodes, setTotpRecoveryCodes] = useState<string[] | null>(
    null,
  );
  const [totpCodesCopied, setTotpCodesCopied] = useState(false);

  const loadProfile = useCallback(async () => {
    setProfileError("");
    try {
      const current = await fetchCurrentUser(token);
      setProfile(current);
      setNewUsername(current.username);
    } catch (cause) {
      setProfileError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [token]);

  useEffect(() => {
    void loadProfile();
  }, [loadProfile]);

  /// 更换头像：客户端压缩（≤256px / ≤40KiB WebP）→ 上传 → 失效 me 查询并刷新资料。
  /// avatar_version 变化会让各处 UserAvatar 按新 key 自动重取。
  const onAvatarFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // 清空 value 允许重复选择同一文件再次触发 change。
    event.target.value = "";
    if (!file || busy) return;
    setMessage("");
    setErrorMessage("");
    setBusy("avatar");
    try {
      const prepared = await prepareAvatarFile(file);
      await uploadAvatar(prepared.contentBase64, prepared.mime, token);
      await queryClient.invalidateQueries({ queryKey: ["me"] });
      await loadProfile();
      setMessage("头像已更新");
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const loadSessions = useCallback(async () => {
    setSessionsError("");
    try {
      const found = await listSessions(token);
      setSessions(found);
    } catch (cause) {
      setSessionsError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [token]);

  useEffect(() => {
    void loadSessions();
  }, [loadSessions]);

  const kickSession = async (sessionId: string) => {
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    setBusy(`kick-${sessionId}`);
    try {
      const done = await runProtected((proof) =>
        revokeSession(sessionId, token, undefined, proof),
      );
      if (done === undefined) return; // 用户取消 Step-up
      setMessage("该设备已退出登录");
      void loadSessions();
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  /// 受 Step-up 保护动作的通用执行器：请求 → 428 换 proof 重放。
  const runProtected = useCallback(
    async <T,>(
      request: (proof: string | undefined) => Promise<T>,
    ): Promise<T | undefined> => {
      try {
        return await request(undefined);
      } catch (cause) {
        if (!(cause instanceof StepUpRequiredError)) throw cause;
        const proof = await controller.requestStepUpProof(cause.challenge);
        if (!proof) return undefined; // 用户取消重认证
        return await request(proof);
      }
    },
    [controller],
  );

  const submitPassword = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    if (newPassword.length < 10) {
      setErrorMessage("新密码至少 10 个字符");
      return;
    }
    if (newPassword !== confirmPassword) {
      setErrorMessage("两次输入的新密码不一致");
      return;
    }
    setBusy("password");
    try {
      const result = await runProtected((proof) =>
        changePassword(oldPassword, newPassword, token, undefined, proof),
      );
      if (result === undefined) return; // 用户取消
      setOldPassword("");
      setNewPassword("");
      setConfirmPassword("");
      setMessage("密码已修改。凭据已变更，请使用新密码重新登录。");
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const submitUsername = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    if (!newUsername.trim()) {
      setErrorMessage("用户名不能为空");
      return;
    }
    setBusy("username");
    try {
      const result = await runProtected((proof) =>
        changeUsername(newUsername.trim(), token, undefined, proof),
      );
      if (result === undefined) return; // 用户取消
      setMessage("用户名已修改。凭据已变更，请使用新用户名重新登录。");
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const sendEmailCode = async () => {
    if (busy || emailCooldown > 0) return;
    setMessage("");
    setErrorMessage("");
    if (!newEmail.trim()) {
      setErrorMessage("请输入新邮箱");
      return;
    }
    setBusy("send-code");
    try {
      const challenge = await requestChangeEmail(newEmail.trim(), token);
      setEmailCodeSent(true);
      setEmailCooldown(challenge.resendAfter);
      setMessage("换绑验证码已发送，请在 10 分钟内输入");
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    if (emailCooldown <= 0) return;
    const timer = setInterval(() => {
      setEmailCooldown((prev) => Math.max(0, prev - 1));
    }, 1000);
    return () => clearInterval(timer);
  }, [emailCooldown > 0]);

  const submitEmail = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    if (!emailCodeSent || !emailCode.trim()) {
      setErrorMessage("请先获取换绑验证码并输入");
      return;
    }
    setBusy("email");
    try {
      const result = await runProtected((proof) =>
        changeEmail(newEmail.trim(), emailCode.trim(), token, undefined, proof),
      );
      if (result === undefined) return; // 用户取消
      setMessage("邮箱已更换。凭据已变更，请使用新邮箱重新登录。");
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const startTotpSetup = async () => {
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    setTotpError("");
    setBusy("totp-setup");
    try {
      const setup = await runProtected((proof) =>
        setupTotp(token, undefined, proof),
      );
      if (setup === undefined) return; // 用户取消 Step-up
      setTotpSetup(setup);
      setTotpRecoveryCodes(null);
      setTotpCodesCopied(false);
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  /// 弹窗内验码激活：失败保留弹窗并回传错误（弹窗自动清空输入）。
  const activateTotpWithCode = async (code: string) => {
    if (busy || !totpSetup) return;
    setTotpError("");
    setBusy("totp-activate");
    try {
      const result = await runProtected((proof) =>
        activateTotp(totpSetup.secret, code, token, undefined, proof),
      );
      if (result === undefined) return; // 用户取消 Step-up，保留弹窗
      // 激活成功：会话已失效，关闭弹窗，进入恢复码一次性回显。
      setTotpSetup(null);
      setTotpRecoveryCodes(result.recoveryCodes);
      setTotpCodesCopied(false);
    } catch (cause) {
      setTotpError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const copyTotpRecoveryCodes = async () => {
    if (!totpRecoveryCodes) return;
    try {
      await navigator.clipboard.writeText(totpRecoveryCodes.join("\n"));
      setTotpCodesCopied(true);
    } catch {
      setErrorMessage("复制失败，请手动抄录恢复码");
    }
  };

  const finishTotpActivation = () => {
    controller.clearSession("credentials-changed");
    navigate("/login", { replace: true });
  };

  /// 关闭双重验证：Step-up 重认证（已激活账号需同时出示第二因子）后停用，
  /// 全部恢复码作废、会话失效，回到登录页。
  const closeTotp = async () => {
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    if (
      !window.confirm(
        "确定关闭双重验证吗？关闭后登录与敏感操作只需密码，全部恢复码立即作废，且所有会话失效需重新登录。",
      )
    ) {
      return;
    }
    setBusy("totp-deactivate");
    try {
      const result = await runProtected((proof) =>
        deactivateTotp(token, undefined, proof),
      );
      if (result === undefined) return; // 用户取消 Step-up
      setMessage("双重验证已关闭，请重新登录。");
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const disableAccount = async () => {
    if (busy) return;
    setMessage("");
    setErrorMessage("");
    if (!profile) return;
    if (
      !window.confirm(
        `确定停用账号「${profile.username}」吗？此操作将撤销全部会话。`,
      )
    ) {
      return;
    }
    setBusy("disable");
    try {
      const disabled = await controller.disableAccount();
      if (disabled) navigate("/login", { replace: true });
    } catch (cause) {
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const emailText =
    profile?.email && profile.emailVerifiedAt
      ? `${profile.email}（已验证）`
      : "未绑定邮箱";

  return (
    <main className="mx-auto w-full max-w-2xl space-y-6 p-6">
      <div className="space-y-1">
        <h1 className="text-xl font-semibold">账号设置</h1>
        <p className="text-sm text-muted-foreground">
          管理你的登录凭据与账号安全选项
        </p>
      </div>

      {profileError && (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {profileError}
        </p>
      )}

      {message && (
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          {message}
        </p>
      )}

      {profile && (
        <section className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-base font-medium">头像</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            支持 PNG/JPEG/WebP/GIF，上传前会自动压缩到 40KiB 以内
          </p>
          <div className="mt-3 flex items-center gap-4">
            <UserAvatar
              userId={profile.id}
              avatarVersion={profile.avatarVersion}
              size={64}
              alt="当前头像"
            />
            <div className="space-y-2">
              <input
                ref={avatarInputRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif"
                className="hidden"
                aria-label="选择头像图片"
                onChange={(event) => void onAvatarFile(event)}
              />
              <Button
                type="button"
                variant="outline"
                disabled={busy !== null}
                onClick={() => avatarInputRef.current?.click()}
              >
                {busy === "avatar" ? "上传中…" : "更换头像"}
              </Button>
            </div>
          </div>
        </section>
      )}

      {profile && (
        <section className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-base font-medium">基本资料</h2>
          <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">用户名</dt>
            <dd>{profile.username}</dd>
            <dt className="text-muted-foreground">邮箱</dt>
            <dd>{emailText}</dd>
            <dt className="text-muted-foreground">注册时间</dt>
            <dd>{new Date(profile.createdAt * 1000).toLocaleString()}</dd>
          </dl>
        </section>
      )}

      <section className="rounded-xl border border-border bg-card p-5">
        <h2 className="text-base font-medium">修改密码</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          修改后全部会话将失效，需使用新密码重新登录
        </p>
        <form className="mt-3 space-y-3" onSubmit={submitPassword} noValidate>
          <div className="space-y-1.5">
            <Label htmlFor="account-old-password">当前密码</Label>
            <Input
              id="account-old-password"
              type="password"
              autoComplete="current-password"
              value={oldPassword}
              disabled={busy !== null}
              onChange={(event) => setOldPassword(event.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="account-new-password">新密码（至少 10 位）</Label>
            <Input
              id="account-new-password"
              type="password"
              autoComplete="new-password"
              value={newPassword}
              disabled={busy !== null}
              onChange={(event) => setNewPassword(event.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="account-confirm-password">确认新密码</Label>
            <Input
              id="account-confirm-password"
              type="password"
              autoComplete="new-password"
              value={confirmPassword}
              disabled={busy !== null}
              onChange={(event) => setConfirmPassword(event.target.value)}
            />
          </div>
          <Button type="submit" disabled={busy !== null}>
            {busy === "password" ? "提交中…" : "修改密码"}
          </Button>
        </form>
      </section>

      {profile && (
        <section className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-base font-medium">修改用户名</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            修改后需使用新用户名重新登录；用户名只能包含字母、数字、下划线与连字符
          </p>
          <form className="mt-3 space-y-3" onSubmit={submitUsername} noValidate>
            <div className="space-y-1.5">
              <Label htmlFor="account-new-username">新用户名</Label>
              <Input
                id="account-new-username"
                autoComplete="username"
                value={newUsername}
                disabled={busy !== null}
                onChange={(event) => setNewUsername(event.target.value)}
              />
            </div>
            <Button type="submit" disabled={busy !== null}>
              {busy === "username" ? "提交中…" : "修改用户名"}
            </Button>
          </form>
        </section>
      )}

      {profile && (
        <section className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-base font-medium">更换邮箱</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            向新邮箱发送一次性验证码（与注册验证码独立），验证通过后完成换绑并重新登录
          </p>
          <form className="mt-3 space-y-3" onSubmit={submitEmail} noValidate>
            <div className="space-y-1.5">
              <Label htmlFor="account-new-email">新邮箱</Label>
              <Input
                id="account-new-email"
                type="email"
                autoComplete="email"
                value={newEmail}
                disabled={busy !== null}
                onChange={(event) => setNewEmail(event.target.value)}
              />
            </div>
            <div className="flex items-end gap-2">
              <div className="flex-1 space-y-1.5">
                <Label htmlFor="account-email-code">验证码</Label>
                <Input
                  id="account-email-code"
                  inputMode="numeric"
                  maxLength={6}
                  value={emailCode}
                  disabled={busy !== null}
                  onChange={(event) => setEmailCode(event.target.value)}
                />
              </div>
              <Button
                type="button"
                variant="outline"
                disabled={busy !== null || emailCooldown > 0}
                onClick={() => void sendEmailCode()}
              >
                {emailCooldown > 0
                  ? `${emailCooldown}s 后重发`
                  : emailCodeSent
                    ? "重新发送"
                    : "发送验证码"}
              </Button>
            </div>
            <Button type="submit" disabled={busy !== null}>
              {busy === "email" ? "提交中…" : "更换邮箱"}
            </Button>
          </form>
        </section>
      )}

      {profile && (
        <section className="rounded-xl border border-border bg-card p-5">
          <h2 className="text-base font-medium">双重验证（TOTP）</h2>
          {totpRecoveryCodes ? (
            <div className="mt-3 space-y-3">
              <p className="text-sm text-muted-foreground">
                双重验证已启用。以下恢复码<strong>仅此一次显示</strong>
                ，请立即抄录并妥善保管；认证器不可用时可用恢复码登录（每码一次性）。
              </p>
              <ul className="grid grid-cols-2 gap-2 rounded-md border border-border bg-muted/50 p-3 font-mono text-sm">
                {totpRecoveryCodes.map((code) => (
                  <li key={code}>{code}</li>
                ))}
              </ul>
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => void copyTotpRecoveryCodes()}
                >
                  {totpCodesCopied ? "已复制" : "复制恢复码"}
                </Button>
                <Button type="button" onClick={finishTotpActivation}>
                  我已保存恢复码，重新登录
                </Button>
              </div>
            </div>
          ) : profile.totpActivated ? (
            <div className="mt-3 space-y-3">
              <p className="text-sm">
                <span className="rounded bg-primary/10 px-1.5 py-0.5 text-xs text-primary">
                  已启用
                </span>{" "}
                登录与敏感操作需输入认证器动态码、恢复码或邮箱验证码。如需更换认证器，
                可先关闭后重新启用。
              </p>
              <Button
                type="button"
                variant="outline"
                disabled={busy !== null}
                onClick={() => void closeTotp()}
              >
                {busy === "totp-deactivate" ? "关闭中…" : "关闭双重验证"}
              </Button>
            </div>
          ) : (
            <div className="mt-3 space-y-3">
              <p className="text-sm text-muted-foreground">
                启用后，登录与敏感操作除密码外还需输入认证器动态码，可显著提升账号安全性。
              </p>
              <Button
                type="button"
                variant="outline"
                disabled={busy !== null}
                onClick={() => void startTotpSetup()}
              >
                {busy === "totp-setup" ? "生成密钥中…" : "启用双重验证"}
              </Button>
            </div>
          )}
        </section>
      )}

      <section className="rounded-xl border border-border bg-card p-5">
        <div className="flex items-center justify-between">
          <h2 className="text-base font-medium">登录设备</h2>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={busy !== null}
            onClick={() => void loadSessions()}
          >
            刷新
          </Button>
        </div>
        <p className="mt-1 text-sm text-muted-foreground">
          当前会话所在的设备列表；可逐台退出其他设备
        </p>
        {sessionsError && (
          <p role="alert" className="mt-2 text-sm text-destructive">
            {sessionsError}
          </p>
        )}
        <ul className="mt-3 space-y-2">
          {sessions.map((session) => (
            <li
              key={session.sessionId}
              className="flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2 text-sm"
            >
              <div className="min-w-0">
                <p className="truncate font-medium">
                  {session.userAgent || "未知设备"}
                  {session.current && (
                    <span className="ml-2 rounded bg-primary/10 px-1.5 py-0.5 text-xs text-primary">
                      当前设备
                    </span>
                  )}
                </p>
                <p className="text-xs text-muted-foreground">
                  {session.ip} · 最近活动{" "}
                  {new Date(session.lastSeenAt * 1000).toLocaleString()}
                </p>
              </div>
              {!session.current && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={busy !== null}
                  onClick={() => void kickSession(session.sessionId)}
                >
                  {busy === `kick-${session.sessionId}` ? "退出中…" : "退出"}
                </Button>
              )}
            </li>
          ))}
          {sessions.length === 0 && !sessionsError && (
            <li className="text-sm text-muted-foreground">暂无活跃会话</li>
          )}
        </ul>
      </section>

      <section className="rounded-xl border border-destructive/40 bg-destructive/5 p-5">
        <h2 className="text-base font-medium text-destructive">危险区</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          停用当前账号将撤销全部会话，账号将无法登录。此操作不可自助恢复。
        </p>
        <Button
          variant="destructive"
          className="mt-3"
          disabled={busy !== null}
          onClick={() => void disableAccount()}
        >
          {busy === "disable" ? "停用中…" : "停用账号"}
        </Button>
      </section>

      {errorMessage && (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {errorMessage}
        </p>
      )}

      <TotpSetupDialog
        setup={totpSetup}
        submitting={busy === "totp-activate"}
        errorMessage={totpError}
        onActivate={(code) => void activateTotpWithCode(code)}
        onCancel={() => {
          setTotpSetup(null);
          setTotpError("");
        }}
      />
    </main>
  );
}
