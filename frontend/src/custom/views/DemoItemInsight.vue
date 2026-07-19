<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { z } from "zod";
import { invokeAction, type SessionContext } from "src/api/client";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
} from "src/contracts/ui-catalog";

const props = defineProps<{
  presentation: ActionPresentationSchema;
  actions: ActionDemoSchema[];
  session: SessionContext;
}>();
const emit = defineEmits<{ close: [] }>();

const insightSchema = z.object({
  total: z.number().int().nonnegative(),
  active: z.number().int().nonnegative(),
  draft: z.number().int().nonnegative(),
});
const insight = ref<z.infer<typeof insightSchema>>();
const loading = ref(false);
const error = ref("");
let controller: AbortController | undefined;
const action = computed(() =>
  props.actions.find(
    (candidate) => candidate.operation_id === props.presentation.operation_id,
  ),
);

async function load() {
  if (!action.value) {
    error.value = `目录缺少 ${props.presentation.operation_id}`;
    return;
  }
  controller?.abort();
  controller = new AbortController();
  loading.value = true;
  error.value = "";
  try {
    const result = await invokeAction(
      action.value,
      {},
      props.session,
      controller.signal,
    );
    if (result.kind !== "json") throw new Error("洞察 Action 必须返回 JSON");
    insight.value = insightSchema.parse(result.data);
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

onMounted(() => void load());
onBeforeUnmount(() => controller?.abort());
</script>

<template>
  <section class="custom-insight relative-position">
    <header class="custom-insight-heading">
      <div>
        <q-badge color="secondary">自定义 View</q-badge>
        <h2>项目运行洞察</h2>
        <p>由静态 view_id 注册表加载，数据仍通过声明的 Action 获取。</p>
      </div>
      <q-btn
        outline
        color="white"
        label="返回通用表格"
        @click="emit('close')"
      />
    </header>
    <q-banner v-if="error" rounded class="bg-red-1 text-negative q-mt-lg">
      <template #avatar><q-icon name="error" /></template>
      {{ error }}
    </q-banner>
    <div v-else-if="insight" class="insight-metrics">
      <article>
        <span>项目总数</span>
        <strong>{{ insight.total }}</strong>
      </article>
      <article>
        <span>运行中</span>
        <strong>{{ insight.active }}</strong>
      </article>
      <article>
        <span>草稿</span>
        <strong>{{ insight.draft }}</strong>
      </article>
    </div>
    <q-inner-loading :showing="loading" dark>
      <q-spinner-gears size="48px" color="white" />
    </q-inner-loading>
  </section>
</template>
