<script setup lang="ts">
import { useDialogPluginComponent, type QDialog } from "quasar";
import type { ComponentPublicInstance } from "vue";

defineProps<{
  title: string;
  message: string;
}>();

defineEmits([...useDialogPluginComponent.emits]);

const { dialogRef, onDialogHide, onDialogOK, onDialogCancel } =
  useDialogPluginComponent();

function setDialogRef(instance: Element | ComponentPublicInstance | null) {
  dialogRef.value = instance as QDialog | null;
}
</script>

<template>
  <q-dialog
    :ref="setDialogRef"
    persistent
    :aria-label="title"
    @hide="onDialogHide"
  >
    <q-card class="action-confirmation-card">
      <q-card-section>
        <h2 class="text-h6 q-my-none">{{ title }}</h2>
      </q-card-section>
      <q-card-section v-if="message" class="q-pt-none">
        {{ message }}
      </q-card-section>
      <q-card-actions align="right">
        <q-btn flat color="primary" label="取消" @click="onDialogCancel" />
        <q-btn
          unelevated
          color="negative"
          label="确认"
          data-autofocus
          @click="onDialogOK"
        />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>

<style scoped>
.action-confirmation-card {
  width: min(440px, calc(100vw - 32px));
}
</style>
