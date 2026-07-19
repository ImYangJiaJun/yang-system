<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import {
  effectiveSchema,
  type JsonSchemaNode,
} from "src/contracts/json-schema";
import type { SessionContext } from "src/api/client";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "src/contracts/ui-catalog";
import RelationSelect from "./RelationSelect.vue";

const props = defineProps<{
  name: string;
  schema: JsonSchemaNode;
  rootSchema: JsonSchemaNode;
  modelValue: unknown;
  required: boolean;
  title?: string;
  description?: string;
  businessField?: FormFieldSchema;
  actions?: ActionDemoSchema[];
  session?: SessionContext;
  multipart?: ActionDemoSchema["multipart"];
}>();

const emit = defineEmits<{ "update:modelValue": [value: unknown] }>();

const resolved = computed(() =>
  effectiveSchema(props.rootSchema, props.schema),
);
const type = computed(() => {
  const value = resolved.value.type;
  return Array.isArray(value) ? value.find((item) => item !== "null") : value;
});
const label = computed(() => props.title || resolved.value.title || props.name);
const relationAction = computed(() =>
  props.actions?.find(
    (action) =>
      action.operation_id === props.businessField?.relation?.operation_id,
  ),
);
const numericMinimum = computed(() => {
  if (typeof resolved.value.minimum === "number") return resolved.value.minimum;
  const value = props.businessField?.validation?.minimum;
  if (value === undefined) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
});
const numericMaximum = computed(() => {
  if (typeof resolved.value.maximum === "number") return resolved.value.maximum;
  const value = props.businessField?.validation?.maximum;
  if (value === undefined) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
});
const dateValue = computed(() =>
  typeof props.modelValue === "string" || typeof props.modelValue === "number"
    ? props.modelValue
    : undefined,
);
const inputType = computed(() => {
  const widget = props.businessField?.widget;
  if (widget === "textarea" || widget === "editor") return "textarea";
  if (widget === "password") return "password";
  if (widget) return "text";
  return resolved.value.format === "password" ? "password" : "text";
});
const enumOptions = computed(() =>
  (resolved.value.enum ?? []).map((value) => ({
    key: JSON.stringify(value),
    label: typeof value === "string" ? value : JSON.stringify(value),
    value,
  })),
);
const selectedEnumKey = computed(() => {
  const key = JSON.stringify(props.modelValue);
  return enumOptions.value.some((option) => option.key === key)
    ? key
    : undefined;
});
const isJson = computed(
  () =>
    type.value === "object" ||
    type.value === "array" ||
    (!type.value && !resolved.value.enum),
);
const jsonDraft = ref("");
const jsonError = ref("");
const uploadInputId = useId();
const isBinary = computed(() => {
  if (resolved.value.format === "binary") return true;
  return (
    type.value === "array" &&
    effectiveSchema(props.rootSchema, resolved.value.items ?? {}).format ===
      "binary"
  );
});
const isMultipleFiles = computed(
  () => type.value === "array" && isBinary.value,
);
const selectedFileNames = computed(() => {
  if (props.modelValue instanceof File) return props.modelValue.name;
  if (Array.isArray(props.modelValue))
    return props.modelValue
      .filter((value) => value instanceof File)
      .map((file) => file.name)
      .join("、");
  return "";
});

watch(
  () => props.modelValue,
  (value) => {
    if (!isJson.value) return;
    jsonDraft.value = value === undefined ? "" : JSON.stringify(value, null, 2);
  },
  { immediate: true },
);

function update(value: unknown) {
  emit("update:modelValue", value);
}

function updateEnum(key: string) {
  const option = enumOptions.value.find((candidate) => candidate.key === key);
  update(option?.value);
}

function updateNumber(value: string | number | null) {
  if (value === null || value === "") {
    update(undefined);
    return;
  }
  const parsed = Number(value);
  if (Number.isFinite(parsed)) update(parsed);
}

function commitJson() {
  if (!jsonDraft.value.trim()) {
    update(undefined);
    jsonError.value = "";
    return;
  }
  try {
    update(JSON.parse(jsonDraft.value));
    jsonError.value = "";
  } catch (error) {
    jsonError.value = error instanceof Error ? error.message : String(error);
  }
}

function selectFiles(event: Event) {
  const target = event.target;
  if (!(target instanceof HTMLInputElement)) return;
  const files = Array.from(target.files ?? []);
  update(isMultipleFiles.value ? files : files[0]);
}
</script>

<template>
  <div class="schema-field">
    <div v-if="isBinary" class="upload-control">
      <label :for="uploadInputId" class="upload-label">
        {{ label }}<span v-if="required"> *</span>
      </label>
      <input
        :id="uploadInputId"
        type="file"
        :multiple="isMultipleFiles"
        :accept="multipart?.allowed_content_types.join(',')"
        @change="selectFiles"
      />
      <small v-if="selectedFileNames">已选择：{{ selectedFileNames }}</small>
      <small v-if="multipart">
        单文件不超过 {{ multipart.max_file_bytes }} bytes；最多
        {{ multipart.max_files }} 个文件
      </small>
    </div>
    <RelationSelect
      v-else-if="businessField?.relation && session"
      :model-value="modelValue"
      :label="label"
      :field="businessField"
      :action="relationAction"
      :session="session"
      :disabled="businessField.read_only"
      @update:model-value="update"
    />
    <q-select
      v-else-if="resolved.enum"
      :model-value="selectedEnumKey"
      :options="enumOptions"
      option-value="key"
      option-label="label"
      :disabled="businessField?.read_only"
      :label="label"
      outlined
      dense
      emit-value
      map-options
      clearable
      @update:model-value="updateEnum"
    />
    <q-toggle
      v-else-if="type === 'boolean'"
      :model-value="Boolean(modelValue)"
      :disabled="businessField?.read_only"
      :label="label"
      @update:model-value="update"
    />
    <q-input
      v-else-if="type === 'integer' || type === 'number'"
      :model-value="typeof modelValue === 'number' ? modelValue : undefined"
      type="number"
      :label="label"
      outlined
      dense
      :step="type === 'integer' ? '1' : '0.1'"
      :min="numericMinimum"
      :max="numericMaximum"
      :disabled="businessField?.read_only"
      @update:model-value="updateNumber"
    />
    <q-input
      v-else-if="businessField?.widget === 'date_time'"
      :model-value="dateValue"
      type="datetime-local"
      :label="label"
      outlined
      dense
      :disabled="businessField.read_only"
      @update:model-value="update"
    />
    <q-input
      v-else-if="!isJson"
      :model-value="
        typeof modelValue === 'string' ? modelValue : String(modelValue ?? '')
      "
      :type="inputType"
      :label="label"
      outlined
      dense
      :rows="businessField?.widget === 'textarea' ? 4 : undefined"
      :maxlength="resolved.maxLength ?? businessField?.validation?.max_length"
      :disabled="businessField?.read_only"
      clearable
      @update:model-value="update"
    />
    <q-input
      v-else
      v-model="jsonDraft"
      type="textarea"
      :label="label"
      outlined
      dense
      :rows="6"
      :disabled="businessField?.read_only"
      :error="Boolean(jsonError)"
      :error-message="jsonError"
      @blur="commitJson"
    />
    <div
      v-if="description || businessField?.description || resolved.description"
      class="field-help"
    >
      {{ description || businessField?.description || resolved.description }}
    </div>
  </div>
</template>
