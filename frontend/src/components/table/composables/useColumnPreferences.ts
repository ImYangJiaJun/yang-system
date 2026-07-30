import {
  computed,
  onScopeDispose,
  ref,
  toValue,
  watch,
  type MaybeRefOrGetter,
} from "vue";
import type { TableViewSchema } from "src/contracts/ui-catalog";

export function useColumnPreferences(
  view: MaybeRefOrGetter<TableViewSchema>,
  hasRowActions: MaybeRefOrGetter<boolean>,
) {
  const visibleColumnNames = ref(
    toValue(view).columns.map((column) => column.field),
  );
  const denseTable = ref(false);
  const visibleColumns = computed(() => [
    ...visibleColumnNames.value,
    ...(toValue(hasRowActions) ? ["__actions"] : []),
  ]);

  function setColumnVisible(field: string, visible: boolean) {
    if (visible && !visibleColumnNames.value.includes(field)) {
      visibleColumnNames.value.push(field);
      return;
    }
    if (!visible && visibleColumnNames.value.length > 1) {
      visibleColumnNames.value = visibleColumnNames.value.filter(
        (name) => name !== field,
      );
    }
  }

  const stopViewWatcher = watch(
    () => toValue(view),
    (nextView) => {
      visibleColumnNames.value = nextView.columns.map((column) => column.field);
    },
  );
  onScopeDispose(stopViewWatcher);

  return {
    visibleColumnNames,
    visibleColumns,
    denseTable,
    setColumnVisible,
  };
}
