# 飞书外部选项：数据摄取与级联 — 设计

> 状态：**设计稿，未实现**。**§3.2 的决策已于 2026-09-22 拍板**（见 §9 决策日志）。
>
> 落地前必须先完成 **§6-P-1**（公网入口，阻塞飞书侧）、**§6-P0**（构建修复，阻塞服务端）、
> **§6-V1**（回写环路，可能推翻本设计）。
>
> 本文只描述「选项数据怎么从多维表格进到 `feishu_option`，以及父子级联怎么筛」。
> 审批控件侧的契约由飞书官方《关联外部选项》文档定义，现有实现已符合，本文不重复；对照结论见 §4.3。

## 1. 目标

1. 让营运维护的**飞书多维表格**成为审批表单选项的事实源，改一处即生效，不需要在审批后台重复维护选项。
2. 支持**父子级联筛选**：选了上级控件后，下级控件只列出该上级下的选项。
3. 满足三条来自使用方的硬约束：
   - **不在多维表格里新建表**；
   - **不在多维表格里新建列 / 视图 / 公式**；
   - **尽可能少在多维表格里做配置**。
4. 规模前提：**一张台账表服务全部控件**。当前该表 30 列 / 228 行，后续会补列、行会增长。

## 2. 非目标

- **不做按人过滤**（`user_id` / `employee_id` 维度）。官方把它列为功能优势，实现侧整体缺席。
- **不做按请求 `locale` 协商语言**。文案语言仍由数据源自身的 `default_locale` 与已录入的翻译集合决定。
- **不改取选项端点的对外契约**（`C1`–`C18` 的字面符合性保持不变）。
- **不做审批控件侧的配置自动化**。每个外部选项控件在审批后台配 URL + Token + 可选 Key 是人工动作。

## 3. 决策记录

### 3.1 产品决策（已确认）

- **P1｜多维表格不新增表。**
- **P2｜多维表格不新增列 / 视图 / 公式。**
- **P3｜「哪个字段是谁的父字段」由配置显式声明；配对的具体值从**台账同行共现**读出。**
  即：不存在"从数据猜层级"这回事（那已被实测证伪，见 §4.6-5）；声明的是**列对**，读的是**同行**。
- **P4｜时效按「可承诺、可告警」定义，不追求事件驱动的秒级。**

### 3.2 架构决策（2026-09-22 拍板）

| 编号 | 决策 | 理由 |
|---|---|---|
| **A1** | **轮询是对外状态的唯一正确性来源；通知只能是 best-effort 加速器** | ① 自动化运行次数是**企业级共享额度**，超限后**静默停摆至次月 1 日**；② 补集停用是非单调操作，只在轮询那轮执行；③ 通知端点逐表人工配置、无重试承诺 |
| **A2** | 服务端**主动拉取**（出站读飞书开放平台），不由多维表格推数据 | 推数据方案的配置全落在多维表格侧，违反 §1 第 3 条 |
| **A3** | 数据源登记**多维表格坐标**（base_token / table_id / field_id） | 现有 `feishu_datasource` 字段集是穷尽的，没有坐标 |
| **A4** | 新增**可过滤的父键列**；不复用 `extra`、不依赖 `linkage_mapping` 存值 | `extra` 是 Text 且未开 `filterable`，而 `filterable` 是 fail-closed |
| **A5** | 拉取路径**整行替换**，不复用 `upsert` 的合并契约 | 合并语义 + `enabled` 单向会导致「删了再加回来」的选项永久不可见 |
| **A6** | 补集停用必须在**完整快照**前提下执行 | 分页失败得到真子集，补集会把活选项全停用——主动写错、静默、不自愈 |
| **A7a** | **值域取「记录」**：台账里**实际出现过的值**（去重）。**不取字段选项目录。** | ① 记录口径下**每个父子必有子值**，不会出现"有父无子"的空下拉；② 免掉目录清洗（目录里约 32% 是重名/测试项/PII 垃圾串）；③ 与"同一张台账表服务所有控件"的架构一致 |
| **A7b** | **层级取「同行共现」**：父列与子列在**同一行**上共现 → 得到 (父值, 子值) 边集。**不用主数据 Base。** | 三条已声明的父子关系实测**全部 0 例「一子多父」**（§4.6-3），共现即正确配对；一张表读一次即可，不需要第二个 Base 与跨 Base 授权 |
| **A8** | 通知端点若实现，**必须与写入端点用不同的 Token** | 现有管理 Token 是全局单值，不按数据源绑定 |
| **A9** | 优先级：**先做轮询，通知是纯加法** | 通知不省掉轮询的任何一件活 |
| **A10** | **单实例部署** → 不做跨实例单飞与快照 CAS | 用户确认生产为单实例 |
| **A11** | 汇率选项文案带**生效期**；生效期默认取首见时间，**首轮播种人工指定** | 汇率按月更新且旧记录留痕 → 同一币种会累积多个历史值，需要可分辨（§4.6-4） |

## 4. 关键事实（逐条核实，带锚点）

### 4.1 现状：选项从哪来、写到哪

- 两张自有表：`feishu_datasource`、`feishu_option`。声明在 `datasource/table.rs:13` 与 `option/table.rs:17`。
- 选项的**唯一写入方**是 `POST /api/v1/feishu/inbound/options/upsert`（`upsert_options.rs:146`），需全局管理 Token；
  删除用 `.../options/delete`（`delete_options.rs:80`），语义是置 `enabled=false`。
- 控制台对选项**只读**（`list_options`），这是架构决策（`option/mod.rs:8-11`）。
- `option_id` 有**表级唯一索引**（`option/table.rs:22-29`），另有跨源归属预检，命中返回 `40901`（`upsert_options.rs:184-204`）。
- **本 addon 没有任何 HTTP 客户端，也不持有飞书凭证。**
- **⚠️ 取选项端点从未被飞书真实调用过**：实测 13 张审批定义全部 `hasurl=False`（无任何控件配过外部选项 URL）。

### 4.2 三条候选路线与被淘汰的理由

| 路线 | 内容 | 结论 |
|---|---|---|
| **A** | 多维表格新建规范化表 + 自动化推数据 | **违反 P1**，出局 |
| **B** | 台账原始行直接推 + 服务端放宽同批去重 | **否决**：配置全落在多维表格侧（每数据源 4 个自动化 + 1 个回填流程），且「放宽去重」是伪需求（分片后跨请求无去重状态）；真正的硬墙是跨源 `40901` + 表级唯一索引 |
| **C** | 服务端定时拉取 | **采纳** |

### 4.3 官方契约中与本文相关的条款

来源：《关联外部选项》官方文档（`C1`–`C18`）。

- **C3**：`linkage_params` 是 `Map<String,String>`，key 为联动字段的字段代码，value 为被联动控件值；**不带时请返回所有的 options**。现实现 `protocol.rs:40-43` 是 `#[allow(dead_code)]`，收到即丢。
- **C10**：`options[].id` 与 `options[].value` 都要求**全局唯一且固定**。
- **C15/C16**：配置粒度是**控件**——每个单选/多选控件各填 URL + Token（必填）+ Key（可选）。这是「一个 `source_key` = 一个控件」的依据。
- **C17**：官方明示「配置了联动参数（`linkage_params`）或使用了 V2 版本（`page_token`、`query`），**暂未对开放平台做完整支持**」。本设计同时踩这两个开关，属**已知风险**（§8-U4）。
- **C1–C18 未定义**：联动层级上限；选项缓存的 TTL / 失效策略。

**一条必须显式接受的契约偏离**：多维表格的 select 选项对象只有 `hue` / `lightness` / `name`，**没有稳定 id**（实测确认）。因此 `option_id` 只能由文案派生，**改文案即断链**。C10 的「固定」只在「不改文案」前提下成立。

#### 4.3.1 提交侧：外部选项存的是 `options.id`（决定 V3 的分量）

官方《审批实例表单控件参数》明文：

> 单选控件 … **如果关联外部选项，则 value 需要传入外部选项的 `options.id`**。（多选同理，是 `options.id` 的数组）

注意这是**语义分叉**：普通单选控件的提交值取审批定义里的 `option.value`；**配了外部选项后，提交值变成外部选项的 `options.id`**。

→ **历史单据里存的就是我们的 `option_id`**（形如 `payment_fx_rate:{hex}`）。
→ 所以 §6-**V3**（详情页重新调接口还是用提交时快照）是**决定性的**：它决定「历史单据显示旧汇率」能不能成立。
→ 也解释了为什么 §8-U5 的「改文案即断链」是**真实业务风险**而非理论问题。

#### 4.3.2 `引用多维表格` = `mutableGroup`，官方列为 API 不支持控件

官方《审批实例表单控件参数》的「API 不支持的控件」表：

| 控件/控件组 | Type |
|---|---|
| 引用多维表格 | `mutableGroup` |

三重含义：

1. **解释 D7**（只能整个组删掉重建）——该类型根本不在 API 支持列表里，无法单独改造子控件。
2. **附带收益**：重建后变成 `radioV2`，进入 API 支持列表，**将来可以程序化提单**。
3. **附注**：用 API 提单时这些控件**本来就无法填写**——所以现在的表单只能人工提单。

#### 4.3.3 `明细/表格`（`fieldList`）是二维数组

官方明文：`fieldList` 的 `value` 是**二维数组**，按明细内控件的顺序依次设置。

**本表单的三个控件在 `付款明细` 这个 `fieldList` 内部**（见 [controls.md](./feishu-option-ingest-controls.md) A-1）：
`往来付款类型`、`项目组合`、`可提供的附件类型`。**重建它们要连带处理明细结构。**

#### 4.3.4 控件 ID 必须与审批定义一致（加重 V2）

官方明文：「控件的 ID，**需要与审批定义中的控件 ID 保持一致**」。

→ 重建控件会分配新 id，任何按字段 id 提单 / 取数的下游都会断。**V2 的分量比原先估计的重。**

### 4.4 多维表格工作流的能力边界

来源类型：**官方明文**＝从飞书帮助中心页面取回正文并反转义内嵌 JSON。

| 事实 | 来源 | 影响 |
|---|---|---|
| 触发器枚举共 9 种，**没有「删除记录时」** | `feishu.cn/hc/zh-CN/articles/740947703250`、`687446300693` | 删除**可能不触发通知**（不影响 A1） |
| 链接**不支持带查询参数** | `larksuite.com/hc/zh-CN/articles/125809335872` | `source_key` 必须进路径段 |
| **不支持内网 URL** | 同上 | → **§6-P-1 公网入口** |
| HTTP 节点响应上限 **60 秒** | 同上 | 端点必须秒回 ACK |
| 请求体可选 `none`；请求头支持键值对 | `feishu.cn/hc/en-US/articles/410063847664` | 通知体可留空 |
| 运行次数额度**企业级共享**，超限**静默停摆至次月 1 日** | `feishu.cn/hc/zh-CN/articles/949255360693` | A1 理由 ① |
| 「新增/修改的记录满足条件时」**不追溯已有记录** | `feishu.cn/hc/zh-CN/articles/740947703250` | 存量靠首次全量播种 |

**未找到官方明文的三点**（§6 实测）：删除是否触发、一次批量改 N 行触发几次、HTTP 节点按状态码还是响应体 `code` 判成败。

### 4.5 框架与仓库侧的硬约束

| 约束 | 位置 | 后果 |
|---|---|---|
| `filterable` **fail-closed**，字段位校验先于角色判定 | `validation.rs:127-141`、`filters.rs:61-66` | 父键列漏开 → **任何带联动参数的请求首屏即失败**，折叠成 `code=50001` |
| `Text` 列**不能建索引**（MySQL 1170） | `schema_sync/render.rs` | 父键列必须 `Str` |
| `unique` 触发 `GROUP BY ... HAVING COUNT(*)>1` 预检 | `schema_sync/plan.rs:344` | 父键列**不能 unique**（父子一对多） |
| 已有数据的表加 `required` 且无默认值的列 → 启动期失败 | `schema_sync/plan.rs:121-130` | 新列必须可空或带默认值 |
| `where_in` **拒绝空列表** | `filters.rs:97-102` | 「补集为空」会整轮报错 → 必须显式短路 |
| `update_in_tx` **无条件覆写 `updated_at`**，但**不动 `created_at`** | `write.rs:302-349` | `created_at` = 选项行首次入库时间（A11 的生效期落点） |
| `Err` 会记 `result="error"` 并计入全站 burn-rate | `observability` 规则 | 失败必须走 `Ok(ApiResponse::fail(...))` |
| `ApiResponse::fail(...)` 的 HTTP 状态是 Action 的 `success_status`（默认 200） | `axum.rs:253`、`response.rs:251` | **鉴权失败对飞书工作流表现为 HTTP 200** |
| workspace `reqwest` 用 `default-tls`；生产镜像**构建阶段不装任何系统包** | `lib_yang/Cargo.toml:69`、`docker/app/Dockerfile:13-34` | 开 `http` feature 后**预计构建失败** → §6-P0 |
| `authorization.outbox_poll_interval_ms` 硬限 `10..=250` | `config/mod.rs:1236-1238` | 轮询间隔**不能复用该配置位** |
| `http.request_timeout_seconds` 默认 30 秒，据此挂 `TimeoutLayer`（超时会**取消 handler future**） | `config/mod.rs:159-161`、`axum.rs:326-331` | 通知端点不能内联拉取 |
| 默认 `RetryConfig` 只重试 `502/503/504`，且 `retry_non_idempotent = false` | `crates/yang-base/src/http/request.rs:104-115` | 覆盖不到 429 / 401 / 403 → §5.3 必须显式配置 |

### 4.6 实测数据（T 时刻快照）

**① 数据源**：单表 `ZoCWb82JQaCCiAspCqbcUvlsnwg` / `tblauuOafa4acvT3`（公司往来付款，**228 行 / 30 列**）。

**② 各候选列的规模**（A7a 取记录口径，故「在用值」才是值域）：

| 列 | fieldID | 类型 | 在用值 |
|---|---|---|---|
| 币种/Currency（单选） | `fld6DuK6tM` | select | **7** |
| 汇率/Exchange Rate | `fldazesSdE` | select | **7** |
| 收款方式/Payment method | `fldiePWPdP` | select | 4 |
| 交易类型/Transaction Type | `fldgvY1jwk` | select | 4 |
| 办公地点(Office location) | `fldeysRdna` | select | 8 |
| 公司名称/Company name | `fldQ7LcB6y` | select | 39 |
| 可提供的附件类型 | `fldP6ThDLp` | select | 9 |
| 项目组合/Project Portfolio\* | `fldkasawcj` | select | 226 |
| 费用大类/Main Exp Cat\* | `fldTyg5VBz` | select | 2 |
| 费用类型/Fee Type\* | `fldEblAr7X` | select | 10 |
| 银行流水摘要-编码 | `fldM0j5Do3` | select | 37 |
| 支出结构_二级项目 | `fldSkg4ATX` | select | 19 |
| 支出结构_三级项目 | `fldRH6hUS3` | select | 24 |

> 取记录口径下**不需要目录清洗**（A7a 的收益之一）。目录侧的缺陷项（约 32% 重名/测试项/PII）只在取目录时才成为问题。

**③ 三条已声明的父子关系**（`[实测 228 行]`）：

| 父 → 子 | 有值行 | 父值 | 子值 | 配对 | 一父多子 | **一子多父** |
|---|---|---|---|---|---|---|
| **办公地点 → 公司名称** | 39 | 8 | 39 | 39 | ✅（成都 13、深圳 20） | **无** |
| **费用大类 → 费用类型** | 43 | 2 | 10 | 10 | ✅（业务划转 8、股东借款 2） | **无** |
| **币种 → 汇率** | 7 | 7 | 7 | 7 | 无（1:1） | **无** |

**三条全部 0 例「一子多父」**——共现即正确配对，这是 A7b 的直接依据。

**④ 汇率会累积（A11 的由来）**：汇率**按月更新**且旧记录**保留旧值** ⇒ 同一币种名下会逐月累积多个历史汇率。
于是「币种子集只有一个元素」这个前提**会随时间失效**，需要靠文案带生效期让用户分辨。

**⑤ 「从数据猜层级」已被证伪（P3 的依据）**：强行用「同名或互为子串」当规则去推：级联2（二级→三级）召回 15/24；
级联1（费用大类→费用类型）对 10 条真实配对 **0 对 / 9 错 / 1 无候选**——把 `APP研发服务费` 的父指到了同级兄弟
`收到-APP研发服务费`（因为 115 条大类目录里有 84 条本身就是子值）。**声明的是列对，不是配对。**

### 4.7 不再采用的来源（保留结论以免重提）

以下来源在 A7a/A7b 拍板后**均不再使用**，记录在此避免后续有人重提：

| 来源 | 为什么不用 |
|---|---|
| 主数据 Base `CE8Xbxn19ay1egsUf2scxgp8nyf` 的 `1_04_支出结构表`（153 行 / 显式三级列） | 用户决定**不再使用该 Base 的维度表**，统一改到台账表；且台账共有值已覆盖 |
| 台账字段的**选项目录**（费用类型 312、项目组合 234…） | 违反 A7a；且含约 32% 缺陷项需清洗 |
| 第三个 Base `AoT5biVqOafFtdsIhFjcXX2vnW4`（映射表，`收款方名称`） | 台账表**后续会补该列** |
| **命名编码法**（把层级编进选项名） | 分隔符已被占用且实测指错：`-` 被 `收到-`/`支付-` 占用（10 条真实配对 ok=0 / wrong=7）；`/` 100% 是「中文/English」双语分隔 |
| **公式 / lookup 算父级** | 违反 P2；实测台账 5 个 formula 字段全是记录级聚合 |
| **「父用目录 + 子用目录∪已用配对」的并集** | 比纯目录更糟：实测死路父级 82.2%–98.2%、子级孤儿 284/294；**父级留空时孤儿可被选中提交出「有子无父」的记录** |

> **⚠️ 同 `field_id` ≠ 同语义**（保留这条陷阱记录）：主表的 `支出结构_一级项目` 与台账的
> `费用大类/Main Exp Cat*` 共享 `fldTyg5VBz`，但目录（33 vs 115）与语义（费用分类 vs 资金往来）都已分叉——
> 「业务划转」「股东借款」**不在主表那 33 项里**。

## 5. 设计

### 5.1 总体形态

```
台账表（唯一数据源）
   │
   ├─ 记录接口（按 field_names 限定列）──▶ 拉取 worker ──▶ 选项派生 ──▶ feishu_option
   │                                         ▲
   └─ 同一批记录同时提供「值域」与「同行配对」  │ 定时全量（唯一正确性来源）
                                              │ 通知（可选，best-effort 加速器）
```

- **轮询（必须）**：周期全量拉取 + 补集停用。
- **通知（可选，纯加法）**：只置脏，不做同步拉取。**本期不做**（A9）。
- **时效契约**：最大可见延迟 = 一个轮询间隔。
- **首次全量拉取 = worker 启动后立即跑第一轮**（兼作存量播种）。
- **单实例（A10）**：不做跨实例单飞与快照 CAS。

### 5.2 数据源登记：坐标、取数选择器与状态

`feishu_datasource` 新增列（**服务端 MySQL 加列，不违反 P1/P2**）。`Str` 未给 `max_length` 时默认 `VARCHAR(255)`：

```rust
ingest_mode         => Radio::<String>::new().require(true).varchar(16)
                           .options([("push", "手工推送"), ("pull", "定时拉取")])
                           .default("push"),          // 存量数据源语义不变
bitable_base_token  => Str::new().title("Base Token").max_length(128),
bitable_table_id    => Str::new().title("Table ID").max_length(128),
bitable_view_id     => Str::new().title("View ID").max_length(128),        // 可空
bitable_field_id    => Str::new().title("取数列 Field ID").max_length(128), // 值域来源列
last_pull_at        => Timestamp::new().title("最近拉取时间"),
last_success_at     => Timestamp::new().title("最近同步成功时间").sortable(true),
consecutive_failures=> Int::new().title("连续失败次数").default(0),
last_error          => Text::new().title("最近错误"),
snapshot_digest     => Str::new().title("快照摘要").max_length(64),
```

- 全部新列**可空或带默认值**（schema_sync 门禁）。
- `base_token` / `table_id` / `field_id` 进 URL 路径段，必须在 `create/update_datasource` 做**字符形状白名单校验**。
- **`linkage_mapping` 已存在**（`datasource/table.rs:47-55`）但**零读写** → 本设计启用它（§5.5）。
- > **`field_allowlist`（敏感列白名单）本期不做**（本条链路两列都安全），
  > 但**推广到其它列前必须补**——该表有 `公司名称` / `银行流水摘要-编码`（样例 `WL01-<人名>`）等 PII。

### 5.3 拉取

**走记录接口**（A7a：值域取记录，不走字段选项接口）。用 `field_names` 限定列。

**`tenant_access_token`**：Redis 缓存键 `yang-system:{deployment}:feishu:tenant_token`，TTL = `expire - 300`；
刷新用 `set_nx_ex` 短锁去重（锁键 `…:tenant_token:lock`，TTL 10 秒）；未持锁者等待并重读缓存（最多 3 次 × 200ms），
超时后用旧值出站并在失败时写 `last_error`；遇 token 失效业务码清缓存强制刷新一次后重试一次。

**出站能力**：用 `yang_base::http` 的既有能力（`HttpClient` / `RetryConfig` / `CircuitBreaker`），不要自己造。

**失败与重试契约**（默认 `RetryConfig` 覆盖不到下列形态，必须显式配置）：

- **429**：可重试，优先读 `Retry-After`，否则指数退避。
- **5xx**：沿用默认。
- **401 / 403**：**不重试**，写 `last_error` 并告警（区分「凭证无效」与「无文档权限」）。
- **HTTP 200 + token 失效业务码**：清缓存、强制刷新一次后重试一次。
- `consecutive_failures` **只在整轮成功时清零**。
- **重试耗尽必须以 `Ok(ApiResponse::fail(...))` 落点**（否则烧 burn-rate）。

**写入逻辑抽成 ctx-free 函数**，与 HTTP handler 共用。原因：审计走 `succeeded_system_event`，
依赖 `ctx.dispatch_target()`，其唯一 setter 是 `yang-base` 的 `pub(crate)`——**worker 自造的 ctx 必然硬失败在 `ConfigError`**。

### 5.4 `option_id` 与 `sort_order` 的派生

```
无父：option_id = {source_key}:{hex(sha256(label))[:12]}
有父：option_id = {source_key}:{hex(sha256(parent_key ‖ U+001F ‖ label))[:12]}
```

- **`source_key` 前缀**：`option_id` 是表级唯一索引，另有跨源 `40901` 预检。前缀让「两个数据源用同一个 label」不可能碰撞。
- **父值进哈希**：保证同一 `source_key` 内「同 label 不同父」不碰撞。
- **按 `option_id` 去重**。**不要按 label 折叠**——那会把「同 label 不同父」重新并成一个，父级选到其中一个时另一个**静默消失**。
- **不引入序号**（会随重排漂移，破坏「固定」）。
- **不要用行号 / 编号 / `record_id` 当码**。
- **`sort_order`**：按本轮快照行序赋 0,1,2,…。取选项端点按 `sort_order ASC, option_id ASC` 排序并据此编码游标
  （`approval_options.rs:201-202`）——全落 0 会让选项序退化成哈希序且跨页不稳。序号**不进 `option_id`**。

### 5.5 级联筛选

**一个控件 = 一行数据源 = 一个 `source_key` = 一个取数列。**

**配置**（`linkage_mapping`，JSON 文本）：

```json
{ "<联动字段代码>": { "parent_source_key": "<父数据源的 source_key>",
                      "parent_field": "<本表里承载父值的列名>",
                      "cascade_field": "<本表里承载子值的列名>" } }
```

**`parent_source_key` 不可省**：父键列存的是**父的** `option_id`，没有它就拼不出来。
**`parent_field` / `cascade_field` 也不可省**：边集要从**本表的这两列同行共现**读出（A7b）。

**派生流程**（一张表、一次读）：

1. 读本表记录（限定 `parent_field` / `cascade_field` 两列）→ 同行共现得到 (父文案, 子文案) 边集。
2. 对每个子值算 `parent_key = {parent_source_key}:{hex(sha256(父文案)[:12])}`。
3. **一致性校验（启动期，fail-closed）**：`parents(边集)` ⊆ 父数据源的 option 集合，不满足则**拒绝启用该数据源**。

**存储**：`feishu_option` 新增三列：

```rust
parent_key     => Str::new().title("父级选项").max_length(192).indexed(true).filterable(true),
effective_from => Timestamp::new().title("生效期"),   // A11
last_push_at   => Timestamp::new().title("最近推送时间"),   // 替代 updated_at 作为存活信号
```

`parent_key` 每一位都不是可选的：`Str` 而非 `Text`（索引）；`filterable` 必开（fail-closed）；
**不能 `unique`**（父子一对多）；**不能 `require`**（存量行）。

**汇率文案带生效期（A11）**：`label = "{汇率值}（{YYYY-MM} 起）"`。
- 生效期默认取**首见时间**（新值入库时写 `effective_from`）。
- **首轮播种时人工指定**——否则上线那一次会把台账里所有已存在的汇率都记成「上线那个月」，是错的。
- ⚠️ `created_at` 是框架自动写的、**不可覆盖**，所以生效期要落在独立的 `effective_from` 列上。

**读取**：`resolve()` 在游标解码之后、构造查询之前插入父级筛选，**挂在顶层**（顶层条件之间是 AND，不影响 keyset 游标）。五条分支：

1. **不带 `linkage_params`** → **回退全量**（C3 硬要求）。这也是**必须按控件拆 `source_key`** 的原因。
2. **value 为空或 trim 后为空** → 回退全量并记 warning（用户尚未选父级是最常见形态）。
3. **归一化**：父键列存**裸 `option_id`**；读侧做**防御式**处理（有 `@i18n@` 前缀才剥、没前缀原样用、两端 `trim`）。
   归一化后匹配不上 → 再按 `(source_key, label)` 精确查一次 → 仍不中则 fail-closed 返回可归因业务码，**不要静默返回 0 行**。
4. **多个 `linkage_params` key** → 按 `linkage_mapping` 取**命中的** key；命中 0 个回退全量，命中 ≥2 个才 fail-closed。
5. **映射缺失** → 回退全量而非空集（空集是最难归因的失败形态）。

> **A7a 带来的简化**：取记录口径下每个父值必有子值，**不需要"父值无边集"分支**。

**禁用 `where_eq(field, json!(null))`**：`validation.rs:204-211` 的 `Eq | Ne` 分支对 null 直接放行，
但那个渲染器在 `sql_render.rs` 且是 `#![cfg(all(test, ...))]`；生产走 `plan.rs:25-27` 的 `WhereCondition::Eq`
（`Condition::Eq` 是 `yang-db` 侧的名字），**不特判 null** → 单测绿、线上恒不命中。表达空值只能用 `where_null()`。

### 5.6 写入语义

- **整行替换，不复用 `upsert` 的合并契约**（A5）：`enabled` 是单向的——删→补集置 false→加回→`upsert` 改 label 但 `enabled` 仍是 false→**永久不可见**。
- **补集停用必须整轮成功才执行**（A6）。**成功判据是断言收敛**（`page_token` 耗尽 + 累计行数与首屏 `total` 一致 + 无 `Err`），
  **不是 `fetched != 0`**——读成功且确实为空时应照常停用，否则选项集永久冻结而控制台仍显示「刚刚同步」。
- **空补集显式短路**（`where_in` 拒绝空列表，`filters.rs:97-102`）。
- **摘要比对**：只对**派生行**算摘要（含 `parent_key` / `label` / `enabled` / `sort_order` / `effective_from`），
  并把**派生规则版本号**并入输入（否则换规则后源内容不变会让新列永远填不上）。
  **事务边界**：快照写入 + 整行替换 + 补集停用 + `digest` 推进必须在**同一事务**内提交。
- **审计**：只在真变化时追加。

### 5.7 通知端点（可选，本期不做）

```
POST /api/v1/feishu/inbound/datasources/{source_key}/notify
```

- 新增 `option/actions/notify_pull.rs`，在 `option/actions/mod.rs` 三处各一行。
  **不要放 `feishu.datasource`**：`action_ref` 已把 module 硬编码为 `feishu.option`，跨 module 复用中间件会让
  `target.module() != 所在 module`，**构建期直接失败**。
- `source_key` 进路径段（官方明示链接不支持查询参数）。
- **Action 保持 `public`**，复用 `ManagementTokenMiddleware`。**DTO 不设 `deny_unknown_fields`**。
- **收到即 ACK，绝不当场拉取**：`TimeoutLayer` 默认 30 秒会**取消 handler future**，
  若拉取实现在请求里会被截断在**写了一半快照**的状态；而飞书节点上限 60 秒。

### 5.8 防抖与并发（单实例，简化版）

- **单实例（A10）** → **不做**跨实例单飞与快照 CAS。
- 脏标记仍存 Redis（进程重启不丢），用 `getset(key, "0")` 一步**取并清**
  （`crates/yang-db/src/redis/client.rs:373`）——若实现成「`get` 判真 → `del` 清除」，
  两次调用之间到达的通知会被 `del` 连带清掉。
- **禁止「在飞有拉取即丢弃后续通知」**；「拉完再看一次」最多补跑一轮。
- 窗口 5–10 秒，配置落在 `FeishuSettings`（**不能复用 `authorization.outbox_poll_interval_ms`**，它硬限 `10..=250`）。
- **Redis 不可用时 fail-open 直接拉**（拉取幂等）。

### 5.9 时效与可观测性

- 契约：**最大可见延迟 = 一个轮询间隔**。
- 每个数据源的 `last_success_at` / `consecutive_failures` / `last_error` 进控制台台账；连续失败要能告警。
- **父键覆盖率守卫**：启用级联时若 `parents(边集)` 占父源 option 的比例低于阈值、或存在父键恒空的父值，
  控制台标黄并给出具体父值清单；把「过滤命中 0 行」与「父值不存在」区分为两个不同业务码。
- **`updated_at` 不再等于「推送还活着吗」**，控制台排序与文案切到 `last_push_at`。

### 5.10 安全与凭证

- **出站凭证**：`app_id` / `app_secret` 是**应用级**（一份服务 N 个数据源）。只申请**只读** scope，
  **只需授权台账 Base 一个**（A7b 不再用主数据 Base，`收款方名称` 列后续并入台账）。
- **风险增量必须显式记录**：现状是「本 addon 不持有飞书凭证、不出网」；本设计变为「持有租户级凭证、主动出网」。
  泄露面从「写脏我们自己的选项表」升级为「读应用可见的全部协作多维表格」。缓解：只读 scope、只加进需要的那张表、secret 目录。
- **管理 Token 是全局单值**（`config/mod.rs:576`），不按数据源绑定。任何能打开某张工作流的人都能取出它，
  然后调 `upsert` / `delete` 写**任意**数据源。**A8**：通知端点若实现，必须用**单独一把 Token**。
- **取选项端点的既有问题**：存在性判定排在 Token 校验之前，匿名调用者可用 `40401` 与 `40101`/`40102` 的差别枚举 `source_key`。

### 5.11 配置与输入契约

`FeishuSettings` 新增：`app_id`、`app_secret`、拉取间隔、防抖窗口、通知 Token（若上通知）。
全部 `#[serde(default)]`；同步 `config.example.toml` 与 `docs/contracts/CONFIGURATION.md`；加进跨段密钥域交叉检查。

`CreateDatasourceInput` / `UpdateDatasourceInput` 均为 `#[serde(deny_unknown_fields)]`，**必须新增字段才能写入坐标**：
`ingest_mode`、`bitable_base_token`、`bitable_table_id`、`bitable_view_id`、`bitable_field_id`、`linkage_mapping`（全部 `Option`）。
在「省略即保持原值」语义下，传空串表示清空。

### 5.12 数据源的删除 / 停用与拉取的关系

1. 只有 `status == active` 且 `ingest_mode == pull` 的数据源参与轮询；
2. 每轮开跑前**重读数据源行**，行已不存在或已停用则立即丢弃本轮全部写；
3. `delete_datasource` 在删除前**清脏标记**；
4. 「强制 `enabled=true`」只作用于「本轮数据源仍存在且 active」的行。

## 6. 前置步（第 0 步）

**P-1 与 P0 阻塞开发开工；V1 可能推翻本设计。**

**P-1｜公网入口（阻塞飞书侧）。**
飞书要求外部选项接口是**公网可访问的地址，不支持内网 URL**（官方报错文案「HTTP 请求 URL 被封禁」）。
当前部署 `config.toml` bind `127.0.0.1:8080`、nginx 只监听 loopback → **需要先建反向代理 / 域名 / 证书 / 白名单**。
同时确认**出站**方向（服务端拉飞书）不受白名单限制。只暴露该路径，不要放开整个后端。

**P0｜构建修复（阻塞服务端）。**
workspace `reqwest` 用 `default-tls`（native-tls → Linux 上是 OpenSSL），而生产镜像**构建阶段（`docker/app/Dockerfile:13-34`）一个系统包都没装**
（`ca-certificates curl` 装在运行时阶段 `:39-44`）。**启用 `http` feature 后预计构建失败**——
**当前未启用该 feature，所以今天不存在构建阻断；请先在冷缓存环境复现一次再据此改 `lib_yang`**。修法二选一，推荐前者：

1. 把 workspace `reqwest` 切到 rustls 系（**改 `lib_yang` 框架仓库**，先推 `lib_yang`，跑一次冷缓存 MSRV）——与 `sqlx` / `lettre` 的既有取向一致；
2. 或在 Dockerfile **构建阶段（`:13-34`）** 与 MSRV 验证命令两处各加 `libssl-dev pkg-config`。

> Windows 本地开发**不受影响**（native-tls 走 schannel），断的是 Linux 构建。

**V1｜回写环路 → ✅ 已排除（2026-09-22 用户确认）。**
用户确认：**不会有数据回写到作为数据源的多维表格**。叠加两项实测佐证：
① 审批流程里**没有「写入多维表格」节点**（V1-1）；
② 台账 Base 的**自动化列表为 0 条**（其它两个 Base 的自动化都作用在无关的表上）；
③ 抽样记录历史显示变更**全部是真人逐字段手改**（`activity_type` 全为 `update`，操作人是同一自然人，
含「深圳→成都」「费用类型来回改三次」这类手工试错痕迹）。
→ **数据源不会被自己的输出污染。**

> **顺带实测到的两个有用事实**：台账 228 行里**只有编号 1–43 的 43 行有实际数据**，其余 185 行是只有编号的空占位行
> ——这正好印证了 §4.6-3 表里「有值行 39 / 43」的量级；且空行不贡献任何值，**A7a 取记录口径的值域天然干净**。

**V2｜重建控件的下游影响。** 删组重建会分配新控件 id。确认有无其它表单通过「关联审批」引用本表单、有无下游按字段 id 取数。

**V3｜飞书详情页行为〔决定性〕。** 展示已提交单据时，外部选项控件是**重新调接口**还是用**提交时快照**？契约全文未定义。
**§4.3.1 已确认提交存的是 `options.id`**，所以这一条决定 A11 的「历史单据显示旧汇率」能否成立。

> **若实测发现是「重新调接口」，会撞上一个机制冲突**：旧汇率 id 已被补集停用（`enabled=false`），
> 我们的读端按 `enabled` 过滤 → 旧 id 不在响应里 → 详情页显示空。
> 而「让停用的旧 id 仍可解析」与「选项集不随月份无限增长」直接冲突。
> **这个冲突必须在 V3 之后专门解一次**，不要在实现时临时拍。可能的出口：
> ① 停用与「不可解析」分离（停用只表示"不出现在新选择里"，但仍可被按 id 解析）；
> ② 或接受历史单据显示空，把汇率值另存到表单的普通字段里。

**V4｜`linkage_params` 真实报文。** 该字段至今挂在 `#[allow(dead_code)]`，**从未观测到真实值**。趁首次联调抓一次。

**M1｜实测 HTTP 节点按状态码还是响应体 `code` 判成败。** `ApiResponse::fail(...)` 的 HTTP 状态是 `success_status`（默认 200），
所以**鉴权失败在工作流日志里会显示成功、不重试、不告警**。（仅通知路径需要，本期不做。）

**M2–M4**（仅通知路径需要，本期不做）：批量改触发几次 / 租户额度 / 删除是否触发。

## 7. 落地顺序

0. **V1 回写环路** → **P-1 公网入口** → **V2 下游影响**。
1. **P0**：`lib_yang/Cargo.toml` 的 reqwest tls；`project/yang-system/Cargo.toml:9` 加 `http` feature；重新生成并提交 `Cargo.lock`。
2. **抽 ctx-free 的写入/应用函数**（`upsert` 的现有单测零改动可保）。
3. **加列**：`datasource/table.rs`（§5.2）、`option/table.rs`（§5.5 三列）；同步 `src/infrastructure/schema.rs:47-56` 与 `:284-304` 的断言。
4. **拉取主体**：新建 `src/addon/feishu/option/domain/pull.rs`（架构门禁：机制代码进 `domain/`）；
   worker 骨架抄 `src/infrastructure/authorization/worker.rs`。
5. **读端联动过滤**：`approval_options.rs`（§5.5）。
6. **配置与启动**：`config/mod.rs`、`config/source.rs` 的 secret 绑定、`bootstrap.rs` 注册 `HttpClient` 与 worker 阶段。
7. **控制台**：`frontend/src/features/feishu/{types.ts, api.ts, components/DatasourceFormDialog.tsx, views/DatasourceDetailPage.tsx}`；
   `datasource/actions/{create,update,list}_datasource.rs` 与 `option/actions/list_options.rs` 的 `select_fields`。
8. **契约产物**：重新生成 OpenAPI 快照与前端 TS 类型。
9. **飞书侧（人工）**：删掉副本里的「引用多维表格-副本」组 → 新建两个单选控件 → 配 URL/Token → 给币种控件配联动。
10. **验收**：不带 linkage 返全量 / 带 linkage 返 1 项 / 7 个币种逐个验 / 抓 `linkage_params` 报文（V4）/ 验详情页行为（V3）。

**首条链路的完整任务清单见 [feishu-option-ingest-tasklist.md](./feishu-option-ingest-tasklist.md)。**

## 8. 风险与未决项

| 编号 | 项 | 类型 |
|---|---|---|
| **U1** | ~~值域口径~~ → **已决（A7a：取记录）** | ✅ 已决 |
| **U2** | **级联收益的下半段不在我们手里**：`C1–C18` 全文无缓存 / TTL 条款，飞书侧是否回旧值**无定义** | 需实测（V3） |
| **U3** | **`linkage_params` 真实报文从未观测到** | 需实测（V4） |
| **U4** | **C17**：官方明示「配置了联动参数或使用 V2（`page_token`、`query`）暂未对开放平台做完整支持」。本设计同时踩这两个开关 | 已知风险 |
| **U5** | **`option_id` 无法做到 C10 的「固定」**：select 选项没有稳定 id，只能由文案派生。① 改文案 → 新 id + 旧 id 置 `enabled=false`（不删除）；② **改父文案会连带改掉其全部子选项的 `option_id`**——断链是子树级的 | 已接受 |
| **U6** | **`updated_at` 语义被污染**：需迁到 `last_push_at`，并修订**四处**同一断言的注释：`option/table.rs:160-169`、`list_options.rs:43-45`、`frontend/.../DatasourceDetailPage.tsx:10-12`、`docs/architecture/feishu-datasource-console.md:55` | 需同批修订 |
| **U7** | ~~多实例~~ → **已决（A10：单实例）** | ✅ 已决 |
| **U8** | **合规前置**：服务端拉取必须给该文档「添加文档应用」。**本设计只需授权台账 Base 一个**（A7b 简化后） | 需确认 |
| **U9** | **飞书查询接口的并发约束**：单表不支持并发请求 | 已知 |
| **U10** | **轮询噪声**：已由「摘要比对 + 只在真变化时写」缓解，但审计表无清理机制 | 已知 |
| **U11** | **出站可达性** → 并入 §6-P-1 | 需确认 |
| **U12** | ~~目标控件清单~~ → 见 [controls.md](./feishu-option-ingest-controls.md) A-2 | ✅ 已有 |
| **U13** | ~~台账行与审批单的因果方向~~ → **已决（V1 排除：无回写，台账人工维护）** | ✅ 已决 |
| **U14** | **审批后台「支持模糊、分页搜索」开关状态未验证**。A7a 取记录口径下，本链路只有 7 项，**不触发**；但**项目组合（226）等列仍会超 100 上限** | 需确认 |
| **U15** | **跨 Base 悬空引用**：`tbl4qMVD3SwlyGvI` 在两个 Base 里都不存在 | 已知，本期排除 |

## 9. 决策日志

| 日期 | 决策 | 结论 |
|---|---|---|
| 2026-09-22 | 值域口径 | **取记录**（台账实际出现过的值），不取字段选项目录 |
| 2026-09-22 | 层级来源 | **同行共现**（`linkage_mapping` 声明列对），不用主数据 Base |
| 2026-09-22 | 汇率变更频率 | 每月 |
| 2026-09-22 | 历史单据显示 | 提交时的**旧汇率**（依赖 V3） |
| 2026-09-22 | 旧记录汇率列 | **留痕**（保留旧值） |
| 2026-09-22 | 历史汇率可分辨性 | 选项文案带**生效期**（A11） |
| 2026-09-22 | 生效期来源 | 首见时间；**首轮播种人工指定** |
| 2026-09-22 | 「引用多维表格」组 | **只能整个组删掉重建** |
| 2026-09-22 | 缺口列 | 台账表**后续会补** `往来付款类型` / `收款方名称` |
| 2026-09-22 | 部署形态 | **单实例** → 不做跨实例单飞与 CAS |
| 2026-09-22 | 控制台范围 | **本期同时做** |
| 2026-09-22 | 公网入口 | **还没有，需要先建**（§6-P-1） |

## 10. 与其他文档的关系

- [feishu-option-ingest-controls.md](./feishu-option-ingest-controls.md) — 目标控件清单（§6-M6 的交付物，含首个表单的实测映射）。
- [feishu-option-ingest-tasklist.md](./feishu-option-ingest-tasklist.md) — 首条链路（`币种 → 汇率`）的任务清单。
- `docs/architecture/feishu-bank-branch-datasource.md` — schema 门禁结论是本设计 §5.2 的前置依据。**该文档有两处表述已与当前源码不符**：
  §4.1（第 72 行）关于「`upsert` 的 UPDATE 条件不含 `source_key`」——现为 `upsert_options.rs:219-224` 两个 `where_eq`；
  §4.7（第 182 行）的 `page(1, PAGE_SIZE + 1)` 分页缺陷——现为 `approval_options.rs:227-234` 的 `page(1, PAGE_SIZE)` 并附编译期断言。
- `docs/architecture/feishu-datasource-console.md` — 控制台界面契约；新增列与端点需在其端点表与投影里同步（另见 §8-U6 的注释修订）。
- `docs/contracts/CONFIGURATION.md` — §5.11 的新增配置项要同步。
- `docs/contracts/SCHEMA.md` — 声明式 Schema 演进规则，§5.2 / §5.5 加列必须遵守。
- `docs/architecture/raw-sql-boundaries.md` — 出站拉取不使用裸 SQL，边界不变。
