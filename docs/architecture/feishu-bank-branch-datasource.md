# 飞书外部数据源（xlsx 文件导入）— 设计

**日期**：2026-09-28（2026-09-21 初稿、2026-09-23 重写、2026-09-28 改为通用导入流程）
**状态**：设计已裁定（2026-09-28 逐节确认），待复核后实施
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
- **不做异步导入与进度查询。** 本次取同步（§5.4、A7）；退出条件写在 §7。
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
| A9 | 列的身份 | **表头名**（trim 后精确匹配），存在绑定的 `field_id` 上 | §5.5：出站与级联全按 `(datasource_id, field_id)` 定位，复用即零改动 |
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
这个字符串**只出现在这两个「断言它已删」的测试里**——那一列连同它的 JSON 解析器
已经**整个删除**（§4.3）。

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
5. **事务内**（**逐字段各一个事务**）：跨源预检 → 整行替换 → 补集停用 → 审计 → 更新绑定状态。

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
> 真机实测 `fldeblar7x` 10/10 好用、`fldm0j5do3` 43 个子项 **0 命中**、`fldtyg5vbz` 4/6。
> 按「三级一律坏」去排查找不到规律。**这条是本设计最该记住的教训：级联的失败是静默空集，
> 不是报错。** 详见 `docs/architecture/feishu-datasource-table-config.md` §8.1。

**读端过滤**：先取绑定行的 `parent_field_id` → `load_parent_source_key` → 命中则把
`where_eq("parent_key", …)` 挂在**顶层**（顶层条件隐式 AND，不影响 keyset 游标）。
飞书回传值经 `normalize_linkage_value` 剥掉 `@i18n@` 前缀。

**失败码**：`40003` 联动键歧义、`40004` 父值无法解析。
四类「回退到全量」的分支（无 `linkage_params` / 映射缺失或坏 / 0 个键命中 / 值为空）都有单测。

> **失败形态已变**：读端后来改成「按 `option_id` 或 `label` 都能解析到父行」，
> 于是对不上时存在性检查会通过，失败**退化成 `code=0` + 空 options 的静默空集**，
> 不再是 `40004`。这也是它长期没被当成 bug 抓出来的原因。

### 4.4 `feishu_option` 的现状与基准实测

| 列 | 声明 | 备注 |
|---|---|---|
| `option_id` | `Str` require **unique** max_length 128 | 表级唯一索引 |
| `source_key` | `Str` require indexed filterable sortable | |
| `label` | `Str` require max_length 255 searchable | |
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
- **可疑空快照守卫**：拉到 0 行且库里仍有已启用行时，判为可疑、**只记录不停用**。
  否则一次上游故障会把该数据源 100% 的选项静默停掉。
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
| 真正一对多的父级 | **9**（共 32 行） |
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
  写 `calamine = "=0.30.1"`。**已在 Rust 1.80.0 上实测编译通过**（连带 `zip 4.2.0`）。
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
- **当前状态**：`calamine` **尚未**加入 `Cargo.toml`（本设计要做的第一件事之一）。

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
- **仓库现状：没有任何 Action 真正用过 multipart。** 唯一的「上传」是头像
  （`account/user/actions/upload_avatar.rs`），它走 **base64 JSON**，不是 multipart。
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
- 全前端**唯一的文件上传入口是头像**（`AccountSettingsPage`），且走 base64 JSON。

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

**唯一要单独裁定的共用点**：`health_check` 对非 `pull` 恒返回 `40905 NOT_CHECKABLE`。
xlsx 源**不走体检**（它没有上游可探），改由详情页展示导入状态（§5.11）。
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

**这是一行改动**，但要同时改**两处白名单**：`create_datasource_table` 与
`update_datasource_table` 都硬编码了 `matches!(mode, "push" | "pull")`，漏一处就会
「向导建得出来、编辑保存不了」。

轮询选表用 `where_eq("ingest_mode", "pull")`，所以 `xlsx_import` 的数据源
**不会被定时任务碰**——正是想要的行为。

> **注意**：`ingest_mode` 是 `Radio`，改选项集会让 schema 同步走 `MODIFY COLUMN`。
> schema 计划器允许 `FieldType::Enum` 在 `char|varchar|enum` 之间自动改，
> 且该列是 `varchar(16)` 而非原生 ENUM，所以是安全的自增改动。

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

**响应**：

```json
{
  "sheet_name": "境内银行网点信息管理",
  "sheets": ["境内银行网点信息管理"],
  "header_row": 1,
  "columns": [{ "name": "开户行行名", "index": 3 }, { "name": "联行号", "index": 5 }],
  "files": [{ "name": "境内银行网点信息管理-1.xlsx" }, { "name": "…-2.xlsx" }]
}
```

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
  出站、级联、父键解析全都按 `(datasource_id, field_id)` 定位，复用它等于零改动；
  xlsx 本来也没有比列名更稳定的身份可用。`field_name` 与 `field_id` 同值。
- **父子关系在同源内**（A11），即 `parent_field_id` 指向本数据源的另一条绑定
  ——这正是 `load_parent_source_key` 的现状约束。所以级联是**数据源内部的**，
  用户配父列时下拉里只有同源已勾选的列（前端 §5.6）。
- **代价要讲明：改 xlsx 的列名 = 要重配绑定。** 严格口径下会「缺列即拒」。
  这是「列名即身份」的必然结果。若上游确实要改列名，用户需要重新走一次向导
  （或让运维直接改绑定行的 `field_id`）。

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

**文件生命周期（D11）**：第 1 步的文件只用于探表头，服务端读完即丢；
前端把 `File` 对象**留在向导 state 里**，第 4 步重新 POST 同一批文件执行导入。
**用户只选一次文件**（除非刷新页面）。这样服务端**零暂存**——没有暂存目录、
没有票据、没有 TTL 清理、没有孤儿文件回收，完全贴合框架的 `RequestScoped` 契约。

**现有向导一行不改**（D12）。两条流程步骤差异大（文件上传 vs Base Token 拉表、
表头 vs 字段列表、xlsx 无视图无坐标），塞进同一个 565 行的组件会让它显著更长，
而它已被架构门禁盯着（TableView 行数 / hook 边界那类规则）。

**除新组件外还要动的地方**：

1. `features/registry.ts` —— 静态登记新向导（禁止动态 import）。
2. `features/feishu/types.ts` 的 `IngestMode` 与 `INGEST_MODE_OPTIONS` 加第三项。
3. 重新生成 OpenAPI 快照与 TS 类型（`python scripts/dump_openapi.py`），
   让两个新 Action 的 `multipart` 契约投影到 `ui-catalog` 与 `api-types`。

> **一处要当心**：`features/feishu/api.ts` 的 `createDatasourceTable` 目前把
> `ingest_mode` **硬编码为 `"pull"`**。走 xlsx 路径时必须让它可传入。

### 5.7 异常行与校验规则

**校验规则（A14：只留机械可判定的，不猜业务语义）**

| 类别 | 规则 | 处置 |
|---|---|---|
| 不入库 | 取数列值为空（trim 后） | `derive_options` 本来就跳过 |
| 不入库 | `option_id` 重复 | `derive_options` 本来就按 `option_id` 去重 |
| **标异常** | 取数列值超过 `label` 的 255 上限 | 截断到 255 + `enabled = false` + 原因 |

原因写 `feishu_option.extra`（`Text` 存 JSON，既有列）：`{"anomaly": "…"}`。

> **与 09-23 版的分歧**：那一版硬编码了「联行号必须 12 位数字」。
> 本版**删除该规则**（A14）——导入器已通用化，业务语义规则不进解析器。
> 银行文件里那两行 13/14 位联行号因此**照常入库并启用**。
> 若要挡住它们，那是数据治理问题（§4.6）。

**文件级校验（整份拒绝，不进任何绑定）**

| 情形 | 处置 |
|---|---|
| 魔数不是 `PK\x03\x04` | 拒绝该文件，点名文件名 |
| 文件之间表头集合不一致 | 拒绝整个请求，点名哪个文件差哪些列 |
| 文件表头缺已配置绑定的列 | 拒绝整个请求，点名缺哪列（D9） |
| 表头有重名 | 拒绝整个请求，点名重名的列 |
| 解析出 0 行且库里有启用行 | **不停用**（复用 `pull.rs` 的空快照守卫，§4.5） |

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
- **实测落库速度**：154,386 行按 500 行/批 = **9.5 s / 12.1 s / 33.4 s**
  （三次运行，12,000–16,000 行/秒），外加解析 0.9 s。
  **中位约 12 s，但最差一次 33.4 s 已超 30 s 默认超时。**
  该次紧跟在一次被强杀的进程之后，可能不具代表性——
  但**三次之间 3.5× 的离散度是真实的**，不能按中位数做设计。
- **原子性让超时成为安全的失败**：超时 → handler 被取消 → 事务回滚 → 数据不变，
  可重试。不存在「导了一半」的状态。
- **处置**：保持同步，但把 `request_timeout_seconds` 从 30 提到 **60** 作为安全边际
  （合法范围 1..=300）。
- **真正的退出条件**：连续多次实测，若 p95 逼近超时上限，改异步
  （先 `copy_to` 落盘 → 后台任务 + 查进度接口）。这也意味着要放弃 D11 的零暂存。

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

### 5.10 复合索引（强烈建议；实测后确认**不是**硬前提）

```rust
// 在 option/table.rs 的 TableSpec 上（index_named）
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

回执按绑定分组，形状对齐 `PullReport` 便于控制台统一展示：

```json
{
  "datasource_id": 7,
  "elapsed_ms": 13240,
  "files": [{ "name": "境内银行网点信息管理-1.xlsx", "rows_read": 100000 },
            { "name": "境内银行网点信息管理-2.xlsx", "rows_read": 54386 }],
  "bindings": [
    { "source_key": "bank_branch_name", "fetched": 154386, "derived": 154362,
      "disabled": 0, "snapshot_digest": "…", "unchanged": false, "anomalies": [] },
    { "source_key": "bank_branch_code", "fetched": 154386, "derived": 154386,
      "disabled": 0, "snapshot_digest": "…", "unchanged": false, "anomalies": [] }
  ]
}
```

（上例的 `anomalies` 都为空，是 A14 的预期结果——银行文件里那几行「脏」数据机械上合法，
见 §4.6。真有超长值时，该字段最多列 100 条并附 `truncated_details: true`。）

- `fetched` 是**读到的数据行数**，`derived` 是**派生出的选项数**。
  两者的差额就是被 `derive_options` 折叠掉的部分（空值与重复 `option_id`）
  ——行名那条绑定上这个差额是 24（154,386 → 154,362），联行号那条是 0。
  **不单列 `deduped` 字段，免得出现两个真相源。**
- `anomalies` **最多列 100 条**；省略时给 `truncated_details: true` 与真实总数
  ——**不做静默截断**。
- 行号是**文件内 1-based 物理行号**（含表头），便于直接去文件里定位。
- 字段名对齐 `PullReport`，便于控制台统一展示。

**控制台展示（§5.1 的那条共用点）**：xlsx 源的详情页显示
**最近导入时间 / 各绑定行数 / 异常行数**，**不提供体检按钮**
（`health_check` 对非 pull 恒返回 `40905`，语义不动）。

**审计**：导入**挂** `ActionLogMiddleware`（与 `pull` 一致，它是写操作）。
探表头**不挂**（不写库）。

## 6. 测试与门禁

**必测**（错了会静默出错数据）

- `xlsx.rs` 用**真实文件**（`docs/境内银行网点信息管理-{1,2}.xlsx`，已在仓库里）做夹具：
  断言 100,000 / 54,386 行、表头名解析（不按位置）、缺列被拒、表头不一致被拒、
  表头重名被拒、魔数不匹配被拒、多 sheet 只读第一张。
- **两级级联端到端**：以行名与联行号两条绑定导入，然后
  ① 不带 `linkage_params` 查子绑定 → 回全量；
  ② 带父的 `option_id` → 只回该父下的子项；
  ③ 带一个**不存在的**父值 → 记下当前失败形态（§4.3 已改为静默空集，**断言要写实况**）。
- **整体替换**：重新导入后旧数据**完全消失**、新数据**完全就位**。
- **回滚**：导入失败时旧数据**一行不少**。
- **空快照守卫**：解析出 0 行且库里仍有已启用行 → **不停用**。
- **逐绑定隔离**：让其中一条绑定的数据坏掉，断言**其他绑定照常提交**（A12）。
- `ingest_mode` 新增取值后，**轮询选表不会选中导入源**（断言 `pull.rs` 的选表查询不命中）。
- **探表头无副作用**：调用后库里零变化。
- **前端**：`XlsxImportWizard` 的步骤推进与客户端校验要有
  `frontend/tests/` 下镜像路径的 Vitest 用例；`IngestMode` 加第三项后
  `types.ts` 的选项表要有回归断言。

**门禁**

- `python scripts/run_ci.py` 全链，含 `--locked`。
- **新增依赖后必须确认 `cargo +1.80.0 check --locked` 仍通过**；`calamine` 锁 `=0.30.1`；
  不要动 `.cargo/config.toml` 的 fallback resolver。
- 改 `ingest_mode` 选项集、加两个新 Action 后**重新生成 OpenAPI 快照与 TS 类型**
  （`python scripts/dump_openapi.py`）。
- `python scripts/check_architecture.py`：两个新 Action 各占一个文件、
  进 `ACTIONS` 数组；已实测批量写入不触发任何规则（§4.10）。
- 前端 `pnpm --dir frontend check` 全链（含 `verify:locale-contract`，
  新增文案要进单语言产品词条）。

**明确不做**

- 不扩 `examples/frontend_demo/`（除 e2e 需要外）。
- 不做 15 万行规模的 Playwright e2e。

## 7. 风险

| 风险 | 处置 |
|---|---|
| **出站性能落在最坏用例上**：两条绑定共 30.9 万行，无复合索引，每页 filesort，且子绑定多一次父值探测 | §4.8 已有单源估算（350–1100ms，预算 2500ms）。实施后**必须在真实 15 万行上实测并 EXPLAIN**；不够就加复合索引（§5.10） |
| 同步导入逼近 60s 超时 | §5.8 退出条件：实测；超了改异步（要放弃 D11 零暂存） |
| **缓冲池被挤占**（**唯一被实测证实的风险**）：128 MiB 池子全库共享，两绑定实测 **114.9 MB = 90%** | §5.10 的复合索引把单次请求触达面从「全部数据页」降到「少量索引页」，是主要缓解；**生产环境必须复核 `innodb_buffer_pool_size`**——128 MiB 是 MySQL 默认值，不是刻意选择 |
| `Using filesort` 现在就存在 | §5.10 的复合索引消除它；实施后**复测 EXPLAIN 确认优化器采纳**。注意不加索引端点仍在预算内，所以这是优化不是阻塞 |
| **`COUNT(*)` 的 125–249 ms 是端点地板价** | 本次动不了（框架 `paginate` 无条件触发）。若并发下逼近 2500 ms，需从框架侧解决，属独立议题 |
| 单事务写 15.4 万行持锁数秒 | 明确接受（单运维、低频）。`pull.rs` 已是这个形状 |
| **漏写 `created_at`/`updated_at` 导致插入失败** | §4.10 陷阱；**用 `option_write.rs` 的 `apply_option_rows`** |
| **multipart 通道在仓库里没有生产消费者**（§4.9） | 这是**新代码路径首次上生产**。测试必须覆盖：魔数嗅探、超限拒绝、临时文件在 handler 返回后确实已删 |
| `max_body_bytes` 抬到 16 MiB 放宽全局请求体上限 | 明确接受；导入受权限门控，multipart 路由另有 per-route 限制 |
| calamine 版本漂移弄坏 msrv job | 锁 `=0.30.1`；不动 fallback resolver；CI `--locked` |
| **改 xlsx 列名 = 绑定失配**（§5.5） | 严格口径下会「缺列即拒」并有明确报错——这是刻意选的失败模式（宁可拒绝，不要静默空集） |
| **级联失败是静默空集**（§4.3 的实测教训） | 出站侧不改；但导入侧的严格口径正是为了不制造这类静默错误 |
| 权限复用 `feishu.datasource.write`，能改数据源的人就能导入 | 明确接受：导入是「改这个数据源的内容」，与改它的配置同级 |

## 8. 未决项

实现期当场确认，不是设计分叉：

1. **多 sheet 支持**。现定「只读第一张 sheet」（§5.7）。若真实数据要求选 sheet，
   那是对标多维表格第 2 步「选视图」的独立改动——探表头要返回多 sheet 的表头，
   导入要带 `sheet_name`。
2. **路径是否改名**。`/api/v1/feishu/datasources/xlsx/probe` 里的 `xlsx`
   是媒体名而非业务名；若将来支持 CSV，这个段要中性化（如 `tabular`）。
3. **父源与子源的导入顺序**。本设计是**一次请求导入所有绑定**，所以不存在这个问题
   ——但**逐绑定事务**意味着同一轮里父绑定与子绑定的提交有先后，
   中间态会出现「子项找得到父」但「父还没提交」的窗口。出站读端对不上父值时的
   行为是**静默回退全量**（§4.3），所以窗口期的表现是「子绑定暂时回全量」。
   **是否需要把父绑定排到子绑定之前提交**，实测后定。
4. **`归属银行` 落不落库**。选项模型只有 取数列 + 父键，没有位置放它。
   若要存，唯一去处是 `extra`（JSON）。当前判断是**不存**——`label` 里已含银行名。
5. **探表头是否要顺带返回行数**。现在只读表头（快）。若前端希望在配置阶段就显示
   「这个大文件有 10 万行」，那要在 probe 里流式扫一遍全表（每个文件 +0.3–0.5s）。
6. **`field_name` 与 `field_id` 同值是否够**。A9 让 xlsx 绑定的两者都是列名。
   若将来要在控制台上区分「上游的列」与「展示名」，需要加列——本次不做。
7. **表头行不在第一行怎么办**。§5.7 定的是「第一个非全空行」。若真实文件前面有标题行
   或合并单元格，探表头会取到标题行，**而用户没有修正的入口**（probe 只返回一行表头）。
   实现期先看真实文件；若确有这种形状，要么让 probe 返回前 N 行候选让用户选，
   要么在请求里带 `header_row` 显式指定。

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
