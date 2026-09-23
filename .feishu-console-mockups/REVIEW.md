# 飞书数据源控制台 · 设计复核（第一性原理）

对象：`docs/architecture/feishu-datasource-console.md`
方法：对前端源码、后端 `src/addon/feishu/**`、`crates/yang-base` 逐条核实，再反推设计。
结论带锚点；凡是我核实后推翻了自己的地方，单列在第三节。

---

## 一、这个控制台到底在管什么

先不看文档的四个目标，只看东西是怎么流过去的（全部有锚点）：

```
多维表格自动化（持服务端配置的全局管理 Token）
        │  upsert_options / delete_options
        ▼
   feishu_option 行 ─────────────────┐
                                     │ 按 source_key 取
飞书审批控件「使用外部选项」           │
        │  approval_options（带用户在飞书后台自定义的那颗 Token）
        └────────────────────────────┘
```

控制台在这条链上**只能碰两个点**：

1. `source_key ↔ token_hash` 这对凭据的创建与轮换 —— `create_datasource.rs:100` 只写哈希，
   该列 `secret(true)`（`datasource/table.rs:33-39`）
2. `status` 这个断路开关 —— 非 active 时 `approval_options` 直接回 40301
   （`approval_options.rs:96-101`）

它**碰不到**的：选项的真相（多维表格是事实源）、Token 明文（只存 SHA-256）、
管理 Token 与加密密钥（服务端配置，`config/mod.rs:568-582`）。

由此推出四件事，文档摸到了其中一部分但没说自己从哪推的：

**1. 它管的不是「很多数据源」，是一对凭据加一个断路开关。**
所以首页的重心是**状态可见性**，不是 CRUD 密度。日常动作是核对，不是增删。

**2. 唯一真正难的事是首次配置。**
四个接缝（飞书控件 → 我们建档 → 回填后台 → 多维表格推送）、
**三个**凭证概念（控件 Token、管理 Token、加密密钥）、两个方向，而用户只看得见其中一个。
文档 §1 的四个目标里，只有「辅助配置指引」在解决真问题——其余三个都在解决展示问题。

**3. 凭据事后无法校验，唯一的验证时机是创建那一刻。**
`approval_options.rs:76-101` 的 `verify_source` 需要**明文** Token 才能比对存储的哈希；
而服务端永远回不出明文。所以「从卡片上点一下，校验这个数据源还有效吗」——
**结构上不可能**。但用户在创建/轮换的那一刻手里正握着明文，那是全系统唯一能一次验完
「数据源在不在 + Token 对不对 + 有没有被停用」的时刻（对应 40401 / 40102 / 40301
三个可归因码，`approval_options.rs:46-62`）。

**4. 「链路通没通」今天几乎无法回答——差一行代码就能回答。**
`feishu_option.updated_at` **已经在表里**（`option/table.rs:59`），框架每次 UPDATE 自动刷新
（`table_query/sql_render.rs:474-492`），而只有持管理 Token 的多维表格自动化会写选项行。
所以它就是「这行选项最后一次被推送的时间」。**但 `list_options` 没有 select 它**
（`list_options.rs:35-43`）。`feishu_datasource.updated_at` 同理（`datasource/table.rs:55`
有，`list_datasources.rs:55-61` 没 select）。
这是整个文档**漏掉的最高杠杆改动**：加一个字段，「推送还活着吗」就从猜测变成事实。

---

## 二、文档站得住的决定

| 决定 | 判断 | 补充 |
|---|---|---|
| 路线 B 而非 A（§4.1） | ✅ 对 | **但理由给弱了。** 文档说省首屏预算；最强的理由它没写：`registry.ts:9-13` 的 props 契约只有三个字段，**没有 URL、没有 catalog**，「点卡片看它的选项」在这条路上根本无法表达。而且路线 A 只能由通用表格上的一个 Action 触发——想删掉表格，却必须先看到表格再点进去。 |
| 删 `presentation()` + `view()` 以隐藏导航（§5.3-1） | ✅ 对 | 机制见第三节第 1 条。但两者**必须成对删**，半吊子改法各有一个坏后果（见下）。 |
| 控制台对选项只读（§2） | ✅ 对，且是架构决策 | 不是「没做」，是第二个可写入口会让「这条选项是谁写的」不可审计。详情页必须显式说明，否则评审会以为是缺功能。 |
| 记录级详情路由 vs 查询参数（§5.2 / §8） | ⚠️ 待定 | 仓库带参路由先例是零。但 `WorkbenchPage` 已经有左右分栏的现成结构（`flex h-full` + 左栏 `w-72`），主从分栏不是全新范式。 |

### 半吊子改法的后果（文档 §7 风险表第 2 行没写清）

- **只删 `view()`**：`compile.rs:157-183` 会给 `module.views` 为空的模块**自动合成**
  一个 `{module}.default` 视图，含全表列，只因 `data_action` 为 `None` 才被
  `registry.rs:177-187` 丢掉——今天是惰性的非契约行为，将来给模块加一个可用作数据源的
  primary action，整张表就会静默复现。
- **只删 `presentation()`**：模块离开 `catalog.modules`，但 view 仍在 `catalog.table_views`，
  `navigation.ts:21-38` 的 `unassignedViews` + `syntheticPageForView` 会把表格
  **挂到「工作台」分组下重新出现**。
- 风险表给的兜底「保留 presentation 但设法隐藏导航」**没有声明面可用**——
  没有「隐藏某模块」的接口，实际等于让人去改前端特判。

---

## 三、我核实后修正的（含我自己先前判断错的）

**1. 我先前说错了：删 `presentation()` 确实会移除导航条目。**
我读 `registry.rs:241-271` 的 `primary_action / allowed_actions / views` 三元判断后断言
模块仍会进目录。**漏了上游**：`compile.rs:342-345`

```rust
let Some(presentation) = &module.presentation else {
    continue;              // 无 presentation 的模块根本不进 RuntimeModule
};
```

没有 presentation 的模块在这一步就被整个跳过，永远到不了我读的那段。**文档 §5.3-1 是对的。**

**2. 角色判断走 `catalog.actions`，不需要解析 JWT。**
UI 目录**本身已按身份投影**：`registry.rs:245,256` 用 `policy.allows(context)` 过滤
（`PolicyMiddleware::allows = authorize(ctx).is_ok()`，`router/middleware.rs:77-79`）。
所以「当前身份有没有写权限」= `catalog.actions` 里有没有 `feishu.datasource.create_datasource`。
我先前说 JWT 的 `permissions` 是「唯一现成机制」——不对，而且前端**根本没有 JWT 解码代码**，
claim 还嵌在 `access.permissions` 而不是顶层。用 catalog 更省事，且与既有机制一致。

**3. `availability` 不是权限信号。**
`ActionPresentationSchema.availability` 是**声明期静态提示**（DSL 里写死的），
后端测试原话：「availability disabled 不能替代服务端授权或阻断真实派发」
（`ui/__tests__/table_view_test.rs:334`）。文档没提它，但谁拿它做角色判断都会写错。

**4. `default_locale` 的取值域是 `zh_cn / en_us / ja_jp`。**（`domain/protocol.rs:50-52`、
`domain/i18n.rs:13`、`datasource/table.rs:45`）
文档 §5.6 要在卡片上展示它。**这是 blocker**：后端对它**零校验**
（`create_datasource.rs:69-81` 只校验 source_key / title / token），而 `i18n.rs:59-72`
把它**原样当作回给飞书的 i18nResources 的 locale 键**。前端写成 `zh-CN`，
会让这个数据源在飞书侧所有语言下都取不到文案（控件显示为空），
而控制台没有任何地方能看出这是自己造成的。

**5. `source_key` 只接受「小写字母开头、`[a-z0-9_]`、≤64 字节」。**（`create_datasource.rs:45-52`）
**连字符非法**——文档与我的初稿里 `expense-category` 这类样例数据本身就违规。
错误原文是 `数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]`。

**6. 「加密存储」的真名是「加密返回」。**（`datasource/table.rs:42`）
它加密的是**返回给飞书的选项信封**，需要服务端配 `feishu.encryption_key`；
跟 Token 存储**毫无关系**（Token 无论如何只存 SHA-256）。
写成「加密存储」会教给评审一个与安全相关的错误结论。
而且它的失败模式在控制台内**不可归因**：开了但没配密钥的数据源，
在卡片上是一片健康色，实际每次被调用都返回 50002（`approval_options.rs:57-58`）。
**它不能长得像健康状态**，也不能与「启用」共用同一个 positive 语义色。

**7. 文档 §5.3-2 的 schemars title 对可选字段不生效，且失败是静默的。**
`json-schema.ts:50-59` 的 `effectiveSchema` 是**替换**而非合并：

```js
const branches = resolved.anyOf ?? resolved.oneOf;
const nonNull = branches.find((b) => b.type !== "null");
return nonNull ? effectiveSchema(root, nonNull) : resolved;   // 丢掉 resolved 自己的 title
```

schemars 给 `Option<String>` 生成 `{"anyOf":[{"type":"string"}],"title":"默认语言"}`，
经它之后只剩分支，**外层 title 被丢弃**，`SchemaField.tsx:58` 的 `resolved.title` 拿到
`undefined`，回落到英文字段名。而 `encrypt_enabled`、`default_locale` 恰恰都是可选的。
（必填字段不受影响。）**文档的风险表说这条「需验证」，而结论是：对可选字段不成立。**

**8. §5.3 的改动 2、3 与 §5.1 自相矛盾。**
§5.1 要建手写的 `DatasourceFormDialog.tsx`。表单一旦手写，`<Label>` 和
`type="password"` 就是自己写的一行——「后端补 schemars title」「token 标密码型」
**全是白做的工**（它们只影响通用 `JsonSchemaForm` 路径）。
反过来若要 schemars title，就该复用通用表单、不需要手写对话框。**必须二选一。**
建议手写（路线 B 的定义就是前端全权自建，先例 `AccountSettingsPage.tsx` 每个表单都是手写的），
那 §5.3 改动 2、3 直接删掉。

**9. §5.5 第 3 步「点『校验数据』确认能拉到选项」控制台做不到。**
唯一能校验的是 `approval_options`，它要按数据源 Token 自校验，而服务端回不出明文
（见第一节第 3 条）。**但同一件事在创建对话框里完全做得到**——文档把它放到了唯一做不到的位置。
改成表单里一个可选的「验证凭据」（用刚输入的明文调一次，回显真实错误码），
就把最站不住的一句承诺变成最有用的一步。

**10. `status` 没声明 `.filterable(true)`。**（`datasource/table.rs:46-51`）
框架对筛选是 fail-closed（`table_query/validation.rs:135-143`，直接 `FieldPermissionDenied`）。
所以**「全部/启用/停用」这个筛选器服务端做不到**。
另外 `title` 只声明了 `.searchable(true)`（能搜、**不能筛、不能排**），
全表只有 `source_key` 是 `.sortable(true)` 的。
不对称：**详情页的选项列表可以按 `enabled` 筛**（`option/table.rs:53-57` 声明了 filterable）。

**11. `list_datasources` 没有默认排序兜底。**（`list_datasources.rs:74-76` 只遍历传入的 `order_by`；
对比 `list_options.rs:62-72` 有回退）
前端不发 `order_by` 就是**无序分页，翻页会重复/漏行**。

**12. `features/account/api.ts` 是全仓最深路径 import 的违规样本。**（`api.ts:3-7` 有 4 处
`@/engine/xxx` 深路径），违反 `frontend/AGENTS.md` 强制规则 2。
把 account 称作「同构先例」等于把这笔债复制一份。
正确写法是 `api.ts` = `catalog.actions.find(operation_id)` + `invokeAction`（从 `@/engine` 出口 import）
+ query key 的薄封装——**不要手写第二个 HTTP 客户端**，否则 401/刷新/信封/错误映射各写一遍，
还会与后端 `route()` 声明的路径脱钩。

**13. §5.2 手写 NavLink 会失去 catalog 的按身份投影。**
`AppLayout.tsx:246-262` 的硬编码先例是**无条件渲染**的（因为人人都有账号），
照抄它会把「飞书数据源」显示给一个连 `feishu.datasource.read` 都没有的身份，
点进去整页 403。修法不是放弃路线 B，而是把渲染条件写成
`catalog.actions` 里有没有 `feishu.datasource.list_datasources`。

---

## 四、四个原型的取舍

评审验收后确认：四个原型实际是 **3 种列表形态 × 2 种首次体验**，不是四种并列的 IA。

| | 列表形态 | 首次体验 | 最强的地方 | 代价 |
|---|---|---|---|---|
| **A 卡片画廊**（文档原案） | 卡片栅格 | 顶部可折叠指引 | 每个数据源有身份感；空态用卡片语言是对的 | 5 个标量字段占 3–4 倍纵向空间，且**丢掉跨行比较**——而一个 3–20 条的同类注册表，存在的意义就是比较 |
| **B 紧凑目录** | 台账表格 | 同上 | 密度；状态一屏扫完；是唯一能承载「按标识排序」的形态 | 卡片那层「对象感」没了；**它的状态筛选当前服务端做不到**（第三节第 10 条） |
| **C 主从分栏** | 左列表 + 右详情 | 右栏放完整指引 | 扫描与钻取在一屏内，无路由跳转；`WorkbenchPage` 已有同构先例 | 引入第二种导航范式；窄屏要塌 |
| **D 向导优先** | 台账（同 B） | **四步装配线** | 唯一直接解决「四个接缝」这个真问题的方案 | 它不是第四种 IA，是 B + 首启流程层 |

### 卡片形态的最强反方论证

这张表能画的只有 5 个标量字段，卡片的信息量等于一行，却要花 3–4 倍纵向空间。
更硬的是它**把仅有的查询能力也丢了**：`source_key` 是这张表**唯一** filterable
且**唯一** sortable 的列，`title` 只能搜；卡片网格让唯一可排序的列变得不可排序，
而状态筛选服务端根本做不到。而且卡片没换来操作能力——通用 TableView 早已把
Toolbar-Form + Row-Form + Row-Invoke 连同后端声明的二次确认一起投影出来了
（`datasource/mod.rs:91-108`），卡片是把引擎已经会渲染的东西重写一遍。

**结论：卡片列表的来源是「它看起来像个数据源目录」。**
唯一站得住的例外是**空态**——那一刻确实只有「一个待创建的对象」这一件事。

### 指引不该是常驻横幅

文档 §5.5 自己写了「有数据时默认折叠」——设计者已经预设了它不被展开。
更符合第一性原理的做法是**让指引只出现在卡住的那一刻，并携带那一刻的数据**：

1. **空态**：四步就是页面正文（这是唯一会读它的时刻，也正是空态该给「一件事的说明 + 一个明确动作」的地方）
2. **创建/轮换成功的回执**：控制台在这个时刻**知道真实的 source_key**，
   能渲染出「要粘回审批后台的那一段」+ 复制按钮。静态横幅在结构上做不到这件事。
3. **详情页 0 选项** 与 **缺 `option.read` 的 403**：把下一步写在各自的位置
4. 剩下的知识（Token 只存摘要、加密返回需服务端配密钥）降级成两个字段的帮助文字

即：**常驻横幅归零，改成「情境化指引 + 字段级帮助文字」。**

---

## 五、推荐

**表格式注册表 + 一次创建时的凭据预检 + 一屏能回答「哪一段断了」的诊断页。**

理由：这个控制台管的不是数据，是一对凭据加一个断路开关；
真实用户是**创建那一刻握着明文的人**（他既要能改服务端配置、又要能进飞书审批后台与多维表格，
是集成/运维工程师，不是「审批管理员」——业务态更像是三个独立权限位的副产品）。

**取舍规则**：三个真实场景（第一次配 / 日常核对 / 出故障排查）里，
只有出故障排查有真实紧迫性；日常核对的美观诉求最先牺牲；空态整屏归「建档」。
**信息架构的正确判据是稳态任务（排查），不是首屏观感——卡片墙在这条判据下是唯一输家。**

具体取法：

1. **骨架**：`routes.tsx` 两条懒路由（保住 URL/前进后退/可分享）；`AppLayout` 一条 NavLink，
   渲染条件是 `catalog.actions` 含 `feishu.datasource.list_datasources`；
   `features/feishu/api.ts` 是 `invokeAction` 的薄封装，不手写 fetch。
2. **首页**：表格而非卡片网格。卡片只保留在空态。
3. **指引**：删掉常驻横幅，按第四节的四条情境化。
4. **详情**：不做完整只读列表，做**单数据源诊断页**——头部 + 一句「选项由多维表格推送」
   + 选项表的「最近写入时间」列 + 0 选项空态 + 403 态。打开频率低正是它该做诊断而非 CRUD 的理由。
5. **后端改动从三处改成五处**：
   ①`list_options` 与 `list_datasources` 补 `updated_at` select（这是最高杠杆的一处）；
   ②删 §5.3 的改动 2、3（与手写表单矛盾）；
   ③§5.3-1 保留，但明确接受「删除确认文案转由前端持有」；
   ④`status` 若要支持筛选，补 `.filterable(true)`；
   ⑤`default_locale` 在前端做成三选一。
6. **e2e 不扩演示后端**：风险面只有三类（该身份有哪些 Action 可见 / 5 个输入输出形状 /
   三个渲染分支），前两类 Vitest 可覆盖，第三类就是渲染分支本身。
   留成待决项会让它无限期悬空，而结论当场就能给。

---

## 六、本次原型交付说明

四个单文件 HTML 在 `.feishu-console-mockups/`，共同底座见 `CONTRACT.md`，
后端事实勘误见 `ERRATA.md`。每个都带一条虚线边框的「原型控制」条（标注为非产品界面），
可切 空 / 有数据 / 加载 / 错误、运维 / 业务 / 审计身份、明暗主题，
并内含一个「已知实现约束」折叠区，列出该方案踩到的后端限制。
