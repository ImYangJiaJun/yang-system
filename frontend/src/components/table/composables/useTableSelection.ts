import {
  computed,
  onScopeDispose,
  ref,
  toValue,
  watch,
  type MaybeRefOrGetter,
} from "vue";
import type { DisplayRow } from "../table-view-model";

export function useTableSelection(
  availableRows: MaybeRefOrGetter<DisplayRow[]>,
) {
  const selectedDisplayRows = ref<DisplayRow[]>([]);
  const selectedRows = computed(() =>
    selectedDisplayRows.value.map((row) => row.data),
  );

  function clear() {
    selectedDisplayRows.value = [];
  }

  const stopRowsWatcher = watch(() => toValue(availableRows), clear);
  onScopeDispose(stopRowsWatcher);

  return {
    selectedDisplayRows,
    selectedRows,
    clear,
  };
}
