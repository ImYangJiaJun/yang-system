import {
  computed,
  onScopeDispose,
  ref,
  toValue,
  type MaybeRefOrGetter,
} from "vue";
import { Dialog, Notify } from "quasar";
import {
  invokeAction,
  StepUpRequiredError,
  type InvocationResult,
  type SessionContext,
} from "src/api/client";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import {
  buildActionInitialValues,
  groupPresentedActions,
} from "../table-view-model";
import ActionConfirmationDialog from "../ActionConfirmationDialog.vue";
import { requestStepUpProof } from "src/components/step-up/requestStepUpProof";

interface UsePresentedActionsOptions {
  presentations: MaybeRefOrGetter<ActionPresentationSchema[]>;
  businessFields: MaybeRefOrGetter<FormFieldSchema[]>;
  actions: MaybeRefOrGetter<ActionDemoSchema[]>;
  session: MaybeRefOrGetter<SessionContext>;
  selectedRows: MaybeRefOrGetter<Array<Record<string, unknown>>>;
  reload: () => Promise<void>;
  emitCustom: (
    presentation: ActionPresentationSchema,
    row?: Record<string, unknown>,
  ) => void;
  invoke?: typeof invokeAction;
  confirm?: (presentation: ActionPresentationSchema) => Promise<boolean>;
  notify?: (type: "positive" | "negative", message: string) => void;
  handleAttachment?: (result: InvocationResult) => void;
  redirect?: (location: string) => void;
  reauthenticate?: (
    challenge: string,
    session: SessionContext,
  ) => Promise<string | undefined>;
}

interface UseTableActionsOptions extends Omit<
  UsePresentedActionsOptions,
  "presentations" | "businessFields"
> {
  view: MaybeRefOrGetter<TableViewSchema>;
}

export function usePresentedActions(options: UsePresentedActionsOptions) {
  const invoke = options.invoke ?? invokeAction;
  const confirm = options.confirm ?? confirmAction;
  const notify =
    options.notify ??
    ((type: "positive" | "negative", message: string) =>
      Notify.create({ type, message }));
  const handleAttachment =
    options.handleAttachment ?? handleInvocationAttachment;
  const redirect =
    options.redirect ??
    ((location: string) => window.location.assign(location));
  const reauthenticate = options.reauthenticate ?? requestStepUpProof;
  const actionDialog = ref(false);
  const actionLoading = ref(false);
  const activePresentation = ref<ActionPresentationSchema>();
  const activeAction = ref<ActionDemoSchema>();
  const actionValues = ref<Record<string, unknown>>({});
  let controller: AbortController | undefined;

  const actionById = computed(
    () =>
      new Map(
        toValue(options.actions).map((action) => [action.operation_id, action]),
      ),
  );
  const presentations = computed(() =>
    toValue(options.presentations).filter(
      (item) => item.availability?.state !== "hidden",
    ),
  );
  const toolbarActions = computed(() =>
    presentations.value.filter((item) => item.placement === "toolbar"),
  );
  const rowActions = computed(() =>
    presentations.value.filter((item) => item.placement === "row"),
  );
  const bulkActions = computed(() =>
    presentations.value.filter((item) => item.placement === "bulk"),
  );
  const toolbarActionGroups = computed(() =>
    groupPresentedActions(toolbarActions.value, 2),
  );
  const directToolbarActions = computed(() => [
    ...(toolbarActionGroups.value.primary
      ? [toolbarActionGroups.value.primary]
      : []),
    ...toolbarActionGroups.value.secondary,
  ]);
  const rowActionGroups = computed(() =>
    groupPresentedActions(rowActions.value, 1),
  );
  const directRowActions = computed(() =>
    rowActionGroups.value.primary ? [rowActionGroups.value.primary] : [],
  );

  async function openAction(
    presentation: ActionPresentationSchema,
    row?: Record<string, unknown>,
  ) {
    if (presentation.interaction === "custom") {
      options.emitCustom(presentation, row);
      return;
    }
    const action = actionById.value.get(presentation.operation_id);
    if (!action) {
      notify("negative", `目录缺少 Action：${presentation.operation_id}`);
      return;
    }
    activePresentation.value = presentation;
    activeAction.value = action;
    actionValues.value = buildActionInitialValues(
      action,
      toValue(options.businessFields),
      row,
      presentation.record_parameter,
    );
    if (presentation.placement === "bulk") {
      actionValues.value.selected = toValue(options.selectedRows);
    }
    if (presentation.interaction === "form") {
      actionDialog.value = true;
      return;
    }
    await submitAction();
  }

  async function submitAction() {
    const action = activeAction.value;
    const presentation = activePresentation.value;
    if (!action || !presentation || actionLoading.value) return;
    actionLoading.value = true;
    controller?.abort();
    const requestController = new AbortController();
    controller = requestController;
    try {
      if (!(await confirm(presentation))) return;
      const session = { ...toValue(options.session) };
      let result: InvocationResult;
      try {
        result = await invoke(
          action,
          actionValues.value,
          session,
          requestController.signal,
        );
      } catch (cause) {
        if (!(cause instanceof StepUpRequiredError)) throw cause;
        const proof = await reauthenticate(cause.challenge, session);
        if (!proof) return;
        result = await invoke(
          action,
          actionValues.value,
          session,
          requestController.signal,
          { stepUpProof: proof },
        );
      }
      handleAttachment(result);
      if (result.kind === "redirect" && result.location) {
        redirect(result.location);
      }
      notify("positive", result.message || "操作成功");
      actionDialog.value = false;
      await options.reload();
    } catch (cause) {
      if (cause instanceof Error && cause.name === "AbortError") return;
      notify(
        "negative",
        cause instanceof Error ? cause.message : String(cause),
      );
    } finally {
      if (controller === requestController) {
        controller = undefined;
        actionLoading.value = false;
      }
    }
  }

  function dispose() {
    controller?.abort();
    controller = undefined;
  }

  onScopeDispose(dispose);

  return {
    actionDialog,
    actionLoading,
    activePresentation,
    activeAction,
    actionValues,
    toolbarActions,
    rowActions,
    bulkActions,
    toolbarActionGroups,
    directToolbarActions,
    rowActionGroups,
    directRowActions,
    openAction,
    submitAction,
    dispose,
  };
}

export function useTableActions(options: UseTableActionsOptions) {
  return usePresentedActions({
    presentations: () => toValue(options.view).action_presentations,
    businessFields: () => toValue(options.view).form.fields,
    actions: options.actions,
    session: options.session,
    selectedRows: options.selectedRows,
    reload: options.reload,
    emitCustom: options.emitCustom,
    invoke: options.invoke,
    confirm: options.confirm,
    notify: options.notify,
    handleAttachment: options.handleAttachment,
    redirect: options.redirect,
  });
}

function confirmAction(
  presentation: ActionPresentationSchema,
): Promise<boolean> {
  if (!presentation.confirmation) return Promise.resolve(true);
  return new Promise((resolve) => {
    Dialog.create({
      component: ActionConfirmationDialog,
      componentProps: {
        title: presentation.confirmation?.title,
        message: presentation.confirmation?.message ?? "",
      },
    })
      .onOk(() => resolve(true))
      .onCancel(() => resolve(false))
      .onDismiss(() => resolve(false));
  });
}

function handleInvocationAttachment(result: InvocationResult) {
  if (!result.blobUrl) return;
  if (result.kind === "preview") {
    window.open(result.blobUrl, "_blank", "noopener,noreferrer");
    return;
  }
  const anchor = document.createElement("a");
  anchor.href = result.blobUrl;
  anchor.download = result.filename ?? "download";
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(result.blobUrl!), 0);
}
