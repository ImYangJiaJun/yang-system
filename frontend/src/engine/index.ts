/**
 * 引擎公共出口：features/ 与 shell/ 引用引擎能力的约定入口。
 *
 * engine/ 是 Schema 驱动的通用解释引擎（与业务无关，可整体复用）：
 * - http/       Action 调用协议与 HTTP 基础设施；
 * - session/    浏览器会话协议（状态机、跨标签页协调、Step-up 协议、生命周期请求）；
 * - catalog/    后端 Catalog 的导航投影与缓存；
 * - contracts/  zod + Ajv 白名单校验与 OpenAPI 生成类型；
 * - renderers/  TableView / JsonSchemaForm / ActionDialog 通用解释器。
 *
 * 引擎内部子结构对外不可见；新增引擎能力时在此显式导出。
 * （深度路径 import 的机器禁令是后续增强，见 docs/architecture/frontend-rebuild/README 进度日志。）
 */

// http —— Action 调用协议
export { invokeAction, fetchUiCatalog } from "./http/client";
export { ApiError, StepUpRequiredError } from "./http/errors";
export type { InvocationResult, SessionContext } from "./http/types";

// session —— 会话协议
export {
  createSessionController,
  type SessionController,
  type SessionSnapshot,
  type SessionRestoreState,
} from "./session/session-controller";
export {
  SessionControllerContext,
  useSessionController,
  useSessionSnapshot,
  useSessionCredentials,
  useRestoredSession,
} from "./session/use-session";
export {
  login,
  logout,
  refreshSession,
  disableAccount,
  type LoginResult,
  type LogoutResult,
  type DisableAccountResult,
} from "./session/lifecycle";
export { completeStepUp, type StepUpProofResult } from "./session/step-up";
export { SessionExpiredError } from "./session/auth-session";

// catalog —— 导航投影
export {
  buildAccountModulePages,
  moduleView,
  modulesForIdentity,
  identityForModuleId,
  visibleAccountIdentities,
  type AccountIdentity,
  type AccountIdentityDefinition,
  type ModulePageDefinition,
} from "./catalog/module-pages";
export { useUiCatalog } from "./catalog/use-catalog";

// contracts —— 契约类型与校验
export type {
  UiCatalog,
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
} from "./contracts/ui-catalog";

// renderers —— 通用解释器构件
export { TableView } from "./renderers/table/TableView";
export { JsonSchemaForm } from "./renderers/form/JsonSchemaForm";
export {
  ActionDialog,
  ConfirmActionDialog,
} from "./renderers/action/ActionDialog";
