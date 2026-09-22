# 任务清单：`币种 → 汇率` 端到端链路（首条落地）

> 配套：[feishu-option-ingest.md](./feishu-option-ingest.md)（设计）、[feishu-option-ingest-controls.md](./feishu-option-ingest-controls.md)（控件清单 A-2）。
> 这是设计文档 §7 落地顺序在**一条链路上的第一次实例化**，跑通后其余链路照做。
>
> **为什么选这条**：最小（7 个币种 × 1:1）、机制代表性最强（同行带值的退化形态）、
> 完全避开 100 条分页上限。

## 0. 链路定义

```
数据源表：ZoCWb82JQaCCiAspCqbcUvlsnwg / tblauuOafa4acvT3（公司往来付款，228 行 / 30 列）
  ├─ 币种/Currency（单选）  fld6DuK6tM  select
  └─ 汇率/Exchange Rate     fldazesSdE  select

source_key：
  payment_currency  （父，无父键）
  payment_fx_rate   （子，parent_key = 同行币种的 option_id）

联动：payment_currency → payment_fx_rate
飞书侧：表单 `往来付款类型/Current Payment Type_正式流程` 的**副本**
```

---

## 1. 已拍板的决策（2026-09-22）

| # | 决策项 | 结论 | 影响 |
|---|---|---|---|
| **D1** | 值域取「字段选项目录」还是「记录」 | **取记录**——台账里实际出现过的值 | 拉取走**记录接口**，不走字段选项接口；币种/汇率各 7 个值；**永不出现「有父无子」→ 不需要"回退全量"分支** |
| **D2** | 汇率变更频率 | **每月** | 每月会产生新的汇率值 |
| **D3** | 历史单据显示哪个汇率 | **提交时的旧汇率** | ⚠️ **依赖未验证的飞书行为**（详情页是否回调接口）→ 见 V1 |
| **D4** | 旧记录的汇率列 | **保留旧汇率（历史留痕）** | 同一币种名下会累积多个历史汇率 |
| **D5** | 历史汇率太多怎么选 | **给汇率文案加生效期**（如「6.9025（2026-09 起）」） | label 含生效期 |
| **D6** | 生效期从哪来 | **服务端自动记首次出现时间**；**首轮播种时人工指定** | `created_at` 记自动值，另需一个**可被一次性覆盖**的机制 |
| **D7** | 「引用多维表格」组能否拆 | **只能整个组删掉重建** | 会分配新控件 id → 见 V2 |
| **D8** | 缺口列（`往来付款类型` / `收款方名称`） | **台账表后续会补这两列** | 本期不做，服务端结构不用改 |
| **D9** | 生产部署形态 | **单实例** | **跨实例单飞与快照 CAS 整块从首版去掉** |
| **D10** | 控制台范围 | **同时做** | 含 4 个前端文件 + OpenAPI 快照 |
| **D11** | 公网入口 | **还没有，需要先建** | ⚠️ **T7/T8 的硬前置** → 见 P-1 |

## 2. 待验证项（未决，阻塞或影响设计）

- [x] **V1｜回写环路 → ✅ 已排除（2026-09-22 用户确认：不会有数据回写到作为数据源的多维表格）**
      佐证：① 审批流程无「写入多维表格」节点；② 台账 Base 自动化 0 条；③ 抽样记录历史全是真人手改。
      **数据源不会被自己的输出污染。**
- [ ] **V2｜重建控件的下游影响〔分量已加重〕**：删组重建会分配新控件 id。需确认：有没有其它表单通过「关联审批」（`widget17796881173030001`）引用本表单？有没有下游按字段 id 取数？
      （副本无历史单据，历史侧风险小。）
      **官方《审批实例表单控件参数》明文「控件的 ID 需要与审批定义中的控件 ID 保持一致」**——id 一致性是硬要求，
      所以任何按字段 id 提单或取数的下游都会断。**这一条在 T8-2 之前必须完成。**
- [ ] **V3｜飞书详情页行为**：展示已提交单据时，外部选项控件是**重新调接口**还是用**提交时快照**？
      决定 D3（显示旧汇率）能否成立。契约 C1–C18 全文未定义。
- [ ] **V4｜`linkage_params` 真实报文**：设计文档 §8-U3 悬置已久。趁本链路跑通时抓一次，确认 key 拼写与 value 形态。
- [ ] **V5｜外部选项接口从未被真实调用过**：实测 13 张审批定义全部 `hasurl=False`，说明该端点从未被飞书调用。整条链路全新未验证。

---

## P-1. 公网入口（新增硬前置）

- [ ] **P-1-1** 定反向代理 / 域名 / 证书方案，使飞书能访问
      `https://<host>/api/v1/feishu/approval/options/{source_key}`。
- [ ] **P-1-2** 同步确认**出站**方向（服务端拉飞书开放平台）不受白名单限制。
- [ ] **P-1-3** 白名单/防火墙：只暴露该路径，不要放开整个后端。

## P0. 构建修复

- [x] **P0-1** `lib_yang/Cargo.toml:69` 的 workspace `reqwest` 从 `default-tls` 切到 rustls 系。
      **先在冷缓存环境复现一次「开 http feature 后镜像构建失败」**，再据此改。
- [x] **P0-2** 先推 `lib_yang`，跑一次冷缓存 MSRV。
- [x] **P0-3** `project/yang-system/Cargo.toml:9` 给 `yang-base` 的 features 数组加 `"http"`。
- [x] **P0-4** 重新生成并提交 `project/yang-system/Cargo.lock`（CI 与 Dockerfile 两次 build 都带 `--locked`）。

## T1. 抽 ctx-free 的写入/应用函数

- [x] **T1-1** 把 `src/addon/feishu/option/actions/upsert_options.rs` 的事务 + 逐条 upsert + 审计
      抽成 ctx-free 的 `(tx, source_key, items)` 形状。
      **原因**：审计走 `succeeded_system_event`，依赖 `ctx.dispatch_target()`，其唯一 setter 是 `yang-base` 的 `pub(crate)`
      —— worker 自造的 ctx 必然硬失败在 `ConfigError`。审计事件改由调用方注入。
- [x] **T1-2** 保住现有 8 条 `upsert_options` 单测零改动。

## T2. 表结构

- [x] **T2-1** `src/addon/feishu/option/table.rs`：
      ```rust
      parent_key    => Str::new().title("父级选项").max_length(192).indexed(true).filterable(true),
      effective_from=> Timestamp::new().title("生效期"),   // 见 D6：可被一次性覆盖
      last_push_at  => Timestamp::new().title("最近推送时间"),
      ```
      `parent_key` 必须 `Str` 而非 `Text`（Text 不能建索引，MySQL 1170）；`filterable` 必开（fail-closed）；
      **不能 `unique`**（父子一对多）；**不能 `require`**（存量行）。
      `effective_from` 由拉取侧写：默认取首见时间，**首轮播种允许人工指定覆盖**（D6）。
- [x] **T2-2** `src/addon/feishu/datasource/table.rs`：坐标列与取数选择器。
      最小集：`ingest_mode`（Radio，默认 `push`）、`bitable_base_token`、`bitable_table_id`、
      `bitable_field_id`、`last_pull_at`、`last_success_at`、`consecutive_failures`、`last_error`、`snapshot_digest`。
      全部**可空或带默认值**（schema_sync 门禁）。
      > `field_allowlist` 本期可缓（两列都安全），但**推广到其它链路前必须补**（该表有 `公司名称` / `银行流水摘要-编码` 等 PII）。
- [x] **T2-3** 启用 `linkage_mapping`（该列已存在但**零读写**）：`create/update_datasource` 入参各加成员，
      并在 `update` 的 `changed` 列表里登记（漏了会让审计 `outcome_code` 变空串）。
- [x] **T2-4** 同步 `src/infrastructure/schema.rs:47-56` 的 `[TableDefinition; 6]` 常量数组长度与 `:284-304` 的精确列表断言。

## T3. 出站能力与配置

- [x] **T3-1** `src/config/mod.rs` 的 `FeishuSettings` 加 `app_id` / `app_secret` / 拉取间隔。
      **一律 `#[serde(default)]`**；加进跨段密钥域交叉检查。
- [x] **T3-2** `src/config/source.rs` 的 `SECRET_BINDINGS` 加 `app_secret`（**走 secret 目录，不走环境变量**）。
- [x] **T3-3** 同步 `config.example.toml` 与 `docs/contracts/CONFIGURATION.md`。
- [x] **T3-4** `src/bootstrap.rs` 的 `ToolsBuilder` 链上注册 `HttpClient`（只开 feature 不会有可用客户端）；
      超时按「单源一次拉取」量级定。

## T4. 拉取实现（**取记录口径**）

- [x] **T4-1** 新建 `src/addon/feishu/option/domain/pull.rs`（架构门禁：机制代码进 `domain/`）。
- [x] **T4-2** `tenant_access_token` 获取与缓存：Redis 键 `yang-system:{deployment}:feishu:tenant_token`，
      TTL = `expire - 300`；刷新用 `set_nx_ex` 短锁去重（键 `…:tenant_token:lock`，TTL 10 秒）；
      未持锁者等待并重读缓存（最多 3 次 × 200ms）；遇 token 失效业务码清缓存强制刷新一次后重试一次。
- [x] **T4-3** **记录拉取**（**不是字段选项接口**——D1 取记录口径）：
      按 `field_names` 限定两列，避免把敏感列读进进程。
      > 注意：走记录接口时**不存在 50 项截断问题**（那是字段选项接口的限制）。
- [x] **T4-4** 选项派生：
      - `payment_currency`：`option_id = payment_currency:{hex(sha256(label))[:12]}`，`parent_key` 空。
      - `payment_fx_rate`：`option_id = payment_fx_rate:{hex(sha256(parent_key ‖ U+001F ‖ label))[:12]}`，
        `parent_key` = **同行币种的 option_id**。
      - `label`（汇率）= `"{汇率值}（{YYYY-MM} 起）"`，月份取 `effective_from`（D5/D6）。
      - `sort_order` 按快照行序赋 0,1,2,…。
- [x] **T4-5** `effective_from` 写入策略：新出现的值取当前时间；**首轮播种走一次性人工指定**（D6）。
- [x] **T4-6** 内容摘要比对（`sha2` 已在依赖，`domain/token.rs:18` 有 hex 封装先例）：
      只在真变化时写库与追加审计；摘要**只在派生规则版本变更时清空**。
- [x] **T4-7** 补集停用：整轮成功才执行；**空补集显式短路**（`where_in` 拒绝空列表，`filters.rs:97-102`）。
      成功判据是**断言收敛**（`page_token` 耗尽 + 累计行数与首屏 `total` 一致 + 无 `Err`），**不是 `fetched != 0`**。
- [x] **T4-8** 失败与重试契约：429 可重试读 `Retry-After`；401/403 **不重试**并告警；
      `consecutive_failures` 只在整轮成功时清零；**重试耗尽必须以 `Ok(ApiResponse::fail(...))` 落点**。
- [x] **T4-9** worker 骨架抄 `src/infrastructure/authorization/worker.rs`。
      **单实例（D9）→ 不做跨实例单飞与快照 CAS。**
- [x] **T4-10** `src/bootstrap.rs` 加 worker 的启动与关闭阶段。

## T5. 读端联动过滤

- [x] **T5-1** `approval_options.rs` 的 `resolve()`：在游标解码之后、构造查询之前插入父级筛选，
      **挂在顶层**（顶层条件之间是 AND，不影响 keyset 游标）。
- [x] **T5-2** `linkage_mapping` 的 JSON 契约：
      ```json
      { "<币种控件的字段代码>": { "parent_source_key": "payment_currency",
                                 "parent_field": "币种/Currency（单选）",
                                 "cascade_field": "汇率/Exchange Rate" } }
      ```
      `parent_source_key` **不可省**——父键列存的是父的 `option_id`。
- [x] **T5-3** 归一化：对 `linkage_params` 的值做**防御式**处理（有 `@i18n@` 前缀才剥、没前缀原样用、两端 `trim`）。
      函数放 `domain/i18n.rs` 与 `i18n_key` 成对，配纯函数单测。
- [x] **T5-4** 分支（**D1 取记录口径下比设计文档简化**）：
      - 不带 `linkage_params` → **回退全量**（C3 硬要求）
      - value 空或 trim 后空 → 回退全量
      - 归一化后匹配不上 → 按 `(source_key, label)` 再查一次 → 仍不中 fail-closed
      - 多 key → 取命中的，≥2 才 fail-closed
      - 映射缺失 → 回退全量
      - **不需要"父值无边集"分支**——取记录口径下每个币种必有汇率（这正是 D1 带来的简化）
      > **禁用 `where_eq(field, json!(null))`**：`validation.rs:204-211` 对 null 放行，但生产走 `plan.rs:25-27` 的
      > `WhereCondition::Eq`，**不特判 null** → 单测绿、线上恒不命中。表达空值只能用 `where_null()`。

## T6. 数据源登记

- [ ] **T6-1** 建两个数据源：

      | source_key | base_token | table_id | field_id | linkage_mapping |
      |---|---|---|---|---|
      | `payment_currency` | `ZoCWb82JQaCCiAspCqbcUvlsnwg` | `tblauuOafa4acvT3` | `fld6DuK6tM` | （空） |
      | `payment_fx_rate` | `ZoCWb82JQaCCiAspCqbcUvlsnwg` | `tblauuOafa4acvT3` | `fldazesSdE` | 见 T5-2 |

- [ ] **T6-2** 记录各自的 `token`（与 `management_api_token` 是**两条独立凭证**）。
- [ ] **T6-3** 触发首轮拉取 + **人工指定首轮播种的生效期**（D6）。

## T7. 控制台（D10：本期做）

- [x] **T7-1** `frontend/src/features/feishu/types.ts`：新增坐标列、`linkage_mapping`、同步状态字段的类型。
- [x] **T7-2** `frontend/src/features/feishu/api.ts`：请求组装（`create/update` 的 body）。
- [x] **T7-3** `components/DatasourceFormDialog.tsx`：表单加坐标与 `linkage_mapping` 录入。
- [x] **T7-4** `views/DatasourceDetailPage.tsx`：显示同步状态（`last_success_at` / `consecutive_failures` / `last_error`）。
- [x] **T7-5** 服务端 `list_datasources` / `list_options` 的 `select_fields` 同步补列。
- [x] **T7-6** 重新生成 OpenAPI 快照与前端 TS 类型。
- [x] **T7-7** 同步 `docs/architecture/feishu-datasource-console.md` 的端点表与投影；修订 §8-U6 那处注释（`DatasourceDetailPage.tsx:10-12` 等四处）。

## T8. 飞书侧配置（人工，D7：**整个组删掉重建**）

- [ ] **T8-1** 在副本里**删掉** `widget17802956697790001`（「引用多维表格-副本」组，含 币种 + 汇率 两个子控件）。
- [ ] **T8-2** 新建两个**单选**控件（币种、汇率），注意重建会分配新 id → 先完成 V2 的核对。
- [ ] **T8-3** 各配：URL = `https://<host>/api/v1/feishu/approval/options/payment_currency`（及 `payment_fx_rate`）、Token、Key 可选（依赖 P-1）。
- [ ] **T8-4** 给**币种**控件配「附加字段 / 联动」，把汇率绑定为被联动控件（1 级、1 项，远低于官方上限）。

> **本链路的组在表单主体上**（`widget17802956697790001`），**不在 `付款明细` 明细容器内**，所以不涉及 `fieldList`。
> 推广到 #4 `往来付款类型` / #5 `项目组合` / #6 `可提供的附件类型` 时，它们**在 `付款明细`（`fieldList`）内部**——
> 官方明确 `fieldList` 的 `value` 是**二维数组、按明细内控件顺序**，重建时要连带处理明细结构。
>
> 另：`引用多维表格` 的类型 `mutableGroup` 官方列为 **API 不支持控件**；重建为 `radioV2` 后进入支持列表，
> **将来可以程序化提单**（现在是人工提单）。

## T9. 验收

- [ ] **T9-1** 不带 `linkage_params` 调 `payment_fx_rate` → **返回全量 7 项**（C3）。
- [ ] **T9-2** 带 `linkage_params` → **只返回该币种的汇率（1 项）**。
- [ ] **T9-3** 7 个币种逐个验一遍。
- [ ] **T9-4** 在副本表单里实机走一遍：选币种 → 汇率下拉只剩一项。
- [ ] **T9-5** **抓一次真实的 `linkage_params` 报文**（V4）。
- [ ] **T9-6** **验证飞书详情页行为**（V3）：真提交一单 → 等一次汇率更新 → 回头看详情页显示什么。
- [ ] **T9-7** **验证回写环路**（V1）：查审批通过后台账表里「币种」列的实际值。

---

## 依赖顺序

```
P-1（公网入口）─┐
D1..D11（已拍板）┤
                └─ P0（构建）→ T1 → T2 → T3 → T4 ─┬─ T5 ─┐
                                                  └─ T6 ─┤
                                                         └─ T7（控制台）→ T8（飞书侧）→ T9（验收）
V1（回写环路）── 建议最先查，成立与否可能推翻本设计
V2（下游影响）── T8-2 之前必须完成
```

## 本链路**不涉及**（推广时才需要）

- 跨实例单飞 / 快照 CAS / 租约——**单实例（D9），整块去掉**
- 通知端点 / 防抖（§5.7 / §5.8，A9 排在轮询之后）
- `field_allowlist` 敏感列白名单（两列都安全）
- `pull_filter` / `consecutive_zero_pulls`
- 「父值无边集」回退分支——**D1 取记录口径后不存在**

---

# 实施记录：出站客户端层（2026-09-22）

**范围**：P0（构建）+ T3（出站能力与配置）+ T4 的 **T4-1/T4-2/T4-3/T4-8**（token 换取与缓存、
记录拉取与分页收敛、失败与重试契约）。**worker 与写路径（T4-4..T4-10、T1、T2、T5、T6）未做。**

## 已落地

| 项 | 落点 |
|---|---|
| P0-1/P0-2 | `lib_yang/Cargo.toml` 的 workspace `reqwest` 由 `default-tls` 改 `rustls-tls-webpki-roots`；`Cargo.lock` 里 `native-tls`/`openssl-sys`/`hyper-tls` 全部消失 |
| P0-3/P0-4 | `project/yang-system/Cargo.toml` 的 yang-base features 加 `http`；`Cargo.lock` 重生成 |
| T3-1/T3-2/T3-3 | `FeishuSettings` 加 `app_id`/`app_secret`/`pull_interval_seconds` 与 `can_pull()`；`source.rs` 四处登记 `feishu_app_secret`；`config.show.toml` 与 `CONFIGURATION.md` 同步 |
| T3-4 | `bootstrap.rs` 注册 `HttpClient::new(30)` |
| T4-1 | 机制代码进 `src/addon/feishu/domain/`（**addon 级**，不是 module 级 `option/domain/`——见下） |
| T4-2 | `domain/tenant_token.rs`：Redis 缓存（TTL = `expire - 300`）+ `set_nx_ex` 短锁（TTL 30s，Lua 比较删除放锁）+ 失效清缓存强刷一次 |
| T4-3 | `domain/bitable.rs`：记录拉取、`field_names` 限定列、分页收敛断言、单元格取值 |
| T4-8 | `domain/outbound.rs`：按**业务码**分类的失败契约与自建重试（`x-ogw-ratelimit-reset`） |

**T1（抽 ctx-free 写函数）被 T4-1 绕开了**：出站客户端层不写库，因此不依赖审计构造。
写路径落地时 T1 仍然是硬前置。

## 设计文档中已被实测/官方文档证伪的三处（**以本节为准**）

1. **`field_names` 要的是字段名，不是 field_id。**
   官方《列出记录》查询参数写的是「字段名称」，`1254024 InvalidFieldNames` 的排查建议是
   「调列出字段接口获取字段名称」；`1254044 FieldIdNotFound` 与 `1254045 FieldNameNotFound`
   是两个不同的码。本文档 T2-2 / T4-3 存的 `bitable_field_id` **不能**直接喂给 `field_names`
   ——必须先经「列出字段」映射（`bitable::resolve_field_name`），且**同名不唯一时要硬失败**。
2. **`page_size` 上限是 500，不是 100。** 官方该接口 `page_size` 最大值 500、默认 20。
   文档里的 100 是**审批后台选项数**的上限，与分页无关。照 100 写会白白多翻 5 倍页数。
3. **成败判据是响应体的业务码，不是 HTTP 状态码。**
   `1254003`/`1254040`/`1254041`/`1254290`/`1254302` 官方**全部标注 HTTP 200**；
   反向地，可重试的 `1254607`/`1254036` 是 **HTTP 400**。
   另外 §5.3 与 T4-8 写的 **`Retry-After` 不存在**——飞书给的是 `x-ogw-ratelimit-reset`（秒），
   且 `yang_base::http::RetryConfig`（`request.rs:806`）只比对状态码、**读不到响应头也读不到响应体**，
   所以「按服务端建议退避」用配置表达不出来，必须自建重试。

## 两处与本文档不同的实现取舍

- **`domain/` 放 addon 级，不放 module 级。** T4-1 原写 `option/domain/pull.rs`，
  但 `AGENTS.md:79` 正文规定 module 层只承载 `mod.rs` / `table.rs` / `actions/`
  （`check_architecture.py:253` 允许，正文不允许）。本次落在 `src/addon/feishu/domain/`，
  与既有 `crypto.rs`/`protocol.rs`/`token.rs` 同级，不必先改门禁文档。
- **单选字段的取值形态按「两种都吃」实现。** 官方响应示例写 `"单选": "选项1"`（裸字符串），
  而本仓库用 lark-cli 抓的真实数据是 `"费用大类/Main Exp Cat*": ["股东借款"]`（数组，
  字段定义 `multiple: false`），manifest 标 `physical_type: array<string>`。
  两个方向都有证据，写错方向是**静默丢数据**，因此 `bitable::cell_label` 两种都接受。

## 仍未做（按依赖顺序）

T1 → T2（加列）→ T4-4..T4-10（派生、摘要、补集停用、worker、接线）→ T5 → T6 → T7 → T8 → T9。

> **写路径落地前必须先定死「空快照歧义」**：多维表格开启高级权限而调用身份不在授权群内时，
> 官方明示可能出现**调用成功但返回数据为空**（`code=0`、`total=0`、`items=[]`）。
> 此时收敛断言会通过，若照常执行补集停用，会把该数据源 100% 已启用的选项静默停掉。
> `bitable::RecordsSnapshot` 已把 `total` 单独暴露出来，供写路径加这道守卫。

---

# 实施记录（二）：写路径、worker、读端级联（2026-09-22 续）

上一节的「仍未做」清单里，T1 / T2 / T4-4..T4-10 / T5 **均已完成**。以下只记与上一节
不同的部分。

| 项 | 落点 |
|---|---|
| T1 | `domain/option_write.rs`（ctx-free 写）+ `audit::succeeded_system_event_without_ctx` |
| T2-1 / T2-2 | `feishu_option` 三列、`feishu_datasource` 十列；**T2-4 经核实是空操作**（那 6 张是运行支撑表，飞书表走 runtime） |
| T2-3 | `create/update_datasource` 各加六个坐标成员；`changed` 列表同步登记 |
| T4-4..T4-7 | `domain/derive.rs` + `domain/pull.rs` |
| T4-9 / T4-10 | `infrastructure/feishu_pull.rs` + `bootstrap.rs` 接线（只在 `can_pull()` 时启动） |
| T5 | `approval_options.rs` 的 `linkage_filter_target` / `resolve_parent_key`；`i18n::normalize_linkage_value` |

## 三处与设计不同的实现取舍（**以本节为准**）

1. **坐标存字段名而不是 `field_id`**。官方 `field_names` 要「字段名称」，传 field_id
   稳定吃 `1254024`。列名是 `bitable_field_name`；**刻意不登记 `bitable_field_id`**
   ——留一个用不上的列只会诱导误用，并有断言钉住这一点。
2. **T5-4 的「按 `(source_key, label)` 再查一次」不实现**。`feishu_option.label` 不是
   `filterable` 列（DSL fail-closed），用它过滤会在运行期吃 `FieldPermissionDenied`；
   而该路径在契约下本就不该执行。改为 fail-closed 返回 `40004`。
3. **补集停用按 500 分片**。`where_in` 有 500 元素上限，活跃选项可以远多于 500；
   撞上上限的表现是整轮在进事务前被打回、快照永远写不进去。

## 三道写路径守卫（都在 `domain/pull.rs`）

- **空快照歧义**：官方明示高级权限下可能「调用成功但返回空」。本轮 0 行而库里仍有
  已启用行 → 判可疑，只记录、**拒绝停用补集**。
- **摘要快路径的附加条件**：摘要是按「本轮应当是什么」算的、不含 `enabled`，所以
  「摘要相同即跳过」必须先确认「本地没有已停用行」，否则被停用的行永远复活不了。
- **每轮开跑前重读数据源行**：管理员可能正在删除或停用它。

## 仍未做

- **T7 控制台前端**（`frontend/src/features/feishu/` 的表单与详情页）。
- **`frontend/src/engine/contracts/api-types.ts` 未随快照更新**：本机 pnpm 受 corepack
  管控（v12.4.1）与项目锁定的 10.33.1 不一致，`dump_openapi.py` 第三步跑不起来。
  用正确的 pnpm 重跑 `python scripts/dump_openapi.py` 即可。
- **T8 飞书侧人工配置**、**T9 验收**。

---

# 实测记录：出站链路首次真实跑通（2026-09-22）

用本地 `config.toml` 里的真实 `app_id` / `app_secret` 调出站探针
（`POST /api/v1/feishu/datasources/pull-probe`），对台账 Base
`ZoCWb82JQaCCiAspCqbcUvlsnwg` / `tblauuOafa4acvT3` 跑了三次。**这是整条出站链路
第一次真实出站**——此前它只在单测与假件上验证过。

## 定论：单选字段回的是**裸字符串**，不是数组

| 探测列 | field_id | field_type | `sample_raw_kinds` |
|---|---|---|---|
| 币种/Currency（单选） | `fld6DuK6tM` | `SingleSelect` | **全是 `string`** |
| 汇率/Exchange Rate | `fldazesSdE` | `SingleSelect` | **全是 `string`** |
| 费用类型/Fee Type* | `fldEblAr7X` | `SingleSelect` | **全是 `string`** |

三个字段一致。**官方《多维表格记录数据结构》是对的**（`3 | 单选 | string`）；
本仓库早先用 lark-cli 抓到的 `"费用大类/Main Exp Cat*": ["股东借款"]` **是那个 CLI 的
归一化**，不是原始响应——这一条从此不必再猜。

`cell_label` 里「两种都接受」的兼容分支保留（成本为零，且对归一化形态免疫），
但主路径确实是 `String`。

## 顺带确认的三件事

1. **`field_id → 精确字段名` 的解析可用**。三次都成功解析出中文名，证明「列出字段」
   这条路走得通，`resolve_field_name` 的实现正确。这也复证了「`field_names` 要的是
   字段名而不是 field_id」。
2. **文档权限已配**。整条链路 `换取 tenant_access_token → 读台账 Base` 全通，
   没有出现 `1254302`——即 U8 的「必须给该文档添加文档应用」已经做了。
3. **`fields` 里的值带尾随换行**。样本是 `"CNY 人民币\n"` 而不是 `"CNY 人民币"`。
   所以 `derive_options` 里的 `label.trim()` 是**承重的**——少了它，派生出选项文案
   会带一个换行。这条实测把那次 trim 从「顺手」变成「必需」。

## 数据画像（与设计文档的记录吻合）

`fld6DuK6tM` 一页 200 行里只有 **7 行有值**（`empty: 193`），首屏 `total` 228——
印证了「228 行里只有 43 行有实际数据，其余是空占位行」。7 个币种与
`.tmp-fs/_inuse.json` 记录的一致。

## 仍未验证

- **V4 `linkage_params` 真实报文**：探针方向是「我们 → 飞书」，而 V4 要的是
  「飞书 → 我们」时的报文，只能等 T8（飞书侧配好外部选项）之后由真实请求产生。
- **补集停用 / 派生的真实行为**：探针只读一页、不写库。要验这些得配一个 `pull`
  数据源让 worker 真跑一轮（T6）。
