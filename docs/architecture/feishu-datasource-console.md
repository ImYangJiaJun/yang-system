# 飞书数据源控制台 — 设计

**日期**：2026-09-21
**状态**：待评审
**范围**：`project/yang-system`（前端为主，后端小改）
**前置阅读**：根 `AGENTS.md`、`frontend/AGENTS.md`、`docs/guides/ADDON_ONBOARDING.md`、
`docs/superpowers/` 之外的飞书后端设计见 lib_yang 仓库的
`docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。

---

## 1. 目标

给已落地的飞书数据源后端一套**自定义前端界面**，替代当前的通用表格页：

1. **统一入口**：导航里只留一个入口，进去就是数据源卡片列表。
2. **卡片列表**：每张卡片展示一个数据源，右上角有「添加数据源」按钮，卡片上可直接
   重命名 / 停用 / 删除。
3. **详情页**：点卡片进入该数据源，查看其中保存的选项数据。
4. **辅助配置指引**：让第一次接触的人知道「先做什么、去哪里做、填什么」。

## 2. 非目标

- **不在控制台里增删改选项。** 选项的增改由飞书多维表格写入 API 承担；控制台保持只读。
  这是既有架构决策（`src/addon/feishu/option/mod.rs:54-57`：「两个并存的可写入口会让
  审计语义与数据来源分叉」），本设计不推翻它。
- 不改 `engine/`（通用解释引擎）。所有新增代码落在 `features/feishu/` 与必要的 `shell/`。
- 不做权限管理面。用户与权限仍由运维 SQL + `access` 端口承担。
- 不做「按审批人过滤选项」这类新业务能力。

## 3. 决策记录

以下四条已与维护者确认，是本文其余部分的前提：

| # | 决策 | 选择 |
|---|---|---|
| 1 | 导航入口 | **只保留一个入口，隐藏通用表格**（两个飞书 module 不再声明 presentation/view） |
| 2 | 详情页能力 | **只读**（不新增选项写 Action） |
| 3 | 指引形态 | **卡片页顶部常驻可折叠提示 + 空状态引导** |
| 4 | 表单中文字段名 | **后端补 schemars `title`**（不改前端 `SchemaField` 的 label 优先级） |

## 4. 关键事实（勘察结论，带锚点）

这一节记录设计所依赖的、已在代码中核实的事实。**实现时如与之冲突，以本节为准并回头修订。**

### 4.1 两条「自定义界面」路线，能力不对等

**路线 A：registry 型自定义视图**（`features/registry.ts` + 后端 `interaction = custom`）

- 注册表形状：`registry.ts:21` 是 `Object.freeze({ "<view_id>": lazy(() => import("...")) })`；
  键是后端下发的 `view_id`，值类型是 `LazyExoticComponent<ComponentType<CustomViewProps>>`
  （`registry.ts:15`）。全文仅 32 行，无动态解析。
- **props 契约只有三个字段**（`registry.ts:9-13`）：
  `{ presentation: ActionPresentationSchema; actions: ActionDemoSchema[]; onClose: () => void }`。
  **没有 router 上下文、没有 URL、拿不到 `catalog.modules` / `table_views`。**
- 数据仍必须经声明的 Action 拿（`features/demo/views/DemoItemInsight.tsx:29-53` 用
  `presentation.operation_id` 反查 action，再 `invokeAction`），不能自带 api 层。
- 触发链路的起点在 `engine/renderers/action/use-presented-actions.ts:113-121`：
  `interaction === "custom"` 时调 `options.onCustom(presentation, row)`。
- 渲染分发共 3 处，都在 `shell/`：`pages/ModulePage.tsx:80`、`pages/BusinessPage.tsx:49`、
  `pages/WorkbenchPage.tsx:44`（DEV 门控）。三处结构相同：命中则渲染自定义组件（外面套
  `CustomViewBoundary` + `Suspense`），否则渲染通用 `TableView`。
- **后端没有「模块默认就是自定义视图」的声明面**：`ModulePresentationSpec.views` 的注释
  明写只装 TableView id（lib_yang `crates/yang-base/src/definition/ui/module.rs:148-149`）。
  所以走这条路时，自定义视图只能由一个 **Action 的 Custom 交互**触发——入口会挂在通用
  表格的一个按钮上，用户先看到表格再点按钮才进卡片，与「进去就是卡片」不符。
- 后端声明方式：`ActionPresentationSpec::new(placement, ActionInteraction::Custom).view_id("…")`；
  `view_id` 在启动期强校验必须是稳定限定标识（`builder/compile.rs:496-506`），写错模块起不来。
  仓库内唯一先例是 `examples/frontend_demo/view.rs:47-51`（`demo.items.insight`）。

**路线 B：静态路由自建页**（照 `account` 路线）

- 前端全权自建页面与 URL。先例完整：`shell/routes.tsx:2` 静态 import `AccountSettingsPage`，
  挂在 `routes.tsx:72` 的 `{ path: "account", element: <AccountSettingsPage /> }`；
  该页面不读 catalog、不含 presentation，直接用 `features/account/api.ts` 打后端。
- 侧边栏需要手写一条 `NavLink`：硬编码先例是 `shell/AppLayout.tsx:246-262` 的
  「个人/账号设置」；业务模块条目则由 catalog 投影生成（`AppLayout.tsx:264-289`）。
- 代价：静态 import 会进首屏预算（`frontend/scripts/verify-bundle-budget.mjs`，
  目标 350 kB / 硬上限 450 kB gzip）。

**本设计选路线 B，并用路由级 `lazy` 消除它唯一的代价。**
`verify-bundle-budget` 只测首屏**静态闭包**、不含 dynamic import，所以
`lazy(() => import("..."))` 既有真实 URL，又不进预算。路线 A 省下的那点预算买不到
「点卡片进详情」所需的导航能力（前进/后退、可分享链接）。

> `registry.ts` 的静态字面量门禁（`scripts/check_architecture.py:812-824`：`import(` 之后
> 第一个非空白字符必须是引号）只作用于 registry 文件本身，不影响 `routes.tsx` 用 lazy。

### 4.2 后端数据面已经够用，不需要新 Action

飞书 addon 共 **8 个 Action**（不是 5 个）：

**`feishu.datasource` 模块**（4 个）

| Action | 方法 / 路径 | 权限 | 输入 | 输出 |
|---|---|---|---|---|
| `list_datasources` | POST `/api/v1/feishu/datasources/query` | `feishu.datasource.read` | `ListInput` | `{items:[{source_key,title,encrypt_enabled,default_locale,status}],page,page_size,total}` |
| `create_datasource` | POST `/api/v1/feishu/datasources` | `feishu.datasource.write` | `{source_key,title,token,encrypt_enabled?,default_locale?}` | `{source_key}` |
| `update_datasource` | PUT `/api/v1/feishu/datasources` | `feishu.datasource.write` | `UpdateDatasourceInput` | `{affected}` |
| `delete_datasource` | DELETE `/api/v1/feishu/datasources` | `feishu.datasource.write` | `{source_key}` | `{deleted,disabled_options}` |

**`feishu.option` 模块**（4 个）

| Action | 方法 / 路径 | 鉴权 |
|---|---|---|
| `list_options` | POST `/api/v1/feishu/options/query` | `feishu.option.read` |
| `approval_options` | POST `/api/v1/feishu/approval/options/{source_key}` | public + 按数据源 Token 自校验 |
| `upsert_options` | POST `/api/v1/feishu/inbound/options/upsert` | public + `ManagementTokenMiddleware` |
| `delete_options` | POST `/api/v1/feishu/inbound/options/delete` | public + `ManagementTokenMiddleware` |

**重命名与启停是同一个 `update_datasource`**：`UpdateDatasourceInput`
（`datasource/actions/update_datasource.rs:23-41`）含
`source_key`（必填）、`title`、`token`、`encrypt_enabled`、`default_locale`、`status`，
除 `source_key` 外全部可选且 `#[serde(default)]`。

语义要点（文件头注释 `:3-5` 与 validate `:51-85`）：

- **省略即保持原值，不是清空。** 若把省略当清空，一次只改标题的调用会抹掉 Token。
- `status` 只接受 `"active" | "disabled"`。
- `title` 必须 1..=100 字符。
- `token` 显式传空白会报错（「不轮换请省略该字段」）。
- 全部字段都省略时报 `ParamInvalid`「没有要更新的字段」。
- 数据源不存在时报 `RecordNotFound("数据源不存在")`。

**按数据源看选项**：`list_options` 把 `source_key` 作为**顶层字段**发在请求体里
（`domain/list_input.rs:52-53` 的扩展字段，生效处 `option/actions/list_options.rs:45-48`），
**不是塞进 `where`**。输出 `{items:[{option_id,source_key,label,i18n,sort_order,is_default,enabled}],…}`；
`i18n` 是 JSON 文本原样返回；未显式排序时回退 `sort_order ASC + option_id ASC`。

**`delete_datasource` 会连带停用其下全部选项**（`datasource/actions/delete_datasource.rs:81-95`），
返回 `disabled_options` 计数——卡片上的删除确认文案必须说清这一点。

### 4.3 表格形态已经具备，缺的只是卡片形态

`datasource/mod.rs` 现有的 `view()` 已把 5 列 + 工具栏「新建」(Form) + 行内「更新」
(Form, `record_parameter="source_key"`) + 行内「删除」(Invoke, 带后端声明的二次确认文案)
全部投影出来。**数据面完整**，本设计要做的是换一层前端形态，并把通用表格入口隐藏。

`record_parameter` 是构建期强校验的（lib_yang `builder/compile.rs:509-539`）：Row 展示必须
声明它，且该参数必须真实存在于 Action 的 params 或 `input_schema.properties`。

### 4.4 前端现状中的三个缺口

1. **没有任何 onboarding 设施**。全 `src/` 搜「帮助 / 指引 / 使用说明 / 了解更多 / 如何」
   零命中；`EmptyState` / `Tooltip` / `Toast` 组件不存在。指引形态要从零造。
2. **后端不下发 `columns[].display`**（lib_yang `definition/ui/table.rs:61-81` 只发
   `field/title/description/widget/required/searchable/filterable/sortable/relation`）。
   所以「启用 / 停用」徽标不能靠后端元数据，前端按 `status` 字符串自己映射。
3. **表单 label 会回落到英文字段名**（`engine/renderers/form/SchemaField.tsx:58`），
   因为 Action 的 `input_schema.properties.*` 没有 `title`。
   （表列的中文名已在后端修好并加测试守住，但那是**另一条路**。）

### 4.5 服务端永远回不出明文 Token

`feishu_datasource.token_hash` 只存 SHA-256 摘要（`datasource/table.rs:33-39`），
单向不可逆。**因此指引里不能展示 Token 本身**，只能告诉用户「去飞书审批后台复制你当初
填的那个值」。轮换只能走 `update_datasource` 重新设一个。

## 5. 设计

### 5.1 落位

新增业务域 `frontend/src/features/feishu/`，与 `features/account/` 同构：

```text
frontend/src/features/feishu/
├── api.ts                          # Action 调用封装 + TanStack Query key
├── views/
│   ├── DatasourceListPage.tsx      # 卡片列表页（含指引与空状态）
│   └── DatasourceDetailPage.tsx    # 某数据源下的选项（只读）
└── components/
    ├── DatasourceCard.tsx          # 单张卡片（含卡片内操作）
    ├── DatasourceFormDialog.tsx    # 新建 / 重命名共用的表单对话框
    └── GuideBanner.tsx             # 顶部可折叠指引 + 空状态引导
```

**依赖方向**（`frontend/AGENTS.md` 锁定）：`shared` ← `engine` ← `features` ← `shell`。
新代码只向下依赖，不 import 其它 feature 域。

### 5.2 路由与导航

`shell/routes.tsx` 在 `RequireAuth` 之下新增两条 **lazy** 路由：

| 路径 | 页面 |
|---|---|
| `/feishu/datasources` | `DatasourceListPage` |
| `/feishu/datasources/:sourceKey` | `DatasourceDetailPage` |

理由见 §4.1：lazy 让真实 URL 与首屏预算兼得。

`shell/AppLayout.tsx` 加一条手写 `NavLink`「飞书数据源」→ `/feishu/datasources`，
照「个人/账号设置」那条先例的位置与样式。

> **记录级详情路由在本仓库是先例为零的**（现有带参路由只有 `m/:moduleId` 与
> `m/:moduleId/v/:viewId`）。这条路由是新范式，评审时值得特别看一眼。

### 5.3 后端改动（三处小改）

1. **两个 module 去掉 `presentation()` 与 `view()`**，保留 Action 注册与认证中间件。
   效果：导航里不再出现自动生成的「飞书数据源」「飞书选项」表格入口，
   `catalog.actions` 仍包含全部 8 个 Action（卡片页照常能调）。
   - 需同步删除两个 module 里针对 View 的测试
     （如 `option/mod.rs` 断言 `spec.actions.len() == 1` 的那条）。
   - **需验证**：去掉 presentation/view 后 `AppBuilder::build` 的模块内容校验仍通过。
2. **给 `CreateDatasourceInput` / `UpdateDatasourceInput` 的字段补 schemars 中文 title**，
   表单 label 才不会回落英文。
   - **需验证**：`#[schemars(title = "…")]` 在前端 `SchemaField.tsx` 的 label 解析里
     是否真的优先于字段名。若否，改用「让 `params()` 返回带 `.title(…)` 的参数」这条
     DSL 路径（`examples/frontend_demo/actions/add.rs:9-17` 有先例）。
3. **`token` 字段标为密码型输入**（当前会渲染成明文单行框）。
   机制待定：schemars 层给 `format` 的写法需实测。

### 5.4 交互契约

| 动作 | Action | 输入 | 界面 |
|---|---|---|---|
| 添加数据源 | `create_datasource` | `source_key` / `title` / `token` / 可选加密与语言 | 右上角按钮 → 表单对话框 |
| 重命名 | `update_datasource` | `{source_key, title}` | 卡片操作 → 同一个表单对话框（预填） |
| 停用 / 启用 | `update_datasource` | `{source_key, status}` | 卡片操作 → 确认后提交 |
| 删除 | `delete_datasource` | `{source_key}` | 卡片操作 → 二次确认（文案须提到会连带停用其下选项） |
| 查看选项 | `list_options` | `{source_key, …标准分页六键}` | 详情页表格（只读） |

提交成功后**回读 `list_datasources` 刷新**——`update_datasource` 只返回 `{affected}`，
不含最新记录。

### 5.5 指引内容（③）

顶部可折叠提示，四步；空数据时自动展开，有数据时默认折叠：

1. **在飞书审批后台配置控件** —— 单选/多选控件选「使用外部选项」，自定义一个 Token
   并记住它（服务端只存摘要，之后无法回显）。
2. **在这里建一个数据源** —— 数据源标识会进接口 URL；粘贴上一步的 Token。
3. **把接口地址与 Token 填回审批后台** —— 点「校验数据」确认能拉到选项。
4. **在多维表格配自动化推送选项** —— HTTP 节点带 `Authorization: Bearer <管理 Token>`
   调写入接口，选项随表格变动自动更新。

指引只写**去哪做、填什么**，不放 Token 明文（§4.5）。

### 5.6 卡片上展示什么

`list_datasources` 返回 5 个字段，卡片用其中 4 个：

- 标题位：`title`，副标题位：`source_key`
- 徽标：`status`（前端映射「启用/停用」，后端不下发 `display`，见 §4.4-2）
- 次要信息：`default_locale`；`encrypt_enabled` 为真时加一个「已加密」标记

## 6. 测试与门禁

**前端**

- Vitest：`frontend/tests/features/feishu/`（镜像 `src/` 路径），覆盖
  `api.ts` 的调用封装与 query key、卡片的操作状态流转、指引折叠、空状态。
- `pnpm check` 全链：Prettier → ESLint `--max-warnings 0` → `tsc --noEmit` → Vitest →
  `verify:locale-contract` → build → `verify:production-build` → `verify:bundle-budget`
  → `verify:deployment-contract`。
- **bundle 增量需实测**：理论上 lazy 路由不进首屏静态闭包、增量≈0，但要 build 后
  跑 `verify:bundle-budget` 确认。若增量落在首屏，说明有依赖被 `shared/ui` 或 `engine`
  静态引用，需要拆。

**后端**

- 删 presentation/view 后跑 `check_architecture.py` 与 `cargo test --lib`。
- 补 schemars title 后重新生成 OpenAPI 快照与 TS 类型（`python scripts/dump_openapi.py` +
  `pnpm gen:contracts`）。

**端到端 — 已知覆盖缺口**

Playwright 用的是 `examples/frontend_demo/` 那个无数据库演示后端
（`frontend/playwright.config.ts:20-42`），**里面没有飞书模块**。因此新页面的 e2e
要么扩演示后端（工作量不小、且演示后端本就是无 DB 的），要么**明确接受 e2e 不覆盖**、
只靠 Vitest + 手工验证。**这一条需要评审时拍板。**

## 7. 风险

| 风险 | 处置 |
|---|---|
| `#[schemars(title)]` 在 `SchemaField` 的 label 解析里不生效 | 实现前先读 `SchemaField.tsx` 原码确认；不行则走 `params()` 带 title 的 DSL 路径 |
| 去掉 presentation 后模块内容校验不过 | 实现时先跑一次 build 验证；不过则保留 presentation 但设法隐藏导航 |
| 记录级详情路由是新范式，可能与现有导航/面包屑不一致 | 评审时确认；必要时退化为 `?sourceKey=` 查询参数（BusinessPage 有先例） |
| lazy 路由仍抬高首屏预算 | build 后实测 `verify:bundle-budget`；必要时把卡片组件再拆一层 lazy |
| 指引文案与后端实际契约漂移 | 指引里不写具体接口路径与字段名，只写「去哪里做什么」 |

## 8. 未决项

1. **e2e 是否扩演示后端**（§6）。
2. `token` 字段标为密码型的**具体机制**（schemars `format` 的写法需实测）。
3. 卡片上「停用」是否需要**乐观更新**（先改 UI 再等接口），还是必须等回读。
4. 是否为「已停用」的数据源在卡片上直接提供「启用」入口（当前设计里走同一个操作菜单）。

## 9. 与其他文档的关系

- 后端（表结构、8 个 Action、飞书契约、加密与来源校验）见 lib_yang 仓库的
  `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。
- 新增业务域的结构规则见 `frontend/AGENTS.md`。
- 首个授权只能由运维 SQL 完成（决策 D2），见 `docs/contracts/AUTHZ_GRANTS.md`。
