import type { InvocationResult } from "@/api/types";

/// 下载/预览附件处理：与旧 useTableActions 的 handleInvocationAttachment 语义一致。
export function handleInvocationAttachment(result: InvocationResult): void {
  if (!result.blobUrl) return;
  if (result.kind === "preview") {
    window.open(result.blobUrl, "_blank", "noopener,noreferrer");
    return;
  }
  const anchor = document.createElement("a");
  anchor.href = result.blobUrl;
  anchor.download = result.filename ?? "download";
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(result.blobUrl ?? ""), 0);
}
