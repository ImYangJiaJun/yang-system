import { useEffect, useState } from "react";

import type { SessionContext } from "@/engine/http/types";
import { StepUpDialog, type StepUpRequest } from "./StepUpDialog";

/// 与 SessionControllerOptions.requestStepUpProof 同形的 UI 回调签名。
export type StepUpProofHandler = (
  challenge: string,
  session: SessionContext,
) => Promise<string | undefined>;

type PendingRequest = StepUpRequest & {
  resolve: (proof: string | undefined) => void;
};

/**
 * Step-up 对话框宿主：把自己注册为 SessionController 的 requestStepUpProof
 * 回调（App 经 delegate ref 接线），一次只处理一个 challenge。
 */
export function StepUpDialogHost({
  onReady,
}: {
  onReady: (handler: StepUpProofHandler) => void;
}) {
  const [pending, setPending] = useState<PendingRequest | null>(null);

  useEffect(() => {
    onReady(
      (challenge, session) =>
        new Promise<string | undefined>((resolve) => {
          setPending({ challenge, session, resolve });
        }),
    );
  }, [onReady]);

  return (
    <StepUpDialog
      request={pending}
      onResolve={(proof) => {
        pending?.resolve(proof);
        setPending(null);
      }}
    />
  );
}
