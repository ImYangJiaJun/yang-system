# 飞书外部数据源（xlsx 文件导入）— 设计

**日期**：2026-09-28（2026-09-21 初稿、2026-09-23 重写、2026-09-28 改为通用导入流程）
**状态**：**已实施**（2026-09-28 设计裁定并逐条核实，同日按本计划落地；后端两个端点、
解析层、配置、契约、前端向导与入口接线全部交付）
**范围**：`project/yang-system`（后端 + 前端）
**前置阅读**：`docs/architecture/feishu-option-ingest.md`、
`docs/architecture/feishu-option-ingest-controls.md`、
`docs/architecture/feishu-datasource-console.md`、
`docs/architecture/feishu-datasource-table-config.md`、
lib_yang 仓库的 `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`

> **修订说明（2026-09-28）**：本版把「银行网点专用的 xlsx 导入」改成**通用流程**——
> 上传文件 → 解析表头 → 用户自己勾列 → 配父级 → 导入，并**对标多维表格的配置向导**。
>
> 三条改变了架构的产品裁定：
>
> 1. **导入流程对标多维表格**：不再要求用户预先在数据源上填列名，而是从文件表头里选。
> 2. **上传不限文件条数**，只要表头一致就合并成一份快照。
> 3. **前端纳入范围**（09-23 版 §2 曾明确「不做前端」）——「由用户自己选字段」本质是 UI 动作。
>
> 同时修掉了 09-23 版被后续提交甩下的四处过期：`linkage_mapping` 列**已被整列删除**
> （§4.1）、`RawValue.parent_label` 已换成祖先链（§4.3）、§4.10 对事务获取方式的错误陈述、
> §9 对 `AGENTS.md` 的过期断言。历史结论见 §3.3。
>
> 文件名保留 `feishu-bank-branch-datasource.md` 不改（其它文档按此名引用），但**内容已通用化**；
> 银行网点退为**驱动用例**（§4.6 的实测数据仍出自它）。
>
> **核实说明（2026-09-28）**：本文档成文后做过一轮逐条核实——把每一处代码锚点、
> 机制描述与实测数字拿去与仓库现状对照，并另做一遍内部一致性与完整度审查。
> 结果是**两条 blocking**（都会让功能按字面实现就不工作）与一批事实订正，
> 已全部落在正文里：
>
> - **`field_name` 必须显式写入**（§5.5）——建绑定路径从不写它，而审批外部选项装配
>   对每条启用绑定 `require` 它；不写就是**整批装配失败**。
> - **`ingest_mode` 的取值域有四处**（§5.2）——除了两处后端白名单，
>   还有一份手工维护的前端契约文件，漏了双端测试都红。
> - 另补了三个**原设计没有的缺口**：逐绑定空快照守卫（§5.7.1）、导入 Action 自己的
>   `MultipartSpec`（§5.4）、导入并发互斥（§5.8）。
>
> 被核实推翻的旧结论集中在 §3.3 的作废表里（含 09-23 版自身的多处错误陈述）。
> **凡是标「实测」的性能与体积数字，仓库内没有独立出处**——它们来自 09-23 那轮
> 在本机的临时表基准，本次未重跑，使用前应复测（§4.4.2 已注明）。

---

## 1. 目标

让飞书外部数据源支持**从 xlsx 文件导入数据**，取数方式与多维表格并列，**出口与数据使用方式完全一致**（§5.1）。

1. **新增第三种取数方式**：`ingest_mode = "xlsx_import"`，与 `push`（手工推送）/ `pull`（定时拉取）
   并列，共用同一套派生与落库管线。
2. **导入流程对标多维表格向导**：上传 → 解析表头 → 用户勾列 → 配父级 → 导入。
   多维表格是「选表 → 选视图 → 勾字段 → 配源标识与父列」，xlsx 是
   「上传 → 解析表头 → 勾列 → 配源标识与父列」，**步骤形状同构**。
3. **不限文件条数**：一次可上传多个文件，只要表头一致就按顺序拼成一份快照
   （物理边界见 §5.4）。
4. **整体替换且原子可见**：重新导入即整份替换，飞书侧搜索期间无空窗、不出现半新半旧。
5. **异常行不喂给飞书**：机械可判定的异常行（如取数列超 `label` 长度上限）
   照常入库但置 `enabled = false`，回执里逐条可归因。**业务语义上的「脏」不算异常**（A14、§5.7）。
6. **出口零分叉**：出站端点、级联、`feishu_option` 落库、控制台展示**一行不改**（§5.1）。

## 2. 非目标

- **不做增量导入。** 与 `pull` 一致，一次导入即一份完整快照。
- **不做「任意表格格式」的自动字段映射。** 取数列与祖先列由**用户在向导里勾选**，
  不做类型推断、不做表头模糊匹配（列名是身份，见 §5.5）。
- **不改 `derive.rs` 的派生口径。** 级联是现有规则的原生形态（§4.3）；
  改口径必须 bump `DERIVE_RULE_VERSION`，本设计不动它。
- **不新建表。** 数据仍落现有 `feishu_option`（§4.1）。
- **不做异步导入。** 本次取同步（§5.4、A7）；**进度查询已落地**（§5.8.1，
  只观测不任务化），退出条件写在 §7。
- **不改 `health_check` 的语义。** xlsx 源不走体检，改由详情页展示导入状态（§5.11）。
- **不做三级以上的专用支持。** 派生已支持任意深度（§4.3），但**浏览器不动**
  ——银行用例仍是两级（D2 理由见 §4.6）。

## 3. 决策记录

### 3.1 产品决策（2026-09-28 确认）

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| D1 | 数据消费方 | 仍是飞书审批外部选项 | 出口不变 |
| D2 | 级联层级（银行用例） | **两级**：`开户行行名` → `联行号` | §4.6：三级会把 23% 的数据变成不可达 |
| D3 | 归属银行 | **只作展示，不参与级联** | 同上；且它对该列 23% 为空 |
| D4 | 导入生命周期 | 会重新导，整体替换 | 复用 `pull` 的补集停用语义（§4.5） |
| D5 | 导入入口 | **后端 Action + 前端向导** | 09-23 版曾定「只做后端」，本次推翻 |
| D6 | 异常行 | 入库但置 `enabled = false`，不喂给飞书 | 复用既有列，无需新列 |
| D7 | 文件数 | **不限条数，按总字节封顶 16 MiB** | §5.4；物理上单请求无法「不限」 |
| D8 | 列的来源 | **从文件表头里勾选**，不预先填列名 | 对标多维表格的「勾选字段」 |
| D9 | 表头与配置缺列 | **缺列即拒**，多列忽略 | §5.7；缺列静默会让审批控件选项凭空消失 |
| D10 | 导入执行 | **同步**（请求内完成） | §5.4；与「服务端零暂存」一致 |
| D11 | 服务端暂存 | **不做**——文件留在浏览器，导入时重传 | §5.6；贴合框架 `RequestScoped` 契约 |
| D12 | 前端形态 | **新建独立向导组件** | §5.6；两条流程步骤差异大，不塞进同一个组件 |

### 3.2 架构决策

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| A1 | 数据放哪 | **不新建表**，落现有 `feishu_option` | §4.1：数据源本就是「对上游一列的投影」 |
| A2 | 如何接入 | **新增 `ingest_mode` 取值**，喂进同一套 `derive_options` | §4.2、§4.4 |
| A3 | 出站路径 | **一行不改**（级联白送） | §5.1；出口零分叉 |
| A4 | 整体替换 | **复用 `pull.rs` 的编排**：摘要比对 → 事务内整行替换 + 补集停用 | §4.5：这套机制已经建好 |
| A5 | 判别列 | **不加** `source_type` | A2 之后没有判别需要 |
| A6 | 解析位置 | 服务端 Rust + calamine 流式 | §4.7 |
| A7 | 执行方式 | 同步（HTTP 请求内完成） | §5.4 |
| A8 | 异常行载体 | `enabled = false` + 原因写 `feishu_option.extra` | 复用既有列 |
| A9 | 列的身份 | **表头名**（trim 后精确匹配），**同时写进绑定的 `field_id` 与 `field_name`** | §5.5：出站与级联全按 `(datasource_id, field_id)` 定位，读路径零改动；但 `field_name` 必须显式写，否则审批选项装配整批失败 |
| A10 | 数据源粒度 | **一个 xlsx 数据集 = 一个数据源**，勾选的每列 = 一条绑定 | §5.5；对标「一张多维表格 = 一个数据源」 |
| A11 | 父子关系 | **同源内**，`parent_field_id` 指向本源的另一条绑定 | §4.3 现状约束（`load_parent_source_key` 按 `datasource_id` 过滤） |
| A12 | 事务粒度 | **逐绑定各一个事务**，与 `pull.rs` 同粒度 | §4.5；一个绑定写坏不拖垮本轮其他绑定 |
| A13 | 事务获取 | 导入 Action 用 `ctx.begin_transaction()` | §4.10；`pull.rs` 走 `database.transaction()` 是因为它没有 ctx，**不可照抄** |
| A14 | 校验规则 | **不硬编码业务语义**，只留机械可判定的规则 | §5.7；09-23 版的「联行号 12 位」随通用化删除 |

### 3.3 历次修订中作废的结论

留档以免有人照着旧版实现：

| 版本 | 结论 | 现状 | 为什么作废 |
|---|---|---|---|
| 09-21 初稿 | 新建 `feishu_bank_branch` 表 | **作废** | 数据源已是「对上游一列的投影」，无需业务表 |
| 09-21 初稿 | `feishu_datasource` 加 `source_type` 判别列 | **作废** | 没有判别需要；`ingest_mode` 已表达取数方式 |
| 09-21 初稿 | 出站 `approval_options` 按类型分流取数 | **作废** | 出站对称，不区分数据来源 |
| 09-21 初稿 | 银行网点专用固定列（联行号/行名/…） | **作废** | 选项模型只有 取数列 + 父键 |
| 09-23 | 「不做前端」（§2 非目标） | **作废** | D5：用户勾列是 UI 动作，本次前端纳入范围 |
| 09-23 | 「不做通用任意 xlsx 的字段映射器」 | **作废** | D8：本次**就是**通用导入，列由用户勾选 |
| 09-23 | 父源/子源是**两个数据源** | **作废** | A10/A11：现为一个数据源、两条绑定（§5.5） |
| 09-23 | 级联声明读 `linkage_mapping.parent_field` | **作废** | 该列**已被整列删除**（§4.1）；改走 `parent_field_id` |
| 09-23 | `RawValue { label, parent_label }` | **作废** | 现为 `RawValue { ancestors, label }`（§4.3） |
| 09-23 | 「`pull.rs` 已在用 `ActionContext::begin_transaction()`」 | **错误** | `pull.rs` 明写不依赖 `ActionContext`（§4.10）；A13 |
| 09-23 | 硬编码「联行号必须 12 位数字」 | **作废** | A14：业务语义规则不进通用导入器 |
| 09-23 | `max_files = 4` | **作废** | D7：不限条数，按总字节封顶 |
| 09-23 | 「根 `AGENTS.md` 完全没提到 feishu addon」 | **过期** | `AGENTS.md:53` 已详述三个 module |
| 09-23 | 「对不上父值时失败退化成静默空集，不再是 `40004`」 | **错误** | 读端解析不出父行时**仍返回 `40004`**；静默空集只剩「父行存在且确无子项」这一合法形态（§4.3） |
| 09-23 | 「`linkage_mapping` 只出现在两个测试里」 | **不准确** | `.rs` 里共 7 处，但**没有一处是生产读取者**（§4.1） |
| 09-23 | 「四类回退分支含『映射缺失或坏』『0 个键命中』」 | **作废** | 那是已删机制的分支；现四类是 无父/不带参数/空 map/值为空（§4.3） |
| 09-23 | 「空快照守卫只记录不停用」 | **不完整** | 它是 `warn` 之后 `return Err`，**整轮失败**；「只记录」会让复用者写出静默不更新的导入器（§4.5） |
| 09-23 | 「改选项集走安全的 MODIFY COLUMN」 | **机制写反** | `Radio` + `.varchar(16)` 编译成 `String` 不是 `Enum`，schema 不比较候选值集合 → **零 DDL**（§5.2） |
| 09-23 | 「导入挂审计、探表头不挂」 | **做不到** | 中间件只有 Module/Addon 级，无 Action 级挂载点；两条都会被记录（§5.11） |
| 09-23 | 「回执形状对齐 `PullReport`」 | **臆造** | 仓库里没有这个类型；现实现是表级的 `TablePullOutcome`，形状不可对齐（§5.11） |
| 09-23 | 「新建向导要登记进 `features/registry.ts`」 | **不必要** | 那是**自定义 View** 注册表（键是后端 view id），向导不是 View（§5.6） |
| 09-23 | 「xlsx 绑定的 `field_name` 与 `field_id` 同值，复用即零改动」 | **blocking** | 建绑定路径**从不写 `field_name`**，而审批选项装配对它 `require` → 恒 NULL 会让装配整批失败。必须显式写入（§5.5） |
| 09-23 | 「改 `ingest_mode` 只欠两处白名单」 | **漏一处** | 还有 `frontend/contracts/feishu-projections.json` 的取值域，漏了双端测试都红（§5.2） |

**已完成、不再包含的**：初稿的「第 0 步修 `page(1, PAGE_SIZE + 1)`」——已修（§4.8）。

## 4. 关键事实（逐条核实，带锚点）

> **行号会漂移。** 本节锚点核实于 2026-09-28；若对不上，按符号名找。

### 4.1 数据源是「对上游一列的投影」

`feishu_datasource` 现在是一份**取数配置**，不只是凭据登记（`datasource/table.rs`）：

| 列 | 说明 |
|---|---|
| `id` | 主键；`filterable` + `sortable`（台账列表靠它收尾出全序） |
| `title` / `status` / `ingest_mode` | 展示名 / 启用停用 / **取数方式**（`Radio`，现值 `push`/`pull`，默认 `push`，`filterable`） |
| `bitable_base_token` / `bitable_table_id` / `bitable_view_id` | 多维表格坐标（三个路径段）。**xlsx 源留空** |
| `last_pull_at` / `last_success_at` / `consecutive_failures` / `last_error` | 同步状态与告警 |
| `snapshot_digest` | **已废弃**（摘要归属已改为「每条绑定一份」），保留列只为避免破坏性删列 |

**取数列与级联声明都已不在表级行上**，它们属于**字段绑定层**
（`datasource/domain/field_table.rs`）。表声明里有两处测试把这件事钉死
（`datasource/table.rs`：`the_table_row_carries_no_credential_or_routing_columns`、
`the_field_level_coordinates_are_gone`），断言 `source_key` / `token_hash` /
`bitable_field_name` / `linkage_mapping` **不在**表级行上。HEAD 上 `linkage_mapping`
这个字符串在 `.rs` 里共 7 处，**没有一处是生产读取者**：两处是上述「断言它已删」的测试，
其余是 `linkage.rs` 的模块文档、`datasource/actions/mod.rs` 的模块文档与
`tests/feishu_approval_options_integration.rs` 的注释，都在讲「它曾经是什么」。
那一列连同它的 JSON 解析器已经**整个删除**（§4.3）。

因此「银行网点」不需要新表：数据源只是**取数列换成各列**的一个绑定集合。

### 4.2 管线：拉取 → 派生 → 落库

`pull.rs` 的模块文档逐字写着编排：
**「选表 → 解析列名 → 一次取回 → 逐字段派生 → 事务内落库与补集停用」**。

关键步骤（`pull.rs`）：

1. **选表**：`where_eq("ingest_mode", "pull")` + `status = active`。
2. **拉取**：`list_all_records(...)` 取 `[取数列, 祖先列…]`——**同一张表 N 个字段共用一份快照**。
3. **派生**：`extract_values_owned(...)` → `derive_options(...)`。
4. **摘要比对**：`snapshot_digest(&derived)` 与库里比对；**内容未变且本地没有已停用行时跳过写库**
   （「没有已停用行」这个附加条件不可省，否则被停用的行永远恢复不了）。
5. **事务内**（**逐字段各一个事务**）：整行替换 → 补集停用 → 审计 → 更新绑定状态。
   **跨源预检与补集扫描都在开事务之前完成**（`pull.rs` 有注释明写这一点），
   事务里只做写入——这一点在复用时要照抄，否则会把预检的长耗时圈进事务里。

**这就是导入要复用的全部下半场。** 导入只替换第 2 步。

### 4.3 级联：形状与配对

**当前的形状是「一条绑定的祖先链」，不是 JSON 声明。** 级联声明原先存在
`feishu_datasource.linkage_mapping` 的一段手写 JSON 里，**那一列已被整个删除**：

- 现在父由**字段绑定行上的 `parent_field_id`** 指向**同一个数据源**里的另一条绑定
  （`field_table.rs`，列注释：「同表内的父列。多级由链涌现」）。
- 读端按绑定体系推出父的 `source_key`：`approval_options::load_parent_source_key`
  按 `datasource_id` + `field_id` 查（**所以父必须同源**，A11）。
- 写端按同一指针取父的**当前**列名：`pull::parent_linkage` 沿 `parent_field_id`
  反复上溯拼链，**带防环守卫**（手工改库能造出 a→b→a，不设 visited 会把一轮拉取卡死）。
- 曾经在此的 `parse_linkage_mapping` / `match_linkage` / 通配键 `"*"` 解析
  **连同单测一并删除**（`linkage.rs` 模块文档记了裁定）。
- `Linkage` 类型由「一个父」扩成「一串祖先」（`LinkageLevel { source_key, field_name }`），
  2026-09-27 改，`DERIVE_RULE_VERSION` 1 → 2。

**配对靠同行共现，不去父表反查**（`derive.rs` 的 `RawValue.ancestors` 注释）：
子绑定从**同一条记录**同时读「取数列」和各级祖先列，用祖先文案逐级折叠算出父键。

**`option_id` 的派生规则**（`derive.rs`，当前 `DERIVE_RULE_VERSION = 2`）：

```text
无父：{source_key}:{hex(sha256(label))[:12]}
有父：{source_key}:{hex(sha256(ancestor_key ‖ U+001F ‖ label)[:12])}

ancestor_key = 从根到直接父逐级折叠（derive::ancestor_key）
```

**为什么任意深度都能配对**：父键与父行自己的 `option_id` **由同一个函数、同一组输入算出**，
相等是**构造性**的。「子行的 `parent_key` == 父行自己的 `option_id`」是唯一正确的不变量
（读端拿父行的真实 `option_id` 做等值匹配）。

> **这一段曾被实测纠偏。** 旧实现把 `option_id_of` 的中间参数硬编码成 `None`，
> 等价于断言「父源自己没有父」，于是父一旦是链的中间列，父键与父的真实 id **恒不相等**，
> 下拉**静默变空**。失效形态很刁钻：**时灵时不灵**——只在父行自己那格的父键为空时才相等。
> 真机实测 `fldeblar7x` 10/10 好用、`fldm0j5do3` 43 个子项 **0 命中**、`fldtyg5vbz` 4/4。
> 按「三级一律坏」去排查找不到规律。**这条是本设计最该记住的教训：级联的失败是静默空集，
> 不是报错。** 详见 `docs/architecture/feishu-datasource-table-config.md` §8.1。

**读端过滤**：先取绑定行的 `parent_field_id` → `load_parent_source_key` → 命中则把
`where_in("parent_key", […])` 挂在**顶层**（顶层条件隐式 AND，不影响 keyset 游标）。
用 `where_in` 而非 `where_eq` 是因为父键**是复数**（一个控件可能挂在多个父下）。
飞书回传值经 `normalize_linkage_value` 剥掉 `@i18n@` 前缀。

**失败码**：`40003` 联动键歧义、`40004` 父值无法解析。
四类「回退到全量」的分支（**无父** / 有父但不带 `linkage_params` / 空 map / 值为空）
都有单测——注意这四类里**没有**「映射缺失或坏」「0 个键命中」，
那两个是已删除的 `linkage_mapping` 时代的分支。

**父值解析不出时仍然返回 `40004`，不是静默空集。** 读端解析父行时同时按
`option_id` 与 `label` 两种口径查（所以「父文案被改名」不再必然失败），
但**两样都查不到**时走的是 `40004 LINKAGE_NOT_RESOLVED`。
而解析父行那一步带的 `enabled = true` 过滤是一条**刻意的守卫**：它挡掉的正是
「父文案改名 → 命中已停用的父行 → 子查询按 `enabled` 滤空 → 表现为 `code=0` + 空集」
这条退化路径，守住的是「**父值不存在**」与「**父值存在但确无子项**」必须可分辨。

> 因此「静默空集」只剩一种合法形态：**父行存在且启用、它确实没有子项**——
> 那是正确结果，不是故障。
>
> **这条是本设计最该记住的教训**：级联的坏法是**静默的**，可分辨性是靠刻意加的守卫
> 维持的。导入侧的「缺列即拒」（D9）是同一个道理——宁可拒绝，不要静默出错数据。

### 4.4 `feishu_option` 的现状与基准实测

| 列 | 声明 | 备注 |
|---|---|---|
| `option_id` | `Str` require **unique** max_length 128 | 表级唯一索引 |
| `source_key` | `Str` require indexed filterable sortable | |
| `label` | `Str` require max_length 255 searchable **filterable** | `filterable` 是级联按文案解析父值的硬依赖（`where_eq("label", …)`），有专门测试钉死 |
| `parent_key` | `Str` max_length 192 **indexed filterable** | 存父的**裸 option_id**；**不能 unique**（一对多）、**不能 require** |
| `sort_order` | `Int` require default 0 sortable **filterable** | filterable 是 keyset 的前提 |
| `is_default` / `enabled` | `Switch` | `enabled` 是出站的过滤位 |
| `i18n` / `extra` | `Text`（JSON 文本） | DSL 没有 Json builder |

索引现状：**只有三个单列索引**（unique(option_id)、index(source_key)、index(parent_key)），
**没有 `index_named(...)` 复合索引**。`sort_order` 是 ORDER BY 首键且参与 keyset 谓词但无索引
→ 每页 filesort。搜索是 `OR LIKE '%kw%'` 前置通配 → 不可索引。

#### 4.4.1 现状实测（2026-09-23，本地 `yang_system` 库，只读）

| 项 | 实测值 |
|---|---|
| `feishu_option` 当前行数 | **14**（`payment_currency` 7 + `payment_fx_rate` 7） |
| 数据源 | 3 个：`test`(push) / `payment_currency`(pull) / `payment_fx_rate`(pull) |
| 索引 | `PRIMARY(id)`、`uk_feishu_option_option_id`(unique)、`idx_feishu_option_source_key`、`idx_feishu_option_parent_key` |
| `innodb_buffer_pool_size` | **134217728（128 MiB）**，`innodb_buffer_pool_instances = 1` |
| MySQL | 8.0.46 |

#### 4.4.2 基准实测（2026-09-23，真实数据，临时表已 DROP）

**方法**：建一张 `_bench_feishu_option`（DDL 与索引照抄 `feishu_option`），
灌入**真实的两个 xlsx 数据**——`label=开户行行名`（去重后 154,362 行）+
`label=联行号`（154,386 行），共 **308,748 行**，`option_id` / `parent_key` 用
`derive.rs` 的真实规则算出。外加一个 7 行的 `payment_currency` 验证隔离性。
对同一条 SQL 跑 5 次取中位。**只建删自己的临时表，未触碰 `feishu_option`。**

> **可信度分级（2026-09-28 核实追加）**：下面这组数字**在仓库里没有独立出处**
> ——没有 benchmark 文件、没有提交信息、没有测试残留，只存在于本文档里。
> 其中**数据侧的部分已被独立复算证实**（行数 154,362 / 154,386、合计、各列去重与空值、
> 以及 §4.6 那张逐列统计表，都能用盘上的两个 xlsx 重算出来，结果一致）；
> 但**耗时、索引体积、缓冲池占比、filesort 是否消失**这些**运行时数字无法复核**。
> 它们出自 09-23 那轮本机临时表基准，本次未重跑。
> **使用前请复测**——尤其是决定加不加 §5.10 那个索引之前。

> **本次改版不影响这组数字。** 实测按「两个 `source_key`」灌数；新模型是
> **一个数据源、两条绑定**，但**每条绑定仍有自己的 `source_key`**，
> 所以 `feishu_option` 的落库形状与实测**完全一致**。

**中位耗时（ms），「快 N×」= 加索引后相对之前的倍数：**

| 场景 | 无复合索引 | 有复合索引 | |
|---|---|---|---|
| 父源 第1页 无关键词 | 246.4 | **1.9** | 快 128× |
| 父源 第1页 常见词「银行」 | 304.3 | **1.9** | 快 163× |
| 父源 第1页 **罕见词（整条行名）** | 204.9 | 241.3 | **慢 1.18×** |
| 父源 `COUNT(*)` | 128.9 | 124.5 | 基本不变 |
| 父源 `COUNT(*)` + 常见词 | 189.9 | 167.0 | 基本不变 |
| 子源 第1页 无关键词 | 323.1 | **3.1** | 快 105× |
| 子源 第1页 码片段「102304」 | 276.6 | **29.3** | 快 9× |
| 子源 第2页 keyset | 332.2 | **2.3** | 快 144× |
| 子源 `COUNT(*)` | 233.3 | 249.0 | 略慢 |
| 级联父值探测 | 1.1 | 1.0 | 不变 |
| **小源 第1页（隔离性）** | **1.2** | **1.2** | **完全不变** |

**端点每次请求的实际付出**（`paginate` 无条件先 COUNT、再 SELECT）：

| | 无索引 | 有索引 |
|---|---|---|
| 父源 无关键词 | 375 ms | **126 ms** |
| 父源 常见词 | 494 ms | **169 ms** |
| 子源 无关键词 | 556 ms | **252 ms** |
| 子源 码片段 | 510 ms | **278 ms** |

**四条结论，其中第一条推翻了先前的判断：**

1. **「表变大 → 别的数据源变慢」被实测证伪。** 表里躺着 30.9 万行时，
   7 行的 `payment_currency` 查询仍是 **1.2 ms**，加不加索引都一样。
   所有查询都带 `source_key = ?`，`idx_feishu_option_source_key` 把它收窄到该源自己的行。
2. **加索引后 filesort 完全消失**（EXPLAIN `Using filesort` → `key=idx_feishu_option_pick`、
   `type=range`、`Using index condition`），常规路径快 **105–163×**。
3. **但索引不是「没有就不可用」**：不加索引时端点实际付出 **375–556 ms**，
   **已经在 2500 ms 预算内**。加索引把它降到 **126–278 ms**。**所以它是显著优化，不是硬前提。**
4. **`COUNT(*)` 才是真正的残余成本**：125–249 ms，且加索引后**几乎不改善**
   （它仍走 `idx_feishu_option_source_key` + `Using where`）。它由框架的 `paginate`
   无条件触发（`read.rs`，无跳过开关），**每次请求都要付**。端点的地板价就是它。

**体积（实测）**：两源 数据 **47.6 MB** + 索引 **67.3 MB** = **114.9 MB**，
占 128 MiB 缓冲池的 **90%**。复合索引每个源约 +11.5 MB。

**诚实标注一处反常**：罕见词（整条行名）加索引后**慢 1.18×**（204.9 → 241.3 ms）。
原因是索引让它走 `type=range` 顺序扫并逐行回表判 `label LIKE`，
而原来的 filesort 只对**少量匹配行**排序。绝对差 36 ms，可接受，
但它说明**索引不是单调改善**——这正是必须实测而不能推理的地方。

### 4.5 整体替换已经建好：补集停用

`pull.rs` 的补集停用就是本需求的「整体替换」：

- 事务内「整行替换 + 补集停用」。
- **可疑空快照守卫**：拉到 0 行且库里仍有已启用行时，判为可疑、**不停用**，
  并且**整张表本轮直接失败**（`tracing::warn` 之后 `return Err(ConfigError)`），
  不是「打条日志继续跑」。否则一次上游故障会把该数据源 100% 的选项静默停掉。
  **「不停用」与「整轮失败」是两件事，都要照抄**——只抄前者会得到一个
  「数据没更新但一声不吭」的导入器。
- **跨源预检** `find_foreign_option_owner`：写之前确认这些 `option_id` 不属于别的源。
- **摘要是按「本轮应当是什么」算的，不含 `enabled`**——所以跳过条件里必须带上
  「本地没有已停用行」，否则被停用的行永远恢复不了。

**导入必须原样复用这四条**，尤其是空快照守卫与跨源预检。

### 4.6 驱动用例的数据实测（两个真实 xlsx）

| 项 | 文件 1 | 文件 2 | 合计 |
|---|---|---|---|
| 字节 | 4,803,614 | 2,594,902 | **7.4 MB** |
| sheet | `境内银行网点信息管理` | 同名 | — |
| 数据行 | 100,000 | 54,386 | **154,386** |
| 序号 | 1–100000 | 100001–154386 | **接续的两半** |

文件在仓库里：`docs/境内银行网点信息管理-{1,2}.xlsx`（`.gitignore` 忽略 `docs/*.xlsx`，
但文件本身在盘上）。**它们现在是测试夹具。**

**三级结构实测**：

| 层级 | 去重后 | 判读 |
|---|---|---|
| L1 `归属银行` | **296** | 真收窄：15.4 万 → 每行约 520 个 |
| L2 `(归属银行, 开户行行名)` | **154,363** | 几乎不折叠 |
| L3 `(归属银行, 开户行行名, 联行号)` | **154,386** | 仅在 **9 处**真正一对多（共 32 行） |

**决策 D2 的依据**：

```text
归属银行为空的行 = 35,611  （23.1%），涉及 35,592 个不同网点名
```

这 23% 不是垃圾数据，是**真实网点**只是该列没填：农商行、村镇银行、`中国人民银行`、
`中华人民共和…`、`浙商银行CIPS虚拟行号`、各类财务公司、香港地区机构。

后果：**在「归属银行 → 支行 → 联行号」这条路上，这 3.56 万个网点根本到不了**。
要保住它们就必须造一个「其他/未分类」根节点，而那个根下会有 3.5 万个支行——等于没选父级。

**因此银行用例选两级（D2）**：`开户行行名` → `联行号`，全部 154,386 行可达，
且与现有派生规则原生匹配，`derive.rs` 一行不用改。

**两级下的实测数字**：

| 项 | 值 |
|---|---|
| 行名绑定选项（去重） | **154,362** |
| 联行号绑定选项 | **154,386** |
| 真正一对多的父级 | **10**（共 34 行）——按**行名**分组 |
| 对照：三元组 `(归属银行, 行名, 联行号)` 分组 | 9（共 32 行）＝ 上表 L3 那条 |
| 联行号唯一性 | **全部唯一，零空值，两文件零重叠** |
| 行名长度 | p50=18 / p95=23 / p99=26 / max=180 |
| 归属银行 / 编码 | 各 296 个去重值，**35,611 行为空** |
| 开户行地址 / 地区编码 | 各 341 个去重值，236 空 |
| **地区名称** | **整列 100% 为空** |

**脏数据（4 行）**：

| 序号 | 内容 | 本设计的分类 |
|---|---|---|
| 153150 | 行名 = 180 个字符的 `1234567890…` | **不算异常**（180 < 255，机械上合法） |
| 153739 | 联行号 = `2223333444555`（13 位） | **不算异常**（A14：不猜业务语义） |
| 153742 | 联行号 = `12345678901234`（14 位） | **不算异常**（同上） |
| 某行 | 归属银行编码 = `2Checkout` | **不算异常**（长度合法、非空） |

> **与 09-23 版的分歧**：那一版把前三条标为异常（因为硬编码了银行规则）。
> 本版**不标**——导入器通用化了，业务语义规则不进解析器（A14）。
> 若要把这类行挡住，那是**数据治理**问题（在源文件里删掉），不是导入器的职责。
> 硬编码一个 `>120` 之类的阈值只会让规则变成拍脑袋。

### 4.7 xlsx 解析：calamine，MSRV 1.80 是硬门禁

- MSRV 1.80 是**真门禁**：CI 有独立 `msrv` job 在 1.80 上跑
  `cargo check --all-targets --locked`（`.github/workflows/ci.yml`）。
  实际工具链 1.97.1，但 1.80 那个 job 过不了就是过不了。
- **`calamine` 只能锁 0.30.x**：0.30.1 → MSRV 1.75；0.31–0.35 → 1.83；0.36.1 → 1.88。
  写 `calamine = "=0.30.1"`。**已在 Rust 1.80.x 上实测编译通过**（连带 `zip 4.2.0`；
  具体版本与来源见本节末的「当前状态」）。
- 生效机制是 `.cargo/config.toml` 的
  `resolver.incompatible-rust-versions = "fallback"`；移除它会让解析选到
  `zip 4.6.x`（需 1.82）从而弄坏 msrv job。
- **实测这两个文件**（release）：流式 **523ms + 311ms**；内存峰值
  **26.9MB**（流式）对 77.8MB（全量加载），时间几乎相同 → **用流式**。
- 流式 API：`Xlsx::worksheet_cells_reader(name)` 返回 `XlsxCellReader`，
  是**手拉游标、不实现 `Iterator`**，需 `while let Some(c) = reader.next_cell()?` 驱动。
  `use calamine::Reader as _;` 必须在作用域内（`sheet_names` 等是 trait 方法）。
- `open_workbook` 会**先整体加载共享字符串表**，是内存下限，0.30.1 无法推迟。
- 新增约 11 个包；其余（`flate2`、`encoding_rs`、`chrono`、`indexmap` 等）已在锁文件里。
- yang-system 被 `lib_yang/Cargo.toml` 的 `exclude` 排除在工作区外，**没有 cargo-deny 门禁**
  （`deny.toml` 只在框架仓库）——新增依赖的许可证审查是**人工责任**；
  上述 crates 的许可证均落在框架 allowlist 内。
- **当前状态**：`calamine = "=0.30.1"` 已加入 `Cargo.toml`，并已在 **Rust 1.80.1 上冷缓存实测通过**
  （空 `CARGO_HOME` / `CARGO_TARGET_DIR` 跑 `cargo check --all-targets --locked`，`exit 0`；
  容器内 `rustc --version` 确为 1.80.1，镜像 digest 就是官方 `library/rust`）。
  **这是本机 docker 实测，不是 CI**——CI 的 `msrv` job 用 `toolchain: "1.80"`
  （`.github/workflows/ci.yml`，今天解析到 1.80.1），要等本分支推上去才会跑。

### 4.8 出站端点：缺陷已修，但有性能边界

- **`page(1, PAGE_SIZE + 1)` 缺陷已修复**：现在是 `query.page(1, PAGE_SIZE)?`，
  `hasMore` 由 `page.total` 推出，并加了**编译期断言**
  `const _: () = assert!(PAGE_SIZE <= MAX_TABLE_QUERY_PAGE_SIZE);`。
  初稿的「第 0 步前置修复」**已完成**，本设计不再包含。
- 但**框架仍然拒绝而非截断**超限 page_size，且 `PAGE_SIZE = 100` 正好等于硬上限
  ——所以「多取一行判 hasMore」在结构上不可能。
- `COUNT(*)` **仍无条件执行**（`read.rs`，无跳过开关），且 COUNT 与 SELECT 是
  **两条独立的 autocommit 语句**，并发写入下可能看到不同快照——这正是 `has_more`
  只能由 `nextPageToken` 承载的原因。
- **无复合索引**，`sort_order` 无索引 → 每页 filesort；搜索前置通配 → 不可索引。
- **本设计正好落在这个最坏用例上**：15.4 万 × 2 绑定 = 约 30.9 万行，
  且联行号那条绑定每次请求还要多一次父值存在性探测查询。

### 4.9 multipart 上传

- **`MultipartSpec` 定义在 lib_yang**：`crates/yang-base/src/definition/media.rs`。
  七个字段：`max_fields`(64) / `max_files`(8) / `max_file_bytes`(10 MiB) /
  `max_text_field_bytes`(64 KiB) / `max_total_bytes`(**32 MiB**) /
  `allowed_content_types` / `lifecycle`。默认常量在同文件顶部。
- 与现有 feishu Action 同构：`ActionFnBuilder` 上的 `.multipart(MultipartSpec::new([...]))`
  （`definition/interface.rs`），可与 `.route()/.permissions()/.register()` 链在一起。
- handler Input 里放 `Vec<UploadedFile>`。**`UploadedFile` 没有 `bytes()` / `reader()`**，
  只能经 `path()` 自己读（`action/upload.rs`），或者 `copy_to()`。
- **临时文件是请求作用域的**：handler 返回即清理（成功与失败路径都有测试钉死）。
  `UploadLifecycle` 只有 `RequestScoped`。
  **必须在 handler 内用完** —— 这也是选同步（A7）的原因之一。
- `allowed_content_types` 与客户端自称 MIME **精确匹配**（`transport/axum.rs`）。
  Windows 上 `.xlsx` 映射为
  `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`，可过。
  **但这是客户端自称**：`media.rs` 明确它不能替代内容校验，
  **handler 必须自行嗅探 `PK\x03\x04`**。
- **启动期 fail-closed**：任一 multipart Action 的 `max_total_bytes`
  大于 `AxumTransportConfig.max_body_bytes` → **进程拒绝启动**。
  `MultipartSpec` 默认 `max_total_bytes = 32 MiB`，而本项目生效上限是 **1 MiB**
  → **不显式设小就起不来**。
- 认证与 JSON Action 一致：去掉 `.public()`、模块挂 `with_authentication`、声明 `.permissions([...])`。
- **仓库现状：`src/` 生产代码里没有任何 Action 用过 multipart。**
  唯一的「上传」是头像（`account/user/actions/upload_avatar.rs`），
  它走 **base64 JSON**，不是 multipart。
  （唯一的 multipart 实例是 `examples/frontend_demo/actions/upload.rs`，
  它是前端演示/验收用的，不是生产路径。）
  **通道完整，但没有生产消费者**——这是本设计要当心的地方（§7）。

### 4.10 批量写入与事务

- 可达：`yang_db::QueryBuilder::from_pool(&pool, &TableRef::new(name)?).insert_batch(&rows)`。
  约束：列集一致、`INSERT_BATCH_SIZE = 500`、`MAX_BIND_PARAMS = 65535`、
  **所有分片在同一事务内**。
- **`created_at` / `updated_at` 陷阱**：它们是 `.required().not_writable()`，
  DDL 渲染为 `BIGINT NOT NULL` **无默认值**。`insert_in_tx` 靠
  `prepare_and_validate_insert` 补盖，**裸 `insert_batch` 绕过它，必须自己写这两个字段**，
  否则 MySQL 严格模式报 `Field 'created_at' doesn't have a default value`。
- **但**：`pull.rs` 的 `apply_option_rows`（`domain/option_write.rs`）已经把这件事做对了
  ——这正是导入该复用它而不是自己拼 `insert_batch` 的理由。
- **事务获取方式：导入 Action 用 `ctx.begin_transaction()`（A13）。**
  `ActionContext::begin_transaction()` 支持 `Transaction::table(&TableRef) -> QueryBuilder`
  与 `insert_batch`。
- **不要照抄 `pull.rs` 的取事务方式**：`pull.rs` 的模块文档明写
  **「本模块不依赖 `ActionContext`」**（后台 worker 没有可用 ctx），
  所以它走 `deps.database.transaction()`。导入是 Action，**有 ctx，就该用 ctx**。
  （09-23 版把这一点写反了，见 §3.3。）
- `scripts/check_architecture.py` 的批量写入规则**不触发**：该正则只在
  `tenant_code_boundaries` 内生效，而它遍历的 `src/addon/{org,work}` 不存在
  （`src/addon/` 下是 `access`/`account`/`demo`/`feishu`）。
  **本设计不需要任何 raw SQL，也不需要门禁豁免。**

### 4.11 action 注册与 `presentation` 无关

`compile_runtime_modules` 在 `presentation` 为 `None` 时 `continue`，
**但** handler 是**独立一轮**注册的（`compile.rs`），遍历
`addons → modules → action_pairs()`，**没有 presentation 守卫**；
而 `catalog.actions` 正是从 `registry.handlers` 投影。

**结论**：新 Action 即使不声明 `presentation()`/`view()`，仍会进 `catalog.actions`、
**权限仍可授予、接口仍可达**。两个导入 Action 挂在既有 `datasource` module 上，
不需要新 module。

### 4.12 前端已具备的能力（本设计只需接，不需建）

这是本次改版把前端纳入范围后**必须核实**的一条——结论是**上传基建已经齐了**：

- **HTTP 层已支持 multipart**：`engine/http/action-request.ts` 的 `appendMultipart()`
  构造 `FormData`，并按 `MultipartSpec` 做**客户端侧**校验
  （max_files / max_file_bytes / max_fields / max_text_field_bytes / max_total_bytes /
  allowed_content_types）；`buildActionRequest()` 在 `request_media_type === "multipart"`
  时自动走 FormData 分支。
- **表单层已能渲染文件控件**：`engine/renderers/form/SchemaField.tsx` 对
  schema 里 `format: "binary"` 的字段自动渲染 `<input type="file">`，
  `accept` 由 `multipart.allowed_content_types` 提供；`engine/contracts/ajv.ts`
  注册了 `binary` format。
- **契约层已声明该媒体类型**：`engine/contracts/ui-catalog.ts` 的
  `request_media_type` 枚举含 `multipart`；`ActionInvokePanel` 已有 multipart action 的渲染通路。
- **取数方式的前端表达只有两种**：`features/feishu/types.ts` 的
  `IngestMode = "push" | "pull"` 与 `INGEST_MODE_OPTIONS`（两条）——本设计要加第三条。
- **现有向导的形状**（`components/DatasourceTableWizard.tsx`，565 行）：
  第 1 步填名称与 Base Token 拉数据表 → 第 2 步选视图 → 第 3 步勾选字段 →
  第 4 步给每个勾选字段配源标识与父列，然后创建。
- **实际在用的**文件上传入口只有头像（`AccountSettingsPage`），且走 base64 JSON。
  另外 `SchemaField.tsx` 会为 `format: binary` 字段渲染通用的 `<input type="file">`
  ——**那个控件一直在，只是没有生产 multipart Action 触发它**。本设计正好是第一个触发它的人。

**结论**：前端要新增的是**向导组件本身**，不是上传基建。

## 5. 设计

### 5.1 一致性边界（本设计的纲）

**入口分叉，出口与数据使用方式完全一致。只有「取数」这一处岔开。**

| | 多维表格 | xlsx 导入 |
|---|---|---|
| 行从哪来 | `list_all_records` 出去拉 | 解析上传的文件 |
| 配置从哪来 | 选 base/table/view + 勾字段（`field_id`） | 上传解析表头 + 勾列（列名） |
| 谁来触发 | worker 定时 | 人点「导入」 |

**共用、零改动**：

- `feishu_datasource` / `feishu_datasource_field` 两张表——**同一类对象**，只是 `ingest_mode` 不同
- `derive_options` + `DERIVE_RULE_VERSION`（派生口径**不许按来源分叉**）
- `feishu_option` 落库：`apply_option_rows` + 补集停用 + 空快照守卫 + 跨源预检
- 级联：`parent_field_id` → `ancestor_key` 逐级折叠，深度不限
- **出站 `approval_options`**：按 `source_key` 路由，**完全不区分数据从哪来**；
  `40003`/`40004` 失败码、`@i18n@<option_id>` 契约不变
- 每条绑定一份入站 Token 凭据（xlsx 源照样签发——它也可以手工推送，且保持模型统一）
- 控制台列表 / 详情 / 编辑 / 删除 / 字段绑定展示

> **「零改动」只覆盖读出与落库这两条链。** 有一处**写入侧**的改动是必须的、
> 且不在下面这张清单里：建 xlsx 绑定时要把列名**同时**写进 `field_id` 与 `field_name`
> ——后者现在没人写，而审批外部选项装配对它 `require`（详见 §5.5）。这是本次核实
> 查出的一条 blocking，别因为「共用、零改动」这句话而漏掉。

**唯一要单独裁定的共用点**：`health_check` 的分支**先看坐标是否齐备**
（`base_token` 与 `table_id` 都有值就直接返回 Ok，不看 `ingest_mode`），
只有坐标缺项时才落到「`ingest_mode == "pull"` 报缺列，否则报 `40905 NOT_CHECKABLE`」。
所以准确说法是：**留空坐标的 xlsx 源会命中 `40905`**——而 xlsx 源按 §4.1 正是不带坐标的，
所以它**不走体检**（它没有上游可探），改由详情页展示导入状态（§5.11）。
**这是新增展示，不是改体检语义。**

### 5.2 接入方式：第三种 `ingest_mode`

```rust
ingest_mode => Radio::<String>::new()
    .title("取数方式")
    .require(true)
    .varchar(16)
    .options([("push", "手工推送"), ("pull", "定时拉取"), ("xlsx_import", "文件导入")])
    .default("push")
    .filterable(true),
```

**表声明这一行不够——取值域在三个地方各写了一遍，漏一处就是红的。**
这是本设计最容易被低估的一步：

| # | 位置 | 形态 | 漏了会怎样 |
|---|---|---|---|
| 1 | `datasource/table.rs` | `Radio` 的 `.options([...])` | 前端选项表与契约门禁对不上 |
| 2 | `create_datasource_table.rs` | `matches!(mode, "push" \| "pull")` 白名单 | 向导建不出来 |
| 3 | `update_datasource_table.rs` | 同上，另一份拷贝 | **向导建得出来、编辑保存不了** |
| 4 | `frontend/contracts/feishu-projections.json` | `enums.feishu_datasource.ingest_mode.values` | **`cargo test` 与 `pnpm check` 双红** |

**第 4 处最容易漏**，因为它不在 `src/` 也不在 `frontend/src/`，而在
`frontend/contracts/` 下的一份**手工维护**的契约里。后端有一个测试对它做
**逐字且含顺序**的比较，前端也有一个测试拿它比对 `INGEST_MODE_OPTIONS`。
`scripts/dump_openapi.py` **不产出这个文件**（它只产 `openapi.json` 与 `api-types.ts`），
所以「重新生成契约」不会替你更新它——必须手工改。

轮询选表用 `where_eq("ingest_mode", "pull")`，所以 `xlsx_import` 的数据源
**不会被定时任务碰**——正是想要的行为。

> **落地状态**：上述四处取值域已全部落地并保持一致（`datasource/table.rs`、
> `create_datasource_table.rs`、`update_datasource_table.rs`、
> `frontend/contracts/feishu-projections.json`），两个新 Action 见
> `datasource/actions/probe_xlsx_headers.rs` 与 `datasource/actions/import_xlsx.rs`。

> **一处机制纠正（与 09-23 版的写法相反）**：`ingest_mode` 虽然声明成 `Radio`
> 且带 `.varchar(16)`，它编译出来是 **`FieldType::String { max_length: 16 }`，不是 `Enum`**
> （`Radio` 只有在不指定 `varchar` 时才走枚举）。而 schema 的类型兼容判定对 `String`
> **只看 varchar/char 与长度够不够，完全不比较候选值集合**。
> 所以**加一个选项值不产生任何 schema 差异，不会生成 `MODIFY COLUMN`**——
> 这比 09-23 版说的「安全的 MODIFY COLUMN 自增」更好：**它是零 DDL**。

### 5.3 Action ①：探表头

```text
POST /api/v1/feishu/datasources/xlsx/probe
权限：feishu.datasource.write
媒体：multipart/form-data
副作用：无（不写库、不签发凭据）
```

**请求**：只有文件字段 `files: Vec<UploadedFile>`。

**MultipartSpec**（必须显式设上限，否则启动失败，§4.9）：

```rust
MultipartSpec::new(["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"])
    .max_files(32)
    .max_file_bytes(16 * 1024 * 1024)
    .max_total_bytes(16 * 1024 * 1024)
```

**处理流程**：

1. 逐文件嗅探魔数 `PK\x03\x04`（§5.7）。
2. 读**第一张 sheet 的第一个非全空行**作为表头（定位规则见 §5.7）。
   **只读表头，不扫全表**——所以这个 Action 快（毫秒级）。
3. 校验**文件之间表头集合完全一致**；不一致则整份拒绝，点名哪个文件差哪些列。
4. 跳过空表头列；表头重名则拒绝（§5.7）。

**响应**（用银行夹具的真实形状，8 列全带出，`index` 是 **1-based** 物理列号）：

```json
{
  "sheet_name": "境内银行网点信息管理",
  "sheets": ["境内银行网点信息管理"],
  "header_row": 1,
  "columns": [
    { "name": "序号", "index": 1 },
    { "name": "开户行行名", "index": 2 },
    { "name": "归属银行", "index": 3 },
    { "name": "归属银行编码", "index": 4 },
    { "name": "联行号", "index": 5 },
    { "name": "开户行地址", "index": 6 },
    { "name": "地区名称", "index": 7 },
    { "name": "地区编码", "index": 8 }
  ],
  "files": [{ "name": "境内银行网点信息管理-1.xlsx" }, { "name": "…-2.xlsx" }]
}
```

**`columns` 返回全部列**（不是筛选过的），因为「哪些列适合当外部选项」由人判断——
这与多维表格 `list_bitable_fields` 的口径一致（它也是「返回全表字段，不受视图影响，
类型码原样带出，由运维自己判断」）。领域列如 `序号`、`地区名称` 也照实带出，
**导入器不做任何类型或语义过滤**。

`sheets` 带回**全部** sheet 名：多于一张时前端提示「只读了第一张」（§5.7）。

### 5.4 Action ②：导入

```text
POST /api/v1/feishu/datasources/{datasource_id}/import
权限：feishu.datasource.write
媒体：multipart/form-data
```

**路径用自增 `datasource_id`，不用 `source_key`。** 09-23 版 §8-1 建议的
`{source_key}` 是错的——`source_key` 是**绑定级**标识（每条绑定一个、全局唯一），
数据源级没有它。

**请求**：路径段 `datasource_id`；文件字段 `files: Vec<UploadedFile>`。

**MultipartSpec（与 probe 同口径，但同样必须显式写出来）**：

```rust
MultipartSpec::new(["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"])
    .max_files(32)
    .max_file_bytes(16 * 1024 * 1024)
    .max_total_bytes(16 * 1024 * 1024)
```

> **漏写它就起不来。** 启动期 fail-closed 比对的是 `MultipartSpec.max_total_bytes`
> 与 `AxumTransportConfig.max_body_bytes`，而 `MultipartSpec` 的默认值是 **32 MiB**，
> 必将超过配置上限 → **进程拒绝启动**。§4.9 已写明，但这条在两个 Action 上各欠一次，
> 别只在 probe 上设。

**处理流程**（与 `pull.rs` 逐段对应，**只有第 2 步不同**）：

| # | 步骤 | 来源 |
|---|---|---|
| 1 | 校验数据源存在、`status = active`、`ingest_mode = "xlsx_import"` | 复用 |
| 2 | **逐文件嗅探 + 解析**（替换 `list_all_records`） | **新增** |
| 3 | 按**每条绑定**的 `field_id`（=列名）取值 → `derive_options(...)` | 复用 `extract_values_owned` 的形状 |
| 4 | **逐绑定**摘要比对，未变且无已停用行则跳过 | 复用 `snapshot_digest` 逻辑 |
| 5 | **逐绑定各一个事务**（A12）：跨源预检 → 整行替换 → 补集停用 → 审计 → 更新绑定状态 | 复用 `option_write.rs` |

第 2 步的细节：

1. 逐文件嗅探魔数 `PK\x03\x04`（`allowed_content_types` 只是客户端自称）。
2. `calamine` 流式读**第一张 sheet**，**按表头名解析列**（不按位置）。
   需要的列名 = **该数据源下每条启用绑定的 `field_id`**。
3. 缺任一启用绑定的列 → **整个请求被拒**，错误里写明缺哪一列（D9）。
   未勾选的列一律忽略（含 `地区名称` 这种整列为空的列）。
4. 逐文件产出 `RawValue { ancestors, label }`（`derive.rs` 的当前形状，§4.3），
   多个文件**按顺序拼接**成一份快照。
5. 逐绑定派生、逐绑定落库；**一个绑定写坏不拖垮其他绑定**（A12）。

**为什么逐绑定事务不破坏「整份替换」**：不同绑定是各自独立的 `source_key`
（各自独立的审批控件），彼此本来就不需要原子可见。这与 `pull.rs` 的既有粒度一致。

**执行方式：同步（A7）**。理由与代价见 §5.8。

### 5.5 列的身份与父子关系

- **一个 xlsx 数据集 = 一个数据源**（A10）。银行用例因此从 09-23 版的
  「两个数据源」塌成**一个数据源、两条绑定**：`开户行行名`（无父）+ `联行号`（父 = 行名）。
  **一次导入搞定，落库时间减半**，且两条绑定共享同一份已解析快照。
- **列的身份 = 表头名**（trim 后精确匹配），存在绑定的 **`field_id`** 上（A9）。

  **为什么这么做是安全的**：全仓对 `field_id` **零格式校验**（只查非空 + 集合内唯一），
  中文列名不会被挡；把它当「多维表格字段 ID」用的唯一地方是 pull 的
  `resolve_current_field_names`，而那条链路被 `ingest_mode == "pull"` 门控，
  xlsx 源**永远进不去**。出站、级联、父键解析全都按 `(datasource_id, field_id)` 定位，
  所以复用它不需要改任何读路径。

  **但有两件必须显式做的事，不是「零改动」**：

  1. **`field_name` 必须显式写入，与 `field_id` 同值。** 建绑定的既有路径
     （`create_datasource_table` / `update_datasource_table`）**从不写 `field_name`**
     ——`FieldBindingInput` 里根本没有这个字段；全仓唯一写它的是 `pull.rs` 的
     `persist_binding`（每轮拉取后回写），而那只服务 `pull`。
     而 `approval_provision` 对**每一条启用绑定**都执行 `require("field_name")`，
     列可为空 → **xlsx 绑定会恒为 NULL，导致审批外部选项装配整批失败**
     （那个函数扫全表，失败范围不限于本数据源）。
     **所以建源时必须把列名同时写进 `field_id` 与 `field_name`。**
  2. **`field_id` 是 `max_length(64)`**，且应用层不校验长度——超过 64 字符的列名
     会在插库时失败。银行夹具的表头都是 3–5 字，碰不到；但通用导入必须**在探表头阶段
     就挡住超长列名**并给出明确报错（§5.7），而不是让它到插库时才炸。

- **父子关系在同源内**（A11），即 `parent_field_id` 指向本数据源的另一条绑定
  ——这正是 `load_parent_source_key` 的现状约束。所以级联是**数据源内部的**，
  用户配父列时下拉里只有同源已勾选的列（前端 §5.6）。
- **`source_key` 另有字符集约束**：ASCII `[a-z0-9_]`、1–64 字节。它是**全局唯一**的
  出站路由键，所以**不能直接用中文列名**——向导第 3 步必须为每列单独派生一个 ASCII 键
  （§5.6）。这与多维表格向导第 4 步是同一件事。
- **代价要讲明：改 xlsx 的列名 = 要重配绑定。** 严格口径下会「缺列即拒」。
  这是「列名即身份」的必然结果。若上游确实要改列名，用户需要重新走一次向导
  （或让运维直接改绑定行的 `field_id` 与 `field_name`）。

### 5.6 前端向导

新建 `frontend/src/features/feishu/components/XlsxImportWizard.tsx`，
登记进 `features/registry.ts` 静态注册表（**禁止按后端字符串动态 import**）。

```text
第 1 步  填名称 + 选文件        → POST …/xlsx/probe → 拿到列名
         （File[] 存进向导 state，后续步骤复用同一批 File）
第 2 步  勾选要作为外部选项的列   （纯前端）
第 3 步  给每列配 source_key + 父列
         （父列下拉 = 同源内已勾选的其他列；对应 §5.5）
第 4 步  创建数据源(JSON) → 导入(multipart，重传同一批 File) → 渲染回执
```

**第二步入口：重新导入（D4 说「会重新导」，所以它必须存在）。**
向导每次都从「新建数据源」开始，而整体替换的常规用法是**对已存在的数据源再传一批文件**。
所以还需要一条**独立的再导入路径**：数据源详情页（§5.11）给一个「重新导入」按钮，
选中文件后**直接打导入 Action**，不再走建源与配对——因为绑定已经配好了，
表头一致即可（这正是 §5.7 严格口径能成立的前提）。

> **⚠️ 已知未做：向导中途刷新不留痕（2026-09-29 终审裁定）。**
> 本节曾写着「建源后立刻把 `datasource_id` 记进 URL/路由 state，让刷新落回详情页」，
> **从未实现，也从未派成任务**。终审在此把它改成显式的「已知未做」，而不是留一句看起来
> 像已交付的设计：本轮不改，理由有两条。
>
> 1. **代价与收益不成比例**。这条要动的是列表页的**状态载体**：`useListQuery` 刻意把
>    搜索/筛选/排序/分页放在 React state + localStorage（见 `frontend/src/features/feishu/
>    list-query.ts` 的模块文档），而「刷新后还记得刚建了哪条源」只能靠 URL/路由 state——
>    等于给这个页面新引入一套与既有状态并存的载体，还要定义它**什么时候清掉**
>    （导入成功之后？下次打开向导？），否则那句「别再建一条」会一直挂着说假话。
> 2. **残余风险是可恢复的、且已经响亮**：建源成功而导入失败时向导**不回滚**、也不关对话框，
>    原地写明「数据源已建好（#N），但导入失败……重试只会重发导入这一步」；真刷新之后
>    重走一遍会撞 `title` 重名（`create_datasource_table` 没有幂等键），那是一次**明确的失败**，
>    不是静默的重复建源——用户回列表页就能看见那条零选项的源。
>
> 要补的话，正确的入口是给建源加幂等键，而不是在前端记一个会过期的 id。

**文件生命周期（D11）**：第 1 步的文件只用于探表头，服务端读完即丢；
前端把 `File` 对象**留在向导 state 里**，第 4 步重新 POST 同一批文件执行导入。
**用户只选一次文件**（除非刷新页面）。这样服务端**零暂存**——没有暂存目录、
没有票据、没有 TTL 清理、没有孤儿文件回收，完全贴合框架的 `RequestScoped` 契约。

> **⚠️ 第 3、4 步共用同一张可编辑的绑定表（2026-09-28 实施期裁定）。**
> 上面的步骤清单是散文；实施时以**可执行的用例**为准，而用例是在**进到第 4 步之后**才操作
> 父列下拉、并断言该改动进了提交物的。所以落地形态是：
> **第 3 步 = 那张绑定表；第 4 步 = 同一张表 + 导入计划 + 提交按钮。**
> 代价认了：3→4 的「下一步」看起来像空操作，且与兄弟向导 `DatasourceTableWizard`
> （第 3 步选字段、第 4 步配绑定）的分工不同。
>
> **由此推出一条必须守的规则：一旦数据源已创建（`createTable` 成功），配置即冻结、文件仍可换。**
> 因为提交是「先建源再导入」两个请求，且**第 2 个失败时第 1 个不回滚**——此时数据源已经存在，
> 用户若还能改勾选/源标识/父列，那些编辑**发不出去**（重试只重发导入），
> 界面上却看着是生效的。**这是用户无法从屏幕自查的那类失效**，所以必须把表冻结并在 UI 上说明原因。
> 而**文件仍要允许更换**：第一次导入失败后换一个文件重试是合理需求（原文件可能本身有问题）。

**现有向导一行不改**（D12）。两条流程步骤差异大（文件上传 vs Base Token 拉表、
表头 vs 字段列表、xlsx 无视图无坐标），塞进同一个 565 行的组件会让它明显更臃肿。
（顺带纠正 09-23 版的猜测：`DatasourceTableWizard` **不在**架构门禁的管辖范围内
——前端门禁只管 `TableView.tsx` 的行数与 hook 边界、`routes.tsx`、`registry.ts`。
所以不拆的理由是**可读性**，不是门禁。理由变了，结论没变。）

**新向导的入口**：`DatasourceListPage.tsx` 的「新建」处加一个二选一
（**多维表格** / **xlsx 文件**），分别打开两个向导。现在那一处直接渲染
`DatasourceTableWizard`，所以入口是明确的。

**除新组件外还要动的地方**：

1. **`features/registry.ts` 不用动。** 它是**自定义 View** 的注册表
   （键是后端 view id，如 `demo.items.insight` / `access.groups.list_groups`），
   向导不是 View。09-23 版按「新页面就登记 registry」想当然写了，实际不需要。
2. `features/feishu/types.ts`：
   - `IngestMode` 与 `INGEST_MODE_OPTIONS` 加第三项；
   - **`syncHealth` 要加 xlsx 分支**——它现在对「非 pull」一律返回
     「由多维表格推送…靠自动化工作流推送选项」，会把 xlsx 源**描述成多维表格推送的**，
     与事实相反。xlsx 源应显示「文件导入」与最近导入时间（§5.11）。
3. `views/DatasourceDetailPage.tsx`：「视图」那一列现在按 `ingestMode === "pull"` 二分，
   xlsx 会落到「—」。要么显示「—（文件导入）」，要么换成「最近导入」。
4. `features/feishu/api.ts`：`createDatasourceTable` 现在把 `ingest_mode`
   **硬编码为 `"pull"`**，必须让它可传入。
5. `frontend/contracts/feishu-projections.json`：手工改 `ingest_mode` 取值域（§5.2 第 4 处）。
6. 重新生成 OpenAPI 快照与 TS 类型（`python scripts/dump_openapi.py`），
   让两个新 Action 的 `multipart` 契约投影到 `ui-catalog` 与 `api-types`。

### 5.7 异常行与校验规则

**校验规则（A14：只留机械可判定的，不猜业务语义）**

| 类别 | 规则 | 处置 |
|---|---|---|
| 不入库 | 取数列值为空（trim 后） | `derive_options` 本来就跳过 |
| 不入库 | `option_id` 重复 | `derive_options` 本来就按 `option_id` 去重 |
| **标异常** | 取数列值超过 `label` 的 255 上限 | 截断到 255 + `enabled = false` + 原因 |

原因写 `feishu_option.extra`（`Text` 存 JSON，既有列）：`{"anomaly": "…"}`。

**超长值截断后 `option_id` 按哪个值算？** 按**截断后的 255 字符**算。
理由：`option_id` 必须由落库的 `label` 算出，否则「id 与 label 对不上」——而
「改文案 = 新 id」是补集停用语义的地基。代价要认：两个前 255 字相同的长值会碰撞成同一个
`option_id`，后者被 `derive_options` 去重掉。**这是刻意的取舍**：宁可少一个选项并留下
`extra` 里的异常记录，也不要一个 id 与内容不符的行。（银行夹具里本数据是 0 行。）

**单元格类型怎么转字符串？** 一律按 `calamine` 给出的**原始单元格**取文本，
**不做数值/日期推断**。这一点必须明说，因为通用导入会遇到数值列：
`联行号` 若在某份文件里被 Excel 存成数值，读出来会丢前导零、大数还会变科学计数法
（`1.23E+13`）——**那是源文件的错，不是导入器的错**。导入器不做「看起来像数字就补零」
这类猜测（与 A14 同一立场）。**银行夹具恰好全是文本单元格，所以掩盖了这个问题**；
真实使用中如果遇到，属于数据治理问题（把该列在源文件里设为文本格式）。
**要不要在探表头时把每列的类型码一并带出**（对标 `list_bitable_fields` 会带类型码），
见 §8。

### 5.7.1 逐绑定空快照守卫（**必须新增，`pull.rs` 没有**）

`pull.rs` 的空快照守卫是**表级**的：它只看「这一轮拉到 0 行」。
但 xlsx 导入会遇到一种它挡不住的情形：

```text
整表有 15 万行（守卫不触发）
  └─ 用户勾的某一列 100% 为空（如银行文件的「地区名称」）
       └─ 该绑定派生 0 个选项
            └─ 补集停用把这个绑定现有的选项全部停用 ← 静默清空
```

**这是本次核实发现的设计缺口，必须补上守卫。**

**规则**：**逐绑定**判定——某绑定本轮**派生 0 个选项**，而库里该 `source_key`
**仍有已启用行**时，判为可疑：

- 该绑定**跳过写库**（不替换、**尤其不执行补集停用**）；
- 回执里该绑定带 `skipped_reason`，让调用方看见；
- 其余绑定照常提交（A12 的逐绑定隔离仍然成立）。

这与表级守卫是**同一条道理、同一个粒度错配的修补**：什么时候该相信「真的是空的」，
什么时候该怀疑「是数据出问题了」——宁可不动，也不要静默清空一个正在被审批使用的控件。

**另外**：用户在向导第 2 步勾列时，前端**应该把整列为空的候选列标出来**
（`probe` 只读表头、不知道列是否全空，所以这一条做不了——见 §8 未决项 5）。

> **与 09-23 版的分歧**：那一版硬编码了「联行号必须 12 位数字」。
> 本版**删除该规则**（A14）——导入器已通用化，业务语义规则不进解析器。
> 银行文件里那两行 13/14 位联行号因此**照常入库并启用**。
> 若要挡住它们，那是数据治理问题（§4.6）。

**文件级校验（整份拒绝，不进任何绑定）**

| 情形 | 处置 |
|---|---|
| 魔数不是 `PK\x03\x04` | 拒绝该文件，点名文件名 |
| **文件之间**表头集合不一致 | 拒绝整个请求，点名哪个文件差哪些列 |
| 文件表头缺已配置绑定的列 | 拒绝整个请求，点名缺哪列（D9） |
| 表头有重名 | 拒绝整个请求，点名重名的列 |
| 解析出 0 行且库里有启用行 | **不停用，且整轮失败**（复用 `pull.rs` 的表级空快照守卫，§4.5） |
| 取数列值超过 `field_id` 的 64 字符上限 | **探表头阶段就拒绝**（§5.5 第 2 条）——别等插库时才炸 |
| **某绑定派生 0 个选项而它库里仍有启用行** | 该绑定跳过写库 + 回执告警，**不补集停用**（§5.7.1，`pull.rs` 没有这条） |

> **⚠️ 「多列忽略」与「跨文件表头不一致即拒」不是一回事（2026-09-28 补注）。**
> 上表两行曾被读成矛盾：B 文件比 A 多一列时，它是「多出未勾选的列」（该忽略）还是
> 「文件之间表头集合不一致」（该拒）？**答案是两者各管一层，都成立**：
>
> - **跨文件**：多个文件的表头集合必须**互相完全一致**——它们声明的是「同一份数据集的两半」。
>   不一致就说明用户拿错了文件，整份拒绝。这一条**与配置无关**，纯看文件之间。
> - **单文件 vs 配置**：一个文件的表头**多出**未勾选的列 → 忽略；**缺**了已勾选的列 → 拒绝（D9）。
>
> 也就是说：**单个**文件多一列无害；但**两个**文件一个多一列、一个不多，就会被跨文件那一条拦下。
>
> **两个端点都要守跨文件那一条**，包括导入端点——**因为「重新导入」不经过探表头**
> （详情页直接调导入），只在探表头做校验会漏掉重新导入这条路径。
> 实现是 `xlsx::require_consistent_headers`，探表头与导入共用。

**绑定启用/停用的口径**：需要解析的列名取自**启用的绑定**（停用的绑定不再出数，
把它的列算进「必需列」会让停用一个绑定就导不进来）。但这也带来一个组合要当心：
**子绑定启用、父绑定停用时**，`pull.rs` 的绑定集校验会因「父不在启用集里」而
**让整表失败**（而不是降级成无级联）。导入沿用同一行为——**这是刻意的**：
静默降级成无级联会让子控件回全量，那比失败更难发现（§4.3 的教训）。

**表头行定位**：第一张 sheet 的**第一个非全空行**。回执里带 `header_row`（1-based 物理行号）
供定位。表头行之前的空行忽略。

**多 sheet（D 未定，暂定此行为）**：**只读第一张 sheet**。`probe` 的响应带出全部 sheet 名，
多于一张时前端提示「只读了第一张」。**这是 §8 的未决项之一**——若真实数据固定单 sheet，
这个默认就够；若要多 sheet 支持，那是一个独立改动（多 sheet 的表头可能不同，
要让用户选 sheet，对标多维表格的第 2 步「选视图」）。

**超长行名（180 字符）不算异常**：180 < 255，机械上合法（§4.6）。

### 5.8 为什么选同步（A7、D10）

- 临时文件是**请求作用域**的（§4.9），handler 一返回就删。做异步必须先 `copy_to`
  落盘到持久目录，再起后台任务 + 查进度接口 + 孤儿文件回收——多一套机器，
  且与 D11「服务端零暂存」直接冲突。
- **实测落库速度（旧逐行路径，2026-09-29）**：**409 行/秒** ⇒ 一条重绑定
  （`联行号`，154,386 行）≈ **6.3 分钟**。本设计的银行用例有两条重绑定
  （§7：`开户行行名` 154,362 + `联行号` 154,386），合计 ≈ **13 分钟**——
  把 `request_timeout_seconds` 的校验硬上限 300 s 也打穿（实测触发 `HTTP 408`）。
  > **出处**：速率是 2026-09-29 在运行时**观测一轮在飞导入**得到的（用户本机 dev，
  > Docker MySQL 8.0.46，`log_bin=1`/`sync_binlog=1`/`innodb_flush_log_at_trx_commit=1`，
  > buffer pool 128M）：`T+136 s` 55,081 行、`T+150 s` 61,287 行。被观测的那一轮跑的
  > 正是**旧逐行路径**（批量当时尚未落地，所以「旧逐行路径」这条归因是观测事实，不是推断）。
  > 6.3 分钟/条与 13 分钟都是**按该速率外推**（409 行/秒 × 行数），不是跑完一轮的墙钟；
  > `HTTP 408` 是那次实跑的失败回执。
  > **旧文写的「9.5 s / 12.1 s / 33.4 s、12,000–16,000 行/秒」是错的**：
  > 409 行/秒与那组数差约 **30 倍**；「按 500 行/批」也不成立——那条路径是
  > `option_write::apply_option_rows` **逐行一条语句**（先试 UPDATE、0 行再 INSERT），
  > 500 只是 `where_in` 的分片粒度，语句数与选项数同阶。按旧数做预算会把
  > 「会超时」这件事整个漏掉。
- **解析不是零头**（2026-09-29 实测，debug 构建，两个真实文件 7.4 MB）：
  `read_header` ×2 = **1.6 s**，`read_snapshot` = **18.3 s / 154,386 行**。
  > **出处**：本机 dev 上跑的一次性基准单测（`cargo test`，debug 构建，
  > `xlsx::tests::bench_real_bank_files -- --nocapture`；它是临时量尺，未留在仓库里）
  > 打印的实测值。文件是 `docs/境内银行网点信息管理-{1,2}.xlsx`（压缩后合计
  > 7,398,516 B；100,000 + 54,386 = 154,386 行），输出为
  > `read_header × 2 = 1.6418823s`、`read_snapshot = 18.3086556s`。
  **旧文写的「外加解析 0.9 s」是错的**：按它做预算会漏掉近 20 s。
- **超时不是「数据不变」**（订正）：事务是**逐绑定**提交的（`import_binding`
  各自 `begin_transaction`），超时丢弃的是 handler 的 future，只回滚当时在飞的那一条，
  先落定的绑定留在库里。要整轮原子得把 N 条绑定塞进同一个事务，代价是持锁时间翻倍，
  本设计不取。
- **处置：把逐行 upsert 换成批量（本轮已落地）。** `option_write::apply_option_rows`
  现在走 `upsert_batch_in_tx`：单条多行 ODKU、500 行/批（`yang-db` 的默认批大小），
  语句数从 ≈2×154k 降到 ≈309；解析那 18.3 s 不变。
  **批量后的墙钟尚未实测**——在拿到实测前不许据此宣称「13 分钟 → 30 秒」，
  也不许据此调 `request_timeout_seconds`（合法范围 1..=300）。
- **退出条件（2026-09-29 改按事实写）**：旧文这里是二选一——「把逐行 upsert 换成批量，
  或改异步」。**批量已做**（本轮落地，见上条）；**进度查询已做**（§5.8.1，
  但那是旁路观测，不是异步任务化）。剩下的条件只有一条：**批量后的实测墙钟若仍逼近
  超时上限，就改异步**（先 `copy_to` 落盘 → 后台任务 + 查进度接口）——
  那意味着放弃 D11 的零暂存。

**并发（本次核实补上的缺口）**：同步 + 逐绑定事务 + 单事务写十几万行持锁**数分钟**
（按实测速率外推：旧逐行路径 ≈6.3 分钟/条绑定），组合起来有一个必须挡的风险——**同一数据源的
两次导入并发**（用户重复点「导入」，或两个运维同时操作）。交错执行会让「父绑定已替换、
子绑定还没」这种窗口被拉长，两次补集停用还会互相覆盖。处置：

- **前端**：导入进行中禁用按钮（最基本的一道）。
- **后端**：导入 Action 需**互斥**——以 `datasource_id` 为键加锁，拿不到锁直接返回
  「该数据源正在导入中」。**落地形态是进程内的**（`domain/import_progress.rs` 的
  进程内表 + `ImportGuard` 的 `Drop` 释放；互斥与进度共用这一张表，见 §5.8.1）。
  > ⚠️ **这道锁只在单实例部署下成立。** 多实例（蓝绿双活、横向扩容）时每个进程
  > 一份表，互斥**静默失效**——两次并发导入会各写各的，正是本节要挡的交错。
  > 那时必须换成 Redis 分布式锁，并处理锁超时与续租（代码注释里已写明这条升级路径）。
- **不做**幂等键：导入是「整体替换」，重复执行的结果与执行一次相同，
  真正要防的是**并发交错**，不是重复提交。

> 这一条超出 `pull.rs` 的既有形状（它是单 worker 串行，天然不会自我并发）。
> **导入是人触发的，必然会并发**，所以不能照抄 `pull.rs` 的假设。

### 5.8.1 导入进度（2026-09-29 落地）

**同步仍然成立**：进度不是把导入变成任务，而是给正在跑的那次导入加一路**旁路观测**。
文件仍是请求作用域的（D11：没有落盘暂存、没有票据、没有孤儿回收、没有队列），
导入仍在 handler 里跑完——§5.8 的「改异步」退出条件因此没有被这一步消掉。

- **进度是进程内的**：`domain/import_progress.rs` 一张
  `OnceLock<Mutex<HashMap<datasource_id, ImportProgress>>>`。条目存在 ⟺ 这条源上有导入在跑，
  随 `ImportGuard::Drop` 消失——**互斥与进度共用这一个清理点**，所以超时后不会留下
  一个转圈的假条目。端点 `GET /api/v1/feishu/datasources/{datasource_id}/import-progress`
  只读它：**不查库、恒 200**，`idle` 是正常态而不是 404。权限复用
  `feishu.datasource.read`（不新增授权位、不改目录）。
- **单实例前提（与互斥同一条）**：蓝绿双活时轮询可能落到另一色，那边**恒答 idle**——
  **「查不到 ≠ 没在跑」**。所以 UI 只在「提交中 + idle」显示不确定文案
  （「正在上传文件…」），**绝不显示「没有导入在跑」**，也不拿 idle 当结束信号。
  升级路径与互斥同一条：Redis 分布式锁 + 心跳。
- **超时即条目消失**：`http.request_timeout_seconds` 超时是**丢弃 handler 的 future**
  ⇒ `ImportGuard` 被 drop ⇒ 进度条目随之消失。导入**不许挪进 `tokio::spawn`**
  （future 不再随请求超时被丢弃，会留下没人清的孤儿条目）。
- **上传阶段报不出进度（两条硬事实）**：① multipart body 在 transport 层读完才 dispatch，
  服务端在 handler 之前拿不到任何字节数；② 浏览器 `fetch`**没有上传进度钩子**
  （`XMLHttpRequest.upload` 才有）。所以向导在整个上传段只能显示不确定文案
  「正在上传文件（共 N 个，X.X MB）…」，真正的阶段从服务端开始读文件才算起。
- **计数口径（前端文案按此写，别顺手统一）**：`rows_done` 是**物理**行序号（含空行），
  回执里的 `rows_read` 是**非空**数据行数；`files_done` 是「读完并校验过表头」的文件数；
  `bindings_done` 是「已开始处理」的绑定数，到不了 `bindings_total`；`rows_total` 来自
  calamine 的 `<dimension>`，读不到就是 `null`（**不画假分母**）。

**两条非目标**：不做**上传字节级**进度（要它得换掉 `fetch`，且服务端那段仍要等
transport 收完 body）；不做**异步任务化**（那正是 §5.8 退出条件里的那条路，要放弃
D11 零暂存，本轮不取）。

### 5.9 配置改动

```toml
[http]
max_body_bytes = 16777216          # 1 MiB → 16 MiB
request_timeout_seconds = 60       # 30 → 60
```

**`max_body_bytes` 必须改**：§4.9 的启动期 fail-closed 比对的是**这个值**
（`bootstrap.rs` 用它构造 `AxumTransportConfig`，覆盖掉 lib_yang 的 2 MiB 默认值），
而两个银行文件合计 7.4 MB 已远超 1 MiB。
**16 MiB 是配置校验允许的硬上限**（`src/config/mod.rs` 的 `1..=16777216`）。

**代价（明确接受）**：`max_body_bytes` 是**全局**请求体上限，抬到 16 MiB 会同时放宽
所有其它端点。缓解：multipart 路由自身有 per-route 的 `DefaultBodyLimit`，
且导入接口受权限门控。

**应用边缘 nginx 必须配同等上限**：`frontend/deploy/nginx.conf` 的
`client_max_body_size` 须 ≥ `max_body_bytes`——缺它时 nginx 默认 1 MiB，会在请求
到达后端前直接 413（2026-09-29 线上实测，4.8 MB 的 xlsx 探表头即复现）。
已由 `frontend/scripts/verify-deployment-contract.mjs` 的变异测试机械守卫
（把 16m 变异回 1m 必须被拒绝）。

### 5.10 复合索引（强烈建议；实测后确认**不是**硬前提）

```rust
// 在 option/table.rs 的 TableSpec 上（index_named）
// ⚠️ TableSpec::index_named 收的是 FieldRef 不是 &str 字面量——
// 下面这种 `["source_key", ...]` 是 Table（命令式 builder）的签名。
// 正确形态照抄 demo/notes/table.rs（绑 table_name + field_ref helper）。
.index_named(
    "idx_feishu_option_pick",
    ["source_key", "enabled", "sort_order", "option_id"],
)
```

实测收益与限度见 §4.4.2。**要点**：

- 加索引把端点从 **375–556 ms 降到 126–278 ms**，**是显著优化**；
  但不加索引时它**已经**在 2500 ms 预算内。所以这是**建议项，不是硬前提**。
- 仍然建议做：常规路径快两个数量级；去掉的 filesort 在**并发**下才是真正危险的部分
  （每请求扫 15 万行 × N 并发会迅速逼近预算——此条为推断，未实测并发）；
  成本很低（建索引 5.1 s，每源 +11.5 MB）。
- **`COUNT(*)` 是端点地板价**（125–249 ms/请求），加索引也不改善，
  由框架 `paginate` 无条件触发。本次动不了，属独立议题。
- **施加方式**：schema 同步支持给已有表 `ALTER TABLE ... ADD INDEX`，
  所以这是声明式的一行，不需要 raw SQL、不需要迁移。

**为什么不是「改用独立的表」**：慢的根源是「单源 15.4 万行 + ORDER BY 无索引要 filesort」，
**这与数据放在哪张表无关**。分表只会额外丢掉级联机制并引入出站分流。

> **对现有数据源是纯收益**：`payment_currency` / `payment_fx_rate` 的查询同样会走这个索引，
> 而不是像今天这样（`type=ALL`）全表扫。

### 5.11 导入回执与控制台展示

回执**按绑定分组**。这里要纠正 09-23 版的一处臆造：它说「形状对齐 `PullReport`」，
但**仓库里没有 `PullReport` 这个类型**——现有的是 `pull.rs` 的 `TablePullOutcome`，
它是**表级**的（字段只有 `fetched` / `fields` / `skipped` / `inserted` / `updated` /
`disabled`），既不分绑定、也没有 `source_key` / `derived` / `unchanged` / `anomalies`。
所以**回执形状没有既有类型可对齐，要自己定义**。能对齐的只有一半：`fetched` / `disabled`
这两个词沿用 `TablePullOutcome` 的叫法，其余是本设计新增的。

```json
{
  "datasource_id": 7,
  "elapsed_ms": 13240,
  "files": [{ "name": "境内银行网点信息管理-1.xlsx", "rows_read": 100000 },
            { "name": "境内银行网点信息管理-2.xlsx", "rows_read": 54386 }],
  "bindings": [
    { "source_key": "bank_branch_name", "fetched": 154386, "derived": 154362,
      "disabled": 0, "complement_skipped": 154362, "snapshot_digest": "…",
      "unchanged": false, "skipped_reason": null, "anomalies": [] },
    { "source_key": "bank_branch_code", "fetched": 154386, "derived": 154386,
      "disabled": 0, "complement_skipped": 154362, "snapshot_digest": "…",
      "unchanged": false, "skipped_reason": null, "anomalies": [] }
  ]
}
```

> 上例两条绑定的 `complement_skipped` **都不是 null**，这不是笔误：单轮补集停用的上限是
> 2 万条（`MAX_COMPLEMENT`，与 `pull.rs::find_doomed` 同款），而这份文件有 15 万多个选项
> ——**每一轮的补集停用都会被跳过**。上限本身是既定设计（不在视图完整时做批量停用），
> 但**跳过必须看得见**：`disabled: 0` 在「扫完了没有要停用的」与「根本没扫」之间没有区别，
> 而后者意味着改过名、删掉的旧选项仍然 `enabled = true`，继续被出站喂给飞书控件。
> 所以回执带 `complement_skipped`（非 null = 本轮**没扫**，值 = 当时已启用的选项数），
> 前端两处回执都把它渲染出来。**2026-09-29 终审补**（此前两处各自都对、合起来是静默的）。

（上例的 `anomalies` 都为空，是 A14 的预期结果——银行文件里那几行「脏」数据机械上合法，
见 §4.6。真有超长值时，该字段最多列 100 条并附 `truncated_details: true`。）

- `fetched` 是**读到的数据行数**，`derived` 是**派生出的选项数**。
  两者的差额就是被 `derive_options` 折叠掉的部分（空值与重复 `option_id`）
  ——行名那条绑定上这个差额是 24（154,386 → 154,362），联行号那条是 0。
  **不单列 `deduped` 字段，免得出现两个真相源。**
- `anomalies` **最多列 100 条**；省略时给 `truncated_details: true` 与真实总数
  ——**不做静默截断**。
- 行号是**文件内 1-based 物理行号**（含表头），便于直接去文件里定位。

**`unchanged = true`（跳过写库）时回执怎么填**：`snapshot_digest` 照常返回**本轮算出的**值
（与库里相同，这正是判定依据）；`fetched` / `derived` 照常返回**本轮解析与派生的真实计数**
——跳过的是**写库**，不是解析，所以这些数字是真读到的。`disabled` 返回 0
（跳过分支的前提就是本地没有已停用行）。`anomalies` 照常返回。

**审计**：这里要纠正 09-23 版——它写「导入挂 `ActionLogMiddleware`、探表头不挂」，
但**框架只有 Module 级与 Addon 级两种中间件挂载点，没有 Action 级**
（`app.rs` 是对整个 feishu addon 挂的，框架再把它插进该 addon 的每个 module）。
两个 Action 同处 `datasource` module，**必然被同样记录**，做不到「一条挂一条不挂」。

**处置：接受两条都进审计日志。** 探表头虽然不写库，但它**读了用户上传的文件**，
留痕无害、且对排查「谁在什么时候传了什么」有用。**撤销 09-23 版那条「探表头不挂」的裁定。**

**控制台展示（§5.1 的那条共用点）**：xlsx 源的详情页显示
**最近导入时间 / 各绑定行数 / 异常行数**，**不提供体检按钮**
（xlsx 源按 §4.1 不带多维表格坐标，因此会命中 `health_check` 的
「取数方式不是定时拉取」分支返回 `40905`；语义不动），
并给一个**重新导入**入口（§5.6）。

**审计**：导入与探表头**都挂** `ActionLogMiddleware`——两条同处 `datasource` module，
框架没有 Action 级挂载点，做不到「一条挂一条不挂」（完整裁定见上面那段，
本条是 09-23 版旧说法在此处的残留，2026-09-29 终审清理）。

## 6. 测试与门禁

**必测**（错了会静默出错数据）

**夹具**——先说清一件事：`docs/境内银行网点信息管理-{1,2}.xlsx` **被 `.gitignore` 忽略**
（`.gitignore` 里有 `docs/*.xlsx`），文件在盘上但**不在版本控制里**，
所以 **CI 上不会有这两个文件**，「用真实文件做夹具」必须另行处置：
要么用 `git add -f` 显式纳入（2.6 MB / 4.8 MB，会进仓库历史，需先确认可接受），
要么在测试里**自造小夹具**（几行、含需要的表头与边界列），真实文件只在本地做一次人工验证。
**推荐后者**——单测不该依赖 7.4 MB 的二进制资产。

必测项：

- `xlsx.rs` 用夹具断言：表头名解析（**不按位置**——把列顺序打乱仍能对上）、
  缺列被拒、**文件之间表头不一致被拒**、表头重名被拒、空表头列被跳过、
  魔数不匹配被拒、多 sheet 只读第一张、**超 64 字符列名在探表头阶段被拒**。
- **两级级联端到端**：以行名与联行号两条绑定导入，然后
  ① 不带 `linkage_params` 查子绑定 → 回全量；
  ② 带父的 `option_id` → 只回该父下的子项；
  ③ 带一个**不存在的**父值 → **断言返回 `40004`**（§4.3：解析不出父行时是报错，
  不是静默空集）；再补一条 ④ 父值存在但确无子项 → `code=0` + 空 options（这是**合法**空集）。
- **整体替换**：重新导入后旧数据**完全消失**、新数据**完全就位**。
- **失败不污染其他绑定（不是「全量回滚」）**：让**后一条**绑定的写入失败，
  断言**它自己**回到导入前状态、**前面已提交的绑定保持新值**（A12 的逐绑定粒度）。
  **不要写成「旧数据一行不少」**——那是单事务的期望，与 A12 直接冲突。
  若要「全量回滚」的语义，得把 A12 改成整请求一个事务，那是另一个设计。
- **表级空快照守卫**：解析出 0 行且库里仍有已启用行 → **不停用，且整轮失败**（§4.5）。
- **逐绑定空快照守卫**（§5.7.1，`pull.rs` 没有的新守卫）：
  有一列整列为空、该绑定派生 0 个选项 → **该绑定跳过写库、不补集停用**，
  其余绑定照常提交，且回执带 `skipped_reason`。
- **`field_name` 被写入**（**回归测试，这条不加就会生产事故**）：
  建 xlsx 数据源后，断言每条绑定的 `field_name` 非空且等于列名；
  再加一条端到端——**该数据源存在时审批外部选项装配仍成功**（§5.5 第 1 条）。
- `ingest_mode` 新增取值后：
  - **轮询选表不会选中导入源**（断言 `pull.rs` 的选表查询不命中）；
  - **`create` 与 `update` 两条白名单都接受 `xlsx_import`**（两处都测，别只测一处）；
  - **契约文件 `feishu-projections.json` 的取值域与表声明一致**（§5.2 第 4 处）——
    后端那个 `assert_enum_domains_match` 测试会替你把关，但要确认它**真的跑到了**。
- **探表头无副作用**：调用后库里零变化。**重复调用**亦无副作用。
- **并发互斥**：同一 `datasource_id` 并发两次导入 → 第二次被挡（§5.8）。
- **前端**：`XlsxImportWizard` 的步骤推进与客户端校验要有
  `frontend/tests/` 下镜像路径的 Vitest 用例；`IngestMode` 加第三项后
  `types.ts` 的选项表要有回归断言；**`syncHealth` 对 xlsx 源不再返回「由多维表格推送」**
  也要有断言（§5.6 第 2 条）。

**门禁**

- `python scripts/run_ci.py` 全链，含 `--locked`。
- **新增依赖后必须确认 `cargo +1.80.1 check --locked` 仍通过**（CI 的 `msrv` job 用 `1.80`）；`calamine` 锁 `=0.30.1`；
  不要动 `.cargo/config.toml` 的 fallback resolver。
- 改 `ingest_mode` 选项集、加两个新 Action 后**重新生成 OpenAPI 快照与 TS 类型**
  （`python scripts/dump_openapi.py`）。**注意它不会更新
  `frontend/contracts/feishu-projections.json`**——那个要手工改（§5.2 第 4 处）。
- `python scripts/check_architecture.py`：两个新 Action 各占一个文件
  （恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register`）。
  **注意 `datasource/actions/mod.rs` 不用 `ACTIONS` 数组**——它是逐条 `mod` 声明 +
  `register_all` 里显式调用 `register`；`action_registry!` 那套只在
  `access/grants`、`access/groups`、`account/user`、`demo/notes` 里。
  照 09-23 版说的去找 `ACTIONS` 数组会落空。
- 已实测批量写入不触发任何架构规则（§4.10）。
- 前端 `pnpm --dir frontend check` 全链（含 `verify:locale-contract`，
  新增文案要进单语言产品词条）。

**明确不做**

- 不扩 `examples/frontend_demo/`（除 e2e 需要外）。
- 不做 15 万行规模的 Playwright e2e。

## 7. 风险

| 风险 | 处置 |
|---|---|
| **出站性能落在最坏用例上**：两条绑定共 30.9 万行，无复合索引，每页 filesort，且子绑定多一次父值探测 | §4.8 已有单源估算（350–1100ms，预算 2500ms）。实施后**必须在真实 15 万行上实测并 EXPLAIN**；不够就加复合索引（§5.10） |
| **同步导入打穿超时**（实测速率外推：旧逐行路径一轮约 13 分钟，连 300 s 硬上限都过不去，见 §5.8） | §5.8 已把逐行 upsert 换成批量 ODKU（批量后墙钟待实测）；仍逼近上限再改异步（要放弃 D11 零暂存）。期间用进度端点（§5.8.1）让用户看得见 |
| **缓冲池被挤占**（**唯一被实测证实的风险**）：128 MiB 池子全库共享，两绑定实测 **114.9 MB = 90%** | §5.10 的复合索引把单次请求触达面从「全部数据页」降到「少量索引页」，是主要缓解；**生产环境必须复核 `innodb_buffer_pool_size`**——128 MiB 是 MySQL 默认值，不是刻意选择 |
| `Using filesort` 现在就存在 | §5.10 的复合索引消除它；实施后**复测 EXPLAIN 确认优化器采纳**。注意不加索引端点仍在预算内，所以这是优化不是阻塞 |
| **`COUNT(*)` 的 125–249 ms 是端点地板价** | 本次动不了（框架 `paginate` 无条件触发）。若并发下逼近 2500 ms，需从框架侧解决，属独立议题 |
| 单事务写 15.4 万行持锁**数分钟**（按实测速率外推：旧逐行路径 ≈6.3 分钟/条绑定；批量化后待实测） | 明确接受（单运维、低频）。`pull.rs` 已是这个形状 |
| **漏写 `created_at`/`updated_at` 导致插入失败** | §4.10 陷阱；**用 `option_write.rs` 的 `apply_option_rows`** |
| **multipart 通道在 `src/` 里没有生产消费者**（§4.9） | 这是**新代码路径首次上生产**。测试必须覆盖：魔数嗅探、超限拒绝、临时文件在 handler 返回后确实已删 |
| **`field_name` 漏写 → 审批外部选项装配整批失败**（§5.5，核实发现的 blocking） | 建源时必须写 `field_name`；配回归测试（§6）。**失败范围是全表、不止本数据源**，所以这条一旦漏了影响面很大 |
| **用户在向导里勾了一列整列为空** → 该绑定选项被补集停用静默清空（§5.7.1） | 逐绑定空快照守卫（§5.7.1）。这是 `pull.rs` **没有**的守卫，必须新写 |
| **导入并发交错**（人触发，`pull.rs` 的单 worker 串行假设不成立，§5.8） | 前端禁用按钮 + 后端按 `datasource_id` 互斥 |
| **改列名 = 绑定失配**（§5.5） | 严格口径下「缺列即拒」并明确报错——刻意选的失败模式 |
| **契约文件漏改**：`feishu-projections.json` 不在 `src/` 也不在 `frontend/src/`，且工具链不产出它（§5.2） | 双端测试都会红，属于「会自己报警」的一类，但要写进改动清单免得来回试 |
| **超 64 字符列名**（§5.5 第 2 条） | 探表头阶段就拒；应用层对 `field_id` 零长度校验，不挡就会到插库时才炸 |
| **数值型单元格丢精度**（§5.7） | 明说「不改单元格语义」；真实遇到属数据治理。**银行夹具全是文本，掩盖了这条** |
| `max_body_bytes` 抬到 16 MiB 放宽全局请求体上限 | 明确接受；导入受权限门控，multipart 路由另有 per-route 限制 |
| calamine 版本漂移弄坏 msrv job | 锁 `=0.30.1`；不动 fallback resolver；CI `--locked` |
| **级联的坏法是静默的**（§4.3） | 出站侧不改（`40004` 与那条 `enabled` 守卫一起保住了可分辨性）；导入侧的严格口径（D9）与逐绑定守卫（§5.7.1）是同一个道理的延伸 |
| 权限复用 `feishu.datasource.write`，能改数据源的人就能导入 | 明确接受：导入是「改这个数据源的内容」，与改它的配置同级 |

## 8. 未决项

实现期当场确认，不是设计分叉：

1. **多 sheet 支持**。现定「只读第一张 sheet」（§5.7）。若真实数据要求选 sheet，
   那是对标多维表格第 2 步「选视图」的独立改动——探表头要返回多 sheet 的表头，
   导入要带 `sheet_name`。
2. **路径是否改名**。`/api/v1/feishu/datasources/xlsx/probe` 里的 `xlsx`
   是媒体名而非业务名；若将来支持 CSV，这个段要中性化（如 `tabular`）。
3. **父绑定与子绑定的提交先后**。本设计是**一次请求导入所有绑定**，但逐绑定事务
   意味着父、子的提交有先后，中间态存在「父还是旧数据、子已是新数据」的窗口。
   **09-23 版说这个窗口的表现是「子绑定暂时回全量」——那是错的**：
   读端解析不出父值时会返回 `40004`，不会回退全量（§4.3）。
   所以窗口期的真实表现更可能是**子控件报错**（父值不在旧数据里）或**空集**
   （父值在旧数据里但子项已换新）。**用真机实测这个窗口，再决定是否把父绑定排到
   子绑定之前提交**；若窗口不可接受，另一条路是把整请求合成一个事务（放弃 A12）。
4. **`归属银行` 落不落库**。选项模型只有 取数列 + 父键，没有位置放它。
   若要存，唯一去处是 `extra`（JSON）。当前判断是**不存**——`label` 里已含银行名。
5. **探表头是否要顺带返回行数与列类型**。现在只读表头，所以快。
   两个增强各要代价：返回**行数**要流式扫全表（每个文件 +0.3–0.5s）；
   返回**列类型码**（对标 `list_bitable_fields` 会带类型码）则几乎免费，
   且能提前暴露 §5.7 那个「数值型联行号丢精度」的问题。
   **倾向只加列类型码，不加行数**——但要注意 `probe` 每次都会重新上传整份文件，
   而 `open_workbook` 会整体加载共享字符串表（§4.7），所以它并不像听起来那么便宜。
6. **测试夹具怎么落地**（§6）。真实 xlsx 被 `.gitignore` 忽略、CI 上不存在，
   所以要么 `git add -f` 把 7.4 MB 纳入版本控制，要么自造小夹具。
   **倾向自造**，真实文件只在本地做人工验证。
7. **表头行不在第一行怎么办**。§5.7 定的是「第一个非全空行」。若真实文件前面有标题行
   或合并单元格，探表头会取到标题行，**而用户没有修正的入口**（probe 只返回一行表头）。
   实现期先看真实文件；若确有这种形状，要么让 probe 返回前 N 行候选让用户选，
   要么在请求里带 `header_row` 显式指定。
8. **重新导入入口的落点**（§5.6）。现在定的是「详情页给一个重新导入按钮」，
   但它是否也要出现在列表页的行操作菜单里、以及刷新后的路由落点怎么写，
   属前端实现细节。
9. **`field_name` 是否要允许与 `field_id` 不同**。A9 让 xlsx 绑定的两者都是列名。
   若将来想在控制台上把「上游的列名」与「展示用名」分开，需要加列——本次不做，
   但建源时**把列名写进 `field_name`** 这件事本身是必须的（§5.5 第 1 条）。

## 9. 与其他文档的关系

- 出站链路、取数链路、级联的完整设计见 `docs/architecture/feishu-option-ingest.md`
  与其 `-controls.md` / `-tasklist.md` / `-verification.md`。
- 控制台设计见 `docs/architecture/feishu-datasource-console.md`（前端已按 T5/T7 落地）。
  **本设计落地后，该文档的取数方式说明需要补 `xlsx_import` 一项。**
- 字段绑定层与表级改造的裁定见 `docs/architecture/feishu-datasource-table-config.md`
  与其 `-decisions.md` / `-tasklist.md`。**本设计依赖其 §8.1（祖先链）与 §8（父指针）。**
- 飞书契约、加密、来源校验见 lib_yang 仓库的
  `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。
- 根 `AGENTS.md` 已详述 feishu addon 的三个 module。
  **本设计落地后需要补两处**：`AGENTS.md` 的 addon 说明加一句 xlsx 导入，
  以及新增的前端向导在 `frontend/AGENTS.md` 里的归位。
