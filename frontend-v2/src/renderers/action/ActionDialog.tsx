import { useId } from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
} from "@/contracts/ui-catalog";
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
export function ConfirmActionDialog({
  presentation,
  onSettle,
}: {
  presentation: ActionPresentationSchema | null;
  onSettle: (confirmed: boolean) => void;
}) {
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
          <Button variant="destructive" onClick={() => onSettle(true)}>
            确认
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
