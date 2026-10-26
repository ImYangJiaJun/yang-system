import { useEffect, useState } from "react";

import { hasOperation, useSessionSnapshot, useUiCatalog } from "@/engine";
import { useToast } from "@/shared/lib/toast";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

import { issuePasswordReset, listUsers, type AdminUser } from "./admin-api";

const ISSUE_OPERATION = "account.admin_users.admin_issue_password_reset";

const STATUS_LABEL: Record<string, string> = {
  active: "启用",
  disabled: "停用",
  deleted: "已删除",
};

export default function UserAdminPage() {
  const session = useSessionSnapshot();
  const token = session.token || undefined;
  const catalog = useUiCatalog();
  const toast = useToast();
  const canIssue = hasOperation(catalog.data, ISSUE_OPERATION);

  const [users, setUsers] = useState<AdminUser[]>([]);
  const [resetToken, setResetToken] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    listUsers(token, 1, controller.signal)
      .then((rows) => {
        if (!cancelled) setUsers(rows);
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          toast.error(error instanceof Error ? error.message : "加载用户失败");
        }
      });
    return () => {
      cancelled = true;
      controller.abort();
    };
    // toast 身份不稳定，只在进入时拉一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token]);

  async function issue(userId: number) {
    try {
      const issued = await issuePasswordReset(token, userId);
      setResetToken(issued.resetToken);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "签发失败");
    }
  }

  return (
    <main className="mx-auto w-full max-w-6xl space-y-6 p-6">
      <h1 className="text-xl font-semibold">用户</h1>
      <table className="w-full text-sm">
        <thead>
          <tr className="text-left text-muted-foreground">
            <th className="py-2 font-medium">用户名</th>
            <th className="py-2 font-medium">邮箱</th>
            <th className="py-2 font-medium">状态</th>
            {canIssue ? <th className="py-2 font-medium" /> : null}
          </tr>
        </thead>
        <tbody>
          {users.map((user) => (
            <tr key={user.id} className="border-t border-border">
              <td className="py-2">{user.username}</td>
              <td className="py-2">{user.email ?? "未绑定"}</td>
              <td className="py-2">
                {STATUS_LABEL[user.status] ?? user.status}
              </td>
              {canIssue ? (
                <td className="py-2 text-right">
                  <Button type="button" onClick={() => void issue(user.id)}>
                    签发重置凭证
                  </Button>
                </td>
              ) : null}
            </tr>
          ))}
        </tbody>
      </table>

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
