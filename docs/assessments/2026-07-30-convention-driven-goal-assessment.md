# yang-system 约定式交付目标达成度与前端生产级能力评估

> 评估对象：`project/yang-system`（后端）、`project/yang-system/frontend`（前端）
> 参照系：`scs/scs-api` + `scs/scs-web`（目标形态的来源系统）
> 评估日期：2026-07-30
> 评估问题：
> 1. 是否达成「后端新增模块/API，前端只需新增一个文件或不修改，即可用默认页面完成大部分内容，同时支持自定义页面和组件」？
> 2. 前端基础功能设施的技术选型与实现是否具备生产级能力？

## 一、结论先行

**目标已达成，且在两个维度上超过了 scs 参照系。** yang-system 走的是「显式契约驱动」路线，比 scs 的「路径字符串约定」更彻底：

| 场景 | scs | yang-system |
|---|---|---|
| 新增模块用默认表格页 | 加 1 个 stub 文件（薄壳 `.vue`） | **零前端改动** |
| 新增 Action/API | 菜单配置 + 可能加页面 | 零改动（自动出现在工具栏/行/批量操作区） |
| 自定义页面 | 按路径放文件（隐式约定，缺失无兜底） | 加文件 + `registry.ts` 注册 1 行（显式、有降级） |
| 契约安全性 | 无校验，页面文件缺失即运行时崩 | 后端构建期校验 + 前端 Zod 边界校验 + 安全降级 |

**前端技术选型合理，不建议更换；核心设施已达生产级（L4 初段，约 4.0/5）。** 剩余缺口集中在生产完整性：access token 存储、可观测性、i18n、无障碍与部署契约门禁，而非框架或架构问题。

边界说明：目前后端仅有 account / admin / org 三个 Addon，「默认页面完成大部分内容」的承诺尚未被足够多的真实业务模块反向验证；树形、relation、批量等通用路径的实现上限有待业务压力检验。

## 二、第一性原理：这个目标的本质

「后端加模块、前端零改动或只加一个文件」由三个必要条件组合而成，缺一不可：

1. **UI 事实的唯一来源在后端。** 有什么模块、什么字段、什么操作、放在哪里、是否危险，必须由后端显式声明。前端靠字符串猜测（后缀、字段名、已知模块列表）的任何残留，都会使「零改动」承诺在新场景下破产。
2. **前端是契约的通用解释器。** 一套与业务无关的渲染引擎（目录 → 导航 → 表格 → 表单 → 动作），能把任何合法契约投影成可用页面；它不认识任何具体业务。
3. **存在受控的逃逸舱。** 默认页面覆盖不了的场景，能以最小成本注入自定义页面/组件，且不破坏前两条（不能因为支持自定义，就把业务事实重新塞回前端）。

以下用这三条分别衡量 scs 与 yang-system。

## 三、参照系：scs 的实现机制

scs 达成目标的机制（证据见 `scs/scs-web`）：

- **菜单下发**：后端接口 `api.api.model` 按 addon 下发 action 列表；`layouts/AddonLayout.vue:132-154` 收到后对未知路由执行 `router.addRoute` 动态注册。
- **路径约定解析**：`router/index.js:30` 用 `import.meta.glob('pages/**/*.vue')` 建立页面文件表；`stores/base.js:122-135` 的 `getActionRoutes` 将 `item.api`（如 `sale.order.table`）替换为 `/src/pages/sale/order/table.vue` 查表得到组件。
- **通用渲染**：1029 行的 `components/table/table.vue` 由后端字段配置驱动，承担所有默认表格页。
- **stub 文件**：每个业务模块至少需要一个薄壳页面（如 `pages/goods/goods/table.vue`，全部内容即 `<ComTableTable :api="route.name" />`）；复杂场景在同路径写完整自定义页。

scs 模式的根本弱点：

- 约定是**隐式的**：页面文件缺失时 `component: this.pages[t]` 为 `undefined`，无校验、无降级，运行时才知道；
- 字段配置靠**口头约定**：纯 JS 项目，无类型、无运行时校验，`package.json` 中 `"test": "echo \"No test specified\" && exit 0"`；
- 通用组件是 god component（1029 行），表格、卡片、树、编辑耦合在一个文件里；
- 因此 scs 的达成度是「加一个 stub 文件」，而不是「零改动」。

## 四、yang-system 后端：UI 事实源已成立

后端通过 yang-base 的 definition 内核，把 UI 语义变成构建期校验过的显式契约：

1. **字段级 UI 语义**：`fields!` 宏声明 `title / require / searchable / sortable / filterable / options / relation / tenant_key` 等，例：`src/modules/org/user/mod.rs:34-73`（`org_org` 字段声明了 `target`/`display`/`select` 关系语义）。
2. **框架生成标准 CRUD**：`crud_at_with_mutations("/api/v1/org/users", ...)`（`src/modules/org/user/mod.rs:108-114`），内置 Add/Put/Del/Select/Get 由 `crates/yang-base/src/action/builtin/` 提供；自定义写法则通过 `#[derive(Action)]` + `params!` 声明参数 schema 与权限，例：`src/modules/org/organization/actions/list.rs`。
3. **表格视图契约**：`ViewSpec` 声明列、数据源 Action、行/工具栏动作的 placement、interaction、确认语、行参数绑定，例：`src/modules/org/user/view.rs`。
4. **模块展示契约**：`ModulePresentationSpec` 声明模块归属身份（AccountIdentitySpec）、标题、图标、排序、主 Action，例：`src/modules/org/organization/mod.rs:61-70`、`src/modules/presentation.rs`。
5. **组合与校验**：每个 Addon 只暴露一个 `build_addon()`（`src/modules/mod.rs`），`AddonSpec` 声明依赖与中间件链（`src/modules/org/mod.rs:28-72`，Token 认证 → 租户解析的顺序具有语义）；`AppBuilder` 构建期完成校验并冻结 Catalog/Registry。
6. **运行时投影**：`UiCatalogAction` 按请求者权限投影 Catalog，带 `schema_version` + SHA-256 `revision`，支持 ETag/304（`crates/yang-base/src/definition/ui.rs`、`crates/yang-base/src/action/ui_catalog.rs`）。

第一性原理第 1 条（事实源在后端）成立：placement、identity、view ownership、行参数绑定、危险确认全部是显式契约，前端无猜测残留。

## 五、yang-system 前端：通用解释器已成立

### 5.1 零改动的证据链

- **路由完全参数化**：`frontend/src/router/routes.ts:45-48` 只有 `/module/:moduleId` 一条业务路由，新增模块不碰路由表；
- **目录驱动导航**：`module-pages.ts` 把 Catalog 的 modules/actions/views 组装为应用中心与模块页定义，按 `identity` + `order` 排序，未声明的视图自动归入未分配区；
- **通用模块页**：`pages/ModulePage.vue` 按 Catalog 渲染模块头、工具栏动作、主数据表/详情、行动作对话框；
- **通用表格**：`components/table/TableView.vue`（320 行，仅编排）+ 5 个 composable（查询、关系选项、动作、选择、列偏好）+ 3 个呈现组件，覆盖搜索、筛选（eq/contains/in/range）、排序、分页、树形（失败安全降级）、批量操作、列显隐与密度偏好；
- **通用表单**：`components/form/JsonSchemaForm.vue` + `SchemaField.vue` 从 Action 的 `input_schema` 自动生成表单，支持 widget hint（text/textarea/password/relation_select/tree_select/date_time/json 等）、校验规则与 multipart 上传；
- **动作编排**：`ActionPresentation`（placement × interaction）驱动按钮位置与行为（form/download/preview/navigate/custom/invoke），含确认框与 availability 提示（仅提示、不冒充权限）。

### 5.2 受控逃逸舱

- 后端声明 `interaction = "custom"` + `view_id`；前端 `custom/registry.ts` 用**静态白名单**把 view_id 映射到组件 loader，注释明确「禁止根据后端字符串拼接 import 路径」；
- 未注册或加载失败时降级回通用页并给出提示（`pages/BusinessPage.vue:24-52`、`pages/WorkbenchPage.vue`）；
- 对比 scs 的 glob 隐式约定：多一步 registry 注册（共两个触点：新文件 + 一行注册），换来构建期可追踪、无隐式解析失败，是生产系统的正确取舍。

第一性原理第 2、3 条成立。

### 5.3 契约与缓存协议

- Catalog 响应经 Zod 严格校验（`contracts/ui-catalog.ts`），schema_version 白名单（2.2/2.3），失败抛 `ContractError` 带字段级明细；
- `api/client.ts:16-48`：带 `If-None-Match: "<revision>"` 请求，304 直接复用本地缓存；
- `stores/catalog.ts`：请求取消、迟到响应 guard（request id 比对）、错误结构化（message + details）。

## 六、前端生产级能力评估

### 6.1 技术选型判定（保留，不更换）

| 选型 | 结论 | 理由 |
|---|---|---|
| Vue 3 Composition API | 保留 | 动态界面 + 强类型组合逻辑，composable 拆分已验证 |
| TypeScript strict + vue-tsc 门禁 | 保留并强化 | Catalog/表格是高动态边界，静态约束兜底 |
| Quasar CLI + Vite（SPA） | 保留 | 后台组件覆盖广，CLI 集成成本最低；SPA 对登录后内部系统合理 |
| Pinia | 保留 | session/identity/tenant/catalog/navigation 已按单一所有权拆分，规模足够 |
| Vue Router | 保留 | 参数化路由 + access-policy 守卫清晰 |
| Zod 4 | 保留 | 后端 Catalog 与 API 返回必须运行时验证，已是边界事实标准 |
| Vitest + Playwright | 保留并扩大 | 与 Vite 工具链一致；E2E 覆盖多角色关键旅程 |
| TanStack Query / SSR / 微前端 | 不引入 | 当前规模无收益，复杂度不为流行买单 |

相对 scs-web（JS、无测试、全局 `$api`、god component），这是代际升级。

### 6.2 已达生产级的设施

- **契约边界**：Zod 校验 + schema_version 白名单 + ETag/304 内容寻址缓存（§5.3）；
- **会话链**：401 单飞刷新 + 原请求重放、refresh token 存 HttpOnly Cookie（前端 JS 不可读）、过期广播事件（`yang:session-expired` / `yang:session-refreshed`）、terminal 失败判定（`api/auth-session.ts`）；
- **请求完整性**：请求取消（AbortController）、竞态防护、`x-request-id` 提取（`api/client.ts`）；
- **工程门禁**：`pnpm check` = prettier + eslint（0 警告）+ vue-tsc + vitest + build + 产物校验；`scripts/verify-production-build.mjs` 禁止公开 source map、禁止 Workbench chunk 进入生产包（Workbench 路由仅 DEV 构建，`router/routes.ts:3-17`）；
- **测试**：20 个单测文件（契约、会话、缓存、访问策略、表格/表单模型）+ 5 个 Playwright spec（登录、身份空间、表格视图、动作演示、正式外壳）；
- **安全设计意识**：自定义组件静态白名单、树形构建失败降级、`availability` 仅作提示不作为权限依据、生产依赖审计入 full gate。

### 6.3 距离完整生产级的缺口（按优先级）

1. **access token 仍在 sessionStorage**（`api/auth-session.ts:7`）：可被同源 XSS 读取，且多标签页不共享会话；CSP 未闭环。refresh token 已迁入 HttpOnly Cookie，access token 内存化是下一步。
2. **可观测性缺失**：无错误上报/监控接入；`x-request-id` 已获取但未进入统一诊断链，前后端错误无法关联。
3. **无 i18n**：文案硬编码中文；`module-pages.ts:30-43` 的 iconTokens 与 `ModulePage.vue:137-152` 的 fieldLabel 映射仍是前端残留的业务猜测（范围很小，但属于第一性原理第 1 条的反例，应逐步收敛进 Catalog）。
4. **无障碍与部署契约未入门禁**：无 a11y 自动检查；SPA history 模式的服务端回退（fallback 到 index.html）未进入自动化验证。
5. **测试结构偏科**：单测重心在纯函数，TableView/JsonSchemaForm 的组件级交互测试偏薄。
6. **业务验证样本不足**：仅 3 个 Addon，通用渲染的复杂度上限（深层关系、大数据量、复杂树）未经真实业务压力检验。

### 6.4 成熟度评分

| 维度 | 评分 | 判断 |
|---|---:|---|
| 技术栈适配度 | 4.5 | 与契约驱动内部管理 SPA 高度匹配 |
| API/契约边界 | 4.5 | Catalog 2.3、Zod、集中 client、请求 id、单飞 refresh |
| Catalog 驱动程度 | 4.4 | 模块、身份、导航、动作展示、视图归属均显式契约驱动 |
| 状态与生命周期 | 4.2 | 单一所有权、应用根唯一启动 |
| 组件可维护性 | 4.1 | Table 顶层只编排，行为已拆分为 composable |
| 浏览器安全 | 3.7 | refresh token 不可读；access token / CSP 待闭环 |
| 测试 | 4.3 | 单测 + 全量 Chromium E2E + 依赖审计 + 产物门禁 |
| 无障碍与可观测性 | 2.8 | 尚未形成门禁与端到端错误关联 |
| **整体** | **4.0** | **L4 初段：显式契约驱动阶段，剩余风险集中在生产完整性** |

（与 `frontend/docs/2026-07-26-frontend-architecture-assessment.md` 的复评结论一致。）

## 七、结论与建议路径

**问题 1（目标达成度）**：达成。后端新增 Module（fields + view + presentation）或 Action，前端零改动即可获得导航、默认表格页、表单对话框、动作编排；自定义页面经「新文件 + registry 一行」接入且有安全降级。路线正确性优于 scs。

**问题 2（生产级能力）**：选型与核心设施（契约、会话、缓存、构建门禁、测试）已达生产级；上线前应闭环四件事——access token 内存化 + CSP、可观测性接入、i18n 收敛、a11y/部署门禁。

建议的下一步验证（非修复）：按 `org/user` 模式新写一个带关系字段、树形与批量动作的真实业务 Addon，全程不改前端，端到端验证「零改动」承诺并暴露通用渲染盲区。
