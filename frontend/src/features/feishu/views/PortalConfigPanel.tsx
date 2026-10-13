/**
 * 数据源详情页的「查询门户」Tab：列出该数据源下的门户，并做增删改启停。
 *
 * # 为什么门户列表自己取一次数据源
 *
 * 面板只吃 `datasourceId`（详情页也按主键路由），开放列与查询键的候选项来自
 * **该数据源的已启用字段绑定**，所以它借 `useDatasourceList` 的单条查询自己取一次。
 * 这条查询与详情页用的是**同一个 query key**（同样的 id 等值条件与排序），
 * 因此命中同一份缓存，不会多打一发。
 *
 * # 权限
 *
 * 「目录里有没有 `feishu.portal.*`」就是权限（与全站一致）：只读身份看不到创建与
 * 行内操作，只读的是渲染条件而不是 disabled——一个点了没反应的按钮比没有更糟。
 */

import { useMemo, useState } from "react";
import { Plus } from "lucide-react";

import { useUiCatalog } from "@/engine";
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";
import { copyText } from "@/shared/lib/clipboard";
import { useToast } from "@/shared/lib/toast";

import { useDatasourceList } from "../api";
import { StatusBadge } from "../components/StatusBadge";
import {
  canWritePortals,
  useInvalidatePortals,
  usePortalActions,
  usePortalList,
} from "../portal-api";
import type { Portal } from "../types";
import {
  formatUnixSeconds,
  portalAccessModeLabel,
  portalPathSlug,
} from "../types";
import PortalAccessLogPage from "./PortalAccessLogPage";
import PortalEditDialog from "./PortalEditDialog";

/// 公开链接的绝对地址。公开页在 `/portal/:slug`，与当前控制台同源。
function portalUrl(portal: Portal): string {
  return `${window.location.origin}/portal/${portalPathSlug(portal)}`;
}

export default function PortalConfigPanel({
  datasourceId,
}: {
  datasourceId: number;
}) {
  const toast = useToast();
  const catalog = useUiCatalog();
  const actions = usePortalActions();
  const invalidate = useInvalidatePortals();
  const canWrite = canWritePortals(catalog.data);

  const listQuery = usePortalList(datasourceId);
  const datasourceQuery = useDatasourceList(
    useMemo(
      () => ({
        page: 1,
        pageSize: 1,
        search: "",
        status: "all" as const,
        orderBy: [{ field: "id", direction: "Asc" as const }],
        id: datasourceId,
      }),
      [datasourceId],
    ),
  );
  const fields = (datasourceQuery.data?.items[0]?.fields ?? []).filter(
    (binding) => binding.enabled,
  );

  const [dialogOpen, setDialogOpen] = useState(false);
  const [editTarget, setEditTarget] = useState<Portal | undefined>(undefined);
  const [logTarget, setLogTarget] = useState<Portal | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<Portal | null>(null);
  const [busy, setBusy] = useState(false);

  const portals = listQuery.data?.items ?? [];

  async function refresh() {
    await invalidate(datasourceId);
  }

  async function toggle(portal: Portal) {
    setBusy(true);
    try {
      await actions.togglePortal(portal.id, !portal.enabled);
      await refresh();
      toast.success(portal.enabled ? "门户已停用" : "门户已启用");
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function remove(portal: Portal) {
    setBusy(true);
    try {
      await actions.deletePortal(portal.id);
      await refresh();
      toast.success("门户已删除");
      setConfirmDelete(null);
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function copyLink(portal: Portal) {
    const url = portalUrl(portal);
    const outcome = await copyText(url);
    if (outcome === "copied") toast.success("链接已复制");
    else toast.error(`复制失败，请手动复制：${url}`, { duration: 10_000 });
  }

  return (
    <section className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold">查询门户</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            门户是给外部人员的公开查询入口：只能按查询键精确匹配，只展示你勾选的开放列。
          </p>
        </div>
        {canWrite ? (
          <Button
            onClick={() => {
              setEditTarget(undefined);
              setDialogOpen(true);
            }}
          >
            <Plus />
            创建门户
          </Button>
        ) : null}
      </div>

      {listQuery.isPending ? (
        <div className="space-y-2">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      ) : listQuery.isError ? (
        <p className="text-sm text-destructive">
          {listQuery.error instanceof Error
            ? listQuery.error.message
            : String(listQuery.error)}
        </p>
      ) : portals.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
          这条数据源下还没有门户。
          {canWrite ? "点右上角「创建门户」开始。" : ""}
        </p>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>标题</TableHead>
                <TableHead>链接</TableHead>
                <TableHead>访问模式</TableHead>
                <TableHead>状态</TableHead>
                <TableHead>创建时间</TableHead>
                <TableHead>操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {portals.map((portal) => (
                <TableRow key={portal.id}>
                  <TableCell className="font-medium">{portal.title}</TableCell>
                  <TableCell className="font-mono text-xs">
                    /portal/{portalPathSlug(portal)}
                  </TableCell>
                  <TableCell>
                    {portalAccessModeLabel(portal.access_mode)}
                  </TableCell>
                  <TableCell>
                    <StatusBadge tone={portal.enabled ? "positive" : "neutral"}>
                      {portal.enabled ? "已启用" : "已停用"}
                    </StatusBadge>
                  </TableCell>
                  <TableCell className="whitespace-nowrap">
                    {formatUnixSeconds(portal.created_at)}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => copyLink(portal)}
                      >
                        复制链接
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => setLogTarget(portal)}
                      >
                        日志
                      </Button>
                      {canWrite ? (
                        <>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={busy}
                            onClick={() => {
                              setEditTarget(portal);
                              setDialogOpen(true);
                            }}
                          >
                            编辑
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={busy}
                            onClick={() => toggle(portal)}
                          >
                            {portal.enabled ? "停用" : "启用"}
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="text-destructive"
                            disabled={busy}
                            onClick={() => setConfirmDelete(portal)}
                          >
                            删除
                          </Button>
                        </>
                      ) : null}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      {dialogOpen ? (
        <PortalEditDialog
          open={dialogOpen}
          onOpenChange={setDialogOpen}
          datasourceId={datasourceId}
          fields={fields}
          portal={editTarget}
          actions={actions}
          onSaved={() => {
            void refresh();
            toast.success(editTarget ? "门户已更新" : "门户已创建");
          }}
        />
      ) : null}

      {logTarget !== null ? (
        <PortalAccessLogPage
          portalId={logTarget.id}
          portalTitle={logTarget.title}
          onClose={() => setLogTarget(null)}
        />
      ) : null}

      <Dialog
        open={confirmDelete !== null}
        onOpenChange={(next) => {
          if (!next) setConfirmDelete(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除门户</DialogTitle>
            <DialogDescription>
              删除后该公开链接立即失效（访问日志保留）。确定删除「
              {confirmDelete?.title}」？
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setConfirmDelete(null)}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => {
                if (confirmDelete) void remove(confirmDelete);
              }}
            >
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
