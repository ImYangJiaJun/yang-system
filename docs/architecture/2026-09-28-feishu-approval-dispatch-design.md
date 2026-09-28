# 飞书审批实例自动创建与编号回填 — 设计

> - 文档性质：**设计规格（spec）**。本文件描述待实现的目标状态，不表示下述能力已经完成。
> - 撰写日期：2026-09-28
> - 代码快照：撰写时对 `src/addon/feishu/`、`src/infrastructure/`、`src/config/` 与 `crates/yang-base` 逐文件核实；飞书侧结论以 `docs/reference/feishu/` 本地文档副本为准，证据以 `file:line` 或 `文档路径:行号` 标注。
> - 关联文档：`docs/architecture/feishu-option-ingest.md`（多维表格工作流能力边界的实测来源、决策 A1/A2/A8）、`docs/contracts/SCHEMA.md`、`docs/contracts/AUDIT.md`、`docs/contracts/CONFIGURATION.md`
> - 验证记录：本设计在落稿前经 42 个独立代理的两阶段对抗验证（6 维度核查 → 每条发现 3 视角反驳），110 条原始发现中确认 11 条、驳回 1 条；**全部 blocker 已并入本文**，溯源见 §10。

## 一、目标

让多维表格里的一行业务数据（报销、付款申请等）由本系统自动创建对应的**飞书原生审批实例**，并把审批编号回填到该行。

三条使用方约束：

1. **不新建飞书审批定义的表单控件** —— 使用方的审批定义只包含创建实例 API 支持的控件（见 §6.1）。
2. **回填的是 `serial_number`（审批单编号）**，不是 `instance_code`。
3. **失败信息写回同一个回填字段**，由人工清空即可重试。

## 二、非目标

- **不回流审批结果**（通过/拒绝/撤回）到多维表格。审批流转全在飞书侧查看。系统的落库表仍记录 `instance_code`，为将来加回流留出前提（§5.3 已说明为何它是**必要前提**）。
- **不做审批定义的表单设计自动化**。目标审批定义由使用方在审批管理后台手工创建。
- **不做多维表格工作流本身的配置自动化**。按钮、HTTP 节点的配置是人工动作（本文 §4.1 给出可照抄的配置）。
- **不做「按行分支到不同审批定义」**。见 §11 已知限制 3。

## 三、触发方式（已决策）

**按钮触发，不是定时触发。** 两个按钮打同一个端点：

| 工作流 | button_type | 请求体 | 处理方式 |
|---|---|---|---|
| 行内按钮 | `buttonField` | `{base_token, table_id, record_id}` | **同步**：当场创建 + 取编号 + 回写，返回结果 |
| 页面按钮 | `buttonElement` | `{base_token, table_id}` | **异步**：立即返回 accepted，后台限速处理全表待处理记录 |

这个决策推翻了最初的「工作流定时触发」提案，理由是多维表格自动化运行次数是**企业级共享额度，超限后静默停摆至次月 1 日**（`docs/architecture/feishu-option-ingest.md:42,137`，该文档决策 A1 的首要理由）。间隔 1 分钟 = 1440 次/天 ≈ 4.3 万次/月，会把全公司的自动化一起打停。

按钮触发顺带消掉了两件事：不需要公网入口做定时回连的可靠性兜底，也不需要在无人值守场景下处理限速排队。代价是存量记录需要人工触发一次页面按钮。

### 3.1 为什么两种粒度都必须有

- **行内按钮**是主路径：谁提交的谁点，语义清晰，秒级返回，工作流可以据此发消息或写日志。压到单条记录后，原设计的异步就不必要了——1 条记录 = create + get 两次调用，远在官方 **HTTP 节点响应上限 60 秒**（`feishu-option-ingest.md:135`）之内。
- **页面按钮**处理存量与遗漏：上线首次运行、以及行内按钮被跳过或进程重启时丢掉的记录。

### 3.2 两个 button_type 的输出能力不同（工作流侧硬约束）

| button_type | 输出 | 后果 |
|---|---|---|
| `buttonField` | 动态（**表字段 + 记录属性**） | 能引用 `$.{stepId}.recordId`，把记录身份传给系统 |
| `buttonElement` | **仅基础触发属性** | 拿不到记录字段，`base_token`/`table_id` 必须在 HTTP body 里写死 |

来源：`~/.claude/skills/lark-base/references/lark-base-workflow-schema.md:960`。

## 四、链路

```
多维表格按钮（行内 / 页面）
  → ButtonTrigger（buttonField / buttonElement）
  → HTTPClientAction POST /api/v1/feishu/approval/dispatch
       Authorization: Bearer <management token>
       body: {base_token, table_id[, record_id]}
  → 校验 + 分流
       有 record_id → 同步处理单条 → 返回 {success, serial_number | error}
       无 record_id → 落任务 → 立即返回 {accepted:true} → 后台 worker 限速处理
  → records/batch_update 回写多维表格
```

### 4.1 工作流侧配置（可照抄）

**行内按钮**（`buttonField`，同步）：

```jsonc
{
  "client_token": "<唯一值>",
  "title": "提交飞书审批",
  "steps": [
    {
      "id": "step_btn",
      "type": "ButtonTrigger",
      "title": "点击提交审批按钮",
      "next": "step_http",
      "data": { "button_type": "buttonField", "table_name": "<数据表名>" }
    },
    {
      "id": "step_http",
      "type": "HTTPClientAction",
      "title": "调用系统创建审批实例",
      "next": null,
      "data": {
        "method": "POST",
        "url": [{ "value_type": "text", "value": "https://<系统公网地址>/api/v1/feishu/approval/dispatch" }],
        "headers": [
          { "key": "Content-Type", "value": [{ "value_type": "text", "value": "application/json" }] },
          { "key": "Authorization", "value": [{ "value_type": "text", "value": "Bearer <管理 Token>" }] }
        ],
        "body_type": "raw",
        "raw_body": [
          { "value_type": "text", "value": "{\"base_token\":\"<base_token>\",\"table_id\":\"<table_id>\",\"record_id\":\"" },
          { "value_type": "ref", "value": "$.step_btn.recordId" },
          { "value_type": "text", "value": "\"}" }
        ],
        "response_type": "json",
        "response_value": "{\"success\":true,\"serial_number\":\"202609280001\",\"message\":\"ok\"}"
      }
    }
  ]
}
```

**页面按钮**（`buttonElement`，异步）：同上，但 `"button_type": "buttonElement"`（去掉 `table_name`），`raw_body` 里去掉 `record_id` 段，且 `response_value` 改为：

```json
{"accepted":true,"message":"已受理，处理结果稍后回填至表格"}
```

> `raw_body` 用 `text + ref + text` 拼接（`lark-base-workflow-schema.md:666-678` 示例 6）。
> `response_type=json` 时后续节点**只能引用 `response_value` 中声明过的字段**（`lark-base-workflow-schema.md:418,827`），所以响应体必须扁平，不要嵌套。

### 4.2 端点契约

```
POST /api/v1/feishu/approval/dispatch
Authorization: Bearer <management token>       # 复用 ManagementTokenMiddleware
Content-Type: application/json

{ "base_token": "...", "table_id": "...", "record_id": "..." }   // record_id 可选
```

响应恒 HTTP 200，成败由 `{code, msg, data}` 信封承载（本 addon 既有约定，见 `src/addon/feishu/option/actions/approval_options.rs:337-374`）：

```jsonc
// 单条成功
{ "code": 0, "msg": "success", "data": { "serial_number": "202609280001" } }
// 单条失败（终态）
{ "code": 40901, "msg": "数据不完整：缺少「申请人」", "data": null }
// 批量已受理
{ "code": 0, "msg": "accepted", "data": { "accepted": true } }
```

> **⚠️ 鉴权失败会静默成功**：框架的 `ApiResponse::fail(...)` HTTP 状态是 200，而工作流 HTTP 节点按状态码还是响应体 `code` 判成败**官方未明文**（`feishu-option-ingest.md:449` 未实测项 M1）。因此**工作流日志可能显示成功而实际鉴权失败**。运维口径：怀疑失败时直接查系统日志与落库表，不要只看工作流执行历史。

## 五、数据模型

### 5.1 三条配置的行归属

新增一张配置表 `feishu_approval_config`（module 层，无外键——`docs/contracts/SCHEMA.md:12,30` 规定 schema_sync 只增不删、外键恒为 `RESTRICT` 且不可变）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | Key | 自增主键 |
| `title` | Str | 配置名（控制台展示） |
| `base_token` | Str | 多维表格坐标 |
| `table_id` | Str | |
| `approval_code` | Str | 目标审批定义 |
| `applicant_field` | Str | 申请人员字段的 **field_id** |
| `backfill_field` | Str | 回填字段的 **field_id** |
| `enabled` | Radio | 启用/停用 |
| `form_snapshot` | Text | `approvals/get` 返回的控件结构快照（JSON） |
| `form_snapshot_at` | Datetime | 快照时间 |

**为什么是独立表而不是复用 `feishu_datasource`**：后者是「外部选项的数据源」，语义是「这个字段的可选值从哪里来」（`docs/architecture/feishu-option-ingest.md:16-18`）。本表是「这张表的这批行要创建什么审批」，两者不共享生命周期，也不共享读写路径。复用会迫使两套完全不同的语义挤在一张表里。

映射项独立成表 `feishu_approval_field_map`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | Key | |
| `config_id` | Int, indexed | 指向配置行 |
| `widget_id` | Str | 审批控件的 id（来自 `approvals/get`） |
| `widget_type` | Str | 控件类型（快照，用于选转换器） |
| `bitable_field` | Str | 多维表格字段的 **field_id** |
| `required` | Bool | 快照的必填标志 |
| `converter` | Str | 转换器标识（见 §6.2） |
| `option_map` | Text | 选项映射（单选/多选的选项名 ↔ 选项 value），JSON |

**用 `field_id` 而不是字段名作 key**：用户在多维表格里改列名会静默失配（现有 `feishu_datasource_field` 表同时存 `field_id` 与 `field_name`，`src/addon/feishu/datasource/domain/field_table.rs:13-84`，本设计对齐该做法——存 id 作 key，另存 name 仅供控制台展示）。

### 5.2 处理状态表 `feishu_approval_task`

这是**认领队列**，不只是记账表。用户明确不加闸门字段（§11 限制 1），所以「是否已处理」的判据必须落在库里，不能落在多维表格的字段上。

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | Key | |
| `config_id` | Int, indexed | |
| `record_id` | Str, unique | 多维表格记录 id |
| `uuid` | Str, unique | 幂等键（§5.4 的派生与存储规则） |
| `state` | Radio | `pending` / `creating` / `created` / `backfilled` / `terminal` |
| `instance_code` | Str, nullable | |
| `serial_number` | Str, nullable | |
| `attempts` | Int | 可重试次数 |
| `lease_until` | Datetime, nullable | 租约（照搬 `authorization_outbox`，见 `src/infrastructure/schema.rs:70-80`） |
| `worker_id` | Str, nullable | |
| `last_error` | Text, nullable | |

**状态机**：

```
pending ──claim──> creating ──create 成功──> created ──batch_update 成功──> backfilled
                     │                          │
                     │ create 终态失败           │ 回写失败（可重试）
                     ▼                          ▼
                  terminal                  回到 created 重试
                     ▲
                     │ 可重试耗尽 / 60012 回捞失败
```

**硬性时序要求**（这条是根因 D 的修正，必须写死在实现里）：

1. **`create` 之前**先写入 `state=creating` + `uuid`（`uuid` 列建唯一索引）。
2. `create` 返回成功后**立即**落库 `instance_code`（`state=created`），**不要**等回写成功才落库。
3. 回写成功后才置 `state=backfilled`。

这样「create 成功但本地未落库」的窗口从「编号永久丢失」降为「可判定、可续跑」。蓝绿 cutover 用 `docker stop`（SIGTERM → 10 秒后 SIGKILL，`deploy/deploy-blue-green.sh:337`）会静默杀掉处理中的批次，没有这条时序就没有任何痕迹。

**认领 SQL 形态**（照搬 `authorization_outbox`，`src/infrastructure/authorization/outbox.rs:102,125`）：

```sql
WHERE (state = 'pending' AND (available_at IS NULL OR available_at <= ?))
   OR (state = 'creating' AND lease_until <= ?)
   FOR UPDATE SKIP LOCKED
```

租约到期可被重新抢占，因此进程崩溃不会造成永久死锁。

### 5.3 为什么 `instance_code` 必须落库

两条独立理由：

1. **官方不支持按审批单编号反查实例**：`docs/reference/feishu/approval/server-docs/approval-v4/approval-related-faqs.md:144` 明文「目前暂不支持通过审批单编号获取审批实例详情」。只把 `serial_number` 写进多维表格，系统侧就丢了这个审批的唯一钥匙。
2. **`60012` 的响应体不含 `instance_code`**（`instance/create.md:90-95`，只有成功响应才有）。响应丢失时唯一的恢复路径是**用 uuid 反查**（§6.4），而反查要拿一个稳定的本地键。

### 5.4 `uuid` 的派生与存储

官方约束（`instance/create.md:43`）：长度 `1~64` 字符，不区分大小写，格式建议 `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`，**单个企业内唯一**。

**不能只用 `record_id` 派生**：`record_id` 只在**一个多维表格内**唯一，不一定全局唯一。两张表的 `record_id` 撞车时，全新记录的首次创建就会吃 `60012` 并静默丢单。

采用两级派生：

```
uuid = 规范 UUIDv5(命名空间 = NS, 名字 = "<base_token>|<table_id>|<approval_code>|<record_id>")
```

- 外层**必须是规范 UUID 形态**（36 字符、带连字符）。`60012` 的语义是「同一企业内 uuid 已存在」，与格式无关；但实际观测到部分网关路径会拒绝非规范形态（该观测未经真实企业验证，实现时以真实响应为准）。
- **用 UUIDv5（SHA-1 基础）而非 UUIDv3（MD5）**：仓库 CI 有 `pnpm audit`，MD5 会触发告警。
- `approval_code` 纳入派生，使得同一行改配置换审批定义后能正常重发（多建一个实例是符合预期的语义，见 §11 限制 4）。
- `NS` 取一个固定常量命名空间。

**`uuid` 必须持久化**（`feishu_approval_task.uuid`，唯一索引），因为它是崩溃恢复的唯一对账键——响应丢失时 `instance_code` 根本不存在。

## 六、处理流程

### 6.1 前置校验（配置保存时，一次）

保存配置时调一次 `GET /open-apis/approval/v4/approvals/:approval_code`（`docs/reference/feishu/approval/server-docs/approval-v4/approval/get.md`），把控件结构存进 `form_snapshot`。同时做三项校验：

1. **`is_external` 必须为 `false`**。三方定义不能调 `instances create`。
2. **不含 API 不支持的控件**。清单见 `~/.claude/skills/lark-approval/references/lark-approval-instance-form-control-parameters.md:14-30`：`text`（说明）、`mutableGroup`（引用多维表格）、`account`（收款账户）、`serialNumber`（流水号）、`tripGroup`（出差控件组）、`apaascorehrOnboardingGroup`、`apaascorehrRegularateGroup`、`remedyGroupV2`、`apaascorehrJobAdjustGroup`、`apaascorehrOffboardingGroup`。
3. **映射完整性**：每个必填控件的 `id` 都有映射项。

> 注意：`is_external` 字段名与控件不支持清单的**确切判据**（「定义里存在就报错」还是「传了才报错」）在本地文档中无明文，§11 限制 6 列为待实测。

### 6.2 类型转换

`approvals/get` 的 `form` **不是可直接提交的模板**（`~/.claude/skills/lark-approval/references/lark-approval-initiate.md:10`），必须按控件类型单独组装 `value`。本地文档给出的结构（`lark-approval-instance-form-control-parameters.md`）：

| 控件 | type | `value` 结构 |
|---|---|---|
| 单行文本 | `input` | 字符串 |
| 多行文本 | `textarea` | 字符串 |
| 数字 | `number` | 数字 |
| 金额 | `amount` | 数字 + `currency` |
| 日期 | `date` | RFC3339 字符串（`2019-10-01T08:12:01+08:00`） |
| 日期区间 | `dateInterval` | `{start, end, interval}` |
| 单选 | `radio` / `radioV2` | 单个选项值；关联外部选项时传 `options.id` |
| 多选 | `checkbox` / `checkboxV2` | 选项值数组 |
| 联系人 | `contact` | `{value: [...], open_ids: [...]}`，推荐只写 `open_ids` |
| 部门 | `department` | 对象数组，元素字段名 `open_id`，值为 `open_department_id` |
| 明细 | `fieldList` | 二维数组，子项按各自控件类型组装 |
| 电话 | `telephone` | `{countryCode, nationalNumber}` |
| 地址 | `address` | 对象数组，至少含地理库 `id` |
| 关联审批 | `connect` | `instance_code` 数组 |

三种转换器：

- **`direct`**：文本/数字类，`value` 直接用多维表格单元格值。
- **`date`**：多维表格日期是**毫秒时间戳**，需按 Base 时区转 RFC3339。**Base 时区必须作为配置项显式指定**，不要猜测（多维表格不带时区信息，审批 date 控件要带偏移量）。
- **`option`**：单选/多选需要在配置时把**多维表格的选项文案**映射到**审批控件的选项 value**（`option_map` 列）。两者不一定同名，必须显式配。

> `address`、`connect`、`attachmentV2`/`image` 这几类需要用户直接提供 token/id（`lark-approval-instance-value-sourcing.md:88-97`），本设计**不自动准备**——若目标审批定义含这类控件，配置保存时应明确报错而非静默传空。

### 6.3 等待语义：必填缺失不写字段

**这是用户明确选择「不加闸门字段」后的补偿设计。**

扫描集合按 §5.2 的落库表定义：**表格中无终态/已回填任务的记录 ∪ 库中处于可重试状态的记录**。「审批编号为空」只作**输出字段**，不作扫描键。

「数据完整性」判据 = 映射到的**必填控件**是否都能取到非空值。

- **不完整 → 等待态**：本轮跳过，**不写任何多维表格字段**，只记 `last_error` 到库 + 系统侧计数/告警。
- 该记录下一轮仍会被扫到并重算（用户补齐字段后自然通过）。

> 为什么不写成终态失败：用户选择不加闸门字段，若把「必填缺失」写成终态，用户先填业务字段、后填「申请人」的那一轮就会**永久钉死该行**（字段非空 → 永不再扫），唯一补救是人工逐行清空。这是必须避免的。
>
> 代价是这类记录每轮都被重新拉出重算。由于只有页面按钮才触发全表扫描，且扫描是只读的，这个代价可接受。官方 FAQ（`approval-related-faqs.md:86-89`）也明确：**必填控件不传值不会报错**，必填为空不是 API 错误，因此把它建模成 API 失败是语义错位。

### 6.4 创建与编号获取

单条：

1. **先落库** `state=creating` + `uuid`（§5.2 时序要求）。
2. `POST /open-apis/approval/v4/instances`，`form` 是 **JSON 数组字符串**（要压缩转义）。
3. 成功 → **立即**落库 `instance_code`，`state=created`。
4. `GET /open-apis/approval/v4/instances/:instance_code` 取 `serial_number`。

> `serial_number` **不在创建响应里**（`instance/create.md:90-95` 只有 `instance_code`；`serial_number` 只在 `instance/get.md:51` 与 `overview-approval-instance.md:47` 出现）。所以每条记录是**两次**调用：create 100 次/分钟（瓶颈），get 1000 次/分钟、50 次/秒（非瓶颈）。
>
> **`serial_number` 是否随创建立即可查未知**——文档未承诺。若 `get` 返回 `1390003`（`instance/get.md:512`，instance code not found），判为**可重试**并退避，**绝不写终态**。否则刚建成的实例会被写成「找不到」，反而制造第二条不可恢复路径。

### 6.5 回写

按 §7.1 的分批规则 `POST .../records/batch_update` 写回填字段。

## 七、幂等与限速

### 7.1 回写必须分批

`batch_update` 的响应体**只有成功侧的 `records`，没有任何 per-record 失败列表**（对比 `batch_get` 有 `forbidden_record_ids`/`absent_record_ids`，`docs/reference/feishu/docs/docs/bitable-v1/app-table-record/batch_get.md:85-86`）。而多维表格批量写是**全有全无**语义（`docs/reference/feishu/docs/server-docs/docs/bitable-v1/bitable-overview.md:28`：「响应状态是全部成功或者失败，不存在部分成功或失败的结果」），所以一条坏记录会让整个信封非 0 返回。

**若按 search 的 500/页一次提交，一条毒记录会丢掉整批回写结果——而实例创建是已经发生的外部副作用，等于批量制造孤儿审批单。**

规则：

- 子批上限 **≤100 条**（不是 search 的 500）。依据：`batch_update` 单次上限 1000（`batch_update.md:3`），但官方对 `1254607` 的排查建议就是「降低批量请求的 page_size」。
- 冲突时**逐级降级**：子批失败 → 对半拆 → 直到定位到具体 `record_id`，只把该条置为回写失败、其余照常落库。
- 回写失败**并入可重试类**：信封 code 非 0 ⇒ 该子批零条落库，可整体重试（同值覆盖，幂等无副作用）。重试间隔按 `1254291` 的建议取 0.5–1 秒。
- 遵守官方「对单一多维表格同时只请求一次写操作」的建议（`bitable-overview.md:29`）——写侧串行。

### 7.2 失败分类必须扩表，不能直接复用

**这是根因 B 的修正，是本文最重要的一条实现约束。**

`outbound.rs` 自述是「本文件是**唯一**出现飞书错误码字面量的地方」（`src/addon/feishu/domain/outbound.rs:38-40`）。现有码表**全是多维表格域的**：

- `CODES_RETRYABLE = [1254036, 1254607, 1255001, 1255002]`（`outbound.rs:74`）
- 任意未列出的业务码落 `Fatal`（`outbound.rs:272-276`）

后果：**`1395001` 会被判成不可重试的终态**，与设计意图完全相反。

必须做的三处改动（都落在 `outbound.rs`，维持「唯一字面量」约定）：

1. **新增审批域可重试集合**，与既有表**并列**判断，不污染多维表格语义：

   ```rust
   /// 审批域瞬态错误。官方排查建议原文即「降低请求频率，并重试」。
   /// 注意它不是频控码——真正的频控是 CODE_RATE_LIMITED / CODE_TOO_MANY_REQUEST。
   pub(crate) const CODES_RETRYABLE_APPROVAL: &[i32] = &[1395001];
   ```

   在 `classify` 的 `outbound.rs:265-271` 分支里与 `CODES_RETRYABLE` 并列 `contains` 判断。对 bitable/tenant_token 两条既有消费路径**严格是加法**（多维表格不会返回 `1395001`）。

2. **`60012` 必须单列一类**，插入 `outbound.rs:275` 的 `Fatal` 兜底**之前**：

   ```rust
   /// uuid 冲突：该 uuid 已创建过实例。**不是失败**，是「响应丢失、实例已存在」的信号。
   pub(crate) const CODE_UUID_CONFLICT: i32 = 60012;
   ```

   新增 `FailureKind::UuidConflict`（或等价的可区分类别）。在 `disposition`（`outbound.rs:451-463`）里对它显式 `Give`，语义是「不重试，交给上层反查」。**绝不能**落进 `Fatal { code }`——`Fatal` 在本模块的语义是「配置/权限错了，别重试」，与 `60012` 要做的「一次查询后继续」正好相反。

3. **补 `fatal_hint` 文案**（现状 `outbound.rs:311-333` 对这些码返回 `None`，运维只看到裸错误体）：`1390001` → 「表单控件参数错误：用 approvals/get 核对控件 id/type 与取值形态」；`1390015` → 「审批定义已停用：去审批管理后台启用后重试」；`1390013` → 「不支持自定义审批流程」。

**术语订正**（避免实现者填错码表）：

- `1395001` **不是限流码**，是审批服务的瞬态服务端错误（`instance/create.md:115` 官方原文「服务出现错误」，排查建议第二步才是「降低请求频率，并重试」）。
- 真正的频控形态是 **HTTP 429**（部分旧版表现为 `400 + 99991400`），`classify` 已覆盖（`outbound.rs:246-250,265-271`）。两者不要混为一谈。

### 7.3 创建调用必须标 `idempotent: true`

`disposition` 在 `idempotent == false` 时对 Retry 类失败**直接 `Give`**（`outbound.rs:463`），且有单测钉死这条不变式（`outbound.rs:814`）。创建实例是有副作用的 POST，按「安全直觉」标 `false` 等于**零重试层**——429/超时/5xx 一次都不退避，立刻落终态，与 §7.4 的意图正好相反。

**依据**：`instance/create.md:43` 明文 uuid 用于幂等操作，「同一个 uuid 只能用于创建一个审批实例，如果冲突则创建失败并返回错误码 60012」。重发不会产生第二个实例。

这条依据必须写成代码注释，并补一条**正向**单测（`disposition(Retry, 1, true, ...) == RetryAfter(_)`），与既有 `:814` 的反向测试成对——否则后人按「POST 有副作用」的直觉改回 `false` 会静默上线。

### 7.4 错误分类：四桶

| 类别 | 成员 | 处置 |
|---|---|---|
| **频控** | HTTP 429、`99991400`、`1254290` | 退避重试（既有实现，读 `x-ogw-ratelimit-reset`） |
| **审批域瞬态** | `1395001` | 有界退避重试（`max_attempts` 3；该码无 `retry-after` 头，`rate_limit_reset_seconds` 返回 `None`） |
| **幂等命中** | `60012` | **不写字段、不落终态**：走 §7.5 回捞 |
| **终态** | `1390001`（表单校验）、`1390013`、`1390015`（定义停用）、`1254024`/`1254302` 等 | 写回填字段 |

**`token 失效`单列一类**：`99991663`/`20013` 在 `outbound.rs:456-460` 是 `Give` 且**不退避**（退避换不出新 token）。正确做法是上层调 `invalidate_and_refresh` 强制刷新一次后重放，照搬 `bitable.rs:680-696` 的既有写法。设计初稿把它写成「可重试 → 退避重试」是错的。

**「5xx 可重试」只在无业务码时成立**：带非 0 业务码的 5xx 走业务码分支落 `Fatal`（`outbound.rs:253-277`），只有拿不到业务码时 5xx 才进退避（`outbound.rs:280-286`）。

### 7.5 `60012` 的回捞路径

**这是根因 A 的修正，是消除静默故障的关键。**

`60012` 的响应体**不含 `instance_code`**（只有成功响应才有），所以设计原稿「重复处理时返回 60012 → 视为幂等成功」在天真的实现下会卡死：拿不到 `instance_code` → 落 `Fatal` → 写进回填字段 → 该行永久钉死。

**官方提供了恢复手段**：`instance/get.md:26` 明文——「如果在创建的时候传了 `uuid` 参数，则本参数也可以通过传 `uuid` 获取指定审批实例详情」。

所以 `60012` 分支的固定动作是：

```
create 返回 60012
  → GET /open-apis/approval/v4/instances/{uuid}      // instance_id 位置直接传 uuid
      ├─ 成功 → 一次调用同时取回 instance_code 与 serial_number
      │         → 落库 state=created → 继续正常回填
      └─ 返回 1390003（instance code not found）
                → 说明实例确实不存在，回到 create 重试（小退避、有限次）
                  不要立刻落终态：并发场景下「60012 成立而 get 暂时 1390003」是可能的竞态窗口
```

对账键是 **uuid，不是 `serial_number`**（后者无法反查实例，`approval-related-faqs.md:144`）。

**优先方案**：把 `create` 写成「先按 uuid 查、查不到再 create」的 get-then-create，从根上消除窗口。`60012` 回捞则作为第二道保险保留。

### 7.6 「清空字段重试」的适用边界

用户选定「失败信息写回填字段、人工清空即可重试」。这条通道**只在实例未创建成功时有效**：

| 情况 | uuid 是否被占用 | 清空字段重试 |
|---|---|---|
| 必填缺失（本地判定） | 否 | ✅ 有效 |
| `1390001` / `1390013` / `1390015`（前置失败） | 否 | ✅ 有效 |
| `60012` 回捞后确认实例已存在 | 是 | ❌ 无效（回捞已收敛为成功，无需重试） |

所以：**已成功建单的记录不要清空该字段重试**。这条必须写进运维文档。

设计初稿「清空字段重试」与「uuid 永久唯一」并列而不加边界，是自相矛盾的。

### 7.7 限速

- **创建接口 100 次/分钟**（`instance/create.md:11`）。全局令牌桶串行发送，取 90/分钟留余量。
- **令牌桶放 Redis**，不用进程内内存——多实例（蓝绿期间新旧并存）会各自持桶，把配额打爆。
- **批量任务串行**：同一配置的下一次批量任务在上一轮未完成时拒绝（幂等，返回已受理），避免两轮处理同一批。
- 单条（行内按钮）**不走批量队列**，直接处理。并发点击同一行由 `feishu_approval_task.record_id` 唯一索引 + `FOR UPDATE` 认领挡住。

> 注意 100 次/分钟是**应用级**还是租户级，本地文档未明文。同一对 `app_id`/`app_secret` 下的所有配置共享这一个桶。

## 八、配置与启动

新增配置项落在 `src/config/mod.rs` 的 `FeishuSettings`（现状见 `src/config/mod.rs:568-626`）：

- `approval_dispatch_enabled: bool` — 总开关
- `approval_scan_interval_seconds: u64` — 后台 worker 的认领轮询间隔
- `approval_create_rate_per_minute: u32` — 令牌桶速率（默认 90）
- `approval_base_timezone: String` — 多维表格日期的时区（IANA 名，供 `date` 转换器用）

`app_secret` **只经 secret 目录**（`src/config/source.rs:486-488`，文件名 `feishu_app_secret`），不提供环境变量入口——它是租户级凭证。本设计**不新增凭据**，复用既有那一对 `app_id`/`app_secret`。

**scope 变更**：应用需新增 `approval:approval` 或 `approval:instance`（`instance/create.md:13`），以及读 `approval:approval:readonly` 或 `approval:approval`（`approval/get`）。**scope 扩大后必须重新发布应用**，且同一条 `tenant_access_token` 会同时具备「写选项文案」与「创建审批实例」两种能力——见 §9.3。

**worker 注册**：抄 `src/infrastructure/feishu_pull.rs` 的骨架（watch 通道 + JoinHandle + 优雅关闭），在 `src/bootstrap.rs` 的 worker 阶段注册，参与 readiness 与关闭预算。

**建表**：按 `src/infrastructure/schema.rs` 的 `TableSpec`/`fields!` 声明。**禁止新增 SQL 迁移文件**（`docs/contracts/SCHEMA.md:3`）。注意 schema_sync **只增不删**，且外键一旦声明即永久存在——所以本设计的三张表**不互相声明外键**，改用 `config_id` 整数列 + 应用层校验。

## 九、安全

### 9.1 端点鉴权

复用 `ManagementTokenMiddleware`（`src/addon/feishu/domain/middleware.rs`，与 `upsert_options.rs:146`、`delete_options.rs:80` 同一模式）。注册用 `.public()` 跳过框架 JWT，由中间件校验静态 Bearer。

**`base_token`/`table_id` 来自请求体，必须校验**：只允许落在**已配置**的 `feishu_approval_config` 行内。否则持 Token 者可操作任意多维表格——这是越权面，不能只靠 Token 保密。

### 9.2 错误信息泄漏

写进多维表格的文本会被**业务表格的读者**看到，范围远大于系统运维。因此写回的错误信息必须**过滤后**再落表：

- 带上**控件 id / 字段名级别**的定位信息（这是用户能行动的部分，如「缺少『申请人』」）。
- **不要**原样回灌飞书响应体（可能含内部 request_id、租户信息、堆栈片段）。`outbound.rs` 的 `summarize` 已对错误体做过收束，落表前再按白名单裁剪一次。

### 9.3 Token 能力膨胀

现有管理 Token 是**全局单值、不按数据源绑定**（`docs/architecture/feishu-option-ingest.md` 决策 A8）。新端点复用它，等于给这把 Token 加上「**以任意用户身份创建审批实例**」的能力——而它今天只用来写选项文案。

缓解：`base_token`/`table_id` 白名单校验（§9.1）把可利用面收敛到已配置的多维表格。**若使用方认为这仍然过宽，应为本端点单独签发一把 Token**（需要改 `ManagementTokenMiddleware` 支持多 Token 或按路由分组），这是一条独立决策，本文不预设。

### 9.4 审计

该仓库要求高权限操作按 `docs/contracts/AUDIT.md` 落 append-only 审计。本动作以系统身份**创建真实审批单据**（有外部副作用、可被审计追责），因此：

- 每条创建成功落一条审计事件（含 `config_id`、`record_id`、`instance_code`）。
- 批量任务落一条任务级审计（受理人、批次数、成功/失败计数）。
- **不挂 Step-up**：这是系统对系统的自动化动作，无自然人操作者；Step-up 的语义是「自然人重新证明身份」，套在这里没有意义。此处需要在使用方评审时确认。

### 9.5 可观测性

按 `docs/contracts/OBSERVABILITY.md`：

- 指标：创建成功/失败计数（按错误码分桶）、令牌桶等待时长、任务队列深度、租约超时次数。
- 告警：可重试重试耗尽、`60012` 回捞失败、定义停用（`1390015`，这类会让整批失败，值得告警）。

## 十、验证溯源

本设计经两阶段对抗验证：6 个维度并行核查（飞书 API 事实、仓库适配、并发与故障恢复、配置模型、安全运维、完备性）→ 每条发现由 3 个不同视角独立反驳（官方文档明文依据 / 仓库代码事实 / 构造翻车场景）。

结果：110 条原始发现，验证前 12 条，**确认 11 条、驳回 1 条**。

| 根因 | 确认条数 | 已并入 |
|---|---|---|
| A：`60012` 无归宿（响应体不含 `instance_code`，落 `Fatal` 后被写字段永久钉死） | 6 | §5.4、§7.2、§7.5 |
| B：`outbound.rs` 失败分类不可直接复用（`1395001` 被判终态、`idempotent` 必须显式置真、token 失效处置写反） | 3 | §7.2、§7.3、§7.4 |
| C：「回填字段为空」既当扫描键又当结果字段（填一半的行被永久钉死、回写失败无法退出扫描集） | 2 | §5.2、§6.3、§7.1 |
| D：状态表只有记账职责没有恢复职责（蓝绿 `docker stop` 静默杀掉处理中批次） | 1 | §5.2 时序要求、§5.3 |

**被驳回 1 条**：「『表级互斥』在现有架构里有落地基础」——核实全仓无任何进程级锁/租约原语，该主张不成立。本设计已改为不依赖它（§7.7 用 DB 唯一索引 + 行锁认领）。

**三处纠偏**（验证过程中对原始发现的措辞修正，避免修正方案开偏）：

1. 「清空字段重试不可用」**只在实例已建成功时成立**；前置失败时 uuid 未被占用，清空重试有效。已按 §7.6 的边界表写入。
2. 「拿不到编号」严格说只覆盖「create 响应已到、`instance_code` 未落库」这条窄缝；但初稿完全由 `isEmpty` 驱动、worker 不回查落库状态，所以即便落库了下一轮仍会重新 create 撞 `60012`。已按 §5.2 的扫描键修正。
3. 初稿「限流 `1395001`」是术语错误，`1395001` 是审批域瞬态服务端错误，真正的频控是 `429`/`99991400`。已按 §7.2 订正。

## 十一、已知限制

用户已明确接受或暂不处理的项，实现时**不要**自行加码：

1. **不加「可提交」闸门字段**（用户决策）。后果：`审批编号` 字段既承担输出又隐含「是否需要处理」的语义，草稿行/测试行没有地方表达「暂不处理」。缓解见 §6.3（必填缺失走等待态不写字段），但**草稿行若字段齐全仍会被误提单**——使用方需自行保证表里不放测试数据，或靠不点页面按钮规避。
2. **不做审批结果回流**（用户决策）。`instance_code` 已落库（§5.3），将来加回流时不需要重建数据。
3. **一个配置对应一个审批定义**。同一张表里不同行走不同审批定义不受支持。若将来需要，扩展点是 `feishu_approval_config` 加一条「按字段值选择配置」的规则表。
4. **改配置换审批定义会再建一个实例**。`approval_code` 纳入了 uuid 派生（§5.4），所以同一行的旧 uuid 不会阻挡新建——语义是「换了定义就是新单子」，使用方需知悉。
5. **`serial_number` 立即可查性未验证**。文档未承诺，缓解见 §6.4（`1390003` 判可重试而非终态）。
6. **`is_external` 字段名与「不支持的控件」的确切判据未验证**。本地文档只有控件清单，没有 `approvals/get` 响应中判定 `is_external` 的字段名，也没有「定义含不支持控件时是构造时报错还是调用时报错」的明文。实现前需用真实审批定义实测一次。
7. **`serial_number` 的稳定时间窗**。刚创建后立即 `get` 是否稳定返回，以及是否有延迟窗口，未知。若实测发现窗口较长，`140003` 之外还需容忍 `serial_number` 为空但实例已存在的响应。
8. **批量任务的「立即返回 accepted」不含进度查询**。工作流侧拿不到处理进度，只能看表格回填结果。若需要进度，扩展点是加一个查询端点（用现有管理 Token 鉴权）。

## 十二、待实测清单

实现前必须用真实凭据跑一次（CI 无凭据，无法自动化）：

| # | 待测 | 影响 |
|---|---|---|
| M1 | 工作流 HTTP 节点按状态码还是响应体 `code` 判成败 | 决定鉴权失败会不会静默显示成功（§4.2 已标风险） |
| M2 | `serial_number` 创建后立即可查性 | §6.4 退避策略的参数 |
| M3 | `60012` 回捞（`instance/get` 传 uuid）的真实响应 | §7.5 的落地依据 |
| M4 | `is_external` 字段名、不支持控件的确切判据 | §6.1 校验的实现 |
| M5 | 100 次/分钟是应用级还是租户级 | §7.7 令牌桶是否需要跨应用协调 |
| M6 | 单条链路端到端耗时 | 确认在 60 秒超时内（§3.1） |
| M7 | **测试审批定义** `D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E`（`往来付款类型/Current Payment Type_正式流程_自动化测试`）的 `form` 是否只含 API 支持的控件、`is_external` 是否为 `false` | §6.1 的前置校验要拿它做第一次实测。**该定义已在其表单里用外部数据源调本系统**，所以两端的凭据与网络连通性已就位——只是**创建实例**这条新链路还没跑过 |

## 十三、实施顺序

前 5 步互相独立，可并行；第 6 步依赖前 5 步：

1. **`outbound.rs` 扩表**（§7.2、§7.3）：审批域可重试集合、`60012` 独立类别、`fatal_hint` 文案、`idempotent` 正向单测。这一步**不改既有行为**，可以先合。
2. **建三张表**（§5.1、§5.2）：`TableSpec`/`fields!` 声明 + `src/infrastructure/schema.rs` 断言同步。
3. **审批 API 客户端**：`instances create` / `instances get` / `approvals get` 的请求构造与响应解析，落在 `src/addon/feishu/domain/approval.rs`（机制代码进 `domain/`）。
4. **多维表格写路径**：`records/batch_update`，落在 `domain/bitable.rs`。现有该文件只有 GET 类端点，需新增写侧。
5. **dispatch Action**：新 Action 文件（一 Action 一文件，`scripts/check_architecture.py` 门禁），走 `ManagementTokenMiddleware`。
6. **后台 worker**：抄 `feishu_pull.rs` 骨架，认领 SQL 抄 `authorization_outbox`。

**上线顺序**（有依赖，不可乱）：

1. 飞书应用加 scope → **重新发布**（不发布则后续全部失败）。
2. 审批管理后台创建目标审批定义（只含支持的控件）→ 拿 `approval_code`。
3. 多维表格加「申请人」（人员）与「审批编号」（文本）两个字段。
4. 系统部署新版本 → 在控制台配一条 `feishu_approval_config`（保存时会做 §6.1 的前置校验，这一步就是校验点）。
5. 配工作流的两个按钮。
6. 先用行内按钮在**一行测试数据**上跑通，再用页面按钮跑存量。

**回滚**：把 `approval_dispatch_enabled` 置 false 即可停止新任务。**已创建的审批实例不能通过本系统撤回**（要撤回需逐单调 `instances cancel`）——这是回滚的硬边界，需要向使用方说明。
