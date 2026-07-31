# yang-system 约定式交付目标与前端生产就绪度评估

> 评估对象：`project/yang-system`（后端）与 `project/yang-system/frontend`（前端）
> 参照系：`D:\code\scs-api` + `D:\code\scs-web`
> 评估日期：2026-07-30
> 证据原则：只把当前源码能够证明或本次实际运行通过的能力记为“已验证”；历史结果、尚未覆盖的合法契约状态和未运行的门禁不外推为完成。
> 评估问题：
>
> 1. 是否已经达成“后端新增模块/API，前端不修改或只新增一个文件，即可用默认页面完成大部分内容，同时支持自定义页面和组件”？
> 2. 前端基础设施的技术选型是否合理，当前实现是否已经具备完整生产就绪能力？

## 一、可审计结论

### 1.1 结论表

| 判断项                                  | 当前状态 | 准确结论                                                                                                                   |
| --------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------- |
| 后端显式 UI 契约                        | 已验证   | 字段、Action、View、Module presentation、权限投影、revision 和 ETag 已形成一条显式链路                                     |
| 显式 View 的默认表格页                  | 已验证   | 后端声明可访问的 `data_action`、字段和 presentation 后，`TableView` 可在不修改前端的情况下渲染                             |
| 仅新增 Module/CRUD 即自动出现可用表格页 | 未达成   | 框架生成的无 Action 默认 View 不会投影到 UI Catalog；必须声明可访问的数据 Action                                           |
| 仅新增 Action/API 即自动进入正式页面    | 未达成   | Action 会进入请求级 Catalog，但只有被 Module/View presentation 引用后才进入正式工具栏、行或批量区                          |
| 正式 `ModulePage` 解释当前合法契约      | 已闭环   | 全部 View 可切换；Module presentation 复用统一执行器并覆盖 toolbar/row/bulk 与当前六类 interaction；业务特例已移出通用页面 |
| 自定义页面安全降级                      | 已验证   | 静态 registry 未命中或加载失败时，`ModulePage`、`BusinessPage`、`WorkbenchPage` 都保留通用页面                             |
| 自定义页面只新增一个文件                | 未达成   | 当前需要新增组件文件并修改 `custom/registry.ts`，共两个手工触点                                                            |
| Vue/Quasar/Pinia/Zod 技术选型           | 合理     | 与登录后的元数据驱动 SPA 匹配，当前没有足以抵消迁移成本的替代框架收益                                                      |
| 完整生产就绪                            | 未达成   | 仓库内门槛已闭环到真实业务规模基线；本提交未 push，远程 CI 与目标环境 TLS/告警送达仍无终态证据                                |

### 1.2 对两个评估问题的直接回答

**问题 1：条件性达成，不是普遍达成。**

- 已经达成的范围：后端新增 Module 时，同时声明 `ModulePresentationSpec`、一个或多个带可访问 `data_action` 的 `ViewSpec`、字段和受支持的 Action presentation，前端可以零修改获得多 View、通用表格、查询、表单，以及模块级 toolbar/row/bulk 与 form/invoke/download/preview/navigate/custom 分派。
- 尚未达成的范围：只注册 Module/CRUD、只新增 Action，以及严格“自定义页面只新增一个文件”。独立 `work` Addon 已证明：符合显式契约的关系、树、双 View 和批量流程无需增加前端业务特判。
- 因此不能写成“后端新增任意模块/API 都自动完成正式页面”，应写成“符合显式 Module/View/presentation 契约的后端变更可以零前端修改交付”。

**问题 2：选型合理，但当前只能定性为准生产阶段。**

- 契约、状态所有权、缓存、会话刷新、单元测试和生产构建门禁已经具备生产导向。
- 生产就绪是门槛判断，不应把安全、可观测性或部署缺口通过平均分抵消。本评估不再使用没有公开量表和权重的 `L4/4.0` 评分。

## 二、第一性原理：目标成立需要什么

“后端增加能力、前端零修改或一个文件扩展”本质上是在约束业务变化的前端变更成本，同时保证行为正确和可运营。要宣称目标普遍达成，至少需要同时满足五个条件：

1. **事实所有权明确。** 模块、字段、操作、位置、交互、危险确认、关系和租户语义必须有唯一权威来源。
2. **契约表达力完整。** 所有承诺支持的业务状态都能由契约表示，不能靠字段名、operation id 后缀或已知模块列表补语义。
3. **解释器对合法契约是完备的。** 每个合法 placement、interaction、View 数量和响应类型在正式页面都有确定行为；无法处理时必须显式拒绝或安全降级。
4. **逃逸舱受控且成本符合目标。** 自定义组件只能从前端静态产物选择，且手工触点数量、缺失检测时机和回退行为必须与目标一致。
5. **证据覆盖正式交付路径。** 验收必须运行在正式路由和生产构建上，并覆盖新模块、权限、失败、部署、监控和性能边界；DEV 工作台通过不能替代正式页面证明。

前三条决定“能否零改动正确渲染”，第四条决定“复杂业务能否低成本扩展”，第五条决定“结论是否经过证明”。当前 yang-system 已完成主要架构骨架，但还没有同时满足全部五条。

## 三、scs 参照系：事实与比较边界

当前 `D:\code\scs-web` 的机制是：

- `src/router/index.js:29-31` 通过 `import.meta.glob('pages/**/*.vue')` 建立页面与布局表；
- `src/stores/base.js:122-135` 将 `item.api` 转换成 `/src/pages/<path>.vue` 后直接读取组件；
- `src/layouts/AddonLayout.vue:131-145` 根据后端菜单结果动态注册未知 Action 路由；
- 业务表格页通常是 `<ComTableTable :api="route.name" />` 薄壳；
- `src/components/table/table.vue` 当前为 1029 行，集中承担较多表格行为；
- `package.json:12` 的 `test` 是成功退出的占位命令，不提供测试证据。

据此可以确认：

- scs 的路径约定确实减少了页面代码，但页面文件缺失只会在运行时表现为组件解析失败；
- yang-system 的后端引用校验、请求级权限投影、前端运行时契约解析和安全降级，在“契约明确性与失败可控性”上优于 scs；
- 不能把这种优势扩大为所有生产能力都更强。scs 已有 i18n 目录；yang-system 的 Nginx 仓库部署合同已在 §9.5 闭环，但多语言仍取决于产品需求，真实域名发布终态也只能在目标环境取证。

## 四、yang-system 后端：已成立的能力与边界

### 4.1 已成立

1. **字段事实集中。** `src/modules/org/user/mod.rs:34-72` 使用 `fields!` 声明 title、required、searchable、sortable、filterable、relation 和 tenant key 等语义。
2. **CRUD 契约由框架生成。** `ModuleSpec::crud_at_with_mutations` 注册 add/put/del/get/select/table Action；业务可替换写 Action Handler，同时保留统一路由、权限和 schema 契约。
3. **View 明确引用字段与 Action。** `src/modules/org/user/view.rs:9-39` 声明数据 Action、列、placement、interaction、record parameter 和删除确认。
4. **Module presentation 显式声明。** identity、title、icon、order、primary Action 与页面级 presentation 不再由前端通过 Action 名称推断。
5. **应用启动期完成组合校验。** `AppBuilder::build` 校验 Addon、Module、依赖、中间件顺序、引用和路由，再冻结 Catalog/Registry。这里的“构建期”是应用启动时的 definition build，不是 Rust 编译期。
6. **请求级授权投影。** `Registry::ui_catalog` 复用 Action dispatch 的授权策略过滤 Action、View、字段和 Module；UI 目录不能替代真实 dispatch、TableQuery 和租户隔离。
7. **内容寻址缓存。** `UiCatalog` 对过滤后的 actions/table_views/modules 计算 SHA-256 revision；`UiCatalogAction` 支持 `If-None-Match`、304、`private, no-cache` 和 `Vary`。

### 4.2 必须保留的边界说明

1. **默认 View 不等于可用 UI View。** `compile_runtime_table_views` 为无显式 View 的表生成 `data_action: None` 的默认 View；`Registry::ui_catalog` 只投影有可访问 data Action 的 View。因此“只新增表和 CRUD 就自动有默认表格页”不成立。
2. **Action 注册与 Action 展示是两件事。** 新 Action 可以出现在 Catalog 的全局 `actions` 中，但正式 Module/View 只消费被 presentation 引用的 Action。
3. **后端不能验证前端 custom registry。** 后端可验证 custom interaction 必须有 `view_id`，但不知道该 ID 是否已进入前端构建产物；当前只能运行时降级。
4. **规模结论有明确边界。** 新增 `work` Addon 后，当前样本覆盖 account、admin、org、work；本地真实 MySQL 基线证明 1 万项目、5 万任务、100 层树、100 条批量和 10 路并发，不外推为生产容量或未测业务形态。

## 五、前端：按运行路径评估，而不是混合计算

### 5.1 三条路径的职责

| 路径                | 构建状态 | 当前能力                                                                                      | 不能据此证明的内容                     |
| ------------------- | -------- | --------------------------------------------------------------------------------------------- | -------------------------------------- |
| `/module/:moduleId` | 生产包含 | 正式账户空间、全部 View 或 primary Action；`work.task` 已用树/清单双 View 和批量 Action 验收 | 目标环境发布与未声明的新交互类型       |
| `/business`         | 生产包含 | Catalog 导航、通用 `TableView`、View Action、custom 安全降级                                  | 每个 Module 的正式账户空间行为         |
| `/workbench`        | 仅 DEV   | 全局 Action 演示、TableView、上传下载预览重定向、自定义 View 调试                             | 生产包中的最终用户路径                 |

`frontend/src/router/routes.ts:3-17` 会从生产包移除 Workbench。任何只在 `/workbench` 通过的功能都必须在正式路由另有证据，才能计入正式交付能力。

### 5.2 通用 TableView 的已验证能力

`components/table/TableView.vue` 负责组合，行为拆到查询、关系选项、动作、选择和列偏好 composable，并由 `TableQueryPanel`、`TableDataGrid`、`TableActionDialog` 呈现。当前代码支持：

- 搜索、eq/contains/in/range 筛选、服务端排序和分页；
- relation options 批量加载与展示；
- 树形数据构建，失败时降级为普通表格；
- toolbar、row、bulk placement；
- form、invoke、custom 以及由响应附件驱动的 download/preview/redirect；
- 确认框、availability 提示、列显隐和密度偏好；
- JSON Schema 表单、关系选择器、widget hint 和受限 multipart。

这里的结论只适用于 `TableView` 已接入且 Catalog 契约完整的路径。

### 5.3 正式 ModulePage 的当前闭环

`frontend/src/pages/ModulePage.vue` 已收敛到与 `TableView` 相同的解释链：

- `moduleView()` 把 Module presentation 合并到当前 View，并以标签页暴露全部 `views`，不再静默丢弃 `views[1..]`；
- `usePresentedActions()` 是 View 与无 View Module 共用的 Action executor，统一处理 hidden/disabled、确认、表单、调用、附件、重定向结果、行参数与批量选择；
- ModulePage 接入静态 custom registry；注册命中时加载组件，未注册或加载失败时保留通用模块页；
- Module presentation 必须同时被 `module.actions` 授权才会进入页面，不能借用 Catalog 中仅全局可见的 Action；
- 字段标题与显示类型改从输出 JSON Schema 推导，`org.tenant` 行为通过 `product-shell/module-extensions.ts` 的显式产品外壳接口注入，通用页面不再判断具体模块 ID、字段名或状态值。

正式路由 E2E 已覆盖多 View、行 form、bulk invoke、download、preview、navigate 的安全分支、custom 成功与未注册回退、hidden/disabled 以及未获 Module 授权的 Action 引用；执行器单测穷举 toolbar/row/bulk × 六类 interaction。

### 5.4 后端不是当前唯一 UI 事实源

除 Catalog 投影外，前端仍保存以下业务知识：

- `module-pages.ts:30-43` 的业务 icon token 映射；
- `product-shell/module-extensions.ts` 对 `org.tenant` 的显式产品外壳扩展；
- `components/account/AccountSwitcher.vue:44-70` 对 `"org"` 和 `/module/org.tenant` 的硬编码；
- `stores/tenant.ts:43-46` 对 `org.tenant.list` operation id 的硬编码。

这些代码可以作为产品外壳的显式特例存在，但存在时只能声称“后端是主要事实源”，不能声称“前端完全不认识具体业务”。

### 5.5 自定义页面的真实成本

当前安全机制是正确的：

- `custom/registry.ts` 是静态白名单，不根据后端字符串拼接 import；
- `ModulePage`、`BusinessPage` 与 `WorkbenchPage` 在未注册或加载失败时保留通用页面。

当前成本也必须如实记录：

- 一个自定义页面需要新增组件文件；
- 还需要修改 `custom/registry.ts`；
- 后端与前端 registry 不存在构建期交叉校验；
- 三条页面路径都已接入同一静态 registry 的运行时安全回退；后端仍不能在构建期证明 `view_id` 已注册。

所以当前是“两个手工触点 + 运行时安全降级”，不是“只新增一个文件 + 构建期保证”。

### 5.6 Zod 与缓存协议的准确描述

- `contracts/ui-catalog.ts` 使用 `safeParse`、字段约束、关联校验和 schema version 白名单，失败会生成带路径明细的 `ContractError`；
- 当前支持 `2.2` 和 `2.3`；
- 多数 schema 使用普通 `z.object()`，没有 `.strict()`；未知对象字段默认被移除，部分展示枚举通过 `.catch()` 安全降级；
- 未知 Action interaction 不会降级为 invoke，而是校验失败；
- 因此准确表述是“运行时边界校验 + 对部分展示字段前向兼容”，不是“所有未知内容都被严格拒绝”；
- `api/client.ts` 发送 `If-None-Match`，304 只在本地已有缓存时复用；
- `stores/catalog.ts` 具备 AbortController、request id 迟到响应保护和结构化错误。

## 六、技术选型与生产就绪度

### 6.1 技术选型

| 选型                        | 当前判断 | 原因                                                                                      |
| --------------------------- | -------- | ----------------------------------------------------------------------------------------- |
| Vue 3 Composition API       | 保留     | 动态契约界面与可组合状态逻辑匹配                                                          |
| TypeScript strict + vue-tsc | 保留     | 高动态 Catalog 边界仍需要静态约束                                                         |
| Quasar CLI + Vite SPA       | 保留     | 后台表格、表单、对话框和响应式组件覆盖足够；当前无 SEO/SSR 刚需                           |
| Pinia                       | 保留     | session、identity、tenant、catalog、navigation、lifecycle 已有清晰 owner                  |
| Vue Router                  | 保留     | 参数化路由和访问策略满足当前规模                                                          |
| Zod                         | 保留     | HTTP JSON 是运行时不可信边界，必须在使用前解析                                            |
| Vitest + Playwright         | 保留     | 工具链匹配；dev 与正式产物浏览器门禁都已进入 full/CI，后续重点是 a11y、可观测性和规模证据 |
| TanStack Query、SSR、微前端 | 暂不引入 | 当前问题是契约完备性和生产门禁，不是缺少新框架                                            |

### 6.2 已有生产导向能力

- refresh token 使用 host-only `HttpOnly; SameSite=Strict` Cookie；HTTPS 下追加 Secure；
- 登录/刷新轮换 Cookie，登出撤销并清除 Cookie；浏览器 POST 校验 `Sec-Fetch-Site` 与 Origin/Referer/Host；
- 401 单飞刷新与原请求重放；terminal 失败清理会话；
- Catalog 运行时解析、ETag/304、取消和竞态保护；
- `pnpm check` 串联 Prettier、ESLint、vue-tsc、Vitest、生产构建和产物检查；
- 生产构建检查禁止公开 source map，并检查 Workbench 标记没有进入产物；
- `scripts/run_ci.py full` 包含 Rust 全目标测试、Clippy、前端生产依赖审计、`pnpm check` 与两套隔离 Playwright。

### 6.3 生产门槛状态

| 门槛                | 当前状态           | 缺少的证据或实现                                                                                                                                                                  |
| ------------------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 浏览器 XSS 会话边界 | 已闭环             | access token 已改为仅驻留内存，Refresh Token 仍为 host-only HttpOnly Cookie；生产入口启用 enforce CSP，并用 Web Locks、版本化跨标签页结束信号闭环刷新轮换与退出同步               |
| 正式页面契约完备性  | 已闭环             | Module 多 View、统一 Action executor、custom/bulk、fail-closed 与显式产品外壳接口均有正式路由和矩阵测试证据                                                                       |
| 正式产物 E2E        | 已闭环             | 独立 Playwright 配置每次重建并启动 `dist/spa`，验证正式模块深链接、生产路由裁剪、无 dev runtime，以及静态/API 404 不被 history fallback 掩盖                                      |
| CI 浏览器门禁       | 已闭环（实现）     | `run_ci.py full` 串行执行隔离 dev 与 production Playwright；quality job 安装 Chromium 后复用同一 full 门禁；本提交未 push，远程 job 终态仍未验证                                  |
| 部署契约            | 已闭环（仓库合同） | 可执行 Nginx 配置、共享响应合同、生产构建 E2E 与变异测试覆盖 history fallback、安全头、HTML/资产缓存和严格 404；真实域名 TLS/边缘 smoke 仍须在首次部署后取证                      |
| 端到端可观测性      | 已闭环（仓库链路） | 已认证前端统一上报无敏感正文错误指纹，`related_request_id` 关联原 Action log/trace，低基数 metric 与真实 promtool firing/silent 演练入 CI；真实 Alertmanager 送达仍须目标环境取证 |
| 无障碍              | 已闭环（自动门禁） | 固定版本 axe 覆盖登录、角色、正式模块与表单对话框的 WCAG 2.2 AA 可自动检测规则；纯键盘旅程、可见焦点和对话框焦点恢复进入 dev E2E/full/CI                                          |
| 真实业务与规模      | 已闭环（本地基线） | 独立 `work` Addon、版本化迁移、1 万项目/5 万任务真实库基线、100 层树/100 条原子批量/十路并发和浏览器弱网乱序均有可重复测试；不外推为生产容量                                      |
| i18n                | 已闭环（单语言合同） | 首发明确只支持 `zh-CN`；HTML、Quasar、Intl 和排序归一受机械门禁约束，不宣称已支持多语言；第二语言/地区需求会自动重新打开为 P0 门槛                                                |

生产就绪必须按目标部署环境逐项通过这些门槛，不能用维度平均分替代。

## 七、把目标变成可执行验收

### 7.1 “约定式交付目标达成”的通过条件

只有以下检查全部通过，才能把问题 1 从“条件性达成”改成“达成”：

1. **已满足（2026-07-31）：** 独立 `work` Addon 包含 project/task 关系、父任务树、批量完成和 outline/backlog 双 View；前端改动仅为通用排序 wire 适配、通用确认框语义与测试，没有 `work` 业务分支。
2. **已满足（2026-07-31）：** 新 Module 只通过后端 presentation 出现在个人身份空间和导航中，前端没有新增模块 ID、operation id、字段名或状态值特判。
3. **已满足（2026-07-31）：** `/module/:moduleId` 能通过标签页访问全部 View，不再只取 `views[0]`。
4. **已满足（2026-07-31）：** 统一执行器单测覆盖 toolbar/row/bulk × form/invoke/download/preview/navigate/custom，正式 `/module` 浏览器用例实际覆盖三个 placement 和六类 interaction 的执行/安全分支，不使用 `/workbench` 结果替代。
5. **已满足（2026-07-31）：** 未授权 Module、Module 未授权的 Action 引用与 hidden presentation 均 fail-closed；disabled 只呈现原因且不可执行。
6. **已满足（2026-07-31）：** custom `view_id` 未注册时正式 Module/Business 页面安全降级；动态加载失败也走同一异常回退分支。
7. 如果目标坚持“自定义页面只新增一个文件”，必须引入构建时静态 manifest/codegen，并增加后端 view id 与前端 manifest 的交叉校验；在此之前验收口径保持“一个新文件 + 一个 registry 修改”。
8. **已满足（2026-07-31）：** Git diff 中没有 `work` 前端业务实现；隔离 E2E 从 Catalog 构造正式 `/module/work.task`，真实解释 100 节点树、弱网查询和 100 条 bulk presentation。

### 7.2 “生产就绪”的通过条件

1. **已满足（2026-07-31）：** access token 内存化；enforce CSP 下已验证登录、刷新、上传和静态自定义组件；多标签页并发刷新与退出同步已有浏览器对抗测试。
2. **已满足（2026-07-31）：** Playwright 在本地 full 与 CI 中使用两组专用且互斥的端口，两个配置都禁止复用既有服务；日常手工调试只有显式设置 `YANG_E2E_REUSE_EXISTING_SERVER=true` 才允许复用。
3. **已满足（仓库合同，2026-07-31）：** 浏览器 smoke 构建并启动 `dist/spa`，验收深链接、响应头 CSP/`frame-ancestors`/HSTS 等安全头、HTML `no-store`、`/assets` 一年 immutable、严格资产 404 和 API 代理边界；真实域名的 TLS/证书/DNS/边缘终态只可在目标环境发布后验收。
4. 正式 `/module` 与 `/business` 覆盖多身份、租户切换、权限变化、全 interaction、失败重试和会话过期。
5. **已满足（仓库链路，2026-07-31）：** 前端错误上报携带原后端 request id；后端 `frontend.error` 日志、Action trace 与低基数 metric 可关联；promtool 演练验证达到阈值 firing、低于阈值 silent。真实 Alertmanager 接收器送达仍属于目标环境验收。
6. **已满足（自动门禁，2026-07-31）：** 登录、角色、正式模块和表单对话框通过 axe WCAG 2.2 AA 扫描；纯键盘完成登录、角色选择、模块操作，关键控件有可见焦点，对话框打开后聚焦且关闭后恢复触发点。屏幕阅读器、高对比度和语音控制仍按目标用户/采购规范做环境人工验收。
7. **已满足（本地基线，2026-07-31）：** `.ecc/benchmarks/work-scale.json` 固化 1 万项目/5 万任务环境、SLO 和实测值；真实库测试覆盖第 500 页、relation options、100 节点上限、100 条原子批量和十路并发。
8. **已满足（产品合同，2026-07-31）：** 当前发布范围明确为单语言 `zh-CN`，HTML、Quasar 与 locale-sensitive API 均固定同一 locale；第二语言/地区格式触发条件和完整重开验收已写入 `frontend/docs/LOCALE.md`。
9. **已满足（本地终态，2026-07-31）：** `python scripts/run_ci.py full` 与隔离 MySQL/Redis 的 `integration` 均从头通过；远程 CI 每个 job 的终态仍须在 push 后取证，不能用本地结果代替。

## 八、优先改进路径

### P0：统一正式页面解释器（已闭环）

1. `ModulePage` 与 `TableView` 已复用 `usePresentedActions()`，不再维护第二套简化 Action 流程。
2. Module 已提供多 View 标签选择，禁止静默丢弃 `views[1..]`。
3. `org.tenant` 进入显式产品外壳扩展，字段标题和显示类型由输出 schema 推导；ModulePage 不再包含具体模块 ID、字段名或状态值特判。
4. custom 安全降级已接入 ModulePage。

### P0：把 E2E 变成可信门禁

1. CI quality job 已通过 `run_ci.py full` 执行 dev 与 production Playwright，并预装 Chromium 与系统依赖；
2. 本地与 CI 的两套 Playwright 使用互斥专用端口且禁止复用旧服务；
3. 生产构建、深链接、安全响应头、HTML/资产分层缓存、严格资产 404 与 API 边界 smoke 已纳入 `e2e:production` 和部署合同校验。

### P0：闭环浏览器安全与诊断

1. access token 内存化与 enforce CSP 已联合闭环；
2. 前端无敏感正文错误指纹、原 request id、后端 Action log/trace、低基数指标和告警 firing/silent 演练已形成仓库内完整诊断链。

### P1：证明通用渲染上限（已闭环本地基线）

`work` Addon 已用真实 MySQL 数据量、关系、树、批量、并发和浏览器弱网对抗完成本地基线；后续新增业务形态仍须按同一方法重新取证。

### P2：按产品需求补齐（i18n 范围已决策）

i18n 当前按 `zh-CN` 单语言产品合同交付；第二语言或地区格式触发时按
`frontend/docs/LOCALE.md` 重新开门。主题等其余能力仍应由明确业务需求触发，不因
“生产级”标签机械引入。

## 九、本次验证快照

本次文档修改后已重新运行：

```powershell
# 前端格式、Lint、类型、单测、生产构建与产物检查
pnpm --dir frontend check

# 使用空闲端口运行 demo backend + dev frontend 的隔离 Chromium E2E
$env:YANG_E2E_FRONTEND_PORT="5188"
$env:YANG_E2E_BACKEND_PORT="18088"
pnpm --dir frontend e2e

# 文档格式与差异
git diff --check
git diff -- docs/assessments/2026-07-30-convention-driven-goal-assessment.md
```

实测结果：

- `pnpm --dir frontend check`：通过；Prettier、ESLint、`vue-tsc`、20 个 Vitest 文件/83 项测试、Quasar SPA 生产构建均成功；产物检查扫描 22 个文件，未发现 Workbench chunk 或公开 source map。
- 隔离 Chromium E2E：通过，18/18，耗时 34.8 秒；验证对象仍是 demo backend + Quasar dev server。
- `git diff --check`：通过；本文档没有空白错误。

注意：

- `playwright.config.ts` 已改为默认 `reuseExistingServer: false`；只有手工显式设置 `YANG_E2E_REUSE_EXISTING_SERVER=true` 才允许调试复用，full 门禁不会设置该开关。
- 本评估没有把未在本次运行的 `python scripts/run_ci.py full`、真实 MySQL/Redis integration 或远程 CI 状态写成当前通过。
- 文档优化不会自动改变代码能力；只有 §7 的验收条件真实通过后，才可以升级结论。

### 9.1 2026-07-31 增量闭环：浏览器 XSS 会话边界

本项采用的安全不变量是：Access Token、Refresh Token 原文都不得进入 Web Storage；路由认证事实只能来自当前标签页的内存状态或同源 HttpOnly Refresh Cookie 恢复结果；跨标签页协议只能传递版本化会话结束元数据，不得传递 Token。

实现证据：

- `frontend/src/api/auth-session.ts` 把 Access Token 改为模块内存状态，启动时主动删除旧 `yang.token`/`yang.refresh-token`，终态刷新失败清空会话上下文；
- `frontend/src/stores/session.ts` 不再从 `sessionStorage` 初始化认证状态，路由进入登录/角色/受保护页面前只通过 HttpOnly Cookie 尝试恢复；
- 同源多标签页刷新使用 Web Locks 串行化，避免两个标签页同时轮换同一 Refresh Cookie；不支持 Web Locks 的浏览器仍保持 fail-closed，最坏结果是竞争失败的标签页重新登录，不会接受未验证 Token；
- `frontend/src/api/session-coordination.ts` 以每标签页 sender id、事件 id 和版本校验同步 logout/expired；BroadcastChannel 与 `storage` fallback 只发送结束原因，不发送凭据；
- `frontend/index.html` 的 enforce CSP 不包含 `unsafe-eval` 或外部脚本源；`verify-production-build.mjs` 会在 CSP 缺失、关键指令缺失或放开外部协议时让生产构建失败。

对抗性验证：

```powershell
# 内存令牌、Web Locks 回调参数、伪造旧 Token、跨标签页畸形消息
pnpm --dir frontend test

# enforce CSP、伪造 Web Storage Token、刷新轮换、双标签页并发与退出同步
$env:YANG_E2E_FRONTEND_PORT="5197"
$env:YANG_E2E_BACKEND_PORT="18097"
pnpm --dir frontend e2e

# CSP 与生产产物边界
pnpm --dir frontend build
pnpm --dir frontend verify:production-build
```

浏览器对抗结果为 20/20，其中上传与静态 `view_id` 自定义组件都在同一 enforce CSP 下通过；双标签页用例同时证明 Refresh 请求最大并发数为 1、两个标签页都没有持久化 Access Token，且一处退出会驱动另一处回到登录页。红绿证据也已保留：旧实现会在 4 个凭据持久化断言上失败；首次浏览器运行又分别暴露了 Web Locks 回调把 `Lock` 误传为 `AbortSignal`、同页 BroadcastChannel 回环两个竞态，修复后对应回归均通过。

本项闭环不外推为“任意已执行脚本都无法读取内存”：一旦可信 bundle 或允许执行的同源脚本自身失陷，运行时凭据仍可能被截获。当前 CSP 关闭的是内联/外部脚本注入面，依赖供应链与 DOM sink 仍需持续审计。生产服务器的响应头 CSP、`frame-ancestors`、history fallback 和缓存头仍属于“部署契约”门槛，本项没有提前把该门槛记为完成。

### 9.2 2026-07-31 增量闭环：正式页面契约完备性

本项采用的不变量是：正式 `/module/:moduleId` 不得静默丢弃合法 View；同一份 Module presentation 在 View 和 primary Action 页面中必须走同一 Action executor；任何未获 Module 授权、hidden 或未知 custom 引用都不得升级为可执行操作；通用页面不得包含具体模块 ID、字段名或状态值分支。

实现证据：

- `frontend/src/module-pages.ts` 只接受同时出现在 `module.actions` 中的 presentation，并把合法 Module presentation 合并到当前 View；
- `frontend/src/pages/ModulePage.vue` 提供多 View 标签页，View 路径复用 `TableView`，无 View 路径复用 `usePresentedActions()`；两条路径共享 form/invoke/download/preview/navigate/custom、row/bulk、确认、附件和重载语义；
- `frontend/src/product-shell/module-extensions.ts` 是产品外壳扩展边界；`org.tenant` 不再散落在通用页面，字段标题与显示类型从 Action 输出 JSON Schema 推导；
- `TableActionDialog` 恢复语义化二级标题，Module 正式页保留 presentation 的业务提交文案；Business/Workbench 的既有通用表单文案保持不变。

对抗性验证：

```powershell
# 红测：旧实现找不到第二个 View 标签，定向用例预期失败
$env:YANG_E2E_FRONTEND_PORT="5198"
$env:YANG_E2E_BACKEND_PORT="18098"
pnpm --dir frontend exec playwright test e2e/formal-shell.spec.ts -g "正式模块页解释多 View"

# 完整静态门禁、91 项 Vitest、生产构建与产物检查
pnpm --dir frontend check

# 隔离 demo backend + dev frontend 的全部正式/开发路径浏览器回归
$env:YANG_E2E_FRONTEND_PORT="5206"
$env:YANG_E2E_BACKEND_PORT="18106"
pnpm --dir frontend e2e

git diff --check
```

红测稳定失败在 `Alpha 项目` 标签不存在；实现后定向用例走过 Alpha/Beta 切换、row form、bulk invoke、下载、预览、navigate 安全分支、已注册/未注册 custom、hidden/disabled 与未获 Module 授权的 Action 引用。全量结果为前端 21 个测试文件/91 项 Vitest、生产构建和 23 文件产物检查全部通过，隔离 Chromium E2E 21/21 通过。

对抗回归还发现统一弹窗最初把视觉标题保留为普通 `div`，导致既有正式账号空间失去 heading 语义；修复为 `h2` 并按页面策略恢复业务提交文案后，`account-spaces` 与全量 E2E 均通过。navigate 的浏览器分支仍遵守 Fetch 对手动 302 的安全边界：Location 不可见时不得猜测或跳转；执行器对可验证 location 的重定向分派由矩阵单测覆盖。

本项不外推为“约定式交付目标已经普遍达成”：独立真实业务 Addon、严格只新增一个 custom 文件和大数据规模仍是单独验收项；也不把 dev server E2E 当作正式产物或部署契约证据。

### 9.3 2026-07-31 增量闭环：正式产物 E2E

本项采用的不变量是：浏览器测试必须消费本次命令刚生成的 `dist/spa`，不能复用 Quasar dev server 或既有端口；合法 history 深链接必须返回 SPA 入口并启动正式路由；带扩展名的缺失静态资源和 API 404 不得被 fallback 伪装为 200 HTML；DEV-only Workbench 与 Vite runtime 不得出现在运行路径。

实现证据：

- `frontend/playwright.production.config.ts` 使用独立测试目录、结果目录和端口变量，`reuseExistingServer: false`；每次先执行 `pnpm build`，再启动产物服务器和 demo backend；
- `frontend/scripts/serve-production-build.mjs` 只读取 `dist/spa`，对页面路由执行显式 history fallback，对 `/assets` 缺失文件严格返回 404，并把 `/api`、`/.well-known`、`/health` 转发到同源后端；
- `frontend/e2e-production/production-build.spec.ts` 从 `/module/account.user` 深链接启动正式模块，检查脚本不包含 Vite dev runtime；再验证 `/workbench` 被生产路由移除、缺失 JS 返回 404、缺失 API 保持后端 404 而不是首页 HTML。

对抗性验证：

```powershell
$env:YANG_PRODUCTION_E2E_FRONTEND_PORT="5305"
$env:YANG_PRODUCTION_E2E_BACKEND_PORT="18305"
pnpm --dir frontend e2e:production
```

红测先在 Vite preview 上真实进入断言并出现两个失败：深链接响应没有可审计 fallback 标记，且缺失 JS 被 history fallback 错误返回 200 HTML。替换为边界明确的产物服务器后，首次绿测又暴露 `account.user` 被 `path.extname()` 误判为静态扩展；最终只把 `/assets` 作为严格静态资源命名空间，并增加 `/module/report.json` 反例，2/2 通过。全门禁还发现 Vitest 会误收集新 Playwright 目录；`vitest.config.ts` 显式排除 `e2e-production/**` 后，21 个测试文件/91 项 Vitest 与生产构建重新通过。

本项当时没有把测试夹具服务器写成目标环境的生产流量服务器，也没有提前关闭部署契约；后续 §9.5 以独立共享合同、可执行 Nginx 配置、浏览器响应断言和 Nginx 运行 smoke 补齐该门槛。

### 9.4 2026-07-31 增量闭环：CI 浏览器门禁实现

本项采用的不变量是：本地 `full` 与 GitHub quality job 必须消费同一浏览器命令集合；dev 与 production 两套 E2E 必须使用不同专用端口并禁止复用既有服务；CI 必须在运行前安装 Playwright Chromium 与系统依赖；自测必须在任一步骤被删除时 fail-closed。

实现证据：

- `scripts/run_ci.py` 的 `FULL` 新增 `Frontend isolated dev-server E2E` 和 `Frontend isolated production-build E2E`，命令分别固定使用 5310/18310 与 5311/18311，并设置 `CI=true`；
- `Command.env` 让端口和 CI 语义成为门禁声明的一部分，而不是依赖调用者手工设置；
- `frontend/playwright.config.ts` 默认禁止 `reuseExistingServer`；只有显式调试开关才允许复用；
- `.github/workflows/ci.yml` 的 quality job 在 `run_ci.py full` 前执行 `playwright install --with-deps chromium`；
- `run_ci.py --self-test` 交叉检查 full 中的两个命令、两组互斥端口、执行顺序和 workflow 的浏览器安装步骤。

对抗性验证：

```powershell
# 新合同断言加入后，旧 FULL 预期失败
python scripts/run_ci.py --self-test

# 与 FULL 完全一致的 dev E2E 环境
$env:CI="true"
$env:YANG_E2E_FRONTEND_PORT="5310"
$env:YANG_E2E_BACKEND_PORT="18310"
pnpm --dir frontend e2e

# 与 FULL 完全一致的 production E2E 环境
$env:YANG_PRODUCTION_E2E_FRONTEND_PORT="5311"
$env:YANG_PRODUCTION_E2E_BACKEND_PORT="18311"
pnpm --dir frontend e2e:production
```

红测失败信息为“full 门禁必须同时执行 dev-server 与 production-build Playwright”；实现后合同自测通过，CI 语义下 dev E2E 21/21、production E2E 2/2 通过。两套 Playwright 都启动并关闭了自己的 demo backend/frontend，结束后对应端口没有监听进程。

本项只证明门禁定义与本地等价执行已闭环。本提交按用户要求只创建 Git commit、没有 push，因此不能声称 GitHub Actions quality job 已取得远程成功终态；该证据必须在后续推送后逐 job 检查。

### 9.5 2026-07-31 增量闭环：生产部署合同

本项采用的不变量是：页面入口和 history fallback 必须 `no-store`；Vite `/assets` 命名空间必须长期 immutable 且缺失文件严格 404；后端路径不得落入 SPA fallback；CSP 必须由响应头提供并包含 `frame-ancestors 'none'`；应用边缘只绑定 loopback，公网 HTTPS、转发头覆盖和 TLS 终态由受信边缘承担。

实现证据：

- `frontend/deploy/deployment-contract.mjs` 集中定义 CSP、HSTS、COOP、Permissions Policy、Referrer Policy、`nosniff`、拒绝 framing 和两类缓存策略；
- `frontend/scripts/serve-production-build.mjs` 消费共享合同，生产构建 E2E 因而验证浏览器实际收到的响应，不只检查配置文本；
- `frontend/deploy/nginx.conf` 是完整、可执行的 Nginx 主配置：loopback listener、静态根目录、后端代理边界、严格 `/assets`、history fallback、安全头和 URI 分层缓存都显式声明；
- `frontend/scripts/verify-deployment-contract.mjs` 把 Nginx 与共享合同交叉校验，并主动破坏 `frame-ancestors`、immutable、资产 404、history fallback、loopback 监听、转发协议白名单和被注释掉的伪指令，七种变异都必须被拒绝；
- GitHub quality job 使用版本与摘要双重固定的 Nginx 官方镜像执行 `nginx -t`，`run_ci.py --self-test` 防止该真实语法检查被静默移除；
- `frontend/deploy/README.md` 固化 TLS 终止层的前置条件和首次真实发布 smoke，明确 HSTS 在本地 HTTP 上只能验证响应合同、不能伪装成浏览器已执行。

对抗性验证：

```powershell
# 红测：临时删除 Nginx CSP 的 frame-ancestors 后，合同必须 fail-closed
pnpm --dir frontend verify:deployment-contract

# 合同交叉校验与七类内置破坏性变异
pnpm --dir frontend verify:deployment-contract

# 独立正式产物浏览器回归
pnpm --dir frontend e2e:production

# 将 dist/spa 挂入真实 Nginx 解析配置
docker run --rm `
  -v "${PWD}\frontend\deploy\nginx.conf:/etc/nginx/nginx.conf:ro" `
  -v "${PWD}\frontend\dist\spa:/usr/share/nginx/html:ro" `
  nginx:1.30.4-alpine3.24@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46 `
  nginx -t
```

红测稳定失败为“Nginx 缺少生产响应头 Content-Security-Policy”，并打印缺失的完整 enforce CSP；恢复后合同校验通过且内置 7/7 破坏性变异全部被拒绝。生产 Chromium 2/2 通过，并验证深链接响应的完整安全头、HTML `no-store`、真实构建 JS 的 immutable 与非 fallback。CI 和本地语法检查都使用版本与摘要双重固定的 `nginx:1.30.4-alpine3.24@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46`，`nginx -t` 通过；容器内运行 smoke 取得深链接 200 + `no-store`、JS 200 + immutable、缺失 JS 404。

本项闭环的是仓库内可交付部署合同，不虚构尚不存在的公网环境。真实域名的证书链、HTTP 到 HTTPS 重定向、HSTS 浏览器执行、DNS/CDN 和远程 smoke 必须在选定目标并发布后取得终态；它们属于环境发布证据，不再是仓库缺少实现。

### 9.6 2026-07-31 增量闭环：端到端错误可观测性

本项采用的不变量是：已被页面捕获的 API 错误也必须进入统一诊断链；浏览器不得上报错误正文、堆栈、URL/query、body 或凭据；原后端 `request_id` 必须作为独立关联字段保留；指标标签只能使用有限枚举；告警规则必须由真实 Prometheus 解析并同时证明 firing 与 silent 边界。

实现证据：

- `frontend/src/observability/error-reporter.ts` 统一规范 API/合同/网络/Vue/全局错误，只在内存 Access Token 存在时发送；事件包含 UUID、route name、稳定 operation、16 位十六进制指纹、status/code 和可选 `related_request_id`，错误正文和非白名单 Error name 不参与指纹，同指纹 10 秒内去重且去重表上限 256 项；
- `frontend/src/boot/observability.ts` 接入 Vue error handler、`window.error` 和 `unhandledrejection`；API client 对页面正常捕获的失败也主动上报，避免只覆盖 uncaught exception；
- `system.observability.report_frontend_error` 是非公开 Action，输入 `deny_unknown_fields` 并对白名单字符和长度 fail-closed；后端日志同时记录上报请求自己的 `request_id` 与原请求 `related_request_id`，指标只使用 `kind/linked`，每用户每实例每分钟 30 次和最多 4096 个活跃用户的边界防止已认证日志放大；
- `ops/prometheus/yang-system.rules.yml` 增加 `YangSystemFrontendErrorBurst`，测试文件同时模拟五分钟 5 次 firing 与 4 次 silent；
- GitHub quality job 以版本和摘要固定的 Prometheus 3.11.3 官方镜像执行 `promtool check rules` 与 `promtool test rules`，本地 CI 自测防止演练被静默删除。

对抗性验证：

```powershell
# 红测一：契约错误未保留显式 request id，且正文/Error.name 会改变指纹
pnpm --dir frontend exec vitest run src/observability/error-reporter.test.ts

# 红测二：只有演练、没有告警规则时，promtool 得到 got: []
docker run --rm --entrypoint /bin/promtool `
  -v "${PWD}\ops\prometheus:/rules:ro" -w /rules `
  prom/prometheus:v3.11.3@sha256:e4254400b85610324913f0dc4acf92603d9984e7519414c5a12811aa6146acc3 `
  test rules yang-system.rules.test.yml

# 单元、真实浏览器 request-id 关联与 Rust 输入边界
pnpm --dir frontend exec vitest run src/observability/error-reporter.test.ts
$env:CI="true"
$env:YANG_E2E_FRONTEND_PORT="5312"
$env:YANG_E2E_BACKEND_PORT="18312"
pnpm --dir frontend exec playwright test e2e/observability.spec.ts
cargo test --locked --lib

# 真实规则解析和 firing/silent 演练
promtool check rules yang-system.rules.yml
promtool test rules yang-system.rules.test.yml
```

前端红测在变化的敏感正文与伪造 `Error.name` 上得到 `expected false, received true`，证明旧指纹会被不可信错误文本改变；告警红测明确显示期望 `YangSystemFrontendErrorBurst`、实际 `got: []`。实现后 reporter 5/5、前端全门禁 22 个测试文件/96 项 Vitest、浏览器关联 2/2、Rust observability 3/3 与完整 lib 99 通过/3 个真实依赖用例按设计 ignored。浏览器证明 Bearer Token 只进入 Authorization header，API、成功 HTTP 后的合同错误与全局运行时事件都不含敏感 message/details，却保留可用的 32 位 `related_request_id`；单测还覆盖显式 request id 关联、敏感正文与伪造 Error.name 不改变指纹，以及没有 `crypto.randomUUID` 的旧目标浏览器回退。Rust 对抗测试证明第 31 次同用户上报被限流而另一用户不受影响。全量 dev E2E 23/23、production E2E 2/2 通过。Prometheus 3.11.3 `check rules` 解析 14 条规则，firing/silent 演练全部通过。

本项不把规则单测写成真实值班送达：Alertmanager receiver、通知渠道、升级路径和告警恢复时间线必须在目标环境演练。仓库内从浏览器错误到 request id、Action log/trace、metric 和告警阈值的实现与可执行证据已经闭环。

### 9.7 2026-07-31 增量闭环：关键旅程无障碍

本项采用的不变量是：自动扫描不能代替键盘行为，键盘可达也不能代替语义与对比度；门禁必须同时覆盖登录、角色选择、正式模块和表单对话框，且对话框关闭后焦点必须回到原触发点。

实现证据：

- 固定 `@axe-core/playwright@4.12.1`，`e2e/accessibility.spec.ts` 对关键页面和对话框运行 WCAG 2.0/2.1/2.2 A/AA 标签；该 spec 由默认 `pnpm e2e` 自动进入 full 与 CI；
- 纯键盘用例通过 Tab/Enter/Escape 完成登录、角色选择、模块操作和对话框关闭，不使用鼠标捷径；
- 全局双层 3px `:focus-visible` 焦点环在浅色与深色背景都有可见边界，测试直接断言关键控件的计算样式；
- 模块导航改为具名 navigation 地标，避免 list 角色包含非法 `nav` 子节点；低对比度导航标题、连接状态、模块说明和表单帮助文字已改用 AA 颜色；
- `TableActionDialog` 提供可访问名称，在完成显示后显式聚焦关闭按钮，并依赖对话框焦点管理把焦点恢复到触发按钮；卡片建立独立层叠上下文，保证遮罩不会改变内容对比度判定；
- `frontend/docs/ACCESSIBILITY.md` 固化自动门禁、组件约束和人工辅助技术验收边界。

对抗性验证：

```powershell
# 红测一：依赖不存在时，Playwright 无法加载 @axe-core/playwright
$env:CI="true"
$env:YANG_E2E_FRONTEND_PORT="5315"
$env:YANG_E2E_BACKEND_PORT="18315"
pnpm --dir frontend exec playwright test e2e/accessibility.spec.ts --retries=0

# 红测二：接入 axe 后真实检出 aria-required-children 与三处 color-contrast
# 红测三：语义/对比度修复后，键盘打开的对话框仍因缺少可访问名称无法按名称定位

# 绿测：不允许重试掩盖失败
$env:YANG_E2E_FRONTEND_PORT="5319"
$env:YANG_E2E_BACKEND_PORT="18319"
pnpm --dir frontend exec playwright test e2e/accessibility.spec.ts --retries=0
```

首个红测报 `Cannot find package '@axe-core/playwright'`，证明门禁此前不存在。安装固定版本后，axe 报出模块导航 `aria-required-children` critical，以及导航标题、连接状态、模块说明的 `color-contrast` serious；这些违规在修复后清零。随后键盘确实打开了对话框，但可访问树只显示匿名 `dialog`，具名 locator 稳定失败；补齐名称后，计算样式断言又证明 Quasar 的 `no-outline` 优先级会静默吞掉焦点环。覆盖组件库抑制规则并补齐焦点合同后，关键旅程无重试通过。

最终专项无重试 2/2、前端 `check`（含 22 个测试文件/96 项 Vitest、生产构建与 7 个部署合同变异）通过；完整 dev E2E 在禁用重试后 25/25，production E2E 2/2 通过。

该闭环只声明仓库自动门禁覆盖的 WCAG 可检测规则与键盘/焦点行为，不把 axe 结果外推为完整合规认证。屏幕阅读器、200%/400% 缩放、Windows 高对比度和语音控制的人工矩阵，只有在目标用户、采购或法规范围确定后才能形成环境终态证据。

### 9.8 2026-07-31 增量闭环：真实业务 Addon 与规模基线

本项采用的不变量是：规模证据必须来自生产同构 Module、Action、迁移和真实 MySQL/Redis，
不能用纯前端 fixture 或 demo 表替代；个人工作区只能由 Token actor 决定，项目、父任务和
批量 ID 必须重复受同一 tenant scope 约束；页面、关系选择和批量上限必须先拒绝越界，再
测量合法上限；本地延迟只作为可重复回归基线，不写成生产容量承诺。

实现证据：

- 新增独立 `work` Addon：`work.project` 提供个人项目 CRUD、关系 options 和一个 View；
  `work.task` 提供项目关系、自关联父任务、outline 树与 backlog 清单双 View，以及
  add/put/del/complete 的 toolbar/row/bulk presentation；
- `PersonalWorkspaceResolver` 把已认证 `user_id` 解析为唯一 tenant capability，并拒绝
  伪造其他 `x-tenant-id`；两张业务表都把 `owner_user` 声明为 tenant key，标准 CRUD、
  relation options 与自定义 repository 复用同一 scope；
- task writer 校验项目 owner、父任务 owner/项目一致性和 100 层递归防环；批量完成只接受
  1..=100 个唯一正整数 ID，事务内先验证全部可见，再全有或全无更新；
- `20260731_0008_create_work_project` 和 `0009_create_work_task` 固化分页/关系索引及
  owner/project/parent 复合外键；迁移 job 的 dry-run、apply、running 恢复、checksum 和
  schema drift 继续 fail-closed；
- 前端没有 `work` Module、operation、字段或状态特判。真实库首次对抗反而暴露 Catalog
  的小写展示排序与 Rust Action 输入 `Asc/Desc` wire 值不一致；`useTableQuery` 现统一在
  API 边界序列化。批量确认又暴露 Quasar ad-hoc dialog 无可访问名称，现改用具名、初始
  聚焦明确的通用确认组件；
- `.ecc/benchmarks/work-scale.json` 记录机器、依赖、数据集、SLO、命令和实测值，避免
  只在文档中留下不可重放的数字。

对抗性验证：

```powershell
# 编译红灯曾真实检出子模块可见性过窄和 resolver 类型不符合中间件契约
cargo check --all-targets

# 真实 MySQL 8 / Redis 7：1 万项目、5 万任务与安全负例
$env:YANG_SYSTEM_TEST_DATABASE_URL="mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL="redis://127.0.0.1:6379/15"
cargo test --test system_integration work_addon_scale_and_adversarial_boundaries_hold `
  -- --ignored --nocapture --test-threads=1

# 版本化迁移的首次执行、重跑、中断恢复和漂移阻断
cargo test --test migration_job_integration -- --ignored --nocapture --test-threads=1

# 通用前端门禁与无重试弱网/批量浏览器矩阵
pnpm --dir frontend check
$env:CI="true"
$env:YANG_E2E_FRONTEND_PORT="5320"
$env:YANG_E2E_BACKEND_PORT="18320"
pnpm --dir frontend e2e
```

真实库第一轮在合法数据写入后拒绝小写 `asc`，错误为
`unknown variant 'asc', expected 'Asc' or 'Desc'`；修复通用 wire 适配后，同一测试写入
1 万项目和 5 万任务并通过：seed 3046ms，第 500 页 20 次样本 p95 113ms，一万项目关系
搜索 p95 13ms，100 条批量完成 19ms，十路并发分页总耗时 72ms。安全负例同时证明
`page_size=101`、100 层关系环、伪造个人 tenant 和混入其他工作区 ID 的批次都失败，且
失败批次没有部分提交。

浏览器红测先证明确认框虽视觉可见，但可访问树中的 `dialog` 没有名称；具名通用确认组件
落地后，正式 `/module/work.task` 在旧请求延迟 700ms、新请求延迟 30ms 的乱序条件下始终
保留新结果，100 节点树全部渲染，表头全选生成恰好 100 条 `selected` 并完成确认。最终
前端 `check` 为 22 个测试文件/96 项 Vitest、26 文件生产产物和 7 个部署合同变异全部通过；
完整 dev E2E 禁用重试后 26/26 通过。迁移真实库矩阵也通过首次 apply、可重入恢复和结构
漂移阻断。

本项完成的是仓库内约定式交付样本和本地回归基线。数据在 loopback Docker 上运行，未包含
公网 RTT、多副本争用、生产数据分布、连接池配额或容量拐点，因此不能据此承诺生产吞吐；
这些值只能作为后续提交不得显著退化的基准。

### 9.9 2026-07-31 增量闭环：单语言产品合同

本项采用的不变量是：没有第二语言需求时，不应仅为“生产级”标签引入翻译框架；但
“按需处理”也不是可审计结论。当前必须明确支持范围、固定所有 locale-sensitive 行为，并
让第二语言或地区格式需求自动重新打开发布门槛。单语言合同不能被写成“已支持 i18n”。

实现证据：

- `frontend/docs/LOCALE.md` 把首发范围定义为仅 `zh-CN`，明确不读取浏览器首选语言、
  不提供运行时切换、不支持 `Accept-Language`，同时列出第二语言、地区格式、服务端多语言
  和无障碍多语言四类重开触发器；
- HTML 已声明 `lang="zh-CN"`，Quasar 固定 `lang: "zh-CN"`；
  `src/product-locale.ts` 成为大小写归一和文本排序的唯一产品 locale 所有者，日期格式也
  复用同一常量；
- Catalog 搜索、应用中心搜索和 Module 稳定排序不再调用依赖浏览器环境的无参数
  `toLocaleLowerCase()` / `localeCompare()`；
- `verify-locale-contract.mjs` 扫描生产源码并拒绝无 locale 的大小写、排序与 Intl 格式化，
  同时校验产品合同的 supported/runtime-switch/reopen-trigger 三个机器标记；
- `pnpm check` 显式执行该门禁，不能只靠文档评审发现 locale 漂移。

对抗性验证：

```powershell
pnpm --dir frontend verify:locale-contract
pnpm --dir frontend check
```

门禁实际执行 5 个破坏性变异：HTML 改为 `en-US`、Quasar 改为 `en-US`、产品 locale
改为 `en-US`、重新引入无参数 `toLocaleLowerCase()`、删除第二语言重开标记；五项均被拒绝。
完整前端门禁继续覆盖格式、Lint、类型、96 项 Vitest、生产构建、产物与部署合同。

本项没有引入空壳翻译表，也不宣称 Catalog、后端错误和通知已经具备语言协商。出现
`LOCALE.md` 所列任一重开事实后，必须在上线前完成 locale 协商、Catalog 缓存隔离、
文案所有权、地区/时区格式、伪本地化、每语言 Playwright/axe 和人工辅助技术矩阵；届时
本行状态应恢复为未闭环，不能沿用当前单语言豁免。

### 9.10 2026-07-31 最终审计：先红后绿的全量与真实依赖门禁

最终审计没有复用各增量门槛的局部结果，而是在全部提交叠加后重新执行：

```powershell
python scripts/run_ci.py full

$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:***@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
python scripts/run_ci.py integration
```

第一次 `full` 在架构检查阶段失败，实际证明门禁是 fail-closed：任务关系并发修复新增的
workspace/project/task 行锁与 add/put 事务没有同步到租户数据路径清单，旧的无锁路径又成为
孤儿条目。修复后：

- `f94bbba` 将 add/put 的 workspace 锁、当前关系锁、项目/父任务锁与写入放进同一事务，
  并使代码标记与 `tenant-data-paths.md` 一一对应；
- `2275e8a` 使批量完成共享 workspace 锁，并在提交前要求 `affected == requested`，任何
  并发集合漂移都会整体回滚；
- `1e07614` 增加相反父关系并发写的真实库对抗：两个请求必须恰好一个成功，最终只保留
  一条边且无环。

修复后的 `full` 从头通过：架构自检与实际检查、Rust formatting/all-targets/all-features、
Clippy `-D warnings`、生产依赖审计、22 个文件共 96 项 Vitest、5 个 locale 破坏性变异、
生产构建与产物检查、7 个部署合同变异、26 项隔离 dev E2E 和 2 项 production-build E2E
全部成功。真实依赖 `integration` 共通过 10 项：Redis 单调缓存、outbox 并发 claim/retry/
过期 lease 重放、版本化迁移 plan/apply/retry/drift、schema apply 并发与失败重试、
bootstrap 单次信任根、两项租户隔离，以及账号/租户生命周期和 work 规模对抗。

针对最后三笔 work 并发修复，另以 `--nocapture` 重跑规模测试并通过：1 万项目、5 万任务、
100 层树，seed 3437 ms，第 500 页 p95 156 ms，1 万关系选项 p95 24 ms，100 条原子批量
23 ms，10 路并发 111 ms。该结果仍只是本机回归基线，不承诺生产容量。

本次未 push，因此远程 CI job 仍无新提交终态；真实域名 TLS/边缘 smoke、Alertmanager
接收器送达以及屏幕阅读器/高对比度/语音控制仍须在确定目标环境后取证。它们是环境发布
终态，不得被本地 `full` 或 demo backend E2E 替代。

## 十、最终结论

yang-system 的显式契约路线正确，后端 Catalog、权限投影、通用 TableView、表单和会话基础设施也已经形成可信骨架；在“显式声明一个或多个可用 View 与 presentation”的契约范围内，多 View 和模块级交互的零前端修改交付已经可行。

在显式契约范围内，约定式交付目标已有独立真实业务证据：`work` Addon 的关系、树、双 View 和批量 Action 只由后端 presentation 驱动，前端没有业务特判。仍不能把结论扩大为“任意 Action 自动进入正式页”，自定义页面也仍有“组件文件 + 静态 registry”两个手工触点；前端产品外壳继续显式持有账号/租户入口知识。

技术选型合理，不建议换框架。仓库内浏览器 XSS 会话边界、正式页面契约完备性、正式产物 E2E、CI 浏览器门禁实现、部署合同、端到端错误可观测性、关键旅程无障碍、真实业务规模基线和 `zh-CN` 单语言产品合同均已有本地对抗证据。当前仍应定义为“仓库门槛已闭环、等待环境终态”，而不是“已经完整生产就绪”：本提交未 push，远程 CI 每个 job 尚未验证；真实域名 TLS/边缘 smoke、Alertmanager 送达和人工辅助技术矩阵也只能在确定目标环境后取证。
