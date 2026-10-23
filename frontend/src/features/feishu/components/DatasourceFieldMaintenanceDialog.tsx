/**
 * 外部数据源维护：字段被删后停用或原地改绑，远端新增字段可刷新后加入。
 *
 * # 边界（后端硬约束，界面不许越界）
 *
 * 维护只改 `field_id` / `field_name` / `parent_field_id` / `enabled`。
 * **绝不改** `source_key`、Token、绑定行 id、数据源 id——审批后台里已经配好的
 * URL/Token 因此不受影响。所以这个对话框里没有任何 `source_key`/Token 的编辑入口，
 * 只有「新增字段」那一刻由服务端签发的一次性 Token 需要复制出去。
 *
 * # 业务人员看字段名
 *
 * 每一行都以**字段名**为主（`fieldName ?? "（字段名还没解析出来）"`），
 * `field_id` 与 `source_key` 只作排查信息（小号等宽、可复制）。
 *
 * # 加载策略
 *
 * 打开时**并行**拉「远端字段列表」与「体检报告」：前者是改绑/新增的候选，
 * 后者给出缺失/重名/类型不支持/未绑定四类信息。任何一次失败都原地展示后端原文，
 * 不吞掉。
 */

import { useCallback, useEffect, useState } from "react";
import { Check, Copy, RefreshCw, Wrench } from "lucide-react";

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
import { Skeleton } from "@/shared/ui/skeleton";

import type { DatasourceMaintenanceClient } from "../api";
import type {
  BitableField,
  DatasourceFieldBinding,
  DatasourceItem,
  HealthReport,
} from "../types";
import {
  approvalOptionsUrl,
  fieldTypeLabel,
  orderBindingsForDisplay,
} from "../types";
import { StatusBadge } from "./StatusBadge";

const NEUTRAL_BAR =
  "rounded-md border border-border bg-muted/50 px-3 py-2 text-sm";

const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

/// 停用确认的正文。**后果写在正文里**，不藏在 tooltip：停用期间控件拿不到选项，
/// 但绑定、`source_key`、Token 都保留，重新启用即恢复。
const DISABLE_MESSAGE =
  "飞书控件会暂时取不到选项（40301），重新启用即可恢复。绑定行、source_key 与 Token 都会保留。";

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function bindingLabel(binding: DatasourceFieldBinding): string {
  return binding.fieldName ?? "（字段名还没解析出来）";
}

/// 远端字段的类型标签。未知/缺失类型**照实说**，不猜成一个「文本」。
function remoteTypeLabel(field: BitableField): string {
  return fieldTypeLabel(field.type);
}

/// 体检项里的类型标签（类型码可能为 `null`）。
function healthTypeLabel(fieldType: number | null): string {
  return fieldType === null ? "类型未知" : fieldTypeLabel(fieldType);
}

export type DatasourceFieldMaintenanceDialogProps = {
  open: boolean;
  /// 目标数据源。只允许「定时拉取」源——它才有远端字段列表与体检可看。
  datasource: DatasourceItem;
  client: DatasourceMaintenanceClient;
  /// 任何一次写成功之后调用，由页面作废数据源与体检查询。
  onChanged?: () => void;
  onClose: () => void;
};

export function DatasourceFieldMaintenanceDialog({
  open,
  datasource,
  client,
  onChanged,
  onClose,
}: DatasourceFieldMaintenanceDialogProps) {
  const datasourceId = datasource.id;
  const appToken = datasource.bitableBaseToken;
  const tableId = datasource.bitableTableId;

  const [fields, setFields] = useState<BitableField[] | null>(null);
  const [report, setReport] = useState<HealthReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /// 正在跑的那个操作（`repoint:<key>` / `status:<key>` / `add:<fld>`）。
  /// 非空时所有写按钮禁用，避免连点发出两条互相打架的请求。
  const [busy, setBusy] = useState<string | null>(null);
  const [repointing, setRepointing] = useState<string | null>(null);
  const [repointTarget, setRepointTarget] = useState("");
  /// `"__keep__"` = 保持原父指针；`"__none__"` = 解除父级；其余是父字段 id。
  const [repointParent, setRepointParent] = useState("__keep__");
  /// 新增字段的父字段选择（key = 新字段 id）。
  const [addParents, setAddParents] = useState<Record<string, string>>({});
  const [disabling, setDisabling] = useState<string | null>(null);
  const [added, setAdded] = useState<null | {
    sourceKey: string;
    fieldName: string;
    fieldId: string;
    token: string;
  }>(null);
  const [copied, setCopied] = useState<"url" | "token" | null>(null);

  const load = useCallback(async () => {
    if (datasourceId === null) return;
    setLoading(true);
    setLoadError(null);
    try {
      const [remote, health] = await Promise.all([
        appToken !== null && tableId !== null
          ? client.listFields(appToken, tableId)
          : Promise.resolve([] as BitableField[]),
        client.health(datasourceId),
      ]);
      setFields(remote);
      setReport(health);
    } catch (cause) {
      setLoadError(messageOf(cause));
    } finally {
      setLoading(false);
    }
  }, [appToken, client, datasourceId, tableId]);

  // 每次打开都重新取一遍：体检与远端字段都是**当下**的事实，缓存一份旧的只会
  // 让人对着已经修好的行继续点「改绑」。
  useEffect(() => {
    if (!open) return;
    setNotice(null);
    setError(null);
    setAdded(null);
    setCopied(null);
    setRepointing(null);
    setDisabling(null);
    void load();
  }, [open, load]);

  const bindings = orderBindingsForDisplay(datasource.fields).map(
    (entry) => entry.binding,
  );
  const byFieldId = new Map(
    datasource.fields.map((binding) => [binding.fieldId, binding]),
  );

  function healthOf(binding: DatasourceFieldBinding) {
    const missing =
      report?.missingFields.find((item) => item.fieldId === binding.fieldId) ??
      null;
    const duplicate =
      report?.duplicateNameFields.some(
        (item) => item.fieldId === binding.fieldId,
      ) === true;
    const unsupported =
      report?.unsupportedTypeFields.some(
        (item) => item.fieldId === binding.fieldId,
      ) === true;
    return { missing, duplicate, unsupported };
  }

  /// 可选的远端字段：排除**其他**绑定占用的 `field_id`；当前字段仍可选
  /// （改绑面板初值就是它，方便对照）。
  function repointCandidates(binding: DatasourceFieldBinding): BitableField[] {
    const occupied = new Set(
      datasource.fields
        .filter((other) => other.sourceKey !== binding.sourceKey)
        .map((other) => other.fieldId),
    );
    return (fields ?? []).filter(
      (field) =>
        field.fieldId === binding.fieldId || !occupied.has(field.fieldId),
    );
  }

  /// 可以作为父字段的绑定：启用中、不是自己、也不是自己的后代（避免界面主动
  /// 造出一个环——后端虽然会拒，但没必要让用户先撞一次墙）。
  function parentCandidates(
    binding: DatasourceFieldBinding,
  ): DatasourceFieldBinding[] {
    const descendants = new Set<string>();
    let frontier = [binding.fieldId];
    while (frontier.length > 0) {
      const parents = new Set(frontier);
      frontier = datasource.fields
        .filter(
          (other) =>
            other.parentFieldId !== null &&
            parents.has(other.parentFieldId) &&
            !descendants.has(other.fieldId),
        )
        .map((other) => {
          descendants.add(other.fieldId);
          return other.fieldId;
        });
    }
    return datasource.fields.filter(
      (other) =>
        other.enabled &&
        other.fieldId !== binding.fieldId &&
        !descendants.has(other.fieldId),
    );
  }

  /// 改绑面板当前生效的父字段。`__keep__` 取原值，`__none__` 取 `null`。
  function effectiveRepointParent(
    binding: DatasourceFieldBinding,
  ): string | null {
    if (repointParent === "__keep__") return binding.parentFieldId;
    if (repointParent === "__none__") return null;
    return repointParent;
  }

  async function run(
    key: string,
    action: () => Promise<void>,
  ): Promise<boolean> {
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      await action();
      return true;
    } catch (cause) {
      // 后端的拒绝理由（「是这些子字段的父级」「远端字段已不存在」…）就是
      // 用户要照着改的那句话，原样展示，不换一句泛化文案。
      setError(messageOf(cause));
      return false;
    } finally {
      setBusy(null);
    }
  }

  async function submitRepoint(binding: DatasourceFieldBinding) {
    if (datasourceId === null || repointTarget === "") return;
    const target = fields?.find((field) => field.fieldId === repointTarget);
    const parentFieldId = effectiveRepointParent(binding);
    const ok = await run(`repoint:${binding.sourceKey}`, async () => {
      const result = await client.repoint({
        datasourceId,
        sourceKey: binding.sourceKey,
        fieldId: repointTarget,
        parentFieldId,
      });
      setRepointing(null);
      setRepointParent("__keep__");
      const affected = result.affectedApprovalConfigs.length;
      const approvalHint =
        affected > 0
          ? `；有 ${affected} 个审批配置引用了该字段，请重新保存这些配置以重建选项映射`
          : "";
      setNotice(
        `已把「${bindingLabel(binding)}」改绑到「${
          target?.fieldName || result.fieldName || result.fieldId
        }」${result.updatedPortals > 0 ? `，同步更新了 ${result.updatedPortals} 个查询门户` : ""}${approvalHint}。`,
      );
    });
    if (ok) {
      onChanged?.();
      void load();
    }
  }

  async function submitToggle(
    binding: DatasourceFieldBinding,
    enabled: boolean,
  ) {
    if (datasourceId === null) return;
    const ok = await run(`status:${binding.sourceKey}`, async () => {
      await client.setEnabled({
        datasourceId,
        sourceKey: binding.sourceKey,
        enabled,
      });
      setDisabling(null);
      setNotice(`已${enabled ? "启用" : "停用"}「${bindingLabel(binding)}」。`);
    });
    if (ok) {
      onChanged?.();
      void load();
    }
  }

  async function submitAdd(field: HealthReport["unboundFields"][number]) {
    if (datasourceId === null) return;
    const selectedParent = addParents[field.fieldId] ?? "__none__";
    const parentFieldId =
      selectedParent === "__none__" ? undefined : selectedParent;
    const ok = await run(`add:${field.fieldId}`, async () => {
      const result = await client.addField({
        datasourceId,
        fieldId: field.fieldId,
        ...(parentFieldId === undefined ? {} : { parentFieldId }),
      });
      setAdded(result);
      setNotice(
        `已新增字段「${result.fieldName || field.fieldName || result.fieldId}」。`,
      );
    });
    if (ok) {
      onChanged?.();
      void load();
    }
  }

  async function copy(value: string, kind: "url" | "token") {
    const outcome = await copyText(value);
    if (outcome === "copied") {
      setCopied(kind);
      setError(null);
    } else {
      // 绝不谎报「已复制」：明文就摊在下面，指路让人手动选中。
      setError("浏览器这次没允许写剪贴板，请手动选中下面那段文本复制。");
    }
  }

  const unbound = report?.unboundFields ?? [];
  const addedUrl =
    added === null
      ? ""
      : approvalOptionsUrl(window.location.origin, added.sourceKey);

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && busy === null) onClose();
      }}
    >
      <DialogContent showCloseButton={busy === null} className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>维护字段</DialogTitle>
          <DialogDescription className="space-y-1">
            <span className="block">
              只支持「定时拉取」数据源。维护只改字段绑定（选哪一列、父字段、启用状态），
              <span className="font-medium">
                不会改变已有控件的接口地址或 Token
              </span>
              。
            </span>
            <span className="block">
              字段 ID 与 source_key 仅作排查信息；日常判断看字段名。
            </span>
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={loading || busy !== null}
            onClick={() => void load()}
          >
            <RefreshCw aria-hidden="true" />
            {loading ? "加载中…" : "刷新字段"}
          </Button>
          <span className="text-xs text-muted-foreground">
            远端字段 {fields?.length ?? "—"} 个 · 绑定{" "}
            {datasource.fields.length} 条
          </span>
        </div>

        {loadError !== null ? (
          <p role="alert" className={ERROR_BAR}>
            {loadError}
          </p>
        ) : null}

        {error !== null ? (
          <p role="alert" className={ERROR_BAR}>
            {error}
          </p>
        ) : null}

        {notice !== null ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            {notice}
          </p>
        ) : null}

        {added !== null ? (
          <div
            role="status"
            className="space-y-2 rounded-md border border-tone-positive/40 bg-tone-positive/10 px-3 py-2 text-sm"
          >
            <p className="font-medium">
              已新增字段「{added.fieldName || added.fieldId}」
            </p>
            <p className="text-muted-foreground">
              请把 URL 与 Token 粘到飞书审批后台的对应控件。Token
              明文在这里直接给出；之后可在「凭据配置」里回显（需要相应权限）。
            </p>
            {(
              [
                ["接口地址", addedUrl, "url", "复制接口地址"],
                ["Token", added.token, "token", "复制 Token"],
              ] as const
            ).map(([label, value, kind, copyName]) => (
              <div key={kind} className="flex flex-wrap items-center gap-2">
                <span className="w-16 shrink-0 text-xs text-muted-foreground">
                  {label}
                </span>
                <code className="min-w-0 flex-1 rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-xs break-all select-all">
                  {value}
                </code>
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={copyName}
                  onClick={() => void copy(value, kind)}
                >
                  {copied === kind ? (
                    <Check aria-hidden="true" />
                  ) : (
                    <Copy aria-hidden="true" />
                  )}
                  复制
                </Button>
              </div>
            ))}
          </div>
        ) : null}

        {/* 当前绑定（含停用行）：业务上看到的是字段名，ID/标识只作排查信息。 */}
        <section className="space-y-2">
          <h3 className="text-sm font-semibold">当前绑定</h3>
          {loading && report === null ? (
            <Skeleton className="h-20 w-full" />
          ) : bindings.length === 0 ? (
            <p className={NEUTRAL_BAR}>这条数据源还没有字段绑定。</p>
          ) : (
            <ul className="divide-y divide-border rounded-lg border border-border">
              {bindings.map((binding) => {
                const { missing, duplicate, unsupported } = healthOf(binding);
                const parentFieldId = binding.parentFieldId;
                const parent =
                  parentFieldId === null
                    ? null
                    : (byFieldId.get(parentFieldId) ?? null);
                const candidates = repointCandidates(binding);
                const parentOptions = parentCandidates(binding);
                const effectiveParent = effectiveRepointParent(binding);
                const parentChanged = effectiveParent !== binding.parentFieldId;
                return (
                  <li
                    key={binding.sourceKey}
                    data-slot="maintenance-row"
                    className="space-y-2 p-3"
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium">
                        {bindingLabel(binding)}
                      </span>
                      <StatusBadge
                        tone={binding.enabled ? "positive" : "neutral"}
                      >
                        {binding.enabled ? "启用" : "已停用"}
                      </StatusBadge>
                      {missing !== null ? (
                        <StatusBadge tone="danger">远端已删除</StatusBadge>
                      ) : null}
                      {duplicate ? (
                        <StatusBadge tone="warning">字段名重复</StatusBadge>
                      ) : null}
                      {unsupported ? (
                        <StatusBadge tone="warning">类型不支持</StatusBadge>
                      ) : null}
                      {parentFieldId !== null ? (
                        <span className="text-xs text-muted-foreground">
                          父字段：{parent?.fieldName ?? parentFieldId}
                        </span>
                      ) : null}
                      <span className="ml-auto font-mono text-xs text-muted-foreground select-all">
                        {binding.sourceKey}
                      </span>
                    </div>

                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-xs text-muted-foreground">
                        {binding.fieldId}
                      </span>
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={`改绑：${bindingLabel(binding)}`}
                        disabled={busy !== null}
                        onClick={() => {
                          setError(null);
                          setNotice(null);
                          setDisabling(null);
                          setRepointing(binding.sourceKey);
                          setRepointParent("__keep__");
                          setRepointTarget(
                            candidates.some(
                              (field) => field.fieldId === binding.fieldId,
                            )
                              ? binding.fieldId
                              : "",
                          );
                        }}
                      >
                        <Wrench aria-hidden="true" />
                        改绑
                      </Button>
                      {binding.enabled ? (
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={`停用：${bindingLabel(binding)}`}
                          disabled={busy !== null}
                          onClick={() => {
                            setError(null);
                            setNotice(null);
                            setRepointing(null);
                            setDisabling(binding.sourceKey);
                          }}
                        >
                          停用
                        </Button>
                      ) : (
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={`启用：${bindingLabel(binding)}`}
                          disabled={busy !== null}
                          onClick={() => void submitToggle(binding, true)}
                        >
                          启用
                        </Button>
                      )}
                    </div>

                    {repointing === binding.sourceKey ? (
                      <div className="space-y-2 rounded-md border border-border bg-muted/30 p-3">
                        <label className="block text-xs font-medium">
                          改绑到哪一列
                          <select
                            aria-label="改绑目标字段"
                            className="mt-1 block w-full rounded border border-border bg-background px-2 py-1 text-sm"
                            value={repointTarget}
                            onChange={(event) =>
                              setRepointTarget(event.target.value)
                            }
                          >
                            <option value="">请选择远端字段</option>
                            {candidates.map((field) => (
                              <option key={field.fieldId} value={field.fieldId}>
                                {field.fieldName || field.fieldId}（
                                {remoteTypeLabel(field)}）
                              </option>
                            ))}
                          </select>
                        </label>
                        <label className="block text-xs font-medium">
                          父字段
                          <select
                            aria-label="改绑父字段"
                            className="mt-1 block w-full rounded border border-border bg-background px-2 py-1 text-sm"
                            value={repointParent}
                            disabled={busy !== null}
                            onChange={(event) =>
                              setRepointParent(event.target.value)
                            }
                          >
                            <option value="__keep__">
                              保持当前
                              {parent === null
                                ? "（无父）"
                                : `（${parent.fieldName ?? binding.parentFieldId}）`}
                            </option>
                            <option value="__none__">无父（解除级联）</option>
                            {parentOptions.map((other) => (
                              <option
                                key={other.sourceKey}
                                value={other.fieldId}
                              >
                                {other.fieldName ?? other.fieldId}
                              </option>
                            ))}
                          </select>
                        </label>
                        <p className="text-xs text-muted-foreground">
                          已被其他绑定占用的列不会出现在这里。改绑保留
                          source_key 与 Token，已配置的控件不用改。
                        </p>
                        {parentChanged ? (
                          <p className="text-xs text-tone-warning">
                            改父级会改变该字段及子树的选项
                            id；已提交审批单里的旧值可能失联。
                          </p>
                        ) : null}
                        <div className="flex gap-2">
                          <Button
                            size="sm"
                            disabled={
                              busy !== null ||
                              repointTarget === "" ||
                              (repointTarget === binding.fieldId &&
                                !parentChanged)
                            }
                            onClick={() => void submitRepoint(binding)}
                          >
                            {busy === `repoint:${binding.sourceKey}`
                              ? "保存中…"
                              : "保存改绑"}
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={busy !== null}
                            onClick={() => {
                              setRepointing(null);
                              setRepointParent("__keep__");
                            }}
                          >
                            取消
                          </Button>
                        </div>
                      </div>
                    ) : null}

                    {disabling === binding.sourceKey ? (
                      <div
                        role="alertdialog"
                        aria-label="确认停用字段"
                        className="space-y-2 rounded-md border border-tone-warning/40 bg-tone-warning/10 p-3 text-sm"
                      >
                        <p>{DISABLE_MESSAGE}</p>
                        <div className="flex gap-2">
                          <Button
                            variant="destructive"
                            size="sm"
                            disabled={busy !== null}
                            onClick={() => void submitToggle(binding, false)}
                          >
                            {busy === `status:${binding.sourceKey}`
                              ? "停用中…"
                              : "确认停用"}
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={busy !== null}
                            onClick={() => setDisabling(null)}
                          >
                            取消
                          </Button>
                        </div>
                      </div>
                    ) : null}
                  </li>
                );
              })}
            </ul>
          )}
        </section>

        {/* 远端新增、尚未绑定：这不是故障，是「可以加一列」的入口。 */}
        <section className="space-y-2">
          <h3 className="text-sm font-semibold">可新增字段</h3>
          <p className="text-xs text-muted-foreground">
            远端有、本地还没绑定的列。新增会生成新的 source_key 与一次性
            Token，已有绑定不受影响。
          </p>
          {report === null ? (
            loading ? (
              <Skeleton className="h-12 w-full" />
            ) : (
              <p className={NEUTRAL_BAR}>
                体检结果还没拿到，暂时列不出可新增字段。
              </p>
            )
          ) : unbound.length === 0 ? (
            <p className={NEUTRAL_BAR}>远端没有尚未绑定的字段。</p>
          ) : (
            <ul className="divide-y divide-border rounded-lg border border-border">
              {unbound.map((field) => (
                <li
                  key={field.fieldId}
                  data-slot="unbound-row"
                  className="flex flex-wrap items-center gap-2 p-3"
                >
                  <span className="text-sm font-medium">
                    {field.fieldName || field.fieldId}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {healthTypeLabel(field.fieldType)}
                  </span>
                  <span className="font-mono text-xs text-muted-foreground">
                    {field.fieldId}
                  </span>
                  <label className="ml-auto flex items-center gap-1 text-xs text-muted-foreground">
                    父字段
                    <select
                      aria-label={`新增父字段：${field.fieldName || field.fieldId}`}
                      className="rounded border border-border bg-background px-2 py-1 text-sm"
                      value={addParents[field.fieldId] ?? "__none__"}
                      disabled={busy !== null}
                      onChange={(event) =>
                        setAddParents((previous) => ({
                          ...previous,
                          [field.fieldId]: event.target.value,
                        }))
                      }
                    >
                      <option value="__none__">无父</option>
                      {datasource.fields
                        .filter((binding) => binding.enabled)
                        .map((binding) => (
                          <option
                            key={binding.sourceKey}
                            value={binding.fieldId}
                          >
                            {binding.fieldName ?? binding.fieldId}
                          </option>
                        ))}
                    </select>
                  </label>
                  <Button
                    variant="outline"
                    size="sm"
                    aria-label={`添加：${field.fieldName || field.fieldId}`}
                    disabled={busy !== null}
                    onClick={() => void submitAdd(field)}
                  >
                    {busy === `add:${field.fieldId}` ? "添加中…" : "添加"}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <DialogFooter>
          <Button variant="ghost" disabled={busy !== null} onClick={onClose}>
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
