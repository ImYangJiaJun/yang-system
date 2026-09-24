# 飞书数据源「以表为单位」配置 — 设计

**日期**：2026-09-23
**状态**：待评审（设计已口头评审通过，待复核书面稿）
**范围**：`project/yang-system`（后端为主，前端向导与控制台）
**前置阅读**：根 `AGENTS.md`、`docs/architecture/feishu-datasource-console.md`、
`docs/architecture/feishu-option-ingest.md`、`docs/architecture/feishu-bank-branch-datasource.md`、
`docs/reference/feishu/`（官方文档离线镜像）

> **本设计取代的现状**：数据源是「一个数据源 = 一个多维表格字段」。控制台的添加表单是
> 四个自由文本输入框，字段一个个手填；同一张表要配 8 个字段就得填 8 遍表单。
> 实测本次目标表有 30 列、其中至少 7 列要作为外部选项，手配成本是主要痛点。

---

## 1. 目标

把数据源的配置与取数单位从**字段**上移到**表**：

1. **向导式配置**：填 `app_token` → 拉数据表列表选表 → 拉视图列表选视图 →
   拉字段列表 → 勾选要作为外部选项的列。
2. **同表内配级联**：勾选时直接指定每列的父列（同表另一列），多级由父链自然涌现。
3. **表级拉取**：一轮拉取扫一次表，把所有勾选列一次取回，再分派到各字段。
4. **出站契约不变**：飞书仍按控件配置的 URL 取数，一字段一个 `source_key`。
5. **Token 自动化**：`Token` 由**系统生成**、控制台**可反复回显复制**，
   轮换由使用人按需触发（§10）。省去运维自己造凭据。
   （Key 加密本次不做，见 §2 与 §10.9。）

## 2. 非目标

- **不改出站契约的形状。** 飞书请求体里没有「字段」参数，字段只能由每个控件各自填的
  URL 决定（§4.1）。本次不试图做「一个 URL 服务整张表」。
- **不做敏感列 allowlist。** 本次不做，显式记在 §11 已知缺口。
- **不做 Key 加密。** 用户决定先不做。因此本次**不改** `encrypt_enabled` 的默认值、
  **不改** `feishu.encryption_key` 的作用域、**不新增** `key_cipher`。
  响应默认仍是**明文**（现状如此）。原始设计留档在 §10.9。
- **不迁 `records/list` → `records/search`。** 见 §11，将来必须迁。
- **不做审批定义的 API 写入。** 飞书不支持，控件仍由人在审批后台手配（§9.3）。
- **不做权限管理面。** 用户与权限仍由运维 SQL + `access` 端口承担。

## 3. 决策记录

### 3.1 产品决策（已确认）

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| D1 | 配置单位 | **表级**：一条数据源 = 一张表 + N 个勾选字段 | 手配成本；同表级联 |
| D2 | 字段身份 | **`field_id`**，拉取时解析成当前名字 | §3.2 A1；改名不断链 |
| D3 | 选项来源 | **从记录派生**（沿用现有 `derive_options`） | §4.5：字段定义选项与实际数据差 8 倍 |
| D4 | 失败策略 | **全有或全无**（一表一轮，失败整表停） | 用户明确选择 |
| D5 | 失败告警 | `consecutive_failures` 达阈值 → **邮件**到配置的收件人列表 | §9.2 |
| D6 | 修复能力 | 控制台提供「体检」：勾选的 `field_id` 与表实际字段比对，缺失即标出 | §9.3 |
| D7 | 敏感列 | **本次不做**，记入 §11 | 用户明确选择 |
| D8 | 级联层级 | 父指针模型：每列**至多一个父列**，深度涌现 | §8；实测存在三级链 |
| D9 | 凭据来源 | `Token` 由**系统生成**，控制台可**反复回显复制** | §10；省去运维自己造凭据 |
| D10 | 轮换时机 | 轮换是**独立按钮**，由使用人决定；复制只回显，不改变任何状态 | §10.3；误点复制不能有后果 |
| D11 | Key 加密 | **本次不做**（用户决定先不做） | §2、§10.9 |

### 3.2 架构决策

| # | 决策 | 选择 | 依据 |
|---|---|---|---|
| A1 | 字段身份存什么 | 存 `field_id`；`field_name` 降为缓存，每轮拉取前按 id 解析刷新 | §4.4：现状把「请求参数」当成了「身份」 |
| A2 | 模型怎么落 | **两张表**：表级 `feishu_datasource` + 字段绑定 `feishu_datasource_field` | §5：`source_key` 的唯一索引必须留在真正需要它的那一层 |
| A3 | 出站路径 | **一行不改**：仍按 `source_key` 路由 | §7：契约未变 |
| A4 | 拉取编排 | 表级一次 `search` → 一份快照 → N 次 derive → N 份落库 | §6 |
| A5 | 摘要/审计/补集上限的单位 | 从「每源」改为「**每字段绑定**」 | §6.3：否则表一大，补集停用会被频繁跳过 |
| A6 | 视图的作用域 | **表级**，一张表一个 `view_id`，所有字段共用 | §4.3 实测：视图不筛字段，只筛行 |
| A7 | Token 归属 | Token 挂在**字段绑定**行（一字段一 Token） | §7：飞书按控件配 URL+Token |
| A8 | 凭据存储 | `Token` **系统生成 + 可逆存储**（推翻「永不存明文」），另存摘要供校验 | §10.1 |
| A9 | 轮换端点 | 独立于回显端点的**写**操作，事务内完成 | §10.3 |

### 3.3 考虑过但未采用

| 方案 | 为什么不用 |
|---|---|
| 单表自引用（`kind` = table/field + `parent_id`） | 一张表两种语义：坐标列在字段行上必须为空、`source_key` 在表级行上为空，而现有列多为 `require` 或有唯一索引。每个查询都要带 `kind` 过滤，漏一个就是静默错数据 |
| 字段清单存 JSON 不落行 | `source_key` 进 URL 路径，必须有全局唯一索引；存 JSON 里做不到，出站按 `source_key` 反查就只能全表扫 |
| 新增「表级分组」容器，底下仍是一字段一数据源 | 语义上等于把「数据源」和「表」拆成两个概念，但二者生命周期完全重合，多一层没有收益 |
| 一个 URL 服务整表，靠 Token 区分字段 | 令牌确实能承载判别（Token 必填且必传），但控件仍要逐个手填同样多的 URL+Token，粘贴成本不降；且 `token_hash` 现在没有唯一索引。**留作后续**，见 §11 |
| **「复制即轮换」**（每点一次复制就重新生成 Token） | 曾认真考虑过。好处实在：系统永远不需要回读明文，于是「只存摘要、永不存明文」这条不变量能**整个保住**（连 `token_cipher` 都不必加）。**但误点一次复制就会让已配好的控件失效**。改为「复制只回显、轮换独立成按钮」（决策 D10）：代价是多存一份密文，换来误操作无后果 |

---

## 4. 关键事实（逐条核实，带锚点）

**实现时如与之冲突，以本节为准并回头修订。**

### 4.1 飞书契约里没有「字段」参数，字段由 URL 决定

官方《关联外部选项》给出的请求体只有七个参数
（`docs/reference/feishu/approval/server-docs/approval-v4/approval/associate-external-options.md:84-92`）：

```
user_id / employee_id / token / linkage_params / page_token / query / locale
```

**没有任何字段标识。** 是哪个字段，完全由审批管理员在那个控件里填的 URL 决定
（同文件 `:49`：「审批系统将对用户配置的外部数据源接口地址发起 HTTP 或 HTTPS 请求」）。

同文档 `:23`、`:42` 明确 **`Key` 是加密解密密钥**，不是字段标识：

> 可选填写 Key，如果填写 Key，则需要在传输数据时进行加密解密。如果未填写 Key，则明文传输数据，不加密。
> Key 用于加密解密。Key 为可选参数，不填写则不进行加密。

**因此「通过 key 确定拉取哪一个字段」在本系统里的落点是 URL 路径里的 `source_key`**，
见 `src/addon/feishu/option/actions/approval_options.rs:227-230`：

```rust
.route(HttpMethod::Post, "/api/v1/feishu/approval/options/{source_key}")
```

其他硬约束（同文档）：

- `:3`、`:7`：**只有「单选」「多选」控件支持关联外部选项**，文档里没有「级联选择」控件。
- `:49`：请求超时 **3 秒**；必须公网可达。
- `:45`：`linkage_params`（联动参数）与 V2（`page_token` / `query`）**「暂未对开放平台做完整支持」**
  —— 即联动只能在审批管理后台手配，不能经 OpenAPI 配。
- `:155`：`i18nResources` **必须至少返回一种语言**，否则控件显示为空。
- `:43`：接口不稳定/返回不合要求造成的单据问题，飞书不做保证与订正。
- **`Token` 与 `Key` 都只在「审批管理后台的控件配置」里手填，请求体里只有 `token`，没有 `key`**
  （`:84-92`）。即：**Key 永远不会被回传给我们**。
  这也是「Key 换错我们观测不到」的根因（见 §10.3 末尾的留档说明）。
- 加密算法是硬契约（`:198-284` 的 Go 参考实现）：密钥取**原文的 SHA-256 摘要**，
  **随机 16 字节 IV 前置**，PKCS#7 填充（已对齐时仍追加整块），输出 `base64(IV ‖ 密文)`。
  现有 `domain/crypto.rs:1-10` 已逐条对齐，**本次不改算法**。

### 4.2 字段列表的三个接口都存在，权限是「任一」

| 用途 | 接口 | 权限（任一即可） |
|---|---|---|
| 列出数据表 | `GET /open-apis/bitable/v1/apps/:app_token/tables` | `base:table:read` / `bitable:app` / `bitable:app:readonly` |
| 列出视图 | `GET .../tables/:table_id/views` | `base:view:read` / `bitable:app` / `bitable:app:readonly` |
| 列出字段 | `GET .../tables/:table_id/fields` | `base:field:read` / `bitable:app` / `bitable:app:readonly` |

三者频率上限均为 20 次/秒，`page_size` 上限 100。

`列出字段` 返回 `field_id` / `field_name` / `type` / `property`
（`type` 枚举见同页响应体字段表：1 文本、2 数字、3 单选、4 多选、18 关联、20 公式、21 双向关联…）。

**高级权限的失败形态要分开看**（这条容易记错）：

- 《查询记录》文档写明：高级权限下调用身份无可管理权限时**可能调用成功但返回空数据**。
- 《列出字段》文档**没有**这句话，它把缺权限记为 `403 / 1254302 Permission denied`。

不要把那句静默空语义外推到 `列出字段`。

### 4.3 【实测】`列出字段` 的 `view_id` 参数不生效

2026-09-23 对目标表实测（`ZoCWb82JQaCCiAspCqbcUvlsnwg` / `tblauuOafa4acvT3` /
`vewAEKSbvO`）：

```bash
lark-cli api GET /open-apis/bitable/v1/apps/$A/tables/$T/fields --params '{"page_size":100}'
lark-cli api GET /open-apis/bitable/v1/apps/$A/tables/$T/fields --params '{"page_size":100,"view_id":"vewAEKSbvO"}'
```

两次返回**完全相同的 30 个字段**，`field_id` 集合与顺序均一致。

**结论：视图不影响「有哪些列可勾」。** 视图真正的职责是**拉取哪些行**
（《查询记录》的 `view_id` 参数，`docs/reference/feishu/docs/docs/bitable-v1/app-table-record/search.md:46`）。

> 官方文档对 `列出字段` 的 `view_id` 只说「多维表格中视图的唯一标识」，未说明其作用；
> 同页那段「当 `filter` 参数或 `sort` 参数不为空时……指定的 `view_id` 会被忽略」是从
> 《查询记录》页面复制过来的——该接口根本没有 `filter`/`sort` 参数。以实测为准。

**UI 必须把这件事讲对**：视图选择器旁要写明「只决定拉取哪些行，不决定能勾哪些字段」。

### 4.4 字段身份：现状把「请求参数」当成了「身份」

`src/addon/feishu/datasource/table.rs:75-81` 明文禁止登记 `bitable_field_id`：

```rust
// **存字段名，不存 field_id。** 官方《列出记录》的 `field_names` 明确要
// 「字段名称」（`1254024 InvalidFieldNames` 的排查建议就是调「列出字段」
// 取名字），传 field_id 会稳定失败。
bitable_field_name => Str::new().title("取数列字段名").max_length(255),
```

理由本身是对的：`field_names` 这个**请求参数**确实要名字。但那是**参数口径**，不是**身份口径**。
名字是会被改的、且 Bitable 界面上的显示名与接口返回值可能忽略空格/换行差异
（`search.md:291` 的 `1254024` 排查建议就是这么写的）。

**表级拉取把这个缺陷放大**：`field_names` 是**一个** GET 查询参数里的 JSON 数组
（`src/addon/feishu/domain/bitable.rs:103-141`，注意 `:116` 的注释——该接口没有 `offset`），
一个坏名字 → `1254024` → **整张表所有字段一起停摆**。现状的爆炸半径是一个字段。

`bitable.rs` 里已经有 `list_all_fields` 与 `resolve_field_name`，解析这一步的后端能力**已存在**。

### 4.5 【实测】字段定义的选项 ≠ 记录里真实出现的值

对目标表（228 行）实测：

| 字段 | 字段定义选项数 | 记录里出现的去重值 |
|---|---:|---:|
| 费用类型/Fee Type* | **312** | 10 |
| 银行流水摘要-编码 | **304** | 37 |
| 项目组合/Project Portfolio* | 234 | — |
| 费用大类/Main Exp Cat* | 115 | 2 |
| 公司名称/Company name | 100 | 39 |
| 办公地点(Office location) | 11 | 8 |
| 币种/Currency（单选） | 8 | 7 |

**决策 D3 取「记录派生」**：只有真实出现过的值成为选项，父子关系由同行共现天然得出
（`src/addon/feishu/domain/derive.rs:44-53`）。选「字段定义」会得到大量永远用不上、
且父子关系只能另外推的两套真相。

> 附带更正一个易犯的误判：CLI 的 `+field-list` 快捷命令**会把选项摘要到 50 个**，
> 让人误以为接口有 50 上限。原生接口返回全量（上表数字即来自原生接口）。做 UI 时
> 不要拿 CLI 的输出当接口行为。

### 4.6 去重已由派生规则承担

`derive_options` 对 `label` 与 `parent_label` **都做 trim**
（`derive.rs:96`、`:104`），并按 `option_id` 去重（`:113-116`）。

目标表的父列确实存在重复选项名（实测）：`费用类型` 312 个选项里 18 个重名、
`公司名称` 100 个里 39 个重名、`办公地点` 11 个里 `成都`/`深圳` 各两次、
`费用大类` 115 个里 5 个重名；`币种` 有 `"CNY 人民币\n"` 这种尾随换行。

**这些都会被现有规则自然折叠。** 本次不新增去重逻辑。

### 4.7 【实测】级联关系与「一子多父」

目标表的四条链，非空行数与去重后规模：

| 链 | 非空行 | 去重后 |
|---|---:|---|
| 币种 → 汇率 | 7 | 7 → 7（实际 1:1） |
| 费用大类 → 费用类型 | 43 | 2 → 10 |
| 费用类型 → 银行流水摘要-编码 | 43 | 10 → 37 |
| 办公地点 → 公司名称 | 39 | 8 → 39 |

**实测到 `费用类型 → 银行流水摘要-编码` 有 6 例「一子多父」**，例如
`pay for services-YL-AR` 同时挂在 `推广测评服务费` 与 `预付储值款` 下。

这推翻了 `derive.rs:47-52` 的注释所载的实测结论：

> 父键由**同行共现**读出（而不是去父表反查）——这是设计 A7b 的直接依据，
> 实测三条父子关系全部 0 例「一子多父」，共现即正确配对。

那句「0 例」是在银行网点 xlsx 数据上量的，**对本表不成立**。
**本实现时须修订该注释**（不是修订算法）。

设计本身扛得住：`option_id` 把 `parent_key` 哈希进了输入
（`derive.rs:6-8`、`:67-74`），同文案不同父会派生成两个不同选项，
`derive.rs:190-207` 有专门的测试钉着这一点。代价是同一段文案在飞书下拉里可能出现两次——
这是级联的固有形态，接受。

### 4.8 出站路径已经具备的能力（本次沿用，不改）

- **级联过滤**：`approval_options.rs:103-194`。带 `linkage_params` 且能确定唯一父级时按
  `parent_key` 过滤，否则回退全量；命中 ≥2 个映射键 `fail-closed`（`LINKAGE_AMBIGUOUS`），
  归一化后父值不存在也 `fail-closed`（`LINKAGE_NOT_RESOLVED`）。
- **V2 分页与搜索**：`page_token` 走 keyset 游标（`:312-318`、`:341-362`），
  `query` 走 `search()`（`:326`）。`hasMore` 由游标存在性唯一决定（`next_page_token`，`:428-435`）。
- **响应契约**：`{code,msg,data}`，不套框架 `ApiResponse` 包络
  （`src/addon/feishu/domain/protocol.rs:139-151`）；HTTP 状态恒 200，成败由 `code` 承载；
  2.5 秒主动收口（`approval_options.rs:51`、`:245-257`）。

---

## 5. 数据模型

```
feishu_datasource                    ← 语义变更：字段级 → 表级
  id
  title                 名称
  ingest_mode           push / pull
  status                active / disabled
  bitable_base_token    ┐
  bitable_table_id      ├ 表级坐标（三个路径段）
  bitable_view_id       ┘
  last_pull_at / last_success_at / consecutive_failures / last_error
  created_at / updated_at

feishu_datasource_field              ← 新表：字段绑定
  id
  datasource_id         → feishu_datasource.id
  field_id              身份；唯一索引 (datasource_id, field_id)
  field_name            缓存，每轮拉取前按 field_id 解析刷新
  source_key            全局唯一，进 URL
  token_hash            Token 摘要（**校验路径用，不需要解密**）
  token_cipher          系统生成 Token 的可逆密文（**只在回显复制时解密**，§10.2）
  token_rotated_at      最近一次轮换时间（§10.3；控制台展示用）
  encrypt_enabled       加密返回（**本次不改**，默认仍是 false）
  default_locale        默认语言
  parent_field_id       同表内的父列，NULL = 无父
  enabled
  snapshot_digest       本字段的快照摘要
  last_push_at
  created_at / updated_at
```

`source_key` 的唯一索引留在**字段绑定**层——它进 URL 路径，必须全局唯一；表级行不需要它。
这正是放弃「单表自引用」方案的原因（§3.3）。

**无存量迁移负担**：本次上线前清空 `feishu_datasource` / `feishu_option`
（用户已确认「现在没有开始使用，可以直接清空数据库」）。因此不设计迁移，
也不存在派生代际共存问题；`DERIVE_RULE_VERSION` 无需 bump（派生口径未变）。

---

## 6. 拉取编排

### 6.1 现状

`domain/pull.rs` 的编排是「选源 → 拉取 → 派生 → 摘要比对 → 事务内落库与补集停用」，
**逐条数据源**执行（`pull.rs:168-192`）。每条源各自：
拼 `[取数列] + [父列]` → `list_all_records` → `extract_values_owned` → `derive_options`
→ `snapshot_digest` 比对 → 事务内整行替换 + 补集停用。

同一张表 N 个字段就是 **N 次全表扫描**。

### 6.2 改为表级

```
选表级源（ingest_mode = pull 且 status = active）
  └─ 解析该表所有勾选字段的当前名字（按 field_id 查「列出字段」）
  └─ field_names = 所有勾选列 + 所有父列 的并集，去重
  └─ 一次 list_all_records 取回一份快照
  └─ 对每个字段绑定：
       extract_values_owned(快照, 该列, 该列的父列)
       → derive_options(该字段的 source_key, 父的 source_key, 值)
       → snapshot_digest 比对
       → 落库 + 补集停用（事务内，按 source_key 界定）
```

拉取合并、落库拆开。

> `field_names` 是**一个** JSON 数组查询参数（`bitable.rs:103-141`），
> 且 `bitable.rs:879` 有「重复字段名被拒」的测试——拼数组时必须去重。

### 6.3 状态列归属

| 列 | 归属 | 理由 |
|---|---|---|
| `snapshot_digest` | **字段绑定** | 一个字段内容没变就该跳过它的写库 |
| `consecutive_failures` / `last_error` | **表级** | 失败是整轮表级的（决策 D4） |
| `last_pull_at` / `last_success_at` | **表级** | 同上 |
| 审计事件 | **每字段每轮一条** | 保留现有「只在真变化时追加」语义 |
| `MAX_COMPLEMENT`（补集停用上限） | **按字段绑定算** | 按表算的话，表一大停用会被频繁跳过 |

---

## 7. 出站契约（不变）

```
POST /api/v1/feishu/approval/options/{source_key}
```

出站路径**一行不改**：`source_key` 从字段绑定行取，`feishu_option.source_key` 的语义不变。
加密那一处也**不动**——Key 加密已移出本次计划（§2、§10.9），
`approval_options.rs:388` 继续用服务端全局的 `context.encryption_key()`。

飞书侧每个「单选/多选」控件仍然各自配置一组 `URL + Token`（§4.1）。因此
**N 个字段 = N 个控件 = N 组 URL+Token**，这是飞书契约决定的，不是本设计的选择。

---

## 8. 级联

**父指针模型**：字段绑定上的 `parent_field_id` 指向**同一张表的另一列**。
每列至多一个父，深度由链涌现：

```
费用大类 ──parent──▶ 费用类型 ──parent──▶ 银行流水摘要-编码
```

三条绑定、两条父指针，不需要在界面上配「三级」。

**与现状的差异**：现在的 `linkage_mapping` 是
`{"<控件代码>":{"parent_source_key":…,"parent_field":…}}`
（`src/addon/feishu/domain/linkage.rs:5-10` 是形状的唯一事实源），
父是**另一个数据源的 `source_key`**，且整段是手写 JSON。改成表级后：

- 父由 `parent_field_id` 指向**同表列**，`parent_source_key` 由系统按该列的 `source_key` 自动填；
- `parent_field` 由系统按该列的 `field_name` 自动填；
- 人不再写 JSON。

**读端必须一起改**（初稿此句写错，2026-09-23 随出站回归修复更正）：`linkage_mapping`
列已被**整个删除**，读端消费的不再是它，而是绑定行上的 `parent_field_id`——
`approval_options.rs` 按 `parent_field_id` 在**同一张表**里找到父绑定、取父的
`source_key`，再按联动参数判定父值。初稿那句「消费的仍是同一份 `linkage_mapping`，
逻辑不需要改」照做会把出站链路写死：`select_fields` 里带上一个已不存在的列，
`1054 Unknown column` 让每一个合法请求恒失败。

**选参数两条路：键匹配优先，通配回退**（2026-09-24 随真机报文修订）。

原文写的是「表级配置下不存飞书控件的字段代码，无法做『精确键』匹配」——这句**下得太早**。
`linkage_params` 的键对应审批定义里的 `linkageConfigs[].key`，是一段**由表单设计者自己
填的自由文本**（§11.2 实测到的是 `手动填写内容`），所以我们完全可以**规定**它填什么：
**填父控件的字段 id**，也就是子绑定行上 `parent_field_id` 的值。于是：

- **键匹配优先**：某个参数的键等于 `parent_field_id`（trim + 大小写不敏感）时就是它，
  **其余参数一律忽略**。一个控件挂多个联动参数不再致命——这正是通配语义做不到的事。
- **通配回退**：没有键命中时保持原语义，**有父即等同一个通配声明**——有父且恰好一个
  联动参数时就用它当父值，≥2 个 fail-closed（`LINKAGE_AMBIGUOUS`）。

回退**必须保留**：存量表单的参数代码是随手填的，砍掉回退等于让它们全部断链；而键抄错
一个字符时也只会退化成现状，不会失败。

键匹配优先的理由不只是「更精确」：读端既能按 `option_id` 也能按 `label` 解析父值之后，
通配语义下「某个**不是**父的参数，其值恰好等于父源里某条文案」会**静默返回错误的子集**；
键匹配把那类误配从「猜一个」变成「明确不是我们的」。

`linkage.rs` 里为旧 JSON 服务的 `parse_linkage_mapping` / `match_linkage` 连同其单测
一并删除（已确认无生产消费者，写端只用 `Linkage` 这个类型），该语义移到
`approval_options.rs::linkage_filter_target` 的文档注释里。

**配套缺口**：控制台的凭据拷贝清单目前只给 **URL + Token** 两项可复制，键匹配要用的
父字段 id 无处可取——运维得自己去多维表格里翻、极易抄错，而抄错的代价从「没影响」
变成「级联静默失效」。清单页需要补这一项（`CredentialChecklist`）。

---

## 9. 失败、告警、修复

### 9.1 失败语义

**全有或全无**（决策 D4）：一轮表级拉取失败，该表所有字段一起停。
与 §6.3 的「失败状态挂表级」一致。

### 9.2 告警邮件

- `consecutive_failures` 达阈值 → 发邮件到配置的收件人列表。
- 配置：`feishu.alert_recipients`，照 `feishu.pull_interval_seconds` 的模式声明
  （`src/config/mod.rs:606-616` 是现成先例：`#[serde(default = …)]` + 常量默认值 + 范围校验）。
- **邮件基建已有，不是新依赖**：`lettre` 已在 `Cargo.toml:17`
  （`smtp-transport` + `tokio1-rustls`），`SmtpEmailSender` / `SmtpSettings` 已实现
  （`src/addon/account/domain/email_delivery.rs:156`）。新发送器照
  `PasswordResetEmailSender`（同文件 `:30-79`）的 trait handle 形状写即可。
- 阈值要可配且要有下限，避免抖动时邮件风暴。

### 9.3 修复能力

「体检」按钮：把该表勾选的 `field_id` 集合拿去和「列出字段」比对，列出差异。

| 情形 | 能否自愈 | 处理 |
|---|---|---|
| 字段被**改名** | **能** | 决策 D2：每轮按 `field_id` 解析当前名字，自动跟上。**不进体检列表** |
| 字段被**删除** | 不能 | 体检标出 → 运维在向导里取消勾选，或改选另一列 |
| 数据表 / 视图被删除 | 不能 | 体检标出 → 改坐标 |
| 权限被撤（`403 / 1254302`） | 不能 | 体检标出 → 运维在 Base 里把应用加为文档应用/协作者 |

**注意**：控件侧的 URL + Token 仍由人在审批后台手配，且审批定义不支持 API 修改
（`docs/reference/feishu/approval/server-docs/approval-v4/approval-related-faqs.md`）。
所以**换 `source_key` = 必须回审批后台改 URL**。向导里改 `source_key` 要给出这个警示。

控制台需要一个「拷贝清单」页，形态见 §10.2.1：**一行一个字段，可复制项是 `URL` 与 `Token`
两项**，字段名与控件号只是标签。复制是纯读、不改状态；轮换是同一页上的独立按钮（§10.3）。

---

## 10. 优化项：Token 由系统生成、可反复回显，轮换是独立操作

> **Key 加密已移出本次计划**（用户决定，先不做）。因此本次**不改** `encrypt_enabled`
> 的默认值、**不改** `feishu.encryption_key` 的作用域、**不新增** `key_cipher`。
> 响应默认仍是**明文**——这是现状，不是本次引入的；见 §11.1。

### 10.1 与现有不变量的冲突（必须显式记账）

现状**只存摘要、永不存明文**，且这是被测试钉住的设计：

- `src/addon/feishu/datasource/table.rs:32-33`：
  > 只存 SHA-256 摘要，永不存明文。`secret(true)` 会把读写权限置为 `Nobody`，
  > 因此必须紧接着显式授回受信角色
- `src/addon/feishu/datasource/actions/create_datasource.rs:204-205`：
  > 只存摘要：本服务只需要校验 Token，永远不需要出示它

**「后续一直可以复制」要求系统能反复出示同一份明文**（不是每次给新的），
与「永远不需要出示它」直接冲突。本次决策**推翻该不变量**（决策 A8），
但**只针对 Token**，且不是无脑存明文：校验仍走摘要，明文只在回显那一条路径上解出来。

### 10.2 方案：两类 Token 并存

用「是不是系统生成的」区分：

| 来源 | 存储 | 能否反复回显 |
|---|---|---|
| 运维**手填** | 只存 `token_hash`（现状不变） | **不能**（我们本来就没有它） |
| 控制台**一键生成** | `token_hash` + `token_cipher` | **能，且不限次数** |

**两份存储分工不同，都不能省**：

- `token_hash` —— 校验路径用它做摘要比对，**校验时不需要解密**；
- `token_cipher` —— 只在「回显给运维复制」这一条路径上解密。

解密面被限制在一个端点里，这是本次刻意保留的收敛。`token_cipher` 用现有
`domain/crypto.rs` 的设施加密，列声明照 `token_hash` 的先例
（`.secret(true).readable_by([SYSTEM_ROLE])`）。

另外三件必须一起做的：

1. **生成用密码学随机源**（`OsRng`），长度足够。格式不限——
   `associate-external-options.md:39`：「参数格式不限，与飞书审批中心表单设计中填写的
   Token、Key 一致即可」。
2. **必须补 `token_hash` 的唯一索引**：它**现在没有**（`table.rs:35-40` 有 `.require(true)`
   与 `.max_length(64)`，没有 `.unique()`）。系统生成后若不唯一，两个字段可能拿到同一个
   Token，出站按 Token 反查就会串。
3. **必须补 DB 唯一键冲突的错误映射**：全仓**没有** `1062`/`ER_DUP_ENTRY` → `ParamInvalid`
   的映射（已 grep 确认）。系统生成 Token 与自动派生 `source_key` 都会撞唯一索引，
   不补的话冲突会以裸 DB 错误冒出来。

### 10.2.1 要复制的东西是两个，且复制不改变任何东西

一张字段一行，可复制项是 **完整 URL** 与 **Token** 两项。字段名与控件号只是标签。

```
字段名                        URL（一个字段一个，不轮换）                 Token
费用类型/Fee Type*   https://…/api/v1/feishu/approval/options/xxx   [复制]
```

**「复制」是纯读操作**：回显当前值，**不重新生成、不改变任何状态**。
同一个 Token 可以一直用下去、反复复制（这正是最初的需求）。要换值走 §10.3 的轮换。

> 审批控件上的 **Key 字段本次留空**（即不加密）。若个别数据源确实需要加密，
> 走**既有的手工路径**：运维自己在控件里填 Key、服务端配 `feishu.encryption_key`、
> 数据源上把 `encrypt_enabled` 打开。**这条路径本次一行不改。**

### 10.3 轮换是独立操作

**轮换**（重新生成 Token）是一个**单独的按钮**，什么时候轮换由使用人决定，
平时一直用同一个值。复制不改变任何状态，所以误点复制不会有任何后果。

设计上的应对：

1. **轮换按钮要二次确认，并写清后果**：「轮换后，已配置该字段的控件会**立即失效**，
   需要把新的 Token 粘回审批后台」。放在确认框里，不是藏在 tooltip 里。
2. **轮换是写操作**：`feishu.datasource.write` 级权限，且**每次轮换追加一条审计**
   （谁、何时、哪个字段）。回显（§10.2.1）另算——它是读，但读的是凭据，
   仍需**独立权限位**（不能挂在 `feishu.datasource.read` 上）并记审计。
3. **轮换要原子**：生成 + 落库 + 返回必须在同一事务里，否则会出现「返回了但没存上」
   ——那种情况下控件配好了却永远验不过。
4. **控制台展示「最近一次轮换时间」**（`token_rotated_at`），
   让人看得出这个字段的凭据是刚换的还是早就配好的。
5. **旧 Token 告警可以真做**：控件拿着旧 Token 调过来会吃 `TOKEN_MISMATCH`（40102），
   而 URL 路径里带着 `source_key`（`approval_options.rs:268-274`），所以能把错误码按
   `source_key` 聚合，在表级页上显示「该字段的控件仍在用旧 Token」。
   这是轮换唯一可归因的失败形态，值得做。

> **将来加 Key 加密时必须回来读这段**：Key 与 Token 的可观测性**完全不同**——
> Key 换错我们**完全观测不到**（请求体里没有 `key`，§4.1），症状只有「审批表单下拉空了」。
> 所以一旦引入系统生成的 Key，**Key 必须与 Token 一起轮换**，且要靠 UI 兜而不是靠监控。

### 10.9 【已移出】Key 加密的原始设计（留档，本次不实现）

以下内容曾设计过，按用户决定移出本次计划。记在这里是为了将来接手时不必重推。

- **默认加密**：`encrypt_enabled` 默认值 `false` → `true`。
- **密钥作用域**：从「服务端全局一把」（`feishu.encryption_key`，`config/mod.rs:590`）
  改为「每字段一把」；`feishu.encryption_key` 降级为**封装密钥**（保险柜钥匙），
  只用来加密落库的 `token_cipher` / `key_cipher`。
  ```
  approval_options ──> 字段的 key_cipher ──解密──> 该字段自己的 Key
                             ▲
               feishu.encryption_key（只封存落库凭据）
  ```
- **加密算法一个字不改**：`domain/crypto.rs:1-10` 已逐条对齐飞书 Go 参考实现
   （`SHA-256(key)` → AES-256-CBC + 随机 IV 前置 + PKCS#7 → `base64(IV‖密文)`）。
   变的只是「用哪把 key」，`derive_key` / `encrypt_bytes` 原样复用。
- **新增列** `key_cipher`（系统生成 Key 的可逆密文）。**没有摘要列**——Key 从不回传
  （§4.1），无从校验。
- **Token 与 Key 必须一起生成、一起轮换**：两者粘在同一个控件里，只换一个会让控件半坏，
  而 Key 半坏是**观测不到**的那种坏（见下条）。
- **新增运维风险，必须连备份流程一起引入**：`feishu.encryption_key` 从「加密响应的钥匙」
  升级为「封存所有凭据的钥匙」（保险柜钥匙）。它丢失 → 所有 `token_cipher` / `key_cipher`
  解不开 → **出示不了、响应也加密不了 → 所有控件一起失效**；若轮换时旧值未保留，
  则**不可恢复**（只能逐字段重新生成、再回审批后台逐个改控件）。
  现状 `config/mod.rs:881-892` 只做了密钥校验并禁止与 `security.totp.aead_key` 复用，
  **没有轮换机制**——再引入时要先补「用旧钥匙解、用新钥匙封」的再加密。

---

## 11. 已知缺口与非目标（显式记账，不静默）

1. **敏感列 allowlist 未做**（决策 D7）。`docs/architecture/feishu-option-ingest.md` 已写明
   推广到其它列前必须补。目标表的 `公司名称` 与 `银行流水摘要-编码`
   （样例值形如 `WL01-<人名>`）都是敏感列。**现状「手打一个字段名」是事实上的闸门；
   勾选网格会把这个闸门拆掉，暴露变成一次点击。** 这是本次改动引入的、尚无归属的新要求。
   **叠加一个事实**：Key 加密本次不做（决策 D11），所以这些敏感列的内容
   **以明文走公网**到飞书。这是现状、不是本次引入的，但勾选网格会让"哪些列暴露了"
   从「手打一个名字」变成「点一下复选框」——两条缺口叠在一起，风险面比单独任何一条都大。
2. ~~**级联读端的真实报文从未观测到**。~~ **已于 2026-09-24 观测到，本条的押注失败了。**
   云服务器日志（`[feishu].log_inbound_requests` 开启后由
   `domain/request_log.rs` 落的 `飞书机器入口请求参数`）抓到真实报文：

   ```
   POST /api/v1/feishu/approval/options/fldq7lcb6y      ← 路径参数 source_key
                                                           （= 公司名称 fldQ7LcB6y，全小写）
   body: { "employeeId": "eabadgcg", "employee_id": "eabadgcg",
           "linkage_params": { "手动填写内容": "成都" },
           "locale": "zh_cn", "openId": "", "open_id": "",
           "tenantKey": "…", "tenant_key": "…",
           "token": "…", "userId": "…", "user_id": "…" }
   ```

   三条实测结论（**推翻了 §8 的原始前提**）：

   - **键是自由文本的「参数代码」**，对应审批定义里的 `linkageConfigs[].key`
     （`docs/reference/feishu/approval/.../approval-definition-form-control-parameters.md:225-227`
     的 `{linkageWidgetID, key, value}`），由表单设计者自己写。它不是控件 ID、
     也不是飞书生成的任何东西——读端对键名不敏感（有父即等同通配），所以无害。
   - **值是被联动控件的文案，不是 `@i18n@<option_id>`**。`linkageWidgetID` 指向的控件
     决定值是什么；本例指向「地点」控件，于是值就是 `成都`。
     **`normalize_linkage_value` 的「剥 `@i18n@` 前缀」路径在本例中根本没被走到。**
   - 报文里同时带 camelCase 与 snake_case 两套键（`employeeId` 与 `employee_id`、
     `openId`、`tenantKey`、`userId`），被 DTO 的「刻意不设 `deny_unknown_fields`」
     自然容忍（`protocol.rs:16-18` 的设计在真机上兑现）。

   **读端因此已改**：`resolve_parent_key` 现在一次查询同时认 `option_id` **和** `label`
   （`option/table.rs` 给 `label` 补了 `filterable`，否则按它过滤会在运行期吃
   `FieldPermissionDenied`），并且**只认启用中的父选项**——否则父文案改名后旧文案会
   命中那条已停用的旧父行，再被子的 `enabled = true` 滤空，把可归因的 40004 退化成
   「`code=0` + 空 options」这种没人能诊断的静默空集。
   同一文案落到多条父选项时取**子树并集**（防御性支路：根级父源按 `option_id` 去重、
   而根级 `option_id` 只是 `hash(label)`，所以这只在父源自身也有父、或 push 源自选 id
   时才会出现）。
   选参数则**键匹配优先、通配回退**：把父控件的字段 id 填进表单的参数代码，就能让
   一个控件挂多个联动参数不再 fail-closed（详见 §8）。
   端到端证据：`tests/feishu_approval_options_integration.rs::approval_options_resolves_the_parent_by_its_label`。

   **尚未解决（本设计另一处押注也跟着失败了）**：`derive.rs:118` 让子行的 `parent_key`
   走 `parent_option_id(...)`，而那个函数硬编码「父源自己没有父」。于是父级一旦是
   **三级链的中间列**（自己也有 `parent_field_id`），它的真实 `option_id` 含祖父键哈希、
   与子行存的 `parent_key` **恒不相等**，`where_in("parent_key", …)` 恒命中 0 行。
   即设计 §4.7 列出的 `费用大类 → 费用类型 → 银行流水摘要-编码` 这条三级链
   **端到端不成立**，且失败形态是静默空集。修它要动 `option_id` 派生口径
   （会连带换掉整棵子树的 id），须单独裁定，不在本次读端修复内。
3. **`records/list` → `records/search` 必须迁，但不是本次**。勾的列一多，GET 的
   URL 变长、响应变大，而 `1254030 TooLargeResponse` 被归为不可重试。
   迁移**不是换 URL**：`field_names` 要变成真 `string[]`、数字列从 string 变 number、
   `view_id` 要挪进 body（对比 `search.md:46-47` 与现有 `bitable.rs:84-141`）。
4. **一张表只能一个视图**（决策 A6）。「A 字段只想拉本月、B 字段要全量」表达不了。
   要支持就得给字段绑定加 per-field 视图覆盖。
5. **一个 URL 服务整表**（靠 Token 判别）留作后续，见 §3.3 末行。
6. **N 个控件仍要手配 N 组 URL+Token**，审批定义不支持 API 修改。靠 §9.3 的拷贝清单缓解。
7. **旧的 `cascade_field` 成员已废弃并被显式忽略**（`linkage.rs:61-63`），
   本次不复活它。
8. **Key 加密整体未做**（决策 D11）。原始设计留档在 §10.9，含 `key_cipher`、
   默认加密、密钥作用域切换，以及随之而来的「封装密钥丢失 = 全量故障」风险与轮换缺口。
   将来接手时读那一节，不要从零推。

---

## 12. 附录：本次实测的复现命令

目标表：`app_token = ZoCWb82JQaCCiAspCqbcUvlsnwg`、`table_id = tblauuOafa4acvT3`、
`view_id = vewAEKSbvO`（228 行）。

```bash
# Windows 的 Git Bash 会把 /open-apis/... 改写成 Windows 路径，必须设这个
export MSYS_NO_PATHCONV=1
A=ZoCWb82JQaCCiAspCqbcUvlsnwg
T=tblauuOafa4acvT3
V=vewAEKSbvO

# §4.3：字段列表（含全量 property.options）。两次调用返回完全一致 → view_id 不生效
lark-cli api GET "/open-apis/bitable/v1/apps/$A/tables/$T/fields" \
  --params '{"page_size":100}' --as user
lark-cli api GET "/open-apis/bitable/v1/apps/$A/tables/$T/fields" \
  --params "{\"page_size\":100,\"view_id\":\"$V\"}" --as user

# §4.5：字段定义的选项数（数各字段 property.options 的长度）

# §4.7：记录抽样，只投影级联相关的 7 列
lark-cli base +record-list --base-token "$A" --table-id "$T" \
  --field-id fld6DuK6tM --field-id fldazesSdE --field-id fldTyg5VBz \
  --field-id fldEblAr7X --field-id fldM0j5Do3 --field-id fldeysRdna \
  --field-id fldQ7LcB6y --limit 2000 --format ndjson --output rows.ndjson --as user
```

字段 id 对照：`fld6DuK6tM` 币种、`fldazesSdE` 汇率、`fldTyg5VBz` 费用大类、
`fldEblAr7X` 费用类型、`fldM0j5Do3` 银行流水摘要-编码、`fldeysRdna` 办公地点、
`fldQ7LcB6y` 公司名称。

---

## 13. 实施顺序（供后续写计划用）

1. **先跑 V4**：一次真实的飞书审批提交，抓 `linkage_params` 报文（§11.2）。
2. 数据模型：两张表 + `source_key`/`token_hash` 唯一索引 + DB 冲突错误映射。
3. 后端元数据 Action：列出数据表 / 列出视图 / 列出字段（三个新端点）。
4. 表级拉取编排（§6），含状态列归属调整与表级失败语义。
5. 向导前端（三步选择 + 勾选 + 父列 + source_key）。
6. 告警邮件（§9.2）与体检/修复（§9.3）。
7. 拷贝清单页（§9.3）。
8. **Token 生成、回显与轮换**（§10）：三个端点分开——生成（建字段时落
   `token_hash` + `token_cipher`）、**回显**（读，独立权限位 + 审计）、
   **轮换**（写，事务内完成 + 审计）。依赖第 2 步的 `token_hash` 唯一索引。

> **Key 加密不在本次范围内**（决策 D11）。原设计曾排在第 9 步（加密作用域切换），
> 已移出，留档在 §10.9。**不要再把它排回计划**——它是会打断出站链路的切换，
> 需要单独的评审与回滚设计。
