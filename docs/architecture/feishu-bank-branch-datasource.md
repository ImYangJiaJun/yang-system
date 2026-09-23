# 飞书银行网点数据源（xlsx 导入 + 两级级联）— 设计

**日期**：2026-09-23（2026-09-21 初稿，按代码现状重写）
**状态**：待评审
**范围**：`project/yang-system`（纯后端）
**前置阅读**：`docs/architecture/feishu-option-ingest.md`、
`docs/architecture/feishu-option-ingest-controls.md`、
`docs/architecture/feishu-datasource-console.md`、
lib_yang 仓库的 `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`

> **重写说明（2026-09-23）**：初稿写在 09-21，当时飞书 addon 只有「多维表格推送」一条链路。
> 之后 12 个提交把架构换掉了：新增了**出站拉取链路**（`domain/pull.rs` + `bitable.rs` +
> `tenant_token.rs`）、**级联过滤**（`domain/linkage.rs`，读端 + 写端）、**取数方式与坐标**
> （`ingest_mode` / `bitable_*` 四列）、控制台前端，并修掉了出站端点的 `page(1,101)` 缺陷。
> 初稿的三个核心结论（新建业务表、加 `source_type` 判别列、出站按类型分流）**全部作废**——
> 它们是在还没有取数管线的架构下写的。本版按现状重写，见 §3.3。

---

## 1. 目标

让飞书外部数据源支持**从 xlsx 文件导入数据**，并用它承载境内银行网点数据
（实测 154,386 行，分两个文件），做到：

1. **新增一种取数方式**：`ingest_mode = "xlsx_import"`，与现有的
   `push`（手工推送）/ `pull`（定时拉取）并列，共用同一套派生与落库管线。
2. **两个数据源、两级级联**：父级 `开户行行名` → 子级 `联行号`，
   让审批人先选网点名、再选（或自动确定）对应的联行号。
3. **整体替换且原子可见**：重新导入即整份替换，飞书侧搜索期间无空窗、不出现半新半旧。
4. **异常行不喂给飞书**：格式不合的行照常入库但置 `enabled = false`，回执里逐条可归因。
5. **归属银行只作展示**，不参与级联（见 §3.1 D2 与 §4.6 的理由）。

## 2. 非目标

- **不做前端。** 控制台已由 T5/T7 建好，但本次导入入口只做后端 multipart Action。
  控制台接入导入 UI 是后续独立任务。
- **不做三级级联。** 明确的取舍，理由见 §3.1 D2 与 §4.6。
- **不做通用「任意 xlsx」导入的字段映射器。** 取数列与祖先列由数据源既有的
  `bitable_field_name` + `linkage_mapping.parent_field` 指定，全部按 **xlsx 表头名**解析。
- **不改 `derive.rs` 的派生规则。** 两级级联是现有规则的原生形态，见 §4.3。
- **不做增量导入。** 与 `pull` 一致，一次导入即一份完整快照。

## 3. 决策记录

### 3.1 产品决策（已确认）

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| D1 | 数据消费方 | 仍是飞书审批外部选项 | 出口不变 |
| D2 | 级联层级 | **两级**：`开户行行名` → `联行号` | §4.6：三级会把 23% 的数据变成不可达 |
| D3 | 归属银行 | **只作展示，不参与级联** | 同上；且它对该列 23% 为空 |
| D4 | 导入生命周期 | 会重新导，整体替换 | 复用 `pull` 的补集停用语义（§4.5） |
| D5 | 导入入口 | 先只做后端接口 | 不碰前端 |
| D6 | 异常行 | 入库但置 `enabled = false`，不喂给飞书 | 复用既有列，无需新列 |
| D7 | 文件数 | 一次请求收 N 个文件（本次 2 个） | 两文件是同一数据集的两半，分次传会互相替换 |

### 3.2 架构决策

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| A1 | 数据放哪 | **不新建表**，落现有 `feishu_option` | §4.1：数据源本就是「对上游一列的投影」 |
| A2 | 如何接入 | **新增 `ingest_mode` 取值**，喂进同一套 `derive_options` | §4.2、§4.4 |
| A3 | 出站路径 | **一行不改**（级联白送） | §4.3：两级是派生规则的原生形态 |
| A4 | 整体替换 | **复用 `pull.rs` 的编排**：摘要比对 → 事务内整行替换 + 补集停用 | §4.5：这套机制已经建好 |
| A5 | 判别列 | **不加** `source_type` | A2 之后没有判别需要 |
| A6 | 解析位置 | 服务端 Rust + calamine 流式 | §4.7 |
| A7 | 执行方式 | 同步（HTTP 请求内完成） | §5.7 |
| A8 | 异常行载体 | `enabled = false` + 原因写 `feishu_option.extra` | 复用既有列 |

### 3.3 初稿中已作废的结论

留档以免有人照着旧版实现：

| 初稿结论 | 现状 | 为什么作废 |
|---|---|---|
| 新建 `feishu_bank_branch` 表 | **作废** | 数据源已是「对上游一列的投影」，无需业务表 |
| `feishu_datasource` 加 `source_type` 判别列 | **作废** | 没有判别需要；`ingest_mode` 已表达取数方式 |
| `approval_options` 按类型分流取数 | **作废** | 出站对称，不区分数据来源 |
| 第 0 步修 `page(1, PAGE_SIZE + 1)` | **已完成** | 见 §4.8 |
| 银行网点专用固定列（联行号/行名/归属银行/…） | **作废** | 选项模型只有 取数列 + 父键，业务列无处安放 |

## 4. 关键事实（逐条核实，带锚点）

**实现时如与之冲突，以本节为准并回头修订。**

### 4.1 数据源是「对上游一列的投影」

`feishu_datasource` 现在已经是一份**取数配置**，而不只是凭据登记（`datasource/table.rs`）：

| 列 | 行号 | 说明 |
|---|---|---|
| `ingest_mode` | :58-64 | `Radio`，现值 `push 手工推送` / `pull 定时拉取`，默认 `push`，`filterable` |
| `bitable_base_token` / `bitable_table_id` / `bitable_view_id` | :66-74 | 多维表格坐标（三个路径段） |
| `bitable_field_name` | :80 | **取数列字段名**——该列文本成为选项的 `label` |
| `linkage_mapping` | :88 | `Text` 存 JSON，级联声明 |
| `last_pull_at` / `last_success_at` / `consecutive_failures` / `last_error` / `snapshot_digest` | :82-86 | 同步状态与快照摘要 |

因此「银行网点」不需要新表：它只是**取数列换成 `联行号` 或 `开户行行名`** 的两个数据源。

### 4.2 管线：拉取 → 派生 → 落库

`pull.rs` 的模块文档（:1）逐字写着编排：
**「选源 → 拉取 → 派生 → 摘要比对 → 事务内落库与补集停用」**。

关键步骤（`pull.rs`）：

1. 选源：`where_eq("ingest_mode", "pull")` + `status = active`（:91、:112）。
   坐标与取数列名缺一不可。
2. 拉取：`list_all_records(...)` 取 `[取数列, 祖先列…]`（:173-181）。
3. 派生：`extract_values_owned(&snapshot, &source.field_name, source.linkage.as_ref())`
   （:184）→ `derive_options(...)`（:192 附近）。
4. 摘要比对：`snapshot_digest(&derived)`（:192），与库里的比对；**内容未变且本地没有
   已停用行时跳过写库**（:221-230）。「没有已停用行」这个附加条件不可省，注释里写了原因。
5. 事务内：跨源预检 → 整行替换 → 补集停用 → 审计 → 更新同步状态（:233 起）。

**这就是导入要复用的全部下半场。** 导入只需要替换第 2 步。

### 4.3 级联：形状、配对与为什么两级刚好

**声明形状**（`linkage.rs:7-10` 是唯一事实源）：

```json
{ "<联动控件字段代码>" | "*": { "parent_source_key": "…", "parent_field": "…" } }
```

`parent_source_key` 是**父数据源**的 `source_key`；`parent_field` 是本数据源里
**承载父文案的列名**。解析入口 `parse_linkage_mapping`（:70）、匹配
`match_linkage`（:114-117），`"*"` 是通配键、精确键优先。

**配对靠同行共现，不去父表反查**（`derive.rs:45-53` 的 `RawValue.parent_label` 注释）：
子数据源从**同一条记录**同时读「取数列」和 `parent_field`，用父文案重算父的 `option_id`。

**`option_id` 的派生规则**（`derive.rs:67-79`）：

```text
无父：{source_key}:{hex(sha256(label))[:12]}
有父：{source_key}:{hex(sha256(parent_key ‖ U+001F ‖ label))[:12]}

parent_option_id(父source_key, 父文案) = option_id_of(父source_key, None, 父文案)
```

**为什么两级恰好可行**：父数据源自己没有父，所以它落库的 id 走「无父」分支
= `{父key}:{hash(行名)[:12]}`；子数据源算父键走 `parent_option_id`（恒传 `None`）
= **同一个式子**。两者相等，配对成立。

**为什么三级不行**（这条在本版被 D2 排除，但必须留档）：中间级自己有父，
其真实 id 是 `{L2}:{hash(L1的id ‖ 行名)[:12]}`，而孙级算出的父键是
`{L2}:{hash(行名)[:12]}`——**永远不相等**。读端 `resolve_parent_key` 会做存在性检查
（`approval_options.rs:170-181`），对不上就返回 `40004`，第三级候选集恒为空。
要支持三级必须把 `RawValue.parent_label` 扩展成祖先链，属独立改动。

**读端过滤**（`approval_options.rs:330-339`）：先取数据源行的 `linkage_mapping`，
再 `resolve_parent_key`，命中则把 `where_eq("parent_key", …)` 挂在**顶层**
（顶层条件隐式 AND，不影响 keyset 游标）。飞书回传值经 `normalize_linkage_value`
（`i18n.rs:37-42`）剥掉 `@i18n@` 前缀。

**失败码**：`40003` 联动键歧义（≥2 个映射键命中）、`40004` 父值无法解析
（存在性检查未通过）。四类「回退到全量」的分支（无 linkage_params / 映射缺失或坏 /
0 个键命中 / 值为空）都有单测。

### 4.4 `feishu_option` 的现状

| 列 | 声明 | 备注 |
|---|---|---|
| `option_id` | `Str` require **unique** max_length 128 | 表级唯一索引 |
| `source_key` | `Str` require indexed filterable sortable | |
| `label` | `Str` require max_length 255 searchable | |
| `parent_key` | `Str` max_length 192 **indexed filterable**（`option/table.rs:65-75`） | 存父的**裸 option_id**；**不能 unique**（一对多）、**不能 require** |
| `sort_order` | `Int` require default 0 sortable **filterable** | filterable 是 keyset 的前提 |
| `is_default` / `enabled` | `Switch` | `enabled` 是出站的过滤位 |
| `i18n` / `extra` | `Text`（JSON 文本） | DSL 没有 Json builder |

索引现状：**只有三个单列索引**（unique(option_id)、index(source_key)、index(parent_key)），
**没有 `index_named(...)` 复合索引**。`sort_order` 是 ORDER BY 首键且参与 keyset 谓词但无索引
→ 每页 filesort。搜索是 `OR LIKE '%kw%'` 前置通配 → 不可索引。

### 4.4.1 现状实测（2026-09-23，本地 `yang_system` 库，只读）

| 项 | 实测值 |
|---|---|
| `feishu_option` **当前行数** | **14**（`payment_currency` 7 + `payment_fx_rate` 7） |
| 数据源 | 3 个：`test`(push) / `payment_currency`(pull) / `payment_fx_rate`(pull) |
| 索引 | `PRIMARY(id)`、`uk_feishu_option_option_id`(unique)、`idx_feishu_option_source_key`、`idx_feishu_option_parent_key` |
| `innodb_buffer_pool_size` | **134217728（128 MiB）**，`innodb_buffer_pool_instances = 1` |
| MySQL | 8.0.46 |

### 4.4.2 基准实测（2026-09-23，真实数据，临时表已 DROP）

**方法**：建一张 `_bench_feishu_option`（DDL 与索引照抄 `feishu_option`），
灌入**真实的两个 xlsx 数据**——父源 `label=开户行行名`（去重后 154,362 行）+
子源 `label=联行号`（154,386 行），共 **308,748 行**，
`option_id` / `parent_key` 用 `derive.rs` 的真实规则算出。外加一个 7 行的
`payment_currency` 用于验证隔离性。然后对同一条 SQL 跑 5 次取中位。
**只建删自己的临时表，未触碰 `feishu_option`。**

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

**四条结论，其中第一条推翻了我先前的判断：**

1. **「表变大 → 别的数据源变慢」被实测证伪。** 表里躺着 30.9 万行时，
   7 行的 `payment_currency` 查询仍是 **1.2 ms**，加不加索引都一样。
   所有查询都带 `source_key = ?`，`idx_feishu_option_source_key` 把它收窄到该源自己的行。
2. **加索引后 filesort 完全消失**（EXPLAIN `Using filesort` → `key=idx_feishu_option_pick`、
   `type=range`、`Using index condition`），常规路径快 **105–163×**。
3. **但索引不是"没有就不可用"**（这条推翻了本版早先写的"必需项"）：
   不加索引时端点实际付出 **375–556 ms**，**已经在 2500 ms 预算内**。
   加索引把它降到 **126–278 ms**。**所以它是显著优化，不是硬前提。**
4. **`COUNT(*)` 才是真正的残余成本**：125–249 ms，且加索引后**几乎不改善**
   （它仍走 `idx_feishu_option_source_key` + `Using where`，要数完匹配的所有行）。
   它由框架的 `paginate` 无条件触发（`read.rs:62`，无跳过开关），**每次请求都要付**。
   端点的地板价就是它。

**体积（实测）**：父+子两源 数据 **47.6 MB** + 索引 **67.3 MB** = **114.9 MB**，
占 128 MiB 缓冲池的 **90%**。复合索引每个源约 +11.5 MB。

**诚实标注一处反常**：罕见词（整条行名）加索引后**慢 1.18×**（204.9 → 241.3 ms）。
原因是索引让它走 `type=range` 顺序扫并逐行回表判 `label LIKE`，
而原来的 filesort 只对**少量匹配行**排序。绝对差 36 ms，可接受，
但它说明**索引不是单调改善**——这正是必须实测而不能推理的地方。

### 4.5 整体替换已经建好：补集停用

`pull.rs` 的补集停用就是本需求的「整体替换」：

- 事务内「整行替换 + 补集停用」（:233、:266-267）。
- **可疑空快照守卫**：拉到 0 行且库里仍有已启用行时，判为可疑、**只记录不停用**
  （:207-215）。否则一次上游故障会把该数据源 100% 的选项静默停掉。
- **跨源预检** `find_foreign_option_owner`：写之前确认这些 option_id 不属于别的源。
- **摘要是按「本轮应当是什么」算的，不含 `enabled`**（:223-226 注释）——所以跳过条件里
  必须带上「本地没有已停用行」，否则被停用的行永远恢复不了。

**导入必须原样复用这四条**，尤其是空快照守卫与跨源预检。

### 4.6 数据实测（两个真实文件）

| 项 | 文件 1 | 文件 2 | 合计 |
|---|---|---|---|
| 字节 | 4,803,614 | 2,594,902 | **7.4 MB** |
| sheet | `境内银行网点信息管理` | 同名 | — |
| 数据行 | 100,000 | 54,386 | **154,386** |
| 序号 | 1–100000 | 100001–154386 | **接续的两半** |

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

后果：**在「归属银行 → 支行 → 联行号」这条路上，这 3.56 万个网点根本到不了**
（没有 L1，进不了任何父级）。要保住它们就必须造一个「其他/未分类」根节点，
而那个根下会有 3.5 万个支行——等于没选父级。

**因此选两级（D2）**：`开户行行名` → `联行号`，全部 154,386 行可达，
且与现有派生规则原生匹配（§4.3），`derive.rs` 一行不用改。

**两级下的实测数字**：

| 项 | 值 |
|---|---|
| 父数据源选项（行名去重） | **154,362** |
| 子数据源选项（联行号） | **154,386** |
| 真正一对多的父级 | **9**（共 32 行） |
| 联行号唯一性 | **全部唯一，零空值，两文件零重叠** |
| 行名长度 | p50=18 / p95=23 / p99=26 / max=180 |
| 归属银行 / 编码 | 各 296 个去重值，**35,611 行为空** |
| 开户行地址 / 地区编码 | 各 341 个去重值，236 空 |
| **地区名称** | **整列 100% 为空** |

**脏数据（4 行）**：

| 序号 | 内容 | 分类 |
|---|---|---|
| 153150 | 行名 = 180 个字符的 `1234567890…` | 超长 → 异常 |
| 153739 | 联行号 = `2223333444555`（13 位） | 非 12 位 → 异常 |
| 153742 | 联行号 = `12345678901234`（14 位） | 非 12 位 → 异常 |
| 某行 | 归属银行编码 = `2Checkout` | **不算异常**（长度合法、非空，只是值不像银行编码） |

### 4.7 xlsx 解析：calamine，MSRV 1.80 是硬门禁

- MSRV 1.80 是**真门禁**：CI 有独立 `msrv` job 在 1.80 上跑
  `cargo check --all-targets --locked`（`.github/workflows/ci.yml:73-98`）。
  实际工具链 1.97.1，但 1.80 那个 job 过不了就是过不了。
- **`calamine` 只能锁 0.30.x**：0.30.1 → MSRV 1.75；0.31–0.35 → 1.83；0.36.1 → 1.88。
  写 `calamine = "=0.30.1"`。**已在 Rust 1.80.0 上实测编译通过**（连带 `zip 4.2.0`）。
- 生效机制是 `.cargo/config.toml:6-7` 的 `resolver.incompatible-rust-versions = "fallback"`；
  移除它会让解析选到 `zip 4.6.x`（需 1.82）从而弄坏 msrv job。
- **实测你的两个文件**（release）：流式 **523ms + 311ms**；内存峰值
  **26.9MB**（流式）对 77.8MB（全量加载），时间几乎相同 → **用流式**。
- 流式 API：`Xlsx::worksheet_cells_reader(name)` 返回 `XlsxCellReader`，
  是**手拉游标、不实现 `Iterator`**，需 `while let Some(c) = reader.next_cell()?` 驱动。
  `use calamine::Reader as _;` 必须在作用域内（`sheet_names` 等是 trait 方法）。
- `open_workbook` 会**先整体加载共享字符串表**，是内存下限，0.30.1 无法推迟。
- 新增约 11 个包；其余（`flate2`、`encoding_rs`、`chrono`、`indexmap` 等）已在锁文件里。
- yang-system 被 `lib_yang/Cargo.toml:5` 的 `exclude` 排除在工作区外，**没有 cargo-deny 门禁**
  （`deny.toml` 只在框架仓库）——新增依赖的许可证审查是**人工责任**；
  上述 crates 的许可证均落在框架 allowlist 内。

### 4.8 出站端点：缺陷已修，但有性能边界

- **`page(1, PAGE_SIZE + 1)` 缺陷已修复**：现在是 `approval_options.rs:371`
  `query.page(1, PAGE_SIZE)?`，`hasMore` 由 `page.total` 推出
  （:380-381 `next_page_token(&rows, total)`），并加了**编译期断言**
  `const _: () = assert!(PAGE_SIZE <= MAX_TABLE_QUERY_PAGE_SIZE);`（:48）。
  初稿的「第 0 步前置修复」**已完成**，本设计不再包含。
- 但**框架仍然拒绝而非截断**超限 page_size（`filters.rs:500-512`），
  且 `PAGE_SIZE = 100` 正好等于硬上限——所以「多取一行判 hasMore」在结构上不可能。
- `COUNT(*)` **仍无条件执行**（`read.rs:62`，无跳过开关），且 COUNT 与 SELECT 是
  **两条独立的 autocommit 语句**，并发写入下可能看到不同快照——这正是 `has_more`
  只能由 `nextPageToken` 承载的原因。
- **无复合索引**，`sort_order` 无索引 → 每页 filesort；搜索前置通配 → 不可索引。
- `feishu-option-ingest.md` §4.8 已估算：单个 `source_key` 持有 154k 行时
  COUNT 150–500ms + SELECT/filesort 200–600ms，**在 2500ms 预算内**。
- **本设计正好落在这个最坏用例上**：父 154,362 + 子 154,386 = 约 30.9 万行，
  且子源每次请求还要多一次父值存在性探测查询（`resolve_parent_key`）。

### 4.9 multipart 上传

- 与现有 feishu Action 同构：`ActionFnBuilder` 上的 `.multipart(MultipartSpec::new([...]))`
  （`definition/interface.rs:221-224`），可与 `.route()/.permissions()/.register()` 链在一起。
- handler Input 里放 `Vec<UploadedFile>`。`UploadedFile` 无 `bytes()`/`reader()`，
  **只能经 `path()` 自己读**（`action/upload.rs:23-88`）。
- **临时文件是请求作用域的**：handler 返回即清理（成功与失败路径都有测试钉死，
  `transport_axum.rs:1810,1831`）。`UploadLifecycle` 只有 `RequestScoped`。
  **必须在 handler 内用完** —— 这也是选同步（A7）的原因之一。
- `allowed_content_types` 与客户端自称 MIME **精确匹配**（`axum.rs:705-732`）。
  Windows 上 `.xlsx` 映射为
  `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`，可过。
  **但这是客户端自称**：`media.rs:48-49` 明确它不能替代内容校验，
  **handler 必须自行嗅探 `PK\x03\x04`**。
- **启动期 fail-closed**（`axum.rs:262-272`）：任一 multipart Action 的 `max_total_bytes`
  大于 `AxumTransportConfig.max_body_bytes` → **进程拒绝启动**。
  `MultipartSpec` 默认 `max_total_bytes = 32 MiB`，而配置上限 16 MiB
  → **不显式设小就起不来**。
- 认证与 JSON Action 一致：去掉 `.public()`、模块挂 `with_authentication`、声明 `.permissions([...])`。

### 4.10 批量写入与事务

- 可达：`yang_db::QueryBuilder::from_pool(&pool, &TableRef::new(name)?).insert_batch(&rows)`。
  约束：列集一致、`INSERT_BATCH_SIZE = 500`、`MAX_BIND_PARAMS = 65535`、
  **所有分片在同一事务内**。
- 事务：`ActionContext::begin_transaction()`（`action/context.rs:470-476`），
  `pull.rs` 已在用。`Transaction::table(&TableRef) -> QueryBuilder` 支持 `insert_batch`。
- **陷阱**：`created_at` / `updated_at` 是 `.required().not_writable()`，DDL 渲染为
  `BIGINT NOT NULL` **无默认值**。`insert_in_tx` 靠 `prepare_and_validate_insert` 补盖，
  **裸 `insert_batch` 绕过它，必须自己写这两个字段**，否则 MySQL 严格模式报
  `Field 'created_at' doesn't have a default value`。
- **但**：`pull.rs` 的 `apply_option_rows`（`domain/option_write.rs`）已经把这件事做对了
  ——这正是导入该复用它而不是自己拼 `insert_batch` 的理由。
- `scripts/check_architecture.py` 的批量写入规则**不触发**：该正则只在
  `tenant_code_boundaries` 内生效，而它遍历的 `src/addon/{org,work}` 不存在。
  **本设计不需要任何 raw SQL，也不需要门禁豁免。**

### 4.11 action 注册与 `presentation` 无关

`compile_runtime_modules` 在 `presentation` 为 `None` 时 `continue`（`compile.rs:343-345`），
**但** handler 是**独立一轮**注册的（`compile.rs:714-750`），遍历
`addons → modules → action_pairs()`，**没有 presentation 守卫**；
而 `catalog.actions` 正是从 `registry.handlers` 投影（`registry.rs:171-175`）。

**结论**：新 Action 即使不声明 `presentation()`/`view()`，仍会进 `catalog.actions`、
**权限仍可授予、接口仍可达**。导入 Action 挂在既有 `datasource` module 上，
不需要新 module，也就不涉及这一问题。

## 5. 设计

### 5.1 接入方式：第三种 `ingest_mode`

```rust
ingest_mode => Radio::<String>::new()
    .title("取数方式")
    .require(true)
    .varchar(16)
    .options([("push", "手工推送"), ("pull", "定时拉取"), ("xlsx_import", "文件导入")])
    .default("push")
    .filterable(true),
```

**这是一行改动。** 轮询选源用的是 `where_eq("ingest_mode", "pull")`（`pull.rs:112`），
所以 `xlsx_import` 的数据源**不会被定时任务碰**——正是想要的行为。

> **注意**：`ingest_mode` 是 `Radio`，改选项集会让 schema 同步走 `MODIFY COLUMN`。
> `plan.rs:164-184` 允许 `FieldType::Enum` 在 `char|varchar|enum` 之间自动改，
> 且该列是 `varchar(16)` 而非原生 ENUM，所以是安全的自增改动。

### 5.2 导入 Action

```text
POST /api/v1/feishu/bank-branches/import   （路径待定，见 §8-1）
权限：feishu.datasource.write             （复用既有权限，不新增）
媒体：multipart/form-data
```

**请求**：文本字段 `source_key`；文件字段 `files: Vec<UploadedFile>`（`max_files = 4`）。

**MultipartSpec**（必须显式设上限，否则启动失败，§4.9）：

```rust
MultipartSpec::new(["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"])
    .max_files(4)
    .max_file_bytes(16 * 1024 * 1024)
    .max_total_bytes(16 * 1024 * 1024)
```

**处理流程**（与 `pull.rs` 逐段对应，只有第 2 步不同）：

| # | 步骤 | 来源 |
|---|---|---|
| 1 | 校验数据源存在、`status = active`、`ingest_mode = "xlsx_import"` | 复用 |
| 2 | **解析 xlsx**（替换 `pull` 的 `list_all_records`） | **新增** |
| 3 | 抽取值 → `derive_options(...)` | 复用 `extract_values_owned` 的形状 |
| 4 | 摘要比对，未变且无已停用行则跳过 | 复用 `snapshot_digest` 逻辑 |
| 5 | 事务内：跨源预检 → 整行替换 → 补集停用 → 审计 → 更新同步状态 | 复用 `option_write.rs` |

第 2 步的细节：

1. 逐文件嗅探魔数 `PK\x03\x04`（`allowed_content_types` 只是客户端自称）。
2. `calamine` 流式读第一张 sheet 的表头，**按表头名解析列**（不按位置）。
   需要的列名来自配置：
   - `bitable_field_name` → **取数列**（本数据源的选项文案来源）
   - `linkage_mapping[..].parent_field` → **父列**（有级联时）
   - 缺任一必需列 → 整个文件被拒，错误里写明缺哪一列。
3. 未知表头忽略。`地区名称` 即使存在也忽略（或按其列名自然落空）。
4. 逐行产出 `RawValue { label, parent_label }`，多个文件**按顺序拼接**成一份快照。

### 5.3 两个数据源与级联配置

银行数据要建**两个数据源**，都 `ingest_mode = "xlsx_import"`，指向同一批 xlsx：

| | 父数据源 | 子数据源 |
|---|---|---|
| `source_key` | 例：`bank_branch_name` | 例：`bank_branch_code` |
| `bitable_field_name`（取数列） | `开户行行名` | `联行号` |
| `linkage_mapping` | 无 | `{ "*": { "parent_source_key": "bank_branch_name", "parent_field": "开户行行名" } }` |
| 派生出的选项数 | **154,362**（行名去重） | **154,386** |

**父子配对为什么成立**（§4.3）：父源无父 → id = `{bank_branch_name}:{hash(行名)[:12]}`；
子源算父键 = `parent_option_id("bank_branch_name", 行名)` = **同一个式子**。

**导入是每个数据源各导一次**（各上传同一批文件）。导入子源**不会**自动刷新父源——
两个数据源是独立的快照。（自动级联刷新是可选增强，见 §8-4。）

**`bitable_*` 坐标列对导入源为空**：它们是多维表格专用。`bitable_field_name` 的**列标题**
本就是「取数列字段名」，语义通用，本次**复用不改名**（改名会牵动拉取链路与前端表单）。

> ⚠️ **一个必须让使用方知道的后果：审批单里落的不是真联行号，是哈希 id。**
>
> 出站契约里 `options[].value` 恒为 `@i18n@<option_id>`（`i18n.rs:18-20`），
> 而 `option_id` 是派生的哈希（§4.3）：
>
> ```text
> 子选项 option_id = bank_branch_code:{sha256(父id ‖ U+001F ‖ 联行号)[:12]}
> 审批单里的值     = @i18n@bank_branch_code:a3f9c2e18b04
> ```
>
> 这是**既有架构的一致行为**（多维表格那条链路今天也是这样），不是本设计引入的。
> 若下游系统需要真联行号，有两条路：
>
> 1. **回查**——按 `option_id` 查 `feishu_option.label`（就是联行号）。零改动。
> 2. **改派生规则**——让导入模式的 `option_id` 用真值。**不推荐**：会按
>    `ingest_mode` 分叉派生规则，而 `derive.rs:31-33` 明确要求改派生口径必须
>    bump `DERIVE_RULE_VERSION`，且「改文案 = 新 id」这条属性会被破坏
>    （真值做 id 时改文案不再产生新 id，补集停用的语义随之改变）。
>
> **本设计的立场是走路线 1。** 若下游确实做不到回查，这是需要重新评审的分叉点。

### 5.4 异常行（D6）

复用 `feishu_option.enabled`：**异常行照常入库，但置 `enabled = false`**，
出站查询本来就 `where_eq("enabled", true)`，所以审批人搜不到；
控制台（已有 `enabled` 筛选）能看到它们。

原因写 `feishu_option.extra`（`Text` 存 JSON，既有列）：`{"anomaly": "联行号不是 12 位数字"}`。

**校验规则**（按**取数列语义**判定，因为同一份数据会经两个数据源各导一次）：

| 类别 | 规则 | 处置 |
|---|---|---|
| 不入库 | 取数列值为空（trim 后） | `derive_options` 本来就跳过（`derive.rs:97-99`） |
| 不入库 | `option_id` 重复 | `derive_options` 本来就按 `option_id` 去重（`derive.rs:113-116`） |
| **标异常** | 取数列值超过 `label` 的 255 上限 | 截断到 255 + `enabled=false` + 原因（本数据 **0 行**） |
| **标异常** | 子源：`联行号` 不是 12 位纯数字 | `enabled=false` + 原因（本数据 **2 行**） |

`归属银行编码 = "2Checkout"` **不算异常**：长度合法、非空，只是值不像银行编码。
同理那行 **180 字符的垃圾行名也不算异常**——180 < 255，机械上合法。
**设计上不猜业务语义——只有可机械判定的规则才用来打标。**
若要把这类「看着像测试数据」的行也挡住，那是**数据治理**问题（在源文件里删掉），
不是导入器的职责；硬编码一个 `>120` 之类的阈值只会让规则变成拍脑袋。

> **对比初稿**：初稿把 180 字符行列为异常、并把 `2Checkout` 列为正常，两者标准不一致。
> 本版统一到「机械可判定」这一条。

> **校验规则落在哪**：本次是银行专用规则，随导入 Action 一并实现。
> 若将来要有第二种导入数据，再抽象成数据源上的一条可选规则（见 §8-2）。

### 5.5 复合索引（强烈建议；实测后确认**不是**硬前提）

**回答「落 `feishu_option` 会不会把查询拖慢」——实测（§4.4.2）之后，三条里只有两条成立。**

| 担心 | 是否成立 | 原因 |
|---|---|---|
| 表变大 → **别的数据源查询变慢** | ❌ **不成立** | 所有查询都带 `source_key = ?`，`idx_feishu_option_source_key` 把它收窄到该源自己的行。`payment_currency` 那 7 个选项仍只扫 7 行，**30 万行银行数据对它是不可见的**。`option_id` 唯一索引变成 30 万条也只是 `O(log n)` |
| 银行源自己的查询慢 | ⚠️ **成立，但没到不可用** | `ORDER BY sort_order, option_id` 无索引 → 每次请求读该源全部行做 filesort。实测端点付出 **375–556 ms**——**在 2500 ms 预算内**。加索引降到 **126–278 ms** |
| **缓冲池被挤占** | ✅ **成立，且这是真风险** | **实测**：父+子两源 数据 47.6 + 索引 67.3 = **114.9 MB，占 128 MiB 池子的 90%**。这才是"表变大"真正的代价——而且是**唯一**被实测证实的代价 |

**建议加这个索引：**

```rust
// 在 option/table.rs 的 TableSpec 上（index_named 见 definition/field.rs:957）
.index_named(
    "idx_feishu_option_pick",
    ["source_key", "enabled", "sort_order", "option_id"],
)
```

**实测它带来的（§4.4.2）：**

1. **filesort 消失**——EXPLAIN 从 `Using filesort` 变成
   `key=idx_feishu_option_pick / type=range / Using index condition`。
   `source_key` 与 `enabled` 两个等值前缀之后，索引序恰好是 `(sort_order, option_id) ASC`。
2. **常规路径快 105–163×**：父源无关键词 246→1.9 ms，子源越页 332→2.3 ms。
3. **端点总付出降到 126–278 ms**（原 375–556 ms）。

**实测它**不**带来的（推翻本版早先的写法）：**

- **`COUNT(*)` 没有改善**（128.9 → 124.5 ms）。优化器仍选 `idx_feishu_option_source_key`
  走 `Using where`，并没有改用新索引做覆盖扫描。**COUNT 是端点的地板价**
  （125–249 ms/请求），由框架 `paginate` 无条件触发（`read.rs:62`），本次改动动不了它。
  **要在预算内进一步优化，得从框架侧想办法，属于独立议题。**
- **不是单调改善**：罕见词（整条行名）反而慢 1.18×（204.9 → 241.3 ms）。
  由于索引让它顺序扫 + 逐行回表判 `LIKE`，而原来的 filesort 只排少量匹配行。
  绝对差 36 ms，可接受。
- **关键词搜索仍不可索引**：`OR LIKE '%kw%'` 前置通配。子源码片段搜索 277→29 ms（快 9×），
  但仍是扫该源范围，不是索引定位。

> **结论修正**：加索引把端点从 375–556 ms 降到 126–278 ms，**是显著优化**；
> 但不加索引时它**已经**在 2500 ms 预算内。所以这是**建议项，不是硬前提**。
> 仍然建议做，理由有三：常规路径快两个数量级；去掉的 filesort 在**并发**下
> 才是真正危险的部分（每请求扫 15 万行 × N 并发会迅速逼近预算——此条为推断，未实测并发）；
> 以及成本很低（建索引 5.1 s，每源 +11.5 MB）。

**为什么不是"改用独立的表"**：初稿的分表方案**救不了这件事**。
慢的根源是「单源 15.4 万行 + ORDER BY 无索引要 filesort」，**这与数据放在哪张表无关**——
换张表，同样是 15.4 万行、同样 filesort、同样占缓冲池。分表只会额外丢掉级联机制
并引入出站分流。**所以答案是加索引，不是分表。**

**施加方式**：schema 同步支持给已有表 `ALTER TABLE ... ADD INDEX`（`plan.rs:330-338`），
所以这是声明式的一行，不需要 raw SQL、不需要迁移。索引名 ≤64 字符
（`idx_feishu_option_pick` 是 23 字符）。

> **对现有数据源是纯收益**：`payment_currency` / `payment_fx_rate` 的查询同样会走这个索引，
> 而不是像今天这样（`type=ALL`）全表扫。

### 5.6 配置改动

```toml
[http]
max_body_bytes = 16777216   # 1 MiB → 16 MiB
```

**必须改**：§4.9 的启动期 fail-closed 要求 multipart 的 `max_total_bytes`
不大于 `max_body_bytes`，而两个文件合计 7.4 MB 已远超 1 MiB。
16 MiB 是配置校验允许的**硬上限**（`config/mod.rs:732-733`）。

**代价（明确接受）**：`max_body_bytes` 是**全局**请求体上限，抬到 16 MiB 会同时放宽
所有其它端点。缓解：multipart 路由自身有 per-route 的 `DefaultBodyLimit`
（`axum.rs:281,306`），且导入接口受权限门控。

### 5.7 为什么选同步（A7）

- 临时文件是**请求作用域**的（§4.9），handler 一返回就删。做异步必须先 `copy_to`
  落盘到持久目录，再起后台任务 + 查进度接口——多一套机器。
- **实测落库速度**：154,386 行按 500 行/批（与 `insert_batch` 同批大小）=
  **9.5 s / 12.1 s / 33.4 s**（三次运行，12,000–16,000 行/秒），
  外加解析 0.9 s。每次导入只落**一个**数据源。
- **中位约 12 s，但观察到的最差一次 33.4 s 已经超过 30 s 超时**。
  该次紧跟在一次被强杀的进程之后（服务端仍在恢复），可能不具代表性——
  但**三次之间 3.5× 的离散度是真实的**，不能按中位数做设计。
- **原子性让超时成为安全的失败**：超时 → handler 被取消 → 事务回滚 → 数据不变，
  可重试。不存在「导了一半」的状态。所以超时的**后果**是可接受的。
- **处置（修订 A7 的落地方式）**：保持同步，但**把 `request_timeout_seconds` 从 30 提到 60**
  作为安全边际（合法范围 1..=300，`config/mod.rs:736`），并在导入回执里返回耗时。
  若实测稳定落在 12 s 以内，这个改动可以不做。
- **真正的退出条件**：连续多次实测，若 p95 逼近超时上限，改异步
  （先 `copy_to` 落盘 → 后台任务 + 查进度接口）。

### 5.8 导入回执

```json
{
  "source_key": "bank_branch_code",
  "fetched": 154386,
  "derived": 154386,
  "disabled": 2,
  "snapshot_digest": "…",
  "unchanged": false,
  "files": [
    {"name": "境内银行网点信息管理-1.xlsx", "rows_read": 100000},
    {"name": "境内银行网点信息管理-2.xlsx", "rows_read": 54386}
  ],
  "anomalies": [
    {"file": "…-2.xlsx", "row": 153739, "value": "2223333444555", "reason": "联行号不是 12 位数字"},
    {"file": "…-2.xlsx", "row": 153742, "value": "12345678901234", "reason": "联行号不是 12 位数字"}
  ],
  "truncated_details": false
}
```

- `fetched` 是**读到的数据行数**，`derived` 是**派生出的选项数**。两者的差额就是被
  `derive_options` 折叠掉的部分（空值与重复 `option_id`）——父数据源上这个差额是 24
  （154,386 → 154,362），子数据源上是 0。**不单列 `deduped` 字段，免得出现两个真相源。**
- 字段名对齐 `pull.rs` 的 `PullReport`（`fetched` / `disabled` 等），便于控制台统一展示。
- `anomalies` **最多列 100 条**；省略时 `truncated_details: true` 并给出真实总数
  ——**不做静默截断**。
- 行号是**文件内 1-based 物理行号**（含表头），便于直接去文件里定位。

## 6. 测试与门禁

**必测**（错了会静默出错数据）

- `xlsx.rs` 用**真实文件**（`docs/境内银行网点信息管理-{1,2}.xlsx`，已在仓库里）做夹具：
  断言 100,000 / 54,386 行、表头名解析、缺列被拒、魔数不匹配被拒。
- **两级级联端到端**：以父源 `source_key` 导入行名、子树导入联行号，然后
  ① 不带 `linkage_params` 查子源 → 回全量；
  ② 带父的 option_id → 只回该父下的子项；
  ③ 带一个**不存在的**父值 → `40004` 而不是空集。
- **整体替换**：重新导入后旧数据**完全消失**、新数据**完全就位**。
- **回滚**：导入失败时旧数据**一行不少**。
- **空快照守卫**：解析出 0 行且库里仍有已启用行 → **不停用**（`pull.rs:207-215` 的等价断言）。
- **异常行**：入库但 `enabled=false`，且不出现在出站结果里。
- `ingest_mode` 新增取值后，**轮询选源不会选中导入源**（断言 `pull.rs:112` 的查询不命中）。

**门禁**

- `python scripts/run_ci.py` 全链，含 `--locked`。
- **新增依赖后必须确认 `cargo +1.80.0 check --locked` 仍通过**；calamine 锁 `=0.30.1`；
  不要动 `.cargo/config.toml` 的 fallback resolver。
- 改 `ingest_mode` 选项集后**重新生成 OpenAPI 快照与 TS 类型**
  （`python scripts/dump_openapi.py`）。
- `scripts/check_architecture.py`：已实测批量写入不触发任何规则（§4.10）。

**明确不做**

- 不加前端、不扩 `examples/frontend_demo/`。
- 不做 15 万行规模的 Playwright e2e。

## 7. 风险

| 风险 | 处置 |
|---|---|
| **出站性能落在最坏用例上**：父 15.4 万 + 子 15.4 万，无复合索引，每页 filesort，且子源多一次父值探测 | §4.8 已有单源估算（COUNT 150–500ms + SELECT/filesort 200–600ms = **350–1100ms**，预算 2500ms）。实施后**必须在真实 15 万行上实测并 EXPLAIN**；不够就加复合索引 `index_named(...)` |
| 同步导入逼近 30s 超时 | §5.7 退出条件：实测；超了改异步或调 `request_timeout_seconds` |
| **缓冲池被挤占**（**唯一被实测证实的风险**）：128 MiB 池子是全库共享，父子两源实测 **114.9 MB = 90%** | §5.5 的复合索引把单次请求触达面从"全部数据页"降到"少量索引页"，是主要缓解；**生产环境必须复核 `innodb_buffer_pool_size`**——128 MiB 是 MySQL 默认值，不是刻意选择 |
| `Using filesort` 现在就存在（§4.4.1 实测，14 行时就有） | §5.5 的复合索引消除它；实施后**复测 EXPLAIN 确认优化器采纳**。注意：不加索引端点仍在预算内（375–556 ms），所以这是优化不是阻塞 |
| **`COUNT(*)` 的 125–249 ms 是端点地板价**，加索引也不改善（实测） | 本次动不了（框架 `paginate` 无条件触发）。若并发下逼近 2500 ms，需从框架侧解决，属独立议题 |
| 单事务写 15.4 万行持锁数秒 | 明确接受（单运维、低频）。`pull.rs` 已是这个形状 |
| **漏写 `created_at`/`updated_at` 导致插入失败** | §4.10 陷阱；**用 `option_write.rs` 的 `apply_option_rows` 而不是自己拼 `insert_batch`** |
| `max_body_bytes` 抬到 16 MiB 放宽全局请求体上限 | 明确接受；导入受权限门控，multipart 路由另有 per-route 限制 |
| calamine 版本漂移弄坏 msrv job | 锁 `=0.30.1`；不动 fallback resolver；CI `--locked` |
| 父子文案必须**逐字符相同**（两端 trim 后），否则配对失败 | 同一个 xlsx 的同一列，天然一致；但**改列名/改数据要两个源一起重导**，否则断链 |
| 显式传了 `linkage_params` 但映射解析不出 → 静默回退全量 | 这是既有行为（§4.3 的四类回退），**白名单式的安全回退**；导入侧不改它 |
| 权限复用 `feishu.datasource.write`，能改数据源的人就能导入 | 明确接受：导入是「改这个数据源的内容」，与改它的配置同级 |

## 8. 未决项

实现期当场确认，不是设计分叉：

1. **Action 路径与归属**：路径用什么（`/api/v1/feishu/bank-branches/import` 是暂定名，
   但它其实是通用的 xlsx 导入）。建议中性路径如
   `/api/v1/feishu/datasources/{source_key}/import`，实施时定。
2. **校验规则要不要抽象成数据源列**。本次硬编码银行规则（D5 选了专用）；
   若将来有第二种导入数据，再加一条「取数列必须匹配」的可选正则，而不是继续堆 if。
3. **父源与子源的导入顺序**。子源先导、父源后导会不会让中间态出现「父值解析不了」
   （`40004`）？理论上会有一段时间子项找不到父。建议**父源先导**，并在文档里写明。
4. **导入后是否自动刷新级联的父源**。当前设计是各导各的；自动刷新更省事但更魔法。
5. **是否给导入挂审计**。`app.rs` 逐个 addon 挂 `ActionLogMiddleware`，导入是写操作，
   倾向要挂（与 `pull` 一致）。
6. **`归属银行` 落不落库**。选项模型只有 取数列 + 父键，没有位置放它。
   若要存，唯一去处是 `extra`（JSON）。当前判断是**不存**——`label` 里已含银行名。

## 9. 与其他文档的关系

- 出站链路、取数链路、级联的完整设计见 `docs/architecture/feishu-option-ingest.md`
  与其 `-controls.md` / `-tasklist.md` / `-verification.md`。
- 控制台设计见 `docs/architecture/feishu-datasource-console.md`（前端已按 T5/T7 落地）。
  本设计落地后，该文档的取数方式说明需要补 `xlsx_import` 一项。
- 飞书契约、加密、来源校验见 lib_yang 仓库的
  `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。
- 根 `AGENTS.md` 仍**完全没有提到 feishu addon**（成文于 feishu 落地之前），属既有文档债，
  建议单独一次提交补齐。
