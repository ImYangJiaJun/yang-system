<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRoute, useRouter } from "vue-router";
import ActionDemo from "components/action/ActionDemo.vue";
import { summarizeAccountSpaces } from "src/account-spaces";
import type { ActionDemoSchema } from "src/contracts/ui-catalog";
import { useCatalogStore } from "stores/catalog";

const route = useRoute();
const router = useRouter();
const store = useCatalogStore();
const { catalog, loading, selectedViewId, session, token } = storeToRefs(store);
const activeAction = ref<ActionDemoSchema>();
const actionDialogOpen = ref(false);

const requestedSpace = computed(() => String(route.params.space ?? ""));
const space = computed(() =>
  summarizeAccountSpaces(catalog.value).find(
    (candidate) => candidate.id === requestedSpace.value,
  ),
);
const allowed = computed(() => Boolean(space.value?.available));
const loggedIn = computed(() => Boolean(token.value.trim()));

function openAction(action: ActionDemoSchema) {
  activeAction.value = action;
  actionDialogOpen.value = true;
}

async function openView(viewId: string) {
  selectedViewId.value = viewId;
  await router.push("/business");
}

function actionIcon(action: ActionDemoSchema): string {
  if (action.operation_id.endsWith(".list")) return "manage_accounts";
  if (action.operation_id.endsWith(".add")) return "person_add";
  if (action.operation_id.endsWith(".create")) return "add_business";
  if (action.operation_id.endsWith(".me")) return "badge";
  if (action.operation_id.endsWith(".logout")) return "logout";
  if (action.method === "DELETE") return "delete_outline";
  return "tune";
}
</script>

<template>
  <q-page padding class="account-space-page relative-position">
    <template v-if="space && allowed">
      <header class="account-space-hero">
        <q-avatar
          size="72px"
          color="primary"
          text-color="white"
          :icon="space.icon"
        />
        <div>
          <span class="account-space-kicker">{{ space.subtitle }}</span>
          <h1>{{ space.title }}</h1>
          <p>{{ space.description }}</p>
        </div>
      </header>

      <q-banner
        v-if="space.id === 'user' && !loggedIn"
        rounded
        class="account-space-notice bg-blue-1 text-primary"
      >
        <template #avatar><q-icon name="login" /></template>
        登录后可查看当前账号并管理安全会话。
        <template #action>
          <q-btn flat color="primary" label="前往登录" to="/login" />
        </template>
      </q-banner>

      <section v-if="space.views.length" class="account-space-section">
        <div class="account-space-section-heading">
          <div>
            <span>Management</span>
            <h2>管理页面</h2>
          </div>
          <small>{{ space.views.length }} 个可访问页面</small>
        </div>
        <div class="account-resource-grid">
          <q-card
            v-for="view in space.views"
            :key="view.view_id"
            flat
            bordered
            class="account-resource-card cursor-pointer"
            @click="openView(view.view_id)"
          >
            <q-card-section>
              <q-icon name="table_view" color="primary" size="30px" />
              <h3>{{ view.title || view.table }}</h3>
              <p>查看并维护当前账号空间中的业务数据。</p>
            </q-card-section>
            <q-card-actions align="right">
              <q-btn flat color="primary" label="进入管理" icon-right="east" />
            </q-card-actions>
          </q-card>
        </div>
      </section>

      <section v-if="space.actions.length" class="account-space-section">
        <div class="account-space-section-heading">
          <div>
            <span>Operations</span>
            <h2>账号操作</h2>
          </div>
          <small>{{ space.actions.length }} 项当前可用操作</small>
        </div>
        <q-list bordered separator class="account-operation-list">
          <q-item
            v-for="action in space.actions"
            :key="action.operation_id"
            v-ripple
            clickable
            @click="openAction(action)"
          >
            <q-item-section avatar>
              <q-avatar
                color="blue-1"
                text-color="primary"
                :icon="actionIcon(action)"
              />
            </q-item-section>
            <q-item-section>
              <q-item-label>{{
                action.title || action.operation_id
              }}</q-item-label>
              <q-item-label caption>{{
                action.description || "账号管理操作"
              }}</q-item-label>
            </q-item-section>
            <q-item-section side>
              <q-icon name="chevron_right" color="grey-6" />
            </q-item-section>
          </q-item>
        </q-list>
      </section>

      <div
        v-if="!loading && !space.views.length && !space.actions.length"
        class="account-space-empty"
      >
        <q-icon name="shield" size="52px" color="primary" />
        <h2>账号空间已就绪</h2>
        <p>当前目录暂无更多操作；登录或切换企业后会自动刷新。</p>
      </div>
    </template>

    <div v-else-if="!loading" class="account-space-empty">
      <q-icon name="lock" size="52px" color="grey-6" />
      <h2>当前账号无法访问此空间</h2>
      <p>管理平台与企业账户只对服务端已授权身份开放。</p>
      <q-btn outline color="primary" label="返回应用中心" to="/" />
    </div>

    <q-dialog v-model="actionDialogOpen">
      <q-card class="account-action-dialog">
        <q-card-section class="row items-center q-pb-none">
          <div class="text-h6">账号操作</div>
          <q-space />
          <q-btn
            v-close-popup
            flat
            round
            dense
            icon="close"
            aria-label="关闭"
          />
        </q-card-section>
        <q-card-section class="scroll account-action-dialog-body">
          <ActionDemo
            v-if="activeAction"
            :action="activeAction"
            :session="session"
            formal
          />
        </q-card-section>
      </q-card>
    </q-dialog>

    <q-inner-loading :showing="loading">
      <q-spinner color="primary" size="48px" />
    </q-inner-loading>
  </q-page>
</template>
