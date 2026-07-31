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

| 判断项 | 当前状态 | 准确结论 |
|---|---|---|
| 后端显式 UI 契约 | 已验证 | 字段、Action、View、Module presentation、权限投影、revision 和 ETag 已形成一条显式链路 |
| 显式 View 的默认表格页 | 已验证 | 后端声明可访问的 `data_action`、字段和 presentation 后，`TableView` 可在不修改前端的情况下渲染 |
| 仅新增 Module/CRUD 即自动出现可用表格页 | 未达成 | 框架生成的无 Action 默认 View 不会投影到 UI Catalog；必须声明可访问的数据 Action |
| 仅新增 Action/API 即自动进入正式页面 | 未达成 | Action 会进入请求级 Catalog，但只有被 Module/View presentation 引用后才进入正式工具栏、行或批量区 |
| 正式 `ModulePage` 完整解释全部合法契约 | 部分达成 | 当前只渲染首个 View，模块级 Action 未完整按 interaction 分派，并保留业务硬编码 |
| 自定义页面安全降级 | 已验证 | 静态 registry 未命中或加载失败时，`BusinessPage`/`WorkbenchPage` 保留通用页面 |
| 自定义页面只新增一个文件 | 未达成 | 当前需要新增组件文件并修改 `custom/registry.ts`，共两个手工触点 |
| Vue/Quasar/Pinia/Zod 技术选型 | 合理 | 与登录后的元数据驱动 SPA 匹配，当前没有足以抵消迁移成本的替代框架收益 |
| 完整生产就绪 | 未达成 | 已有较强工程基础，但浏览器安全闭环、正式产物 E2E、部署契约、端到端可观测性、a11y 和真实业务压力证据仍不完整 |

### 1.2 对两个评估问题的直接回答

**问题 1：条件性达成，不是普遍达成。**

- 已经达成的范围：后端新增 Module 时，同时声明 `ModulePresentationSpec`、带可访问 `data_action` 的 `ViewSpec`、字段和受支持的 Action presentation，前端可以零修改获得通用表格、查询、表单和 View Action。
- 尚未达成的范围：只注册 Module/CRUD、只新增 Action、一个 Module 包含多个 View、模块级 custom/download/preview/navigate/bulk、严格“只新增一个自定义文件”等场景。
- 因此不能写成“后端新增任意模块/API 都自动完成正式页面”，应写成“符合当前可渲染契约子集的后端变更可以零前端修改交付”。

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
- 不能把这种优势扩大为所有生产能力都更强。scs 已有 i18n 目录和 nginx 部署配置，而 yang-system 对应生产契约尚未闭环。

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
4. **现有业务样本有限。** 当前只有 account、admin、org 三个 Addon，不能用它们证明深层关系、大树、大数据量和复杂批量流程的上限。

## 五、前端：按运行路径评估，而不是混合计算

### 5.1 三条路径的职责

| 路径 | 构建状态 | 当前能力 | 不能据此证明的内容 |
|---|---|---|---|
| `/module/:moduleId` | 生产包含 | 正式账户空间、Module 标题、首个 View 或 primary Action、模块级 Action 对话框 | 多 View、完整 interaction 分派、Module 级 custom/bulk |
| `/business` | 生产包含 | Catalog 导航、通用 `TableView`、View Action、custom 安全降级 | 每个 Module 的正式账户空间行为 |
| `/workbench` | 仅 DEV | 全局 Action 演示、TableView、上传下载预览重定向、自定义 View 调试 | 生产包中的最终用户路径 |

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

### 5.3 正式 ModulePage 的当前限制

`frontend/src/pages/ModulePage.vue` 仍不是对 Catalog 的完备解释器：

- `ModulePage.vue:304-309` 只渲染 `modulePage.views[0]`；
- `ModulePage.vue:213-220` 的模块级 `openAction` 不检查 interaction，统一打开 `ActionDemo` 对话框；
- ModulePage 中的 `TableView` 没有处理 `custom-action`；
- 模块级 presentation 只分 row/toolbar，没有 bulk 路径；
- `ModulePage.vue:118-125`、`:137-167`、`:222-250`、`:363-374` 保留 `org.tenant`、字段名和状态值特判。

因此“placement × interaction 在正式模块页全部成立”不正确。

### 5.4 后端不是当前唯一 UI 事实源

除 Catalog 投影外，前端仍保存以下业务知识：

- `module-pages.ts:30-43` 的业务 icon token 映射；
- `ModulePage.vue` 的字段 label、状态格式和 `org.tenant` 特判；
- `components/account/AccountSwitcher.vue:44-70` 对 `"org"` 和 `/module/org.tenant` 的硬编码；
- `stores/tenant.ts:43-46` 对 `org.tenant.list` operation id 的硬编码。

这些代码可以作为产品外壳的显式特例存在，但存在时只能声称“后端是主要事实源”，不能声称“前端完全不认识具体业务”。

### 5.5 自定义页面的真实成本

当前安全机制是正确的：

- `custom/registry.ts` 是静态白名单，不根据后端字符串拼接 import；
- `BusinessPage` 与 `WorkbenchPage` 在未注册或加载失败时保留通用页面。

当前成本也必须如实记录：

- 一个自定义页面需要新增组件文件；
- 还需要修改 `custom/registry.ts`；
- 后端与前端 registry 不存在构建期交叉校验；
- `ModulePage` 尚未接入 custom 回退链。

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

| 选型 | 当前判断 | 原因 |
|---|---|---|
| Vue 3 Composition API | 保留 | 动态契约界面与可组合状态逻辑匹配 |
| TypeScript strict + vue-tsc | 保留 | 高动态 Catalog 边界仍需要静态约束 |
| Quasar CLI + Vite SPA | 保留 | 后台表格、表单、对话框和响应式组件覆盖足够；当前无 SEO/SSR 刚需 |
| Pinia | 保留 | session、identity、tenant、catalog、navigation、lifecycle 已有清晰 owner |
| Vue Router | 保留 | 参数化路由和访问策略满足当前规模 |
| Zod | 保留 | HTTP JSON 是运行时不可信边界，必须在使用前解析 |
| Vitest + Playwright | 保留并补门禁 | 工具链匹配；下一步重点是正式产物和 CI 隔离，不是替换测试框架 |
| TanStack Query、SSR、微前端 | 暂不引入 | 当前问题是契约完备性和生产门禁，不是缺少新框架 |

### 6.2 已有生产导向能力

- refresh token 使用 host-only `HttpOnly; SameSite=Strict` Cookie；HTTPS 下追加 Secure；
- 登录/刷新轮换 Cookie，登出撤销并清除 Cookie；浏览器 POST 校验 `Sec-Fetch-Site` 与 Origin/Referer/Host；
- 401 单飞刷新与原请求重放；terminal 失败清理会话；
- Catalog 运行时解析、ETag/304、取消和竞态保护；
- `pnpm check` 串联 Prettier、ESLint、vue-tsc、Vitest、生产构建和产物检查；
- 生产构建检查禁止公开 source map，并检查 Workbench 标记没有进入产物；
- `scripts/run_ci.py full` 包含 Rust 全目标测试、Clippy、前端生产依赖审计和 `pnpm check`。

### 6.3 尚未通过的生产门槛

| 门槛 | 当前状态 | 缺少的证据或实现 |
|---|---|---|
| 浏览器 XSS 会话边界 | 未闭环 | access token 仍在 `sessionStorage`；缺少 enforce 模式 CSP 和多标签页策略 |
| 正式页面契约完备性 | 未闭环 | 多 View、Module interaction、custom/bulk 与业务特例尚未收敛 |
| 正式产物 E2E | 未闭环 | Playwright 当前启动 Quasar dev server 和 demo backend，不测试 `dist/spa` 部署行为 |
| CI 浏览器门禁 | 未闭环 | `run_ci.py full` 与当前 GitHub Actions quality job 不执行 Playwright |
| 部署契约 | 未闭环 | history fallback、安全头、HTML/静态资源缓存策略和深链接 smoke test 未入门禁 |
| 端到端可观测性 | 部分具备 | 后端已有 tracing/request id 等基础信号，前端也提取 request id；尚无统一错误上报、关联检索和告警验收 |
| 无障碍 | 未闭环 | 无 axe 等自动检查，也没有键盘/焦点关键旅程门禁 |
| 真实业务与规模 | 未验证 | 深层 relation、大树、大分页、复杂批量、弱网和并发边界尚无基线 |
| i18n | 按产品需求决定 | 当前文案硬编码中文；只有明确多语言需求时才是上线阻塞项 |

生产就绪必须按目标部署环境逐项通过这些门槛，不能用维度平均分替代。

## 七、把目标变成可执行验收

### 7.1 “约定式交付目标达成”的通过条件

只有以下检查全部通过，才能把问题 1 从“条件性达成”改成“达成”：

1. 新增一个独立真实业务 Addon，包含关系字段、树 View、批量 Action、至少两个 View；除自定义扩展外，`frontend/` 无业务代码 diff。
2. 新 Module 只通过后端 presentation 出现在身份空间和导航中，不增加前端模块 ID、operation id、字段名或状态值特判。
3. `/module/:moduleId` 能访问全部 View，而不只 `views[0]`。
4. 在正式路由验证 toolbar/row/bulk × form/invoke/download/preview/navigate/custom 的受支持矩阵；不使用 `/workbench` 结果替代。
5. 未授权 Module/View/Action 始终 fail-closed；availability 只控制提示，不替代服务端权限。
6. custom `view_id` 未注册和加载失败时，正式 Module/Business 页面都能安全降级。
7. 如果目标坚持“自定义页面只新增一个文件”，必须引入构建时静态 manifest/codegen，并增加后端 view id 与前端 manifest 的交叉校验；在此之前验收口径保持“一个新文件 + 一个 registry 修改”。
8. 用 Git diff 证明约定场景没有前端业务改动，并由隔离 E2E 证明页面真实可用。

### 7.2 “生产就绪”的通过条件

1. access token 内存化或采用经过威胁建模的等价方案，并在 enforce CSP 下验证登录、刷新、上传和自定义组件。
2. Playwright 在 CI 中使用唯一端口或由测试框架分配端口，禁止误复用开发者已有服务。
3. 增加针对生产构建的浏览器 smoke：构建 `dist/spa`，以真实 history fallback 服务器启动，验证深链接、缓存头和安全头。
4. 正式 `/module` 与 `/business` 覆盖多身份、租户切换、权限变化、全 interaction、失败重试和会话过期。
5. 前端错误上报包含 request id，能够与后端 trace/log/metric 关联，并完成一次告警演练。
6. 增加关键页面 a11y、键盘导航和焦点恢复门禁。
7. 为大分页、relation options、树节点上限和批量 Action 建立数据规模与响应时间基线。
8. 推送前运行 `python scripts/run_ci.py full`、隔离 Playwright、必要的真实 MySQL/Redis integration；远程 CI 每个 job 都必须有终态成功证据。

## 八、优先改进路径

### P0：统一正式页面解释器

1. 让 `ModulePage` 复用 `TableView` 的统一 Action executor，而不是维护第二套简化 Action 流程。
2. 为 Module 增加多 View 选择或明确主 View 契约，禁止静默丢弃 `views[1..]`。
3. 把 `org.tenant`、字段 label、状态展示和租户入口语义移入 Catalog 或显式产品外壳接口。
4. 将 custom 安全降级接入 ModulePage。

### P0：把 E2E 变成可信门禁

1. CI 执行 Playwright；
2. 本地与 CI 都使用隔离端口；
3. 将生产构建 history fallback、安全头和深链接 smoke 纳入验证。

### P0：闭环浏览器安全与诊断

1. access token 存储与 CSP 联合设计；
2. 前端错误、request id、后端 trace 和告警形成完整诊断链。

### P1：证明通用渲染上限

新增真实业务 Addon，并以大数据量、关系、树和批量流程压测，而不是继续用纯 demo 扩大结论。

### P2：按产品需求补齐

i18n、主题、多地区格式等应由明确业务需求触发，不应仅因“生产级”标签机械引入。

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

- `playwright.config.ts` 在本地使用 `reuseExistingServer: true`；默认 5173/18080 端口已被旧服务占用时，测试可能连到错误实例。隔离端口结果才可作为本次证据。
- 本评估没有把未在本次运行的 `python scripts/run_ci.py full`、真实 MySQL/Redis integration 或远程 CI 状态写成当前通过。
- 文档优化不会自动改变代码能力；只有 §7 的验收条件真实通过后，才可以升级结论。

## 十、最终结论

yang-system 的显式契约路线正确，后端 Catalog、权限投影、通用 TableView、表单和会话基础设施也已经形成可信骨架；在“显式声明可用 View 与 presentation”的契约子集内，零前端修改交付已经可行。

当前仍不能宣称目标普遍达成：任意 Action 不会自动进入正式页面，ModulePage 不是完备解释器，自定义页面仍有两个手工触点，前端也仍保留业务知识。

技术选型合理，不建议换框架。当前阶段应定义为“准生产、等待关键门禁闭环”，而不是“已经完整生产就绪”。后续结论升级必须由正式路径、生产构建、隔离 CI、浏览器安全、部署与可观测性证据共同支持。
