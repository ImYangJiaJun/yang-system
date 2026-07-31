<script setup lang="ts">
import type { SessionContext } from "src/api/client";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
} from "src/contracts/ui-catalog";
import JsonSchemaForm from "components/form/JsonSchemaForm.vue";

defineProps<{
  activePresentation?: ActionPresentationSchema;
  activeAction?: ActionDemoSchema;
  businessFields: FormFieldSchema[];
  actions: ActionDemoSchema[];
  session: SessionContext;
  loading: boolean;
  submitLabel?: string;
}>();

const open = defineModel<boolean>({ required: true });
const values = defineModel<Record<string, unknown>>("values", {
  required: true,
});
const emit = defineEmits<{ submit: [] }>();
</script>

<template>
  <q-dialog v-model="open">
    <q-card class="action-dialog-card">
      <q-card-section class="row items-center">
        <h2 class="text-h6 q-my-none">
          {{ activePresentation?.title || activeAction?.title }}
        </h2>
        <q-space />
        <q-btn v-close-popup flat round dense icon="close" aria-label="关闭" />
      </q-card-section>
      <q-separator />
      <q-card-section class="scroll action-dialog-content">
        <JsonSchemaForm
          v-if="activeAction"
          v-model="values"
          :schema="activeAction.input_schema"
          :params="activeAction.params"
          :business-fields="businessFields"
          :actions="actions"
          :session="session"
          :multipart="activeAction.multipart"
        />
      </q-card-section>
      <q-separator />
      <q-card-actions align="right">
        <q-btn v-close-popup flat label="取消" />
        <q-btn
          color="primary"
          :label="submitLabel || '提交'"
          :loading="loading"
          @click="emit('submit')"
        />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>
