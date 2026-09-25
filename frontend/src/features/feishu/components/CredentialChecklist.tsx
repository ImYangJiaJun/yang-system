/**
 * 凭据拷贝清单：**一块一个字段绑定**，两种情况下三项可复制。
 *
 * # 为什么不是表格（2026-09-25 改）
 *
 * 它原来是四列表格：字段 / 接口地址 / 复制・轮换 / 最近轮换。在详情页 896px 的内容列里
 * 那张表**撑出横向滚动条**，「最近轮换」整列被切在卡片外，而接口地址被截断成
 * `…/options/expense_categ…`（全文只能靠悬停 `title` 看）。
 *
 * 这一栏是整个控制台唯一的产出物——运维要把它抄进飞书审批后台。主数据被截断、
 * 按钮被推出视野，等于把交付环节做成了猜谜。所以改成块：地址整行完整显示（`break-all`
 * 换行而不截断），复制按钮与它同排。轮换与复制的距离由布局拉开，不再只靠一条分隔线。
 *
 * # 三项还是两项：取决于有没有父字段
 *
 * - **接口地址**：本地拼的，一个请求都不发。
 * - **Token**：走**回显端点**取当前值，只读、不重新生成。
 * - **联动 key**（只有带父字段的绑定有）：父绑定的 `field_id`，运维要把它填进审批后台
 *   那个联动控件的「参数代码」。它是**级联精确匹配**的唯一依据，见
 *   [`CredentialParent`](types.ts) 的说明；服务端从不比较审批表单里的 widget 代码。
 *
 * 清单上另有一格 Key（做加密用的那个）——那是**另一回事**：本项目不做 Key 加密
 * （设计决策 D11），控件里那一格留空，我们这边也没有对应的值可给。
 *
 * # 「复制」与「轮换」的分工（决策 D10）
 *
 * - **复制是纯读**：误点复制不能有任何后果。三项复制一个都不写状态。
 * - **轮换是写**：独立按钮、必须二次确认，确认框里写清后果——已配置该字段的控件会
 *   **立即失效**，得把新 Token 粘回审批后台。
 *
 * # Token 明文为什么不常驻
 *
 * 清单是常驻的一页，而凭据是 `secret` 级的东西。明文只在两条路径上出现：
 * 用户点「复制 Token」（回显一次）与轮换成功后（那一次响应就是新值）。
 * 两者都进本组件的本地状态，刷新页面即消失。块里画的永远是掩码。
 *
 * # 复制失败时的退路（2026-09-24 修）
 *
 * 明文 HTTP 部署下 `navigator.clipboard` 不存在，复制成了「点了没反应」。
 * 而这一页有个别处没有的问题：**Token 是唯一一处「复制不成，就彻底拿不到」的值**
 * ——它只进本地状态，块里从不渲染；更糟的是「轮换」会成功并让已配好的飞书控件
 * 立刻失效，而新 Token 又复制不出来，运维就被留在「集成已死、新值拿不到」的死角里。
 *
 * 所以失败时**只有那一次**把明文就地渲染成可选中文本（带 `select-all`，点一下全选）。
 * 接口地址与联动 key 本来就渲染在块里，只指路、不重复摆一遍。
 */

import { Check, Copy } from "lucide-react";
import { useState, type ReactNode } from "react";

import { copyText } from "@/shared/lib/clipboard";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

import type { CredentialClient } from "../api";
import type { CredentialItem } from "../types";
import {
  approvalOptionsUrl,
  credentialLabel,
  formatUnixSeconds,
} from "../types";

/// 轮换确认框的正文。**逐字**是设计 §10.3 的口径：后果写在正文里，不藏在 tooltip。
const ROTATE_MESSAGE =
  "轮换后，已配置该字段的控件会立即失效，需要把新的 Token 粘回审批后台。";

/// Token 的掩码。块里永远画它，明文只走复制那条路（见文件头）。
const TOKEN_MASK = "••••••••••••••••";

/// 「最近轮换」那一行的文案。**三态不许折平**：`undefined`（投影没给这一列，
/// 拿不到）与 `null`（服务端明确说从未轮换过）是两件事，画成同一句就是假话。
function rotatedText(item: CredentialItem): string {
  if (item.tokenRotatedAt === undefined) return "轮换时间未知";
  if (item.tokenRotatedAt === null) return "从未轮换";
  return `最近轮换 ${formatUnixSeconds(item.tokenRotatedAt)}`;
}

export type CredentialChecklistProps = {
  items: ReadonlyArray<CredentialItem>;
  /// 取选项接口的地址前缀。默认当前站点——飞书要访问的是同一个域名。
  origin?: string;
  /// 凭据入口。**不给就是「这个部署没有回显与轮换的权限」**：
  /// 那时清单照旧显示 URL 与联动 key（两者都是本地就能拿到的值），只是 Token 与轮换不可用。
  client?: CredentialClient;
  onRotated?: (sourceKey: string, token: string) => void;
};

/// 一项可复制的值：左边一个短标签，右边是值本身加操作按钮。
///
/// 标签固定宽度而不是自适应：块里三行的标签长短不一（接口地址 / Token / 联动 key），
/// 定宽才能让三个值左边缘对齐，扫起来是一条竖线。
function CredentialLine({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="grid grid-cols-[4.5rem_minmax(0,1fr)] items-start gap-x-3 gap-y-1">
      <span className="pt-1.5 text-xs text-muted-foreground">{label}</span>
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        {children}
      </div>
    </div>
  );
}

/// 值本身。`break-all` 是刻意的：接口地址没有空格可断，用 `truncate` 就会把它截掉，
/// 而这一段的全文正是要粘出去的东西。
function CredentialValue({ value }: { value: string }) {
  return (
    <code className="min-w-0 flex-1 rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-xs break-all select-all">
      {value}
    </code>
  );
}

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
  /// 剪贴板写不进去时的退路。
  ///
  /// `shownAt` = 这一项在块里显示在哪个标签下（接口地址 / 联动 key）；Token 不显示
  /// 在块里，所以它是 `undefined`，同时带上 `plaintext`——那一次不给屏幕就没别的路可走。
  const [copyFailure, setCopyFailure] = useState<{
    sourceKey: string;
    shownAt?: string;
    plaintext?: string;
  } | null>(null);

  /// 复制成功才点亮「已复制」；否则把退路交回给渲染层。绝不谎报成功
  /// ——用户会去别处粘一个空的，比不响应更糟。
  async function copy(key: string, value: string): Promise<boolean> {
    if ((await copyText(value)) === "copied") {
      setCopiedKey(key);
      return true;
    }
    return false;
  }

  /// 复制一个**块里本来就显示着**的值（接口地址 / 联动 key）：值在本地，不发请求。
  async function copyShown(
    item: CredentialItem,
    shownAt: string,
    key: string,
    value: string,
  ) {
    setError(null);
    setCopiedKey(null);
    setCopyFailure(null);
    if (!(await copy(key, value))) {
      setCopyFailure({ sourceKey: item.sourceKey, shownAt });
    }
  }

  /// 复制 Token：**只回显**（读），没有值就取一次。绝不触发轮换。
  async function copyToken(item: CredentialItem) {
    setError(null);
    setCopiedKey(null);
    setCopyFailure(null);
    const existing = tokens[item.sourceKey];
    if (existing !== undefined) {
      if (!(await copy(`${item.sourceKey}#token`, existing))) {
        setCopyFailure({ sourceKey: item.sourceKey, plaintext: existing });
      }
      return;
    }
    if (client === undefined) {
      setError("当前身份没有回显凭据的权限，取不到这个 Token。");
      return;
    }
    try {
      const token = await client.reveal(item.sourceKey);
      setTokens((previous) => ({ ...previous, [item.sourceKey]: token }));
      // 回显已经成功了（Token 就在手里），死只死在剪贴板：那就把它摆出来。
      if (!(await copy(`${item.sourceKey}#token`, token))) {
        setCopyFailure({ sourceKey: item.sourceKey, plaintext: token });
      }
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function confirmRotate() {
    if (rotating === null || client === undefined) return;
    setPending(true);
    setError(null);
    // 轮换会作废旧值，所以上次复制失败时摆出来的那段明文**当场就失效了**。
    // 不清掉它，用户可能照着屏幕把那串已经作废的 Token 粘回审批后台。
    setCopyFailure(null);
    // 同理：那一行的「已复制」勾勾指的是**旧** Token（剪贴板里也还是旧的）。
    // 留着它等于说「新的这串已经复制好了」，而它其实还没被碰过。
    setCopiedKey(null);
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
        一块一个字段：接口地址与 Token 都可以直接复制，复制不改变任何状态，
        误点没有后果；要换 Token 走那一块里的「轮换」。 带父字段的还多一项「联动
        key」。审批控件里另有
        <span className="font-medium">一格 Key</span>
        （做加密用的那个）——本项目不做 Key 加密，那一格留空，它与这里的「联动
        key」不是一回事。
      </p>

      {client === undefined ? (
        <p className="text-xs text-muted-foreground">
          当前身份没有回显与轮换凭据的权限：地址与联动 key 照旧可复制，Token
          与轮换这两项用不了。
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

      {copyFailure === null ? null : (
        <div
          role="alert"
          className="space-y-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <p>
            {
              // 只说「这次没写成」这个已知事实，再点出最常见的原因——
              // 断言「就是明文 HTTP 干的」在 HTTPS 上（例如权限被拒）就是假话。
            }
            浏览器这次没允许写剪贴板，明文 HTTP 页面最常见的原因是这个。
            {copyFailure.shownAt === undefined
              ? // Token 那条路：它不像地址那样本来就渲染在页面上，必须摆出来。
                `字段 ${copyFailure.sourceKey} 的这段 Token 只能手动复制，`
              : // 本来就显示着的值只指路。提示挂在清单**上方**，所以往下指，
                // 并点名是哪一项——多行时「上面那一行」既指错方向、也对不上号。
                `字段 ${copyFailure.sourceKey} 的「${copyFailure.shownAt}」就在下面那一块里，`}
            点一下即可全选，再按 Ctrl/Cmd + C。
          </p>
          {copyFailure.plaintext === undefined ? null : (
            <code className="block rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-xs break-all text-foreground select-all">
              {copyFailure.plaintext}
            </code>
          )}
        </div>
      )}

      <ul className="divide-y divide-border rounded-lg border border-border">
        {items.map((item) => {
          const url = approvalOptionsUrl(base, item.sourceKey);
          // 一页有多块、每块最多三个复制点，所以可访问名必须点名**是哪一个字段的哪一项**：
          // 只喊「复制」的话，屏幕阅读器用户无从分辨。
          const buttonName = (action: string) =>
            `${action}：${credentialLabel(item)}`;
          const copied = (target: string) =>
            copiedKey === `${item.sourceKey}#${target}`;
          return (
            <li
              key={item.sourceKey}
              data-slot="credential-row"
              className="space-y-2.5 p-4"
            >
              <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <span className="text-sm font-medium">
                  {item.fieldName ?? "（字段名还没解析出来）"}
                </span>
                <span className="font-mono text-xs text-muted-foreground">
                  {item.sourceKey}
                </span>
                <span className="ml-auto text-xs text-muted-foreground tabular-nums">
                  {rotatedText(item)}
                </span>
              </div>

              {
                // 父被停用时服务端按「无父」处理，这条级联整个取不到选项。
                // 不说这一句，那一串联动 key 会看起来是「填上就能用」的。
              }
              {item.parent !== null && !item.parent.enabled ? (
                <p className="text-xs text-tone-warning">
                  父字段「{item.parent.label}
                  」已停用，这条级联现在取不到选项——先把它启用。
                </p>
              ) : null}

              <CredentialLine label="接口地址">
                <CredentialValue value={url} />
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={buttonName("复制接口地址")}
                  onClick={() =>
                    void copyShown(
                      item,
                      "接口地址",
                      `${item.sourceKey}#url`,
                      url,
                    )
                  }
                >
                  {copied("url") ? (
                    <Check aria-hidden="true" />
                  ) : (
                    <Copy aria-hidden="true" />
                  )}
                  复制
                </Button>
              </CredentialLine>

              <CredentialLine label="Token">
                <CredentialValue value={TOKEN_MASK} />
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={buttonName("复制 Token")}
                  onClick={() => void copyToken(item)}
                >
                  {copied("token") ? (
                    <Check aria-hidden="true" />
                  ) : (
                    <Copy aria-hidden="true" />
                  )}
                  复制
                </Button>
                {/* 轮换**不挨着**复制：它是写操作，点错会作废已经配好的控件。
                    真正的闸门是下面那个确认框。 */}
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={buttonName("轮换 Token")}
                  onClick={() => {
                    setError(null);
                    setRotating(item);
                  }}
                >
                  轮换
                </Button>
              </CredentialLine>

              {item.parent === null ? null : (
                <CredentialLine label="联动 key">
                  <CredentialValue value={item.parent.linkageKey} />
                  <Button
                    variant="outline"
                    size="sm"
                    aria-label={buttonName("复制联动 key")}
                    onClick={() =>
                      void copyShown(
                        item,
                        "联动 key",
                        `${item.sourceKey}#linkage`,
                        item.parent?.linkageKey ?? "",
                      )
                    }
                  >
                    {copied("linkage") ? (
                      <Check aria-hidden="true" />
                    ) : (
                      <Copy aria-hidden="true" />
                    )}
                    复制
                  </Button>
                  <p className="basis-full text-xs text-muted-foreground">
                    父字段「{item.parent.label}
                    」的字段 id，填进审批后台那个联动控件的「参数代码」。
                    那个控件挂了不止一个联动参数时，不填它就判不出哪个是父值。
                  </p>
                </CredentialLine>
              )}
            </li>
          );
        })}
      </ul>

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
