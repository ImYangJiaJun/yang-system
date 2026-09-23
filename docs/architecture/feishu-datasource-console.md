# 飞书数据源控制台 — 设计

**日期**：2026-09-21
**状态**：已评审定稿（同日复核）
**范围**：`project/yang-system`（前端为主，后端小改）
**前置阅读**：根 `AGENTS.md`、`frontend/AGENTS.md`、`docs/guides/ADDON_ONBOARDING.md`、
`docs/superpowers/` 之外的飞书后端设计见 lib_yang 仓库的
`docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。

> **复核说明（2026-09-21）**：本版对后端源码逐条核实过一轮，修正了若干事实并据此改了设计：
> 列表定为**卡片 + 台账双视图**、指引改为情境化（不做常驻横幅）、表单改为手写
> （原 §5.3 的 schemars title / token format 两处后端改动随之取消）、
> 详情页增加「最近推送」列、创建后自动做一次 Token 连通性预检，
> 以及四处字段级错误（见 §4.6–§4.8）。四个可交互原型与逐条复核见
> `project/yang-system/.feishu-console-mockups/`（`REVIEW.md` 是复核全文）。
>
> **数据源数量的预期量级是「可能上百」**——这一条决定了双视图、搜索、分页与
> 三处查询字段声明的取舍（§4.10、§5.3）。

---

## 1. 目标

给已落地的飞书数据源后端一套**自定义前端界面**，替代当前的通用表格页：

1. **统一入口**：导航里只留一个入口，进去就是数据源列表。
2. **列表（两种视图，可切换）**：数据源列表支持**卡片**与**台账**两种视图，在页内一键切换。
   卡片给「一眼看到有哪些、哪些停了」的概貌；台账给上百条量级下的查找、排序与横向对比。
   右上角有「添加数据源」按钮，列表项上可直接重命名 / 停用 / 删除。
   两种视图**共享同一份搜索词、排序与分页状态**（切视图不丢上下文）。
3. **详情页**：点卡片进入该数据源，查看其中保存的选项数据。
4. **情境化指引**：让第一次接触的人知道「先做什么、去哪里做、填什么」——
   **指引只出现在他卡住的那一刻，并携带那一刻的数据**（§5.5），不设常驻横幅。

## 2. 非目标

- **不在控制台里增删改选项。** 选项的增改由飞书多维表格写入 API 承担；控制台保持只读。
  这是既有架构决策（`src/addon/feishu/option/mod.rs:54-57`：「两个并存的可写入口会让
  审计语义与数据来源分叉」），本设计不推翻它。
- 不改 `engine/`（通用解释引擎）。所有新增代码落在 `features/feishu/` 与必要的 `shell/`。
- 不做权限管理面。用户与权限仍由运维 SQL + `access` 端口承担。
- 不做「按审批人过滤选项」这类新业务能力。

## 3. 决策记录

以下六条是本文其余部分的前提：

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| 1 | **列表形态** | **卡片 + 台账双视图，页内可切换** | 四个形态（卡片栅格／台账／主从分栏／向导优先）做了可交互原型横向比较。卡片在 ≤20 条时最强，台账在上百条时最强；**预期量级是「可能上百」**，故两者都要。实现在 §5.6 |
| 2 | **默认视图** | **台账**，且视图选择持久化 | 同上：默认值是按预期量级选的，不是按观感选的。持久化照 `shell/density.ts` 的 `yang.density.v1` 先例 |
| 3 | **搜索与分页** | **都要**（两个视图共享一份状态） | 上百条量级下没有搜索等于不可用。`title` 已 `.searchable(true)`，搜索**不动后端**；分页控件形态见 §5.6 |
| 4 | 导航入口 | 只保留一个入口：两个 module 的 `presentation()` 与 `view()` **成对删除** | §4.1、§5.3-1。**成对是硬要求**，只删一个各有更坏的后果 |
| 5 | 详情页能力 | **只读**（不新增选项写 Action） | §2 |
| 6 | 详情页「最近写入」列 | **做，且默认按它倒序** | §4.8。**它不是「推送还活着吗」的信号**——入站推送与出站拉取都会写选项行，两者都改 `updated_at`。同步存活改看数据源行的 `last_success_at` / `consecutive_failures`（详情页新增的「同步」区）。列名已由「最近推送」改为「最近写入」 |
| 7 | 创建/轮换后 | **自动做一次 Token 连通性预检** | §4.9。这是全系统唯一能验证凭据的时刻（服务端永远回不出明文） |
| 8 | 指引形态 | **常驻横幅归零，改情境化指引 + 字段级帮助文字** | §5.5 |
| 9 | 表单 | **手写表单**，不走通用 `JsonSchemaForm` | 路线 B 的定义就是前端全权自建；也因此原 §5.3 的两处后端改动取消 |
| 10 | 角色分流 | 读 `catalog.actions` 里有没有对应 `operation_id`，**不解析 JWT** | §4.5 |

> 决策 1 的四个原型：`A-card-gallery.html`（选定）、`B-dense-directory.html`、
> `C-master-detail.html`、`D-setup-wizard.html`，位于 `.feishu-console-mockups/`。

## 4. 关键事实（复核结论，带锚点）

**实现时如与之冲突，以本节为准并回头修订。**

### 4.1 两条「自定义界面」路线，能力不对等

**路线 A：registry 型自定义视图**（`features/registry.ts` + 后端 `interaction = custom`）

- props 契约只有三个字段（`registry.ts:9-13`）：
  `{ presentation: ActionPresentationSchema; actions: ActionDemoSchema[]; onClose: () => void }`。
  **没有 router 上下文、没有 URL、拿不到 `catalog.modules` / `table_views`。**
- 触发链路的起点在 `engine/renderers/action/use-presented-actions.ts:113-121`：
  `interaction === "custom"` 时调 `options.onCustom(presentation, row)`。
- 渲染分发共 3 处，都在 `shell/`：`pages/ModulePage.tsx:80`、`pages/BusinessPage.tsx:49`、
  `pages/WorkbenchPage.tsx:44`（DEV 门控）。
- **后端没有「模块默认就是自定义视图」的声明面**：`ModulePresentationSpec.views` 只装
  TableView id（lib_yang `crates/yang-base/src/definition/ui/module.rs:148-149`）。
  所以走这条路时，自定义视图只能由一个 **Action 的 Custom 交互**触发——入口会挂在通用
  表格的一个按钮上，用户先看到表格再点按钮才进卡片。

**路线 B：静态路由自建页**（照 `account` 路线）

- 前端全权自建页面与 URL。先例：`shell/routes.tsx:72` 的 `{ path: "account", ... }`；
  该页面不读 catalog、不含 presentation。
- 侧边栏需要手写一条 `NavLink`：硬编码先例是 `shell/AppLayout.tsx:246-262`。

**本设计选路线 B。**

> **选 B 的最强理由不是首屏预算，而是能力缺失。** 路线 A 的 props 里没有 URL、拿不到
> catalog，「点卡片进详情、详情有可分享的地址、能前进后退」在这条路上**根本无法表达**；
> 而且它必须先渲染通用表格才能给出入口。首屏预算只是顺带的好处。
>
> 代价用**路由级 `lazy`** 消除：`verify-bundle-budget` 只测首屏**静态闭包**、不含
> dynamic import。仓库已有的先例是 React Router 的**路由级 `lazy:` 字段**
> （`routes.tsx:14-24`），照它写，不必引入 React `lazy`。
>
> `registry.ts` 的静态字面量门禁（`scripts/check_architecture.py:812-824`）只作用于
> registry 文件本身，不影响 `routes.tsx` 用 lazy。

### 4.2 后端数据面已经够用，不需要新 Action

飞书 addon 共 **8 个 Action**：

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
  前端实现「留空 = 不改」只能靠**省略该 key**，不能传空串。
- `status` 只接受 `"active" | "disabled"`。
- `title` 必须 1..=100 **字符**（错误原文 `名称必须在 1..=100 字符`，`create_datasource.rs:66`）。
- `token` 显式传空白会报错（`Token 不能为空；不轮换请省略该字段`）。
- 全部字段都省略时报 `ParamInvalid「没有要更新的字段」`。
- 数据源不存在时报 `RecordNotFound("数据源不存在")`。

**按数据源看选项**：`list_options` 把 `source_key` 作为**顶层字段**发在请求体里
（`domain/list_input.rs:52-53`，生效处 `option/actions/list_options.rs:45-48`），
**不是塞进 `where`**。输出 `{items:[{option_id,source_key,label,i18n,sort_order,is_default,enabled}],…}`；
`i18n` 是 JSON 文本原样返回；未显式排序时回退 `sort_order ASC + option_id ASC`。

**`delete_datasource` 会连带停用其下全部选项**（`datasource/actions/delete_datasource.rs:81-95`），
返回 `disabled_options` 计数——卡片上的删除确认文案必须说清这一点。

> **二次确认文案声明在 View 上**：`datasource/mod.rs:100-108` 的 `present_action(...)`
> 带 `ActionConfirmation::new("删除数据源", "删除后其下全部选项会被同时停用，且不可恢复。确认删除？")`。
> 删掉 `view()` 之后这条拿不到，**由前端持有同一份文案**（决策 2 已接受这个代价）。
> 注意 `catalog.actions` 里没有 `confirmation` 字段（`engine/contracts/ui-catalog.ts:26-57`），
> 它只存在于 `modules[].action_presentations` 与 `table_views[].action_presentations`。

### 4.3 表格形态已经具备，缺的只是列表形态

`datasource/mod.rs` 现有的 `view()` 已把 5 列 + 工具栏「新建」(Form) + 行内「更新」
(Form, `record_parameter="source_key"`) + 行内「删除」(Invoke, 带确认文案) 全部投影出来。
**数据面完整**，本设计要做的是换一层前端形态。

`record_parameter` 是构建期强校验的（lib_yang `builder/compile.rs:509-539`）。

### 4.4 前端现状中的缺口

1. **没有任何 onboarding 设施**。全 `src/` 搜「帮助 / 指引 / 使用说明 / 了解更多 / 如何」
   零命中；`EmptyState` / `Tooltip` / `Toast` / `Card` / `Tabs` / `Accordion` 组件均不存在。
   `shared/ui/` 只有 10 个原语：badge、button、checkbox、dialog、dropdown-menu、input、
   label、select、skeleton、table。指引形态与空状态都要从零造。
2. **后端不下发 `columns[].display`**（lib_yang `definition/ui/table.rs:61-81`）。
   所以「启用 / 停用」徽标不能靠后端元数据，前端按 `status` 字符串自己映射。
3. **`Badge` 没有语义色 variant**：只有 `default / secondary / destructive / outline`，
   而 `index.css` 的 palette 是**全消色**的（每个 token 都是 `oklch(L 0 0)`），**没有绿色**。
   要表达「启用」必须先往 `index.css` 的 `:root` 与 `.dark` 两块**同时新增语义 token**
   （`--tone-positive` / `--tone-warning` / `--tone-info`，彩度压在 0.08–0.12），
   组件只引用 token，不写内联色值（`frontend/AGENTS.md` 禁止手写颜色）。

### 4.5 权限是按身份投影的，`catalog` 就是权限信号

- UI 目录**本身已按身份过滤**：`crates/yang-base/src/definition/builder/registry.rs:245,256`
  用 `.filter(|runtime| runtime.policy.allows(context))`；
  `PolicyMiddleware::allows = authorize(ctx).is_ok()`（`router/middleware.rs:77-79`）。
- 所以「当前身份有没有写权限」= `catalog.actions` 里有没有 `feishu.datasource.create_datasource`。
  **不需要前端解析 JWT**（前端目前也没有任何 JWT 解码代码）。
- 三个权限是**三粒独立位**：`feishu.datasource.read`、`feishu.datasource.write`、
  `feishu.option.read`（分别在 `list_datasources.rs:41`、`create_datasource.rs:84`、
  `list_options.rs:24`）。存在「有 `datasource.read`、没有 `option.read`」这种组合：
  列表正常，点进详情**选项区 403**。设计必须覆盖它（§5.7）。
- ⚠️ **`ActionPresentationSchema.availability` 不能用于角色判断。** 它是**声明期静态提示**
  （DSL 里写死的），后端测试明确「availability disabled 不能替代服务端授权或阻断真实派发」
  （`ui/__tests__/table_view_test.rs:334`）。

### 4.6 三个字段的真实约束（原先写错的地方）

| 字段 | 真实约束 | 出处 |
|---|---|---|
| `source_key` | 必须**小写字母开头**，其余只含 `[a-z0-9_]`，≤64 **字节**。**连字符非法。** 错误原文：`数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]` | `create_datasource.rs:45-52,57-61` |
| `default_locale` | 取值域是 **`zh_cn` / `en_us` / `ja_jp`**（下划线，不是 `zh-CN`）。**后端对它零校验** | `domain/protocol.rs:50-52`、`domain/i18n.rs:13`、`datasource/table.rs:45` |
| `title` | 1..=100 **字符**，错误原文 `名称必须在 1..=100 字符` | `create_datasource.rs:63-67` |

**`default_locale` 写错的后果不是显示问题**：`i18n.rs:59-72` 的 `build_result_body`
把它**原样当作回给飞书的 `locale` 键**，而 `create_datasource` / `update_datasource`
对它没有任何校验。前端写成 `zh-CN` 会把这个非法值写进库，并让该数据源在飞书侧
**所有语言下都取不到文案**（控件显示为空），而控制台里看不出是自己造成的。
**所以它必须做成三选一，不能是自由文本。**

### 4.7 「加密返回」不是「加密存储」，而且不能显示为健康状态

- `datasource/table.rs:42` 的字段 title 就是**「加密返回」**。它加密的是
  **返回给飞书的选项信封**，需要服务端配置 `feishu.encryption_key`。
- 它与 Token 存储**毫无关系**：`token_hash` 无论如何只存 SHA-256 摘要（`table.rs:33-39`）。
  写成「加密存储 / Token 以密文保存」会得出一个与安全相关的**错误结论**。
- 它的失败模式在控制台内**不可归因**：开了但服务端没配密钥的数据源，卡片上是一片健康色，
  而它每次被飞书调用都返回 `50002 服务端未配置加密密钥`（`approval_options.rs:57-58`）。
  控制台看不到服务端配置（`config/mod.rs:577-581`）。
  **因此这个标记不能与 `status` 的「启用」共用同一个语义色**，改用 info。

### 4.8 一个被漏掉的高杠杆改动：`updated_at` 已经在表里

- `feishu_option.updated_at` **已声明**（`option/table.rs:59`），框架每次 UPDATE 自动刷新
  （`crates/yang-base/src/table/table_query/sql_render.rs:474-492`）。
- 而只有持管理 Token 的多维表格自动化会写选项行——所以它就是
  **「这行选项最后一次被推送的时间」**。
- **但 `list_options` 没有 select 它**（`list_options.rs:35-43`）。`feishu_datasource.updated_at`
  同理（`datasource/table.rs:55` 有，`list_datasources.rs:55-61` 没 select）。

补上它就是**控制台对「推送还活着吗」唯一能给、且诚实的信号**。
这是原设计漏掉的最高杠杆改动（§5.3-2）。

### 4.9 服务端永远回不出明文 Token

`feishu_datasource.token_hash` 只存 SHA-256 摘要（`datasource/table.rs:33-39`），
单向不可逆。**因此：**

- 指引里不能展示 Token 本身，只能告诉用户「去飞书审批后台复制你当初填的那个值」。
- **「校验数据源还有效吗」这类事后校验结构上不可能**——唯一的校验器
  `approval_options`（`approval_options.rs:76-101` 的 `verify_source`）需要**明文**才能比对
  存储的哈希。所以控制台**不能**提供「点一下校验这个数据源」的按钮。
- **但创建/轮换的那一刻用户手里正握着明文**，那是全系统唯一能一次验完
  「数据源在不在 + Token 对不对 + 有没有被停用」的时机（对应 40401 / 40102 / 40301
  三个可归因码，`approval_options.rs:46-62`）。这决定了 §5.5 的指引落点。

> **预检必须排在「创建成功之后」，不能排在表单提交之前。**
> `approval_options` 是按 `source_key` 查库、再拿存储的哈希与传入的明文比对的
> （`verify_source`），所以**数据源行必须先存在**。创建前调用只会拿到「不存在」这一种结果，
> 没有信息量。轮换 Token 同理——`update_datasource` 成功之后再检。
>
> **实现前需确认一处契约**（本文档未核实）：`approval_options` 的 Token 是经由
> header / body / query 中的哪一个传入、字段名叫什么。这决定预检请求怎么写。
> 它是 `public` 端点（按数据源 Token 自校验），但**前端调用时必须显式带上这颗明文 Token**，
> 与其它受保护 Action 走 `Authorization` 头的方式不同。

### 4.10 列表类 Action 的查询能力边界

| 约束 | 事实 | 后果 |
|---|---|---|
| 排序 | `feishu_datasource` 里**只有 `source_key`** 声明了 `.sortable(true)`；`title` 只有 `.searchable(true)`（能搜、**不能筛、不能排**）；`status` 三位全关 | 台账列头目前**只能按标识排序**；上百条量级下用户一定想按名称排 → 需补 `.sortable(true)`（§5.3-3） |
| 无默认排序兜底 | `list_datasources.rs:74-76` 只遍历传入的 `order_by`（对比 `list_options.rs:62-72` 有回退） | **不发 `order_by` 就是无序分页，翻页会重复/漏行。** 列表页必须每次显式发排序键 |
| 筛选 | `status` **没有** `.filterable(true)`（`datasource/table.rs:46-51`），框架对筛选 fail-closed（`table_query/validation.rs:135-143`） | 「全部 / 启用 / 已停用」筛选**服务端做不到**。上百条量级下这是刚需（找停用的那些）→ 需补这一行（§5.3-3） |
| 计数 | `total` 只在显式请求 `count_total` 时才非 null（`list_options` 已确认如此，`list_datasources` 实现时需同样确认） | 分页控件要显示「共 N 个」就必须带上这个开关，否则只能显示「下一页/上一页」 |
| 不对称 | 选项侧**可以**按 `enabled` 筛（`option/table.rs:53-57` 声明了 filterable），数据源侧的 `status` **不能** | 两个页面的筛选能力不对称，不能画同样的筛选器 |

## 5. 设计

### 5.1 落位

新增业务域 `frontend/src/features/feishu/`，与 `features/account/` 同构：

```text
frontend/src/features/feishu/
├── api.ts                          # Action 调用薄封装 + query key
├── list-query.ts                   # 搜索 / 筛选 / 排序 / 分页 / 视图选择
│                                   #（两个视图共用同一份状态，见 §5.4）
├── views/
│   ├── DatasourceListPage.tsx      # 列表页（双视图 + 情境化指引 + 空状态）
│   └── DatasourceDetailPage.tsx    # 某数据源下的选项（只读）
└── components/
    ├── DatasourceCardGrid.tsx      # 卡片视图（栅格 + 卡片内操作）
    ├── DatasourceLedger.tsx        # 台账视图（可排序列头 + 密度行高）
    ├── ListToolbar.tsx             # 视图切换 + 搜索 + 状态筛选
    ├── ListPagination.tsx          # 分页（两视图共用状态，外观按视图不同）
    ├── DatasourceFormDialog.tsx    # 新建 / 重命名共用的手写表单对话框
    ├── TokenPrecheckNotice.tsx     # 创建后的连通性预检回执（§5.4）
    ├── ConfirmDialog.tsx           # 停用 / 删除确认（标题 + 说明 + 取消/确认）
    └── StatusBadge.tsx             # tone 语义徽标（新增 tone token 的消费方）
```

**依赖方向**（`frontend/AGENTS.md` 锁定）：`shared` ← `engine` ← `features` ← `shell`。
新代码只向下依赖，不 import 其它 feature 域。

> **`api.ts` 必须是 `invokeAction` 的薄封装，不要手写 fetch。**
> `engine/index.ts` 已导出 `invokeAction` / `useUiCatalog` / `ActionDemoSchema`；
> `invokeAction`（`engine/http/client.ts:47-64`）从 Action schema 解析 method+path、
> 做 token refresh、信封解析与错误映射。
> `features/account/api.ts:3-7` 有 4 处 `@/engine/xxx` **深路径 import**，违反
> `frontend/AGENTS.md` 强制规则 2（features 引用引擎一律走 `@/engine` 公共出口）——
> **不要照抄它**；把 account 称作「同构先例」指的是目录结构，不是它的 import 写法。

### 5.2 路由与导航

`shell/routes.tsx` 在 `RequireAuth` 之下新增两条 **lazy** 路由，写法照
`routes.tsx:14-24` 的路由级 `lazy:` 字段：

| 路径 | 页面 |
|---|---|
| `/feishu/datasources` | `DatasourceListPage` |
| `/feishu/datasources/:sourceKey` | `DatasourceDetailPage` |

`shell/AppLayout.tsx` 加一条 `NavLink`「飞书数据源」→ `/feishu/datasources`，
样式照 `AppLayout.tsx:246-262`。

> **⚠️ 但渲染条件必须加权限门控。** `AppLayout` 里那条硬编码先例是**无条件渲染**的
> （因为人人都有账号）；飞书这条不是——三个权限位独立，存在连
> `feishu.datasource.read` 都没有的身份。照抄会让这种身份看到入口、点进去整页 403。
>
> 条件写成：
>
> ```tsx
> const canRead = catalog?.actions.some(
>   (a) => a.operation_id === "feishu.datasource.list_datasources",
> );
> ```
>
> 这与 catalog 的投影机制一致（§4.5），不需要新的声明面。

> **记录级详情路由在本仓库是先例为零的**（现有带参路由只有 `m/:moduleId` 与
> `m/:moduleId/v/:viewId`，且都指向同一个 `ModulePage`）。这条新范式评审时值得特别看一眼。
> 退化方案是 `?sourceKey=`（`BusinessPage` 有先例），但那样会丢掉可分享路径。
> 详情页需要自己定义「返回列表」的位置（仓库没有 `Breadcrumb` 组件）。

### 5.3 后端改动（三处必做）

1. **两个 module 成对删除 `presentation()` 与 `view()`**，保留 Action 注册与认证中间件。
   效果：导航里不再出现自动生成的「飞书数据源」「飞书选项」表格入口。

   **成对是硬要求**，机制在 `crates/yang-base/src/definition/builder/compile.rs:342-345`：

   ```rust
   let Some(presentation) = &module.presentation else {
       continue;              // 无 presentation 的模块根本不进 RuntimeModule
   };
   ```

   两个半吊子改法各有更坏的后果：

   - **只删 `view()`**：`compile.rs:157-183` 会给 `module.views` 为空的模块**自动合成**
     一个 `{module}.default` 视图，含全表列，只因 `data_action` 为 `None` 才被
     `registry.rs:177-187` 丢掉——今天是惰性的非契约行为，将来给模块加一个可用作数据源的
     primary action，整张表就会静默复现。
   - **只删 `presentation()`**：模块离开 `catalog.modules`，但 view 仍在 `catalog.table_views`，
     `shell/navigation.ts:21-38` 的 `unassignedViews` + `syntheticPageForView` 会把表格
     **挂到「工作台」分组下重新出现**。

   - 需同步删除两个 module 里针对 View 的测试
     （如 `option/mod.rs:108-113` 断言 `spec.actions.len() == 1` 的那条）。
   - **需验证**：去掉后 `AppBuilder::build` 的模块内容校验仍通过；且 `catalog.actions`
     仍包含全部 8 个 Action（卡片页照常能调）。

   > **顺序**：新页面先上线并验证，**再**摘 `presentation`/`view`。
   > 反过来做，中间态是两个飞书 module 完全没有 UI。

2. **`list_options` 与 `list_datasources` 补 `updated_at` 的 select，并给 `updated_at` 加
   `.sortable(true)`。**（§4.8、决策 6）
   这是本次「花一行代码、换一个真能力」的改动：补上之后详情页可以显示并**按**
   「选项最后一次被推送的时间」排序，故障排查才有一手证据。
   排序要显式声明，因为框架对未声明 sortable 的字段排序是 fail-closed 的（§4.10）。
   - 需同步重新生成 OpenAPI 快照与 TS 类型
     （`python scripts/dump_openapi.py` + `pnpm gen:contracts`）。

3. **给 `feishu_datasource` 的 `title` 补 `.sortable(true)`、`status` 补 `.filterable(true)`。**
   （§4.10、决策 3）
   台账视图在上百条量级下**必须**能按名称排序、按状态筛选，而这两个字段现在都不支持
   （`title` 只有 `.searchable(true)`、`status` 三位全关）。框架对两者都是 fail-closed，
   所以这不是「优化」而是「能力缺口」。
   - 两处各一行；补完后台账列头的「名称」列可排序、工具栏可放「全部 / 启用 / 已停用」筛选。
   - **若不做这一条**，台账视图就必须砍掉列头排序与状态筛选，只剩搜索——
     请在那时明确接受这个降级。

> **原 §5.3 的改动 2、3（补 schemars title、把 token 标为密码型）全部取消。**
> 理由是决策 5 选了手写表单：表单一旦手写，`<Label>接口 Token</Label>` 和
> `type="password"` 就是自己写的一行，那两处后端改动只影响通用 `JsonSchemaForm` 路径，
> 对手写对话框毫无作用。**两者只能二选一，原设计两头都写了。**
>
> 补充一条实测结论，用于将来真要复用通用表单时：**`#[schemars(title)]` 对可选字段不生效。**
> `engine/contracts/json-schema.ts:50-59` 的 `effectiveSchema` 是**替换**而非合并——
> 它取 `anyOf` 的非 null 分支后直接返回该分支，**丢掉外层的 `title`**；
> `SchemaField.tsx:58` 的 `resolved.title` 因此拿到 `undefined`，回落到英文字段名，
> 且失败是**静默的**。`encrypt_enabled` / `default_locale` 恰恰都是可选的。

### 5.4 交互契约

| 动作 | Action | 输入 | 界面 |
|---|---|---|---|
| 搜索 | `list_datasources` | `{search, …}` | 列表上方搜索框（**两个视图共用**） |
| 按状态筛选 | `list_datasources` | `{where: status eq …}` | 台账工具栏「全部 / 启用 / 已停用」；依赖 §5.3-3 |
| 排序 | `list_datasources` | `{order_by, …}` | 台账列头（名称 / 标识）；卡片视图用同一个排序键、不显示列头 |
| 翻页 | `list_datasources` | `{page, page_size, count_total}` | 列表底部（两个视图共用同一个页码控件） |
| 切换视图 | —（纯本地） | — | 工具栏视图切换；选择持久化（决策 2） |
| 添加数据源 | `create_datasource` | `source_key` / `title` / `token` / 可选加密与语言 | 右上角按钮 → 手写表单对话框 → **成功后自动预检** |
| 重命名 | `update_datasource` | `{source_key, title}` | 列表项菜单 → 同一个表单对话框（预填） |
| 停用 / 启用 | `update_datasource` | `{source_key, status}` | 列表项菜单 → **确认对话框**（文案见下） |
| 删除 | `delete_datasource` | `{source_key}` | 列表项菜单 → **二次确认**（文案见下） |
| 查看选项 | `list_options` | `{source_key, …标准分页六键}` | 详情页表格（只读） |

**列表查询必须每次显式带排序。** 默认 `order_by = title ASC, id ASC`（`id` 收尾是
为了保证全序：`title` 会重名）；台账列头切换排序时换成对应字段。
**不要用 `source_key`**——它不在表级行上，排序校验会直接 400。
不发就是无序分页，翻页会重复/漏行（§4.10）。

**搜索与筛选的结果集变化要回到第 1 页**，否则会停在一个不存在的页码上。

**视图不改变数据。** 卡片与台账是同一份 `list_datasources` 结果的两种渲染，
不各自发请求，也就不会出现「切了视图数字对不上」。

**创建/轮换后的自动预检**（决策 7）：

1. 表单提交 → `create_datasource` 成功（或 `update_datasource` 轮换成功）。
2. **在关掉对话框之前**，用刚输入的明文 Token 调一次 `approval_options`。
   （顺序不能颠倒，原因见 §4.9；Token 的传递方式实现前需确认。）
3. 回执按真实结果分三种：
   - **通了** —— 渲染带真实 `source_key` 的「把这段粘回飞书审批后台」+ 复制按钮（§5.5-2）。
   - **402/401（Token 不匹配）** —— 明确说「Token 与数据源不一致」，并指出**数据源已创建**，
     可以就地轮换重填，不必删掉重建。
   - **40301（数据源已停用）/ 其它可归因码** —— 照实回显码与含义。
4. 预检**失败不阻塞**创建结果——数据源已经建好了，这一步只回答「链路通没通」。

**表单规则**（手写，故这些都在前端）：

- `source_key`：帮助文字写「小写字母开头，只能包含小写字母、数字与下划线，最长 64 字节。
  它会进接口 URL。」输入时实时校验（`^[a-z][a-z0-9_]{0,63}$`）。
  **创建后不可改**（`UpdateDatasourceInput` 里它是主键）。
- `default_locale`：**三选一**下拉，value 用 `zh_cn` / `en_us` / `ja_jp`，
  显示文字用「简体中文 / English / 日本語」。默认 `zh_cn`。（§4.6）
- `token`：密码型输入；帮助文字写明「服务端只保存它的 SHA-256 摘要，之后无法回显」。
  **重命名时留空 = 不提交该字段**（不是传空串，否则报 `Token 不能为空`）。
- `encrypt_enabled`：标签写**「加密返回」**，说明写「返回给飞书的选项内容加密传输；
  需服务端已配置加密密钥」，并**常驻一行**写明真实代价：未配置时会返回
  `50002 服务端未配置加密密钥`，控件取不到任何选项。（§4.7）

**两个确认对话框要说得出区别：**

| | 标题 | 正文要求 |
|---|---|---|
| 停用 | 停用数据源 | 说明**正在使用该数据源的飞书审批控件会立即取不到选项**（40301），且随时可再启用恢复、数据与选项都不删 |
| 删除 | 删除数据源 | **逐字用后端原文**：「删除后其下全部选项会被同时停用，且不可恢复。确认删除？」 |

用「标题 + 说明 + 取消(ghost) / 确认(destructive)」的对话框形态，
**不要**照抄 `AccountSettingsPage` 里的 `window.confirm`（那是历史遗留，不是新代码的范式）。

**提交成功后回读 `list_datasources` 刷新**——`update_datasource` 只返回 `{affected}`，
不含最新记录。**既然回读是必需的，就不做乐观更新**（乐观更新只买到一次闪烁）。
同理，**已停用的数据源在卡片菜单里直接给「启用」**——只多一个条目，
却消掉了「唯一恢复路径藏在看不见的菜单里」这个状态。

### 5.5 指引：情境化，不做常驻横幅

**删掉「顶部常驻可折叠指引」。** 理由：原文 §5.5 自己写了「有数据时默认折叠」——
设计者已经预设了它不被展开，稳态下它就是列表上方一条没人点的折叠带。
而且它**结构上做不到「具体」**：横幅在渲染时不知道用户刚建了哪个数据源。

改成**指引只出现在卡住的那一刻，并携带那一刻的数据**，四个落点：

1. **空态（没有数据源）** —— 四步就是页面正文。这是唯一会真正被读的时刻，
   也正是空态该给「一件事的说明 + 一个明确动作」的地方。此时**没有卡片栅格**，
   整屏归「建档」。
2. **创建 / 轮换成功的回执** —— 控制台在这个时刻**知道真实的 `source_key`**，
   能渲染出「要粘回飞书审批后台的那一段」+ 复制按钮。
   静态横幅做不到这件事，这是控制台唯一能「具体」的指引时刻。
   > ⚠️ **这里不能有「校验数据」按钮。** 唯一的校验器 `approval_options` 需要明文
   > Token 才能自校验，而服务端永远回不出明文（§4.9）——控制台**做不到**这件事。
   > 原 §5.5 第 3 步「点『校验数据』确认能拉到选项」承诺了一个不存在的动作。
   > 那一步实际发生在飞书审批后台自己的界面里。
   > **可选**：在同一张表单里放一个「用刚输入的 Token 试拉一次」的预检
   > （此刻明文在手，调 `approval_options` 回显真实错误码）。这是全系统唯一的验证时机。
3. **详情页 0 选项** —— 数据源刚建好、自动化还没推过任何选项。这是最真实的首次体验，
   也正是「链路到底通没通」最需要回答的一屏。写明下一步去哪做（多维表格的自动化），
   **不要**只写「暂无数据」。
4. **详情页 403（缺 `option.read`）** —— 说明「你没有查看选项的权限」，而不是白屏或空列表。

**其余知识降级成字段级帮助文字**（那才是做决定的地方）：Token 只存摘要、
加密返回需要服务端已配密钥、`source_key` 会后进 URL。

**四步的内容**（只写「去哪做、填什么」，不写具体接口路径与字段名，也不放 Token 明文）：

1. **在飞书审批后台配置控件** —— 单选/多选控件选「使用外部选项」，自定义一个 Token
   并记住它（服务端只存摘要，之后无法回显）。
2. **在这里建一个数据源** —— 数据源标识会进接口 URL；粘贴上一步的 Token。
3. **把接口地址与 Token 填回审批后台** —— 在飞书那边点「校验数据」确认能拉到选项。
4. **在多维表格配自动化推送选项** —— HTTP 节点带 `Authorization: Bearer <管理 Token>`
   调写入接口，选项随表格变动自动更新。

### 5.6 两个视图各展示什么

`list_datasources` 的表级行返回 `id` / `title` / `status` / `updated_at` /
`ingest_mode` / 三个坐标 / 同步状态六个字段，外加一个 `fields[]`（字段绑定）。
两种视图用的是**同一份数据**。

> **表级行上没有 `source_key`、没有 `encrypt_enabled`、没有 `default_locale`。**
> 一条数据源 = 一张表 = N 条绑定，所以这三项都属于**绑定层**（`fields[]` 里的每一条）：
> `source_key` 进 URL、后两项在详情页的字段绑定表里逐字段显示。
> 本节曾把 `encrypt_enabled` / `default_locale` 写成表级的「公共字段」，照那写就会把
> 已删的东西正好加回表级行与卡片——**它们恒为默认值，画出来是一句假话**。

**公共字段渲染规则：**

- `status` → 前端映射「启用 / 已停用」，用 tone 语义色（后端不下发 `display`，§4.4-2）
- 绑定的 `default_locale` → **显示为「简体中文 / English / 日本語」而不是 `zh_cn`**
- 绑定的 `encrypt_enabled` → 为真时一个「加密返回」标记，**用 info 色而非 positive**（§4.7）

**卡片视图**（概貌用）：

- 标题位 `title`；副标题位**首个绑定的 `source_key`**（等宽；一条表级行有 N 个）
- 徽标行：**只有 `status`**。原设计这里是 `status` + 「加密返回」+ 默认语言，但那两个
  属性属于**绑定层**（一条数据源有 N 个字段，可以各自加密、各自语言），表级行上没有
  单一值可显示——照着画的结果是对每一条数据源都恒画「—」与一个空语言徽标。
  逐字段的取值在详情页的字段绑定表（`FieldBindingsTable`），见 §4.7 末。
- 栅格 `repeat(auto-fill, minmax(260px, 1fr))`；卡片整块可点进详情
- 菜单「⋯」在右上角，**只在有写权限时渲染**

**台账视图**（查找与对比用，**默认视图**）：

| 列 | 内容 | 可排序 |
|---|---|---|
| 名称 | `title` | 是（依赖 §5.3-3） |
| 标识 | 首个绑定的 `source_key`，等宽 | **否**（表级行上没有单一 `source_key`，点一下会把请求打成 400） |
| 状态 | tone 徽标 | 否（`status` 未声明 sortable） |
| （操作） | 「⋯」菜单，悬停/聚焦时出现 | — |

> 「加密返回」与「默认语言」两列**已删**（原设计有）：它们是**绑定级**属性，
> 表级行上恒为空，逐字段的取值改在详情页的字段绑定表里显示。

- 工具栏：搜索框 + 「全部 / 启用 / 已停用」分段筛选（筛选依赖 §5.3-3）
- 行高受 `--density-cell-y` 驱动（`html[data-density]`），页头「密度」菜单实时切换
- 行整行可点进详情
- **列头不要给未声明 sortable 的列画排序箭头**——那是在要求后端改动

**两个视图都不能有的东西：**

| 不要画 | 原因 |
|---|---|
| 选项数量（「8 个选项」） | `list_datasources` 不返回选项计数。要显示需给它加字段，或逐源多打一次 `list_options` |
| 数据源的「最后同步时间」 | `list_datasources` 不返回 `updated_at`。补 §5.3-2 之后**可以**加（详情页的选项侧已按决策 6 做了） |
| Token 明文（哪怕「查看」按钮） | 服务端只存 SHA-256 摘要（§4.9） |
| 接口地址 | 指引里只写「去哪做、填什么」 |

> **卡片视图的密度代价**：卡片的信息量等于台账的一行，但占 3–4 倍纵向空间，
> 且拿不到列头排序。这正是默认视图选台账的原因（决策 2）——
> 卡片的价值在「数据源不多时一眼看到有哪些、哪些停了」，不在上百条时找那一个。

### 5.7 状态与角色

必须实现并逐一手工验收的状态：

| 状态 | 要求 |
|---|---|
| 加载中 | 按当前视图渲染骨架屏（`Skeleton`）：卡片视图是卡片形状的骨架，台账视图是若干行骨架。不用转圈 |
| 空（一个数据源都没有） | 四步指引正文 + 主行动「添加数据源」，**不渲染空栅格/空表**，工具栏也不渲染 |
| **搜索/筛选无结果** | 与上面区分开：说明「没有匹配的数据源」+ 一个「清除筛选」的动作。**不是**「暂无数据」 |
| 错误 | 错误条（`role="alert"`）+ 「重试」；用真实文案 |
| 有数据 | 当前视图（卡片栅格或台账） |
| **翻页边界** | 结果集变化（搜索/筛选/删除）后回到第 1 页；空页不出现（见 §5.4） |
| 详情 · 有选项 | 只读选项表，含「最近推送」列，默认按它倒序（决策 6） |
| 详情 · **0 选项** | 专门空态，指向多维表格自动化（§5.5-3） |
| 详情 · **403** | 缺 `option.read` 时的说明 + 重试，**不是**白屏也不是空列表（§4.5） |
| **创建后预检失败** | 数据源**已创建成功**；回执明确说清「建好了，但 Token 没验证过」+ 可归因码 + 就地轮换重填的入口（§5.4） |

**角色分流**：读 `catalog.actions`（§4.5）。

- **有 `feishu.datasource.write`**（`catalog.actions` 含 `create_datasource`）：
  显示「添加数据源」与卡片上的重命名 / 停用 / 删除。
- **无写权限**：这些入口**整个不渲染**（不是禁用）。理由：禁用表示「此刻不可用」，
  而这里的语义是「这个入口不属于你」。
  「禁用 + 理由」只用于真正的情境性禁用，例如在已停用的数据源上再点「停用」。
- 侧边栏那条 `NavLink` 的渲染条件是 `feishu.datasource.read`（§5.2）。

## 6. 测试与门禁

**前端**

- Vitest：`frontend/tests/features/feishu/`（镜像 `src/` 路径），覆盖：
  `api.ts` 的调用封装与 query key、`list-query.ts` 的状态归约、列表项操作状态流转、
  情境化指引的四个落点、权限门控（有/无写权限两态），以及这些**渲染分支**：
  空 / 搜索无结果 / 详情 0 选项 / 详情 403 / 创建后预检失败。
- **两个必须断言的查询行为**（错了会静默出错数据）：
  ① 每次 `list_datasources` 请求都带 `order_by`（不发就是无序分页，翻页重复/漏行，§4.10）；
  ② 搜索/筛选变化后回到第 1 页（§5.4）。
- **双视图一致性**：切视图不重新发请求、不改变结果集；同一份搜索词与页码在两个视图下相同。
- `pnpm check` 全链：Prettier → ESLint `--max-warnings 0` → `tsc --noEmit` → Vitest →
  `verify:locale-contract` → build → `verify:production-build` → `verify:bundle-budget`
  → `verify:deployment-contract`。
- **bundle 增量需实测**：理论上 lazy 路由不进首屏静态闭包、增量≈0，但要 build 后
  跑 `verify:bundle-budget` 确认（目标 350 kB / 硬上限 450 kB gzip）。

**后端**

- 删 `presentation`/`view` 后跑 `check_architecture.py` 与 `cargo test --lib`。
- 补 `updated_at` select 后重新生成 OpenAPI 快照与 TS 类型。

**端到端 —— 本次明确接受不覆盖**

**不扩演示后端。** 理由：Playwright 用的是 `examples/frontend_demo/` 那个无数据库演示后端
（`frontend/playwright.config.ts:20-42`），**里面没有飞书模块**。为它引入飞书模块与数据库夹具，
测到的是一条与真实契约无关的 mock 路径。

本功能的风险面只有三类：该身份有哪些 Action 可见（catalog 驱动）、5 个输入输出形状、
**三个渲染分支**。前两类 Vitest 可覆盖，第三类就是渲染分支本身——都在上面。
**改为**：Vitest 覆盖分支 + 一次手工全链路
（建数据源 → 填回审批后台 → 多维表格推一次选项 → 回详情页看到那行并看到「最近推送时间」）。

> 若将来仍要 e2e：`page.route()` 拦掉两个 Action 端点、对录制 fixture 断言即可，
> 不必动演示后端——路线 B 的页面数据全部走自建 `api.ts`，不依赖 catalog。

## 7. 风险

| 风险 | 处置 |
|---|---|
| 列表不发 `order_by` 导致翻页重复/漏行 | §5.4 已定为硬要求；前端每次显式带排序键，并在测试里断言 |
| 上百条时台账仍不够用（列头只有名称/标识可排） | 明确接受：其余列不画排序箭头（它们没声明 sortable）。要更多排序键就再加后端声明 |
| 双视图导致两套列表状态各写一份 | §5.4 约束为**同一份查询状态 + 同一份数据**，两种渲染；`order_by`/`search`/`page` 存在一处 |
| 默认视图选错（台账 vs 卡片） | 视图选择持久化（决策 2），用户一次切换后不再被默认值打扰 |
| 去掉 `presentation`/`view` 后构建期模块校验不过 | 实现时**先跑一次 build 验证**；两处必须成对删（§5.3-1） |
| 中间态是两个飞书 module 完全没有 UI | **新页面先上线并验证，再摘 presentation/view**（§5.3-1 的顺序要求） |
| 记录级详情路由是新范式，可能与现有导航不一致 | 评审时确认；仓库没有 `Breadcrumb`，返回按钮位置要自己定义 |
| `default_locale` 写成 `zh-CN` 导致飞书侧取不到文案 | 前端强制三选一（§4.6）；后端无校验，只能靠前端守 |
| 「加密返回」被当成健康状态展示 | 用 info 色而非 positive，并常驻写出 50002 的代价（§4.7） |
| `source_key` 连字符被当成合法输入 | 前端实时校验 `^[a-z][a-z0-9_]{0,63}$`（§4.6） |
| **预检把「创建成功」和「Token 不对」混成一屏** | §5.4 与 §5.7 已定：预检**失败不阻塞**创建结果，回执必须说清「数据源已创建」 |
| **预检顺序写反（提交前调用）** | §4.9 已写明必须排在创建之后；`approval_options` 依赖已存在的 `source_key` |
| 前端手写 `api.ts` 重复实现 HTTP 层 | 用 `@/engine` 的 `invokeAction`，不照抄 `features/account/api.ts` 的深路径（§5.1） |
| 指引文案与后端实际契约漂移 | 指引里不写具体接口路径与字段名，只写「去哪里做什么」 |

## 8. 未决项

> 三条原始未决项已全部定案：数据源量级按「可能上百」处理（决策 1–3）、
> 详情页做「最近推送」列并默认倒序（决策 6）、创建后自动预检（决策 7）。
> 以下是实现期才会遇到、需要当场确认的点，都不是设计分叉。

1. **`approval_options` 的 Token 传递方式**：header / body / query 中的哪一个、字段名叫什么。
   这决定预检请求怎么写（§4.9 已标记）。它是 `public` 端点，前端要显式带明文 Token，
   **不走** `Authorization` 头——与其它 7 个 Action 不同。
2. **`list_datasources` 是否支持 `count_total`。** `list_options` 已确认支持
   （不传则 `total` 为 null）；数据源侧实现时要确认，否则分页控件只能显示「上/下一页」，
   显示不了「共 N 个」。
3. **卡片视图的分页控件形态。** 仓库只有通用表格的分页脚可参考，卡片页的页码控件样式要新定
   （两种视图共用同一份分页状态，但外观不同）。
4. **视图切换控件的位置与形态。** 建议放在工具栏搜索框左侧、用分段控件（与状态筛选同族）；
   要避免与页头已有的「密度」菜单在视觉上打架。

## 9. 与其他文档的关系

- 后端（表结构、8 个 Action、飞书契约、加密与来源校验）见 lib_yang 仓库的
  `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。
- 新增业务域的结构规则见 `frontend/AGENTS.md`。
- 首个授权只能由运维 SQL 完成（决策 D2），见 `docs/contracts/AUTHZ_GRANTS.md`。
- 本次复核全文与四个可交互原型见 `project/yang-system/.feishu-console-mockups/`。
