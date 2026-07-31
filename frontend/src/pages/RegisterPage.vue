<script setup lang="ts">
import { onUnmounted, ref } from "vue";
import { useRouter } from "vue-router";
import { register, requestRegistrationEmail } from "src/api/auth";

const router = useRouter();
const username = ref("");
const email = ref("");
const emailCode = ref("");
const password = ref("");
const confirmPassword = ref("");
const passwordVisible = ref(false);
const sendingCode = ref(false);
const submitting = ref(false);
const resendRemaining = ref(0);
const statusMessage = ref("");
const errorMessage = ref("");
let cooldownTimer: ReturnType<typeof setInterval> | undefined;

function stopCooldown() {
  if (cooldownTimer !== undefined) clearInterval(cooldownTimer);
  cooldownTimer = undefined;
}

function startCooldown(seconds: number) {
  stopCooldown();
  resendRemaining.value = seconds;
  cooldownTimer = setInterval(() => {
    resendRemaining.value = Math.max(0, resendRemaining.value - 1);
    if (resendRemaining.value === 0) stopCooldown();
  }, 1_000);
}

async function sendCode() {
  if (sendingCode.value || resendRemaining.value > 0) return;
  errorMessage.value = "";
  statusMessage.value = "";
  const normalizedEmail = email.value.trim().toLowerCase();
  if (!normalizedEmail) {
    errorMessage.value = "请输入邮箱";
    return;
  }
  sendingCode.value = true;
  try {
    const challenge = await requestRegistrationEmail(normalizedEmail);
    email.value = normalizedEmail;
    statusMessage.value = `若邮箱可用于注册，验证码将在 ${Math.ceil(challenge.expiresIn / 60)} 分钟内送达。`;
    startCooldown(challenge.resendAfter);
  } catch (cause) {
    errorMessage.value =
      cause instanceof Error ? cause.message : "验证码发送失败，请稍后重试";
  } finally {
    sendingCode.value = false;
  }
}

async function submit() {
  if (submitting.value) return;
  errorMessage.value = "";
  if (password.value !== confirmPassword.value) {
    errorMessage.value = "两次输入的密码不一致";
    return;
  }
  submitting.value = true;
  try {
    await register(
      username.value.trim(),
      password.value,
      email.value.trim().toLowerCase(),
      emailCode.value.trim(),
    );
    await router.replace({ name: "login", query: { registered: "1" } });
  } catch (cause) {
    errorMessage.value =
      cause instanceof Error ? cause.message : "注册失败，请稍后重试";
  } finally {
    submitting.value = false;
  }
}

onUnmounted(stopCooldown);
</script>

<template>
  <main class="register-page row items-center justify-center q-pa-md bg-grey-2">
    <q-card class="register-card">
      <q-card-section>
        <h1 class="text-h5 q-my-none">创建账号</h1>
        <p class="text-grey-7 q-mb-none">使用已验证邮箱创建全局 YANG 账号。</p>
      </q-card-section>
      <q-separator />
      <q-form class="q-pa-lg q-gutter-md" @submit.prevent="submit">
        <q-input
          v-model="username"
          name="username"
          label="帐号"
          autocomplete="username"
          filled
          autofocus
          :rules="[
            (value: string) =>
              (value.trim().length >= 3 && value.trim().length <= 64) ||
              '帐号长度必须为 3 到 64 个字符',
          ]"
        />
        <q-input
          v-model="email"
          name="email"
          label="邮箱"
          type="email"
          autocomplete="email"
          filled
          :rules="[(value: string) => Boolean(value.trim()) || '请输入邮箱']"
        />
        <q-btn
          type="button"
          outline
          color="primary"
          class="full-width"
          :label="
            resendRemaining > 0
              ? `重新发送（${resendRemaining}s）`
              : '发送验证码'
          "
          :loading="sendingCode"
          :disable="resendRemaining > 0"
          @click="sendCode"
        />
        <q-input
          v-model="emailCode"
          name="email_code"
          label="邮箱验证码"
          inputmode="numeric"
          autocomplete="one-time-code"
          maxlength="6"
          filled
          :rules="[
            (value: string) =>
              /^\d{6}$/.test(value.trim()) || '请输入 6 位验证码',
          ]"
        />
        <q-input
          v-model="password"
          name="password"
          label="密码"
          :type="passwordVisible ? 'text' : 'password'"
          autocomplete="new-password"
          filled
          :rules="[
            (value: string) => value.length >= 10 || '密码至少 10 个字符',
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
          label="确认密码"
          :type="passwordVisible ? 'text' : 'password'"
          autocomplete="new-password"
          filled
          :rules="[
            (value: string) => value === password || '两次输入的密码不一致',
          ]"
        />
        <q-banner
          v-if="statusMessage"
          rounded
          class="bg-blue-1 text-primary"
          aria-live="polite"
        >
          {{ statusMessage }}
        </q-banner>
        <q-banner
          v-if="errorMessage"
          rounded
          class="bg-red-1 text-negative"
          aria-live="assertive"
        >
          {{ errorMessage }}
        </q-banner>
        <q-btn
          type="submit"
          color="primary"
          label="创建账号"
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
.register-page {
  min-height: 100vh;
}

.register-card {
  width: min(100%, 520px);
}
</style>
