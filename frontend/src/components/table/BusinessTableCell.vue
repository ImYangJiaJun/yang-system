<script setup lang="ts">
import { computed } from "vue";
import type { TableColumnSchema } from "src/contracts/ui-catalog";
import { resolveCellPresentation } from "./business-cell-model";

const props = defineProps<{
  column: TableColumnSchema;
  value: unknown;
  relationLabel?: string;
}>();

const presentation = computed(() =>
  resolveCellPresentation(props.column, props.value, props.relationLabel),
);
</script>

<template>
  <q-badge
    v-if="presentation.kind === 'status'"
    rounded
    class="business-status"
    :class="`business-status--${presentation.tone}`"
  >
    {{ presentation.text }}
    <q-tooltip v-if="presentation.tooltip">{{
      presentation.tooltip
    }}</q-tooltip>
  </q-badge>
  <span v-else-if="presentation.kind === 'relation'" class="business-relation">
    <q-icon name="link" size="15px" />
    {{ presentation.text }}
    <q-tooltip v-if="presentation.tooltip">{{
      presentation.tooltip
    }}</q-tooltip>
  </span>
  <span
    v-else-if="presentation.kind === 'boolean'"
    class="business-boolean"
    :class="`business-boolean--${presentation.tone}`"
  >
    <q-icon :name="value ? 'check_circle' : 'cancel'" size="17px" />
    {{ presentation.text }}
  </span>
  <code v-else-if="presentation.kind === 'json'" class="business-json">
    {{ presentation.text }}
    <q-tooltip v-if="presentation.tooltip" max-width="420px">
      {{ presentation.tooltip }}
    </q-tooltip>
  </code>
  <span
    v-else
    class="business-cell-text"
    :class="`business-cell-text--${column.display?.importance || 'secondary'}`"
  >
    {{ presentation.text }}
  </span>
</template>
