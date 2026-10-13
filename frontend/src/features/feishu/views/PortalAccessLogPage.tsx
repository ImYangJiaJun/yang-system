/**
 * 门户访问日志查看（对话框形态）。
 *
 * 它挂在「查询门户」Tab 里，而不是一条独立路由：日志只有从某个门户进去才有意义，
 * 给它一条 URL 反而要多维护一份「门户 id → 数据源 id」的还原逻辑。
 *
 * 查询值**只显示哈希前缀**——后端本来就只存 SHA-256，这里再截断一次是为了让一列
 * 里放得下；完整值对排障没有额外价值（它本来就不可逆）。
 */

import { useState } from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
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

import { ListPagination } from "../components/ListPagination";
import { StatusBadge } from "../components/StatusBadge";
import type { ToneName } from "../components/StatusBadge";
import { usePortalAccessLog } from "../portal-api";
import type { AccessLogEntry } from "../types";
import { accessLogStatusLabel, formatUnixSeconds } from "../types";

const PAGE_SIZE = 20;

const STATUS_TONE: Record<AccessLogEntry["status"], ToneName> = {
  hit: "positive",
  miss: "neutral",
  rate_limited: "warning",
  forbidden: "danger",
};

export default function PortalAccessLogPage({
  portalId,
  portalTitle,
  onClose,
}: {
  portalId: number;
  portalTitle: string;
  onClose: () => void;
}) {
  const [page, setPage] = useState(1);
  const logQuery = usePortalAccessLog(portalId, page, PAGE_SIZE);
  const items = logQuery.data?.items ?? [];
  const total = logQuery.data?.total ?? null;

  return (
    <Dialog open onOpenChange={(next) => (next ? undefined : onClose())}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>访问日志 · {portalTitle}</DialogTitle>
          <DialogDescription>
            每次查询（命中与否、被限速、口令错误）都会记一条；查询值只存哈希。
          </DialogDescription>
        </DialogHeader>

        {logQuery.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : logQuery.isError ? (
          <p className="text-sm text-destructive">
            {logQuery.error instanceof Error
              ? logQuery.error.message
              : String(logQuery.error)}
          </p>
        ) : items.length === 0 ? (
          <p className="text-sm text-muted-foreground">还没有访问记录。</p>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>IP</TableHead>
                  <TableHead>查询值哈希</TableHead>
                  <TableHead>结果数</TableHead>
                  <TableHead>状态</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {items.map((entry) => (
                  <TableRow key={entry.id}>
                    <TableCell className="whitespace-nowrap">
                      {formatUnixSeconds(entry.created_at)}
                    </TableCell>
                    <TableCell>{entry.ip_address ?? "—"}</TableCell>
                    <TableCell className="font-mono text-xs">
                      {entry.query_value_hash.slice(0, 12) || "—"}
                    </TableCell>
                    <TableCell className="tabular-nums">
                      {entry.result_count}
                    </TableCell>
                    <TableCell>
                      <StatusBadge tone={STATUS_TONE[entry.status]}>
                        {accessLogStatusLabel(entry.status)}
                      </StatusBadge>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}

        <ListPagination
          page={page}
          pageSize={PAGE_SIZE}
          total={total}
          pending={logQuery.isPlaceholderData}
          onPageChange={setPage}
          onPageSizeChange={() => undefined}
        />
      </DialogContent>
    </Dialog>
  );
}
