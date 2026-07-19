<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { invokeAction, type SessionContext } from "src/api/client";
import { parseRelationOptions } from "src/contracts/table-data";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "src/contracts/ui-catalog";

const props = defineProps<{
  label: string;
  modelValue: unknown;
  field: FormFieldSchema;
  action?: ActionDemoSchema;
  session: SessionContext;
  disabled?: boolean;
}>();

const emit = defineEmits<{ "update:modelValue": [value: unknown] }>();
const options = ref<Array<{ value: string | number; label: string }>>([]);
const loading = ref(false);
const error = ref("");
let controller: AbortController | undefined;
let timer: number | undefined;
const selectValue = computed(() =>
  typeof props.modelValue === "string" || typeof props.modelValue === "number"
    ? props.modelValue
    : undefined,
);

function selectedValues(): Array<string | number> {
  return typeof props.modelValue === "string" ||
    typeof props.modelValue === "number"
    ? [props.modelValue]
    : [];
}

async function load(search?: string) {
  if (!props.action) {
    error.value = `目录缺少关系 Action：${props.field.relation?.operation_id ?? "unknown"}`;
    return;
  }
  controller?.abort();
  controller = new AbortController();
  loading.value = true;
  error.value = "";
  try {
    const result = await invokeAction(
      props.action,
      {
        search: search?.trim() || null,
        selected: selectedValues(),
        filter: {},
        page: 1,
        limit: 20,
      },
      props.session,
      controller.signal,
    );
    if (result.kind !== "json") throw new Error("关系 Action 必须返回 JSON");
    options.value = parseRelationOptions(result.data).items;
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

function remoteSearch(value: string) {
  if (timer !== undefined) window.clearTimeout(timer);
  timer = window.setTimeout(() => void load(value), 250);
}

function filterOptions(value: string, update: (callback: () => void) => void) {
  remoteSearch(value);
  update(() => undefined);
}

watch(
  () => props.modelValue,
  () => void load(),
);
onMounted(() => void load());
onBeforeUnmount(() => {
  controller?.abort();
  if (timer !== undefined) window.clearTimeout(timer);
});
</script>

<template>
  <div class="relation-select">
    <q-select
      :model-value="selectValue"
      :options="options"
      option-value="value"
      option-label="label"
      :disabled="disabled || !action"
      :loading="loading"
      :label="label"
      outlined
      dense
      emit-value
      map-options
      use-input
      input-debounce="0"
      clearable
      @filter="filterOptions"
      @update:model-value="emit('update:modelValue', $event)"
    />
    <small v-if="error" class="field-error">{{ error }}</small>
  </div>
</template>
