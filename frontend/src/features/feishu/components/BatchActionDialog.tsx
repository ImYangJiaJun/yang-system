import { useEffect, useState } from "react";

import { useToast } from "@/shared/lib/toast";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Label } from "@/shared/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import { Textarea } from "@/shared/ui/textarea";

import {
  BATCH_ACTION_LABELS,
  type BatchApprovalAction,
  type FeishuUser,
} from "../batch-approval-api";
import { FeishuUserPicker } from "./FeishuUserPicker";

export type BatchActionParams = {
  comment?: string;
  transferUser?: FeishuUser;
  addSignUsers?: FeishuUser[];
  addSignType?: number;
  approvalMethod?: number;
  rollbackMode?: "initiator" | "previous";
};

const ACTION_DESCRIPTION: Record<BatchApprovalAction, string> = {
  pass: "同意后审批流进入下一节点；请确认所选任务确实可以放行。",
  refuse: "拒绝后审批流终止，必须填写拒绝理由。",
  forward: "把当前任务转交给另一位飞书用户，原审批人不再持有该任务。",
  rollback: "把审批退回到发起人或上一节点；具体目标由服务端按实例解析。",
  add_sign: "为当前任务增加审批人。加签位置与审批方式的含义以飞书租户为准。",
};

/// 批量写操作的参数与二次确认。它本身就是确认对话框：动作、条数与目标都在
/// 提交按钮上方逐项列出，提交后才会创建 job。
export function BatchActionDialog({
  open,
  action,
  count,
  accessToken,
  submitting,
  errorMessage,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  action: BatchApprovalAction | null;
  count: number;
  accessToken: string | undefined;
  submitting: boolean;
  errorMessage: string;
  onCancel: () => void;
  onConfirm: (params: BatchActionParams) => void;
}) {
  const toast = useToast();
  const [comment, setComment] = useState("");
  const [transferUser, setTransferUser] = useState<FeishuUser | null>(null);
  const [addSignUsers, setAddSignUsers] = useState<FeishuUser[]>([]);
  const [addSignType, setAddSignType] = useState("1");
  const [approvalMethod, setApprovalMethod] = useState("1");
  const [rollbackMode, setRollbackMode] = useState<"initiator" | "previous">(
    "initiator",
  );

  useEffect(() => {
    if (!open) return;
    setComment("");
    setTransferUser(null);
    setAddSignUsers([]);
    setAddSignType("1");
    setApprovalMethod("1");
    setRollbackMode("initiator");
  }, [open, action]);

  if (!action) return null;

  const label = BATCH_ACTION_LABELS[action];
  const commentRequired = action === "refuse";
  const commentMissing = commentRequired && !comment.trim();
  const transferMissing = action === "forward" && transferUser === null;
  const addSignMissing =
    action === "add_sign" &&
    (addSignUsers.length === 0 || addSignUsers.length > 10);
  const valid = !commentMissing && !transferMissing && !addSignMissing;

  const confirm = () => {
    if (!valid) {
      toast.error(
        commentMissing
          ? "拒绝必须填写理由"
          : transferMissing
            ? "请选择转交目标"
            : "加签需要选择 1–10 位审批人",
      );
      return;
    }
    onConfirm({
      comment: comment.trim() || undefined,
      ...(action === "forward" && transferUser ? { transferUser } : {}),
      ...(action === "add_sign"
        ? {
            addSignUsers,
            addSignType: Number(addSignType),
            approvalMethod: Number(approvalMethod),
          }
        : {}),
      ...(action === "rollback" ? { rollbackMode } : {}),
    });
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen && !submitting) onCancel();
      }}
    >
      <DialogContent aria-label={`${label}确认`} showCloseButton={!submitting}>
        <DialogHeader>
          <DialogTitle>
            {label} {count} 条审批
          </DialogTitle>
          <DialogDescription>{ACTION_DESCRIPTION[action]}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-2">
          <p className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm">
            即将对 <strong>{count}</strong> 条待办执行「{label}
            」。提交后由后台逐条 执行，结果在进度对话框内查看。
          </p>

          {(action === "forward" || action === "add_sign") && (
            <FeishuUserPicker
              label={action === "forward" ? "转交目标" : "加签审批人"}
              accessToken={accessToken}
              value={
                action === "forward"
                  ? transferUser
                    ? [transferUser]
                    : []
                  : addSignUsers
              }
              onChange={(users) => {
                if (action === "forward") {
                  setTransferUser(users[0] ?? null);
                } else {
                  setAddSignUsers(users.slice(0, 10));
                }
              }}
              multiple={action === "add_sign"}
              disabled={submitting}
            />
          )}

          {action === "add_sign" && (
            <>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <Label htmlFor="batch-add-sign-type">加签位置</Label>
                  <Select
                    value={addSignType}
                    onValueChange={setAddSignType}
                    disabled={submitting}
                  >
                    <SelectTrigger id="batch-add-sign-type" className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="1">位置 1</SelectItem>
                      <SelectItem value="2">位置 2</SelectItem>
                      <SelectItem value="3">位置 3</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="batch-approval-method">审批方式</Label>
                  <Select
                    value={approvalMethod}
                    onValueChange={setApprovalMethod}
                    disabled={submitting}
                  >
                    <SelectTrigger
                      id="batch-approval-method"
                      className="w-full"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="1">方式 1</SelectItem>
                      <SelectItem value="2">方式 2</SelectItem>
                      <SelectItem value="3">方式 3</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                飞书用户态接口的 1/2/3
                枚举含义在不同文档版本里不一致，最终以租户
                实际行为为准；选择不确定时请先在飞书内单独处理一单验证。
              </p>
            </>
          )}

          {action === "rollback" && (
            <div className="space-y-1.5">
              <Label htmlFor="batch-rollback-mode">退回目标</Label>
              <Select
                value={rollbackMode}
                onValueChange={(value) =>
                  setRollbackMode(value as "initiator" | "previous")
                }
                disabled={submitting}
              >
                <SelectTrigger id="batch-rollback-mode" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="initiator">退回发起人</SelectItem>
                  <SelectItem value="previous">退回上一节点</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                批量退回只支持这两种目标；服务端无法从实例详情唯一确定目标节点时，
                该条会失败并提示单独处理。
              </p>
            </div>
          )}

          <div className="space-y-1.5">
            <Label htmlFor="batch-action-comment">
              审批意见{commentRequired ? "（必填）" : "（选填）"}
            </Label>
            <Textarea
              id="batch-action-comment"
              rows={3}
              maxLength={500}
              disabled={submitting}
              value={comment}
              onChange={(event) => setComment(event.target.value)}
            />
          </div>

          {errorMessage && (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {errorMessage}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            disabled={submitting}
            onClick={onCancel}
          >
            取消
          </Button>
          <Button
            type="button"
            variant={action === "refuse" ? "destructive" : "default"}
            disabled={submitting || !valid}
            onClick={confirm}
          >
            {submitting ? "提交中…" : `确认${label}`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
