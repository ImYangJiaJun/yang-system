/**
 * 审批派发配置页（路由 `/feishu/approval/configs`，路由级 lazy → 必须 default 导出）。
 *
 * 页面接线四件事：
 * 1. **列表**：配置名 / 坐标 / 审批 Code / 时区 / 启用 / 快照时间 / 更新时间；
 *    行操作（启停 / 删除 / 映射明细）由 `ApprovalConfigRowActions` 渲染，门控
 *    `feishu.approval.write`——无写权限时整组不出现（只读行）；
 * 2. **启停**：`update_config` 的 `enabled` 翻转，可逆操作不弹确认；
 * 3. **删除**：ConfirmDialog 形态二次确认，成功 toast（连同被清掉的 pending 任务数）；
 * 4. **新建**：`ApprovalConfigDialog` 四步向导（坐标 → Code+控件预览 → 列+时区 → 提交）。
 *
 * 提交后一律**回读列表**（`update` / `delete` 的回执不包含最新记录，回读是必需的）。
 * 空态判定同数据源页：「total 为 0」才有资格说没有配置，当前页为空是页码越界。
 */

import { useEffect, useMemo, useState } from "react";
import { Plus, Search } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";

import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
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
import { useToast } from "@/shared/lib/toast";

import {
  feishuQueryKeys,
  useApprovalActions,
  useApprovalConfigList,
  useApprovalWizardClient,
} from "../api";
import { ApprovalConfigDialog } from "../components/ApprovalConfigDialog";
import { ApprovalConfigRowActions } from "../components/ApprovalConfigRowActions";
import { ApprovalWorkflowGuide } from "../components/ApprovalWorkflowGuide";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge } from "../components/StatusBadge";
import { useDebouncedValue } from "../list-query";
import type { ApprovalConfigItem } from "../types";
import { formatUnixSeconds } from "../types";

/// 删除确认的目标：主键 + 展示名（正文之外那行指认对象）。
type ConfirmTarget = {
  configId: number;
  title: string;
};

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

/// 映射明细展开行：widget_id / 类型 / 列名 / 必填 / 转换器。
function MapDetailRow({ item }: { item: ApprovalConfigItem }) {
  return (
    <TableRow data-testid={`maps-of-${item.id}`}>
      <TableCell colSpan={8} className="bg-muted/30 py-2">
        {item.maps.length === 0 ? (
          <p className="px-4 text-xs text-muted-foreground">
            这个配置还没有字段映射——它由首次派发自动装配，或回到向导重建。
          </p>
        ) : (
          <div className="px-4">
            <p className="pb-1 text-xs font-medium text-muted-foreground">
              字段映射（{item.maps.length} 条）
            </p>
            <table className="w-full text-xs">
              <thead>
                <tr className="text-left text-muted-foreground">
                  <th className="pb-1 pr-4 font-medium">控件</th>
                  <th className="pb-1 pr-4 font-medium">控件类型</th>
                  <th className="pb-1 pr-4 font-medium">多维表格列</th>
                  <th className="pb-1 pr-4 font-medium">必填</th>
                  <th className="pb-1 font-medium">转换器</th>
                </tr>
              </thead>
              <tbody>
                {item.maps.map((map) => (
                  <tr key={map.widgetId} className="border-t border-border/60">
                    <td className="py-1 pr-4 font-mono">{map.widgetId}</td>
                    <td className="py-1 pr-4">{map.widgetType}</td>
                    <td className="py-1 pr-4">
                      <span className="font-mono">{map.bitableField}</span>
                      {map.bitableFieldName ? (
                        <span className="ml-1 text-muted-foreground">
                          {map.bitableFieldName}
                        </span>
                      ) : null}
                    </td>
                    <td className="py-1 pr-4">
                      {map.required ? "必填" : "可选"}
                    </td>
                    <td className="py-1">{map.converter}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </TableCell>
    </TableRow>
  );
}

export default function ApprovalConfigsPage() {
  const queryClient = useQueryClient();
  const toast = useToast();
  const actions = useApprovalActions();
  const wizardClient = useApprovalWizardClient();

  const [search, setSearch] = useState("");
  const debouncedSearch = useDebouncedValue(search, 300);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(10);

  const [expandedId, setExpandedId] = useState<number | null>(null);
  const [mutatingId, setMutatingId] = useState<number | null>(null);
  const [confirmTarget, setConfirmTarget] = useState<ConfirmTarget | null>(
    null,
  );
  const [deletePending, setDeletePending] = useState(false);
  const [wizardOpen, setWizardOpen] = useState(false);

  const listQuery = useApprovalConfigList({
    page,
    pageSize,
    search: debouncedSearch,
  });

  const items = useMemo(() => listQuery.data?.items ?? [], [listQuery.data]);
  const total = listQuery.data?.total ?? null;
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  const settled =
    !listQuery.isPending && !listQuery.isError && !listQuery.isPlaceholderData;
  const emptyPage = settled && items.length === 0;
  const pageOutOfRange = emptyPage && lastPage !== null && page > lastPage;

  useEffect(() => {
    if (pageOutOfRange && lastPage !== null) setPage(lastPage);
  }, [pageOutOfRange, lastPage, setPage]);

  async function refreshList() {
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalConfigs(),
    });
  }

  /// 启停：`enabled` 翻转。可逆操作，不弹确认；失败回显后端原文。
  function toggleEnabled(item: ApprovalConfigItem) {
    setMutatingId(item.id);
    void (async () => {
      try {
        await actions.updateConfig(item.id, { enabled: !item.enabled });
        toast.success(item.enabled ? "已停用" : "已启用");
        await refreshList();
      } catch (cause) {
        toast.error(messageOf(cause));
      } finally {
        setMutatingId(null);
      }
    })();
  }

  function confirmDelete() {
    if (confirmTarget === null) return;
    const { configId, title } = confirmTarget;
    setDeletePending(true);
    void (async () => {
      try {
        const result = await actions.deleteConfig(configId);
        setConfirmTarget(null);
        toast.success(
          `已删除「${title}」${
            result.deletedPendingTasks > 0
              ? `，并清掉 ${result.deletedPendingTasks} 条待处理任务`
              : ""
          }`,
        );
        await refreshList();
      } catch (cause) {
        toast.error(messageOf(cause));
      } finally {
        setDeletePending(false);
      }
    })();
  }

  /// 向导提交成功：关向导、回读列表、toast。
  function submitWizard() {
    setWizardOpen(false);
    void (async () => {
      await refreshList();
      toast.success("配置创建成功");
    })();
  }

  if (!actions.canRead) {
    return (
      <div className="space-y-4 p-6">
        <h1 className="text-lg font-semibold">审批派发</h1>
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          当前身份没有查看审批派发配置的权限，请联系运维管理员开通。
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4 p-6">
      <div>
        <h1 className="text-lg font-semibold">审批派发</h1>
        <p className="text-sm text-muted-foreground">
          管理飞书审批派发配置：选好多维表格坐标与审批定义后，表格记录会按映射装配成审批单。
        </p>
      </div>

      <ApprovalWorkflowGuide />

      <div className="flex flex-wrap items-center gap-2">
        <div className="relative">
          <Search
            className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            value={search}
            onChange={(event) => {
              setSearch(event.target.value);
              setPage(1);
            }}
            placeholder="搜索配置名"
            aria-label="搜索配置"
            className="h-8 w-56 pl-7"
            autoComplete="off"
          />
        </div>
        {actions.canWrite ? (
          <Button
            size="sm"
            className="ml-auto"
            onClick={() => setWizardOpen(true)}
          >
            <Plus aria-hidden="true" />
            新建配置
          </Button>
        ) : null}
      </div>

      {listQuery.isPending && !settled ? (
        <p className="text-sm text-muted-foreground">加载中…</p>
      ) : listQuery.isError ? (
        <p className="text-sm text-destructive">{messageOf(listQuery.error)}</p>
      ) : items.length === 0 && total === 0 ? (
        <p className="text-sm text-muted-foreground">
          还没有审批派发配置。点「新建配置」走向导：选多维表格坐标 → 填审批 Code
          → 选申请人/回填列 + 时区 → 提交。
        </p>
      ) : (
        <div className="space-y-3">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>配置名</TableHead>
                <TableHead>坐标</TableHead>
                <TableHead>审批 Code</TableHead>
                <TableHead>时区</TableHead>
                <TableHead>启用</TableHead>
                <TableHead>快照时间</TableHead>
                <TableHead>更新时间</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item) => (
                <FragmentRow
                  key={item.id}
                  item={item}
                  expanded={expandedId === item.id}
                  pending={mutatingId === item.id}
                  canWrite={actions.canWrite}
                  onToggleExpanded={() =>
                    setExpandedId((current) =>
                      current === item.id ? null : item.id,
                    )
                  }
                  onToggleEnabled={toggleEnabled}
                  onRequestDelete={(target) => {
                    setConfirmTarget({
                      configId: target.id,
                      title: target.title,
                    });
                  }}
                />
              ))}
            </TableBody>
          </Table>
          <ListPagination
            page={page}
            pageSize={pageSize}
            total={total}
            pending={listQuery.isPending}
            onPageChange={setPage}
            onPageSizeChange={(next) => {
              setPageSize(next);
              setPage(1);
            }}
          />
        </div>
      )}

      <ApprovalConfigDialog
        open={wizardOpen}
        client={wizardClient}
        onCancel={() => setWizardOpen(false)}
        onSubmitted={submitWizard}
      />

      <Dialog
        open={confirmTarget !== null}
        onOpenChange={(next) => {
          if (!next && !deletePending) setConfirmTarget(null);
        }}
      >
        <DialogContent showCloseButton={!deletePending}>
          <DialogHeader>
            <DialogTitle>删除配置</DialogTitle>
            <DialogDescription>
              <span className="block">
                删除后该配置的字段映射与待处理任务会一并清除；已完结的审批单流水保留。
                删除不可恢复。
              </span>
              {confirmTarget ? (
                <span className="mt-2 block font-mono text-xs">
                  {confirmTarget.title}（#{confirmTarget.configId}）
                </span>
              ) : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => setConfirmTarget(null)}
              disabled={deletePending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={confirmDelete}
              disabled={deletePending}
            >
              {deletePending ? "删除中…" : "删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/// 一行 + 可选的映射展开行。抽成组件避免列表 map 里铺两行。
function FragmentRow({
  item,
  expanded,
  pending,
  canWrite,
  onToggleExpanded,
  onToggleEnabled,
  onRequestDelete,
}: {
  item: ApprovalConfigItem;
  expanded: boolean;
  pending: boolean;
  canWrite: boolean;
  onToggleExpanded: () => void;
  onToggleEnabled: (item: ApprovalConfigItem) => void;
  onRequestDelete: (item: ApprovalConfigItem) => void;
}) {
  return (
    <>
      <TableRow>
        <TableCell className="font-medium">{item.title}</TableCell>
        <TableCell className="font-mono text-xs">
          {item.baseToken} / {item.tableId}
        </TableCell>
        <TableCell className="font-mono text-xs">{item.approvalCode}</TableCell>
        <TableCell>{item.baseTimezone}</TableCell>
        <TableCell>
          {item.enabled ? (
            <StatusBadge tone="positive">启用</StatusBadge>
          ) : (
            <StatusBadge tone="neutral">已停用</StatusBadge>
          )}
        </TableCell>
        <TableCell className="tabular-nums">
          {formatUnixSeconds(item.formSnapshotAt ?? 0)}
        </TableCell>
        <TableCell className="tabular-nums">
          {formatUnixSeconds(item.updatedAt)}
        </TableCell>
        <TableCell>
          <ApprovalConfigRowActions
            item={item}
            canWrite={canWrite}
            pending={pending}
            expanded={expanded}
            onToggleExpanded={onToggleExpanded}
            onToggleEnabled={onToggleEnabled}
            onRequestDelete={onRequestDelete}
          />
        </TableCell>
      </TableRow>
      {expanded ? <MapDetailRow item={item} /> : null}
    </>
  );
}
