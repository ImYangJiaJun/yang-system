import { Dialog } from "quasar";
import type { SessionContext } from "src/api/client";
import StepUpDialog from "src/components/table/StepUpDialog.vue";

export function requestStepUpProof(
  challenge: string,
  session: SessionContext,
): Promise<string | undefined> {
  return new Promise((resolve) => {
    Dialog.create({
      component: StepUpDialog,
      componentProps: { challenge, session },
    })
      .onOk((proof: unknown) =>
        resolve(typeof proof === "string" && proof ? proof : undefined),
      )
      .onCancel(() => resolve(undefined))
      .onDismiss(() => resolve(undefined));
  });
}
