/**
 * 撤销直授权限的二次确认（对齐 feishu 域 `ConfirmDialog` 的形状：目标指认行
 * 在正文之外、pending 期间关不掉；域间禁止互相 import，这里保留本域副本）。
 *
 * 撤销不可逆：行一撤销，目标用户立即失去该权限，重新生效只能再授予一次——
 * 所以正文之外必须看得见撤的是哪一条（权限字符串 + 目标用户）。
 */

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

export function RevokeConfirmDialog({
  open,
  target,
  pending = false,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  /// 被撤销对象的标识：权限字符串 + 目标用户（名称给人认，id 给两条同名时分）。
  target?: string;
  pending?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) onCancel();
      }}
    >
      <DialogContent showCloseButton={!pending}>
        <DialogHeader>
          <DialogTitle>撤销权限</DialogTitle>
          <DialogDescription>
            <span className="block">
              撤销后该用户立即失去此权限，且不可恢复；重新生效需要再次授予。
            </span>
            {target ? (
              <span className="mt-2 block font-mono text-xs">{target}</span>
            ) : null}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={onCancel} disabled={pending}>
            取消
          </Button>
          <Button variant="destructive" onClick={onConfirm} disabled={pending}>
            {pending ? "提交中…" : "确认撤销"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
