<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  ApiError,
  invokeAction,
  type InvocationResult,
  type SessionContext,
} from "@/api/client";
import { ContractError, type ActionDemoSchema } from "@/contracts/ui-catalog";
import { initialObject } from "@/contracts/json-schema";
import JsonSchemaForm from "./JsonSchemaForm.vue";

const props = defineProps<{
  action: ActionDemoSchema;
  session: SessionContext;
}>();

const values = ref<Record<string, unknown>>({});
const loading = ref(false);
const result = ref<InvocationResult>();
const error = ref<{ message: string; details?: unknown; requestId?: string }>();
let controller: AbortController | undefined;

const methodTagType = computed(() => {
  if (props.action.method === "GET") return "success";
  if (props.action.method === "DELETE") return "danger";
  if (props.action.method === "PUT" || props.action.method === "PATCH")
    return "warning";
  return "primary";
});

watch(
  () => props.action,
  (action) => {
    values.value = initialObject(action.input_schema);
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
  <section class="action-demo" data-testid="action-demo">
    <div class="action-heading">
      <div>
        <div class="action-tags">
          <el-tag :type="methodTagType" effect="dark">{{
            action.method
          }}</el-tag>
          <el-tag v-if="action.requires_auth" type="warning" effect="plain"
            >需要认证</el-tag
          >
          <el-tag effect="plain">{{ action.response_kind }}</el-tag>
        </div>
        <h2>{{ action.title || action.operation_id }}</h2>
        <p>{{ action.description || "该 Action 未提供说明。" }}</p>
      </div>
      <code class="operation-id">{{ action.operation_id }}</code>
    </div>

    <el-alert
      v-if="action.request_media_type === 'multipart'"
      type="info"
      :closable="false"
      :title="`受限 multipart：最多 ${action.multipart?.max_files ?? 0} 个文件，单文件 ${action.multipart?.max_file_bytes ?? 0} bytes`"
      show-icon
    />

    <div class="route-line">
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
        <el-tag size="small" effect="plain">{{ parameter.source }}</el-tag>
        {{ parameter.title || parameter.name }}
      </span>
    </div>

    <div class="action-controls">
      <el-button
        type="primary"
        :loading="loading"
        data-testid="invoke-action"
        @click="submit"
      >
        发起真实调用
      </el-button>
      <el-button v-if="loading" @click="controller?.abort()">取消</el-button>
    </div>

    <el-alert
      v-if="error"
      type="error"
      :title="error.message"
      :description="
        error.requestId ? `request-id: ${error.requestId}` : undefined
      "
      :closable="false"
      show-icon
    />
    <pre v-if="error?.details" class="result-panel error-panel">{{
      JSON.stringify(error.details, null, 2)
    }}</pre>

    <div v-if="result" class="result-wrap" data-testid="action-result">
      <div class="result-meta">
        <el-tag type="success">
          {{
            result.kind === "redirect" && result.status === 0
              ? "Redirect 已拦截"
              : `HTTP ${result.status}`
          }}
        </el-tag>
        <span>{{ result.durationMs }} ms</span>
        <span v-if="result.requestId">request-id: {{ result.requestId }}</span>
      </div>
      <pre v-if="result.kind === 'json'" class="result-panel">{{
        JSON.stringify(result.data, null, 2)
      }}</pre>
      <el-result
        v-else-if="result.kind === 'redirect'"
        icon="info"
        title="服务端请求重定向"
      >
        <template #sub-title>{{
          result.location || "浏览器安全策略隐藏 Location，页面未自动跳转"
        }}</template>
      </el-result>
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
