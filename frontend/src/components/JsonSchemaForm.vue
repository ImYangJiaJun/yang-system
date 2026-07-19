<script setup lang="ts">
import { computed } from "vue";
import { asJsonSchema, effectiveSchema } from "@/contracts/json-schema";
import type { ActionDemoSchema } from "@/contracts/ui-catalog";
import type { SessionContext } from "@/api/client";
import type { FormFieldSchema } from "@/contracts/ui-catalog";
import SchemaField from "./SchemaField.vue";

const props = defineProps<{
  schema: unknown;
  modelValue: Record<string, unknown>;
  params?: ActionDemoSchema["params"];
  businessFields?: FormFieldSchema[];
  actions?: ActionDemoSchema[];
  session?: SessionContext;
  multipart?: ActionDemoSchema["multipart"];
}>();

const emit = defineEmits<{
  "update:modelValue": [value: Record<string, unknown>];
}>();

const root = computed(() => asJsonSchema(props.schema));
const resolved = computed(() => effectiveSchema(root.value, root.value));
const properties = computed(() => resolved.value.properties ?? {});
const required = computed(() => new Set(resolved.value.required ?? []));
const presentation = computed(
  () =>
    new Map(
      (props.params ?? []).map((parameter) => [parameter.name, parameter]),
    ),
);
const businessFields = computed(
  () =>
    new Map((props.businessFields ?? []).map((field) => [field.field, field])),
);

function updateField(name: string, value: unknown) {
  emit("update:modelValue", { ...props.modelValue, [name]: value });
}
</script>

<template>
  <el-empty
    v-if="Object.keys(properties).length === 0"
    description="此 Action 无输入字段"
    :image-size="64"
  />
  <el-form v-else label-position="top" class="schema-form">
    <SchemaField
      v-for="(property, name) in properties"
      :key="name"
      :name="name"
      :schema="property"
      :root-schema="root"
      :model-value="modelValue[name]"
      :required="
        required.has(name) || Boolean(presentation.get(name)?.required)
      "
      :title="presentation.get(name)?.title"
      :description="presentation.get(name)?.description"
      :business-field="businessFields.get(name)"
      :actions="actions"
      :session="session"
      :multipart="multipart"
      @update:model-value="updateField(name, $event)"
    />
  </el-form>
</template>
