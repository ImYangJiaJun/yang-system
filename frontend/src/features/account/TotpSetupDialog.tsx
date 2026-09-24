import { useEffect, useState } from "react";

import type { TotpSetupResult } from "./api";
import { copyText } from "@/shared/lib/clipboard";
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
  // 剪贴板写不进去（明文 HTTP 部署）时不再只是「不亮已复制」——那是没反应。
  const [copyFailed, setCopyFailed] = useState(false);
  const [code, setCode] = useState("");

  // 打开（setup 变更）时重置本地状态并生成二维码。
  useEffect(() => {
    setCode("");
    setCopied(false);
    setCopyFailed(false);
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
    // 降级路径见 shared/lib/clipboard.ts：明文 HTTP 部署上 navigator.clipboard 不存在。
    if ((await copyText(setup.secret)) === "copied") {
      setCopied(true);
      // 失败标记必须跟着这次结果走：不清掉的话，先失败再成功后界面上会同时挂着
      // 「已复制」和那条失败提示，两句里必有一句是假的。
      setCopyFailed(false);
      return;
    }
    // 悄悄不亮「已复制」等于没反应；密钥旁边的文字已带 `select-all`，把退路说出来。
    setCopied(false);
    setCopyFailed(true);
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
              {copyFailed ? (
                <p role="alert" className="text-xs text-destructive">
                  {
                    // 只说「这次没写成」这个已知事实，再点出最常见的原因——
                    // 断言「就是明文 HTTP 干的」在 HTTPS 上（例如权限被拒）就是假话。
                  }
                  浏览器这次没允许写剪贴板，明文 HTTP 页面最常见的原因是这个。
                  上面的密钥点一下即可全选，再按 Ctrl/Cmd + C。
                </p>
              ) : null}
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
