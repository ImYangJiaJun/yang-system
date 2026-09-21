/**
 * 停用 / 启用 / 删除的二次确认。
 *
 * 为什么自建而不用引擎的 `ConfirmActionDialog`：那个组件的确认按钮**写死了
 * destructive**。而「停用」是可逆的——它会立刻让飞书审批里正在用这个数据源的控件
 * 取不到选项，但数据源和选项都在，随时能再启用。把可逆操作画成破坏性操作，会让人
 * 以为自己在做一件不可挽回的事。所以这里 `tone` 可配：删除 destructive，停用/启用中性。
 *
 * 删除的正文**逐字**用后端原文（原 `datasource/mod.rs` 的 `ActionConfirmation`，
 * 摘掉 view 投影后由前端持有同一份文案），不得改写、不得拼接。
 * 指认被删对象靠的是正文**之外**的那行 `source_key`——三段文案都只说后果、不说对象，
 * 而删除不可逆，看不见删的是哪一条就等于闭着眼睛按下去。
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

export type ConfirmKind = "delete" | "disable" | "enable";

/// 后端原文，逐字。改一个字都算改契约。
const DELETE_MESSAGE = "删除后其下全部选项会被同时停用，且不可恢复。确认删除？";

const DISABLE_MESSAGE =
  "停用后，正在使用该数据源的飞书审批控件会立即取不到选项。数据源不会被删除，已推送的选项照旧保留，随时可以再启用。";

const ENABLE_MESSAGE = "启用后，正在使用该数据源的飞书审批控件会重新取到选项。";

type ConfirmCopy = {
  title: string;
  message: string;
  confirmLabel: string;
  tone: "destructive" | "neutral";
};

function confirmCopy(kind: ConfirmKind): ConfirmCopy {
  switch (kind) {
    case "delete":
      return {
        title: "删除数据源",
        message: DELETE_MESSAGE,
        confirmLabel: "删除",
        tone: "destructive",
      };
    case "disable":
      return {
        title: "停用数据源",
        message: DISABLE_MESSAGE,
        confirmLabel: "停用",
        tone: "neutral",
      };
    case "enable":
      return {
        title: "启用数据源",
        message: ENABLE_MESSAGE,
        confirmLabel: "启用",
        tone: "neutral",
      };
  }
}

export type ConfirmDialogProps = {
  open: boolean;
  kind: ConfirmKind;
  /// 被操作的数据源的标识：三个动作都要展示，用来指认对象。
  /// 它渲染在正文**之外**，所以正文仍然逐字是后端原文。
  sourceKey?: string;
  pending?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

export function ConfirmDialog({
  open,
  kind,
  sourceKey,
  pending = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const copy = confirmCopy(kind);

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) onCancel();
      }}
    >
      <DialogContent showCloseButton={!pending}>
        <DialogHeader>
          <DialogTitle>{copy.title}</DialogTitle>
          <DialogDescription>
            <span className="block">{copy.message}</span>
            {sourceKey ? (
              <span className="mt-2 block font-mono text-xs">{sourceKey}</span>
            ) : null}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={onCancel} disabled={pending}>
            取消
          </Button>
          <Button
            variant={copy.tone === "destructive" ? "destructive" : "default"}
            onClick={onConfirm}
            disabled={pending}
          >
            {pending ? "提交中…" : copy.confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
