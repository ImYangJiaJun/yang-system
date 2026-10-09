import { hasOperation } from "@/engine";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";
import { WORKSPACE_OPERATION_IDS } from "@/features/access/workspace-api";

export function canReadAccessWorkspace(
  catalog: UiCatalog | undefined,
): boolean {
  return [
    WORKSPACE_OPERATION_IDS.lookup,
    WORKSPACE_OPERATION_IDS.listUserGrants,
    WORKSPACE_OPERATION_IDS.listHolders,
  ].some((operationId) => hasOperation(catalog, operationId));
}
