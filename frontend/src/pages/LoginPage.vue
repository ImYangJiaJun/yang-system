<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";
import { login } from "src/api/auth";
import { useCatalogStore } from "stores/catalog";

const router = useRouter();
const store = useCatalogStore();
const username = ref("");
const password = ref("");
const submitting = ref(false);
const errorMessage = ref("");

async function submit() {
  if (submitting.value) return;
  errorMessage.value = "";
  submitting.value = true;
  try {
    const result = await login(username.value.trim(), password.value);
    store.setAccessToken(result.accessToken);
    await router.replace("/");
  } catch (cause) {
    errorMessage.value =
      cause instanceof Error ? cause.message : "登录失败，请稍后重试";
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <main class="login-page">
    <q-card class="login-card">
      <q-card-section class="login-heading">
        <span class="brand-mark">Y</span>
        <div>
          <h1>登录 YANG System</h1>
          <p>使用账号和密码继续</p>
        </div>
      </q-card-section>

      <q-form class="login-form" @submit.prevent="submit">
        <q-input
          v-model="username"
          name="username"
          label="用户名"
          autocomplete="username"
          outlined
          autofocus
          :rules="[(value: string) => Boolean(value.trim()) || '请输入用户名']"
        >
          <template #prepend><q-icon name="person" /></template>
        </q-input>
        <q-input
          v-model="password"
          name="password"
          label="密码"
          type="password"
          autocomplete="current-password"
          outlined
          :rules="[(value: string) => Boolean(value) || '请输入密码']"
        >
          <template #prepend><q-icon name="lock" /></template>
        </q-input>
        <q-banner v-if="errorMessage" rounded class="bg-red-1 text-negative">
          {{ errorMessage }}
        </q-banner>
        <q-btn
          type="submit"
          color="primary"
          label="登录"
          unelevated
          size="lg"
          :loading="submitting"
          class="full-width"
        />
      </q-form>
    </q-card>
  </main>
</template>
