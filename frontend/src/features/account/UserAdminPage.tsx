import { useCallback, useEffect, useState } from "react";

import { hasOperation, useSessionSnapshot, useUiCatalog } from "@/engine";
import { useToast } from "@/shared/lib/toast";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import {
  disableUser,
  enableUser,
  issuePasswordReset,
  listUsers,
  type AdminUser,
} from "./admin-api";

const ISSUE_OPERATION = "account.admin_users.admin_issue_password_reset";
const MANAGE_OPERATION = "account.admin_users.admin_disable_user";

const STATUS_LABEL: Record<string, string> = {
  active: "启用",
  disabled: "停用",
  deleted: "已删除",
};

const STATUS_VARIANT: Record<string, "default" | "secondary" | "destructive"> =
  {
    active: "default",
    disabled: "secondary",
    deleted: "destructive",
  };

const PAGE_SIZE = 20;

/// 待确认的账号状态变更：停用不可撤销（会撤销全部会话），启用可逆。
type PendingToggle = { user: AdminUser; action: "disable" | "enable" };

export default function UserAdminPage() {
  const session = useSessionSnapshot();
  const token = session.token || undefined;
  const catalog = useUiCatalog();
  const toast = useToast();
  const canIssue = hasOperation(catalog.data, ISSUE_OPERATION);
  const canManage = hasOperation(catalog.data, MANAGE_OPERATION);

  const [users, setUsers] = useState<AdminUser[]>([]);
  const [page, setPage] = useState(1);
  const [search, setSearch] = useState("");
  // 已应用的搜索词：输入框实时变化，但只在点「搜索」/回车后才重新取数。
  const [appliedSearch, setAppliedSearch] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [resetToken, setResetToken] = useState<string | null>(null);
  const [pendingToggle, setPendingToggle] = useState<PendingToggle | null>(
    null,
  );
  const [togglePending, setTogglePending] = useState(false);

  const reload = useCallback(
    (signal?: AbortSignal) => {
      setLoading(true);
      setError("");
      // 后端 list_users 只有 page/page_size，没有关键词参数；带搜索词时用
      // authenticated-only 的 /api/v1/users/lookup（同一个 UserView 投影）。
      return listUsers(token, page, appliedSearch, signal)
        .then((rows) => setUsers(rows))
        .catch((cause: unknown) => {
          if (cause instanceof Error && cause.name === "AbortError") return;
          setError(cause instanceof Error ? cause.message : "加载用户失败");
        })
        .finally(() => setLoading(false));
    },
    [token, page, appliedSearch],
  );

  useEffect(() => {
    const controller = new AbortController();
    void reload(controller.signal);
    return () => controller.abort();
  }, [reload]);

  async function issue(userId: number) {
    try {
      const issued = await issuePasswordReset(token, userId);
      setResetToken(issued.resetToken);
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : "签发失败");
    }
  }

  async function confirmToggle() {
    if (!pendingToggle || togglePending) return;
    setTogglePending(true);
    try {
      if (pendingToggle.action === "disable") {
        await disableUser(token, pendingToggle.user.id);
        toast.success(`已停用 ${pendingToggle.user.username}`);
      } else {
        await enableUser(token, pendingToggle.user.id);
        toast.success(`已启用 ${pendingToggle.user.username}`);
      }
      setPendingToggle(null);
      await reload();
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : "操作失败");
    } finally {
      setTogglePending(false);
    }
  }

  const issuedSearch = appliedSearch.trim();

  return (
    <main className="mx-auto w-full max-w-6xl space-y-6 p-6">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold">用户</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            查看账号、签发重置凭证，或停用/启用账号
          </p>
        </div>
        <form
          className="flex items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            setPage(1);
            setAppliedSearch(search);
          }}
        >
          <Input
            className="w-56"
            placeholder="搜索用户名或邮箱"
            aria-label="搜索用户名或邮箱"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
          <Button type="submit" variant="outline">
            搜索
          </Button>
          {issuedSearch && (
            <Button
              type="button"
              variant="ghost"
              onClick={() => {
                setSearch("");
                setPage(1);
                setAppliedSearch("");
              }}
            >
              清除
            </Button>
          )}
        </form>
      </header>

      {error && (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {error}
        </p>
      )}

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>用户名</TableHead>
            <TableHead>邮箱</TableHead>
            <TableHead>状态</TableHead>
            <TableHead className="text-right">操作</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {users.length === 0 ? (
            <TableRow>
              <TableCell
                colSpan={4}
                className="h-24 text-center text-muted-foreground"
              >
                {loading
                  ? "加载中…"
                  : issuedSearch
                    ? "没有匹配的账号"
                    : "暂无账号"}
              </TableCell>
            </TableRow>
          ) : (
            users.map((user) => (
              <TableRow key={user.id}>
                <TableCell>{user.username}</TableCell>
                <TableCell>{user.email ?? "未绑定"}</TableCell>
                <TableCell>
                  <Badge variant={STATUS_VARIANT[user.status] ?? "secondary"}>
                    {STATUS_LABEL[user.status] ?? user.status}
                  </Badge>
                </TableCell>
                <TableCell className="text-right">
                  <div className="flex justify-end gap-1">
                    {canIssue && (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={() => void issue(user.id)}
                      >
                        签发重置凭证
                      </Button>
                    )}
                    {canManage && user.status !== "deleted" && (
                      <Button
                        type="button"
                        variant={user.status === "active" ? "ghost" : "outline"}
                        size="sm"
                        onClick={() =>
                          setPendingToggle({
                            user,
                            action:
                              user.status === "active" ? "disable" : "enable",
                          })
                        }
                      >
                        {user.status === "active" ? "停用" : "启用"}
                      </Button>
                    )}
                  </div>
                </TableCell>
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>

      <footer className="flex items-center justify-end gap-2 text-sm">
        <span className="text-muted-foreground">第 {page} 页</span>
        <Button
          variant="outline"
          size="sm"
          disabled={page <= 1 || loading}
          onClick={() => setPage((current) => current - 1)}
        >
          上一页
        </Button>
        {/* 后端不分页返回总数，只能按「本页是否满员」判断有没有下一页。 */}
        <Button
          variant="outline"
          size="sm"
          disabled={loading || users.length < PAGE_SIZE}
          onClick={() => setPage((current) => current + 1)}
        >
          下一页
        </Button>
      </footer>

      <Dialog
        open={pendingToggle !== null}
        onOpenChange={(open) => {
          if (!open && !togglePending) setPendingToggle(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {pendingToggle?.action === "disable" ? "停用账号" : "启用账号"}
            </DialogTitle>
            <DialogDescription>
              {pendingToggle?.action === "disable"
                ? `停用后 ${pendingToggle?.user.username} 的全部会话立即失效，且无法登录，直到重新启用。`
                : `启用后 ${pendingToggle?.user.username} 可以重新登录。`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={togglePending}
              onClick={() => setPendingToggle(null)}
            >
              取消
            </Button>
            <Button
              variant={
                pendingToggle?.action === "disable" ? "destructive" : "default"
              }
              disabled={togglePending}
              onClick={() => void confirmToggle()}
            >
              {togglePending
                ? "提交中…"
                : pendingToggle?.action === "disable"
                  ? "停用"
                  : "启用"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={resetToken !== null}
        onOpenChange={(open) => {
          if (!open) setResetToken(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>重置凭证</DialogTitle>
            <DialogDescription>
              明文只显示这一次，关闭后不再保留。
            </DialogDescription>
          </DialogHeader>
          <p className="font-mono text-sm break-all">{resetToken}</p>
        </DialogContent>
      </Dialog>
    </main>
  );
}
