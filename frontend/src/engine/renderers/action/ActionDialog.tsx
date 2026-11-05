import { useId } from "react";

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
} from "@/engine/contracts/ui-catalog";
import { JsonSchemaForm } from "../form/JsonSchemaForm";
import type { ActionDialogState } from "./use-presented-actions";

/// Action 表单对话框（对齐旧 TableActionDialog）：标题、动态表单、取消/提交。
export function ActionDialog({
  state,
  businessFields,
  actions,
  submitting,
  submitLabel,
  onClose,
  onSubmit,
}: {
  state: ActionDialogState | null;
  businessFields: FormFieldSchema[];
  actions: ActionDemoSchema[];
  submitting: boolean;
  submitLabel?: string;
  onClose: () => void;
  onSubmit: (values: Record<string, unknown>) => void;
}) {
  const formId = useId();
  return (
    <Dialog
      open={Boolean(state)}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {state?.presentation.title || state?.action.title}
          </DialogTitle>
          {state?.action.description && (
            <DialogDescription>{state.action.description}</DialogDescription>
          )}
        </DialogHeader>
        {state && (
          <JsonSchemaForm
            key={`${state.action.operation_id}:${JSON.stringify(state.initialValues)}`}
            formId={formId}
            schema={state.action.input_schema}
            params={state.action.params}
            businessFields={businessFields}
            actions={actions}
            multipart={state.action.multipart}
            defaultValues={state.initialValues}
            onSubmit={onSubmit}
          />
        )}
        <DialogFooter>
          <Button variant="ghost" onClick={onClose} disabled={submitting}>
            取消
          </Button>
          <Button type="submit" form={formId} disabled={submitting}>
            {submitting ? "提交中…" : submitLabel || "提交"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 危险操作确认对话框（对齐旧 ActionConfirmationDialog 语义）。
///
/// 确认按钮的强调由 `appearance.emphasis` 决定：只有声明为 `danger` 的 Action
/// 才画成破坏性按钮。以前写死 destructive，可逆操作（停用/启用）也被画成
/// 「不可挽回」，业务页因此各自重写确认框（见 features/feishu/components/ConfirmDialog.tsx）。
export function ConfirmActionDialog({
  presentation,
  onSettle,
}: {
  presentation: ActionPresentationSchema | null;
  onSettle: (confirmed: boolean) => void;
}) {
  const danger = presentation?.appearance?.emphasis === "danger";
  return (
    <Dialog
      open={Boolean(presentation)}
      onOpenChange={(open) => {
        if (!open) onSettle(false);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{presentation?.confirmation?.title}</DialogTitle>
          <DialogDescription>
            {presentation?.confirmation?.message}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onSettle(false)}>
            取消
          </Button>
          <Button
            variant={danger ? "destructive" : "default"}
            onClick={() => onSettle(true)}
          >
            确认
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
