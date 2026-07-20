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
const passwordVisible = ref(false);

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
    <section class="login-brand-panel">
      <div class="login-brand-copy">
        <q-avatar size="82px" color="white" text-color="primary">Y</q-avatar>
        <h1>YANG System</h1>
        <p>统一管理个人账号、平台账号与企业组织。</p>
      </div>
    </section>
    <aside class="login-drawer-panel">
      <q-card flat class="login-card">
        <q-card-section class="login-heading">
          <div>
            <h2>用户登录</h2>
            <p>使用 YANG 账号进入系统</p>
          </div>
        </q-card-section>

        <q-form class="login-form" @submit.prevent="submit">
          <q-input
            v-model="username"
            name="username"
            label="帐号"
            autocomplete="username"
            filled
            autofocus
            :rules="[(value: string) => Boolean(value.trim()) || '请输入帐号']"
          >
            <template #prepend><q-icon name="person" /></template>
          </q-input>
          <q-input
            v-model="password"
            name="password"
            label="密码"
            :type="passwordVisible ? 'text' : 'password'"
            autocomplete="current-password"
            filled
            :rules="[(value: string) => Boolean(value) || '请输入密码']"
          >
            <template #prepend><q-icon name="lock" /></template>
            <template #append>
              <q-icon
                :name="passwordVisible ? 'visibility' : 'visibility_off'"
                class="cursor-pointer"
                @click="passwordVisible = !passwordVisible"
              />
            </template>
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
        <q-card-section class="login-footer text-center text-grey-7">
          YANG 生态 · 契约驱动企业应用
        </q-card-section>
      </q-card>
    </aside>
  </main>
</template>
