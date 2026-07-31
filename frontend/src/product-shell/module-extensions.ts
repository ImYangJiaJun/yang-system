import { storeToRefs } from "pinia";
import { toValue, type MaybeRefOrGetter } from "vue";
import type { SessionContext } from "src/api/client";
import type { ActionDemoSchema } from "src/contracts/ui-catalog";
import { useTenantStore } from "stores/tenant";

export interface ProductModuleRowAction {
  id: string;
  label: string;
  disabled: boolean;
  execute: () => void;
}

interface ProductModuleRuntime {
  selectedOrganizationId?: number;
  selectOrganization: (row: Record<string, unknown>) => void;
  reloadOrganizations: () => Promise<void>;
}

interface ProductModuleExtension {
  rowActions?: (
    runtime: ProductModuleRuntime,
    row: Record<string, unknown>,
  ) => ProductModuleRowAction[];
  afterMutation?: (runtime: ProductModuleRuntime) => Promise<void>;
}

const productModuleExtensions: Readonly<
  Record<string, ProductModuleExtension>
> = Object.freeze({
  "org.tenant": {
    rowActions: (runtime, row) => {
      const selected = runtime.selectedOrganizationId === row.id;
      return [
        {
          id: "select-organization",
          label: selected ? "当前企业" : "进入企业",
          disabled: selected,
          execute: () => runtime.selectOrganization(row),
        },
      ];
    },
    afterMutation: (runtime) => runtime.reloadOrganizations(),
  },
});

export function useProductModuleExtensions(options: {
  actions: MaybeRefOrGetter<ActionDemoSchema[]>;
  session: MaybeRefOrGetter<SessionContext>;
}) {
  const tenantStore = useTenantStore();
  const { selectedOrganization } = storeToRefs(tenantStore);

  function runtime(): ProductModuleRuntime {
    return {
      selectedOrganizationId: selectedOrganization.value?.id,
      selectOrganization(row) {
        if (
          typeof row.id !== "number" ||
          typeof row.name !== "string" ||
          typeof row.code !== "string"
        ) {
          throw new Error("企业列表缺少 id、name 或 code");
        }
        tenantStore.selectOrganization({
          id: row.id,
          name: row.name,
          code: row.code,
        });
      },
      reloadOrganizations: () =>
        tenantStore.loadOrganizations(
          toValue(options.actions),
          toValue(options.session).token ?? "",
        ),
    };
  }

  function rowActions(
    moduleId: string,
    row: Record<string, unknown>,
  ): ProductModuleRowAction[] {
    return (
      productModuleExtensions[moduleId]?.rowActions?.(runtime(), row) ?? []
    );
  }

  async function afterMutation(moduleId: string) {
    await productModuleExtensions[moduleId]?.afterMutation?.(runtime());
  }

  return { rowActions, afterMutation };
}
