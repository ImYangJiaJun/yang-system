# 审批可选回填扩展、发起人参数化与进度追踪 — 设计

> - 文档性质：**设计规格（spec）**。本文件描述待实现的目标状态，不表示下述能力已经完成。
> - 撰写日期：2026-10-12
> - 需求来源：用户三条需求——(1) 在回填申请编号之外，可选回填申请状态、审批流程、发起时间、完成时间、发起人部门、当前处理人；(2) 发起人由请求参数传入（工作流触发人），表格中的发起人字段改由系统回填；(3) 系统追踪审批进度并更新多维表格。
> - 验证记录：落稿前经 12 个独立代理的两阶段对抗验证（6 项事实查证 → 每项 1 个独立复核者推翻式核验），4 项修正后确认、2 项原样通过，全部结论附真实报文或 `file:line` 证据，溯源见 §4 与 §11。
> - 修订记录：2026-10-12 初稿将进度追踪定为「轮询先行 → 事件订阅」两阶段；**同日用户二次确认修订为直接做事件订阅，不设轮询过渡**（见 §3.2 D4）。
> - 修订记录：2026-10-12 晚，生产服务器（SSH 授权）+ 飞书真机验证 8 项完成，结论已回写 §4/§5/§6/§11/§12；D1–D8 经用户评审全部通过，一期获准开工。
> - 关联文档：`docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`（链路背景、§12 待实测清单）、`docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md`（`requested_by` 的来历）、`docs/contracts/SCHEMA.md`

## 一、目标

在既有「多维表格按钮 → 创建飞书审批实例 → 回填 `serial_number`」链路上增三个能力：

1. **可选回填扩展**：申请状态、审批流程、发起时间、完成时间、发起人部门、当前处理人，六项各自可选，配了列才回填，不配不影响现状。
2. **发起人参数化**：发起人由请求参数传入（工作流触发人），不再从表格列读取；创建成功后由系统把发起人**写回**表格的发起人列。
3. **进度追踪**：**直接以飞书审批事件订阅实现**——接收实例状态变更事件，更新对应多维表格行；终态后停止。不经轮询过渡（用户 2026-10-12 二次确认）。

推翻原设计的一条非目标：`2026-09-28` 设计 §2 明确「不回流审批结果到多维表格」——本设计在需求三范围内**有限恢复**回流（只回填本文列举的六项 + 发起人，不回流审批意见附件等其余内容）。

## 二、非目标

- **不做审批意见 / 评论 / 附件回填**。`operation_records[].comment` 在真实报文里连 START 事件都不带该字段（见 §4.2），PASS/REJECT 是否返回未实测，不做。
- **非目标 2**：审批流程概要只覆盖「已到达节点 + 当前卡点」；补齐后续节点需要审批定义 `node_list`（100 次/分钟，且条件节点无法预判），列为可选增强不进本期（§12 缺口 2）。
- **不做轮询式进度追踪**（用户 2026-10-12 决策：追踪一步到位做事件订阅，轮询不作为追踪机制、也不作为过渡阶段）。
- **不预先引入用户身份授权流程**。若 §11 验证项 5 证实事件**仅** `user_access_token` 可用，回呈用户决策（引入用户授权 vs 放弃事件方案），不擅自实现 OAuth 流程。
- **不做审批定义 / 工作流配置自动化**（沿用原设计）。
- **姓名 → open_id 反查不做默认路径**。触发人通道若实测只能拿到姓名再启用（§11 缺口 1b）。

## 三、决策记录

### 3.1 用户已确认（2026-10-12）

| 问题 | 决策 |
|---|---|
| 发起人来源 | **请求参数传入**（工作流触发人），不读表格列；表格发起人列由系统回填 |
| 六项回填字段 | **全部可选**，配置了目标列才回填 |
| 进度追踪 | 需求成立；机制 = **事件订阅，一来就做**，不经过轮询过渡（用户 2026-10-12 二次确认） |

### 3.2 设计决策（本稿提出，实现前可挑战）

| # | 决策 | 理由 |
|---|---|---|
| D1 | 六项可选回填用 `feishu_approval_config` 上 **6 个新增可空列**，不建 JSON 列 | 与既有 `applicant_field` / `backfill_field` 同构；控制台表单直接渲染；加可空列对 schema 同步安全（本仓库约定只增不删） |
| D2 | 发起人参数**缺省时回退旧行为**（读表格列），传了才走新路径并回写 | 旧工作流模板不破坏；新模板传参即生效；无需迁移存量配置 |
| D3 | 全表异步受理时，发起人写入该配置**当轮 pending 任务行**的新增列 `applicant_open_id`，worker 处理时读取 | 异步轮与受理请求之间只有任务行是持久通道；单条同步路径直接透传不需要落列 |
| D4 | 进度追踪**直接做事件订阅**（HTTP Webhook 回调 + store-then-process），**不设轮询阶段** | 用户二次确认；事件自带 `instance_code` + 新 `status`、同实例有序（§4.6），富化只需每事件 1 次详情 GET |
| D8 | **不设事件丢失的轮询对账** | 用户决策；重推窗口外丢失登记为已知缺口（§12），如后续需要再单独决策 |
| D5 | 当前处理人回填到**人员列**（写 `open_id` 数组），不写姓名 | 写人员列不需要通讯录 scope；姓名需 `contact:user.base:readonly` + 可见范围，留作可选后续 |
| D6 | 完成时间仅在 `end_time != "0"` 时写入；未完成留空 | 真实报文未完成形态是字符串 `"0"`（§4.1），写 0 会在日期列显示 1970 |
| D7 | 状态列优先支持单选与文本两种形态 | 单选写形态有文档/实测冲突（§4.4），探针定论前文本列是零风险退路 |

### 3.3 未决点

全部收敛进 §11 真机验证清单。追踪功能开工前必须先过验证项 5（事件身份与 subscribe API，**阻塞二期**），其余逐项标注是否阻塞一期。

## 四、事实基础（12 代理对抗验证后的确认结论）

> 本章是 §5–§7 全部设计的地基。每条结论的证据形态：**报文** = 本仓库真实抓取的 v4 实例详情（`.inst-raw.json` / `.inst-v1.json`，复核确认 `.test-instance.json` 与前者内容相同、非独立第三份）；**schema** = lark-cli 内嵌官方 OpenAPI 定义；**文档** = open.feishu.cn 实时抓取或本地副本；**代码** = `file:line`。
>
> 仓库既有教训再次应验：官方文档页与真实响应**多处不符**（记忆 `feishu-docs-contradict-live-api`），凡冲突处以真实报文为准，本章逐条标注。

### 4.1 实例详情字段（复核：corrected 后 confirmed）

`GET /open-apis/approval/v4/instances/:instance_id`（本仓库生产路径，`approval.rs:53-58`，tenant token，配额 1000 次/分钟、50 次/秒——文档依据）响应 15 个顶层键（报文 + schema 双证）：

| 用途 | 字段 | 形态（报文实测） |
|---|---|---|
| 发起时间 | `start_time` | **字符串**毫秒时间戳（`"1790750409857"`），无 `created_at` |
| 完成时间 | `end_time` | 未完成 = 字符串 `"0"`（非 null、非缺字段、非数字 0）；完成后应为毫秒串，**未实测**（§11 缺口 4） |
| 申请状态 | `status` | 枚举 5 值：`PENDING` / `APPROVED` / `REJECTED` / `CANCELED` / `DELETED`，**无 `REVERTED`** |
| 退回标记 | `reverted` | 独立布尔（撤回走 `status="CANCELED"`） |
| 发起人 | `user_id` | `ou_` 前缀 open_id（随 `user_id_type` 参数，默认 open_id） |
| 发起人部门 | `department_id` | `od-` 前缀 open_department_id，**是 ID 不是名称** |
| 定义 | `definition_code` / `definition_name` | **真实键是 `definition_code`，不是 `approval_code`** |

**文档-报文冲突的真相（2026-10-12 真机定论，推翻本节初稿「文档滞后」的解释）**：**bot（tenant token）与 user 两种身份走同一 `GET /instances/:instance_id` 返回两种并存的字段形态**——

- **bot 路径（生产实际在用）= 文档 get.md 形态**：`approval_code` / `approval_name` / `task_list[]` / `timeline[]` / `uuid` / `user_id`（工号如 `eabadgcg`）+ 独立 `open_id` / `department_id`（**非 od- 前缀**，实测 `"db59a45c851464gf"`），**无** `current_nodes` / `tasks` / `comments` / `operation_records` / `definition_code`。真实报文（生产 tenant token curl，实例 9B0F4A3E）已核。
- **user 路径（lark-cli）= 新形态**：`definition_code` / `definition_name` / `tasks[]` / `current_nodes[]` / `operation_records[]` / `comments[]` / `department_id`（od- 前缀）。本地 `.inst-raw.json` 等报文均属此形态。
- `GET /instances/detail?instance_code=` 别名路径**拒绝 tenant token**（99991663 实测），仅 user 身份可用——生产保持 `/instances/:id` 不变。
- **对实现的直接后果**：`InstanceDetail` 扩展解析要以 **bot 形态**为准（生产就是 bot），取当前节点/处理人用 `task_list[]`（status=PENDING 即当前），流程概要用 `task_list`+`timeline`；lark-cli user 形态仅作对照，不进生产代码。初稿依据 user 形态写的「`approval_code` 恒为 None」**对生产不成立**——bot 形态里它存在且有值。

枚举注意：实例**列表查询**的 `instance_status` 参数拼写不同（`PENDING/RECALL/REJECT/DELETED/APPROVED/ALL`）；任务级 `tasks[].status` 是另一组（`PENDING/APPROVED/REJECTED/TRANSFERRED/DONE`）。三处勿混。

### 4.2 节点与流程（复核：corrected → partial）

- **没有独立的 `/nodes` 或 `/timeline` 接口**——流程节点、任务、动态全部**内联在实例详情响应**里（schema + 报文双证）。
- `current_nodes[]`：`{node_id, node_name, type(AND/OR/SEQUENTIAL), approvers:[{task_id, user_id}]}`——当前卡点 ✓，当前处理人只有 `user_id` 无姓名 ✓。
- `tasks[]`：`{id, node_id, node_name, user_id, status, type, start_time, end_time}`——已到达节点的名称、状态、处理人、时间全齐 ✓。**bot 形态对应 `task_list[]`**（字段同构，多 `open_id`、`user_id` 为工号，实测 task status=`DONE` 亦在枚举内）。
- `operation_records[]`：schema 定义含 `comment`（Reason），**真实 START 事件不含该字段**；PASS/REJECT 是否返回未实测（§11 验证项 7）。
- 路径分歧（复核澄清）：生产代码走 `GET /instances/:instance_id` + tenant token（在用、可工作）；lark-cli / 当前文档路径是 `GET /instances/detail?instance_code=...` + user token。两条路径响应同构；别名关系未确认（§11 验证项 8），**本期不改生产路径**。
- 配额：实例详情 1000 次/分钟 50 次/秒（文档）；审批定义 `GET /approvals/:approval_code` 100 次/分钟（文档）。

### 4.3 部门（复核：upheld，原样通过）

**最优链路已确认——审批实例详情自带 `department_id`，无需调通讯录 user 接口**（报文 `od-e2ebe89e...` 实证；文档同页标注「发起审批用户所在部门的 ID」）。

**⚠️ 2026-10-12 bot 路径修正 / ✅ 2026-10-15 形态闭环**：生产（bot）形态的 `department_id` 是**另一种格式**（实测 `"db59a45c851464gf"`，非 od- 前缀）。2026-10-15 真机（lark-cli，`department_id_type=department_id` + 该 ID）验证**格式被接口接受**——错误停在 40004「no dept authority」（可见范围），而非 99992357「不是有效的 department_id」。结论：按前缀分流 `department_id_type`（`od-` → `open_department_id`，其余 → `department_id`）即可直喂，**无需再试调**；实现见 `approval.rs::resolve_department_name`，解析失败退化为写原始 ID。

**scope 已开通**（2026-10-15 应用管理员确认 `contact:department.base:readonly` 已申请，§11 验证项 6 的 99991672 不再适用）。剩余风险只有**部门可见范围**：目标部门须在应用的通讯录权限范围内，否则 40004——解析失败退化为写原始 ID（有 warn 日志可查，不阻塞派发）。

- 取部门名：`GET /open-apis/contact/v3/departments/:department_id` → `data.department.name`；scope `contact:department.base:readonly`；部门须在应用通讯录权限范围内，否则 **40004**（真实复现）。
- 一步到位的 `department_path` 仅 `user_access_token` 返回，bot 身份不返回——本项目 bot 流程不可用，弃。
- 实例**列表查询**接口不带 `department_id`——追踪回填必须走 get/detail（与 §7 设计一致）。
- 通讯录 user 接口仅在「从 open_id 独立反查部门」时才需要——本设计不需要。

### 4.4 多维表格写入形态（复核：upheld）

| 列类型 | field_type | 写入形态 | 证据级别 |
|---|---|---|---|
| 人员 | **11** | `[{"id":"ou_xxx"}]`（键必须 `id`，数组必填） | 文档（官方「记录写接口仅支持传入 id」+ 错误码 1254066 佐证） |
| 日期 | **5** | 毫秒时间戳 **number**（`1674206443000`）；空值 `null` | 文档示例 + lark-base 实测另收 ISO 串 |
| 单选 | **3** | **已定论（2026-10-12 真机探针）**：写**裸字符串**，数组形态被 `1254062 SingleSelectFieldConvFail` 拒绝（响应明文「the value of 'Single Option' must be a string」）；**选项不存在时自动创建**（探针写入后字段选项列表新增该值，与官方文档一致）。本仓库 `pull_probe.rs:24-26` 的「数组形态」是**读侧**形态被误当写侧——写侧以本结论为准 | 探针实证（生产 tenant token，探针记录已删） |
| 文本 | **1** | 字符串；**现有代码已放行** | 代码 `approval_dispatch.rs:182` |

- **扩展点**：`approval_dispatch.rs:178` 的 `match field.field_type` 现只放行 `Some(15)`（链接）与 `Some(1)|None`（文本），`:186` 拦截其余——需增 `Some(3)` / `Some(5)` / `Some(11)` 三臂。
- 类型编号与 `bitable.rs:298-300` 注释、`FieldItem.field_type` 的 `serde(rename="type")`（`bitable.rs:304`）一致。
- 本仓库 `batch_update_records` 不传 `user_id_type`（`bitable.rs:1266-1276` query 传空）→ 默认 open_id，与审批回执 `ou_` 前缀**天然匹配**（报文实证）。
- **只读列不可回填**：公式(20)/查找引用(19)/自动编号(1005)/创建修改时间(1001/1002)/创建修改人(1003/1004)/流程(24)/按钮(3001)——写入被静默丢进 `ignored_fields`。配置校验时应拒绝这些类型。
- 相关错误码：1254062（单选）/1254064（日期）/1254066（人员）——须进错误分类表（§5.3）。

### 4.5 发起人参数通道（复核：upheld）

- 既有 `requested_by` **仅记录**：`dispatch.rs:468` → `request_log_writer.rs:39-45` → 落 `feishu_approval_request_log.requested_by`（`request_log_table.rs:30`），不进派发。发起人必须是**新字段**，不能复用它。
- 工作流侧触发人变量：按钮触发器（`buttonField` / `buttonElement` 均有）输出 `$.{stepId}.user`，值为 `{id, name}` 对象，**id 统一 OpenID `ou_` 前缀**（lark-cli 内嵌 `lark-base-workflow-schema.md:664,735,743`）。
- **M11 已定论（2026-10-12 真机）**：生产 `request_log.requested_by` 实值为**姓名**（杨佳俊 / 王谨，utf8mb4 核实），即 `$.step_btn.user` 插进 `raw_body` 文本槽渲染成 **name 而非 open_id**。**请求侧拿不到触发人 open_id**——一期发起人传参必须启用「姓名 → open_id 解析」（缺口 1b）；且经本应用 contact API 解析出的 open_id 属本应用命名空间，顺带消解 §4.5 的跨应用 1254066 风险。前置：应用需开通 contact scope（§11 验证项 6，当前为 0）。
- **跨应用 open_id 风险**（原始路径若拿 id 才相关）：本仓库实测记录 open_id 跨应用交叉使用报 1254066（`design.md:812-813,983`）。M11 定论为姓名后，此风险被「本应用自解析」路径绕开。
- 姓名 → open_id 解析（`contact/v3/users/search` / `batch_get_id`）：重名需二次消歧；**M11 定论后已启用为一期主路径**（§6.1，前置 contact scope）。
- 改动面：`applicant_field` 定义于 `table.rs:66-69`；读取点 grep 约 15 处，跨 8 个后端文件（`approval_provision` / `dispatch` / `approval_dispatch` / worker / `create_config` / `update_config` / `list_configs`）+ 3 个前端文件（`api.ts` / `api-types.ts` / `ApprovalWorkflowGuide.tsx`）。**列本身无 DDL 变更**（存 field_id 不变），变的是语义（读源 → 写目标）。

### 4.6 追踪机制（复核：corrected 后 confirmed）

**事件订阅（二期方案，用户确认直做）**——运行时事实修正了文档叙述：

- 事件键：`approval.instance.status_changed_v4` + `approval.task.status_changed_v4`（lark-cli 运行时目录；文档页旧名 `approval_instance`）。
- **身份障碍**：运行时元数据 `auth_types: ["user"]`——事件仅支持 `user_access_token`；bot 身份消费直接报错（lark-cli 实测拒绝文案）。本项目全部出站是 tenant token。
- 所需 scope（运行时）：`approval:instance:read` / `approval:task:read`；文档另写 `approval:approval:readonly`——两说并存。
- **subscribe API 已实证可用（2026-10-12 二期门槛通过）**：生产 tenant token 真机 `POST /open-apis/approval/v4/approvals/D0557DA6.../subscribe` 返回 **HTTP 200 `{"code":0,"data":{},"msg":"success"}`**——bot 身份可订阅，lark-cli 此前 404 属 CLI 侧问题。事件推送链路的其余部分（后台事件订阅配置、回调端点、验签）仍是二期开发项。
- payload（schema 确认）：`instance_code` / `status` / `approval_code` / `operate_time` / `event_id` / `start_user`；status **8 值**（详情接口 5 值 + `REVERTED` / `OVERTIME_CLOSE` / `OVERTIME_RECOVER`——事件与详情枚举不一致，实现各自为准）。
- 投递语义（文档）：至少一次（须 `event_id` 幂等）、3 秒内回 200、重推 15s→5min→1h→6h 共 4 次（≈7.1 小时窗口）、**同实例有序**。

**既有轮询基建（复核确认，仅作事实记录）**——

- 详情接口 1000 次/分钟、50 次/秒（文档逐字；本仓库 `outbound.rs` 已消费 `x-ogw-ratelimit-reset` 与 99991400 退避）。
- 本项目现状（全部复核确认）：**有入站 axum 服务器**（`bootstrap.rs:332`，路由含 `dispatch.rs:215`）但**无任何事件回调/WebSocket/验签路由**；worker 轮询默认 30s（`config/mod.rs:699-700`）；`feishu_approval_task` 已有 `instance_code` + `record_id` + `config_id`——事件或轮询拿到 instance_code 均可反查回填行；task 表的 `state` 是**派发生命周期**（`task_table.rs:65-77`），无实例状态列。

## 五、需求一：可选回填扩展

### 5.1 配置模型（D1）

`feishu_approval_config` 新增 6 个可空 `Str(128)` 列，存目标列 `field_id`（与 `backfill_field` 同轴：存 id 不存列名，改列名不失效；写时按 §4.4 现场解析列名）：

| 列 | 回填内容 | 目标列类型约束 |
|---|---|---|
| `status_field` | 申请状态 | 文本(1) 或 单选(3) |
| `flow_field` | 审批流程概要 | 文本(1) 或 单选(3) |
| `start_time_field` | 发起时间 | 日期(5) |
| `end_time_field` | 完成时间 | 日期(5) |
| `department_field` | 发起人部门名称 | 文本(1) |
| `handler_field` | 当前处理人 | 人员(11) |

规则：

- 列为 `NULL` / 空串 = 不回填，现状完全不变（D1 的可选语义）。
- `flow_field` 收单选（2026-10-13 放宽）：真机目标表把「审批流程」建成了单选（§11 快照），单选写形态已定论（§4.4），与 `status_field` 同路。**代价**：流程概要的每个不同取值都会自动新建一个选项，选项列表随流程推进增长（选项数上限 20000/字段，实践中够用）。
- 建配置（provision）与控制台保存时校验目标列存在且类型匹配——**只读类型直接拒绝**（§4.4），错误进「配置期报错」既有通道（`[配置]` 前缀写回 `backfill_field`，沿用 `write_config_failure` 的文案约定）。
- 控制台建配置向导（Task 13 产物）表单加 6 个可选输入，空 = 不启用。

### 5.2 取值与写入

全部取自**一次** `get_instance`（详情已在 `finish_backfill` 前拿到，`InstanceDetail` 扩展解析即可，零额外 API 调用）：

| 字段 | 取值逻辑 |
|---|---|
| 申请状态 | `status` 原值（可选加 `reverted` → 「已退回」后缀）；写文本列转中文映射（`PENDING`→审批中 等 5 条），写单选列写选项名（写形态已定论为裸字符串、选项可自动创建，§4.4） |
| 审批流程 | `tasks[]` 按时间序拼 `node_name{✓/✗}` + `current_nodes[]` 拼 `node_name{⏳}`，如 `部门审批✓ → 财务审批⏳`；只覆盖已到达节点（§二非目标 2） |
| 发起时间 | `start_time` 字符串 → `i64` 毫秒 number 写日期列 |
| 完成时间 | 同上，但 `end_time == "0"` 时**不写**（D6） |
| 发起人部门 | `department_id` → 部门接口取 `name`（§4.3，1 次 API + 新 scope）→ 写文本；部门接口失败**不阻断编号回填**，降级为跳过该项 |
| 当前处理人 | `current_nodes[].approvers[].user_id` 去重 → 人员列 `[{"id":...}]`（D5）；无当前节点（已终态）时写空数组清列 |

### 5.3 `BitableBackfill` 扩展

`approval_dispatch.rs:178` 的 `match field.field_type` 增三臂：

```rust
Some(3) => ...   // 单选：形态待探针（§11 缺口 3）；探针前先走文本退路（D7）
Some(5) => ...   // 日期：value 须为 number 毫秒；字符串先转换
Some(11) => ...  // 人员：value 须为 [{"id":"ou_.."}] 数组
```

- 错误码归类进 `outbound` 失败分类：1254062/1254064/1254066 均为**数据/配置问题 → 终态**（写 `backfill_field` 原因），不进可重试桶。
- 六项写入失败**均不改变编号回填的结论**：`finish_backfill` 里编号写成功即 `Backfilled`，扩展字段写失败只 `tracing::warn`——编号是主产物，扩展字段是附带（与既有「回填失败不吞事实」原则一致）。
- 批量场景（worker）：扩展字段随 `dispatch_one` 单条写（每条已有一次 `batch_update` 基础上最多再合并一次），不进 `backfill_in_chunks` 的分批通道——分批只服务编号的全有全无语义。

### 5.4 触发时机

| 字段 | 创建后立即回填 | 需要后续刷新 |
|---|---|---|
| 发起时间 / 发起人部门 / 创建时点的流程与状态 | ✓ | — |
| 申请状态 / 当前处理人 / 完成时间 | ✓（初始值） | **✓ 由需求三事件订阅刷新**（§7.4） |

即：一期写初始值，二期事件到达时刷新——两期共用同一套写入函数。

## 六、需求二：发起人参数化

### 6.1 请求契约

`DispatchInput`（`dispatch.rs:60`）新增可选字段：

```rust
/// 发起人 open_id（工作流触发人 `$.step_btn.user`）。提供则替代表格列读取，
/// 创建成功后由系统回填到 applicant_field 列。
#[serde(default)]
applicant: Option<String>,
```

- 鉴权口径不变：白名单校验 + 管理 Token（`dispatch.rs:347` 起的 `config` 查询）已约束可操作面，新字段不扩权。
- 校验：trim 后非空才生效；**值形态为姓名**（M11 定论：`$.step_btn.user` 渲染成 name，§4.5），系统经 contact API 解析成 open_id 后使用；解析失败（未命中/重名多命中）→ `Waiting` 并写明原因，等人工修工作流变量。
- 响应体不加字段（工作流 `response_value` 契约不动）。

### 6.2 语义翻转与兼容（D2）

`dispatch_one` 的申请人解析（`approval_dispatch.rs:266-282`）改为：

```
输入有 applicant → 用它（跳过 applicant_open_id(cells) 读取）
输入无 applicant → 回退读表格列（现状原样）
```

- `applicant_field` 配置的语义在**传参路径**下翻转为写目标；未传参的存量工作流行为零变化。
- 传参路径下，`Waiting` 判据从「表格缺申请人」变为「请求缺 applicant 且表格也缺」。
- 提示性变更：前端向导（`ApprovalWorkflowGuide.tsx:157-159`）补一段——新模板应传 `applicant`，`requested_by` 仅作记录。

### 6.3 异步路径持久化（D3）

全表受理（无 `record_id`）是异步的：受理请求与 worker 处理之间隔一个派发间隔周期（默认 30s，`approval_scan_interval_seconds`），applicant 必须落库：

- `feishu_approval_task` 新增可空列 `applicant_open_id`。**存储已解析的 open_id**（M11 定论：请求参数是姓名，受理时解析一次落库，worker 不重复解析）。
- 受理请求带 applicant 时，**由 worker 在轮首按 config 把该参数写入仍为 `pending` 的任务行**（Action 侧只把 applicant 随 `request_dispatch()` 信号传给 worker 队列，避免 Action 与 worker 之间的时序竞态——Action 自己 UPDATE pending 行会与 worker 的 seed/claim 交错）。
- worker `process_one` 取发起人优先级：`task.applicant_open_id` → 表格列（D2 回退）→ `Waiting`。
- 单条同步路径不经过任务行，`DispatchInput.applicant` 直接进 `OrchestrationInput`（`approval_dispatch.rs:87` 加字段）。

### 6.4 写回发起人列

传参路径创建成功（`finish_backfill` 的 `Backfilled` 分支）后，把 `[{"id": applicant}]` 写入 `applicant_field` 目标列：

- 目标列类型须为人员(11)，配置校验同 §5.1；写入走 §5.3 的新臂。
- 写失败同样只 `warn`，不影响 `Backfilled` 结论。
- 回退路径（无参）**不写回**——行为与现状完全一致。
- 跨应用 open_id 若触发 1254066：终态错误进 `backfill_field`（§5.3 分类），真机验证项 2 通过后才放开该路径（§11）。

## 七、需求三：进度追踪（事件订阅，直做不过渡）

### 7.1 机制决策（D4 / D8）

**直接做事件订阅，不设轮询阶段**（用户 2026-10-12 二次确认，推翻初稿「轮询先行」）。接收 `approval.instance.status_changed_v4` 事件 → 富化 → 更新多维表格。不设事件丢失的轮询对账（D8），重推窗口外丢失登记为已知缺口（§12）。

**两大前置障碍在 §11 验证项 5，开工前必须真机定论**：

1. **订阅身份**：lark-cli 运行时元数据显示事件 `auth_types: ["user"]`（仅 `user_access_token`），bot 身份消费被拒（§4.6）。本项目全部出站是 tenant token。若真机证实 bot 不可用 → **回呈用户决策**（引入用户授权流程 vs 放弃事件方案），本设计不擅自实现 OAuth（§二）。
2. **subscribe API**：文档声称 `POST /approvals/:approval_code/subscribe`（tenant token），lark-cli 实测 404——须用真实审批定义端到端验证。

**选型备注（对比信息，非轮询路线）**：飞书另有长连接 WebSocket 订阅方式，但官方 SDK 仅 Go/Python/Java/Node.js，Rust 无官方客户端，自研成本高——**本设计选 HTTP Webhook**：入站 axum 服务器现成（§4.6），新增一条路由即可。

### 7.2 订阅链路

1. **后台配置**（人工一次性）：开发者后台 → 事件订阅 → 配 Encrypt Key + Verification Token、选 **HTTP Webhook** 指向本服务（生产边缘 18654）、添加 `approval.instance.status_changed_v4` 事件并申请 scope（运行时 `approval:instance:read`，文档另写 `approval:approval:readonly`——两说并存，验证项 5 定论）。
2. **按定义订阅**：文档声称建配置成功后调 `POST /open-apis/approval/v4/approvals/:approval_code/subscribe`（tenant token，100 次/分钟）；接进 provision 流程（每配置一次）——**以验证项 5 的真机结论为准**。
3. **回调路由**（新增 Action 路径，如 `POST /api/v1/feishu/approval/event`）：
   - `url_verification` challenge 应答 → 验签（Verification Token / Encrypt Key）→ AES 解密（若开启加密）→ **3 秒内回 200**（官方时限）；
   - **store-then-process**：先验签解密落事件表，再异步处理——绝不在 3 秒窗口里做出站富化调用；
   - **幂等**：按 `event_id` 去重（至少一次投递，可能重复）；**同实例有序**（官方语义），同实例事件按接收序处理即可。

### 7.3 事件处理

- **映射回填行**：payload 带 `instance_code` → `feishu_approval_task.instance_code` 反查 → `record_id` + `config_id`（该列已存在，§4.6）。
- **富化**：事件只带 `status` / `operate_time` 等少量字段，六项富字段仍需一次 `get_instance`（每事件 1 次，配额见 §7.5）。
- **写表格**：状态 / 流程 / 当前处理人 / 完成时间 → 复用 §5.2 写入函数；同时更新 task 行 `instance_status` / `last_synced_at`。
- **终态收敛**：写入 `APPROVED/REJECTED/CANCELED/DELETED` 后置终态列，该行退出后续处理；事件仍会到达（如有）则按幂等跳过。
- **`REVERTED` / `OVERTIME_CLOSE` / `OVERTIME_RECOVER`**：事件枚举 8 值、详情 5 值（§4.6）——收到未知值时先 `get_instance` 取详情 5 值口径写表，事件专有值只记 `last_error` 不写状态列（映射表见 §12 缺口 4）。

### 7.4 新增表：事件落地表 `feishu_approval_event`

store-then-process 的落地缓冲（结构照 `feishu_approval_request_log` 的先例）：

| 列 | 说明 |
|---|---|
| `event_id`（唯一索引） | 幂等键（重复投递在唯一键处天然去重） |
| `event_type` / `instance_code` / `status` / `operate_time` | payload 关键字段 |
| `received_at` / `processed_at`（可空） | 未处理行 = 待富化队列；`processed_at IS NULL` 即工作集 |
| `raw_payload` | 原文留痕，排查用 |

处理失败（富化 GET 失败、写表失败）→ `processed_at` 保持 NULL + 重试计数，连续失败超限置错误并停止（防毒事件死循环）。

### 7.5 配额预算（每事件 1 次详情 GET，1000 次/分钟）

| 事件量级 | 消耗/分钟 | 占比 |
|---|---|---|
| 10 事件/min | 10 | 1% |
| 50 事件/min | 50 | 5% |
| 100 事件/min | 100 | 10% |
| 1000 事件/min | 1000 | 100% **上限**（1000 事件/分 ≈ 每分钟千单变更，现实不可达；`PULL_RETRY` + `x-ogw-ratelimit-reset` 退避兜底） |

与创建接口（100 次/分钟、独立配额）互不占用。富化失败不阻塞 3 秒应答（store-then-process 隔离）。

## 八、数据模型变更

| 表 | 变更 | 类型 | 迁移风险 |
|---|---|---|---|
| `feishu_approval_config` | +`status_field` / `flow_field` / `start_time_field` / `end_time_field` / `department_field` / `handler_field` | 6 × 可空 `Str(128)` | 仅新增可空列，schema 同步只增不删，安全 |
| `feishu_approval_task` | +`applicant_open_id` | 可空 `Str(64)` | 同上 |
| `feishu_approval_task`（二期） | +`instance_status` / `last_synced_at` | 可空 `Str(32)` / 可空时间戳 | 同上 |
| `feishu_approval_event`（二期，**新表**） | 事件落地缓冲：`event_id` 唯一、`event_type` / `instance_code` / `status` / `operate_time` / `received_at` / `processed_at` / `raw_payload` / 重试计数 | 见 §7.4 | 建表 + 唯一索引，schema 同步新增表安全 |

- `applicant_field` 列**不变**（既有列零 DDL），仅语义在传参路径下翻转（§6.2）。
- 同步后须回写 `docs/contracts/SCHEMA.md` 与 schema_anchor（既有约定）。

## 九、代码改动面（按文件）

| 文件 | 改动 |
|---|---|
| `src/addon/feishu/domain/approval.rs` | `InstanceDetail` / `InstanceDetailData` 按 **bot 形态**扩展解析（`task_list` / `timeline` / `department_id` / `user_id`+`open_id` / `uuid`，§4.1 双形态说明）；新增部门名查询函数（`contact/v3/departments/:id`，先试调 bot 形态 department_id）；新增姓名→open_id 解析（`contact/v3/users/search`，§6.1） |
| `src/addon/feishu/domain/approval_dispatch.rs` | `DispatchInput` 加 `applicant` + 6 个回填目标；`finish_backfill` 后接扩展写入（§5.2）；`applicant_open_id` 解析分支（§6.2）；`write_many` 三类型臂 + 错误码分类（§5.3）；**一期顺手**：`DispatchResult::Backfilled` 携带 `instance_code`（现在只有 serial_number，task 表回写需要它） |
| `src/addon/feishu/approval/actions/dispatch.rs` | `DispatchInput` 加 `applicant` 字段与校验（§6.1）；全表受理把 applicant 交给 worker（§6.3） |
| `src/addon/feishu/approval/table.rs` | config 加 6 列、task 加 2+2 列（§八） |
| `src/addon/feishu/domain/approval_provision.rs` | 建配置时校验 6 个可选目标列的类型（§5.1） |
| `src/infrastructure/feishu_approval_worker.rs` | `load_task`/`process_one` 读 `applicant_open_id`；派发成功落 `instance_status`；**一期顺手**：`record_outcome` 回写 task 行的 `instance_code`/`serial_number`（两列已存在、现全 NULL，§12 缺口 10） |
| 事件回调路由 + 处理器（二期，新增；建议 `src/addon/feishu/approval/actions/event.rs`） | challenge 应答、验签解密、3s 内 200、`event_id` 幂等落表、store-then-process 富化与写表（§7.2–§7.4） |
| `src/addon/feishu/approval/table.rs`（二期） | 事件表 `feishu_approval_event` 表定义 |
| `src/addon/feishu/domain/approval_provision.rs`（二期） | 建配置成功后调 subscribe API（§7.2 第 2 步，以验证项 5 结论为准） |
| 控制台 `create_config`/`update_config`/`list_configs` + 前端 | 6 个可选字段的读写与表单（§5.1） |

零改动：`request_log` 链路、鉴权中间件、白名单校验、`backfill_in_chunks` 分批通道、uuid 派生与 60012 回捞。

## 十、分期与验收

### 一期：可选回填 + 发起人参数化（纯出站，无新基建）

（勾选进度以 git log 为准，Wave2a 完成后再勾）

1. [x] `InstanceDetail` 按 bot 形态扩展解析 → 验收：单测覆盖 `"0"` 形态、5 值枚举、`approval_code`/`task_list`/`timeline` 键名（bot 真实形态）。 (f2fa474/8d34f1d)
2. [x] 配置 6 列 + 类型校验 + 控制台表单 → 验收：只读类型列配置被拒且 `[配置]` 文案回填。**生产六列已建好（§11），零 DDL 与人工准备**。
3. [x] `write_many` 三类型臂 + 错误码分类 → 验收：端到端 mock 用例逐字段断言发给 `batch_update` 的请求体；单选臂写**裸字符串**（§4.4 定论）。 (f2fa474/8d34f1d)
4. [x] `applicant` 参数（姓名）+ contact 解析 + 回退 + 写回发起人列 → 验收：传参路径断言 `create` 请求体 `open_id` 为解析结果、且申请人列被写；不传参路径回归（既有测试全绿）。**依赖 contact scope 开通（§11 验证项 6）**。 **代码已备，真机等 scope 审核。**
5. [x] 全表路径 `applicant_open_id`（解析后）落任务行 → 验收：受理后 worker 消费到参数。 (f2fa474/8d34f1d)
6. [x] 验证项 1a 已定论（M11=姓名）；验证项 2 改为「姓名解析后 open_id 提交无 1254066」，随 4 一并真机。 **代码已备，真机等 scope 审核。**
7. [x] task 表 `instance_code`/`serial_number` 回写（§12 缺口 10）→ 验收：backfilled 行两列非 NULL。 (f2fa474/8d34f1d)

### 二期：事件订阅追踪（直做；开工门槛 = §11 验证项 5 通过）

1. [ ] **验证项 5 已通过**（2026-10-12 真机 subscribe 200，§4.6）——但回调端点仍需后台配事件订阅（Encrypt Key / Verification Token / Webhook 地址），人工前置。
2. [ ] 事件落地表 `feishu_approval_event` + 回调路由（challenge / 验签 / 解密 / 3s 200 / 幂等）→ 验收：真机事件落一行、重复投递被唯一键去重、验签失败被拒。
3. [ ] store-then-process 处理器：`instance_code` 反查 → 富化 GET → 复用 §5.2 写表 → 终态收敛 → 未知事件枚举按 §7.3 处置 → 验收：模拟状态迁移用例（PENDING→APPROVED 触发四字段更新、终态行重复事件幂等跳过、富化失败留待重试且不超限）。
4. [ ] provision 接 subscribe → 验收：新建配置自动订阅，真机建单收到事件。
5. [ ] 配额与 3 秒窗口测试：富化慢不阻塞应答；事件风暴下 GET 退避生效（照 `PULL_RETRY` 既有用例风格）。

## 十一、真机验证清单

> **2026-10-12 执行记录**：用户授权 SSH 生产服务器后，8 项验证以只读查询/日志 + 真机 GET + 单选写探针（探针记录已删除、孤儿任务行已清理）完成。下表更新各项结论。

| # | 验证项 | 阻塞 | 结论（2026-10-12） |
|---|---|---|---|
| 1a | **M11**：`$.step_btn.user` 渲染形态 | ✅ **已定论** | **姓名**（requested_by 落库实值：杨佳俊/王谨）。请求侧拿不到 open_id；一期启用姓名→open_id 解析 |
| 1b | 姓名 → open_id 解析 | **启用**（随一期 4） | 接口 `contact/v3/users/search` 存在；重名需消歧；**依赖 contact scope**（见 6） |
| 2 | 跨应用 open_id 1254066 | **绕开** | M11=姓名后走本应用自解析，风险消解；解析结果提交真机验证并入一期 4 |
| 3 | **单选写探针** | ✅ **已定论** | **裸字符串成功 + 选项自动创建**（写入「探钢单选B」后选项列表新增）；数组形态 1254062 拒绝（「must be a string」）。D7 状态列可直接用单选 |
| 4 | APPROVED/REJECTED `end_time` | ✅ **已定论** | CANCELED 实例（9B0F4A3E）实测 `end_time:"1790751033954"` 毫秒串 → 完成态形态确认；D6 成立。APPROVED 未单独取到（instances/query 缺 `approval:approval.list:readonly` scope，不阻塞——枚举同属完成态） |
| 5 | **事件身份与 subscribe API** | ✅ **门槛通过** | 生产 tenant token `POST .../subscribe` → **200 `{"code":0}`**，bot 可订阅。后台事件订阅配置（Encrypt Key/Webhook）仍为二期人工前置 |
| 6 | 通讯录部门/用户 scope | ⚠️ **缺，需后台开通** | 部门 GET、用户 GET 均 99991672：需开通 `contact:contact.base:readonly`（任一 contact 接口级）+ 字段级 `contact:department.base:readonly` / `contact:user.department:readonly`，并把发起人部门纳入通讯录权限范围。阻塞一期「发起人部门」与「姓名解析」两项 |
| 7 | `operation_records[].comment` | 不阻塞（非目标） | 未取到 PASS/REJECT 实例（4 缺 list scope）；bot 形态对应 `timeline[]`（实测 START/CANCEL 无 comment），实现时按 bot 形态 |
| 8 | 实例详情双路径 | ✅ **已定论** | `/instances/:id` + tenant token 可用；`/instances/detail` 拒 tenant token（99991663）。**更正**：bot 与 user 身份同路径返回**两种并存字段形态**（§4.1），生产以 bot 形态为准 |
| 9 | 详情接口配额文档值 | 不阻塞（退避兜底） | 未触发限流；`PULL_RETRY`+`x-ogw-ratelimit-reset` 已在位 |
| 10 | Webhook 投递行为 | 不阻塞开发 | 未验证（需后台配事件订阅后才能收真机事件），并入二期端到端 |

**生产现状快照（同日核实）**：目标表六列已全部建好（申请状态单选 8 选项 / 发起时间·完成时间日期 / 发起人·当前处理人人员 / 发起人部门文本 / 审批流程单选）；`feishu_approval_task` 已有 `instance_code`/`serial_number` 列但 20 行全 NULL（未回写——二期追踪需补）；后端日志 7 天见 9 个 instance_code、serial 202610120103–126、332 次 1390003、1 次 60012；9 条 pending 行对应记录已在表中不存在（用户删行遗留，worker 每轮空转报「记录不存在」，与本设计无关的既有清理项）。

## 十二、已知缺口（如实登记）

1. 部门名称与姓名解析依赖新 scope——验证项 6 已核实**确实缺 scope**（99991672），需开发者后台开通 `contact:contact.base:readonly` + 字段级 `contact:department.base:readonly` / `contact:user.department:readonly`，并把发起人纳入通讯录数据权限；阻塞一期「发起人部门」与「姓名解析」两项。
2. 流程概要不含未到达节点；条件节点（表单值驱动）永远无法静态预判——近似值语义已在 §二写明。
3. 当前处理人姓名回填（D5 的可选项）需要 `contact:user.base:readonly` + 可见范围，未排期。
4. 事件与详情的 status 枚举不一致（8 vs 5）——二期追踪按详情 5 值写表，事件专有值按 §7.3 处置。注意：详情 task status 实测还见 `DONE`（bot 形态 task_list），节点级枚举比实例级更宽。
5. `end_time` 完成态已定论（CANCELED 实例毫秒串，验证项 4）；APPROVED 实例仍缺实测（需 `approval:approval.list:readonly` scope，不阻塞）；`reverted=true` 时 status 取值仍无报文。
6. 「文档与响应冲突」的真相 = bot/user 两种字段形态并存（§4.1 定论）——实现一律以 **bot 形态**（生产 tenant token）为准，遇新差异追加进 `docs/reference/feishu/` 记录。
7. **事件丢失无兜底**（D8）：不设轮询对账，事件重推窗口约 7.1 小时（15s→5min→1h→6h 共 4 次），超窗丢失该次状态变更，多维表格与实际状态出现偏差，需人工或后续事件覆盖；如不可接受需单独决策对账机制。
8. **bot 形态 `department_id` 格式异常**（实测 `"db59a45c851464gf"`，非 od- 前缀）——能否直接喂 contact 部门接口未验证，一期实现「发起人部门」时先试调；若不通需找 bot 形态下取部门 od- id 的路径。
9. 生产 9 条 pending 任务行对应记录已在表中不存在（用户删行遗留，worker 每轮空转「记录不存在」）——与本设计无关的既有数据清理项，建议运维择机清理（需另行授权）。
10. 生产 `feishu_approval_task` 的 `instance_code` / `serial_number` 列存在但 20 行全 NULL 未回写——二期追踪的映射依赖它们，一期派发路径可顺手补写（零成本高收益，已并入一期改动面）。