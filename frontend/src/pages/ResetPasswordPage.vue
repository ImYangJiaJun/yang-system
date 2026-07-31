<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";
import { resetPassword } from "src/api/auth";
import { publishSessionEnd } from "src/api/session-coordination";
import { useApplicationSession } from "src/composables/useApplicationSession";

const router = useRouter();
const applicationSession = useApplicationSession();
const resetToken = ref("");
const newPassword = ref("");
const confirmPassword = ref("");
const submitting = ref(false);
const errorMessage = ref("");
const passwordVisible = ref(false);

async function submit() {
  if (submitting.value) return;
  errorMessage.value = "";
  if (newPassword.value !== confirmPassword.value) {
    errorMessage.value = "两次输入的新密码不一致";
    return;
  }
  submitting.value = true;
  try {
    await resetPassword(resetToken.value.trim(), newPassword.value);
    resetToken.value = "";
    newPassword.value = "";
    confirmPassword.value = "";
    applicationSession.clearSession();
    publishSessionEnd("credentials-changed");
    await router.replace({
      name: "login",
      query: { reason: "credentials-changed" },
    });
  } catch (cause) {
    errorMessage.value =
      cause instanceof Error ? cause.message : "密码重置失败，请稍后重试";
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <main
    class="reset-password-page row items-center justify-center q-pa-md bg-grey-2"
  >
    <q-card class="reset-password-card">
      <q-card-section>
        <h1 class="text-h5 q-my-none">重置密码</h1>
        <p class="text-grey-7 q-mb-none">
          输入管理员通过受控渠道交付的一次性凭证。凭证成功使用后立即失效。
        </p>
      </q-card-section>
      <q-separator />
      <q-form class="q-pa-lg q-gutter-md" @submit.prevent="submit">
        <q-input
          v-model="resetToken"
          name="reset_token"
          label="重置凭证"
          type="password"
          autocomplete="one-time-code"
          filled
          :rules="[
            (value: string) => Boolean(value.trim()) || '请输入重置凭证',
          ]"
        />
        <q-input
          v-model="newPassword"
          name="new_password"
          label="新密码"
          :type="passwordVisible ? 'text' : 'password'"
          autocomplete="new-password"
          filled
          :rules="[
            (value: string) => value.length >= 10 || '新密码至少 10 个字符',
          ]"
        >
          <template #append>
            <q-icon
              :name="passwordVisible ? 'visibility' : 'visibility_off'"
              class="cursor-pointer"
              @click="passwordVisible = !passwordVisible"
            />
          </template>
        </q-input>
        <q-input
          v-model="confirmPassword"
          name="confirm_password"
          label="确认新密码"
          :type="passwordVisible ? 'text' : 'password'"
          autocomplete="new-password"
          filled
          :rules="[
            (value: string) =>
              value === newPassword || '两次输入的新密码不一致',
          ]"
        />
        <q-banner v-if="errorMessage" rounded class="bg-red-1 text-negative">
          {{ errorMessage }}
        </q-banner>
        <q-btn
          type="submit"
          color="primary"
          label="重置密码"
          unelevated
          size="lg"
          class="full-width"
          :loading="submitting"
        />
        <q-btn flat label="返回登录" class="full-width" to="/login" />
      </q-form>
    </q-card>
  </main>
</template>

<style scoped>
.reset-password-page {
  min-height: 100vh;
}

.reset-password-card {
  width: min(100%, 480px);
}
</style>
