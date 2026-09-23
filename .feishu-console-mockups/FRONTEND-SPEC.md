# 飞书数据源控制台 · 前端实现规格

配套设计文档：`docs/architecture/feishu-datasource-console.md`（**逐节照它做**）。
本文件只写"照着做会做错"的部分：已核实的契约、分层禁令、以及视觉口径。

---

## 0. 分层禁令（`frontend/AGENTS.md`，机器门禁强制）

依赖方向只允许向下：`shared ← engine ← features ← shell`。

- `features/` 引用引擎能力**一律走 `@/engine` 公共出口**。出口没有的能力先在
  `engine/index.ts` 显式导出——但**本次不要改 engine/**，出口已经够用（见 §2）。
  ⚠️ `features/account/api.ts` 有 4 处 `@/engine/xxx` 深路径 import，**那是违规样本，
  不要照抄**。
- `features/` 各域之间**禁止互相 import**。
- `shared/` 禁止 import engine/features/shell。本次**不新增 `shared/ui/` 组件**。
- 测试放 `frontend/tests/features/feishu/`（镜像 `src/` 路径），用 `@/` 与 `@test/` 别名。
  `src/` 里只放生产代码。
- 不新增 UI 依赖。可用的只有 `@/shared/ui/` 的 10 个原语：
  `badge button checkbox dialog dropdown-menu input label select skeleton table`。
  图标用 `lucide-react`。
- `src/engine/contracts/api-types.ts` 是生成物，**禁止手改**。

---

## 1. 后端契约（已逐条核实，照抄）

### 列表查询输入（`list_datasources` / `list_options` 共用，`ListInput`）

```jsonc
{
  "page": 1,                 // 1 起步
  "page_size": 10,           // 1..=100，越界 400
  "search": "北京",           // 在表声明的 searchable 字段上做关键词搜索
  "where": { "type": "eq", "field": "status", "value": "disabled" },
  "order_by": [{ "field": "title", "direction": "Asc" }],
  "count_total": true,       // 不传则响应里 total 为 null
  "source_key": "expense_category"   // 扩展字段，只有 list_options 用
}
```

**三个坑：**

1. `order_by[].direction` 是 **PascalCase 的 `"Asc"` / `"Desc"`**，不是小写。
2. 后端是 `deny_unknown_fields` —— 多传一个没声明的键就 400。
3. `total` 只在 `count_total: true` 时非 null。

### 响应形状

```jsonc
// list_datasources
{ "items": [{ "source_key", "title", "encrypt_enabled", "default_locale", "status", "updated_at" }],
  "page", "page_size", "total" }

// list_options
{ "items": [{ "option_id", "source_key", "label", "i18n", "sort_order",
              "is_default", "enabled", "updated_at" }], "page", "page_size", "total" }
```

`updated_at` 是 **unix 秒（number）**，不是 ISO 串。`i18n` 是 **JSON 文本**（可能是
`'{"zh_cn":"差旅费","en_us":"Travel"}'` 或 `null`），**不要直接铺在单元格里**。

### 三个字段的真实语义（写错会被用户当成 bug）

| 字段 | 真值 | 界面必须写成 |
|---|---|---|
| `status` | `"active"` / `"disabled"` | 启用 / 已停用 |
| `default_locale` | **`zh_cn` / `en_us` / `ja_jp`**（下划线） | 简体中文 / English / 日本語 |
| `encrypt_enabled` | 加密的是**返回给飞书的信封** | **「加密返回」**，不是「加密存储」；且与 Token 存储无关 |
| `source_key` | 小写字母开头，只含 `[a-z0-9_]`，≤64 字节 | 帮助文字写清；表单实时校验 `^[a-z][a-z0-9_]{0,63}$` |

### Action 清单（`operation_id`）

```
feishu.datasource.list_datasources   POST /api/v1/feishu/datasources/query
feishu.datasource.create_datasource  POST /api/v1/feishu/datasources
feishu.datasource.update_datasource  PUT  /api/v1/feishu/datasources
feishu.datasource.delete_datasource  DELETE /api/v1/feishu/datasources
feishu.option.list_options           POST /api/v1/feishu/options/query
```

**删除确认文案（后端原文，逐字，不得改写）：**
标题「删除数据源」，正文「删除后其下全部选项会被同时停用，且不可恢复。确认删除？」

**停用确认文案（前端自写，必须与删除说得出区别）：**
说清「停用后，飞书审批中正在使用该数据源的控件会立即取不到选项」，并说明
「数据源不会被删除、选项照旧保留、随时可再启用」。

`update_datasource` 语义：除 `source_key` 外全部可选，**省略即保持原值**——
「留空 = 不改」只能靠**不提交该 key** 实现，传空串会报 `Token 不能为空`。
全部字段都省略会报「没有要更新的字段」。

**前端错误文案分两层**：字段级校验用界面话；服务端拒绝类**直接回显后端原文**
（`数据源不存在`、`没有要更新的字段`、`Token 不能为空；不轮换请省略该字段`）。

### 列表必须每次显式带排序

`list_datasources` 后端现在有 `source_key ASC` 兜底，但**前端仍要显式发**，
并按用户选的列切换。默认 `[{field:"source_key",direction:"Asc"}]`。

**搜索/筛选/删除后必须回到第 1 页**，否则会停在不存在的页码上。

---

## 2. 引擎公共出口（`@/engine`）—— 只能用这些

```ts
import {
  invokeAction,            // (action, values, context: SessionContext, signal?, opts?) => Promise<InvocationResult>
  useUiCatalog,            // () => UseQueryResult<UiCatalog>
  useSessionCredentials,   // () => SessionContext
  ApiError,
  StepUpRequiredError,
  type ActionDemoSchema,
  type UiCatalog,
} from "@/engine";
```

**`invokeAction` 需要一个 `ActionDemoSchema`**（它从里面取 method + path）。
所以要：`useUiCatalog()` → `catalog.actions.find(a => a.operation_id === "…")` → `invokeAction`。

`InvocationResult` 的形状见 `@/engine` 的类型（含 `data` 与 `message`）。

**不要手写 `fetch`**。不要新建第二个 HTTP 客户端。

### 权限门控（设计文档 §4.5）

UI 目录**本身已按身份投影**。所以：

```ts
const canRead  = catalog?.actions.some(a => a.operation_id === "feishu.datasource.list_datasources");
const canWrite = catalog?.actions.some(a => a.operation_id === "feishu.datasource.create_datasource");
```

- `canRead` 决定侧边栏那条 `NavLink` 是否渲染。
- `canWrite` 决定「添加数据源」与列表项上的重命名/停用/删除**是否渲染**
  （不渲染，不是禁用——禁用表示"此刻不可用"，这里的语义是"这个入口不属于你"）。
- **不要**读 JWT、**不要**用 `presentation.availability`（它是声明期静态提示，
  后端测试明写它不能替代服务端授权）。

---

## 3. 视觉口径

### 新增的三个 tone token（写进 `src/index.css` 的 `:root` 与 `.dark` **两块**）

调色板原本全消色，**没有绿色**。要表达「启用」必须先加 token（`AGENTS.md` 禁止手写色值）：

```css
:root {
  --tone-positive: oklch(0.55 0.09 155);   /* 启用 / 已加密 */
  --tone-warning:  oklch(0.62 0.11 75);    /* 已停用 / 需注意 */
  --tone-info:     oklch(0.55 0.08 245);   /* 中性说明 / 关系 */
}
.dark {
  --tone-positive: oklch(0.72 0.11 155);
  --tone-warning:  oklch(0.78 0.12 75);
  --tone-info:     oklch(0.72 0.09 245);
}
```

并在 `@theme inline` 里补映射，让 Tailwind 能用 `text-tone-positive` / `bg-tone-positive/10`：

```css
@theme inline {
  --color-tone-positive: var(--tone-positive);
  --color-tone-warning:  var(--tone-warning);
  --color-tone-info:     var(--tone-info);
}
```

彩度刻意压在 0.08–0.12：界面其余部分完全消色，高饱和会显得是外来的。

### 组件口径（照抄，这是"像不像这个应用"的关键）

- 页面容器：`<main className="mx-auto w-full max-w-6xl space-y-6 p-6">`
  （`AccountSettingsPage` 用 `max-w-2xl`，列表页是宽内容，用 `max-w-6xl`；
  详情页用 `max-w-4xl`）
- 页头：`<div className="space-y-1"><h1 className="text-xl font-semibold">飞书数据源</h1>`
  `<p className="text-sm text-muted-foreground">…</p></div>`
- 分节卡片：`rounded-xl border border-border bg-card p-5`，标题 `text-base font-medium`
- **卡片不加阴影**，靠 1px 边框分界
- 状态徽标**不用 `Badge` 的四个 variant**（它们是"主要/次要"语义），
  按 tone 自己拼：`inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium`
- 错误条：`rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive` + `role="alert"`
- 中性说明条：`rounded-md border border-border bg-muted/50 px-3 py-2 text-sm` + `aria-live="polite"`
- 列表行：`flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2 text-sm`
- 元信息行：`text-xs text-muted-foreground`，多段用 ` · ` 连接
- 台账行高由 `--density-cell-y` 驱动：`style={{ paddingBlock: "var(--density-cell-y)" }}`，
  **不要写死 `py-3`**
- 数字用 `font-variant-numeric: tabular-nums`（Tailwind `tabular-nums`）
- `source_key`、`option_id`、Token 用等宽：`font-mono`

### 文案

中文，主动语态，按钮说清后果。提交中写「提交中…」。
**不写「暂无数据」**——空态要给"一件事的说明 + 一个明确动作"。

---

## 4. 要建的文件与各自职责

```
frontend/src/features/feishu/
├── api.ts                       # Action 薄封装 + query key（见下）
├── list-query.ts                # 搜索/筛选/排序/分页/视图选择的状态归约
├── types.ts                     # DatasourceItem / OptionItem / 查询状态类型
├── views/
│   ├── DatasourceListPage.tsx   # 双视图 + 情境化指引 + 空/加载/错误 + 权限门控
│   └── DatasourceDetailPage.tsx # 只读选项 + 最近推送列（默认倒序）+ 0 选项 + 403
└── components/
    ├── DatasourceCardGrid.tsx   # 卡片视图
    ├── DatasourceLedger.tsx     # 台账视图（名称/标识可排序）
    ├── ListToolbar.tsx          # 视图切换 + 搜索 + 状态筛选
    ├── ListPagination.tsx       # 分页（两视图共用状态）
    ├── DatasourceFormDialog.tsx # 手写表单（新建/重命名共用）
    ├── TokenPrecheckNotice.tsx  # 创建后的连通性预检回执
    ├── ConfirmDialog.tsx        # 停用/删除确认（tone 可配：destructive / 中性）
    └── StatusBadge.tsx          # tone 语义徽标
```

### `api.ts` 要点

- query key 工厂，例如：
  `["feishu","datasources",{page,pageSize,search,status,orderBy}]`、
  `["feishu","options",sourceKey,{page,pageSize}]`。
  ⚠️ 最近的提交 `R2-H6` 引入了「会话边界级联清空查询缓存」——
  新 key 必须在会话切换时被清掉。**照 `features/account/api.ts` 的 key 写法对齐**
  （它以 `["me"]` 之类顶层键为首段，被 session-reset 按前缀清理）。
- 每个调用：`catalog.actions` 里找不到 operation_id 时抛明确错误（不要静默）。
- 类型转换：把后端的 `items/page/page_size/total` 转成前端类型。

### `list-query.ts` 要点

纯函数 + 一个 `useListQuery()` hook：

- 状态：`{ view: "ledger" | "cards", page, pageSize, search, status, orderBy }`
- `view` 持久化到 `localStorage`（key 用 `yang.feishu.datasource.view`，
  **try/catch 包裹**，参考 `shell/density.ts` 的写法）
- **默认 `view: "ledger"`**
- **默认 `orderBy: [{field:"source_key",direction:"Asc"}]`**
- 搜索/筛选/`pageSize` 变化 → `page` 归 1
- 导出纯函数供单测：`nextStateOnSearch`、`nextStateOnFilter`、`nextStateOnPage` 等，
  或一个 reducer。**必须是可单测的纯逻辑**，不要全塞在组件里。

### 四个情境化指引落点（设计 §5.5，不设常驻横幅）

1. **空态**：四步就是页面正文，不渲染空栅格/空表，工具栏也不渲染
2. **创建成功回执**：带**真实 `source_key`** 的「粘回飞书审批后台」+ 复制按钮
3. **详情 0 选项**：指向多维表格自动化，不写「暂无数据」
4. **详情 403**：缺 `option.read` 时的说明 + 重试，**不是**白屏也不是空列表

另外「搜索无结果」要与「一个数据源都没有」区分开，给「清除筛选」动作。

---

## 5. 测试（`frontend/tests/features/feishu/`）

Vitest + `@testing-library/react`。至少覆盖：

- `api.ts`：query key 形状；缺 operation_id 时抛错
- `list-query.ts`：**搜索/筛选后 page 归 1**；默认 orderBy 是 source_key Asc；
  view 持久化的读写在 localStorage 抛错时不崩
- **权限门控**：无写权限时「添加数据源」与列表项操作菜单**不渲染**（不是禁用）
- **渲染分支**：空 / 搜索无结果 / 详情 0 选项 / 详情 403 / 创建后预检失败
- **必须断言的查询行为**：每次列表请求都带非空 `order_by`

---

## 6. 不要做的事

- 不改 `engine/`、不改 `shared/`、不改 `features/` 其它域
- 不画后端给不出的字段：**选项数量**、**数据源的最后同步时间**
  （`list_datasources` 不返回；详情页的「最近推送」来自 `list_options.updated_at`，可以画）
- 不在指引里写接口路径与字段名，不放 Token 明文
- 详情页**只读**：选项区不得有任何增删改入口，且要显式说明「选项由多维表格推送」
- 不用 `window.confirm`（历史遗留，不是范式）
