# 飞书银行网点数据源（纯库存储 + xlsx 导入）— 设计

**日期**：2026-09-21
**状态**：待评审
**范围**：`project/yang-system`（后端为主，含前端 `catalog` 投影的说明）
**前置阅读**：根 `AGENTS.md`、`docs/architecture/feishu-datasource-console.md`、
`docs/architecture/raw-sql-boundaries.md`、lib_yang 仓库的
`docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`

---

## 1. 目标

飞书外部数据源模块今天只有一种数据来源：**多维表格通过管理 Token 推送选项**
（`upsert_options` / `delete_options`）。本设计新增第二种数据源类型——**纯数据库存储**，
数据由 xlsx 文件导入，不经多维表格：

1. **新增数据源类型** `bank_branch`，与现有 `bitable` 类型并存于同一张
   `feishu_datasource` 注册表，共用 `source_key` 命名空间与同一条出站接口。
2. **xlsx 导入接口**：一次请求上传若干 xlsx（本次数据为 2 个文件、合计 154,386 行），
   解析、校验、入库，并返回逐条可归因的导入回执。
3. **整体替换语义**：重新导入即替换该数据源的全部数据，且替换对飞书侧**原子可见**——
   搜索期间不出现空窗，也不出现半新半旧。
4. **异常行不喂给飞书**：格式不合的行照常入库但打标，审批人搜索不到，回执里可见。

## 2. 非目标

- **不做前端。** 本次只交付后端接口（multipart Action）。控制台页面属于
  `docs/architecture/feishu-datasource-console.md` 的范围，尚未开工。
- **不做通用的「任意 xlsx」导入。** 决策 D5 明确为银行网点专用：按业务语义建固定列。
  换一种数据要再建一张表。
- **不做增量/合并导入。** 决策 D4：重新导入 = 整体替换，不做 upsert 合并。
- **不新增第三种数据源类型**，但取数分流按可扩展的形状实现（见 §5.4）。
- **不改 `option_id` 的全局唯一约束**，也不动 `feishu_option` 的既有语义。

## 3. 决策记录

### 3.1 产品决策（已确认）

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| D1 | 数据消费方 | **仍是飞书审批外部选项** | 出口不变，仍走 `approval_options`，不新建消费通道 |
| D2 | 飞书侧用法 | **打字搜索后选** | 服务端必须支持按关键词搜索；这是 154k 行可行的前提 |
| D3 | 列结构 | **固定少列，两文件同构** | 可以建固定表结构与索引，不需要动态列/EAV |
| D4 | 导入生命周期 | **会重新导，整体替换** | 需要原子切换语义（§5.5） |
| D5 | 通用性 | **银行网点专用** | 按业务语义建固定列；控制台可筛选统计 |
| D6 | 导入入口 | **先只做后端接口** | 不碰前端，交付面最小 |
| D7 | 异常行 | **导入但标记，不喂给飞书** | 脏数据不丢，但审批人搜不到 |

### 3.2 架构决策

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| A1 | 数据放哪 | **新建 `feishu_bank_branch` 表**，不复用 `feishu_option` | §4.1：复用会造成**静默跨源改写** |
| A2 | 类型判别 | `feishu_datasource` 加 `source_type` 列，**必须带默认值** | §4.2：schema 同步不能给已有数据的表加「无默认值的必填列」 |
| A3 | 取数分流 | `approval_options` 按 `source_type` 选表，**但归一成同一个 `OptionRow`**，信封渲染零改动 | 分流只影响「从哪查」，不影响「怎么回」，风险面最小 |
| A4 | 导入执行方式 | **同步**（在 HTTP 请求内完成） | §5.6：原子性使超时成为**安全的失败**；实测解析仅 0.9s，插入预估 5–15s < 30s 超时 |
| A5 | 原子替换机制 | **单事务 DELETE + insert_batch**，靠 InnoDB MVCC 保证可见性 | §5.5：不需要批次号，失败即整体回滚 |
| A6 | 模块是否进 catalog | **新模块不声明 `presentation()` / `view()`** | §4.6：实测 action 注册与 presentation 无关，因此无导航入口但权限可授、接口可达——正合「只做后端」 |
| A7 | xlsx 解析位置 | **服务端 Rust + calamine** | §4.4：浏览器解析会把 154k 行变成约 300 次请求 |

## 4. 关键事实（逐条核实，带锚点）

**实现时如与之冲突，以本节为准并回头修订。** 以下均为阅读源码或实际构建/实测所得，
非推测；推测处已显式标注。

### 4.1 为什么不能复用 `feishu_option`

- `feishu_option.option_id` 是**全表唯一**，与 `source_key` 无关：
  `.unique(true)` 降低为单列 `PendingIndex`（`table/definition.rs:767-773`），
  渲染为 `UNIQUE KEY uk_feishu_option_option_id (option_id)`（`schema_sync/render.rs:98-102`）。
- `upsert_options` 的 UPDATE 条件**不含 `source_key`**（`upsert_options.rs:184`），
  而 `delete_options` 刻意含（`delete_options.rs:102`，注释写明「防止用一个数据源的
  凭证停用另一个数据源的选项」）。两处不对称。
- **后果**：若把 154k 行银行网点与多维表格选项放进同一张表，一旦飞书自动化推来一个
  撞码的 `option_id`，`upsert_options` 会**静默改写银行网点那一行**，无报错、无日志。
  这是「平时不出事、出事查不出来」的缺陷，新建表可让它在结构上不可能发生。
- 附带代价：`feishu_option` 没有异常标记列，也没有批次概念；154k 行混入后
  `list_options`（控制台详情页）要在同表内分页。

### 4.2 schema 同步能做什么、不能做什么

`DatabaseInitializer` 是唯一写入者，无迁移表、无版本号、无回滚 SQL
（`src/infrastructure/schema.rs:1-4`）。

**能**（`plan.rs`）：

- 给已有表 **ADD COLUMN**，可空列或有默认值的列都行——包括非空表上的
  `NOT NULL` **有默认值**列（拒绝条件要求 `default_value.is_none()`，`plan.rs:121-130`）。
- 给已有表 **ADD INDEX / ADD UNIQUE INDEX**（`plan.rs:330-338`）；
  唯一索引受重复数据预检门禁（`preflight.rs:83-105`），有重复会**中止整次同步**。

**不能**：

- 补一个缺失的**自增主键**列——**无条件拒绝，即使表是空的**（`plan.rs:115-120`）。
- 加「必填且无默认值」的列到**已有数据**的表（`plan.rs:121-130`）。
- 改自增标志、改除 String/Enum 之外的类型（`plan.rs:164-184`）。
- **删列**（`schema_validation.rs:104-105`，额外列不属于表定义的所有权范围）。

**推论（A2 的依据）**：`source_type` 必须带默认值（`default("bitable")`），
否则在已有数据的 `feishu_datasource` 上加不上去。

### 4.3 DDL 类型映射（`schema_sync/render.rs:298-374`）

| DSL | MySQL 类型 |
|---|---|
| `Str::new().max_length(N)` | `VARCHAR(N)`，N ∈ 1..=16383 |
| `Str::new()`（无 max_length） | `VARCHAR(255)` |
| `Text::new()` | `TEXT` |
| `Int::new()` | **`BIGINT`**（不是 INT） |
| `Switch::new()` | `TINYINT(1)` |
| `Radio::<String>::new().varchar(N)` | `ENUM(...)` / `VARCHAR(N)` |
| `Timestamp::new()` | `BIGINT` |
| `Key::new()` | `BIGINT AUTO_INCREMENT`，即主键 |

注意：`CREATE TABLE` 的**列顺序是按字段名字母序**，不是声明序（`render.rs:81-86`）。
`TEXT`/`JSON` 列不能有数据库默认值（`render.rs:363-368`）。

### 4.4 xlsx 解析：calamine，且 MSRV 是硬门禁

- MSRV 1.80 是**真门禁**：CI 有独立 `msrv` job 在 1.80 上跑
  `cargo check --all-targets --locked`（`.github/workflows/ci.yml:73-98`）。
  实际构建工具链是 1.97.1，但 1.80 那个 job 过不了就是过不了。
- **`calamine` 只能锁 0.30.x**：0.30.1 → MSRV 1.75；0.31.0–0.35.0 → 1.83；
  0.36.1（最新）→ 1.88。必须写 `calamine = "=0.30.1"`。
- **已在 Rust 1.80.0 上实测编译通过**，连带 `zip 4.2.0` / `quick-xml 0.37.5`。
  起到作用的机制是 `.cargo/config.toml:6-7` 的
  `resolver.incompatible-rust-versions = "fallback"`；该配置若被移除，
  解析会选到 `zip 4.6.x`（需 1.82）从而弄坏 msrv job。
- **实测解析你的两个文件**（release）：流式 **523ms + 311ms**；
  内存峰值 **26.9MB**（流式）对 77.8MB（全量加载），时间几乎相同——**用流式**。
- 流式 API 是 `Xlsx::worksheet_cells_reader(name)` 返回的 `XlsxCellReader`，
  它是**手拉游标**、不实现 `Iterator`，需 `while let Some(c) = reader.next_cell()?` 驱动。
  `use calamine::Reader as _;` 必须在作用域内（`sheet_names`/`worksheet_range` 是 trait 方法）。
- 注意 `open_workbook` 会**先整体加载共享字符串表**，这是内存下限，0.30.1 无法推迟。
- yang-system 有自己的 `Cargo.lock`（被 `lib_yang/Cargo.toml:5` 的 `exclude` 排除在工作区外），
  **没有 cargo-deny 门禁**（`deny.toml` 只在框架仓库）。新增依赖的许可证审查是人工责任；
  上述 crates 的许可证均落在框架 allowlist 内。
- 新增依赖约 11 个包，其余（`flate2`、`encoding_rs`、`chrono`、`indexmap` 等）已在锁文件里。

### 4.5 批量写入与事务

- **可达**：`yang_db::QueryBuilder::from_pool(&pool, &TableRef::new(name)?)`（`yang-db/mod.rs:101`）
  后接 `insert_batch(&rows)`（`yang-db/.../write.rs:275`）。
  `insert_batch` 的约束：列集必须一致、空输入被拒、`INSERT_BATCH_SIZE = 500`、
  `MAX_BIND_PARAMS = 65535`、**所有分片在一个事务内**（`write.rs:14,23,337-420`）。
- `yang_base::table::Record` 是 `#[serde(transparent)]` 的 JSON map（`record.rs:58-63`），
  可直接作为 `insert_batch` 的行类型。
- **事务**：`ActionContext::begin_transaction()`（`action/context.rs:470-476`），
  `upsert_options.rs:202` 已在生产使用。`Transaction::table(&TableRef) -> QueryBuilder`
  （`yang-db/transaction.rs:257`），因此在事务内同样能 `insert_batch`。
- **陷阱（必须处理）**：`created_at` / `updated_at` 声明为 `.required().not_writable()`
  （`definition.rs:196-205`），DDL 渲染成 **`BIGINT NOT NULL` 且无默认值**
  （`render.rs:351-373`）。`insert_in_tx` 之所以能写，是因为
  `prepare_and_validate_insert` 在权限校验**之后**补盖时间戳（`write.rs:156-245`）。
  **裸 `insert_batch` 绕过这个流程，必须自己写入这两个字段**，否则 MySQL 严格模式报
  `Field 'created_at' doesn't have a default value`。
- 同理，裸路径**跳过**：租户注入、逐字段写权限校验、默认值填充、时间戳、`FieldConfig::validate`
  （必填/长度/类型/自定义验证器）。这些要么在导入前自行实现，要么在文档里明确接受跳过。
  注意这些函数都是 `pub(crate)`（`write.rs:156`），**本 crate 调不到**，只能重实现。
- `insert_in_tx`（`write.rs:72`）保留全部校验，但**一次只插一行**——154k 行会产生 154k 条语句，
  不可用。这是必须走 `insert_batch` 的原因。
- 删除：`TableQuery::delete_in_tx(self, tx: &mut Transaction) -> Result<u64, BaseError>`
  （`write.rs:507`），配 `where_eq` 使用。
- **架构门禁不拦批量写入**：批量正则（`scripts/check_architecture.py:49-52`）确实匹配
  `.insert_batch(`，但它只在 `tenant_code_boundaries`（`:362-378`）内生效，而该函数遍历的
  `src/addon/{org,work}` 不存在，函数在 `:337` 提前返回。已实测确认。raw-sql 门禁也不匹配
  （正则要求出现 `sqlx::query` token）。**本设计不需要任何 raw SQL，也不需要门禁豁免。**

### 4.6 action 注册与 presentation 无关（决定 A6）

- `compile_runtime_modules` 在 `presentation` 为 `None` 时 `continue`
  （`compile.rs:343-345`）——模块不进 `RuntimeModule`、不进 `catalog.modules`，因此**无导航入口**。
- **但** handler 的注册是**独立的一轮**，遍历 `addons → modules → module.action_pairs()`，
  **没有任何 presentation 守卫**（`compile.rs:714-750`）。
- 而 `catalog.actions` 正是从 `registry.handlers` 投影出来的
  （`registry.rs:171-175`）。
- **结论**：不声明 `presentation()` / `view()` 的模块，其 Action 仍会进入
  `catalog.actions`、仍可被 `project_permissions` 投影、**权限仍可授予、接口仍可达**。
  这正是「只做后端接口、不进导航」需要的形状。

### 4.7 出站接口的既有缺陷（**必须先修**）

`approval_options.rs:217`：

```rust
let page = query.page(1, PAGE_SIZE + 1)?.paginate_records().await?;
```

- `PAGE_SIZE = 100`（`:34`），所以这里请求 `page(1, 101)`。
- `TableQuery::page` 在 `page_size > MAX_TABLE_QUERY_PAGE_SIZE (100)` 时
  **返回 `ParamInvalid`**（`filters.rs:507-512`，常量见 `query_params.rs:49`）。
- `?` 把错误抛出 `resolve`，`handle` 把任何 `Err` 映射为 `code 50001`（`:112-116`）。
- **没有任何分支能绕过**：不显式调 `page()` 也不行，`paginate` 内部
  `with_effective_pagination()` 会再调一次 `page(page, page_size)?`（`read.rs:41-45`）。
  框架的「有界预取」入口 `prefetch_limit()` 是 `pub(crate)`（`filters.rs:522`），本 crate 调不到。
- **现状**：该端点**永远无法返回成功分页**，飞书审批控件取不到任何选项。
- `hasMore` 只能靠 `PaginatedResult::total`（`query_params.rs:619`）判定——
  而 `count_internal` 本来就在每次 `paginate` 里无条件执行（`read.rs:62`），
  结果却被丢弃。修复顺带把这个白交的 COUNT 变成有用的。

### 4.8 出站查询的 SQL 形状与索引

生成的语句（`approval_options.rs:186-194`）：

```sql
SELECT `option_id`,`label`,`i18n`,`sort_order`,`is_default` FROM `feishu_option`
WHERE `source_key` = ? AND `enabled` = ?
  [AND (`option_id` LIKE ? OR `label` LIKE ?)]     -- query 非空时
  [AND (`sort_order` > ? OR (`sort_order` = ? AND `option_id` > ?))]  -- 翻页时
ORDER BY `sort_order` ASC, `option_id` ASC
LIMIT 100 OFFSET 0
```

- `LIMIT`/`OFFSET` 由 `plan.rs:148-157` 给出，`page=1` 时恒为 `OFFSET 0`。
- 搜索是 `OR LIKE '%kw%'`，**前置通配符用不上索引**；关键词上限 126 字节
  （`MAX_LIKE_PATTERN_LEN = 128`，`validation.rs:256`）。
- 排序需要 `(sort_order, option_id)`，而 `sort_order` 只声明了 `sortable`、**没有索引**
  （`option/table.rs:44-48`）→ 必然 filesort。
- `COUNT(*)` 不可避免：`paginate` 无条件执行（`read.rs:62`），没有跳过开关；
  而 `page(1, 101)` 又不可用，所以「多取一行判 hasMore」在结构上不可能。
- **推测（未实测，中等置信度）**：单 `source_key` 持有 154k 行时，每请求要扫两遍；
  温缓存下 COUNT 约 150–500ms、SELECT+filesort 约 200–600ms。
  合起来在 2500ms 预算内但不宽裕。
- **消除 filesort 的办法**：复合索引 `(source_key, <valid>, sort_order, code)`。
  两个等值前缀之后索引序恰好是 `(sort_order, code) ASC`，与 ORDER BY 一致，
  LIMIT 100 可以在第 100 行停下；COUNT 也变成覆盖索引扫描。
  复合索引在本框架**可声明**：`TableSpec::index_named(...)`（`definition/field.rs:957`）。
  **必须在真实数据上 `EXPLAIN` 验证优化器确实采纳**——推理不能替代实测。

### 4.9 multipart 上传

- 声明方式与现有 feishu Action 同构：`ActionFnBuilder` 上的 `.multipart(MultipartSpec::new([...]))`
  （`definition/interface.rs:221-224`），可与 `.route()/.permissions()/.register()` 链在一起。
- handler 的 Input 结构体里放 `Vec<UploadedFile>` 字段。
  `UploadedFile` 无 `bytes()`/`reader()`，**只能经 `path()` 自己读**
  （`action/upload.rs:23-88`）。
- **临时文件是请求作用域的**：handler 返回即清理，成功与失败路径都有测试钉死
  （`transport_axum.rs:1810,1831`）。`UploadLifecycle` 只有 `RequestScoped` 一个变体。
  因此**必须在 handler 内用完**；这也是 A4 选同步的原因之一。
- `allowed_content_types` 与客户端自称的 MIME **精确匹配**（`axum.rs:705-732`）。
  Windows 上 `.xlsx` 由注册表映射为
  `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`，全小写、无参数，可过。
  **但这是客户端自称**：框架文档明确 `allowed_content_types` 不能替代内容校验
  （`media.rs:48-49`），**handler 必须自行嗅探 ZIP/OOXML 魔数 `PK\x03\x04`**。
- **启动期 fail-closed**（`axum.rs:262-272`）：任一 multipart Action 的 `max_total_bytes`
  大于 `AxumTransportConfig.max_body_bytes` → **进程拒绝启动**：
  `multipart Action {reference} 的 max_total_bytes={} 超过 AxumTransportConfig.max_body_bytes={}`。
  `MultipartSpec` 默认 `max_total_bytes = 32 MiB`，而配置上限是 16 MiB——
  **不显式设小就起不来**。
- 限流发生在读之前：`Content-Length > max_total_bytes` 直接 413（`axum.rs:635-647`）。
- 认证与 JSON Action 完全一致：去掉 `.public()` + 模块挂 `with_authentication` +
  声明 `.permissions([...])` 即可。

### 4.10 数据实测（两个真实文件）

| 项 | 文件 1 | 文件 2 | 合计 |
|---|---|---|---|
| 字节 | 4,803,614 | 2,594,902 | **7.4 MB** |
| sheet 名 | `境内银行网点信息管理` | 同名 | — |
| 行 × 列 | 100,001 × 8 | 54,387 × 8 | — |
| 数据行 | 100,000 | 54,386 | **154,386** |
| 序号区间 | 1–100000 | 100001–154386 | **接续的两半，同一数据集** |

**列剖析**：

| 列 | 空值 | 去重 | 最大长度 | 判读 |
|---|---|---|---|---|
| 序号 | 0 | — | 6 | 连续 1..154386，作排序键 |
| 开户行行名 | 0 | 154,362 | 180（仅 1 行 >100） | p50=18 / p95=23 / p99=26 |
| 归属银行 | **35,611** | 296 | 16 | 23% 缺失 → 可空 |
| 归属银行编码 | **35,611** | 296 | 9 | 与上一列同时为空 |
| **联行号** | **0** | **154,386（全唯一）** | 14 | 154,384 条为 12 位纯数字，无重叠 |
| 开户行地址 | 236 | 341 | 10 | 是「省+市」，非详细地址 |
| 地区名称 | **154,386（整列为空）** | 0 | 0 | **死列，不入表** |
| 地区编码 | 236 | 341 | 5 | 与开户行地址一一对应 |

**脏数据（4 行，混在真实数据中）**：

| 序号 | 内容 |
|---|---|
| 153150 | 开户行行名 = 180 个字符的 `1234567890…` |
| 153739 | 联行号 = `2223333444555`（13 位） |
| 153742 | 联行号 = `12345678901234`（14 位） |
| 某行 | 归属银行编码 = `2Checkout`（支付公司名） |

**联行号全唯一且两文件零重叠**——这使它可以安全地充当飞书契约的 `option_id`。

## 5. 设计

### 5.1 落位

```text
src/addon/feishu/bank_branch/          # 新 module，与 datasource/option 同构
├── mod.rs                             # 装配：不声明 presentation()/view()（A6）
├── table.rs                           # feishu_bank_branch 表声明
└── actions/
    ├── mod.rs
    └── import_branches.rs             # multipart 导入 Action
src/addon/feishu/domain/
├── context.rs                         # FeishuContext 增加第三个 Repository
├── xlsx.rs                            # 新增：calamine 流式读取 + 表头映射
├── branch_import.rs                   # 新增：行校验、异常判定、批量装载
└── protocol.rs                        # 出站信封（不改）
src/addon/feishu/datasource/
└── table.rs                           # 增加 source_type 列
src/addon/feishu/option/actions/
└── approval_options.rs                # 修 page(1,101) + 按类型分流取数
```

`FeishuContext` 目前是固定两字段结构体（`context.rs:17-21`）与固定两入参构造函数
（`:25-35`），加第三个 `Repository` 是机械改动。原因见 `context.rs:14-15` 的注释：
`Registry::dispatch` 只向 Action 注入**所在 module 的主表**，跨表访问必须走该上下文。

### 5.2 数据模型

**`feishu_datasource` 增加一列**（必须带默认值，见 §4.2）：

```rust
source_type => Str::new()
    .title("数据源类型")
    .require(true)
    .max_length(16)
    .default("bitable")
    .filterable(true),
```

取值 `"bitable"`（默认，现有数据全部落到这里）| `"bank_branch"`。

**新表 `feishu_bank_branch`**（列名用英文，`title` 用中文——`every_field_has_an_explicit_chinese_label`
那类断言要求每个字段都有展示名）：

| 列 | 声明 | 说明 |
|---|---|---|
| `id` | `Key::new().title("ID")` | 框架要求的主键 |
| `source_key` | `Str(64)` require + filterable + sortable | 关联数据源 |
| `code` | `Str(32)` require + searchable + filterable + sortable | ← 联行号，映射 `option_id` |
| `name` | `Str(255)` require + searchable | ← 开户行行名，映射 `label` |
| `bank` | `Str(64)` 可空 + filterable + sortable | ← 归属银行（23% 空） |
| `bank_code` | `Str(32)` 可空 + filterable + sortable | ← 归属银行编码 |
| `address` | `Str(64)` 可空 + filterable | ← 开户行地址 |
| `region_code` | `Str(16)` 可空 + filterable + sortable | ← 地区编码 |
| `sort_order` | `Int` require + default(0) + sortable + filterable | ← 序号 |
| `is_valid` | `Switch` require + default(true) + filterable | 异常行标记（D7） |
| `anomaly_reason` | `Str(128)` 可空 | 异常原因 |
| `created_at` / `updated_at` | `Timestamp` created_at / updated_at + sortable | 框架管理 |

`地区名称` 整列为空，**不入表**。

**索引**（必须显式命名——`render.rs:415-419` 要求索引名 ≤64 字符）：

```rust
.index_named("idx_feishu_bank_branch_pick", ["source_key", "is_valid", "sort_order", "code"])
.unique_named("uk_feishu_bank_branch_source_code", ["source_key", "code"])
```

- `pick` 索引让出站查询的 `ORDER BY sort_order, code` 走索引序，消除 filesort，
  并使 `COUNT(*)` 成为覆盖索引扫描（§4.8）。
- `unique` 建在 `(source_key, code)` 而非全局 `code`：允许将来并存多个银行数据集，
  且是导入去重的依据。
- **字段的 `searchable` / `filterable` / `sortable` 是 fail-closed 的**：
  出站查询用到的每一列都必须显式打开对应位，否则 `TableQuery` 直接拒绝。

**为什么不需要 `enabled` 列**：数据源级 `status` 已提供停用能力，而「某个网点关闭」
通过重新导入（整体替换）自然消失。按 YAGNI 不引入逐行启停。

### 5.3 导入接口

```text
POST /api/v1/feishu/bank-branches/import
权限：feishu.bank_branch.write
媒体：multipart/form-data
```

**请求**：文本字段 `source_key`；文件字段 `files`（`Vec<UploadedFile>`，
`max_files = 4`——本次 2 个，留余量）。

**MultipartSpec**（必须显式设上限，否则启动失败，§4.9）：

```rust
MultipartSpec::new(["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"])
    .max_files(4)
    .max_file_bytes(16 * 1024 * 1024)
    .max_total_bytes(16 * 1024 * 1024)
```

**处理流程**：

1. 校验数据源存在且 `source_type == "bank_branch"`，否则 `ParamInvalid`。
2. 逐个文件嗅探魔数 `PK\x03\x04`（`allowed_content_types` 只是客户端自称）。
3. `calamine` 流式读第一张 sheet 的表头，**按表头名匹配列**（不按位置：
   列序可能变）。必需表头 `联行号`、`开户行行名`；缺任一 → 整个文件被拒并说明缺哪一列。
   可选表头：`序号`、`归属银行`、`归属银行编码`、`开户行地址`、`地区编码`。
   未知表头忽略。`地区名称` 即便存在也忽略。
4. 逐行读取 → 校验分类（§5.7）→ 攒入缓冲区。
5. 单事务：`DELETE WHERE source_key = ?` → 分批 `insert_batch`（500 行/批，§4.5）。
6. 返回回执（§5.8）。

**一次请求传 N 个文件是刻意的**：D4 是整体替换语义，若分两次调用，
第二次会替换掉第一次的数据——而你的两个文件是同一数据集的两半。

### 5.4 取数分流

`approval_options` 的 `resolve` 在读出数据源行后增加一步：

```text
读 feishu_datasource（现在多 select 一列 source_type）
  ├─ "bitable"     → 现有查询（feishu_option），只做 §6 的修复
  └─ "bank_branch" → 同样的 where/search/order/keyset 形状，换成
                     .where_eq("source_key", ?)
                     .where_eq("is_valid", true)        // D7：异常行不喂给飞书
                     .search(query)
                     .order_by("sort_order", Asc).order_by("code", Asc)
```

**两条路径归一成同一个 `OptionRow` 列表**，之后的
`build_result_body` / 信封 / 加密 / 错误码**全部零改动**：

| 出站字段 | bitable 来源 | bank_branch 来源 |
|---|---|---|
| `options[].id` | `option_id` | `code` |
| `options[].value` | `@i18n@<option_id>` | `@i18n@<code>` |
| `label` | `label` | `name` |
| `i18n` | `i18n` (JSON) | 空（`BTreeMap::new()`） |
| `is_default` | `is_default` | `false` |
| `sort_order`（游标用） | `sort_order` | `sort_order`（= 序号） |

`build_result_body` 已保证**至少回一个语言**（数据源的 `default_locale`，
缺失翻译回落 label，`i18n.rs:59-86`），所以「无 i18n 的银行网点」天然满足飞书契约，
不需要任何改动。

分流用 `match` 而不是把两张表硬编码进同一个函数体，是为了让第三种类型可以平移接入。

### 5.5 原子替换（A5）

**单事务 `DELETE` + `insert_batch`，靠 InnoDB MVCC 保证可见性。**

- 导入期间并发到达的飞书搜索请求读到的是**旧快照**——不是空集。
  提交的那一刻原子切换到新数据。**没有空窗期，也不需要批次号或指针翻转。**
- 失败（校验失败、解析异常、超时被取消）→ 事务回滚 → **数据完全保持原样**。
- `delete` 与 `insert` 必须在**同一事务**内且 delete 在前：
  唯一键 `(source_key, code)` 保证了顺序敏感。
- 已知代价：154k 行的单事务会持有 undo log 与行锁，持续数秒。
  在只有一个运维人员偶尔导入的前提下可接受。

### 5.6 为什么选同步（A4）

- 临时文件是**请求作用域**的（§4.9），handler 一返回就删。做异步必须先 `copy_to`
  落盘到某个持久目录，再起后台任务 + 一个查进度的接口——多一套机器。
- 实测解析 0.9s；插入预估 5–15s（309 批 × 500 行）；`request_timeout_seconds = 30`。
- **关键在于原子性让超时成为安全的失败**：超时 → handler 被取消 → 事务回滚 →
  数据不变，用户可以重试。不存在「导了一半」的状态。
- **退出条件（写进实施计划）**：实测一次完整导入耗时；若逼近 30s，
  改为异步（先 `copy_to` 再后台任务），或调大 `request_timeout_seconds`
  （合法范围 1..=300，`config/mod.rs:736`）。这是一次可测量的判断，不是设计分叉。

### 5.7 校验与异常行（D7）

**分三类，回执里逐条可归因：**

| 类别 | 规则 | 处置 | 本次数据 |
|---|---|---|---|
| **不入库** | `联行号` 为空 | 跳过（它是必填且构成唯一键） | 0 行 |
| **不入库** | `联行号` 重复（同批内或跨文件） | 保留首条，其余跳过 | 0 行 |
| **入库 + 标异常** | `联行号` 不是 12 位纯数字 | `is_valid=false`，写原因 | 2 行 |
| **入库 + 标异常** | `开户行行名` 为空 | 用 `code` 兜底为 `name`，`is_valid=false` | 0 行 |
| **不入库** | `联行号` 超过 `Str(32)` 上限 | 跳过——**它是唯一键，截断可能造成静默撞码** | 0 行 |
| **入库 + 标异常** | `name` / `bank` / `bank_code` / `address` / `region_code` 超长 | 截断到上限，`is_valid=false`，原因写明截断 | 1 行（180 字符名） |

`归属银行编码 = "2Checkout"` **不算异常**：它长度合法、非空，
只是值看起来不像银行编码。设计上不猜业务语义——只有可机械判定的规则才用来打标。

**异常行照常入库、照常参与 `sort_order` 与游标**，只是出站查询用 `is_valid = true` 过滤掉，
所以审批人搜不到；控制台（将来）能查到并看到原因。

### 5.8 导入回执

```json
{
  "source_key": "bank_branch",
  "imported": 154384,
  "flagged": 3,
  "skipped": 0,
  "files": [
    {"name": "境内银行网点信息管理-1.xlsx", "rows_read": 100000, "imported": 99999},
    {"name": "境内银行网点信息管理-2.xlsx", "rows_read": 54386, "imported": 54385}
  ],
  "flagged_rows": [
    {"file": "…-2.xlsx", "row": 153150, "code": "…", "reason": "开户行行名超过 255 字符，已截断"},
    {"file": "…-2.xlsx", "row": 153739, "code": "2223333444555", "reason": "联行号不是 12 位数字"},
    {"file": "…-2.xlsx", "row": 153742, "code": "12345678901234", "reason": "联行号不是 12 位数字"}
  ],
  "skipped_rows": [],
  "truncated_details": false
}
```

- `flagged_rows` / `skipped_rows` **最多各列 100 条**；被省略时
  `truncated_details: true` 并给出真实总数——**不做静默截断**。
- 行号是**文件内的 1-based 物理行号**（含表头行），便于直接去文件里定位。

### 5.9 配置改动

```toml
[http]
max_body_bytes = 16777216   # 1 MiB → 16 MiB
```

**必须改**，因为 §4.9 的启动期 fail-closed：multipart 的 `max_total_bytes`
不能大于 `max_body_bytes`，而两个文件合计 7.4 MB 已远超 1 MiB。
16 MiB 是配置校验允许的**硬上限**（`config/mod.rs:732-733`）。

**代价（明确接受）**：`max_body_bytes` 是**全局**请求体上限，
抬到 16 MiB 会同时放宽所有其它端点的请求体。缓解措施：
multipart 路由自身有 per-route 的 `DefaultBodyLimit`（`axum.rs:281,306`），
且导入接口受权限门控。

### 5.10 权限

新增 `feishu.bank_branch.write`（格式受 `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$` 约束，
`permission_catalog.rs:14`）。

**首个授权必须由运维 SQL 完成**：没有任何角色默认拿到业务权限
（`grants.rs:19-21` 只给 `user` 角色且权限集为空），且**未在任何 Action 上声明的权限不能授予**
（`permission_catalog.rs:103-115`）。本次导入接口上线后，需要一次 ops SQL 把该权限授给运维账号。

将来控制台要读这张表时，再补 `feishu.bank_branch.read`——本次不声明（YAGNI）。

### 5.11 数据源的创建与删除（本设计新增的接口面）

**必须解决「`bank_branch` 数据源怎么诞生」**——现有 `create_datasource` 不传类型，
新列有默认值，所以照原样调用只会建出 `bitable` 数据源，导入接口永远拿不到合法目标。

**创建**：`CreateDatasourceInput`（`create_datasource.rs:17-41`）增加一个**可选**字段：

```rust
#[serde(default)]
source_type: Option<String>,   // 省略 = "bitable"，保持既有调用方行为不变
```

- 取值域 `"bitable" | "bank_branch"`，与 `status` 同样在 `validate()` 里白名单校验
  （`status` 的既有写法见 `create_datasource.rs:69-74`）。
- **`token` 对 `bank_branch` 同样必填**：出站接口是 public 端点、按数据源 Token 自校验
  （`token.rs:18-44`），所以它和 `bitable` 数据源一样需要 Token。`title` / `default_locale` /
  `encrypt_enabled` 语义完全不变。
- ⚠️ `CreateDatasourceInput` 带 `#[serde(deny_unknown_fields)]`（`create_datasource.rs:17-18`），
  加字段是编译期可见的改动；**需同步重新生成 OpenAPI 快照与 TS 类型**。

**`source_type` 创建后不可改**：

- `UpdateDatasourceInput` **不增加该字段**。理由是它会改变数据实际存放的表——
  一个 `bank_branch` 数据源被改成 `bitable` 后，154k 行会瞬间变成不可达的孤儿数据，
  而出站接口会静默返回空选项（`code 0` + 空 `options`），从控制台看不出是自己造成的。
  这与 `default_locale` 写错导致飞书取不到文案是同一类失败（见
  `feishu-datasource-console.md` §4.6），但后果更大。
- 要换类型就新建一个数据源。

**删除**：`delete_datasource` 现在会「连带停用其下全部选项」并返回 `disabled_options`
计数（`delete_datasource.rs:81-95`）。对 `bank_branch` 数据源，语义要改成**在同一个事务里
删除 `feishu_bank_branch` 中该 `source_key` 的全部行**——154k 行孤儿数据既无用又会持续占空间。
返回体里复用 `disabled_options` 字段承载删除行数（或新增 `deleted_branches` 字段，
实施时二选一并保持 OpenAPI 与 TS 类型同步）。

## 6. 前置修复（第 0 步，独立于本需求）

**`approval_options.rs:217` 的 `page(1, PAGE_SIZE + 1)` 必须改为 `page(1, PAGE_SIZE)`**，
`hasMore` 改用 `page.total`：

```rust
let page = query.page(1, PAGE_SIZE)?.paginate_records().await?;
let has_more = page.total > page.data.len();
let mut rows: Vec<_> = page.data.iter().map(read_row).collect::<Result<_,_>>()?;
```

- keyset 条件下 `total` 恰好是「游标之后还剩多少行」，语义正好。
- `count_internal` 本来就在跑（`read.rs:62`），这个改法**不增加任何查询**。
- **必须补 `resolve` 的单测**：现有测试只覆盖 `verify_source` 与 `read_row`
  （`approval_options.rs:275-408`），`resolve` 零覆盖——这正是这个 bug 能存活至今的原因。
  至少要有一条测试断言「单页返回成功且 `code == 0`」。

> **这一条独立于本需求。** 在修复之前，飞书审批控件走这条链路**取不到任何选项**，
> 无论数据来自多维表格还是银行网点。

## 7. 测试与门禁

**后端**

- `bank_branch/table.rs` 的单测照 `option/table.rs` 的写法：主键、每个字段的
  `searchable`/`filterable`/`sortable` 位、中文展示名不退化、索引名 ≤64 字符。
- `domain/xlsx.rs`：用**真实文件**（`docs/境内银行网点信息管理-{1,2}.xlsx`）
  做解析测试——断言 100,000 / 54,386 行、表头匹配、缺列时被拒、魔数不匹配时被拒。
  这两个文件已在仓库里，可直接作为夹具。
- `branch_import.rs`：逐条覆盖 §5.7 的五个分类，断言 `is_valid` 与 `anomaly_reason`。
- **必须断言的导入行为**（错了会静默出错数据）：
  ① 重新导入后旧数据**完全消失**且新数据**完全就位**（整体替换）；
  ② 导入失败时旧数据**一行不少**（回滚）；
  ③ 异常行入库但不出现在出站结果里。
- `approval_options` 的分流：`source_type` 两种取值各一条端到端用例，
  外加 §6 的 `resolve` 回归测试。
- 集成测试照 `tests/common/` 的既有方式，用真实 MySQL/Redis。

**门禁**

- `python scripts/run_ci.py` 全链，含 `cargo check --locked` 与 msrv job 的等价位。
- **新增依赖后必须确认 `cargo +1.80.0 check --locked` 仍然通过**——
  calamine 锁 `=0.30.1`，且不要动 `.cargo/config.toml` 的 fallback resolver。
- `scripts/check_architecture.py`：已实测批量写入不触发任何规则（§4.5）。
- 改 `source_type` 与新增表后**重新生成 OpenAPI 快照与 TS 类型**
  （`python scripts/dump_openapi.py`）。

**明确不做**

- 不加前端、不扩 `examples/frontend_demo/`。
- 不做 154k 行的端到端 Playwright。

## 8. 风险

| 风险 | 处置 |
|---|---|
| **出站链路当前完全不可用**（§4.7） | 列为第 0 步，先修 + 先补 `resolve` 单测 |
| 同步导入逼近 30s 超时 | §5.6 的退出条件：实测；超了改异步或调 `request_timeout_seconds` |
| 单事务 154k 行持锁数秒 | 明确接受（单运维、低频）。若将来并发导入，改批次号 + 指针翻转 |
| **漏写 `created_at`/`updated_at` 导致插入失败**（§4.5） | 实施时必须显式写入；用真实文件的集成测试会立刻暴露 |
| 复合索引未被优化器采纳，filesort 依旧 | 索引建好后在真实 154k 行上跑 `EXPLAIN` 验证，不靠推理 |
| `max_body_bytes` 抬到 16 MiB 放宽全局请求体上限 | 明确接受；导入接口受权限门控，multipart 路由另有 per-route 限制 |
| calamine 版本漂移弄坏 msrv job | 锁 `=0.30.1`；不动 fallback resolver；CI `--locked` |
| `source_type` 默认值漏设导致启动失败 | §4.2 已写明必须 `default("bitable")`；启动即暴露，不会静默 |
| 将来第二种 xlsx 与银行网点列不同 | 决策 D5 已明确为专用表；换数据要再建表，是已知代价 |
| 导入接口无前端，误用风险高 | 回执逐条可归因；权限独立可撤销 |

## 9. 未决项

以下都是**实现期当场测量或确认**的点，不是设计分叉：

1. **一次完整导入的实测耗时**（决定 §5.6 是否切异步）。
2. **`EXPLAIN` 是否采纳 `idx_feishu_bank_branch_pick`**（§4.8）。
3. **`FeishuContext` 加第三个 Repository 后的构造点**：
   `feishu/mod.rs:32-42` 目前硬编码两份，加第三份是机械改动，但要确认
   `app.rs:113-127` 的 `if let Some(feishu)` 条件装配路径同样覆盖新表。
4. **`approval_options` 的 2.5s 预算在 154k 行下是否达成**——
   实测；若超，先看索引，再考虑去掉那次 `COUNT(*)`（需要框架侧改动，超出本次范围）。
5. **`insert_batch` 分批失败时的错误归因**：`BaseError` 需要转成回执里可读的说明。
6. **是否给导入加一条 `ActionLogMiddleware` 审计**：`app.rs` 逐个 addon 挂，
   导入是写操作，倾向要挂。

## 10. 与其他文档的关系

- 出站接口的飞书契约、加密、来源校验见 lib_yang 仓库的
  `docs/superpowers/specs/2026-09-21-feishu-approval-external-options-design.md`。
- `docs/architecture/feishu-datasource-console.md` 描述的控制台**尚未实现**，
  且其列出的数据源字段里没有 `source_type`。本设计落地后需要在该文档补一节说明
  第二种类型（该文档 §4.2 的 8 个 Action 表格也应增加本设计的导入 Action）。
- 根 `AGENTS.md` 目前**完全没有提到 feishu addon**（成文于 feishu 落地之前），
  已与仓库现状不符。这属于既有文档债，建议单独一次提交补齐，不在本次范围内。
- 首个授权只能由运维 SQL 完成，见 `docs/contracts/AUTHZ_GRANTS.md`。
