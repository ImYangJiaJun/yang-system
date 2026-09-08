import { useEffect, useState } from "react";

import type { TotpSetupResult } from "./api";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

/// qrcode 是 CJS 包：构建产物只有 default 导出（rolldown 互操作），
/// dev/vitest 下可能是命名导出，两种形态都兼容。
type QrCodeModule = {
  toDataURL?: (
    text: string,
    options: { width: number; margin: number },
  ) => Promise<string>;
};

async function renderQrDataUrl(text: string): Promise<string> {
  const mod = (await import("qrcode")) as unknown as QrCodeModule & {
    default?: QrCodeModule;
  };
  const toDataURL = mod.toDataURL ?? mod.default?.toDataURL;
  if (!toDataURL) throw new Error("qrcode.toDataURL 不可用");
  return toDataURL(text, { width: 192, margin: 1 });
}

/// TOTP 设置弹窗：扫码/复制密钥 → 输入认证器动态码（满位自动提交）完成激活。
/// 验证失败由父级回传 errorMessage，弹窗自动清空输入等待重试。
/// qrcode 库按需动态加载，不进入首屏 bundle。
export function TotpSetupDialog({
  setup,
  submitting,
  errorMessage,
  onActivate,
  onCancel,
}: {
  setup: TotpSetupResult | null;
  submitting: boolean;
  errorMessage: string;
  onActivate: (code: string) => void;
  onCancel: () => void;
}) {
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [qrFailed, setQrFailed] = useState(false);
  const [copied, setCopied] = useState(false);
  const [code, setCode] = useState("");

  // 打开（setup 变更）时重置本地状态并生成二维码。
  useEffect(() => {
    setCode("");
    setCopied(false);
    setQrDataUrl(null);
    setQrFailed(false);
    if (!setup) return;
    let cancelled = false;
    renderQrDataUrl(setup.otpauthUri)
      .then((url) => {
        if (!cancelled) setQrDataUrl(url);
      })
      .catch(() => {
        if (!cancelled) setQrFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [setup]);

  // 父级回传错误时清空输入，便于立即重试。
  useEffect(() => {
    if (errorMessage) setCode("");
  }, [errorMessage]);

  const copySecret = async () => {
    if (!setup) return;
    try {
      await navigator.clipboard.writeText(setup.secret);
      setCopied(true);
    } catch {
      // 剪贴板不可用（非安全上下文等）：密钥文本本身可手动选中复制。
      setCopied(false);
    }
  };

  const digits = setup?.digits ?? 6;

  const changeCode = (raw: string) => {
    const next = raw.replace(/\D/g, "").slice(0, digits);
    setCode(next);
    if (next.length === digits && !submitting) onActivate(next);
  };

  return (
    <Dialog
      open={Boolean(setup)}
      onOpenChange={(open) => {
        if (!open && !submitting) onCancel();
      }}
    >
      <DialogContent
        aria-label="启用双重验证"
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>启用双重验证</DialogTitle>
          <DialogDescription>
            用认证器应用（如 Microsoft Authenticator、Google
            Authenticator）扫描二维码，或手动输入密钥。
          </DialogDescription>
        </DialogHeader>
        {setup && (
          <div className="space-y-4 py-2">
            <div className="flex justify-center">
              {qrFailed ? (
                <p className="max-w-full rounded-md border border-border bg-muted/50 px-3 py-2 font-mono text-xs break-all select-all">
                  {setup.otpauthUri}
                </p>
              ) : qrDataUrl ? (
                <img
                  src={qrDataUrl}
                  alt="TOTP 二维码"
                  className="rounded-md border border-border"
                />
              ) : (
                <div
                  className="size-48 animate-pulse rounded-md bg-muted"
                  aria-label="二维码加载中"
                />
              )}
            </div>

            <div className="space-y-1.5">
              <Label>密钥</Label>
              <div className="flex items-center gap-2">
                <code className="flex-1 rounded-md border border-border bg-muted/50 px-3 py-2 font-mono text-sm break-all select-all">
                  {setup.secret}
                </code>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => void copySecret()}
                >
                  {copied ? "已复制" : "复制"}
                </Button>
              </div>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="totp-setup-code">
                认证器验证码（{digits} 位数字）
              </Label>
              <Input
                id="totp-setup-code"
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={digits}
                autoFocus
                disabled={submitting}
                value={code}
                onChange={(event) => changeCode(event.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                输入满 {digits} 位自动验证并启用
              </p>
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
        )}
      </DialogContent>
    </Dialog>
  );
}
