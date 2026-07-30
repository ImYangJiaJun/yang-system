<script setup lang="ts">
import { onBeforeUnmount, onMounted } from "vue";
import { useRouter } from "vue-router";

let dispose: (() => void) | undefined;
let mounted = true;
const router = useRouter();

onMounted(async () => {
  const { startApplication } = await import("src/application/startApplication");
  if (!mounted) return;
  dispose = startApplication(router);
});

onBeforeUnmount(() => {
  mounted = false;
  dispose?.();
});
</script>

<template>
  <router-view />
</template>
