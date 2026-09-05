# 前端技术栈重构 ADR 组

状态：Accepted
日期：2026-08-27
适用范围：`frontend/` 控制台整体重构（破坏性，需求方明确授权 Big Bang，不做渐进绞杀与可逆切换）

本目录取代已删除的 `docs/architecture/frontend-stack-rebuild-adr.md`（单文件初版）。
初版经架构评审（`yang-system-前端技术栈重构-adr-架构评审.md`）后按评审意见拆分为
五个独立决策；评审中"渐进替换与可逆切换"相关建议经需求方裁定不予采纳，其余意见
全部吸收。

## 共同背景与系统本质

本前端不是普通 CRUD 管理台，而是**后端 Catalog/Registry 驱动的 UI 解释器**：

```text
后端 Catalog/Registry（唯一事实源）
  → UiCatalog 投影（纯 TS）
  → 通用解释器：TableView（表格）+ JsonSchemaForm（表单）+ Action 语义
  → 静态 custom registry 逃生舱
  → 手写页面：登录 / 注册 / 密码重置 / Step-up
```

## 不变量（所有 ADR 共同遵守）

1. 后端 Catalog 是唯一事实源；前端不引入第二套元数据格式（拒绝 amis 类低代码渲染器）。
2. 自定义视图只能通过构建期静态注册表解析，禁止根据后端字符串构造动态 import。
3. 保持 SPA + Nginx 静态部署形态；不引入 SSR/SSG（无 SEO 诉求，只增加复杂度）。
4. 现有质量门禁不降级：typecheck / lint / Vitest / locale 契约 / 构建与部署契约 /
   dev-server 与 production-build 双环境 Playwright。
5. 后端零改动：HTTP 语义、错误码体系、Refresh Cookie 会话、Step-up proof 契约不变。
6. 跨栈共享逻辑（投影、会话、校验、错误映射）沉淀为无框架 import 的纯 TS core。

## 决策索引

| ADR                                        | 决策                     | 结论                                                                                   |
| ------------------------------------------ | ------------------------ | -------------------------------------------------------------------------------------- |
| [ADR-1](adr-1-build-and-ui-primitives.md)  | 构建链与 UI 原语         | 解除 Quasar 耦合；原生 Vite + Tailwind 4 + shadcn/ui（源码自有）                       |
| [ADR-2](adr-2-table-engine.md)             | 表格引擎                 | TanStack Table v8 headless；虚拟滚动按需引入，非一期硬依赖                             |
| [ADR-3](adr-3-framework-and-state.md)      | 视图框架、路由与状态分层 | React 19 + react-router；Query / SessionController / 本地状态三层分离                  |
| [ADR-4](adr-4-contracts-and-validation.md) | 契约与运行时验证边界     | OpenAPI codegen（静态类型）+ zod（固定协议 envelope）+ Ajv（动态 JSON Schema）三轨分离 |
| [ADR-5](adr-5-acceptance-and-cutover.md)   | 验收指标与切换策略       | 量化视觉/性能/无障碍指标；能力清单驱动的工作量估算；一次性 cutover                     |

## 需求方显式确认项登记（二审新增）

以下为影响真实用户或产品节奏、必须由需求方显式确认的决策，不视为技术默认：

| 决策                    | 内容                                                                                                                 | 状态                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------- | ----------------------------- |
| **功能冻结窗口**        | 重构期（预计 6–8 周，并行期硬上限 10 周）内旧前端只接安全与关键修复（双写 v2），普通新功能等待切换后实现（ADR-5 §5） | ✅ 需求方已确认（2026-08-27） |
| **浏览器契约收紧**      | 放弃 Safari ≤ 14 等旧浏览器，影响不可升级的存量设备可达性（ADR-1 §5）                                                | ✅ 需求方已确认（2026-08-27） |
| **Big Bang 一次性切换** | 不建渐进绞杀/金丝雀/可逆切换机制，仅保留制品级回退底线 + 三件安全网（ADR-5 §6）                                      | ✅ 需求方已确认（2026-08-27） |

## 二审状态

经 `yang-system-前端技术栈重构-adr组-二审对照评审.md` 评审：ADR-1/2/3 批准，
ADR-4/5 有条件批准；全部条件（OpenAPI spike 降级、前后端校验交叉验证契约、
双层 bundle 预算、冻结窗口登记、三件切换安全网、CI self-test 同步、浏览器变更
理由说明）已落实到对应 ADR。状态保持 **Proposed**，进入 M0/M1 落地验证，M1 完成
后按实测重估 M2–M4 并做终审。

## 进度记录

### 2026-08-27 M0 + M1 完成

- **M1 检查点 0（OpenAPI spike）：通过**。`DefinitionCatalog::to_openapi` 投影覆盖
  全部 18 个已注册 Action（路径/方法/operationId 逐一命中，写操作携带与调度同源的
  输入 Schema）；已固化为 `src/app.rs` 契约测试
  `openapi_projection_covers_all_catalog_actions`（CI 回归锁定）。导出机制落地为
  `src/bin/openapi-dump.rs`（惰性资源、不依赖真实 MySQL/Redis），ADR-4 §2.1 的
  剩余缺口关闭。
- **M0 基座**：`frontend-v2/`（React 19 + Vite 8 + Tailwind 4 + shadcn 源码组件 +
  TanStack Query/Table + zod 4 + Ajv 2020-12 + react-hook-form），typecheck/lint/
  test/build/format 全绿。
- **M1 纯 TS core**：旧前端 contracts/module-pages/api/session 全部平移（框架零
  耦合）；SessionController 独立状态机 + React 薄壳；Ajv 关键词白名单（白名单外
  显式报错）；`pnpm gen:contracts` 契约生成链（openapi-typescript 仅类型）与 18 个
  operation 全覆盖的交叉验证契约测试。Vitest 80 用例绿。
- **M1 垂直切片**：Catalog 拉取 → 导航投影 → 通用模块页（TanStack Table，查询参数
  契约与旧实现逐字段一致）→ Action 弹窗（react-hook-form + Ajv resolver）提交成功
  端到端；对接 `examples/frontend_demo` 真实联调通过（catalog 与 list Action 经
  Vite 代理 curl 验证）。集成测试断言请求方法/路径/body 精确匹配。Vitest 累计
  20 文件 119 用例绿，门禁由父级复核通过。

**M2–M4 重估（按 M1 实测）**：M1 实际耗时远低于估算（人机协作口径约 1 个工作日
vs 估算 2–2.5 周），核心解释器的两个最大风险项（表格查询契约对齐、动态表单校验）
已在切片中验证。M2（能力清单 5–12 完整化）重估为 3–5 个工作日；M3（页面与外壳）
2–3 天；M4（门禁/部署契约/CI 切换/文档）2–3 天。已知留待 M2 的事项：bulk Action
链路的真实后端覆盖（演示后端未声明 bulk presentation）、download/preview/redirect
的集成测试、无视图模块的 primaryAction 回退、enum/relation 控件从原生 select
升级为 Radix Select（需补 jsdom pointer-capture mock）、bundle 分包优化
（当前产物 812 kB，目标值 350 kB，硬上限 450 kB 需 code-splitting 后达标）。

### 2026-08-27 M2 完成（能力清单 5–12）

bulk Action 真实链路（演示后端补 bulk-delete + Bulk placement）、
download/preview/redirect 集成测试、无视图模块 primaryAction 回退、
enum/relation 升级 Radix Select（jsdom pointer-capture mock）、树形表格安全降级
（resolveDisplayRows 纯函数）、StepUpDialog + Promise 化宿主（proof 单次消费、
取消不破坏会话）、失效传播（session-bridge + clearSession(reason) 修复导航竞态）、
multipart 上传（MIME 白名单后端强制）、列偏好持久化。前端 146 用例 / 后端 104 用例
全绿；演示后端 bulk/upload/download 真机 curl 验证通过。

**决策记录**：`openapi-dump` 从 `src/bin/` 迁至 `examples/`（`c90bd45`）——架构门禁
锁定 src/ 为纯组合根且明确拒绝 src/bin/，开发期契约工具按 frontend_demo 先例归位，
比给检查器开白名单更符合门禁原意。

### 2026-09-04 M3 完成（能力清单 13–17）

注册（邮箱验证码 + 冷却）/密码重置（query token 双通道）、身份切换（IdentityStore
纯 TS + SelectIdentityPage + AccountSwitcher）、Dashboard/Business/Workbench
（DEV 门控 + lazy，生产产物无 workbench chunk）、custom 静态注册表（加载失败回退
通用表格）、locale 契约门禁平移、密度三档（36/44/54px）。176 用例绿。

**决策记录**：身份/会话状态用外置 store 而非 useState——select+navigate 在事件
处理器内被 useSyncExternalStore 拆成不一致帧会触发路由守卫误跳；ModulePage 身份
守卫改为不一致帧渲染空 + 宏任务延迟跳转。

### 2026-09-04 M4 完成与一次性切换（ADR-5 §6 执行）

- **M4a 门禁平移**：Playwright 双环境（5310/18310、5311/18311）18+2 用例全绿；
  axe WCAG 2.2 AA 零 critical/serious；视觉回归基线 6 张入库（CI 跳过——Windows
  采集基线在 Linux 字体栅格化差异会误报，`a987211`）；部署契约逐字沿用
  （7 安全响应头、loopback-only、缓存策略）；bundle 双层预算落地，实测首屏 JS
  gzip **257.9 kB**（目标 350 / 硬上限 450）。
- **切换**（`60359ac`）：frontend-v2/ 更名为 frontend/，旧 Quasar 前端删除
  （git 历史保留）；check_architecture.py 前端规则改写为 React 结构语义
  （main.tsx 唯一 createRoot、workbench DEV 门控、TableView 行为边界、custom
  注册表静态字面量 import）并同步 self-test；`.gitattributes` 补
  `frontend/**/*.png binary`（防止基线截图被文本归一化损坏）；AGENTS.md /
  README.md 同步。`run_ci.py quick` 与 self-test 全绿。
- 需求方确认项登记表中的浏览器契约收紧已随切换生效。

ADR-1 至 ADR-5 状态翻转为 **Accepted**。

### 2026-09-05 单元测试与生产源码隔离（修订 ADR-5 §7「同目录测试」）

37 个 Vitest 文件从 `src/` 同目录迁移至 `frontend/tests/`（镜像 src 目录结构，
`git mv` 保留历史）；共享 helper/fixture 从 `src/test/` 迁为 `tests/helpers/` 与
`tests/fixtures/`，setup 迁为 `tests/setup.ts`。`src/` 自此为纯生产代码。

**决策记录**：

- 测试经 `@/` 别名引用被测源码，新增 `@test/` 别名指向 `tests/`（仅
  vitest.config + tsconfig 注册，构建不含 tests/）；被测单元 import 统一从
  `./xxx` 改写为 `@/<dir>/xxx`，消除相对路径对文件位置的耦合。
- 选「顶层 tests/ 镜像」而非「src/ 下 **tests** 子目录」：前者让 `src/` 目录树
  只表达生产结构，且 locale 契约等扫 `src/` 的门禁语义保持「生产代码」；
  locale 契约扫描范围显式扩到 `tests/`，保证门禁强度不下降。
- `tests/contracts/openapi-contract.test.ts` 的 `../../contracts/openapi.json`
  因镜像深度不变天然继续有效（指向 `frontend/contracts/` 生成物）。

### 2026-09-05 src/ 引擎/应用分离（方案六，两个提交：`16a8546` + 本条目所属提交）

`src/` 从按技术类型分层（api/app/catalog/components/...）重构为按架构角色分层，
与 react-admin / Strapi / Directus 等 Schema 驱动管理台的「引擎/应用分离」范式同构：

- `engine/`：与业务无关的通用解释引擎——renderers（table/form/action/module）、
  contracts（zod + Ajv + OpenAPI 类型）、catalog（导航投影与缓存）、http（Action
  调用协议）、session（浏览器会话协议全家桶）；`engine/index.ts` 是约定公共出口。
- `features/auth/`：唯一业务域——登录/注册/重置/身份选择页面、注册与密码重置
  流程请求（`api.ts`）、StepUpDialog、身份 store；`features/registry.ts` 是自定义
  视图静态注册表，自定义视图按域放 `features/<域>/views/`。
- `shell/`：应用外壳（routes/auth-gate/session-bridge/AppLayout/通用页面编排）。
- `shared/`：shadcn 源码组件与产品文案。
- 依赖方向约定：`shared` ← `engine` ← `features` ← `shell`。

**决策记录**：

- **会话协议归 engine 而非 features/auth**：renderers 有 6 处直接消费
  `useSessionCredentials`（表格查询、表单关系选项、Action 调用都需要凭据视图）；
  且后端 `yang_base::action::auth` 将浏览器会话 Cookie 作为框架能力提供，前端
  会话协议是其镜像，属平台能力。对照 react-admin 的 `useGetIdentity` 也在 ra-core。
- **auth.ts 沿职责拆分为二**：会话生命周期（login/refresh/logout/disable，被
  SessionController 直接依赖）留 `engine/session/lifecycle.ts`；注册与密码重置
  流程（账户域业务，无会话状态机依赖）迁至 `features/auth/api.ts`。拆分后
  engine 不反向依赖 features，分层成立。
- **createRoot 门禁升级**：从「扫描 layout/pages/components 三个目录」改为
  「扫描 src/ 全部 tsx、唯一豁免 main.tsx」，不再依赖目录名，对结构演进免疫。
- **自定义页面的承载方式**：registry 保持单一静态注册表（门禁不变），值指向
  `features/<域>/views/` 的懒加载组件；复杂业务页面的私有组件/hooks 收拢在
  本域目录，两个域共享的逻辑才下沉 shared/。
- **后续增强（未做，记录在案）**：engine 深度路径 import 的机器禁令
  （eslint-plugin-boundaries 或 check_architecture 扩展）、features 间禁止互相
  import 的机器强制。
- 门禁同步：check_architecture.py 三处路径（TableView/routes/registry）+
  self-test fixture；verify-locale-contract 两处路径；components.json shadcn
  别名；eslint react-refresh 豁免路径；dump_openapi.py 生成目标路径。
- 结构规则落成约束性文件 `frontend/AGENTS.md`（分层职责、依赖方向、新文件
  归位判断、门禁对照表），根 AGENTS.md 指向其为权威记录；后续结构演进须
  同提交同步该文件与门禁。
