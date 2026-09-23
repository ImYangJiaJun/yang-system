# 飞书数据源控制台 · 原型保真契约

这份文件是 4 个原型**共同的底座**。所有 token、组件配方、文案、图标都从这里取，
不允许各原型自行发明。差异只允许出现在**信息架构**上。

---

## 0. 交付边界

- **单文件 HTML**：内联 `<style>` + 内联 `<script>`，双击即可打开。
- **零外部依赖**：不引 CDN、不引字体、不引图标库。图标一律内联 SVG。
- **必须在 `file://` 下工作**：不能用 `fetch`、ES module import、`localStorage` 之外的花活。
  状态放内存变量即可（`localStorage` 可用但要 try/catch）。
- `<!doctype html>` 起手，`<html lang="zh-CN">`，`<title>` 用中文产品名。
- 必须响应式：≥1024 正常，~400px 宽不出现横向滚动（表格可局部 `overflow-x:auto`）。
- 键盘焦点可见，`prefers-reduced-motion` 生效。

---

## 1. 色彩 token（逐字抄自 `frontend/src/index.css`，不要改数值）

应用用 Tailwind v4 + oklch。原型里**照抄这套变量名**，组件只引用变量，不写死颜色。

```css
:root {
  --radius: 0.625rem;              /* = 10px，圆角基准 */
  --background: oklch(1 0 0);
  --foreground: oklch(0.145 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0.145 0 0);
  --popover: oklch(1 0 0);
  --popover-foreground: oklch(0.145 0 0);
  --primary: oklch(0.205 0 0);
  --primary-foreground: oklch(0.985 0 0);
  --secondary: oklch(0.97 0 0);
  --secondary-foreground: oklch(0.205 0 0);
  --muted: oklch(0.97 0 0);
  --muted-foreground: oklch(0.556 0 0);
  --accent: oklch(0.97 0 0);
  --accent-foreground: oklch(0.205 0 0);
  --destructive: oklch(0.577 0.245 27.325);
  --border: oklch(0.922 0 0);
  --input: oklch(0.922 0 0);
  --ring: oklch(0.708 0 0);
  --sidebar: oklch(0.985 0 0);
  --sidebar-foreground: oklch(0.145 0 0);
  --sidebar-accent: oklch(0.97 0 0);
  --sidebar-border: oklch(0.922 0 0);
  --density-cell-y: 0.625rem;
}
/* 暗色：应用是在 <html> 上挂 .dark 类，由页头按钮切换 */
.dark {
  --background: oklch(0.145 0 0);
  --foreground: oklch(0.985 0 0);
  --card: oklch(0.205 0 0);
  --card-foreground: oklch(0.985 0 0);
  --popover: oklch(0.205 0 0);
  --popover-foreground: oklch(0.985 0 0);
  --primary: oklch(0.922 0 0);
  --primary-foreground: oklch(0.205 0 0);
  --secondary: oklch(0.269 0 0);
  --secondary-foreground: oklch(0.985 0 0);
  --muted: oklch(0.269 0 0);
  --muted-foreground: oklch(0.708 0 0);
  --accent: oklch(0.269 0 0);
  --accent-foreground: oklch(0.985 0 0);
  --destructive: oklch(0.704 0.191 22.216);
  --border: oklch(1 0 0 / 10%);
  --input: oklch(1 0 0 / 15%);
  --ring: oklch(0.556 0 0);
}
html[data-density="compact"] { --density-cell-y: 0.25rem; }
html[data-density="loose"]   { --density-cell-y: 1rem; }
```

**同时要响应系统主题**：`@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { …同一组暗色值… } }`。
再加 `:root[data-theme="dark"] { …暗色… }` 让显式切换在两个方向都赢。
`body` 必须显式 `background: var(--background)`。

### 语义色 —— 本次要新增的 token（重要）

引擎的 `CellPresentation.tone` 词汇是 `neutral | info | positive | warning | negative`
（`engine/renderers/table/business-cell-model.ts:9`），但：

- `shared/ui/badge.tsx` 只有 `default / secondary / destructive / outline`，**没有语义 variant**；
- 现有 palette 是**全消色**的（每个 token 都是 `oklch(L 0 0)`），**根本没有绿色**。

而 `frontend/AGENTS.md` 禁止手写颜色、禁止内联色值。所以**正确做法是在 `index.css`
的 `:root` 与 `.dark` 两块里同时新增语义 token**，组件只引用 token。

原型里请把这套新增 token 明确写进 `:root` 和 `.dark`，让评审看到"要动设计系统什么"：

```css
/* 新增：语义状态色（引擎 tone 词汇的前端投影） */
:root {
  --tone-positive:  oklch(0.55 0.09 155);   /* 启用 / 已加密 */
  --tone-warning:   oklch(0.62 0.11 75);    /* 已停用 / 需注意 */
  --tone-info:      oklch(0.55 0.08 245);   /* 中性说明 / 关系 */
}
.dark {
  --tone-positive:  oklch(0.72 0.11 155);
  --tone-warning:   oklch(0.78 0.12 75);
  --tone-info:      oklch(0.72 0.09 245);
}
```

彩度刻意压低在 0.08–0.12：这套界面其余部分完全消色，高饱和会立刻显得是外来的。

| tone | 含义 | 色源 |
|---|---|---|
| `positive` | 启用 / 已加密 | `--tone-positive` |
| `warning` | 已停用 / 待处理 | `--tone-warning` |
| `info` | 中性信息 / 关系 | `--tone-info` |
| `negative` | 失败 / 危险 | `--destructive`（已有） |
| `neutral` | 缺省 | `--muted-foreground`（已有） |

徽标**底色**用 `color-mix(in oklab, var(--tone-*) 12%, transparent)`，暗色下 20%；
**字号用色本身**，不用白字。落地时等价写法是 Tailwind 的 `bg-tone-positive/10 text-tone-positive`
（需在 `@theme inline` 里补 `--color-tone-positive` 映射）。

**不要**用纯色填充 + 白字——那是 Badge 的 `default` variant，语义是"主要"而不是"状态"。

> 备选方案（不新增 token）：既然整个 palette 消色，也可以**只用形态**编码状态——
> 实心点 ● = 启用、空心点 ○ = 已停用，再配文字。这四个原型统一采用**新增 token** 的版本
> 以便横向比较；备选方案在评审结论里单列。

---

## 2. 字体与字号

**不加载任何网络字体**——应用没有配，靠 Tailwind 默认系统栈。照抄：

```css
font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto,
  "Helvetica Neue", Arial, "Noto Sans SC", "PingFang SC", "Microsoft YaHei", sans-serif;
```

等宽（`source_key`、Token、`option_id`、恢复码这类技术标识）用：

```css
font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
```

字号阶梯（Tailwind 名字 → 用途）：

| 类 | px | 用途 |
|---|---|---|
| `text-xl` + `font-semibold` | 20 | 页面主标题（`<h1>`） |
| `text-lg` + `font-semibold` | 18 | 对话框标题 |
| `text-base` + `font-medium` | 16 | 分节标题（`<h2>`） |
| `text-sm` | 14 | 正文、按钮、导航、表单控件 |
| `text-xs` | 12 | 元信息、状态徽标、导航分组标签 |

行高：正文 `1.5`，标题 `1.25`。
数字列（`sort_order`、计数、页码）加 `font-variant-numeric: tabular-nums`。
**不要**用全大写、不要给中文加 `letter-spacing`、不要加"01/02/03"式编号（除非内容真的是有序流程）。

---

## 3. 尺寸与间距

- 圆角：`--radius` 10px 为基准。`sm`=6px、`md`=8px、`lg`=10px、`xl`=14px。
  **不要一个圆角用到底**：卡片/分节用 `xl`，按钮/输入框用 `md`，徽标用 `md`，小标记用 `sm`。
- 阴影：应用只用 Tailwind 的 `shadow-xs`（按钮/输入）与 `shadow-lg`（对话框/浮层）。
  **卡片不要加阴影**——靠 `1px` 边框分界。这是这套设计系统的关键克制点。
- 侧边栏宽 `240px`（`w-60`），固定不滚动。
- 主内容区：`padding: 24px`（`p-6`），页面容器 `max-width` 由各原型自定。
- 间距用 flex/grid 的 `gap`，不要用逐元素 margin 拼接。
- 页面最小侧边留白 16px。

---

## 4. 组件配方（照抄，这是"像不像这个应用"的关键）

### 4.1 应用外壳（逐字抄自 `shell/AppLayout.tsx`）

```
根容器   flex, height 100svh, overflow hidden, bg=--background, color=--foreground
├─ aside  宽 240px, flex 列, 右边框 --border
│  ├─ 品牌区  padding 12px 16px, 底边框；logo 40×40 + 文字 text-sm font-semibold
│  ├─ 身份切换器  padding 8px, 底边框；按钮 flex gap 8px, radius md, padding 6px 8px, 悬停 bg --accent
│  ├─ nav  flex:1, overflow-y auto, padding 12px, 分组间距 16px
│  │  └─ 分组标签  padding 0 8px 4px, text-xs font-medium color --muted-foreground
│  │     条目      flex gap 8px, radius md, padding 6px 8px, text-sm
│  │              悬停 bg --accent；激活 bg --accent + font-medium
│  │              图标 16×16, flex-shrink 0
│  └─ 底栏  padding 12px, 上边框；"退出登录" ghost 按钮占满宽, 左对齐, gap 8px
└─ 右侧列  flex:1, min-width 0, flex 列
   ├─ 页头  flex, justify-end, gap 8px, padding 8px 16px, 底边框
   │        「密度」outline 小按钮 + 明暗切换 icon 按钮（36×36）
   └─ main  flex:1, min-width 0, overflow-y auto
```

侧边栏导航条目（`NavLink`）的类串，逐字：

```
flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors
hover:bg-accent hover:text-accent-foreground
激活追加：bg-accent font-medium text-accent-foreground
```

### 4.2 页面骨架（抄自 `features/account/AccountSettingsPage.tsx`）

```html
<main class="mx-auto w-full max-w-2xl space-y-6 p-6">
  <div class="space-y-1">
    <h1 class="text-xl font-semibold">页面标题</h1>
    <p class="text-sm text-muted-foreground">一句话说明这个页面能做什么</p>
  </div>
  …
</main>
```

### 4.3 分节卡片

```html
<section class="rounded-xl border border-border bg-card p-5">
  <h2 class="text-base font-medium">分节标题</h2>
  <p class="mt-1 text-sm text-muted-foreground">补充说明（可省）</p>
  <div class="mt-3 …">内容</div>
</section>
```

### 4.4 按钮（`shared/ui/button.tsx` CVA，逐字）

基类：
```
inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm
font-medium transition-all disabled:pointer-events-none disabled:opacity-50
focus-visible:outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50
```
变体：
- `default`  `bg-primary text-primary-foreground shadow-xs` 悬停 `bg-primary/90`
- `outline`  `border bg-background shadow-xs` 悬停 `bg-accent text-accent-foreground`
- `secondary` `bg-secondary text-secondary-foreground shadow-xs`
- `ghost`    `hover:bg-accent hover:text-accent-foreground`
- `destructive` `bg-destructive text-white shadow-xs` 悬停 `bg-destructive/90`
- `link`     `text-primary underline-offset-4 hover:underline`

尺寸：`default` 高 36px / 横向 16px；`sm` 高 32px / 横向 12px；`icon` 36×36。

### 4.5 状态徽标

`shared/ui/badge.tsx` 只有 `default / secondary / destructive / outline` 四个 variant，
**没有语义色 variant**。原型按第 1 节的 tone 表自己拼，但保持 Badge 的形状：

```
display:inline-flex; align-items:center; justify-content:center; gap:4px;
border-radius:8px; border:1px solid; padding:2px 8px;
font-size:12px; font-weight:500; white-space:nowrap; width:fit-content;
```

### 4.6 表单控件

- 输入框：高 36px，`border:1px solid var(--input)`，`border-radius:8px`，横向内边距 12px，
  字号 14px，`background: transparent`。聚焦：`border-color: var(--ring)` + `box-shadow: 0 0 0 3px color-mix(in oklab, var(--ring) 50%, transparent)`。
- 字段包裹：`<div class="space-y-1.5">` 即 `display:grid; gap:6px`。
- 标签：`font-size:14px; font-weight:500; line-height:1`。
- 必填不标星号，改为把要求写进标签文字，如「数据源标识（1–64 字符）」。
- 校验错误：控件下方 `text-sm` `color: var(--destructive)`，并给控件加 `aria-invalid="true"`。

### 4.7 对话框（`shared/ui/dialog.tsx`，逐字）

- 遮罩：`position:fixed; inset:0; z-index:50; background:rgb(0 0 0 / 0.5)`。
- 内容：宽 `100%`、`max-width: 512px`（`sm:max-w-lg`），居中（`top/left 50%` + `translate(-50%,-50%)`），
  `background: var(--background)`，`border:1px solid var(--border)`，`border-radius:10px`，`padding:24px`，
  `display:grid; gap:16px`，`box-shadow` 大阴影（`0 10px 15px -3px rgb(0 0 0/.1), 0 4px 6px -4px rgb(0 0 0/.1)`）。
- 右上角关闭按钮：绝对定位 `top:16px right:16px`，16×16 的 X 图标，`opacity:.7`。
- 标题 `text-lg font-semibold`；描述 `text-sm text-muted-foreground`。
- 底部按钮区：`display:flex; gap:8px; justify-content:flex-end`，主按钮在右。
- 打开动效：`opacity 0→1` + `scale .95→1`，200ms。
  尊重 `prefers-reduced-motion: reduce`（去掉动效）。

### 4.8 提示条 / 横幅

| 用途 | 配方 |
|---|---|
| 错误 | `border:1px solid color-mix(in oklab, var(--destructive) 40%, transparent); background: color-mix(in oklab, var(--destructive) 10%, transparent); padding:8px 12px; border-radius:8px; color: var(--destructive); font-size:14px` + `role="alert"` |
| 中性说明 | `border:1px solid var(--border); background: color-mix(in oklab, var(--muted) 50%, transparent); padding:8px 12px; border-radius:8px; font-size:14px` + `aria-live="polite"` |
| 危险区 | `border:1px solid color-mix(in oklab, var(--destructive) 40%, transparent); background: color-mix(in oklab, var(--destructive) 5%, transparent); padding:20px; border-radius:14px`，标题 `color: var(--destructive)` |

### 4.9 列表行

```
display:flex; align-items:center; justify-content:space-between; gap:12px;
border:1px solid var(--border); border-radius:8px; padding:8px 12px; font-size:14px
```
次级行：`font-size:12px; color:var(--muted-foreground)`，多段用 ` · ` 连接（应用中点，两侧各一空格）。

### 4.10 加载 / 空 / 错误

- 加载：骨架块 `background: var(--muted); border-radius:6px; animation: pulse 2s infinite`，
  页面级加载是 `padding:32px` 下两三块骨架（宽 256 / 384 / 满宽）。
- 空：**仓库目前没有 `EmptyState` 组件**——原型要给出它的形态，作为本次新增件。
  它必须包含：一件事的说明 + 一个明确动作，不是一句"暂无数据"。
- 错误：见 4.8 错误条 + 一个「重试」outline 按钮。

---

## 5. 图标

只用 lucide 风格（24×24 viewBox，`fill:none`、`stroke:currentColor`、`stroke-width:2`、
`stroke-linecap:round`、`stroke-linejoin:round`），默认 16×16，导航/按钮内 16，
元信息内 14，页头按钮 16。

原型里用 `<svg class="i" viewBox="0 0 24 24" aria-hidden="true">…</svg>`，
`.i { width:16px; height:16px; flex-shrink:0; fill:none; stroke:currentColor; stroke-width:2; stroke-linecap:round; stroke-linejoin:round; }`

**允许使用的图标（需要用到的自行内联路径，不要发明新图标）：**

`plus` `minus` `x` `check` `chevron-down` `chevron-right` `chevron-left` `chevrons-up-down`
`arrow-left` `arrow-right` `search` `more-horizontal` `refresh-cw` `external-link` `copy`
`moon` `sun` `circle-user` `log-out` `users` `building-2` `shield-check` `puzzle` `table-2`
`list` `database` `lock` `key-round` `power` `trash-2` `square-pen` `triangle-alert` `info`
`circle-check` `circle-alert` `circle-help` `sliders-horizontal` `filter` `clock` `send`

---

## 6. 内容（用真数据，不要 lorem，不要占位符）

### 6.1 数据源样例（编造但必须像真的）

| `source_key` | `title` | `status` | `encrypt_enabled` | `default_locale` |
|---|---|---|---|---|
| `expense-category` | 报销事由分类 | active | true | `zh-CN` |
| `leave-type` | 请假类型 | active | false | `zh-CN` |
| `purchase-dept` | 采购归口部门 | active | true | `zh-CN` |
| `vendor-list` | 供应商名录 | disabled | false | `zh-CN` |
| `it-asset-class` | IT 资产分类 | active | false | `en-US` |

再补 2–3 条让列表有滚动感：`travel-city`（差旅城市）、`contract-type`（合同类型，disabled）、`cost-center`（成本中心）。

### 6.2 选项数据样例（`list_options` 的返回形状）

字段：`option_id` `source_key` `label` `i18n` `sort_order` `is_default` `enabled`

以 `expense-category` 为例：
差旅费 / 办公用品 / 业务招待 / 培训费 / 通讯费 / 房租水电 / 咨询服务费 / 其他

`is_default` 为真的那一条加「默认」标记；`enabled=false` 的行整行降饱和并加「已停用」。

### 6.3 四步指引（文档 §5.5，逐字沿用其语义，不要改写成别的流程）

1. **在飞书审批后台配置控件** —— 单选/多选控件选「使用外部选项」，自定义一个 Token 并记住它（服务端只存摘要，之后无法回显）。
2. **在这里建一个数据源** —— 数据源标识会进接口 URL；粘贴上一步的 Token。
3. **把接口地址与 Token 填回审批后台** —— 点「校验数据」确认能拉到选项。
4. **在多维表格配自动化推送选项** —— HTTP 节点带 `Authorization: Bearer <管理 Token>` 调写入接口，选项随表格变动自动更新。

**硬约束：指引里绝不出现 Token 明文**（服务端只存 SHA-256 摘要，永远回不出明文）。
指引只写"去哪做、填什么"，不写具体接口路径与字段名。

### 6.4 已确认的后端文案（必须逐字复用，不要另写一遍）

删除确认（来自 `datasource/mod.rs` 的 `ActionConfirmation`）：

> **标题**：删除数据源
> **正文**：删除后其下全部选项会被同时停用，且不可恢复。确认删除？

### 6.5 后端错误文案（原型里错误态要用真的）

- `数据源不存在`
- `没有要更新的字段`
- `Token 不能为空；不轮换请省略该字段`
- `title 长度必须在 1..=100`
- `status 只接受 "active" 或 "disabled"`

### 6.6 界面用语

用使用者认得的词，不用系统内部的词：

| 不要写 | 要写 |
|---|---|
| `source_key` | 数据源标识 |
| `token` | 接口 Token |
| `encrypt_enabled` | 加密存储 |
| `default_locale` | 默认语言 |
| `status=disabled` | 已停用 |
| `management token` | 管理 Token |

按钮用主动语态且说清后果：「添加数据源」「重命名」「停用」「启用」「删除」。
提交中的态用「提交中…」，成功后提示与按钮同名，如「已停用」。

---

### 6.6.1 后端**给不出**的东西 —— 原型里绝对不许画

这一条比"该画什么"更重要。原型里出现一个后端拿不到的字段，评审就会照着它选方案。

| 不要画 | 原因 |
|---|---|
| 卡片/行上的**选项数量**（"8 个选项"） | `list_datasources` 只返回 `source_key/title/encrypt_enabled/default_locale/status`，**没有选项计数**。要显示它得新增后端字段或逐源调 `list_options`。 |
| **最后同步时间** / **同步状态** | 没有任何 Action 返回它。 |
| **Token 明文**（哪怕是"查看"按钮） | `token_hash` 只存 SHA-256 摘要，单向不可逆，服务端**永远回不出明文**。 |
| **接口地址**（`/api/v1/feishu/approval/options/xxx`） | 指引里只能写"去哪做、填什么"，不写具体路径（文档风险表明确要求）。 |
| 数据源上的**编辑选项**入口 | 控制台对选项**只读**是架构决策，不是没做。详情页必须显式说明"选项由多维表格推送"。 |

反过来说：**如果某个原型看起来必须要有这些字段才好用，那它其实是在要求后端改契约**——
这本身是有价值的信号，但必须在原型里标注出来，不能默默画上。

### 6.7 详情页里 `i18n` 字段怎么显示

`list_options` 的 `i18n` 是 **JSON 文本原样返回**。直接铺在单元格里会是一长串
`{"zh-CN":"差旅费","en-US":"Travel"}`，把表格撑坏。原型里请：

- 有值：显示一个 `i18n` 小徽标（tonal `info`），悬停/展开才看到内容；
- 无值：显示 `—`。

同理 `enabled` / `is_default` 是 boolean，**不要**显示成"是/否"纯文本——
用 tone 徽标（启用=positive、已停用=warning）和一个小「默认」标记。

### 6.8 密度

行高一律由 `--density-cell-y` 驱动（`padding-block: var(--density-cell-y)`），
**不要写死**。方案 B 的页头「密度」菜单要真的能切换 `html[data-density]` 并让行高当场变化——
这是那个方案的核心卖点，必须能演示。

---

## 7. 必须演示的状态（每个原型都要有，缺一个就会让人误判方案）

1. **空状态** —— 一个数据源都没有。指引展开，主行动明确。
2. **有数据** —— 上表 5–8 条，含至少一条 `disabled`。
3. **详情** —— 某个数据源的选项列表（只读）。
4. **填写对话框** —— 新增数据源的表单（含 Token 字段，必须是密码型）。
5. **删除确认** —— 用 §6.4 的原文，且要说清"会连带停用其下选项"。
6. **加载中** —— 骨架屏。
7. **错误** —— 用 §6.5 的真实文案 + 重试。
8. **两种角色** —— 见 §8。
9. **明暗两色** —— 页头可切换。

原型内部用一个小状态切换器（不要做成产品的一部分，标注成"原型控制"）来演示 1/2/6/7，
其余（3/4/5）用真实的点击进入。

---

## 8. 两种角色（本次设计的核心约束）

使用者是**两类人**：运维/集成负责人（建、改、删数据源）与业务/审批管理员（只看）。

**权限怎么来（已核实）**：前端持有的 access token 是 JWT，其自定义声明里有
`permissions: string[]` —— 这是唯一现成机制。三个权限：
`feishu.datasource.read`、`feishu.datasource.write`、`feishu.option.read`。

> 注意：`catalog` 里的 `availability` 是**声明期静态提示**（DSL 里写死的），
> 后端测试明确它"不能替代服务端授权"。**不要**用它做角色判断。

原型里请在页头或侧边栏底部放一个"当前身份"切换，并在两态下都截图级地展示：
- **运维态**：能看到「添加数据源」、卡片/行上的重命名/停用/删除。
- **业务态**：这些入口**整个不出现**（不是禁用）。

禁用 vs 隐藏要有理由：业务态对写操作的隐藏是"这个入口不属于你"，
所以隐藏；而"已停用数据源上不能再停用一次"这种才是禁用 + 理由 tooltip。

---

## 9. 明确不要做的事

- 不加阴影到卡片、不用渐变色、不用玻璃拟态。
- 不用"AI 生成感"的默认套路：奶油底 + 衬线大标题 + 陶土色点缀；近黑底 + 单一荧光色；
  报纸式细线密栏；每个块都同圆角同阴影；`01/02/03` 编号；全大写小标签；
  标题里只把某一个词变斜体/变色；每个标题上方加一行 tracked-out 的小字眉标。
- 不为了好看而增加控件。少即是多。
- 不要把"指引"做成又一个需要读的大段文字——它要能扫。
- 不写"暂无数据"这种空话。
