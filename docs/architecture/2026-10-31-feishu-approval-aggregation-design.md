# 飞书审批「同批聚合派发」— 设计

- 文档性质：设计（决策记录 + 实施约束 + 验证清单）
- 撰写日期：2026-10-31
- 需求来源：运营反馈「同批多条记录逐条建单太吵」，产品负责人明确要求实现
- 状态：待评审
- 关联：
  - `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`（派发主设计）
  - `docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md`（请求日志）
  - `docs/contracts/SCHEMA.md`（schema 增量同步规则）

## 一、目标

在审批派发配置上新增一个开关 `aggregate_batch`（默认关闭）。开关打开后：

1. 同一次批量勾选（同一配置、复选框列为 true、回填列为空）的记录中，**除明细控件子树外的所有已映射字段，按转换后的表单值完全一致**的记录，合成为一个审批实例。
2. 明细控件内每条记录一行；组内所有记录回填同一个审批编号；每组只发一条通知。
3. 开关关闭时行为与现状逐字节一致，存量配置零感知。

非目标：

- 不跨配置、跨批次、跨发起人聚合；不自动把未勾选的记录并入。
- 不支持明细行级审批（飞书 approve/reject 是实例粒度，无行号参数）。
- 不改单条派发语义：行内按钮仍然独立建单（见 §六）。
- 不新增独立的组控制台页面；组通过 leader 任务行与控制台任务列表可见。

## 二、已确认的产品决策

| # | 决策 | 结论 |
|---|---|---|
| 1 | 开关位置与默认值 | 配置级开关，默认关闭；开启时必须已配置批量范围复选框列，否则保存拒绝 |
| 2 | 聚合范围 | 同一次批量勾选内的记录；每次手动点击按**当时的勾选**重新分组尚未提交的组（已建实例的组不动） |
| 3 | 一致性口径 | 比较转换后的表单值：选项按解析结果、日期/数字按转换结果、`{type,value}` 包装归一；附件按源文件 `file_token+name+size`（保持源顺序） |
| 4 | 组内校验失败 | 整组暂停、不派发、不写任何成员的回填列；飞书通知提醒「修好后重新点击」 |
| 5 | 重试方式 | 暂停组只能手动点击重试，轮询不自动重试 |
| 6 | 通知粒度 | 每组一条 |
| 7 | 单条点击 | 允许独立建单（即使该记录属于某个组，允许出现两个实例） |
| 8 | 组行数上限 | 不设业务上限；内部按 100 条分片读写，上线前真机验证大组 |

> 决策 2 的含义：组成员集合只在播种那一刻确定；两次点击之间不会有任何自动增删。批量按钮再次点击时，未提交的组按当前勾选重新分组，已提交（`instance_code` 已落库）的组保持原成员；单条行内点击不触发重组。
> 这是决策 4「暂停后修复再点」能闭环的前提：修复后数据的签名会变化，必须重新分组。

## 三、现状与差距

### 3.1 身份链

现状是严格的一对一：一条多维表格记录 = 一行 `feishu_approval_task` = 一个按 `record_id` 派生的 `uuid` = 一个审批实例 = 一次编号回填。

- 任务表唯一键 `(config_id, record_id)` 与 `uuid`（`src/addon/feishu/approval/domain/task_table.rs:52-142`）。
- `uuid = 规范化 UUIDv5/SHA-256(base_token|table_id|approval_code|record_id)`，同时是重试幂等键与 `60012` 回捞键（`src/addon/feishu/domain/approval_uuid.rs:36-72`）。
- 认领只认 `state=pending`、`effect_phase ∈ {ready, created}` 的行，租约 + `FOR UPDATE SKIP LOCKED`（`src/addon/feishu/domain/approval_execution/repository.rs:173-203`）。
- 真正的状态机是 `pending → (effect_phase creating/created) → backfilled / terminal / unnotified / review`；`state=creating/created` 是历史死值。
- 事件按 `instance_code` 反查任务行后取 `.first()`，只回写这一条记录（`src/infrastructure/feishu_event_worker.rs:562-589,806-840`）。
- 回填只写 `input.record_id`；通知去重按 `(record_id, config_id)` 查请求日志；人工重开按被点击配置逐行重置。
- 代码里没有任何组/批次概念；批量范围复选框只是播种过滤器（`bitable.rs:1573-1600`）。

### 3.2 明细单行是本地写死的

- `assemble_definition` 对每个 `fieldList` 只递归一次并写死 `value: [row]`（`src/addon/feishu/domain/approval_convert.rs:316-341`）。
- `FormInput` 只有一份 `cells` 与一张按 `widget_id` 索引的附件 code 表（同文件 `:191-208`）。
- 官方定义 `fieldList.value` 是二维数组（外层行、内层子控件），明细父级不允许落映射。

### 3.3 多行明细已被真机验证

2026-10-09 的探针报文（`target/approval-documented-request.json`、`...-response.json`、`...-readback.json`）显示：非明细控件各一组值、明细两行、两行子控件值不同，`create` 返回 `code=0`（实例 `A4B88F14-082F-4B41-87D1-A08949C2BD95`），回读仍是两行。

官方文档没有给出通用行数上限，也没有「支持多行」的明文；只验证过 2 行。**行数上限与超长请求体属于上线前真机待验证项**。

### 3.4 聚合的收益

- 创建接口限流按应用共享：默认 90、上限 100 次/分钟；现在每条记录一次 `create` + 一次 `get`（`docs/architecture/2026-09-28-feishu-approval-dispatch-design.md:564,719-722`）。聚合把 N 次 `create` 降到 1 次。
- 通知从 N 条降为 1 条，减少对发起人与审批人的打扰。

## 四、总体设计

### 4.1 组身份

- 组 = 一行新表 `feishu_approval_group` + 组成员任务行。
- 组键 `group_key` = `hex(sha256(config_id | "|" | signature_hash | "|" | 排序后的成员 record_id 用 "," 连接))[0..32]`。
  - 含成员集合，保证同签名但成员不同的两个组不会撞键。
  - `signature_hash` = `hex(sha256(规范化签名 JSON))[0..32]`，单独存组行，供处理期复核。
- 组 `uuid` = 复用现有 `derive_uuid(base_token, table_id, approval_code, group_key)` 派生并持久化到组行；这是组实例的 `60012` 回捞键。
- 组成员任务行：`group_id = group_key`，`group_seq` 从 0 开始；`group_seq=0` 是 leader（唯一可认领行），其余为成员。
- 成员行保留自己按 `record_id` 派生的 `uuid`（唯一键要求），**不参与组创建**；成员行被单条点击时正好用它独立建单。
- 已建实例的组：`group.instance_code / serial_number` 落组行；leader 与成员行镜像 `serial_number` 供控制台查看，`instance_code` 只落组行，避免与成员行的单条实例码互相覆盖。

### 4.2 数据模型

配置表 `feishu_approval_config` 新增：

```
aggregate_batch => Switch::new()
    .title("同批聚合派发")
    .require(true)
    .default(false)
```

任务表 `feishu_approval_task` 新增：

```
group_id  => Str::new().title("聚合组键").max_length(64).filterable(true),
group_seq => Int::new().title("组内序号").require(true).default(0),
```

- `group_seq=0` 且 `group_id` 非空 = 组 leader；`group_seq>=1` = 成员；`group_id` 为空 = 现有单条任务。
- 不做 `group_id` 唯一索引（一组 N 行）；查询按 `(config_id, group_id)`，`config_id` 已有索引，组内行数有限。

新表 `feishu_approval_group`（纯绑定表，无 Action；一张表一个 module，需在 `src/addon/feishu/approval/mod.rs` 与 `src/addon/feishu/mod.rs` 装配）：

```
id            => Key
config_id     => Int::new().require(true).indexed(true).filterable(true)
group_key     => Str::new().require(true).max_length(64).filterable(true)
uuid          => Str::new().require(true).unique(true).max_length(64).filterable(true)
signature_hash=> Str::new().require(true).max_length(64)
instance_code => Str::new().max_length(128).filterable(true)
serial_number => Str::new().max_length(64)
created_at / updated_at
unique_named("uk_feishu_approval_group_config_key", [config_id, group_key])
```

任务状态枚举新增 `paused`（Radio 选项，物理列是 VARCHAR，应用层校验，无 DB CHECK，属于纯增量）：

```
("paused", "已暂停")
```

`src/addon/feishu/approval/domain/task_table.rs:86-100` 的 options 列表、`delete_config` 的活跃状态白名单（`src/addon/feishu/approval/actions/delete_config.rs:91-103`）、`reopen` 的白名单（`src/addon/feishu/domain/approval_execution/repository.rs:105-114`）同步加入。

### 4.3 一致性签名

实现位置：`src/addon/feishu/domain/approval_convert.rs` 新增 `build_signature`，复用 `assemble_widget`；附件部分在 `src/addon/feishu/domain/approval_attachment.rs` 新增 `source_identities`。

流程：

1. 解析 `form_snapshot`，恢复控件树与映射（与 `build_form` 同一套预校验）。
2. 从定义树递归收集所有顶层 `fieldList` 节点，取 `walk()` 的全部 widget_id 作为**明细字段集合**，签名一律排除；明细父级本身也排除。
3. 对每个非明细、已映射控件调用现有转换器，得到与提交一致的片段（选项按解析后的 `option_map`；日期按配置时区；数字/公式归一；`{type,value}` 解包）。
4. 附件/图片：不调 `transfer`，用 `attachments()` 解析出的源文件按源顺序编码 `file_token|name|size`，作为签名值。
5. 空值统一编码为显式 `null`；按定义顺序、以 widget_id 为键序列化为规范 JSON（稳定排序）。
6. 未映射列、不可提单控件、明细子树内的说明/公式控件都不参与签名。

约束：

- 签名阶段**禁止任何网络副作用**（不上传、不查实例）；外部选项解析只查本地选项表。
- 签名与提交必须使用同一轮解析出的 `option_map`；处理期复核时重新解析，若选项表在两次解析间变化导致签名变化，按 §五 暂停并要求重试。
- 选项 label 在同一轮内解析为同一 option_id 由现有 `pick_option` 的确定性排序保证。

### 4.4 播种分组

入口：`seed_config`（`src/infrastructure/feishu_approval_worker.rs:1091-1230`）。仅 `aggregate_batch=true` 时走新分支，`false` 时原逻辑不变。

**聚合播种只发生在批量点击轮**：HTTP 请求未带 `record_id` 的整表按钮。唤醒信号需要能区分批量与单条（例如 `RoundTrigger::Manual { regroup: bool }`）。单条行内点击仍唤醒 worker 处理队列，但**不建组、不解散组、不重组**：避免一次行点击顺带改变其它组的成员，也避免把行点击的发起人语义扩散给别的记录。

批量点击轮，对**被点击配置**：

1. **解散可安全解散的未提交组**：候选 = `group_id` 非空、组行 `instance_code` 为空、leader 无未过期租约，且 `effect_phase = ready`、leader `state` 不是 `review`。
   - 组内行若自身有 `instance_code`（被单条点击独立提交过）或已有回填：摘除 `group_id`、`group_seq=0`，保留为单条任务行。
   - 其余成员行直接删除，组行删除。
   - `effect_phase ∈ {creating, lookup, created}` 或 `state=review` 的组**永不解散**：外部可能已经建单，只有保留组 uuid 才能靠 `60012` 回捞同一实例（§4.6 恢复分支）。
2. **已提交组的补回填**：组行 `instance_code` 非空、但任一成员的回填列为空（用户清空编号）时，重新打开 leader（`state=pending`、`effect_phase=created`）；处理期只补编号，不重复创建。
3. **重建分组**：候选 = 批量过滤条件命中（复选框 true + 回填列为空）且任务表中没有行的记录。
   - 逐条：`rekey_cells_by_field_id` → 解析外部选项 → 预检附件源标识 → `build_signature`。
   - 按签名分组；组内按扫描返回顺序排 `group_seq`，第一条为 leader。
   - 一个事务内插入组行 + N 行任务行：leader `uuid` 仍按记录派生（与单条一致），组 `uuid` 按 `group_key` 派生写组行；全员 `state=pending`、`effect_phase=ready`。
   - 签名计算失败（不支持的控件、定义非法等）：该记录单独成组（`group_key = hash(config|record_id|错误摘要)`），让处理期照常报错并暂停，不静默丢行。
4. 播种命中 0 条时的既有反馈逻辑不变。

轮询（`Poll`/`Pure`）不新建任务行，行为不变；聚合只在手动点击的播种轮生效。

### 4.5 认领

`claim_ready` 增加「只认 leader」过滤：worker 批量认领时 WHERE 加 `AND group_seq = 0`（`src/addon/feishu/domain/approval_execution/repository.rs:173-203`）；`dispatch_single` 按 `record_id` 精确认领（`only_id`）时**不加**该条件，否则组内成员无法被单条点击独立建单（决策 7）。成员行永远不进批量认领集；组通过 leader 行获得租约、generation 与 attempts。

### 4.6 处理

`process_one` 发现 `leader.group_id` 非空时走组路径，其余走现有 `dispatch_one`：

1. 读组行；按 `(config_id, group_id)` 取全部成员行并按 `group_seq` 排序。若组行 `instance_code` 非空，或 `effect_phase ∈ {creating, lookup}`：先走**恢复分支**，按 `instance_code`/组 `uuid` 取实例，跳过签名复核，直接进入取编号与回填。
2. `records/batch_get` 按 100 条分片读取全部成员记录；`absent/forbidden` 分别给出可行动错误（沿用现有文案拆分）。
3. 按记录重新解析外部选项，组内同 label 必须解析到同一 option_id；冲突判组级配置错误。
4. 逐记录预检附件；`build_group_form`：顶层 `fieldList` 每条记录一行，**已进入某行的嵌套 `fieldList` 仍只一行**（避免 N×N）；非明细控件取 leader 的值（签名保证一致）。
5. 复核签名（仅当从未尝试过创建：组行 `instance_code` 为空、leader `attempts=0` 且 `effect_phase=ready`）：任一成员的当前签名与组行 `signature_hash` 不同 → 暂停整组，错误写「组内非明细字段已变化，请重新点击批量派发」。已发生过创建尝试的组不复核签名：沿用同一组 `uuid` 重试，靠 `60012` 回捞，避免「同一批数据两个实例」。
6. `validate_connections` → 附件 `transfer`（逐记录）→ 用真实 code 重建 form → `create`，请求体 `uuid` 用组行 `uuid`。
7. `60012` → 按组 `uuid` 调 `get_instance` 回捞；成功即视为已创建。
8. 创建成功后立刻落组行 `instance_code`；`instances get` 取 `serial_number`；用 `Backfill::write_many` 按 100 条分片给**全部成员记录**写同一编号，沿用「单批全有全无 + 失败对半拆隔离毒记录」。分片之间没有跨片事务：失败后整组保持可重试，已写入的分片下次以同值覆盖；某个记录永久写不进时，组重试耗尽转 `review` 并通知，其余成员保留编号。
9. 回填成功后：组行与全部成员行 `state=backfilled`、`serial_number` 落库；回填失败保持 `effect_phase=created` 可重试。

`DispatchSnapshot` 不需要新增开关字段：分组发生在播种期，处理期按任务行/组行执行。

### 4.7 事件与扩展字段

- 事件 worker 先按 `instance_code` 查任务行（单条路径，现状）；未命中再按 `instance_code` 查组行。
- `resolve_missing_task` 的 uuid 兜底同样要查组表：事件可能在 `instance_code` 落库之前到达，`event.uuid` 就是组 `uuid`；只查任务表会把这类事件判成 `Absent` 永久丢弃。
- 命中组行：按 `(config_id, group_key)` 取成员行，向**每条成员记录**写扩展字段（状态/流程/发起/完成/部门/处理人，值与单条一致），成员行更新 `instance_status / last_synced_at`；组行不重复存运行态。

### 4.8 通知

- 每组成员共享结论时只发一条（leader 行触发）。
- 成功：编号 + 链接 + 组内条数 + 成员行号摘要（前 5 个，超出写「等 N 条」）。
- 暂停：具体失败成员的记录行号与原因 + 「修好后请重新点击批量派发」。
- 组任务**跳过**现有按 `(record_id, config_id)` 查请求日志的 `http_path_notified` 去重（批量请求日志 `record_id=NULL`，且 leader 记录可能有过单条点击日志），只用 `last_error` 变化去重，避免吞掉组通知或重复发。

## 五、状态与错误

| 场景 | 处理 | 回填列 | 自动重试 |
|---|---|---|---|
| 必填缺失 / 明细为空 / 值非法 | 整组 `paused` | 不写 | 否，等手动点击 |
| 组内非明细签名已变化 | 整组 `paused`，提示重新点击 | 不写 | 否 |
| 记录 `absent/forbidden`（Fatal） | 整组 `paused`，文案拆开 | 不写 | 否 |
| 读记录瞬时失败 / 429 / 1395001 / 5xx | 维持 `pending`，指数退避，attempts 递增 | 不写 | 是 |
| 创建被飞书明确拒绝（1390001/1390013/1390015） | 整组 `paused`（错误留库，不写记录） | 不写 | 否 |
| 回填失败 | `effect_phase=created` 重试 | 已有编号 | 是 |
| 可重试耗尽 | `review`（现状） | 不写 | 手动点击 |

- 组任务**永不把错误写进任何成员的回填列**：一旦写入，该记录会退出扫描集，而组仍缺行，无法修复。
- `paused` 不参与认领；`Poll` 轮不会自动重试。
- `reopen`（点击被点击配置）把 `paused` 一并重置为 `pending`；组身份、成员行、组行保留；未提交组在播种阶段先解散再重建（§4.4），因此不会带着过期成员重跑。

## 六、单条点击的共存规则

- 行内单条点击完全走现有同步路径，开关不影响；按 `record_id` 精确认领时不受 `group_seq=0` 过滤约束（§4.5），也不会触发聚合播种或重组（§4.4）。
- 成员行被单条点击：用该记录自己的 `uuid` 建单，组行与组 `uuid` 不受影响；组处理仍会把该记录算作明细行，因此可能产生两个实例（已确认接受）。编号回填以最后一次写入为准。
- leader 行被单条点击：同样用记录 `uuid` 建单；组的 `uuid/instance_code` 只在组行，不会互相覆盖。
- 下一次批量点击重建未提交组时：已有单条实例的成员行被摘除出组（保留为单条任务），其余成员重新分组。

## 七、播种/处理的关键不变量

1. 组内 `group_key`、成员集合、`group_seq` 一旦插入，两次点击之间不再变化。
2. 未提交组（组行 `instance_code` 为空）在下一次手动点击时解散重建；已提交组不动。
3. 组 `uuid` 只用于组实例，成员行 `uuid` 只用于单条实例；两者不交叉。
4. 组内所有成员记录回填同一编号。跨分片不保证原子：失败后整组重试并以同值覆盖；永久写不进的记录会让组进入 `review` 并通知，此时可能出现「部分成员有编号、其余没有」的可见中间态，但不会静默。
5. 明细行顺序 = 播种时的扫描顺序（`group_seq`），重试与补员不改变顺序。
6. 错误只在系统侧（`last_error`）与通知里出现，不写多维表格。

## 八、前端与契约

- 配置向导第 3 步（`frontend/src/features/feishu/components/ApprovalConfigDialog.tsx:935-940`）新增复选框「同批聚合派发」，沿用现有 Checkbox（仓库没有 Switch 组件），仅在已选批量范围列时可勾选。
- `frontend/src/features/feishu/api.ts`（create/update 映射）与 `types.ts`（`ApprovalConfigItem`）新增字段；详情页（`ApprovalConfigDetails.tsx:683-690`）展示只读行。
- 后端：`create_config` / `update_config` 输入（`deny_unknown_fields`，必须显式加字段）、`list_configs` 的 `CONFIG_ITEM_COLUMNS` / `ConfigItem` / 赋值、worker 播种 `select_fields`。
- 校验：`create` 在 `build_plan` 后用**解析后的** `batch_scope_field_id` 判断；`update` 在事务内读旧行合并「本次修改 + 库内现值」，仅当本次显式提交开关或范围列时校验；清空范围列时若聚合仍为 true，同样拒绝。
- 改完跑 `python scripts/dump_openapi.py` 并提交两个生成物；前端跑 `pnpm --dir frontend check`。
- 状态展示：`paused` 需要中文标签（任务列表/详情沿用 state 的 Radio 标题）。

## 九、测试与验证

按仓库规则，每处新增分支至少做一次摘除式变异（`docs/guides/TESTING_SILENT_FAILURES.md`）。

单元测试（`#[cfg(test)]`）：

- 签名：选项/日期/数字/包装归一；附件按源标识与顺序；明细子树全部排除；空值稳定编码。
- 分组：同签名合并、不同签名拆组、签名失败单独成组、`group_key` 含成员集合。
- 组装：顶层 `fieldList` N 行、嵌套 `fieldList` 仍 1 行、必填按行校验、空明细暂停、附件 code 按记录。
- 幂等：组 `uuid` 稳定；组内成员 uuid 独立；60012 回捞按组 uuid。
- 表声明：新增列/唯一键/状态白名单断言（对齐 `task_table.rs` 现有测试）。

集成测试（`tests/feishu_approval_dispatch_integration.rs`，真实 MySQL/Redis）：

- 两条同签名记录 → 一条组行 + 两行任务 + 一次 create、form 两行；回填两条同编号。
- 开关关闭 → 逐条实例，行为不变。
- 组内一条必填缺失 → 两条都 `paused`、回填列不变、不发 `create`、通知一条且含「重新点击」。
- 点击重开 → 未提交组解散重建；修复后一单两行成功。
- 事件 → 两条记录扩展字段都更新。
- 单条点击成员 → 独立实例；组不受影响；不触发其它未提交组的重组。
- 已提交组清空部分成员编号 → 点击只补回填、不重复建单。
- 组进入 `review` 后再次点击 → 沿用同一组 uuid，`60012` 回捞原实例。
- 事件先于 `instance_code` 落库（`event.uuid` 兜底）→ 仍能 fan-out 扩展字段。
- 超过 100 条的组：分片回填部分成功后重试收敛到全量。
- `delete_config` 清理组行与成员行；`paused` 计入活跃状态。
- 新表加入 `reset_database` 的 DROP TABLE 列表；新 `#[ignore]`/测试入口同步 `scripts/run_ci.py`。

前端：对话框 Vitest（开关默认关、未选范围列时不可勾选、create/update 载荷）、E2E 夹具补字段。

真机验证清单（上线前，技术测试定义）：

- 2 / 10 / 50 / 100 / 200 行明细创建；回读行数与内容；审批端阅读/打印表现。
- 空明细成员、嵌套明细、附件多行、外部选项多行。
- 组实例的 60012 回捞、事件推送、编号回读。

## 十、上线与兼容

- 纯增量 schema：配置列 `require+default(false)`；任务列 `group_seq require+default(0)`、`group_id` 可空；新表。已有数据的表加「必填无默认」列会被拒绝启动，本设计全部避开；无需手工 ALTER。
- 开关关闭时新分支短路；存量任务行 `group_id=NULL, group_seq=0`，处理路径不变。
- 部署只需重启应用完成加列/建表；`delete_config` 与测试夹具需同步新表。
- 回滚：关闭开关即可停止新的聚合；已建组实例照常按编号回填与事件同步（组行保留）。

## 十一、已知限制

1. 审批是整单粒度：一个明细行有问题只能整组暂停或整单退回，不能行级审批。
2. 单条点击允许独立建单，因此同一记录可能同时出现在组实例与单条实例中（产品已确认接受）。
3. 组成员集合在两次点击之间冻结；新的同签名记录只有在已提交组之后才一定另建实例。
4. 签名使用解析后的选项 id；选项表在播种与处理之间被改动会导致暂停并要求重试。
5. 官方未给出明细行数上限；不设业务上限，超大组的行为待真机验证。
6. 组实例的 `instance_code` 只在组行；控制台任务列表若要做组下钻，需要后续加 join（本期不做）。
