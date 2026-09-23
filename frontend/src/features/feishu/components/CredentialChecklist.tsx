/**
 * 凭据拷贝清单：一行一个字段绑定，**两个可复制项**（URL 与 Token）+ 一个轮换按钮。
 *
 * # 为什么是两个不是三个
 *
 * 审批控件上还有第三格 `Key`，但本次**不做 Key 加密**（设计决策 D11），
 * 所以控件里那一格留空、我们这边也没有对应的值可给。
 *
 * # 「复制」与「轮换」的分工（决策 D10）
 *
 * - **复制是纯读**：URL 是本地拼的，一个请求都不发；Token 走**回显端点**取当前值，
 *   不重新生成、不改任何状态。同一个 Token 可以一直用、反复复制。
 * - **轮换是写**：单独一个按钮，必须二次确认，且确认框里写清后果——
 *   已配置该字段的控件会**立即失效**，得把新 Token 粘回审批后台。
 *
 * 两者放在同一页是刻意的（运维就是在这页上拷贝），所以轮换按钮**不挨着**复制按钮：
 * 它在独立的一列里，带自己的说明文字，且真正的闸门是那个确认框。
 *
 * # Token 明文为什么不常驻
 *
 * 清单是常驻的一页，而凭据是 `secret` 级的东西。明文只在两条路径上出现：
 * 用户点「复制 Token」（回显一次）与轮换成功后（那一次响应就是新值）。
 * 两者都进本组件的本地状态，刷新页面即消失。
 */

import { Check, Copy } from "lucide-react";
import { useState } from "react";

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import type { CredentialClient } from "../api";
import type { CredentialItem } from "../types";
import { approvalOptionsUrl, formatUnixSeconds } from "../types";

/// 轮换确认框的正文。**逐字**是设计 §10.3 的口径：后果写在正文里，不藏在 tooltip。
const ROTATE_MESSAGE =
  "轮换后，已配置该字段的控件会立即失效，需要把新的 Token 粘回审批后台。";

export type CredentialChecklistProps = {
  items: ReadonlyArray<CredentialItem>;
  /// 取选项接口的地址前缀。默认当前站点——飞书要访问的是同一个域名。
  origin?: string;
  /// 凭据入口。**不给就是「这个部署没有回显与轮换的权限」**：
  /// 那时清单照旧显示 URL（那是纯本地拼接），只是 Token 与轮换不可用。
  client?: CredentialClient;
  onRotated?: (sourceKey: string, token: string) => void;
};

export function CredentialChecklist({
  items,
  origin,
  client,
  onRotated,
}: CredentialChecklistProps) {
  const base = origin ?? window.location.origin;
  /// 回显/轮换拿到的明文，按 `source_key` 存。刷新即失。
  const [tokens, setTokens] = useState<Record<string, string>>({});
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rotating, setRotating] = useState<CredentialItem | null>(null);
  const [pending, setPending] = useState(false);

  async function copy(sourceKey: string, value: string) {
    try {
      await navigator.clipboard.writeText(value);
      setCopiedKey(sourceKey);
    } catch {
      // 剪贴板不可用（无权限 / 非安全上下文）：什么都不做，也**不谎报已复制**
      // ——用户会去别处粘一个空的，比不响应更糟。
    }
  }

  /// 复制 URL：值是在本地拼出来的，所以一个请求都不发。
  async function copyUrl(item: CredentialItem) {
    setError(null);
    setCopiedKey(null);
    await copy(
      `${item.sourceKey}#url`,
      approvalOptionsUrl(base, item.sourceKey),
    );
  }

  /// 复制 Token：**只回显**（读），没有值就取一次。绝不触发轮换。
  async function copyToken(item: CredentialItem) {
    setError(null);
    setCopiedKey(null);
    const existing = tokens[item.sourceKey];
    if (existing !== undefined) {
      await copy(`${item.sourceKey}#token`, existing);
      return;
    }
    if (client === undefined) {
      setError("当前身份没有回显凭据的权限，取不到这个 Token。");
      return;
    }
    try {
      const token = await client.reveal(item.sourceKey);
      setTokens((previous) => ({ ...previous, [item.sourceKey]: token }));
      await copy(`${item.sourceKey}#token`, token);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function confirmRotate() {
    if (rotating === null || client === undefined) return;
    setPending(true);
    setError(null);
    try {
      const token = await client.rotate(rotating.sourceKey);
      setTokens((previous) => ({ ...previous, [rotating.sourceKey]: token }));
      onRotated?.(rotating.sourceKey, token);
      setRotating(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
      setRotating(null);
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="space-y-3">
      <p className="text-xs text-muted-foreground">
        一行一个字段：每行两项可复制——接口地址与 Token。复制不改变任何状态，
        误点没有后果；要换 Token 走右边的「轮换」。 审批控件里还有一格
        Key，本次不做 Key 加密，那一格留空。
      </p>

      {client === undefined ? (
        <p className="text-xs text-muted-foreground">
          当前身份没有回显与轮换凭据的权限：地址照旧可复制，Token
          与轮换这一栏用不了。
        </p>
      ) : null}

      {error === null ? null : (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {error}
        </p>
      )}

      <div className="overflow-x-auto">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>字段</TableHead>
              <TableHead>接口地址（粘到外部选项配置里）</TableHead>
              <TableHead className="text-right">复制 / 轮换</TableHead>
              <TableHead>最近轮换</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((item) => {
              const url = approvalOptionsUrl(base, item.sourceKey);
              return (
                <TableRow key={item.sourceKey} data-slot="credential-row">
                  <TableCell className="align-top">
                    <span className="block">
                      {item.fieldName ?? "（字段名还没解析出来）"}
                    </span>
                    <span className="block font-mono text-xs text-muted-foreground">
                      {item.sourceKey}
                    </span>
                  </TableCell>
                  <TableCell className="align-top">
                    <code
                      className="block max-w-[28rem] truncate font-mono text-xs"
                      title={url}
                    >
                      {url}
                    </code>
                  </TableCell>
                  <TableCell className="align-top">
                    <div className="flex items-start justify-end gap-2">
                      <div className="flex flex-col gap-1">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label="复制 URL"
                          onClick={() => void copyUrl(item)}
                        >
                          {copiedKey === `${item.sourceKey}#url` ? (
                            <Check aria-hidden="true" />
                          ) : (
                            <Copy aria-hidden="true" />
                          )}
                          复制 URL
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label="复制 Token"
                          onClick={() => void copyToken(item)}
                        >
                          {copiedKey === `${item.sourceKey}#token` ? (
                            <Check aria-hidden="true" />
                          ) : (
                            <Copy aria-hidden="true" />
                          )}
                          复制 Token
                        </Button>
                      </div>
                      {/* 轮换按钮**独立成一栏**，不挨着复制按钮：它是写操作，
                          点错会作废已经配好的控件。真正的闸门是确认框。 */}
                      <div className="flex flex-col gap-1 border-l border-border pl-3">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setError(null);
                            setRotating(item);
                          }}
                        >
                          轮换
                        </Button>
                      </div>
                    </div>
                  </TableCell>
                  <TableCell className="align-top text-xs text-muted-foreground">
                    {/* 看得出这个字段的凭据是刚换的还是早就配好的。
                        「拿不到」（列表端点不返回这一列）与「从未轮换」是两件事，
                        前者画成后者就是一句可查证的假话。 */}
                    {item.tokenRotatedAt === undefined
                      ? "—"
                      : item.tokenRotatedAt === null
                        ? "从未轮换"
                        : formatUnixSeconds(item.tokenRotatedAt)}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>

      <Dialog
        open={rotating !== null}
        onOpenChange={(next) => {
          if (!next && !pending) setRotating(null);
        }}
      >
        <DialogContent showCloseButton={!pending}>
          <DialogHeader>
            <DialogTitle>轮换 Token</DialogTitle>
            <DialogDescription>
              <span className="block">{ROTATE_MESSAGE}</span>
              {rotating === null ? null : (
                <span className="mt-2 block font-mono text-xs">
                  {rotating.sourceKey}
                </span>
              )}
              {client === undefined ? (
                <span className="mt-2 block text-destructive">
                  当前身份没有轮换凭据的权限。
                </span>
              ) : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={pending}
              onClick={() => setRotating(null)}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={pending || client === undefined}
              onClick={() => void confirmRotate()}
            >
              {pending ? "轮换中…" : "轮换 Token"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
