<script setup lang="ts">
import type { SessionContext } from "src/api/client";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import JsonSchemaForm from "components/form/JsonSchemaForm.vue";

defineProps<{
  activePresentation?: ActionPresentationSchema;
  activeAction?: ActionDemoSchema;
  view: TableViewSchema;
  actions: ActionDemoSchema[];
  session: SessionContext;
  loading: boolean;
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
        <div class="text-h6">
          {{ activePresentation?.title || activeAction?.title }}
        </div>
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
          :business-fields="view.form.fields"
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
          label="提交"
          :loading="loading"
          @click="emit('submit')"
        />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>
