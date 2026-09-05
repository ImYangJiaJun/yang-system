import { useState, type FormEvent } from "react";

import { completeStepUp } from "@/engine/session/step-up";
import type { SessionContext } from "@/engine/http/types";
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

export interface StepUpRequest {
  challenge: string;
  session: SessionContext;
}

/// 敏感操作重新认证对话框（对齐旧 StepUpDialog.vue）：
/// challenge + 账号密码 → step-up/complete 换一次性 proof；凭据与 proof 均不落存储。
export function StepUpDialog({
  request,
  onResolve,
}: {
  request: StepUpRequest | null;
  onResolve: (proof: string | undefined) => void;
}) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");

  const close = (proof: string | undefined) => {
    setUsername("");
    setPassword("");
    setErrorMessage("");
    onResolve(proof);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!request || loading || !username.trim() || !password) return;
    setLoading(true);
    setErrorMessage("");
    try {
      const result = await completeStepUp(
        request.challenge,
        { username: username.trim(), password },
        request.session,
      );
      close(result.proof);
    } catch (cause) {
      setPassword("");
      setErrorMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Dialog
      open={Boolean(request)}
      onOpenChange={(open) => {
        if (!open && !loading) close(undefined);
      }}
    >
      <DialogContent
        aria-label="敏感操作重新认证"
        onInteractOutside={(event) => event.preventDefault()}
      >
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>敏感操作重新认证</DialogTitle>
            <DialogDescription>
              请重新输入账号密码。凭据与本次 proof 不会保存在浏览器存储中。
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-1.5">
              <Label htmlFor="step-up-username">用户名</Label>
              <Input
                id="step-up-username"
                autoComplete="username"
                disabled={loading}
                value={username}
                onChange={(event) => setUsername(event.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="step-up-password">密码</Label>
              <Input
                id="step-up-password"
                type="password"
                autoComplete="current-password"
                disabled={loading}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
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
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              disabled={loading}
              onClick={() => close(undefined)}
            >
              取消
            </Button>
            <Button
              type="submit"
              disabled={loading || !username.trim() || !password}
            >
              {loading ? "验证中…" : "验证并继续"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
