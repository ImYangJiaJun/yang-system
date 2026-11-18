# 审批退回重提后多维表格台账跟随审批表单 — 设计（收窄版）

- 文档性质：设计（决策记录、边界、验证清单）
- 撰写日期：2026-11-13
- 修订：2026-11-16 按需求澄清收窄范围
- 状态：待 P0 真机验证
- 关联文档：
  - `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`
  - `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md`
  - `docs/architecture/2026-10-25-approval-durable-execution.md`
  - `docs/contracts/SCHEMA.md`

## 一、需求与范围

### 1.1 目标

审批派发后，如果审批被**退回**，发起人在飞书工作台**同一审批实例内**修改表单并重新提交（`instance_code` 不变），系统要把用户修改后的表单数据同步回**最初派发的那条（或那组）多维表记录**，保证台账数据始终是审批事实的投影。

### 1.2 已确认的业务事实

| 事实 | 结论 |
|---|---|
| 台账状态 | 多维表台账在派发后被**锁定**，不允许人工修改；系统是唯一可写方。 |
| 重提路径 | 用户走的是「**提交**」（`allow_resubmit`，同一实例），`instance_code` 不变。 |
| 聚合派发 | 生产上**有些配置启用、有些没启用** `aggregate_batch`，两种都要支持。 |
| 一期同步范围 | **所有可映射控件类型**：文本/多行文本/数字/金额/公式/日期/日期区间/单选/多选/电话/人员/部门/关联审批/附件/图片/控件组子控件/明细/嵌套明细/外部选项。只有「目标列本身不可写」或「聚合顶层派生值无法归因」才走显式降级，不再按字段类型分期。 |

### 1.3 明确不做

- 不处理「**再次提交**」（`allow_submit_again`，新实例、新 `instance_code`）——那是另一条路径，现有任务身份链查不到。
- 不处理拒绝/撤回后重新发起的新实例。
- 不做无边界双向同步：只同步「配置了映射、目标列可写」的字段；不可逆的聚合派生值按显式模式处理（`leader / all_members / skip`），不静默跳过。
- 不自动创建/删除多维表记录；聚合明细行增删/重排不自动建删行。

## 二、第一性原理：为什么这是「投影回写」而不是「双向同步」

正向转换 `F: 多维表格单元格 → 审批 form` **不是单射**：

- 聚合组顶层 `formula` 是成员求和；
- 顶层附件/图片是「文件名+大小」去重后的并集；
- 顶层多选是成员选中项并集；
- 附件 `file_token` → 审批 file code → 回读是 12 小时签名 URL；
- 关联列 `record_id` → 实例 Code；
- 日期毫秒被格式化成秒级 RFC3339；
- `fieldList` 只有数组顺序，没有行主键。

因此不存在全局逆函数。本需求可行的前提是：

> **台账是审批结果的投影，审批值是事实源；对可逆轴做回写，对不可逆轴按显式降级规则处理（`leader / all_members / skip`），并在控制台说明原因，不静默丢数据。**

因为台账已锁定、审批是唯一事实源，**冲突策略就是「审批优先」**：不需要逐字段 CAS，也不需要判断「台账是否被人工改过」。实现只需要：

1. 检测「退回 + 同实例重提」；
2. 读取当前台账；
3. 把审批 form 反向映射成目标单元格；
4. 与当前台账比较，只写差异；
5. 附件按需下载/上传，外部选项按需反查文案。

## 三、飞书契约（已确认 / 待真机）

### 3.1 已确认

| 事实 | 说明 |
|---|---|
| 退回 | task 级 `ROLLBACK`；实例状态仍 `PENDING`；当前任务 `ROLLBACK`，被退回人任务 `PENDING`。 |
| 同实例重提 | `allow_resubmit=true` 时退回后可在同一实例内「提交」；API 为 `POST /open-apis/approval/v4/tasks/resubmit`，需带原 `instance_code`、`task_id`、完整 `form`。 |
| 实例详情 | `GET /open-apis/approval/v4/instances/:id` 返回 `form`、`status`、`task_list`、`timeline`、`uuid` 等。 |
| 回读形态 | `date` 为 RFC3339；`dateInterval` 为 `{start,end,interval}`；`radio/checkbox` 回读文案，外部选项回读选项 ID；`contact` 的 `open_ids` 是 open_id；`attachmentV2` 的 `value` 是 12 小时签名 URL、`ext` 是文件名；`connect` 是实例 Code 数组；`fieldList` 是二维数组。 |
| 多维表写入 | `batch_update` 的 `fields` 按**当前字段名**作键；可写类型 1/2/3/4/5/7/11/13/15；17 附件需先上传素材。 |
| 附件上传 | `POST /open-apis/drive/v1/medias/upload_all`，multipart：`file_name / parent_type(bitable_file 或 bitable_image) / parent_node(App Token) / size / file`；≤20MB；返回 `file_token`。 |
| 事件 | 每个审批事件已由事件 worker `GET` 详情；HTTP 200 是飞书判定推送成功的唯一信号，5xx 会重推。 |

### 3.2 待 P0 真机验证（阻塞）

1. **`form` 最新性**：同实例退回→工作台修改→提交后，`GET` 的 `form` 是否返回最新值？是否有延迟？这是 go/no-go。
2. **台账锁的边界**：多维表台账锁定后，**应用身份（tenant token）是否仍能 `batch_update`**？如果锁也挡住应用，本需求需要先调整锁或权限。
3. **附件回读形态**：`attachmentV2/image` 的 `value` 是 URL 数组还是逗号串？`ext` 是否含文件名/大小？签名 URL 下载是否需要鉴权？
4. **附件写入形态**：`upload_all` 的 `parent_node` 是否必须是 App Token、`parent_type` 取值、20MB 边界；写回附件字段的准确形态 `[{"file_token":"..."}]`。
5. **外部选项回读**：`option.key` 是否等于本系统 `option_id`；`option.text` 是否可能为空；`feishu_option` 能否按 `source_key + option_id` 命中（含停用/删除/同名合并）。
6. **清空语义**：文本空串、数字/日期/单选 `null`、多选/人员 `[]`、复选框 `false` 在 `batch_update` 中的实际效果。
7. **重提事件**：重提本身是否推送 `approval_task` / `approval_instance` 事件；若完全不推，周期对账是唯一触发。
8. **明细行编辑**：退回后聚合明细是否允许增行/删行/重排；行序是否稳定。

## 四、代码现状与缺口

### 4.1 已有能力

| 能力 | 位置 |
|---|---|
| 每条事件已 `GET` 详情并拿到 `detail.form` | `src/infrastructure/feishu_event_worker.rs:987-1252` |
| 实例表单解析 | `src/addon/feishu/domain/approval_export/form.rs`（`InstanceForm` / `FormNode` / `FixedNumber`） |
| 控件映射与正向转换 | `src/addon/feishu/domain/approval_convert.rs`、`approval_provision.rs` |
| 目标单元格编码 | `bitable::encode_export_cell`（1/2/3/4/5/7/11/13/15） |
| 批量读写 | `bitable::batch_update_records`、`get_records_by_ids`（≤100） |
| 组身份与成员序 | `feishu_approval_group` + `load_members`（`group_seq asc, id asc`） |
| 失败隔离 | `backfill_in_chunks` 对半拆批定位毒记录 |
| 附件上传范式 | `approval_attachment.rs` 的 multipart 与下载/上传（方向是「多维表→审批」，需另写反向） |

### 4.2 缺口

1. 事件 worker 只调用 `write_extended` 写状态/流程/时间/处理人，**不回写表单业务字段**。
2. `run_round` 在 `events.is_empty()` 时提前 `return`；周期对账要放到这个提前返回之外。
3. 事件 `operate_time` 入站存秒，`task_list.start_time` / `timeline.create_time` 是毫秒；必须显式归一，禁止混用。
4. `FormNode` 丢弃 `contact` 顶层 `open_ids`；`is_empty` 对 `false`/`0` 的判定与业务空值不同。
5. `batch_update` 按字段名写，写库前必须 `list_all_fields` + `resolve_field_names` 把 `field_id` 解析成当前名。
6. 主字段、`batch_scope_field`、评论/导出/外部选项源列不在现有 denylist，表单同步可能触发副作用。
7. 附件反向（审批签名 URL → 多维表素材上传）完全缺失。
8. 外部选项反向（`option_id → label`）缺失；正向的 `option_map` 是派发时逐记录解析的，未持久化。
9. 聚合组的 `member_row_keys` 未冻结；明细行没有行主键。
10. `task.updated_at` 只有 `sortable` 没有 `filterable`，不能做扫描条件；需要独立的调度列。

## 五、设计

### 5.1 总体结构

```text
飞书审批事件 ──> event_worker（已 GET detail）
                    │
                    ├─ 更新 instance_status / 扩展字段（现有）
                    │
                    └─ approval_form_sync（新增）
                          ├─ 退回+重提检测（timeline/task_list）
                          ├─ 反向映射（文本/数字/日期/选项/多选/电话/复选框/人员）
                          ├─ 附件：签名 URL 下载 → medias/upload_all → file_token
                          ├─ 外部选项：option_id → feishu_option.label
                          ├─ 读当前台账 → 字段级 diff
                          └─ batch_update（只写差异）

周期对账（run_loop 每 tick，事件为空也跑）
    └─ 扫描 awaiting_resubmit / next_at due 的 task/group，重跑同一同步函数
```

**不新建表**：复用 `feishu_approval_task` / `feishu_approval_group` 行作为状态载体。

### 5.2 触发：如何判断「退回 + 同实例重提」

每次事件已经拿到 `detail`，直接看 `detail.timeline` 与 `detail.task_list`：

1. `last_rollback_ms` = `timeline` 中 `type in {ROLLBACK, ROLLBACK_SELECTED}` 的最大 `create_time`（毫秒）。
2. 退回目标是否为发起人：
   - `timeline[].task_id` 指向的是**被退回**的审批任务，不是退回目标；审批人与发起人不同时不能只看它的 `open_id`；
   - 退回后新建的任务（`start_time > last_rollback_ms`）才指向目标节点：该任务的 `open_id == detail.open_id` 或节点是 `START` 即发起人退回；
   - `timeline.ext` 的 `rollback_node_ids` 含 `START` 也成立；
   - 不能证明时记 `rollback_target_unknown`，不同步。
3. 重提证据：退回之后**发起人任务完成**（`task_list[].end_time > last_rollback_ms`），或实例已进入终态。
   **不能**用「出现新任务」（`start_time > last_rollback_ms`）：真实飞书在 `ROLLBACK_SELECTED` 之后十几毫秒就创建发起人任务，
   它的 `start_time` 只比退回时间晚一点、`end_time` 仍是 0，用户还没有重提（2026-11-17 真机实测）；
   按新任务判断会提前同步离线表单并标记 `synced`，真正的重提事件随后全部被 `rollback_already_handled` 跳过。
4. 若 `last_rollback_ms > task.form_sync_last_rollback_ms` 且有重提证据 → 执行同步。
5. 同期内重复事件、乱序事件：只接受更大的 `last_rollback_ms`；旧事件不改变已同步状态。
6. 若事件只看到退回、尚未重提：写 `form_sync_state='awaiting_resubmit'`，`form_sync_next_at=now+复扫间隔`，交给周期对账。

**周期对账**：
- 每轮扫描 `form_sync_state='awaiting_resubmit'` 或 `form_sync_next_at <= now` 且 `instance_code` 非空的 task/group；按 `instance_code` 聚组，同一实例一轮只 `GET` 一次。
- 事件丢失或重提无事件时，靠它最终一致。
- `run_loop` 每个 tick 都执行，不受 `events.is_empty()` 影响。

### 5.3 同步算法（单条 / 非聚合）

输入：`task.record_id`、`config_id`、`detail.form`、`detail.timeline/task_list`。

1. 解析 `InstanceForm::parse(detail.form)`；空/非法 → `review`。
2. 加载 `feishu_approval_field_map`（`widget_id / widget_type / bitable_field / converter / option_map / option_source_key / interval_end_field`）。
3. 对每个映射控件生成 `desired`：
   - 文本/数字/日期/电话/复选框：`FormNode` → `SyncValue`；
   - 固定选项：回读文案/值 → `option_map` 反查 → 目标列选项名；
   - 外部选项：`feishu_option(source_key, option_id)` → `label`；回读是文案时按 `labels_equivalent` 匹配；
   - 多选：逐项映射，任一项失败则整个字段跳过；
   - 附件/图片：`value` 签名 URL + `ext` 文件名 → 下载 → `upload_all` → `file_token`；
   - 控件组：递归索引 `FormNode.value`，子控件按各自类型处理；
   - 关联审批：文本/超链接/同表关联列（实例 Code → record_id 反查）；
   - 部门：`open_department_id → contact 部门名`，写文本列；
   - 嵌套 `fieldList`：按正向语义只支持单行；
   - 无法落地的形态（目标列不可写、超过 20MB 的附件、旧 `document/address` 无可用 URL）→ 字段级 `review` 并记原因。
   - 全类型规则见 §5.3.1。
4. 读当前台账：`get_records_by_ids([record_id])`，用 tolerant rekey（按 `field_id`；忽略非目标未知列；目标列缺失才失败）。
5. 逐字段比较 `desired` 与 `current`：
   - 相等 → 不写；
   - 不同 → 进入 patch；
   - 附件：按 `name + size`（或 `name`，若 size 不可得）比较；相同跳过，不同则上传并写新数组；
   - 外部选项：按目标列现有选项名比较；
   - 清空：仅当 `form_sync_allow_clear=true` 且 P0 确认语义后写。
6. `list_all_fields` + `resolve_field_names` 把 `field_id` 解析成当前字段名。
7. `batch_update_records` 写 patch（单记录一次；聚合按 100 条分片）。
8. 写后回读校验（可选但推荐）：`batch_get` 确认目标列等于 `desired`；不一致 → `review`。只读列（19/20）不写，但同样回读比对，不一致 → `readonly_mismatch` review + 告警。
9. 更新 `form_sync_last_rollback_ms / form_synced_at / form_sync_hash / form_sync_error`。

**幂等**：`form_sync_hash` = `desired` 的规范化哈希；相同则跳过。写成功但哈希落库失败，下一轮 `current == desired` 只补哈希，不重复写。

### 5.3.1 全字段矩阵（一期）

| widget_type | 目标列 | 一期支持 | 写入规则 / 边界 |
|---|---|---|---|
| `input` / `textarea` | 1 文本 / 3 单选 | ✅ | 文本目标写字符串；单选目标只接受目标列已有选项，未命中 → `option_not_found` review + 告警，绝不自动新增选项 |
| `number` / `amount` | 2 数字 | ✅ | `FixedNumber`；整数精确；小数做 f64 往返校验；超精度/非法格式 → 字段 `review` |
| `formula` | 2 数字 / 19/20 只读 | ✅ | 目标 2 写公式结果；目标 19/20 不写，写后回读比对，不一致 → `readonly_mismatch` review + 告警；聚合顶层见 §5.4 |
| `date` | 5 日期 | ✅ | 严格 RFC3339 → 绝对毫秒；`0`/`"0"`/缺失 → Empty，绝不写 0 |
| `dateInterval` | 5+5 两列 / 5 单列 / 1 文本 / 19/20 只读 | ✅ | 两列：start/end 都写；单列：写 start 并记 `end_not_written`；文本：写 `"start ~ end"`；19/20 只读比对 |
| `radio` / `radioV2` | 3 单选 / 2 数字 / 1 文本 | ✅ | 固定选项 `option_map` 反查；外部选项 `feishu_option` 反查；目标 2 时把选项文案按十进制解析，解析失败 → `invalid_value` review |
| `checkbox` / `checkboxV2` | 4 多选 / 1 文本 | ✅ | 逐项映射；任一项失败整个字段 `review`；文本目标用 `、` 连接 |
| `select` / `multiSelect` | 3 / 4 / 1 | ✅（定义出现时） | 同 radio/checkbox |
| `telephone` | 13 电话 / 1 文本 | ✅ | `"+CC national"`；保留区号，不默认 +86 |
| `contact` | 11 人员 / 1 文本 | ✅ | 优先 `open_ids`；只有 user_id 时能解析则解析，否则 `review`；文本目标写 open_id 或姓名（可配置） |
| `department` | 1 文本 | ✅ | `open_department_id → contact 部门名`；解析失败写 ID 原文；目标 11 人员列配置期拒绝 |
| `connect` | 1 文本 / 15 超链接 / 18/21 关联 | ✅ | 文本写实例 Code；超链接写 AppLink（多 Code 取第一条并 `review`）；同表关联列按回填列反查 record_id，写 `{"link_record_ids":[...]}`；跨表关联暂不支持（正向也不支持） |
| `attachmentV2` / `image` / `imageV2` | 17 附件 | ✅ | 下载签名 URL → `medias/upload_all` → `file_token`；≤20MB；超过走分片上传或 `review` |
| `attachment`（旧） | 17 附件 / 1 文本 | ⚠️ | 回读若给 URL 则按附件处理；只有 code 无法下载 → `review`；正向本就需要人工 code |
| `address` / `document` | 1 文本 / 17 附件 | ⚠️ | 回读若给可下载 URL/可读文本则支持；否则 `review`；正向本就需要人工值 |
| 控件组子控件（`leaveGroupV2` / `workGroup` / `outGroup`） | 按子控件类型 | ✅ | 递归索引 `FormNode.value`；子控件按各自类型处理；假勤/加班类型 ID 直取 |
| 顶层 `fieldList`（明细） | 目标行 | ✅ | 非聚合 `row_count=1`；聚合 `row_count=成员数`，按行键/下标匹配 |
| 嵌套 `fieldList` | 目标行 | ✅（单行） | 嵌套只支持一行；多行 `review`（正向也只产出一行） |
| 控件组内嵌套 `fieldList` | 目标行 | ✅ | 聚合顶层按正向语义取 leader；写 leader 或按 `aggregate_derived_mode` |
| 不支持类型（`text` / `mutableGroup` / `account` / `serialNumber` / …） | — | ❌ | 官方/本系统本就不支持提单，无法反向同步；配置期标记 |

**硬边界（不是分期，是能力边界）**：

1. 目标列 19/20（查找/公式）**只读校验**：不写，写后回读与审批表单同字段值做归一化比较，不一致 → `readonly_mismatch` review + 告警；18/21（关联，除同表反查）、22（地理位置）、1001-1005（自动字段）不可写 → 配置期拒绝或运行期 `review`。
2. 聚合顶层派生值（formula 求和、附件/图片并集、多选并集）无法归因到成员 → 由 `form_sync_aggregate_derived_mode` 控制（见 §5.4）。
3. 旧 `attachment` / `document` / `address` 正向本来就需要人工值；配置里出现时按回读形态尽力处理，处理不了 `review`。
4. 附件超过 20MB 需要分片上传；一期可先 `review`，业务确有超大附件再补 `upload_prepare/upload_part/upload_finish`。
5. 控件组/嵌套明细的形态必须与正向产出一致；用户新增多行嵌套明细 → `review`，不猜。

**只读列校验与告警**：19/20 目标不进入 `patches`，只进入 `verifications`；有写入时在写后回读，无写入时直接比对当前记录。数字文本会去千分位/币种符号后按 `FixedNumber` 比较，文本做 trim，多值排序去重；不一致只记录稳定的 `readonly_mismatch` 结果码，不把字段值写进 `form_sync_error`/日志。告警默认发给审批发起人（`detail.open_id`），正文只带配置、审批编号、记录、字段名、错误码和数量，不带金额/姓名等字段值；发送失败只记 warning，不改变同步结论。

### 5.4 聚合组

生产上部分配置启用 `aggregate_batch`，必须处理：

1. **成员与行序**：
   - 组行 `feishu_approval_group` + 成员任务行；成员顺序 = `group_seq asc, id asc`（`group_seq` 可能留洞，不能当数组下标）。
   - 在 create 前冻结 `group.member_row_keys`（`v=1, rows[{record_id, row_key?}]`）；恢复路径不重建。该列没有 `filterable`，写入前必须先读组行再按 `config_id + group_key` 更新，不能拿它当 WHERE 条件。
   - 每个被映射的顶层 `fieldList` 必须是同一行数；控件组内的整单共享 `fieldList` 不参与这项校验（按 leader / 派生模式处理，多行 `review`）。
2. **行匹配（无行键时的内容多重集）**：
   - 有 `form_sync_row_key_widget` 的存量配置继续按业务键匹配（行为不变）。
   - 没有行键时，身份只能来自**已映射字段的内容**：每条表单行算一个规范化内容签名（排序去重、trim、数字按十进制、附件按 name+size），台账行用同一套 `current_value` 反投影。签名完全相同的行先互相抵消；剩余行按顺序配对并整行覆写，剩余表单行新建、剩余台账行软作废。
   - 不按“位置 + 相似度”猜哪一行被修改：任意配对后每个计划都会完整覆写一条记录，最终活动行的多重集一定等于表单行集合。行数不变时优先复用原记录，只有数量变化才新增/作废。
   - leader/锚点固定吃第 0 条表单行：第 0 条计划可能带 leader 专属派生值（顶层公式/附件并集/多选并集），只能写 leader。其余行仍按内容匹配。
   - 软作废**不新增列**：在 `申请编号` 等可承载文本的回填列原值末尾追加 `-台账作废`（超链接保留原 link）。只写文本（1）/超链接（15）列；单选、数字、日期、人员等格式不允许的列跳过；`申请编号` 是必写列，无法承载标记时整条作废失败并 `review`。重复同步按“已以 `-台账作废` 结尾”判幂等，不会重复追加。
   - 新建行会复制 leader 的未映射列（`申请编号`、`申请状态`、`批量选择`、发起人/时间等），**映射列先全部清空再按本行计划覆写**，避免重复派发和串值。
   - 新增记录同时插入 `state=backfilled` 的任务行：聚合组带 `group_id/group_seq`，下次退回同步才看得见。
   - 无行键且用户重排：内容相同的行互换无差别；内容不同的行按上述配对，最终数据仍正确，但记录 id 可能变化。这是无行键的固有限制。
3. **单条任务**：
   - 一条单条审批当前对应一条台账记录；明细行数 `!= 1` 时整单 `review`（`row_count_mismatch`），不再静默只写第 0 行。
   - 单条多明细 → 多台账记录需要独立的状态/事件回填模型，二期再做。
4. **顶层共享标量**：文本/数字/日期/固定选项/电话/复选框 → 写全部成员（派发时签名保证一致）。
5. **顶层派生值**（`formula` 求和、附件/图片并集、多选并集）无法归因到成员，由 `form_sync_aggregate_derived_mode` 控制：
   - `leader`（默认）：写到 leader 行；
   - `all_members`：镜像到所有成员（适用于共享/汇总列）；
   - `skip`：不写，控制台列明原因。
   推荐把需要逐成员统计的附件/多选放到明细行内。
6. **明细行内字段**：按成员独立值写；行内附件/图片、外部选项、人员、控件组子控件均支持；行内 `formula` 仅在目标列是可写数字列时写；嵌套 `fieldList` 仅支持单行，多行 `review`。
7. **部分失败**：成功成员更新，失败成员保留旧值并 `retry/review`；组状态按成员聚合。

### 5.5 附件（一期）

**读取**：`FormNode.value` 是签名 URL（数组或逗号串），`ext` 是文件名；P0 确认形态。

**下载**：`GET` 签名 URL（12 小时有效）；用现有 HTTP 客户端，`binary_limit` 设上限；5 QPS 节流（每次 sleep 200ms）；429 退避。

**上传到多维表**：
- `POST /open-apis/drive/v1/medias/upload_all`
- multipart：`file_name`、`parent_type=bitable_file`（普通附件）或 `bitable_image`（图片控件）、`parent_node=<base app_token>`、`size=<字节数>`、`file=<二进制>`。
- 返回 `data.file_token`；写入附件列 `[{"file_token":"..."}]`，顺序按审批 form 顺序。
- ≤20MB 用 `upload_all`；超过 20MB 走分片上传（`upload_prepare / upload_part / upload_finish`）；一期若暂不实现分片，则 `review` 并记 `attachment_too_large`。
- 上传无幂等键；重试可能产生孤儿文件（无删除 API），但不影响台账正确性；同一单元格按 `file_token` 数组覆盖。

**去重**：
- 当前台账附件 `[{"file_token","name","size"}]`；若 desired 的 `name+size` 集合与当前完全一致 → 不下载/不上传。
- 文件被替换但同名同大小 → 可能漏判；P0 确认 `ext` 是否含大小，否则接受此限制。

**清空**：用户删除附件 → 写空数组 `[]`；仅 `form_sync_allow_clear=true`。

### 5.6 外部选项（一期）

**正向**：派发时 `resolve_external_options` 把 `label → option_id` 填进内存 `WidgetMap.option_map`，未持久化。

**反向**：
1. 回读 `FormNode` 的 `option` / `value`：
   - 优先 `option.key` / `value` 里的 option ID；
   - 若只有文案，按 `feishu_option(source_key)` 的 `label` 用 `labels_equivalent` 匹配。
2. `feishu_option` 按 `source_key + option_id` 查 `label`；查不到/停用/同 label 多 id → 该字段 `review`。
3. 固定选项：反查配置 `option_map`（`label → value`）得到 `value → label`；多对一 → `review`。
4. 目标列是单选/多选：写目标列现有选项名（`labels_equivalent` 匹配）；不存在时写文案，由多维表自动创建选项（P0 确认是否接受）。
5. 多选：逐项映射，任一项失败则整字段跳过。

### 5.7 写入与失败处理

- **字段名解析**：`list_all_fields` + `resolve_field_names`；重名/缺失 fail-closed。
- **批量**：单记录一次 patch；聚合按 100 条分片；Fatal 对半拆批定位毒记录，Retry（429/1254290/1254291/5xx）不拆批。
- **写后回读**：默认开启；不一致重试一次，仍不一致 → 该记录 `review`。
- **失败分类**：
  - 附件下载/上传失败 → 重试；耗尽后该字段 `review`，其他字段继续；
  - 外部选项反查失败 → 该字段 `review`；
  - 目标列 deleted/forbidden → 该记录 `review`，不重建；
  - 台账锁/权限拒绝 → 记录级 `review`，告警（P0 确认应用是否可写锁定行）；
  - 表单缺失/非法 → `review`。
- **事件预算**：表单同步业务失败**不消耗**事件 10 次重试预算；只有同步状态写库失败才让事件重试。
- **重试**：`form_sync_next_at` 退避 30s/2m/10m/30m/1h/2h；达上限 `review`；控制台 `retry_form_sync` 只重置状态，由 worker 持租约执行。

### 5.8 与现有回写/评论/导出的关系

- `write_extended`（状态/流程/时间/部门/处理人/申请人/编号）与表单同步**分开写**，列集合不相交。
- 表单同步**不通过字段映射写**：`applicant_field / backfill_field / status_field / flow_field / start_time_field / end_time_field / department_field / handler_field / batch_scope_field`、主字段、评论规则列、导出规则列、外部选项源列。新增明细行时，这些未映射列从既有 leader/成员行复制，避免新行因回填列为空被再次派发。
- 同步**不创建新列**。唯一例外是软作废：在 `申请编号`（必写）以及配置了且格式允许的 `status_field / flow_field / department_field` 原值末尾追加 `-台账作废`；人员/日期/数字/单选等不接受文本的列保持原值。作废行仍保留审批链接，便于从台账回到原实例。
- 表单同步写入会触发多维表格 `record_changed` 事件；评论 worker 对「form sync 刚写过的 record」加 60 秒抑制窗口。
- 聚合顶层附件/多选并集按 `form_sync_aggregate_derived_mode` 处理（默认写 leader，可选 all_members/skip），不静默丢失。

### 5.9 可观测性

- metrics：`fetched / synced_records / written_fields / attachments_uploaded / options_resolved / skipped{reason} / review / retry / backlog`。
- 日志只记 `event_id / task_id / group_key / record_id / widget_id / field_id / 结果码`，不记表单值。
- 控制台：任务/组行显示 `form_sync_state / form_synced_at / form_sync_error`；配置详情列出「不会自动同步的字段及原因」。

## 六、数据模型（最小集）

### 6.1 `feishu_approval_config`

| 列 | 类型 | 说明 |
|---|---|---|
| `form_sync_mode` | `Radio<String>`，required，`varchar(16)`，default `off` | `off / shadow / live`；`shadow` 只计算和记录，不写台账 |
| `form_sync_allow_clear` | `Switch`，required，default `false` | P0 确认清空语义后可开 |
| `form_sync_row_key_widget` | `Str(128)`，可空 | 聚合明细行键子控件；为空时按行下标匹配 |
| `form_sync_aggregate_derived_mode` | `Radio<String>`，required，`varchar(16)`，default `leader` | 聚合顶层派生值（公式求和/附件并集/多选并集）写哪：`leader / all_members / skip` |

### 6.2 `feishu_approval_task` / `feishu_approval_group`

| 列 | 类型 | 说明 |
|---|---|---|
| `form_sync_state` | `Radio<String>`，required，`varchar(32)`，default `disabled` | `disabled / awaiting_resubmit / synced / retry / review / closed`；`awaiting_resubmit` 17 字符，必须 `varchar(32)` |
| `form_sync_last_rollback_ms` | `Int`（BIGINT），可空，filterable | 最近一次退回的源毫秒时间；只增不减 |
| `form_sync_hash` | `Str(64)`，可空 | 最近一次成功同步的 `desired` 规范化哈希 |
| `form_synced_at` | `Timestamp`，可空 | 最近成功同步时间 |
| `form_sync_next_at` | `Timestamp`，可空，filterable+sortable | 周期对账/重试调度 |
| `form_sync_attempts` | `Int`，required，default 0 | 重试预算 |
| `form_sync_error` | `Text`，可空 | 只存结果码与中文说明，不存表单值 |
| `form_sync_lease_until` / `form_sync_worker` | `Timestamp` / `Str(128)`，可空 | 同步租约，单实例也启用 |

组额外：

| 列 | 类型 | 说明 |
|---|---|---|
| `member_row_keys` | `Text`，可空 | create 前冻结的成员行序快照；只写一次 |

> 若一期不做 `member_row_keys`（例如先只支持非聚合），可先不加该列；但既然生产有聚合配置，建议一期就加上。

### 6.3 配置项

| 配置 | 默认 | 说明 |
|---|---|---|
| `feishu.approval_form_sync_enabled` | false | 部署级全局开关；P0 通过后打开 |
| `feishu.approval_form_sync_scan_interval_seconds` | 15 | 周期对账间隔 |
| `feishu.approval_form_sync_batch` | 20 | 单轮最多处理实例数 |
| `feishu.approval_form_sync_active_interval_seconds` | 600 | `awaiting_resubmit` 复扫间隔 |
| `feishu.approval_form_sync_lease_seconds` | 120 | 同步租约 |
| `feishu.approval_form_sync_bitable_qps` | 5 | 多维表读写与附件上传节流 |
| `feishu.approval_form_sync_comment_guard` | true | 评论回环抑制 |
| `feishu.approval_form_sync_max_attachment_bytes` | 20MB | 附件上限；超过 review |

不需要 HMAC 密钥/逐字段基线：台账锁定、审批优先，直接比对当前台账与 desired。

## 七、配置与控制台

- `create_config` / `update_config` / `list_configs`：增加 `form_sync_mode`、`form_sync_allow_clear`、`form_sync_row_key_widget`；旧客户端缺字段按默认值。
- 保存配置时校验：
  - `form_sync_mode=live` 时，目标列必须可写且类型在矩阵内；
  - 系统列/主字段/评论/导出/外部选项源列冲突 → 拒绝；
  - 聚合配置的 `form_sync_row_key_widget` 必须是明细子控件（若配置）；
  - 同一 `bitable_field` 不被多个控件占用。
- 前端配置详情「接入数据」tab 增加同步模式、清空开关、聚合行键字段、风险提示、不可同步字段清单。
- 任务页增加同步状态、最近同步时间、错误 tooltip、`retry_form_sync` 按钮。
- 新增 Action `retry_form_sync`（一文件一 `handle` 一 `register`；只重置状态，由 worker 执行）。
- 重新生成 OpenAPI / `api-types.ts`。

## 八、分期

| 阶段 | 内容 | 出口条件 |
|---|---|---|
| P0 真机 | 同实例退回→修改→提交后 `GET form` 最新性；台账锁下应用可写；附件/外部选项/清空形态；重提事件；聚合明细行编辑；控件组/关联/部门/日期区间/嵌套明细回读 | 全部通过 |
| P1 领域层 | `approval_form_sync` 纯函数 + 单测：**全字段**反向映射、附件下载/上传、外部选项反查、关联/部门/控件组/嵌套明细、diff、编码；shadow 模式 | 单测全绿；shadow 观察 1-2 周 |
| P2 非聚合 live | **全字段**：文本/数字/金额/公式/日期/日期区间/选项/多选/电话/人员/部门/关联/附件/图片/控件组/嵌套明细/外部选项；单条配置先灰度 | 真实退回重提后台账正确 |
| P3 聚合 live | `member_row_keys`、顶层共享标量、明细行映射、明细附件/选项/人员、派生值 `aggregate_derived_mode`；行数/行键不一致 review | 聚合配置验证通过 |
| P4 收尾 | 控制台、告警、重试、文档、运维手册 | 可运维 |

**工作量粗估**：P0 1-2 人日；P1 10-15 人日（全字段 + 附件/外部选项/关联/部门/控件组）；P2 6-10 人日；P3 8-12 人日；合计约 25-39 人日。

## 九、P0 验证清单（阻塞）

1. 测试审批定义显式 `allow_resubmit=true`；审批人退回；发起人修改**文本/数字/金额/公式/日期/日期区间/固定选项/外部选项/多选/人员/部门/关联审批/电话/附件/图片/控件组子控件/明细行/嵌套明细**后点「提交」；在 +0/+10s/+2min/+10min/+1h `GET` 详情：
   - `form` 是否反映修改？
   - `instance_code/uuid` 是否不变？
   - `timeline/task_list` 是否出现可判定的退回+重提序列？
2. 台账行锁定后，用应用身份 `batch_update` 是否成功？失败码是什么？
3. 附件：`attachmentV2/image` 的 `value/ext` 形态；签名 URL 是否需要鉴权；`upload_all` 的 `parent_type/parent_node/size` 与返回 `file_token`；写回附件列后立刻 `batch_get` 是否可见。
4. 外部选项：`option.key/text/value`；`feishu_option` 按 `source_key + option_id` 是否命中；停用/同名合并如何表现。
5. 清空：各类型清空的 API 语义。
6. 聚合：明细行能否增/删/重排；行序；行键控件是否可加。
7. 事件：重提是否推送事件；v1 还是 v2 信封。

## 十、降级

若 `form` 不是最新值，或台账锁阻止应用写入：

1. 先解决锁/权限（应用加入可写角色/白名单）。
2. 若 `form` 旧快照：只能由系统代调 `tasks/resubmit`（需要用户在本系统改单）或改走 append-only 快照 + 人工采纳；不能包装成「工作台修改自动跟随」。
3. 附件/外部选项若 P0 形态不支持，逐项降级为「跳过+控制台提示」，不静默写错。

## 十一、参考来源

- <https://open.feishu.cn/document/server-docs/approval-v4/instance/get>
- <https://open.feishu.cn/document/server-docs/approval-v4/instance/create>
- <https://open.feishu.cn/document/server-docs/approval-v4/task/resubmit>
- <https://open.feishu.cn/document/server-docs/approval-v4/event/common-event/approval-task-event>
- <https://open.feishu.cn/document/server-docs/approval-v4/event/common-event/approval-instance-event>
- <https://open.feishu.cn/document/server-docs/approval-v4/approval-related-faqs>
- <https://open.feishu.cn/document/docs/bitable-v1/faq?lang=zh-CN>（附件上传/字段格式）
