import { useCallback, useMemo, useRef, useState } from "react";

import { invokeAction } from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
} from "@/engine/contracts/ui-catalog";
import { handleInvocationAttachment } from "./attachment";
import {
  buildActionInitialValues,
  groupPresentedActions,
  type PresentedActionGroups,
  type SourceRow,
} from "../table/table-view-model";

/**
 * Action 执行 hook（旧 usePresentedActions 的 React 版）：
 * presentation 分组（toolbar/row/bulk + primary/secondary/overflow）、
 * 确认对话框、成功通知与刷新。Quasar Dialog/Notify 副作用替换为 React
 * 状态（确认对话框）与 notice 回调位。
 */

export type ActionNotice = {
  type: "positive" | "negative" | "warning";
  message: string;
};

export interface ActionDialogState {
  presentation: ActionPresentationSchema;
  action: ActionDemoSchema;
  initialValues: Record<string, unknown>;
}

interface UsePresentedActionsOptions {
  presentations: ActionPresentationSchema[];
  businessFields: FormFieldSchema[];
  actions: ActionDemoSchema[];
  selectedRows: SourceRow[];
  reload: () => void | Promise<unknown>;
  onCustom?: (presentation: ActionPresentationSchema, row?: SourceRow) => void;
  /// 副作用注入点（对齐旧 usePresentedActions）：默认真实下载/跳转，测试可替换。
  handleAttachment?: (
    result: import("@/engine/http/types").InvocationResult,
  ) => void;
  redirect?: (location: string) => void;
  /// 通知回调：成功/失败/警告消息。默认使用内部 notice 状态，可覆盖为 toast。
  onNotice?: (notice: ActionNotice) => void;
}

export function usePresentedActions(options: UsePresentedActionsOptions) {
  const session = useSessionCredentials();
  const [dialog, setDialog] = useState<ActionDialogState | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [notice, setNotice] = useState<ActionNotice | null>(null);
  const [confirmation, setConfirmation] =
    useState<ActionPresentationSchema | null>(null);
  const confirmationResolve = useRef<((ok: boolean) => void) | undefined>(
    undefined,
  );
  const abortRef = useRef<AbortController | undefined>(undefined);

  const actionById = useMemo(
    () =>
      new Map(options.actions.map((action) => [action.operation_id, action])),
    [options.actions],
  );

  const visible = useMemo(
    () =>
      options.presentations.filter(
        (item) => item.availability?.state !== "hidden",
      ),
    [options.presentations],
  );

  const groupsFor = useCallback(
    (placement: ActionPresentationSchema["placement"], directLimit: number) =>
      groupPresentedActions(
        visible.filter((item) => item.placement === placement),
        directLimit,
      ),
    [visible],
  );

  const toolbarActionGroups: PresentedActionGroups = groupsFor("toolbar", 2);
  const rowActionGroups: PresentedActionGroups = groupsFor("row", 1);
  const bulkActions = visible.filter((item) => item.placement === "bulk");
  const directToolbarActions = [
    ...(toolbarActionGroups.primary ? [toolbarActionGroups.primary] : []),
    ...toolbarActionGroups.secondary,
  ];

  const askConfirmation = (presentation: ActionPresentationSchema) =>
    new Promise<boolean>((resolve) => {
      confirmationResolve.current = resolve;
      setConfirmation(presentation);
    });

  const settleConfirmation = (ok: boolean) => {
    setConfirmation(null);
    confirmationResolve.current?.(ok);
    confirmationResolve.current = undefined;
  };

  const openAction = (
    presentation: ActionPresentationSchema,
    row?: SourceRow,
  ) => {
    if (presentation.interaction === "custom") {
      if (options.onCustom) options.onCustom(presentation, row);
      else
        setNotice({
          type: "warning",
          message: `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用模块页`,
        });
      return;
    }
    const action = actionById.get(presentation.operation_id);
    if (!action) {
      setNotice({
        type: "negative",
        message: `目录缺少 Action：${presentation.operation_id}`,
      });
      return;
    }
    const values = buildActionInitialValues(
      action,
      options.businessFields,
      row,
      presentation.record_parameter,
    );
    if (presentation.placement === "bulk") {
      values.selected = options.selectedRows;
    }
    if (presentation.interaction === "form") {
      setDialog({ presentation, action, initialValues: values });
      return;
    }
    void submit(presentation, action, values);
  };

  const submit = async (
    presentation: ActionPresentationSchema,
    action: ActionDemoSchema,
    values: Record<string, unknown>,
  ) => {
    if (submitting) return;
    setSubmitting(true);
    abortRef.current?.abort();
    const requestController = new AbortController();
    abortRef.current = requestController;
    try {
      if (presentation.confirmation && !(await askConfirmation(presentation))) {
        return;
      }
      const result = await invokeAction(
        action,
        values,
        session,
        requestController.signal,
      );
      (options.handleAttachment ?? handleInvocationAttachment)(result);
      if (result.kind === "redirect" && result.location) {
        (options.redirect ?? ((location) => window.location.assign(location)))(
          result.location,
        );
      }
      const notice: ActionNotice = {
        type: "positive",
        message: result.message || "操作成功",
      };
      if (options.onNotice) {
        options.onNotice(notice);
      } else {
        setNotice(notice);
      }
      setDialog(null);
      await options.reload();
    } catch (cause) {
      if (cause instanceof Error && cause.name === "AbortError") return;
      const notice: ActionNotice = {
        type: "negative",
        message: cause instanceof Error ? cause.message : String(cause),
      };
      if (options.onNotice) {
        options.onNotice(notice);
      } else {
        setNotice(notice);
      }
    } finally {
      if (abortRef.current === requestController) {
        abortRef.current = undefined;
        setSubmitting(false);
      }
    }
  };

  return {
    dialog,
    submitting,
    notice,
    confirmation,
    toolbarActionGroups,
    directToolbarActions,
    rowActionGroups,
    bulkActions,
    openAction,
    dismissNotice: () => setNotice(null),
    closeDialog: () => setDialog(null),
    settleConfirmation,
    submitDialog: (values: Record<string, unknown>) => {
      if (dialog) void submit(dialog.presentation, dialog.action, values);
    },
  };
}
