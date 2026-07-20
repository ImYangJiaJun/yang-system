<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  ApiError,
  invokeAction,
  type InvocationResult,
  type SessionContext,
} from "src/api/client";
import { ContractError, type ActionDemoSchema } from "src/contracts/ui-catalog";
import { initialObject } from "src/contracts/json-schema";
import JsonSchemaForm from "components/form/JsonSchemaForm.vue";

const props = defineProps<{
  action: ActionDemoSchema;
  session: SessionContext;
  formal?: boolean;
  initialValues?: Record<string, unknown>;
}>();
const emit = defineEmits<{
  completed: [result: InvocationResult];
}>();

const values = ref<Record<string, unknown>>({});
const loading = ref(false);
const result = ref<InvocationResult>();
const error = ref<{ message: string; details?: unknown; requestId?: string }>();
let controller: AbortController | undefined;

const methodTagColor = computed(() => {
  if (props.action.method === "GET") return "positive";
  if (props.action.method === "DELETE") return "negative";
  if (props.action.method === "PUT" || props.action.method === "PATCH")
    return "warning";
  return "primary";
});

watch(
  [() => props.action, () => props.initialValues],
  ([action, initialValues]) => {
    values.value = {
      ...initialObject(action.input_schema),
      ...(initialValues ?? {}),
    };
    result.value = undefined;
    error.value = undefined;
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  controller?.abort();
  if (result.value?.blobUrl) URL.revokeObjectURL(result.value.blobUrl);
});

async function submit() {
  if (loading.value) return;
  if (result.value?.blobUrl) URL.revokeObjectURL(result.value.blobUrl);
  result.value = undefined;
  error.value = undefined;
  controller = new AbortController();
  loading.value = true;
  try {
    result.value = await invokeAction(
      props.action,
      values.value,
      props.session,
      controller.signal,
    );
    emit("completed", result.value);
  } catch (cause) {
    if (cause instanceof ApiError) {
      error.value = {
        message: cause.message,
        details: cause.details,
        requestId: cause.requestId,
      };
    } else if (cause instanceof ContractError) {
      error.value = { message: cause.message, details: cause.details };
    } else if (cause instanceof Error && cause.name !== "AbortError") {
      error.value = { message: cause.message };
    }
  } finally {
    loading.value = false;
  }
}
</script>

<template>
  <section
    class="action-demo"
    :class="{ 'action-demo--formal': formal }"
    data-testid="action-demo"
  >
    <div class="action-heading">
      <div>
        <div v-if="!formal" class="action-tags">
          <q-badge :color="methodTagColor">{{ action.method }}</q-badge>
          <q-badge v-if="action.requires_auth" outline color="warning">
            需要认证
          </q-badge>
          <q-badge outline color="primary">{{ action.response_kind }}</q-badge>
        </div>
        <h2>{{ action.title || action.operation_id }}</h2>
        <p>{{ action.description || "该 Action 未提供说明。" }}</p>
      </div>
      <code v-if="!formal" class="operation-id">{{ action.operation_id }}</code>
    </div>

    <q-banner
      v-if="action.request_media_type === 'multipart'"
      rounded
      class="bg-blue-1 text-primary"
    >
      <template #avatar><q-icon name="info" /></template>
      受限 multipart：最多 {{ action.multipart?.max_files ?? 0 }} 个文件，单文件
      {{ action.multipart?.max_file_bytes ?? 0 }} bytes
    </q-banner>

    <div v-if="!formal" class="route-line">
      <span>{{ action.method }}</span>
      <code>{{ action.path }}</code>
    </div>

    <JsonSchemaForm
      v-model="values"
      :schema="action.input_schema"
      :params="action.params"
      :multipart="action.multipart"
    />

    <div v-if="action.params.length" class="param-summary">
      <span v-for="parameter in action.params" :key="parameter.name">
        <q-badge outline color="primary">{{ parameter.source }}</q-badge>
        {{ parameter.title || parameter.name }}
      </span>
    </div>

    <div class="action-controls">
      <q-btn
        color="primary"
        :label="formal ? action.title || '确认' : '发起真实调用'"
        :loading="loading"
        data-testid="invoke-action"
        @click="submit"
      />
      <q-btn
        v-if="loading"
        flat
        color="grey-8"
        label="取消"
        @click="controller?.abort()"
      />
    </div>

    <q-banner v-if="error" rounded class="bg-red-1 text-negative">
      <template #avatar><q-icon name="error" /></template>
      <strong>{{ error.message }}</strong>
      <div v-if="error.requestId">request-id: {{ error.requestId }}</div>
    </q-banner>
    <pre v-if="error?.details" class="result-panel error-panel">{{
      JSON.stringify(error.details, null, 2)
    }}</pre>

    <div v-if="result" class="result-wrap" data-testid="action-result">
      <div class="result-meta">
        <q-badge color="positive">
          {{
            result.kind === "redirect" && result.status === 0
              ? "Redirect 已拦截"
              : `HTTP ${result.status}`
          }}
        </q-badge>
        <span>{{ result.durationMs }} ms</span>
        <span v-if="result.requestId">request-id: {{ result.requestId }}</span>
      </div>
      <pre v-if="result.kind === 'json'" class="result-panel">{{
        JSON.stringify(result.data, null, 2)
      }}</pre>
      <div v-else-if="result.kind === 'redirect'" class="redirect-result">
        <q-icon name="info" size="40px" color="primary" />
        <strong>服务端请求重定向</strong>
        <span>{{
          result.location || "浏览器安全策略隐藏 Location，页面未自动跳转"
        }}</span>
      </div>
      <div v-else class="attachment-result">
        <a
          :href="result.blobUrl"
          :download="
            result.kind === 'download' ? result.filename || '' : undefined
          "
          target="_blank"
        >
          {{ result.kind === "download" ? "下载文件" : "打开预览" }}
        </a>
        <span v-if="result.filename">{{ result.filename }}</span>
      </div>
    </div>
  </section>
</template>
