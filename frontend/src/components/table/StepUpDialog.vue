<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue";
import { useDialogPluginComponent, type QDialog, type QInput } from "quasar";
import type { ComponentPublicInstance } from "vue";
import { completeStepUp } from "src/api/step-up";
import type { SessionContext } from "src/api/client";

const props = defineProps<{
  challenge: string;
  session: SessionContext;
}>();

defineEmits([...useDialogPluginComponent.emits]);

const { dialogRef, onDialogHide, onDialogOK, onDialogCancel } =
  useDialogPluginComponent();
const username = ref("");
const password = ref("");
const loading = ref(false);
const errorMessage = ref("");
const usernameInput = ref<QInput>();
let controller: AbortController | undefined;

function setDialogRef(instance: Element | ComponentPublicInstance | null) {
  dialogRef.value = instance as QDialog | null;
}

async function submit() {
  if (loading.value || !username.value.trim() || !password.value) return;
  controller?.abort();
  controller = new AbortController();
  loading.value = true;
  errorMessage.value = "";
  try {
    const result = await completeStepUp(
      props.challenge,
      { username: username.value.trim(), password: password.value },
      props.session,
      controller.signal,
    );
    password.value = "";
    onDialogOK(result.proof);
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    password.value = "";
    errorMessage.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

onBeforeUnmount(() => controller?.abort());
</script>

<template>
  <q-dialog
    :ref="setDialogRef"
    persistent
    aria-label="敏感操作重新认证"
    @hide="onDialogHide"
    @show="usernameInput?.focus()"
  >
    <q-card class="step-up-dialog-card">
      <q-form @submit.prevent="submit">
        <q-card-section>
          <h2 class="text-h6 q-my-none">敏感操作重新认证</h2>
          <p class="text-body2 text-grey-7 q-mb-none">
            请重新输入账号密码。凭据与本次 proof 不会保存在浏览器存储中。
          </p>
        </q-card-section>
        <q-card-section class="q-gutter-md q-pt-none">
          <q-input
            ref="usernameInput"
            v-model="username"
            autocomplete="username"
            label="用户名"
            outlined
            :disable="loading"
          />
          <q-input
            v-model="password"
            autocomplete="current-password"
            label="密码"
            type="password"
            outlined
            :disable="loading"
          />
          <q-banner v-if="errorMessage" dense class="bg-red-1 text-negative">
            {{ errorMessage }}
          </q-banner>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn
            flat
            color="primary"
            label="取消"
            :disable="loading"
            @click="onDialogCancel"
          />
          <q-btn
            type="submit"
            unelevated
            color="primary"
            label="验证并继续"
            :loading="loading"
            :disable="!username.trim() || !password"
          />
        </q-card-actions>
      </q-form>
    </q-card>
  </q-dialog>
</template>

<style scoped>
.step-up-dialog-card {
  width: min(460px, calc(100vw - 32px));
}
</style>
