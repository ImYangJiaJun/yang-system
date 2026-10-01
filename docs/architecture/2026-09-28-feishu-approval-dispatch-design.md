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
          { "value_type": "text", "value": "\",\"requested_by\":\"" },
          { "value_type": "ref", "value": "$.step_btn.user" },
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
> `requested_by` 引用 `$.step_btn.user`（行内与页面按钮的触发器输出均有 `user`；该值形态
> 待实测，见 §12 M11；未带的旧工作流不破坏，落库记 `feishu-workflow`，见 §5.5）。
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
| `base_timezone` | Str | Base 时区（IANA 名），日期控件折 RFC3339 用 |
| `enabled` | Radio | 启用/停用 |
| `form_snapshot` | Text | `approvals/get` 返回的控件结构快照（JSON） |
| `form_snapshot_at` | Datetime | 快照时间 |

**唯一索引 `(base_token, table_id)`**（实现期补）。理由不是「防重复」而是**并发首调**：
两个工作流同时点第一次按钮时会各建一份配置，此后每次派发读到哪一份都不确定（两份
的映射可能不同）。有唯一索引时后到的那次插入直接失败，被 `dispatch` 端点折成
「配置已存在」，语义正确且不需要额外加锁。

**`base_token`/`table_id` 同时是白名单**（§9.1）：派发端点强制校验坐标落在**已配置且
启用**的行内。管理 Token 是全局单值、不按数据源绑定，光靠它保密等于把「以任意用户
身份创建审批实例」的能力开放到所有协作表格上。

**为什么是独立表而不是复用 `feishu_datasource`**：后者是「外部选项的数据源」，语义是「这个字段的可选值从哪里来」（`docs/architecture/feishu-option-ingest.md:16-18`）。本表是「这张表的这批行要创建什么审批」，两者不共享生命周期，也不共享读写路径。复用会迫使两套完全不同的语义挤在一张表里。

映射项独立成表 `feishu_approval_field_map`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | Key | |
| `config_id` | Int, indexed | 指向配置行 |
| `widget_id` | Str | 审批控件的 id（来自 `approvals/get`） |
| `widget_type` | Str | 控件类型（快照，用于选转换器） |
| `bitable_field` | Str | 多维表格字段的 **field_id** |
| `bitable_field_name` | Str | 建配置当时的列名，**仅供控制台展示** |
| `required` | Bool | 快照的必填标志 |
| `converter` | Str | 转换器标识（见 §6.2） |
| `option_map` | Text | 选项映射（单选/多选的选项名 ↔ 选项 value），JSON |

**用 `field_id` 而不是字段名作 key**：用户在多维表格里改列名会静默失配（现有 `feishu_datasource_field` 表同时存 `field_id` 与 `field_name`，`src/addon/feishu/datasource/domain/field_table.rs:13-84`，本设计对齐该做法——存 id 作 key，另存 name 仅供控制台展示）。

> **但「存 id」只适用于本服务的库，不适用于飞书接口的入参。** 飞书**记录类**接口一律
> 按**列名**定位字段（见 §6.5），所以每次调用都要在边界上把 id 折成当前列名。这条
> 是实测踩出来的——初稿只在**读**侧做了折算（`rekey_cells_by_field_id`），**写**侧
> 漏了，于是回写带着 `fld…` 去打一个按名匹配的接口。

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

### 5.5 派发请求记录表 `feishu_approval_request_log`（2026-09-30 承接追加）

每次打到派发端点的请求在 `feishu_approval_request_log` 落一行记录（**入口请求一记**，
单条与批量都记；表字段定义见 `docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md` §3）。
与 `feishu_approval_task`（§5.2）的分工：task 表承载逐条处理状态机，本表只记「这一请求
打进来、结果如何」——单条同步请求记最终结果（编号或失败原因），批量请求记「已受理」，
逐条结果由 task 表承载、控制台可下钻。请求人取请求体 `requested_by`（§4.1，工作流模板
引用 `$.step_btn.user`），未带记 `feishu-workflow`。落库用独立连接、写失败仅降级
`tracing::error!`，绝不影响派发结果；管理 Token 鉴权失败与反序列化 400 不落此表。
本表由纯绑定表 module `feishu.approval_request_log` 承载（无 Action，照
`approval::build_task_module` 形态）。实现与详细设计见
`docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md`。

## 六、处理流程

### 6.1 前置校验：首次派发时自动建配置

**默认链路不需要人工配映射。** 使用方的多维表格列名与审批控件名**严格对应**，所以
配置改由**首次派发时自动创建**：端点发现 `(base_token, table_id)` 没有配置行时，就
取定义、取列、**按名匹配**、校验，全过才在同一事务里写入配置 + 映射。

调用方为此必须在请求体里给出三条**只有它知道**的信息：`approval_code`（提哪个定义）、
`applicant_field`（谁当发起人）、`backfill_field`（编号写回哪列）。三条**要么全给、
要么全不给**——半套的后果是「配置建了但没有回填列」，而校验失败正是要写进回填列的
（§6.1.2），没有它错误就只剩 HTTP 响应，页面按钮拿不到。

#### 6.1.1 按名匹配（`approval_match`）

| 规则 | 判据 |
|---|---|
| 控件 ↔ 列 | 控件 `name` 与列 `field_name` 都 `trim` 后**精确相等** |
| 一列只服务一个控件 | 按 `field_id` 判重（用列名当键会漏掉「两条控件指向被改过名的同一列」） |
| 同名多列 | 报错，不挑一列——飞书按名字取值会取到不确定的那一列 |
| 固定选项 | 控件 `option[].text` ↔ 列 `property.options[].name`，取 `option[].value` |

两个关键坐标（申请人、回填列）接受 **`field_id` 或列名**：请求体由人在工作流的
`raw_body` 里写死，两种形态都自然。**优先按 id 匹配**（id 稳定，列名可改可重名），
但**存储一律用 id**。

#### 6.1.2 三项校验与「一次报全」

1. **三方定义**：`approvals get` **不返回 `is_external`**（实测，该字段只在
   `approvals/search` 里）。判据改为「**`approvals get` 拿不到 `form`**」——三方定义
   走 `external_approvals` 另一个资源，取不到表单。
2. **不含 API 不支持的控件**：`text`、`mutableGroup`、`serialNumber`、各类
   `*Group`（见 `lark-approval-instance-form-control-parameters.md:14-30`）。
3. **不含需人工准备值的控件**：`address`（地理库 id）、`connect`（已存在的
   `instance_code`）、`attachment*` / `image*` / `document`（file code）。
   **必填的拦下、可选的放行**——留空提交对可选控件是合法的。

**失败原因一次返回全部**，不是遇到第一个就停：改一条跑一轮会让使用方来回多轮。
原因落到两个地方——HTTP 响应（工作流日志）+ **回填列**（表格里的人），后者的文案带
`[配置]` 前缀，与 `classify_failure` 写的**数据类**错误区分开（同一个单元格里会同时
出现「缺必填列」与「配置校验不过」，处置完全不同）。

任一校验不过时**一个字节都不落库**：半套配置（有配置没映射）会让之后每次派发都卡在
「该配置没有字段映射」，而配置行看着是好的。

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

#### 6.2.1 `option` 是**多态字段**（实测）

同一个 `option` 键在不同控件上分别是**缺失 / `null` / 数组 / 对象**：

| 形态 | 实测出现在 | 语义 |
|---|---|---|
| 缺失 | `input` | 该控件没有选项概念 |
| `null` | `input`（另一个控件） | 同上，但**键存在** |
| `array` | `radioV2`（固定选项） | 固定选项，`text`/`value` 可派生 |
| `object` | `fieldList` / `connect` | **不是选项**，是该控件的配置 |

所以判别器是**两维**的：`type` 决定控件语义，`option` 的**类型**决定值能否派生。
用 `Option<WidgetOptions>`（untagged 枚举外面套一层 `Option`）表达——这是唯一**不依赖
变体声明顺序**的写法：untagged 的单元变体会抢在数组与对象之前匹配，把后面两个 arm
全挡死（实测踩过）。

另外实测 `externalData.key` 是**空字符串**，不能当取数源；链接态要认的是
`externalDataLinkage == true`（且 `rename = "externalData"` 不能省——字段名是驼峰，
漏了它该位恒为 `None`，症状是「链接态被当成非链接」）。

#### 6.2.2 需要人工准备值的控件

`address`、`connect`、`attachmentV2`/`image` 这几类需要用户直接提供 token/id
（`lark-approval-instance-value-sourcing.md:88-97`），本设计**不自动准备**——配置期就
明确报错而非静默传空（静默传空会让飞书收到一个看似合法的空控件，错误信息指向飞书
而不是指向配置）。

#### 6.2.3 空值与「不传」

- **空**有多种形态：`null`、空串、空数组、以及 `{"text": ""}` 这类只有空内容的
  多态单元格。最后一种最容易漏——只看 `map.is_empty()` 会把它判成有值，于是必填
  校验放行、飞书侧报 `1390001`，错误指向飞书而不是指向这份数据。
- **可选控件在空值时整个 JSON 都不能传**（官方 FAQ `approval-related-faqs.md:86-89`）：
  一旦传入控件 JSON，就必须设 `value`，否则接口报错。

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
> **`serial_number` 随创建立即可查——已真机实测（2026-09-28）**：创建后立刻 `instances get`
> 就拿到了编号（见 §12.3）。但 `1390003`（instance code not found，`instance/get.md:512`）
> 仍判**可重试**并退避、**绝不写终态**——那是并发窗口的语义，与「通常查得到」不矛盾。
> 否则刚建成的实例会被写成「找不到」，反而制造第二条不可恢复路径。

### 6.5 回写

按 §7.1 的分批规则 `POST .../records/batch_update` 写回填字段。

**`records[].fields` 按「列名」作键，不是 `field_id`。**《数据结构概述》：
「`fields` 字段为 map 型，由**字段名称**和其具体内容的键值对组成」，`key` 一栏写的就是
「多维表格数据表中的字段名称」；官方每个写示例（`batch_create` / `batch_update` /
`update`）也都是列名。

而本服务的配置里存的是 `field_id`（§5.1，刻意如此——改列名不该让配置失效）。两条轴
不同源，所以**每次派发都要在边界上折一次**，与读侧的
`bitable::rekey_cells_by_field_id` 正好是同一件事的两个方向：

```
配置行 backfill_field = "fldSerial"     ← 存 id
        ↓ bitable::resolve_field_name(&fields, "fldSerial")
写请求 fields = {"审批编号": "202609280001"}   ← 用名
```

用 `resolve_field_name` 而不是就地 `find`：它同时拦「列被删了」与「**列名不唯一**」。
后者尤其重要——按名写的接口在重名前会写到不确定的那一列上，那比「写不进去」更坏。
所以 `DispatchInput` 的对应字段叫 `backfill_field_name` 并要求调用方传**列名**：
让「写到哪一列」在调用处一眼可见，而不是埋在一个看着像 id 的字符串里。

> 这条是**端到端 mock 实测**逼出来的：初稿只在读侧折算了 id→名，写侧直接把配置值当
> 键发出去。更刺眼的是，同一个 worker 里**上一步**刚用 `resolve_field_name` 把同一个
> id 折成列名去填筛选器（`filter.field_name` 必须用名），下一步回写却用回了 id——
> 一个函数里两套口径。初次真机联调请确认飞书是否**也**接受 id 作键（官方错误表里
> `1254044 FieldIdNotFound` 与 `1254045 FieldNameNotFound` 并存，即 id 可能也认）；
> 按列名写在「只认名」与「都认」两种假设下都成立，按 id 写只在后者成立，故取前者。

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
5. ~~**`serial_number` 立即可查性未验证**~~ → **已实测**：创建后立刻即可查到（§12.3）。
   缓解措施（`1390003` 判可重试而非终态）保留，它管的是并发窗口。
6. ~~**`is_external` 字段名与「不支持的控件」的确切判据未验证**~~ → **已实测**：
   `approvals get` **不返回 `is_external`**（该字段只在 `approvals/search` 里），判据
   改为「`approvals get` 拿不到 `form`」；**不支持的控件清单**以
   `lark-approval-instance-form-control-parameters.md:14-30` 为准，**定义里存在且必填
   就拦**（可选控件留空提交是合法的）。详见 §6.1.2。
   遗留：`search_launchable` **只支持 `user_access_token`**，`tenant_access_token`
   用不了——所以「列出可用定义」这类功能不要指望它。
7. ~~**`serial_number` 的稳定时间窗**~~ → **单次实测无窗口**（创建后立刻可查，§12.3）。
   实现仍按「为空即判可重试」写，不依赖这一点。
8. **批量任务的「立即返回 accepted」不含进度查询**。工作流侧拿不到处理进度，只能看表格回填结果。若需要进度，扩展点是加一个查询端点（用现有管理 Token 鉴权）。

## 十二、待实测清单

实现前必须用真实凭据跑一次（CI 无凭据，无法自动化）：

| # | 待测 | 影响 |
|---|---|---|
| M1 | 工作流 HTTP 节点按状态码还是响应体 `code` 判成败 | 决定鉴权失败会不会静默显示成功（§4.2 已标风险） |
| M2 | ~~`serial_number` 创建后立即可查性~~ **✅ 已实测** | §6.4——立即可查，见 §12.3 |
| M3 | ~~`60012` 回捞（`instance/get` 传 uuid）的真实响应~~ **✅ 已实测** | §7.5——回捞可行，且**一次拿全**，见 §12.3 |
| M4 | `is_external` 字段名、不支持控件的确切判据 | §6.1 校验的实现 |
| M5 | 100 次/分钟是应用级还是租户级 | §7.7 令牌桶是否需要跨应用协调 |
| M6 | 单条链路端到端耗时 | 确认在 60 秒超时内（§3.1） |
| M7 | **测试审批定义** `D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E`（`往来付款类型/Current Payment Type_正式流程_自动化测试`）的 `form` 是否只含 API 支持的控件 | §6.1 的前置校验要拿它做第一次实测。**该定义已在其表单里用外部数据源调本系统**，所以两端的凭据与网络连通性已就位——只是**创建实例**这条新链路还没跑过。⚠️ 该定义含 `connect`/`attachmentV2`/`fieldList`，且 `办公地点(Office location)` 是 `option: []` 又未链接外部数据源（派生不出选项）——**拿它直接联调会被配置期校验正确拒绝**；端到端联调需要另建一个只含文本/数字/日期/单选/多选的测试定义 |
| M8 | **`batch_update` 的 `fields` 是否也接受 `field_id` 作键** | §6.5。官方错误表里 `1254044 FieldIdNotFound` 与 `1254045 FieldNameNotFound` 并存，即 id 可能也认；文档的规范表述与全部写示例都指向**列名**。实现已按列名（两种假设下都成立）。验证方式：对一条**已存在**的记录，用 `field_id` 作键写回它**现有的值**（内容不变的写），看回包是 `0` 还是 `1254044` |
| M9 | **`records/batch_get` 是否真的没有 `field_names`** | §6.5 / 读路径。实测请求体只有 `record_ids` / `user_id_type` / `with_shared_url` / `automatic_fields`，`field_names` 只出现在**错误表**里——实现已去掉该投影。若某个租户上它其实接受，投影回退属于优化而非正确性 |
| M10 | **`isEmpty` 筛选的 `value` 必须是 `[]`** | 播种扫描（§5.2）。传 `null` 或不传会吃 `1254018`。实现按空数组，已由 `empty_field_filter` 钉住 |
| M11 | **`$.step_btn.user` 的形态（纯字符串或对象，对象则取子字段）** | §4.1 的 `raw_body` 补 `requested_by` 段（引用该值）。行内与页面按钮的触发器输出均有 `user`；未带的旧工作流不破坏，落库记 `feishu-workflow`。真机联调时确认 |

### 12.1 本地文档副本与真实响应不符之处（已实测，实现按实测）

飞书开放平台的文档**不能当作准确依据**。以下是本功能实测推翻了本地文档副本的七处，
每一处都会变成**静默故障**或**误导性报错**——所以写代码前先用 `lark-cli` 对着真实租户
取一次报文（只读），再照着实测写：

| # | 文档说 | 实测是 | 不对时的症状 |
|---|---|---|---|
| 1 | 控件 `name` 「必须以 `@i18n@` 开头」 | 服务端已解好的可读中文（`公司名称/Company name`） | 按名匹配的前提；信文档就走不通 |
| 2 | `option` 是选项数组 | **四种形态**：缺失 / `null` / 数组 / 对象 | 解析崩或静默判成「有选项但派生不出」 |
| 3 | `externalData.key` 是取数源 | 实测是**空字符串** | 拿它查数据源必然查空 |
| 4 | `records/batch_get` 有 `field_names` | 请求体**没有**该参数（只在错误表里） | 轻则被忽略、重则整批 400，而错误指向「字段名不匹配」 |
| 5 | （未言明） | 响应 `fields` **按列名**作键，而配置存 `field_id` | **每条记录都「缺少申请人」，不报错**——最危险的一处 |
| 6 | `record_id` 是记录字段 | 它是**响应里的系统字段、不可过滤** | 拿它当 `filter.field_name` 得不到按 id 的定位 |
| 7 | `isEmpty` 的 `value` 随便 | 必须是**空数组** `[]` | 吃 `1254018` |

另有两处**接口能力**的实测结论：`approvals get` 不返回 `is_external`（§6.1.2）、
`search_launchable` 只支持 `user_access_token`（§11 限制 6）。

这些结论都钉成了测试夹具（先例：
`approval_match::tests::real_form_shape_parses_with_all_four_option_kinds` 直接喂真实
响应字节）。本地文档副本在 `docs/reference/feishu/`，可作**线索**，不可作**结论**。

### 12.2 端到端 mock 实测（合成数据，不依赖真实租户）

真实定义 `D0557DA6-…` 不能用来联调（见 M7），所以另有一组**合成**定义 + 合成列名的
端到端用例（`approval/actions/dispatch.rs` 的 `tests::end_to_end_*`）：传输换成脚本化
回放并记录每个真实请求体，数据库换成惰性连接池，其余全是生产代码。

它验的是**接缝**——那是单测证明不了、又最容易静默出错的地方：

- **正路**：6 个控件按名匹配 → 列名解析成 `field_id` → 转换器与选项映射 → 读记录 →
  按 id 重映射 → `form` 组装 → 创建 → 取编号 → 回写。断言逐字段核对**发给飞书的两个
  请求体**：`form` 是压缩后的 JSON **字符串**、数字传 JSON 数字、日期折成
  `2026-09-28T00:00:00+08:00`、单选传字符串而多选传数组、可选控件空值时整个 JSON 都
  不传、`uuid` 由 (表格坐标, 定义, 记录) 派生。
- **错路**：必填控件在表里没有同名列 → 建配置失败、原因**点名**该控件、并带 `[配置]`
  前缀写进回填列；同时钉住「校验不过时一个字节都不落库」。

两条结论都做过**变异验证**（改完即还原）：把 `rekey_cells_by_field_id` 改成不换键 →
正路用例红在「重映射后必须能按 `field_id` 取到申请人」；去掉 `[配置]` 前缀 → 错路用例
红在「必须带 `[配置]` 前缀」。**§6.5 的写键缺陷正是这一轮实测发现的。**

### 12.3 真机实测（2026-09-28，用**本系统自己的飞书应用**提单成功）

第一次真实调用 `POST /open-apis/approval/v4/instances` 就成功建单，用的是**本系统拉取
多维表格数据的那一个应用**（不是 lark-cli 的应用——后者没申请审批 scope，见下）。
定义取 `往来付款类型/Current Payment Type_测试`（`D0557DA6-…`），表单全部用模拟值。

| 结论 | 实测证据 | 影响到哪 |
|---|---|---|
| **`/instances` + `tenant_access_token` 可用** | `code: 0`，`instance_code = 2A1B9E76-…` | §6.4 的路径成立；本服务正是这么调的 |
| **该端点拒绝 `user_access_token`** | `99991668 user access token not supported` | 只能用应用身份。lark-cli 的**用户态**提单走的是**另一个端点** `/instances/initiate`——本服务不要用它 |
| **`serial_number` 立即可查** | 创建后立刻 `get` 得 `202609280098` | **M2 关闭**（§6.4 的退避仍保留，管的是并发窗口） |
| **`60012` 是 HTTP 200 + `code: 60012`** | 同 uuid 重发 → `{"code":60012,"msg":"uuid conflict"}`，**响应体没有 `data`、没有 `instance_code`** | §7.5：除按 uuid 回捞外无路可走——这条实测坐实了回捞路径的必要性 |
| **`instance_id` 位置可以传 uuid** | `GET /instances/<uuid>` → `code: 0`，返回同一个 `instance_code` **且同时给出 `serial_number`** | §7.5 的落地依据；**回捞一次拿全**，不必再打一次详情（设计里省的那一次调用成立） |
| uuid **不区分大小写** | 传小写、详情里回显大写 `E2E00000-…-D0` | 派生值大小写无所谓 |
| **实例不存在 = `1390003` + HTTP 400** | `GET /instances/<不存在的 uuid>` → `{"code":1390003,"msg":"instance code not found"}`，**HTTP 400** | §7.2：**必须按 body 业务码判成败**，按 HTTP 状态码判会把「不存在」判错 |
| **必填控件可以不传** | 必填的「办公地点」（`option` 是空数组）与「附件」（`attachmentV2`）**整个控件不传**，创建成功 | §6.1：本系统的配置期校验**比 API 更严**（有意为之）；API 本身会接受残缺提单 |
| `approvals get` 的 `form` **不返回 `text`（说明）控件** | 定义里查不到，实例详情里却回显了三个 `type: "text"` | §6.1 的 `form_snapshot` 与实例真实表单会有这点差异；`text` 本就不可提单，无害 |
| **单选发的是 id、读回的是文案** | 发 `{"type":"radioV2","value":"mpuvnw0h-…-0"}`，读回 `value: "个人"` + 兄弟字段 `option: {key:"mpuvnw0h-…-0", text:"个人"}` | §6.5 附近的读写口径：**写用 id、读得文案**，不对称，回流功能要注意 |
| `fieldList` 的 `option.input_type` 定义里是 `LIST`、实例回显 `FORM` | 同上 | 无实际影响，别拿它做判据 |
| **`open_id` 是应用维度的** | 传的 open_id 属于另一个应用，实例解析出的发起人是另一个 `open_id` / `user_id` | 官方 `1254066` 明确警告不能跨应用交叉使用。本设计**正确**：申请人 open_id 由本应用从多维表格人员列读出，读的与提的是同一个应用 |

**两个应用 scope 的差异（上线前的硬门槛）**：lark-cli 那个应用
（`cli_aa2135b3a1a19bc1`）调应用身份提单直接被拒——

```
99991672 access denied: app … has not applied for the required scope(s):
         approval:approval, approval:instance
```

而本系统自己的应用**已有**这些 scope（所以提单成功）。这正是 §13「上线顺序」第 1 条
「飞书应用加 scope → **重新发布**」要保证的事：**换应用就要重新确认 scope 并发布**，
否则表现就是上面这个 `99991672`。

### 12.4 外部数据源单选的提交与回读（同日第二次实测）

第一次提单漏掉了两个**外部数据源单选**（`办公地点`、`公司名称`）——我按 `approvals get`
的返回保守地整个不传。补做之后拿到了这一组结论，其中两条直接支撑 §6.1 的设计选择：

| 结论 | 实测证据 |
|---|---|
| **`value` 就是本系统 `feishu_option.option_id`** | 发 `fldeysrdna:d22258711825`（成都）、`fldq7lcb6y:dc6b027d9457`（成都市易威行科技有限公司），`code: 0`，实例 `42A39B53-…` 建成 |
| **提交前必须自检级联** | 公司名称那条选项的 `parent_key` 必须**正好等于**办公地点选中的 `option_id`——父不匹配时飞书侧的行为未验证，脚本里先 assert 住 |
| **`approvals get` 的 `externalDataLinkage` 不可信** | `办公地点` 报 `externalDataLinkage: false`、`externalData.key` 是空串，但它**确实**是本系统外部数据源的一列（台账 `feishu_datasource_field`：`field_id=fldeysRdna`、`field_name=办公地点(Office location)`、`source_key=fldeysrdna`）。**判「这个控件是不是我们的外部选项」不能只看这个标志位**——§6.1 的 `load_external_options` **按列名回台账查**，这条实测正好给那个选择补上了依据 |
| **台账的 `field_name` 就是控件名** | 台账里存的正是 `办公地点(Office location)` / `公司名称/Company name`，与控件 `name` **逐字相同**——「列名与控件名严格对应」这条默认链路的前置条件在真实数据上成立 |
| **外部选项的回读是残缺的** | 回读 `{"value": "", "option": {"key": "fldeysrdna:d22258711825", "text": ""}}`——**key 是我们给的 id，text 是空串**；而**静态**选项（收款方类型）回读是 `{"value": "个人", "option": {"key": "mpuvnw0h-…", "text": "个人"}}`，文案齐全 |
| 由此推出的后果 | 将来若做**审批结果回流**（§2 非目标，但 §5.3 已为它落了 `instance_code`）：外部选项**只能拿回 id**，要显示文案必须回查本系统的 `feishu_option`；静态选项则直接有文案。两条路要分开写 |

> 这六条里最值得记住的是第三条：**飞书对「哪些控件用了我们的外部数据源」的自我描述是错的**
> （`externalDataLinkage: false` / `externalData.key: ""`），而**本系统的台账是准的**。
> 这与 §12.1 的基调一致——以实测与本系统的数据为准，不以飞书的字段为准。

### 12.5 外部数据源单选的提交值：**`option_id` 是对的**（本节推翻了自己 12.5 初稿的结论）

补上「办公地点」「公司名称」后页面显示「未填写」，我先误判成「提交值形态错了，应该提交文案」
并写进了本节初稿。**那个结论是错的**，代码不需要改。真正的结论如下。

#### 实验一：`option` 不是入参

按「已解析实例」的完整形态提交（`value` 给文案 + `option{key,text}`）：
**`option` 对象被完全忽略**，飞书只读 `value`，并把它原样当 key 存。所以「把解析结果喂回去」
这条路不通——`option` 是**输出**字段。

#### 实验二：文案形态是错的

提交 `成都` → 存成 `option.key = "成都"`、文案空。历史上还留下过
`option.key = "option not found by 深圳"`（`7E3F5C84-…`）——那是**飞书拿到文案后去反查选项、
没查到**时自己写的，说明文案**不是**它认的身份。

#### 实验三：id 形态是对的（决定性）

`BF7F1A9C-…`（`202609280102`）里：

```
办公地点  value='成都'                        option={"key": "fldeysrdna:d22258711825", "text": "成都"}
公司名称  value='成都龙芯佳和科技有限公司'      option={"key": "fldq7lcb6y:bc6c077547c5",  "text": "成都龙芯佳和科技有限公司"}
```

`option.key` **就是我们的 `option_id`**，而 `value`/`text` 是**飞书自己解析出来的文案**。
所以**正确的提交身份是 `option_id`** —— 也就是 §12.4 的实现（`load_options` 建
`label → option_id`）**本来就是对的**。

> 这个实例不是我们建的：它的 `付款备注` 是我提交那单的原文，`uuid` 也不是我的，说明它是
> **把我的实例在 UI 里「再次提交」**（`allow_submit_again`）产生的。

#### 仍未解决：为什么 API 提单解析不出文案

同一次实验里，纯 API 提交（`42A39B53-…`、`698BE291-…`）的 `option.key` 与
`BF7F1A9C` **完全相同**，但 `value`/`text` 都是空 —— 打开就是「未填写」。

已排除的：**不是时机问题**（事后重读仍是空）、**不是取数端点不通**（后端日志里飞书
确实在调 `POST /api/v1/feishu/approval/options/{source_key}`，带
`linkage_params: {"fldeysrdna": "成都"}`，40 分钟内 13 次）。**是「客户端路径解析、
API 路径不解析」**：`再次提交` 那条路走的是 UI，UI 手里有刚从我们端点拉到的
「id ↔ 文案」对照表，所以能存下文案。

**影响与待决**：值（我们的 `option_id`）**存对了**，单据的路由与下游处理都不受影响；
但**审批人看到的是空的「未填写」**。这条要在联调清单里单列，并决定是否接受。

### 12.6 把「外部选项」这条协议对着规范读完，以及穷举过的五种提交形态

`associate-external-options.md` 是我们这个端点的规范来源，逐条对照后**我们的实现是合规的**：

| 规范要求 | 我们的实现 |
|---|---|
| `data.result.options[].id`：选项唯一标识，全局唯一且固定 | `option_id`（`fldeysrdna:d22258711825`）✓ |
| `data.result.options[].value`：到 `i18nResources` 匹配文案的 Key | `@i18n@<option_id>` ✓ |
| `i18nResources` **必须返回**，返回空会导致**显示是空的**，至少一种语言 | 默认语言恒存在并标 `isDefault` ✓ |
| 提单时「关联外部选项」控件的 `value` 传 **`options.id`** | `option_map` 的 `label → option_id` ✓ |

**但规范里还埋着一句要命的**：

> 配置了**联动参数**（对应 `linkage_params` 参数）或 使用了 V2 版本（对应 `page_token`、`query`
> 参数），**暂未对开放平台做完整支持**。

我们的 `办公地点 → 公司名称` 正是一对联动控件，`linkage_params: {"fldeysrdna": "成都"}`
也确实躺在飞书发给我们端点的请求体里（后端日志）。**开放平台这条路对联动选项本来就不完整支持。**

#### 穷举过的提交形态（都建单成功，都没能在页面显示文案）

| # | 提交的 `value` | 飞书存成 | 结论 |
|---|---|---|---|
| 1 | `fldeysrdna:d22258711825`（`options.id`，**规范要求的**） | `key` = 原值，`text` 空 | 身份对、文案没解析 |
| 2 | `成都`（文案） | `key` = 文案，`text` 空 | 错（历史实例里还留下过 `option not found by 深圳`） |
| 3 | `@i18n@fldeysrdna:d22258711825`（`options.value`） | `key` = 原值，前缀没被剥 | 错 |
| 4 | 形态 1 再加 `option{key,text}` 对象 | **`option` 被整个忽略**，只用 `value` | `option` 是**输出**字段 |
| 5 | 形态 3 再加 `i18n_resources` 把 `@i18n@<id>` 映射成文案 | 单建成了，文案仍空 | `i18n_resources` 确实**只对单行/多行文本生效**（规范明说），不作用于单选 |

**顺带又一处文档不符**：`i18n_resources[].texts` 的类型是 **`i18n_resource_text[]`（`{key,value}` 数组）**，
而该文档的**示例值写成一个 JSON map**。照抄示例会吃
`9499 Invalid parameter type in json: texts`。本次联调实测。

#### 现在的分歧点（需要页面确认才能继续）

唯一带着解析后文案的实例 `BF7F1A9C-…` 是**走 UI「再次提交」**产生的；而纯 API 提交的同形实例
`option.key` 与它一字不差，`text` 却是空。

所以下一步取决于一个问题：**`BF7F1A9C` 在页面上是否也是「未填写」？**

- **也是未填写** → 那 API 回读里的文案根本不参与渲染，问题在**定义侧的联动配置**（规范之外的
  管理后台配置），与我们的提单无关。
- **能显示** → 那么 API 提单的外部选项就是拿不到文案，需要在「接受空显示」与「换掉这两个控件」
  之间做决策。

### 12.7 我方链路已全绿；问题收敛到飞书侧（2026-09-28 收尾实测）

去掉定义里的联动参数后，逐环实测了一遍，**我方每一环都合规**：

| 环节 | 实测结果 |
|---|---|
| 定义侧的关联外部选项配置 | 两个控件的 `externalData.externalUrl` 已填成我们的端点 URL、`token` 已填、`linkageConfigs` 已清空 |
| 定义侧能拿到我们的数据 | 后端日志里飞书**确实**在 POST `/api/v1/feishu/approval/options/{source_key}` |
| 我们端点的响应 | 直接打它（token 只留在服务器内存里）：`{"code":0,"msg":"success!","data":{"result":{"options":[{"id":"fldeysrdna:d22258711825",...8 条],"i18nResources":[{"locale":"zh_cn","isDefault":true,"texts":{"@i18n@fldeysrdna:1bb5e3986433":"深圳",…}}]}}}` —— **与规范逐字段一致** |
| 提单的 `value` | 按规范传 `options.id`（`fldeysrdna:d22258711825`） |

**结论**：`options[].id`、`options[].value`、`i18nResources`（含 `@i18n@<id>` → 文案）、
提单值，四处都对得上规范，端点也实测返回正确的 id 与文案。剩下的**不是我们能改的**——
飞书对 **API 创建**的实例不解析外部选项文案，只有从**客户端**提交（UI 的「再次提交」）
才会把文案写进实例。

#### 对功能的影响与建议

我们的派发链路**只回填审批编号**，不读回这两个选项，所以**功能不受影响**；受影响的是
**审批人在详情页看到的是「未填写」** —— 看不到付款主体，这是实际可用性问题。

三条可选处置（需使用方在审批后台改定义，不是本系统的改动）：

1. **加一个同名文本控件兜底**（推荐）：把公司名称/办公地点**另配**成单行文本，由本系统
   按名字段照常填文案。审批人一定看得到，且不影响现有联动选择。
2. 把这两个控件改成**静态单选**（放弃外部数据源）——39 个公司名要手工维护，不划算。
3. 接受空显示。

> 这一节同时说明：§12.5 里「提交值形态错了」与 §12.6 的兜底猜测**都不是原因**。
> 真正的边界是飞书的**客户端/API 两条路径行为不一致**。

### 12.8 提交形态穷举（11 种）——全部无法让 **API 创建**的实例带上文案

按使用方要求把可能的编码形态扫了一遍。每单最多在**两个**外部选项控件上各放一种形态，
其余控件保持不变。

| # | 提交内容 | 结果 |
|---|---|---|
| 1 | `fldeysrdna:d22258711825`（`options.id`，**规范明文要求的**） | 建单成功；`key` = 原值，`text` 空 |
| 2 | `@i18n@fldeysrdna:d22258711825`（`options.value`） | 原样存，`@i18n@` 前缀没被剥 |
| 3 | `成都`（文案） | 原样存为 key |
| 4 | `d22258711825`（裸 hash） | 原样存为 key |
| 5 | `["fldq7lcb6y:bc6c077547c5"]`（数组） | **`1395006` 拒绝**：「控件值不合法或者为空。控件= widget…」 |
| 6 | `{"id":…}` / `{"value":…}`（对象） | **`1395006` 拒绝**（同上，且**点名控件 id**） |
| 7 | 形态 1 再带 `option{key,text}` | `option` **被整个忽略** —— 它是**输出**字段 |
| 8 | 形态 2 再带 `i18n_resources`（`{key,value}` 数组形态） | 单建成了，文案仍空；规范确说 `i18n_resources` 只对单行/多行文本生效 |
| 9 | `type: "radio"`（v1） | **`1390001` 拒绝**：「类型和定义中不匹配。input= radio, expect= radioV2」 |
| 10 | 改用 `user_id`（租户内稳定）而非**别的应用签发的** `open_id` | 发起人解析成同一个人；文案仍空 |
| 11 | 去掉定义侧联动参数后重试（`externalUrl` + `token` 均已配全） | 文案仍空 |

**结论（到这里可以定了）**：飞书对 **API 创建**的实例，会把提交值**原样**存进
`option.key`，且**从不**填 `option.text` / `value`；只有**客户端**提交
（UI 的「再次提交」，见 §12.5 的 `BF7F1A9C`）才会把文案一并写进实例。
**这不是提交格式问题，改代码解决不了。**

顺带两条硬约束（对实现有直接影响，都是本次实测出来的错误码）：

- **`1395006`**：外部选项控件的值**只能是字符串**（数组/对象都被拒），且**报错点名控件 id** ——
  比 `1390001` 更精确，值得进 `fatal_hint`。
- **`1390001`**：`type` 必须与定义**逐字一致**（`radio` ≠ `radioV2`）——印证了
  §6.2「转换器/类型在配置期定死、运行期不推断」这条设计是对的。

#### UI/API 对照实验（已做，结论坐实）

使用方在**当前定义**下用审批页面手工提了一单，与我的 API 提单相隔 3 分钟、同一定义、
同样两个选项 —— **变量控住了**：

```
16:43:13  202609280123  手工在审批页面提的
    办公地点  value='成都'                    option={"key":"fldeysrdna:d22258711825","text":"成都"}
    公司名称  value='成都市易威行科技有限公司'  option={"key":"fldq7lcb6y:dc6b027d9457","text":"成都市易威行科技有限公司"}

16:40:07  202609280121  本系统 API 提单
    办公地点  value=''                        option={"key":"fldeysrdna:d22258711825","text":""}
    公司名称  value=''                        option={"key":"fldq7lcb6y:bc6c077547c5","text":""}
```

**`option.key` 一字不差**，差别只在提交路径。至此三件事同时被排除：**不是提交格式**
（§12.8 的 11 种）、**不是定义侧配置**（同一份定义）、**不是时机**。

**可以下定论了：飞书对 API 创建的实例不解析外部选项文案，只有客户端提交才会写。**
这不是本系统能通过改代码或改请求解决的问题。

#### 因此使用方需要在定义侧做一次取舍

派发链路只回填审批编号、不读回这两个选项，所以**功能不受影响**；受影响的是**审批人在
详情页看到空白**，认不出付款主体。三条处置（按推荐排序，都在审批后台做）：

1. **加一个同名单行文本控件兜底（推荐）**：公司名称/办公地点另配一个**文本**控件，
   本系统按名匹配会照常把文案填进去 —— 审批人一定看得到，且不影响现有的外部选项选择。
2. 改成静态单选（放弃外部数据源）—— 39 个公司名要手工维护，不划算。
3. 接受空显示。

### 12.9 把手工那一单**原样复投**：第三次确认，并顺带挖出「回读 ≠ 可回投」

使用方要求严格按手工提交那单的数据与格式用 API 再提一单。结果分两段。

#### 第一段：`instances get` 的 `form` **不是可回投的表示**（三处不对称）

直接原样回投，连吃两个错：

| 回读形态 | 回投为什么不行 |
|---|---|
| `type:"text"`（说明控件） | API **不支持**该控件 |
| `fieldList` 里的 `attachmentV2` | 回读是**下载 URL**（`https://internal-api-drive-stream…`），而提交要的是 **file code** → `1395006` |
| 单选/多选的 `value` | 回读是**展示文案**（`"个人"`），而 API 要的是**选项值**。原话：<br>`1390001 控件的值不存在单选框的选项中… 值= 个人, 选项= [{"value":"mpuvnw0h-…","text":"个人"}]` |

**第三条是这一节最有价值的东西**：它说明 `form` 里 `value`/`option.text` 是**展示态**，
而**线上值是 `option.key`**。按这个同构关系，**外部选项控件的线上值就是它的 `option.key`
—— 即我们的 `option_id`**。这与 §12.4 的实现独立地吻合，等于给那个实现又加了一条依据。

> 对**将来做审批结果回流**（§2 非目标，但 §5.3 已为它落了 `instance_code`）来说，这三条映射
> 是硬需求：回流拿到的 `form` 不能直接当输入用，必须先做「展示态 → 线上值」的回折，
> 附件还要另走上传换 file code。

#### 第二段：还原成线上值后复投 —— **仍然没有文案**

把上述三处还原（文案 → `option.key`；去掉 `text` 与附件），其余**逐字节照抄**手工那单，
连 `name` / `ext` / **带 `text` 的 `option` 对象**都一起发：

```
发出: {"id":"widget17902293404030001","type":"radioV2",
       "value":"fldeysrdna:d22258711825",
       "option":{"key":"fldeysrdna:d22258711825","text":"成都"}}
结果: 建单成功（B45FBF8C-…），回读 value='' option={"key":"fldeysrdna:d22258711825","text":""}
```

**线上值与手工那单完全一致，连标签都一并喂了，飞书照样不写。** 这是第三次独立确认
（前两次：§12.8 的 11 种形态、§12.8 末的 UI/API 对照），而且这次连 `option.text` 都提供了。

**结论不变且更硬**：`option` 是纯输出字段，API 创建的实例拿不到外部选项文案。
§12.4 的实现在三条独立依据下都是对的，**不需要改**。

### 12.10 机制查实：API 提单**不触发**外部选项取数（日志对齐）

前面只知道「API 路径拿不到文案」。把两边的日志按时间对齐后，机制清楚了。

**实例侧**（按提交时间）与**我们端点侧**（飞书来的取数请求）：

| 北京时间 | 实例 / 事件 | 路径 | 外部选项 |
|---|---|---|---|
| 16:09:46, 16:09:48 | ← 取数 | | |
| **16:10:06** | `202609280102` | **UI（再次提交）** | **有文案** |
| 16:11:30 … 16:40:07 | `…D2`…`…DA`（6 单） | API | 全空白 |
| **16:42:12, 16:42:14** | ← 取数 | | |
| **16:43:13** | `202609280123` | **UI（手工提单）** | **有文案** |
| 16:46:53 | `…DD` | API（原样复投） | 空白 |
| 16:47:09, 16:47:12, 16:49:06 | ← 取数（发生在提交**之后**） | | |

**⚠️ 上面这个「取数时机」的解释已被全量日志推翻，留在这里以见推理是怎么走错的。**

使用方提供了完整日志后可以看到：`16:20:52` / `16:20:55` **带着完整用户上下文**
（`employeeId: eabadgcg`、`tenantKey`、`userId`）取过数，紧接着 `16:20:56` 的 API 提单
（`…D4`）**仍然是空白**。所以「提交前有没有取过数」**既不充分也不必要**，不能当判据。
这一节的表格本身没错，错的是我从它读出的因果。

### 12.11 端点侧「从未失败」——官方 FAQ 的头号成因被排除

官方 FAQ 对「外部关联的单选字段在飞书卡片里没有显示（空白）」给的头号解释是
**外部数据源返回接口报错，导致获取选项失败**，并明确「接口不稳定或不可用造成的问题，
飞书审批不做单据正确性保证」。这条能不能成立，我们的日志可以直接回答 ——
因为端点**自己会记失败**：

- `approval_options.rs:387` `tracing::error!(… "飞书取选项请求失败")`（任何失败信封）
- 同文件 `:392` `tracing::error!("飞书取选项请求超出内部处理预算")`（2.5 秒预算用尽）

查云端 12 小时日志：

| 指标 | 值 |
|---|---|
| 端点被飞书调用 | **38 次** |
| 端点失败 | **0 次** |
| 全库 ERROR / WARN | **0 行** |

**结论：我们的端点从未对飞书失败过。** 它 38 次都返回了 `code:0` + 8 / 39 条带 id 的
options + 带 `@i18n@<id>` → 文案的 `i18nResources`（§12.7 直接打过它，逐字段对过规范）。

所以 FAQ 的头号成因在本例**排除**；剩下的只能是飞书侧对**API 创建**的实例不写外部选项文案。

**至此五条独立证据齐了**：

1. §12.8 —— 11 种提交形态全部无效；
2. §12.8 末 —— UI/API 对照实验（同定义、同选项、相隔 3 分钟）；
3. §12.9 —— 原样复投，连带 `text` 的 `option` 对象都喂了；
4. §12.11 —— 端点 38 次调用零失败，FAQ 头号成因排除；
5. §12.5 的 `BF7F1A9C` —— 唯一带文案的实例来自客户端路径。

**这不是本系统能通过改代码或改请求解决的问题。** 处置建议见 §12.8 末（定义侧加同名
单行文本控件兜底）。

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
