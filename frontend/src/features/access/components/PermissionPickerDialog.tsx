/**
 * 权限选择对话框（直授 / 组条目共用一个）。
 *
 * 只收集选择，**不发请求**：确认后把 `PermissionSelection` 交给调用方回调，
 * 由调用方走写接口、回读、落提示（「写完全部回读」纪律不变）。
 *
 * 三个刻意写死的行为：
 * 1. **管理员等价权限要二次确认**：选中含管理员等价权限（G2）时，确认按钮下方
 *    亮红色警示条（列出 reason），勾选「我已知晓上述风险」前确认按钮不可用——
 *    不是禁用选择，而是让「点下去」这个动作本身带上知情确认；
 * 2. **直授单选、组条目多选**：直授一次只授予一条权限（带过期时间字段），
 *    组条目一次可勾多条（组里没有过期概念，字段不出现）；
 * 3. **过期时间用原生 date 控件 + 「永久」选项**：转 unix 秒整数（选到哪天授权到
 *    那天的最后一秒），「永久」或清空提交 `null`（服务端 `expires_at: Option<i64>`）。
 */

import { useEffect, useMemo, useState } from "react";
import { RefreshCw } from "lucide-react";

import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
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
import { cn } from "@/shared/lib/utils";

import { groupPermissionMeta, type PermissionMeta } from "../permission-meta";
import { usePermissionMeta } from "../workspace-api";
import { PermissionBadge } from "./PermissionBadge";

/// 本组件收集到的选择：确认时交给调用方。
export type PermissionSelection = {
  permissions: string[];
  /// 直授模式的过期时间（unix 秒整数）；「永久」或组条目模式为 null。
  expiresAt: number | null;
};

export function PermissionPickerDialog({
  open,
  onOpenChange,
  mode,
  busy,
  onSubmit,
  exclude = [],
  presetPermission = null,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// 直授模式（单选 + 过期字段）或组条目模式（多选、无过期字段）。
  mode: "direct" | "group";
  busy: boolean;
  onSubmit: (selection: PermissionSelection) => void;
  /// 候选排除：组条目模式下把「已在组里」的权限挪出候选
  /// （已在矩阵里的那条没有再加一次的意义，服务端虽幂等）。
  exclude?: string[];
  /// 预选（直授模式的重新授予）：打开时把目标权限放进选中，用户只改日期。
  presetPermission?: string | null;
}) {
  const { meta, isPending, isError, refetch } = usePermissionMeta();

  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  /// 二次确认（只在选中含管理员等价权限时被要求）。
  const [confirmed, setConfirmed] = useState(false);
  /// 过期时间（direct 模式）：date 控件的原生值 `YYYY-MM-DD`，空 = 永久。
  const [expiresOn, setExpiresOn] = useState("");
  const [permanent, setPermanent] = useState(true);

  /// 打开即清场：下次打开不会带着上一次的选择/确认/过期时间；
  /// 预选（重新授予）时把目标权限放进去，其余情形从空开始。
  useEffect(() => {
    if (!open) return;
    setSelected(presetPermission ? [presetPermission] : []);
    setConfirmed(false);
    setExpiresOn("");
    setPermanent(true);
  }, [open, presetPermission]);

  /// 选中项一旦变化，「已知晓风险」必须重新勾（不能跨选择沿用）。
  useEffect(() => {
    setConfirmed(false);
  }, [selected]);

  /// 搜索：权限字符串 / 中文名 / 说明，三路都匹配（不区分大小写）；
  /// `exclude` 先于搜索过滤（组条目模式下已在该组的权限不再出现）。
  const needle = search.trim().toLowerCase();
  const visible = useMemo(() => {
    const all = [...meta.values()].filter(
      (item) => !exclude.includes(item.permission),
    );
    if (needle === "") return all;
    return all.filter(
      (item) =>
        item.permission.toLowerCase().includes(needle) ||
        item.title.toLowerCase().includes(needle) ||
        (item.description ?? "").toLowerCase().includes(needle),
    );
  }, [meta, needle, exclude]);
  const groups = useMemo(() => groupPermissionMeta(visible), [visible]);

  function toggle(permission: string) {
    setSelected((current) => {
      if (current.includes(permission)) {
        return current.filter((candidate) => candidate !== permission);
      }
      // 直授单选：新选择替换旧选择（一次只授予一条）
      return mode === "direct" ? [permission] : [...current, permission];
    });
  }

  /// 选中项里的管理员等价权限（含各自 reason，供警示条逐条列出）。
  const dangerSelected = useMemo(
    () =>
      selected
        .map((permission) => meta.get(permission))
        .filter((item): item is PermissionMeta => item !== undefined)
        .filter((item) => item.adminEquivalent),
    [selected, meta],
  );
  const dangerReasons = useMemo(
    () => [
      ...new Set(
        dangerSelected
          .map((item) => item.reason)
          .filter((r): r is string => r !== null),
      ),
    ],
    [dangerSelected],
  );

  /// 选到哪天，授权到那天的最后一秒（比「当天 00:00 过期」宽容一天）。
  function expiresAtOf(): number | null {
    if (permanent || expiresOn === "") return null;
    const ms = Date.parse(`${expiresOn}T23:59:59Z`);
    return Number.isFinite(ms) ? Math.floor(ms / 1000) : null;
  }

  const canSubmit =
    !busy && selected.length > 0 && (dangerSelected.length === 0 || confirmed);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {mode === "direct" ? "授予权限" : "加入权限到组"}
          </DialogTitle>
          <DialogDescription>
            {mode === "direct"
              ? "从权限目录选择一条权限授予该用户，可设过期时间。"
              : "从权限目录勾选要加入组的权限（可多选）。"}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="搜索权限…"
            aria-label="搜索权限"
          />

          {isPending ? (
            <p className="text-sm text-muted-foreground">正在加载权限目录…</p>
          ) : isError ? (
            <div
              role="alert"
              className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              <span>权限目录加载失败，请重试。</span>
              <Button variant="outline" size="sm" onClick={refetch}>
                <RefreshCw aria-hidden="true" />
                重试
              </Button>
            </div>
          ) : groups.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {search.trim() === "" ? "权限目录是空的。" : "没有匹配的权限。"}
            </p>
          ) : (
            <div className="space-y-2">
              {groups.map((group) => (
                <details
                  key={group.title}
                  open
                  className="rounded-lg border border-border bg-card"
                >
                  <summary className="cursor-pointer px-3 py-2 text-sm font-medium select-none">
                    {group.title}
                    <span className="ml-2 text-xs font-normal text-muted-foreground">
                      共 {group.permissions.length} 项
                    </span>
                  </summary>
                  <ul className="space-y-1 px-3 pb-3">
                    {group.permissions.map((item) => (
                      <li key={item.permission}>
                        <label
                          className={cn(
                            "flex cursor-pointer items-start gap-2 rounded-md border px-3 py-2",
                            item.adminEquivalent
                              ? "border-destructive/40"
                              : "border-border",
                          )}
                        >
                          <Checkbox
                            className="mt-0.5"
                            checked={selected.includes(item.permission)}
                            onCheckedChange={() => toggle(item.permission)}
                            aria-label={`${item.title} 权限`}
                          />
                          <span className="min-w-0 flex-1">
                            <span className="flex flex-wrap items-center gap-2">
                              <span className="text-sm font-medium">
                                {item.title}
                              </span>
                              {item.adminEquivalent ? (
                                <PermissionBadge
                                  kind="danger"
                                  reason={item.reason}
                                />
                              ) : null}
                            </span>
                            <span className="block truncate font-mono text-xs text-muted-foreground">
                              {item.permission}
                            </span>
                            {item.description !== undefined ? (
                              <span className="mt-0.5 block text-xs text-muted-foreground">
                                {item.description}
                              </span>
                            ) : null}
                          </span>
                        </label>
                      </li>
                    ))}
                  </ul>
                </details>
              ))}
            </div>
          )}
        </div>

        {mode === "direct" ? (
          <fieldset className="space-y-2 border-t border-border pt-3">
            <legend className="text-sm font-medium">过期时间</legend>
            <div className="flex flex-wrap items-end gap-3">
              <label className="flex items-center gap-2 text-sm">
                <Checkbox
                  checked={permanent}
                  onCheckedChange={(checked) => setPermanent(checked === true)}
                  aria-label="永久有效"
                />
                永久有效
              </label>
              {!permanent ? (
                <div className="space-y-1">
                  <Label htmlFor="permission-expires-on">过期日期</Label>
                  <Input
                    id="permission-expires-on"
                    type="date"
                    value={expiresOn}
                    onChange={(event) => setExpiresOn(event.target.value)}
                  />
                </div>
              ) : null}
            </div>
          </fieldset>
        ) : null}

        {dangerSelected.length > 0 ? (
          <div
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            <p className="font-medium">
              所选包含 {dangerSelected.length} 项管理员等价权限，
              授予它们等于交出一部分系统管理能力。
            </p>
            {dangerReasons.length > 0 ? (
              <ul className="mt-1 list-disc space-y-0.5 pl-5">
                {dangerReasons.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            ) : null}
            <label className="mt-2 flex items-center gap-2">
              <Checkbox
                checked={confirmed}
                onCheckedChange={(checked) => setConfirmed(checked === true)}
                aria-label="确认风险"
              />
              <span>我已知晓上述风险，仍要继续</span>
            </label>
          </div>
        ) : null}

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={busy}
          >
            取消
          </Button>
          <Button
            disabled={!canSubmit}
            onClick={() => {
              onSubmit({
                permissions: [...selected],
                expiresAt: mode === "direct" ? expiresAtOf() : null,
              });
            }}
          >
            {mode === "direct" ? "确认授予" : "确认加入"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
