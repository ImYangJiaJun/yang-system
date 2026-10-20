import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Plus } from "lucide-react";
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
import { ApprovalConfigDetails } from "../components/ApprovalConfigDetails";
import { FeishuOAuthCard } from "../components/FeishuOAuthCard";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge } from "../components/StatusBadge";
import { PAGE_SIZE_OPTIONS, useDebouncedValue } from "../list-query";
import type { ApprovalConfigItem, ApprovalConfigQuery } from "../types";

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

export default function ApprovalConfigsPage() {
  const [params, setParams] = useSearchParams();
  const queryClient = useQueryClient();
  const toast = useToast();
  const actions = useApprovalActions();
  const wizardClient = useApprovalWizardClient();
  const container = useRef<HTMLDivElement>(null);
  const listScroll = useRef(0);
  const restoreScroll = useRef(false);
  const rawId = params.get("config");
  const configId = rawId === null ? undefined : Number(rawId);
  const validId =
    configId !== undefined && Number.isSafeInteger(configId) && configId > 0;
  const pageValue = Number(params.get("page") ?? 1);
  const page = Number.isSafeInteger(pageValue) && pageValue > 0 ? pageValue : 1;
  const sizeValue = Number(params.get("pageSize") ?? 10);
  const pageSize = PAGE_SIZE_OPTIONS.some((size) => size === sizeValue)
    ? sizeValue
    : 10;
  const search = params.get("search") ?? "";
  const debouncedSearch = useDebouncedValue(search, 300);
  const status: ApprovalConfigQuery["status"] =
    params.get("status") === "enabled"
      ? "enabled"
      : params.get("status") === "disabled"
        ? "disabled"
        : "all";
  const [wizardOpen, setWizardOpen] = useState(false);
  const [editTarget, setEditTarget] = useState<ApprovalConfigItem>();
  const [confirm, setConfirm] = useState<{
    item: ApprovalConfigItem;
    kind: "delete" | "toggle";
  } | null>(null);
  const [pending, setPending] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  const listQuery = useApprovalConfigList(
    { page, pageSize, search: debouncedSearch, status },
    { enabled: rawId === null },
  );
  const detailQuery = useApprovalConfigList(
    { page: 1, pageSize: 1, search: "", configId: validId ? configId : 0 },
    { enabled: validId },
  );
  const detail = detailQuery.data?.items.find((item) => item.id === configId);
  const items = listQuery.data?.items ?? [];
  const total = listQuery.data?.total ?? null;
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  function changeParams(patch: Record<string, string | null>, replace = false) {
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        for (const [key, value] of Object.entries(patch)) {
          if (value === null) next.delete(key);
          else next.set(key, value);
        }
        return next;
      },
      { replace },
    );
  }
  useEffect(() => {
    if (
      rawId === null &&
      !listQuery.isPending &&
      !listQuery.isError &&
      !listQuery.isPlaceholderData &&
      lastPage !== null &&
      page > lastPage
    ) {
      setParams(
        (current) => {
          const next = new URLSearchParams(current);
          next.set("page", String(lastPage));
          return next;
        },
        { replace: true },
      );
    }
  }, [
    rawId,
    listQuery.isPending,
    listQuery.isError,
    listQuery.isPlaceholderData,
    lastPage,
    page,
    setParams,
  ]);
  useEffect(() => {
    if (rawId === null && restoreScroll.current && !listQuery.isPending) {
      const main = container.current?.closest("main");
      if (main) main.scrollTop = listScroll.current;
      restoreScroll.current = false;
    }
  }, [rawId, listQuery.isPending]);
  function openDetail(id: number) {
    if (rawId === null) {
      listScroll.current = container.current?.closest("main")?.scrollTop ?? 0;
      restoreScroll.current = true;
    }
    changeParams({ config: String(id) });
    const main = container.current?.closest("main");
    if (main) main.scrollTop = 0;
  }
  function back() {
    restoreScroll.current = true;
    changeParams({ config: null });
  }
  async function refreshList() {
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalConfigs(),
    });
  }
  async function confirmAction() {
    if (!confirm) return;
    setPending(true);
    try {
      const { item, kind } = confirm;
      if (kind === "delete") {
        const result = await actions.deleteConfig(item.id);
        toast.success(
          `已删除「${item.title}」${
            result.deletedTasks > 0
              ? `，并清掉 ${result.deletedTasks} 条任务行`
              : ""
          }；派发请求记录保留`,
        );
        back();
      } else {
        await actions.updateConfig(item.id, { enabled: !item.enabled });
        toast.success(item.enabled ? "已停用" : "已启用");
      }
      setConfirm(null);
      await refreshList();
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setPending(false);
    }
  }
  async function refreshNames(ids: number[]) {
    setRefreshing(true);
    try {
      let incomplete = 0;
      for (let offset = 0; offset < ids.length; offset += 10)
        incomplete += (
          await actions.refreshNames(ids.slice(offset, offset + 10))
        ).length;
      if (incomplete)
        toast.error(
          `${incomplete} 条配置的名称未补充完整，已有名称保留；请核对飞书访问权限。`,
        );
      else toast.success("名称已补充；展示名称不代表工作流已接通。");
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      await refreshList();
      setRefreshing(false);
    }
  }
  function submitWizard() {
    setWizardOpen(false);
    toast.success(editTarget ? "配置更新成功" : "配置创建成功");
    void refreshList();
  }
  const filtered = search.trim() !== "" || status !== "all";
  if (!actions.canRead)
    return (
      <p className="p-6">当前身份没有查看审批派发配置的权限，请联系管理员。</p>
    );

  return (
    <div
      ref={container}
      className="mx-auto max-w-6xl space-y-6 p-4 sm:p-6 lg:p-8"
    >
      {rawId !== null ? (
        <>
          <Button variant="ghost" onClick={back}>
            <span aria-hidden="true">←</span> 返回配置列表
          </Button>
          {!validId ? (
            <p>无法找到这条配置</p>
          ) : detailQuery.isPending ? (
            <p role="status">正在读取配置…</p>
          ) : detailQuery.isError ? (
            <div role="alert">
              <p>暂时无法读取配置：{messageOf(detailQuery.error)}</p>
              <Button
                variant="outline"
                onClick={() => void detailQuery.refetch()}
              >
                重新加载
              </Button>
            </div>
          ) : !detail ? (
            <p>无法找到这条配置</p>
          ) : (
            <ApprovalConfigDetails
              key={detail.id}
              item={detail}
              canWrite={actions.canWrite}
              pending={pending || refreshing}
              onEdit={() => {
                setEditTarget(detail);
                setWizardOpen(true);
              }}
              onToggle={() => setConfirm({ item: detail, kind: "toggle" })}
              onDelete={() => setConfirm({ item: detail, kind: "delete" })}
              onRefreshNames={() => void refreshNames([detail.id])}
              onOpenConfig={openDetail}
              onRequests={() => changeParams({ tab: "requests" })}
            />
          )}
        </>
      ) : (
        <>
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <h1 className="text-2xl font-semibold tracking-tight">
                审批配置
              </h1>
              <p className="mt-2 text-sm text-muted-foreground">
                看清每张表发起什么审批，以及数据如何填写。
              </p>
            </div>
            {actions.canWrite && (
              <Button
                onClick={() => {
                  setEditTarget(undefined);
                  setWizardOpen(true);
                }}
              >
                <Plus aria-hidden="true" />
                新建配置
              </Button>
            )}
          </div>
          <FeishuOAuthCard />
          <div className="flex flex-wrap items-center gap-3">
            <div className="flex gap-1" aria-label="配置状态筛选">
              {(
                [
                  { value: "all", label: "全部" },
                  { value: "enabled", label: "已启用" },
                  { value: "disabled", label: "已停用" },
                ] as const
              ).map((option) => (
                <Button
                  key={option.value}
                  size="sm"
                  variant={status === option.value ? "default" : "ghost"}
                  aria-pressed={status === option.value}
                  onClick={() =>
                    changeParams(
                      {
                        status: option.value === "all" ? null : option.value,
                        page: "1",
                      },
                      true,
                    )
                  }
                >
                  {option.label}
                </Button>
              ))}
            </div>
            <Input
              className="w-full sm:ml-auto sm:w-72"
              aria-label="搜索配置"
              placeholder="搜索配置、数据表或审批"
              value={search}
              onChange={(event) =>
                changeParams(
                  { search: event.target.value || null, page: "1" },
                  true,
                )
              }
            />
            {actions.canWrite && items.length > 0 && (
              <Button
                size="sm"
                variant="outline"
                disabled={refreshing || listQuery.isFetching}
                onClick={() => void refreshNames(items.map((item) => item.id))}
              >
                {refreshing ? "名称补充中…" : "补充本页名称"}
              </Button>
            )}
          </div>
          {listQuery.isPending ? (
            <p role="status">正在读取配置…</p>
          ) : listQuery.isError ? (
            <div role="alert">
              <p>暂时无法读取配置：{messageOf(listQuery.error)}</p>
              <Button
                variant="outline"
                onClick={() => void listQuery.refetch()}
              >
                重新加载
              </Button>
            </div>
          ) : items.length === 0 && total === 0 ? (
            <div className="rounded-lg border p-8 text-center">
              <p className="font-medium">
                {filtered ? "没有符合条件的配置" : "还没有审批配置"}
              </p>
              <p className="mt-2 text-sm text-muted-foreground">
                {filtered
                  ? "尝试更换关键词或清除筛选。"
                  : "新建配置：选择来源数据表和审批，再设置申请人、编号写回列和字段对应。"}
              </p>
              {filtered && (
                <Button
                  className="mt-4"
                  variant="outline"
                  onClick={() =>
                    changeParams(
                      { search: null, status: null, page: "1" },
                      true,
                    )
                  }
                >
                  清除筛选
                </Button>
              )}
            </div>
          ) : (
            <div className="space-y-4">
              <div className="overflow-hidden rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>配置名称</TableHead>
                      <TableHead>来源数据表</TableHead>
                      <TableHead>发起的审批</TableHead>
                      <TableHead>配置状态</TableHead>
                      <TableHead className="text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {items.map((item) => (
                      <TableRow key={item.id}>
                        <TableCell>
                          <button
                            className="text-left font-medium underline-offset-4 hover:underline focus-visible:outline-ring"
                            onClick={() => openDetail(item.id)}
                          >
                            {item.title}
                          </button>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {item.maps.length} 项字段对应
                          </p>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <p>{item.baseName?.trim() || "表格名称待补充"}</p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {item.tableName?.trim() || "数据表名称待补充"}
                          </p>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          {item.approvalName?.trim() || "审批名称待补充"}
                        </TableCell>
                        <TableCell>
                          <StatusBadge
                            tone={item.enabled ? "positive" : "neutral"}
                          >
                            {item.enabled ? "已启用" : "已停用"}
                          </StatusBadge>
                        </TableCell>
                        <TableCell className="text-right">
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => openDetail(item.id)}
                          >
                            查看配置
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
              <ListPagination
                page={page}
                pageSize={pageSize}
                total={total}
                pending={listQuery.isFetching}
                onPageChange={(value) => changeParams({ page: String(value) })}
                onPageSizeChange={(value) =>
                  changeParams({ pageSize: String(value), page: "1" })
                }
              />
            </div>
          )}
          <p className="text-xs text-muted-foreground">
            已启用仅表示可受理派发请求；工作流设置和运行结果请在飞书及派发记录中核对。
          </p>
        </>
      )}
      {wizardOpen && (
        <ApprovalConfigDialog
          open
          client={wizardClient}
          initialConfig={editTarget}
          onCancel={() => setWizardOpen(false)}
          onSubmitted={submitWizard}
        />
      )}
      <Dialog
        open={confirm !== null}
        onOpenChange={(open) => {
          if (!open && !pending) setConfirm(null);
        }}
      >
        <DialogContent showCloseButton={!pending}>
          <DialogHeader>
            <DialogTitle>
              {confirm?.kind === "delete"
                ? "删除配置"
                : confirm?.item.enabled
                  ? "停用配置"
                  : "启用配置"}
            </DialogTitle>
            <DialogDescription>
              <span className="block">
                {confirm?.kind === "delete"
                  ? "删除后该配置的字段映射与全部任务行（含已完结）会一并清除；仅派发请求记录保留。删除不可恢复。"
                  : confirm?.item.enabled
                    ? "停用后，新的派发请求将不再通过此配置受理。"
                    : "启用后，此配置可受理新的派发请求；仍需飞书工作流触发。"}
              </span>
              {confirm && (
                <span className="mt-2 block">
                  {confirm.item.title}（#{confirm.item.id}）
                </span>
              )}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={pending}
              onClick={() => setConfirm(null)}
            >
              取消
            </Button>
            <Button
              variant={confirm?.kind === "delete" ? "destructive" : "default"}
              disabled={pending}
              onClick={() => void confirmAction()}
            >
              {pending
                ? "处理中…"
                : confirm?.kind === "delete"
                  ? "删除"
                  : confirm?.item.enabled
                    ? "确认停用"
                    : "确认启用"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
