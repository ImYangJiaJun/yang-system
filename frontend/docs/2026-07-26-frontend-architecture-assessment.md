# yang-system 前端架构与技术选型评估

> 评估对象：`D:\code\lib_yang\project\yang-system\frontend`
> 初评仓库快照：`450372f20ab54ceb9dec230b87b5ecf5780bd54b`
> 复评仓库快照：`11563b072fcbc6c7ac94b83f07e680a09471409c`
> 评估日期：2026-07-26
> 形态：Quasar CLI + Vue 3 SPA，消费 YANG UI Catalog
> 说明：版本与框架建议按 2026-07-26 官方资料校准；评分用于排序，不代表真实用户流量认证。

## 一、结论先行

技术选型总体合理，不建议换框架。

`Vue 3 + TypeScript + Quasar CLI/Vite + Pinia + Vue Router + Zod + Vitest + Playwright` 与当前“Catalog 驱动的内部管理系统/基础系统控制台”高度匹配。它不是对 SEO 内容站、离线优先应用或原生多端的普遍最优解：

- Vue 的 Composition API 适合沉淀动态表格、Schema 表单和会话编排逻辑；
- Quasar 提供后台系统需要的大量一致 UI 组件和布局能力；
- Pinia 足够管理客户端会话、身份和 Catalog；
- Zod 很适合把后端动态 Catalog 当作不可信输入做运行时校验；
- Playwright 适合验证多角色、登录刷新和真实浏览器交互。

综合复评为 **3.7/5（L3 后段）**。项目已具备完整前端工程骨架、严格类型检查、运行时契约校验、单元测试和 E2E，不是简单 demo。分数只因依赖供应链闭环小幅上调；主要架构瓶颈没有因为版本升级而消失：

1. 后端虽然输出 Catalog，前端仍通过 `operation_id` 后缀、字段名和硬编码模块表推断业务语义；
2. `TableView.vue` 和 Catalog store 承担过多职责；
3. 应用生命周期在多个页面/布局重复启动；
4. access/refresh token 存于 `sessionStorage`，仍可被同源 XSS 读取；
5. 组件级交互、无障碍和生产可观测性门禁不足。

下一阶段应把项目从“Catalog 辅助的前端”推进为“**显式 UI 契约驱动的前端**”，而不是引入新的大型状态/请求框架。初评时的 Quasar 发布阻断已经解除，当前最高收益点转为显式 UI 契约、唯一应用生命周期和按行为拆分 Table/store。

### 复评增量

| 初评问题             | 复评状态   | 已落地证据                                                                   | 剩余边界                                                  |
| -------------------- | ---------- | ---------------------------------------------------------------------------- | --------------------------------------------------------- |
| W-00 Quasar 安全公告 | **已完成** | `0b69ad7` 升级到 Quasar 2.22.0；`501435e` 将 production audit 纳入 full gate | 继续依赖锁定版本与审计门禁                                |
| W-01 UI 语义启发式   | **未开始** | `ModulePage` 仍按 `.list/.me/.select` 与输入字段 `id` 推断 placement         | 先扩展基础库 Catalog，再迁移前端，禁止双解释链            |
| W-02 TableView 过宽  | **未开始** | 当前仍为 1,094 行，混合查询、筛选、关系、选择、Action 和呈现                 | 先提取无 DOM composable，再拆呈现组件                     |
| W-03/W-04 状态与启动 | **未开始** | `catalog.ts` 仍为 296 行；`start()` 仍有 3 个调用者                          | 建立唯一 app boot，随后拆 session/identity/tenant/catalog |
| W-05/W-06 浏览器边界 | **未开始** | refresh token 仍在 `sessionStorage`；`/workbench` 仍无 build/permission gate | 需要前后端协议和部署决策                                  |

因此，技术选型依旧正确，当前问题是职责和契约没有收敛，而不是 Vue、Quasar 或 Pinia 不够强。

## 二、技术选型判断

| 选型                  | 结论               | 理由                                                    |
| --------------------- | ------------------ | ------------------------------------------------------- |
| Vue 3 Composition API | 保留               | 动态界面、强类型组合逻辑和渐进拆分适配度高              |
| TypeScript strict     | 保留并强化         | Catalog/表格是高动态边界，需要静态约束兜底              |
| Quasar CLI + Vite     | 保留               | 后台组件覆盖广，CLI 集成构建/路由/图标/样式最省总成本   |
| SPA                   | 当前合理           | 登录后的内部系统不依赖 SEO；需补服务器 history fallback |
| Pinia                 | 保留               | 当前规模足够，不需要换 Redux 类方案                     |
| Vue Router            | 保留               | 路由元数据和身份入口清楚                                |
| Zod 4                 | 保留               | 后端 Catalog 和 API 返回必须运行时验证                  |
| Vitest                | 保留               | 与 Vite/Vue 工具链一致                                  |
| Playwright            | 保留并扩大关键旅程 | 适合真实浏览器隔离、自动等待和多角色场景                |
| TanStack Query        | 暂不引入           | 先拆请求 composable；出现广泛缓存失效/去重需求后再评估  |
| SSR/Nuxt              | 不引入             | 当前没有 SEO、首屏公开内容或服务端渲染收益              |
| 微前端                | 不引入             | 当前模块规模和团队边界不足以抵消运行复杂度              |

### 官方能力校准

- Vue 官方提供一等 TypeScript 支持，同时明确 Vite 只转译、不做类型检查，因此项目保留 `vue-tsc` 门禁是正确的：[Vue TypeScript 指南](https://vuejs.org/guide/typescript/overview)。
- Vue 官方将逻辑复用和良好类型推导列为 Composition API 的核心优势，适合后续从大组件提取 composable：[Composition API FAQ](https://vuejs.org/guide/extras/composition-api-faq.html)。
- Quasar 官方把 CLI with Vite 作为希望获得完整 Quasar 体验时的推荐入口；SPA history 模式部署必须由 Web Server 回退到 `index.html`：[Quasar CLI with Vite](https://quasar.dev/start/vite-plugin/)、[SPA 部署](https://quasar.dev/quasar-cli-vite/developing-spa/deploying/)。
- Pinia 是 Vue 当前默认状态管理方案，并强调 store 可模块化设计：[Pinia Introduction](https://pinia.vuejs.org/introduction.html)。
- Zod 4 是稳定版本，适合把 TypeScript 类型与运行时输入验证对齐：[Zod 4](https://zod.dev/packages/zod)。
- Playwright 的自动等待、隔离 browser context 和面向用户行为的断言适合关键 E2E：[Auto-waiting](https://playwright.dev/docs/actionability)、[Writing tests](https://playwright.dev/docs/writing-tests)、[Best practices](https://playwright.dev/docs/best-practices)。

这些官方资料只能证明技术路线仍受支持，不能证明本项目的精确版本组合天然兼容；兼容性结论仍必须来自锁定 Node/pnpm 后的 frozen install、类型检查、测试和生产构建。

## 三、第一性原理：前端架构要优化什么

1. **后端事实不能靠字符串猜。** 若 placement、identity、navigation、危险级别会影响行为，它们必须是显式契约。
2. **不可信数据在边界校验一次。** Catalog/API 响应进入应用前由 Zod 校验，内部消费已验证类型。
3. **视图只负责组合和呈现。** 请求取消、缓存、关系加载、选择状态和动作编排不应全部塞进单个 `.vue` 文件。
4. **会话事实只有一个 owner。** token 刷新、启动、过期清理和路由跳转不能由多个布局各自触发。
5. **客户端授权只改善体验。** 真正权限始终由后端决定；前端不能把隐藏按钮等同于安全控制。
6. **浏览器中的持久凭证默认不安全。** 任何可被 JavaScript 读到的 token 都暴露给同源 XSS。
7. **抽象要由重复压力驱动。** 不因为库流行就增加 server-state、微前端、SSR 或通用表单引擎。
8. **生产完整性包含部署、无障碍和诊断。** build 成功不等于用户可用。

## 四、成熟度评分

| 维度             |    评分 | 判断                                                     |
| ---------------- | ------: | -------------------------------------------------------- |
| 技术栈适配度     |     4.5 | 与内部管理 SPA、动态 Catalog 高度匹配                    |
| API/契约边界     |     4.1 | Zod、集中 client、请求 id、单飞 refresh 较完整           |
| Catalog 驱动程度 |     3.2 | 基础设施已成形，仍有较多 operation/字段名启发式          |
| 状态与生命周期   |     3.2 | Pinia 可用，但 store 过宽且多入口 start                  |
| 组件可维护性     |     3.1 | 通用能力强，TableView/ModulePage 体积和职责过大          |
| 浏览器安全       |     3.2 | 依赖公告已关闭并进入门禁，但 token 仍可被 JS 读取        |
| 测试             |     3.9 | 单元 + Playwright + 生产依赖审计，组件交互和安全负例不足 |
| 无障碍与可观测性 |     2.8 | 尚未形成自动门禁和端到端错误关联                         |
| **整体**         | **3.7** | **供应链阻断已关闭，进入契约与可维护性收敛阶段**         |

## 五、当前架构的成熟点

### 5.1 动态契约没有被盲目信任

`src/contracts/` 使用 Zod 校验 UI Catalog、表格 View、字段、筛选、排序、Action 和账号接口。它还校验支持的 schema version、默认 operator 是否位于允许集合等跨字段约束。

这很关键：TypeScript 只能约束编译期，不能保证后端响应。当前做法把动态能力控制在入口，而不是让 `unknown` 或随意类型断言扩散到组件。

### 5.2 API 会话链具备工程完整性

集中 API client 已具备：

- request id；
- 统一错误 envelope；
- `AbortController`；
- access token 注入；
- refresh single-flight，避免并发 401 发起多个刷新；
- 刷新成功后只重试一次；
- 刷新失败清理会话并发出 session-expired 事件。

这条链清楚、可测，也避免页面自行实现认证重试。

### 5.3 Catalog 加载正确处理了竞态

Catalog store 会取消旧请求，并用 request id 防止慢响应覆盖新身份/新租户结果。`CatalogCache` 不以“已有数据”作为跨身份快捷路径，只复用完全相同输入的缓存。

对于角色和租户可切换的后台，这比普通“页面 mount 就 fetch”更可靠。

### 5.4 通用表格已经覆盖复杂后台需求

`src/components/table/TableView.vue` 与其 model 已支持：

- 服务端分页、筛选、排序；
- 可见列；
- 选择与批量/行级 Action；
- relation option 批量加载；
- Tree 数据循环和最大节点保护；
- Action dialog 与动态表单；
- 请求取消和响应校验。

能力本身完整，当前问题是职责集中，而不是功能缺失。

### 5.5 动态 UI 仍使用静态组件白名单

自定义 widget/组件通过前端静态 registry 解析，没有根据后端字符串拼接 dynamic import。这样后端 Catalog 只能选择已审核组件，不能把任意模块路径变成执行入口。

这是正确的安全和构建边界，应继续保留。

### 5.6 工程门禁较完整

项目启用严格 TypeScript、`noUncheckedIndexedAccess`、unused 检查、ESLint、Prettier、Vitest、`vue-tsc` 和 production build。测试文件覆盖 API client、auth session、Catalog、请求组装、表格/树模型、路由策略，并有 Playwright 登录/管理/组织等旅程。

## 六、关键问题与改进建议

### W-00：Quasar 中危安全公告（已完成）

**原优先级：P1（发布阻断）；复评状态：升级与持续门禁闭环。**

初评时 `pnpm --dir frontend audit --prod` 返回 1 项中危：

- 当前解析版本：Quasar 2.21.4；
- 受影响范围：`<= 2.21.4`；
- 修复版本：`>= 2.22.0`；
- 风险：公开 `extend(true, ...)` 深合并在处理攻击者可控键时可能造成 prototype pollution。

公告与修复范围见 [GitHub Reviewed Advisory GHSA-3r53-75j5-3g7j](https://github.com/advisories/GHSA-3r53-75j5-3g7j)。

复评时已完成：

- `0b69ad7` 将 Quasar 受控升级到 2.22.0 并更新 lockfile；
- frozen install、类型检查、Vitest、production build 与 Playwright 关键旅程保持绿色；
- `pnpm audit --prod --audit-level moderate` 返回无已知漏洞；
- `501435e` 把 production dependency audit 加入 `python scripts/run_ci.py full`，以后同级公告会阻断完整门禁。

**持续验收条件：**

- 不降低 audit level，不把扫描失败静默转为 warning；
- 若未来必须接受 advisory 例外，必须记录 owner、到期日、影响路径和退出条件；
- 依赖升级继续通过 frozen install、full 与浏览器旅程，不单独以 audit 绿色替代行为验证。

### W-01：前端仍在推断后端没有表达的 UI 语义

**优先级：P1（平台扩展性主线）；影响：新增模块成本、错误页面行为。**

`src/module-pages.ts` 硬编码账号、管理员、组织等已知模块及其标题/图标/身份。`src/pages/ModulePage.vue` 还通过以下规则推断行为：

- operation id 以 `.list` 或 `.me` 结尾就是主 Action；
- `.select / .login / .refresh` 不作为普通次级 Action；
- 输入含 `id` 就被归为行级 Action，否则归为工具栏 Action；
- 模块归属和身份部分依赖 operation id 文本。

这些规则短期有效，但本质上把后端领域约定复制进了前端。一个命名完全合法的新 Action 可能被放错位置，新增模块也不能只靠 Catalog 出现。

**建议：扩展显式 UI Catalog，而不是增加更多前端 if。**

建议新增或完善：

```text
module:
  id, title, description, icon
  navigation_group, navigation_order
  required_identity / audience

action presentation:
  semantics: query | command | row | bulk
  placement: primary | toolbar | row | overflow | hidden
  interaction: page | dialog | confirm | download | background
  view_id
  selection: none | single | multiple
  danger_level
  refresh_policy
```

基础库当前已经有部分 `action_presentations`，应继续把前端真正使用的稳定业务语义补全，而不是再建一套前端私有 metadata。Catalog 不应输出 Quasar 组件名、任意模块路径等实现细节；图标也宜使用受控语义 token，由前端静态白名单映射。

**验收条件：**

- 新增一个模块和标准 Action 时，前端无需修改 `module-pages.ts` 或后缀判断；
- operation id 改名不改变 UI placement；
- Catalog schema version 对新增字段有兼容策略；
- 后端构建期校验 view/action 引用和互斥配置；
- 前端只保留静态组件 registry，不允许 Catalog 指定任意 import。

### W-02：`TableView.vue` 是当前最大的维护瓶颈

**优先级：P1；影响：可读性、测试、并行开发。**

`TableView.vue` 约 1,094 行，同时负责查询状态、列、筛选、排序、分页、relation、选择、动作弹窗、API 调用、渲染和大量样式。行数本身不是缺陷；真正风险是异步状态难以独立验证、修改影响面过大和多个职责一起演进。它不是因为 Vue 不合适而变大，而是业务能力已经超过单组件合理职责。

**建议按行为边界渐进拆分：**

- `useTableQuery`：分页、筛选、排序、取消、响应校验；
- `useRelationOptions`：relation 批量加载和缓存；
- `useTableSelection`：选择、跨页清理、Action 可用性；
- `useTableActions`：请求组装、确认、弹窗、刷新策略；
- `useColumnPreferences`：可见列和未来持久化；
- `TableToolbar`、`TableFilters`、`TableActionDialog`、`TableBody` 等纯呈现组件。

先移动现有逻辑并保持行为不变，不要同时重写 UI 或引入新状态库。

**验收条件：**

- 顶层 `TableView` 只编排 composable 和子组件；
- 每个 composable 有无 DOM 的快速单元测试；
- 查询取消、relation 竞态、选择清理、Action 刷新都有回归测试；
- bundle 和交互性能不退化。

### W-03：Catalog store 混合了四种不同状态

**优先级：P1；影响：生命周期、理解成本。**

`src/stores/catalog.ts` 同时拥有：

- access/refresh token 与 session；
- account identity；
- tenant 选择与组织列表；
- Catalog 加载、缓存、监听和导航相关状态。

这使任何身份变化都容易触发大范围副作用，也让 store 难以单独测试和销毁。

**建议拆为：**

- `useSessionStore`：会话、refresh、过期；
- `useIdentityStore`：当前产品身份；
- `useTenantStore`：组织列表和 tenant 选择；
- `useCatalogStore`：只负责给定 session/identity/tenant 的 Catalog；
- 路由导航状态留在 router 或独立轻量 store。

拆分时用明确 action 协调，不通过多个隐式 `watch` 相互触发。

**验收条件：**

- 每个 store 有单一状态所有权；
- 清空 session 可以确定性级联清空 tenant/catalog；
- identity/tenant 切换不会出现旧请求覆盖；
- store 可在测试中独立创建和 dispose。

### W-04：应用启动在三个页面/布局重复发生

**优先级：P1；影响：请求竞态和隐式副作用。**

当前 `store.start()` 分别由：

- `src/layouts/MainLayout.vue`；
- `src/layouts/WorkbenchLayout.vue`；
- `src/pages/RoleSelectionPage.vue`

调用。`start()` 会取消旧 watcher 并重新加载，路由切换可能因此把“应用生命周期”变成“哪个页面最后 mount”的隐式行为。

**建议路径：**

- 在 Quasar boot file 或 app root 只启动一次；
- `start()` 保证幂等并返回显式 disposer；
- HMR、登出和测试 teardown 明确调用 dispose；
- 页面只表达所需数据，不负责启动全局会话系统。

**验收条件：**

- 一次应用实例只存在一组全局 watcher/listener；
- 布局切换不重复加载相同 Catalog；
- HMR 和测试重复 mount 不累积 listener。

### W-05：`sessionStorage` 不能保护 refresh token 免受 XSS

**优先级：P1；影响：账号接管。**

当前 access token 和 refresh token 都写入 `sessionStorage`。它比 `localStorage` 的持久时间更短、标签页隔离更好，但仍能被该 origin 中任意 JavaScript 读取。OWASP 明确不建议把认证 token、JWT 或 refresh token 存入 `localStorage` 或 `sessionStorage`，优先使用 `HttpOnly; Secure; SameSite` cookie 或 BFF：[OWASP Session Management](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html)。

**推荐目标：先做简短浏览器威胁模型，再联合改造前后端。**

- refresh token 放入 `HttpOnly + Secure + SameSite` cookie；
- access token 仅保存在内存，短 TTL；
- refresh endpoint 配置 CSRF 防护、Origin/Referer 校验和 refresh rotation/reuse detection；
- logout 服务端撤销 refresh family；
- 明确多标签页下 rotation、登出和身份切换语义，必要时用 `BroadcastChannel` 同步；
- 部署严格 CSP，先 report-only 再 enforce；CSP 是纵深防御，不替代输出编码和输入清洗：[MDN CSP 指南](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/CSP)。

HttpOnly cookie 会引入 CSRF、SameSite、CORS、rotation 和多标签页一致性问题，不能只改前端存储位置；这些必须作为同一个认证协议验收。

若短期必须保留纯 Bearer SPA：

- 至少把 refresh token 迁出 Web Storage；
- 禁止 `v-html`/动态脚本/eval；
- 收紧第三方脚本和依赖；
- 采用 CSP nonce/hash、短 TTL、rotation 和复用检测；
- 明确该方案仍不能抵御成功执行的同源 XSS。

**验收条件：**

- JavaScript 无法读取 refresh token；
- token rotation 和重放有后端集成测试；
- CSP 在生产 enforce，违规有报告入口；
- 安全测试覆盖 XSS 后 token 外泄边界、CSRF 和登出撤销。

### W-06：Workbench 不应无条件进入生产包

**优先级：P1；影响：攻击面和产品可读性。**

`/workbench` 是有价值的开发/诊断工具，但当前路由没有明确身份/权限门槛，若随正式包对所有登录用户开放，会泄露 Catalog、Action 结构或试验入口。

**建议路径：**

- 开发环境由 build flag 启用；
- 生产若确有运维价值，使用显式后端 permission 控制；
- 禁止仅靠前端隐藏；
- 对试执行 Action 使用正常后端授权、审计和危险确认。

**验收条件：**

- 普通生产用户无法访问 Workbench 路由和相关 chunk；
- 生产启用时必须由后端权限决定；
- Workbench 的所有请求仍经过正式 API client 和审计。

### W-07：Catalog 缓存应升级为协议，而不是反复序列化比较

**优先级：P2；影响：大 Catalog 性能和一致性。**

当前缓存以完整输入/内容相等为安全前提，正确性优先是合理的；每次对大对象 `JSON.stringify` 理论上会随 Catalog 增长产生额外 CPU 和内存，但当前没有大 Catalog 基准，因此这里只是可演进点，不是已证实的性能缺陷。

**建议路径：**

- 后端返回 catalog revision/ETag；
- revision 至少绑定 schema version、应用定义版本和授权投影版本；
- 前端使用 `If-None-Match`；
- 身份/租户改变时 key 必须改变；
- 保留请求取消和“旧响应不得覆盖新上下文”的 guard。

**验收条件：**

- 未变化 Catalog 返回 304 或轻量 revision；
- 不再为缓存命中序列化完整 Catalog；
- 权限变化不会复用旧投影。

### W-08：测试重心应从纯函数扩展到关键组件交互

**优先级：P1；影响：重构安全。**

当前约 20 个测试文件，纯 model、contract、API client 覆盖不错，另有 5 个 Playwright spec；静态结构分析未发现 `TableView`、动态 Action 表单、Catalog 启动/销毁的充分直接组件覆盖证据。间接挂载或 E2E 可能触达部分路径，因此不能等同于“完全未测试”。

**建议路径：**

- 使用 Vue Test Utils 增加组件测试：
  - TableView 筛选/排序/取消；
  - SchemaField 不同 widget、错误和 secret；
  - Action dialog 的 placement/selection/confirm；
  - session expired 与 store dispose；
- Playwright 增加：
  - 多角色切换；
  - 权限撤销后的旧页面；
  - 跨租户负例；
  - refresh 竞争；
  - keyboard-only 和 axe 扫描。

不设置武断覆盖率数字；按高风险交互矩阵确定完成度。

**验收条件：**

- 关键组件的 loading、empty、error、取消和迟到响应均有直接覆盖证据；
- E2E 覆盖角色、会话、租户和 Workbench 权限矩阵；
- 约定浏览器矩阵中的失败会阻断发布，而不是只在本地记录。

### W-09：无障碍和错误可观测性尚未进入门禁

**优先级：P1；影响：真实可用性和生产诊断。**

ESLint 以基础 recommended 规则为主，尚未形成 Vue accessibility 规则、axe E2E、全局错误捕获和 release/request id 关联。

**建议路径：**

- 增加 `eslint-plugin-vuejs-accessibility` 或等价规则；
- Playwright 接入 axe，优先守住登录、导航、表格、Dialog；
- 设置 `app.config.errorHandler` 和 `unhandledrejection`；
- 前端错误上报包含 release、route、request id、Catalog revision，不上报 token/表单敏感值；
- 给 loading、empty、error、offline、permission-denied 建立一致状态组件。

**验收条件：**

- 关键旅程无严重 axe 违规；
- 键盘可完成登录、导航、筛选和 Action 确认；
- 后端 5xx 可由前端事件的 request id 追到服务端；
- source map 仅上传到受控错误平台，不公开部署。

### W-10：运行时与浏览器版本策略需要显式化

**优先级：P2；影响：可复现构建和兼容性。**

当前本机为 Node `v24.13.0`、pnpm `10.33.1`；`scripts/setup_local.ps1` 明确要求 Node 24+ 并安装 pnpm 10.33.1，项目 `packageManager` 也固定 pnpm 10，因此当前本地约定内部一致。另一方面，`quasar.config.ts` 的构建目标仍包含 Node 20 和 Safari 14；Vite 8 官方最低 Node 为 20.19+ 或 22.12+，当前 Quasar 新项目文档建议 Node 22+、pnpm 11+：[Vite 8 发布说明](https://vite.dev/blog/announcing-vite8)、[Quasar 环境要求](https://quasar.dev/start)。

这不表示现有构建一定不兼容，但说明“开发机、CI、声明的 build target、官方当前推荐”尚未由一个版本策略统一。

**建议路径：**

- 继续使用当前已约定的 Node 24，并用 Volta、`.node-version` 或 CI image 固定到可复现的 24.x；不要仅把门槛降为 22；
- pnpm 10 与当前 lockfile 一起保留；升级到 pnpm 11 时受控更新 lockfile、CI 和 `packageManager`，不要只改声明；
- frozen install 当前会提示 `@parcel/watcher`、`esbuild` 的 build scripts 被忽略；应审计后显式记录允许/忽略策略，不要为了消除警告而批量批准依赖脚本；
- 明确浏览器支持矩阵；若承诺 Safari 14，增加 Playwright WebKit 和缺失 API/polyfill 审计；
- 若没有 Safari 14 真实用户，删除过旧目标，减少转译和兼容负担。

**验收条件：**

- 开发机、CI 和部署镜像使用同一 Node/pnpm 策略；
- frozen install、类型检查、测试和 build 在干净环境可重复；
- 依赖 build script 的允许/忽略清单经过审计；
- 浏览器承诺与 Playwright project 矩阵一致。

### W-11：SPA 部署契约尚未进入自动化门禁

**优先级：P1；影响：深链接、缓存、安全头和生产诊断。**

production build 通过只能证明静态产物生成成功，不能证明 Web Server 会正确处理 history 路由、缓存和安全头。

**建议路径与验收条件：**

- 任意合法深链接刷新都回退到应用 `index.html`，不存在的静态资源仍返回 404；
- HTML 不长缓存，带内容 hash 的静态资源使用 `immutable` 长缓存；
- HTTPS、CSP、frame、MIME sniffing、referrer 等安全头有部署测试；
- 私有 source map 只上传到受控错误平台，公网不可直接下载；
- 部署 smoke test 同时验证版本号、错误脱敏和 request id 关联。

## 七、建议的目标分层

```text
App boot
  ├─ Session store
  ├─ Identity store
  ├─ Tenant store
  └─ Catalog store
       └─ validated UI projection
            ├─ Router/navigation
            ├─ Module page
            ├─ Table feature
            │    ├─ query composable
            │    ├─ relation composable
            │    ├─ selection composable
            │    └─ action composable
            └─ Schema form feature

API boundary
  ├─ client + request id
  ├─ refresh coordinator
  ├─ Zod response validation
  └─ error/telemetry projection
```

关键约束：

- store 不直接渲染 UI；
- view 不拥有全局 session 生命周期；
- Catalog 不指定任意可执行模块；
- router guard 只改善导航体验，后端始终执行真实授权；
- Action placement 不从字符串或字段名推断。

## 八、分阶段改进路径

### 阶段 0：契约封口与安全决策（1—2 周）

1. ✅ 已升级 Quasar 到修复版本，并将 dependency audit 纳入 full gate。
2. 定义 module/navigation/action presentation Catalog vNext。
3. 固定 Node/pnpm/浏览器支持矩阵并记录 SPA/Quasar ADR 与非目标。
4. 基于公网/内网和权限价值完成浏览器威胁模型，明确 refresh token cookie/BFF 或 Bearer 残余风险方案。
5. 给 Workbench 增加 build/permission gate。

**退出条件：** Catalog vNext、兼容策略、认证存储和 Workbench 暴露 ADR 获批；过渡期间禁止新增 operation/字段名启发式。

### 阶段 1：先拆行为，再拆页面（2—4 周）

1. 将 `store.start()` 移到唯一 app boot。
2. 拆 session/identity/tenant/catalog store。
3. 从 TableView 提取五个核心 composable。
4. 由前后端并行实现选定的 refresh token/Cookie/BFF 或强化 Bearer 协议，并完成 CSRF/rotation/多标签页测试。
5. 保持 UI 和 API 行为不变，增加组件回归测试。

**退出条件：** 顶层组件只编排；每个异步行为可独立测试和 dispose；生产认证协议达到 W-05 验收条件。

### 阶段 2：Catalog 真正驱动 UI（4—8 周）

1. 后端输出显式 module/navigation/placement/interaction；
2. 删除 `.list/.me/.select` 和 `id` 字段启发式；
3. 引入 ETag/revision 协议；
4. 增加 schema compatibility fixture 和未知语义安全降级；
5. 滚动发布期只支持明确的相邻版本窗口，不长期保留双 Catalog/双解释链。

**退出条件：** 用一个 fixture 新增模块，前端零业务代码改动即可正确导航和呈现。

### 阶段 3：生产完整性（6—10 周）

1. CSP 从 report-only 推进到 enforce，并完善违规报告与 XSS 增强测试；
2. 全局错误上报和 request id 关联；
3. accessibility lint + axe + keyboard E2E；
4. history fallback、缓存头、安全头、source map 和部署 runbook。

**退出条件：** 认证、无障碍、部署和诊断均有自动化门禁及运行手册。

### 阶段 4：由数据决定是否增加框架

只有出现以下信号时再评估 TanStack Query：

- 多页面共享相同 server state；
- 大量重复的 stale/retry/dedupe/invalidation；
- 手写缓存状态机已经超过抽象成本。

只有出现公开 SEO/首屏需求时再评估 SSR/Nuxt；只有存在独立团队、独立发布节奏和强隔离需求时再评估微前端。

## 九、建议的验收矩阵

| 目标                         | 自动化证据                                                                    |
| ---------------------------- | ----------------------------------------------------------------------------- |
| 新模块零前端业务改动         | Catalog fixture contract test + Playwright，未知语义安全降级                  |
| 身份/租户切换无旧数据覆盖    | store/composable race test                                                    |
| refresh 并发唯一             | API client unit test + browser E2E                                            |
| refresh token 不可由 JS 读取 | cookie 属性集成测试 + 浏览器断言                                              |
| 多标签页会话一致             | rotation、登出和身份切换的多页 E2E                                            |
| TableView 可安全拆分         | component tests + visual/E2E 关键旅程                                         |
| Workbench 不泄露             | production build chunk/route test + permission E2E                            |
| 无障碍                       | eslint accessibility + axe + keyboard E2E                                     |
| 生产可诊断                   | error event 含 release/route/request id 的集成测试                            |
| 部署可用                     | production build + 深链接 history fallback + HTML/静态资源缓存策略 smoke test |

## 十、最终判断

前端框架和技术栈选择是合理的，切换 React、Nuxt、微前端或另一套 UI 库都会增加迁移成本，却不能解决当前真正问题。

最优路径是：

1. 让后端 Catalog 显式表达 UI 语义；
2. 让前端按行为拆分 Table 和状态；
3. 让应用生命周期只有一个 owner；
4. 把 refresh token 移出 JavaScript 可读存储；
5. 用组件测试、E2E、无障碍和可观测性完成生产闭环。

完成这些后，前端可以从“完整的基础系统 SPA”升级为“可扩展的 YANG 契约驱动管理平台”，且不需要更换现有框架。

## 十一、验证记录与结论边界

### 本机新鲜验证

| 命令                                                              | 结果 | 可见证据                                                                                                                  |
| ----------------------------------------------------------------- | ---- | ------------------------------------------------------------------------------------------------------------------------- |
| `pnpm --dir frontend install --frozen-lockfile`（初评）           | 通过 | 依赖按 lockfile 安装，Quasar prepare 成功                                                                                 |
| `python scripts/run_ci.py full`（复评）                           | 通过 | production audit 无已知漏洞；ESLint、Prettier、`vue-tsc`、15 个 Vitest 文件/71 个测试、Quasar 2.22.0 SPA production build |
| `pnpm --dir frontend e2e`（复评）                                 | 通过 | Chromium 18/18，33.0 秒；登录/refresh/失效、角色、Catalog、Action、TableView 与自定义视图旅程                             |
| `pnpm --dir frontend audit --prod --audit-level moderate`（复评） | 通过 | Quasar 原公告不再出现，当前生产依赖无已知漏洞                                                                             |

E2E 实际覆盖登录成功/失败、refresh、会话失效、不同账号空间、未授权 Module fail-closed、Catalog 切换、动态 Action、上传/下载/预览/重定向，以及 TableView 的树、筛选、排序、关系表单和行操作。

这证明当前锁定版本组合在本机 Node `v24.13.0`、pnpm `10.33.1` 下可安装、类型检查、测试、审计和构建；W-00 不再是发布阻断。它仍不替代 Firefox/WebKit、低端设备或生产 Web Server 验证，也不证明 W-01—W-11 的架构与运行风险已经解决。

以下结论仍需真实环境数据支持，本文不作通过声明：

- 大 Catalog 下的解析、渲染和内存上限；
- 低端设备和 Safari 14 的实际兼容性；
- WCAG 全量人工审查；
- CSP/HttpOnly cookie 方案（当前尚未实现）；
- 生产 CDN/Web Server history fallback。
