# 飞书审批「同批聚合派发」— 设计

- 文档性质：设计（决策记录 + 实施约束 + 验证清单）
- 撰写日期：2026-10-31
- 修订：2026-10-31 v2，按第一性原理对抗性复核重写副作用归属、播种顺序、暂停持久化、单条点击与测试设施
- 修订：2026-11-02 v3，改为「配置开关 ∧ 请求参数 `aggregate=true`」双条件生效；开关入口移到配置详情「接入数据」tab
- 需求来源：运营反馈「同批多条记录逐条建单太吵」，产品负责人明确要求实现
- 状态：待实施
- 关联：
  - `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`（派发主设计）
  - `docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md`（请求日志）
  - `docs/contracts/SCHEMA.md`（schema 增量同步规则）

## 一、目标

同批聚合需要**两把钥匙同时打开**：配置级开关 `aggregate_batch`（在控制台「接入数据」tab 维护，默认关闭）**且**本次派发请求体带 `"aggregate": true`。两者同时为真时：

1. 同一次批量勾选（同一配置、复选框列为 true、回填列为空）的记录中，**除明细控件子树与顶层公式控件外的所有已映射字段，按转换后的表单值完全一致**的记录，合成为一个审批实例。顶层公式控件（付款总金额、合计付款金额等）是整单合计，按成员值相加后提交，不参与一致性比较。
2. 明细控件内每条记录一行；组内所有记录回填同一个审批编号；每组只发一条通知。
3. 任一条件不满足时行为与现状逐字节一致；已存在的组仍由 leader 收尾，不因关开关或请求不带 `aggregate` 而卡死。

非目标：

- 不跨配置、跨批次、跨发起人聚合；不自动把未勾选的记录并入。
- 不支持明细行级审批（飞书 approve/reject 是实例粒度，无行号参数）。
- 不改非聚合配置的单条派发语义。

## 二、已确认的产品决策

| # | 决策 | 结论 |
|---|---|---|
| 1 | 开关位置与默认值 | 配置级开关，默认关闭；**入口在配置详情「接入数据」tab**（与工作流模板同屏，便于对照）；开启时必须已配置批量范围复选框列，否则保存拒绝 |
| 1b | 生效条件 | **开关 ∧ 请求参数**：批量请求体带 `"aggregate": true` 才新建组；省略等价 `false`（存量工作流不改也安全）。单条点击恒不聚合 |
| 2 | 聚合范围 | 同一次批量勾选内的记录；每次批量点击按**当时的勾选**重新分组尚未提交的组（已建实例的组不动） |
| 3 | 一致性口径 | 比较转换后的表单值：选项按解析结果、日期/数字按转换结果、`{type,value}` 包装归一；附件按源文件 `file_token+name+size`（保持源顺序）；**顶层公式控件不参与比较，按成员求和** |
| 4 | 组内校验失败 | 整组暂停、不派发、不写任何成员的回填列；飞书通知提醒「修好后重新点击」 |
| 5 | 重试方式 | 暂停组只能手动批量点击重试；轮询不自动重试 |
| 6 | 通知粒度 | 每组一条 |
| 7 | 单条点击 | 允许独立建单；点组成员时先把该记录摘出组，再按单条语义建单 |
| 8 | 组行数上限 | 不设业务上限；受批量扫描上限与飞书表单体积约束，超限必须显式报错 |
| 9 | 申请人校验 | 批量点击按**点击人**过滤扫描到的记录：「申请人」人员列明确写着别人的记录**跳过**（不建任务行、不 reopen、不参与组维护），其余照常提交；不单独发跳过通知，轮末汇总只报处理条数。空/普通文本视为未填，建单后覆盖成点击人。单条点击同规则但**受理前终态拒绝**；组派发在成员级再兜一次（整组暂停）。恢复已建组只补编号，不覆盖明确写着别人的非空申请人列 |

## 三、现状与复核结论

### 3.1 现有身份链

一条多维表格记录 = 一行 `feishu_approval_task` = 一个按 `record_id` 派生的 `uuid` = 一个实例。任务表唯一键 `(config_id, record_id)` 与 `uuid`；`uuid` 同时是重试幂等键与 `60012` 回捞键（`approval_uuid.rs`）。

### 3.2 复核确认的硬约束（决定本设计）

1. **执行副作用绑定在被认领的 leader 任务行**：`ExecutionTransport` 在 create 成功时调用 `confirm_effect(..., "created", instance_code)`，SQL 落在 `claim.id`；`finish` 也按任务行收尾并清租约。因此「组 instance_code 只落组行」不能与「直接复用 `ExecutionTransport` 默认行为」同时成立。
2. **`finish` 的阶段白名单是 `ready/created`**；`60012` 会把效果阶段置为 `lookup`。若组照抄单条路径，leader 在 `lookup` 阶段既不能 finish 也不能再次 claim，组会每轮被扫成 `review`。
3. **`reopen` 会把 `effect_phase` 重置为 `ready`**（leader 无 instance_code 时），现有字段无法表达「曾经尝试过创建」；必须由组行持久化这一事实。
4. **认领 SQL 只认 `state='pending' AND effect_phase IN ('ready','created')`**；批量认领没有组过滤，单条认领按 `only_id` 精确定位。
5. **成员顺序依赖 `group_seq` 升序**；DSL 的 `sortable` 是 fail-closed，未声明会在运行期报 `FieldPermissionDenied`。
6. **worker 的 transport 是 `Arc<HttpClientTransport>` 具体类型**，无法在真实库测试里注入回放传输；事件 worker 已经是 `Arc<dyn OutboundTransport>`。
7. **通知行号编辑 `RowNumberEdit` 只能表达单个 `record_id`**，同一条消息多次编辑是覆盖式更新。
8. **明细多行的真机证据只覆盖 2 行、子控件为 radioV2/textarea/checkboxV2/attachmentV2 且两行等长**；amount、dateInterval、number、formula、contact、department、telephone、明细内外部选项、稀疏行/不等长行均未验证。

### 3.3 结论

多行明细在飞书 API 层可行（2 行真机 `code=0`）；本设计的主要工作量在组身份、副作用归属、暂停持久化与事件路由，而不是明细数组本身。

## 四、总体设计

### 4.1 组身份与副作用归属

- 组 = 一行 `feishu_approval_group` + N 行任务行。
- **组行是组实例身份的唯一来源**：`uuid`、`instance_code`、`serial_number`、`effect_phase`。
- **leader 任务行只承载执行簿记**：`worker_id/lease_until/generation/attempts/state/effect_phase`；**不写组 `instance_code`，不写组 `serial_number`**。成员行只承载自身状态与成员序号，也不写组身份。
- 组创建/回捞/回填确认走**组专用确认函数**（§4.6），它同时把执行阶段镜像到 leader 的 `effect_phase`（认领需要），但实例码只落组行。
- 组 `uuid` = `derive_uuid(base, table, approval_code, group_key)`；组键含签名与成员集合（§4.3）。
- 成员行 `uuid` 仍按 `record_id` 派生，只服务单条点击。
- 组行 `effect_phase` 是**持久化的外部副作用标记**：`ready → creating → created / lookup`。`reopen` 只重置 leader 的阶段，不清组行的阶段；解散只允许 `ready`。

### 4.2 数据模型

配置表 `feishu_approval_config` 新增：

```
aggregate_batch => Switch::new().title("同批聚合派发").require(true).default(false)
```

任务表 `feishu_approval_task` 新增：

```
group_id  => Str::new().title("聚合组键").max_length(64).filterable(true),
group_seq => Int::new().title("组内序号").require(true).default(0).sortable(true),
```

- `group_seq=0` 且 `group_id` 非空 = leader；`>=1` = 成员；`group_id` 为空 = 普通单条任务。
- `group_seq` 必须 `sortable(true)`：`load_members` 按它排序。

新表 `feishu_approval_group`：

```
id            => Key
config_id     => Int::new().require(true).indexed(true).filterable(true)
group_key     => Str::new().require(true).max_length(64).filterable(true)
uuid          => Str::new().require(true).unique(true).max_length(64).filterable(true)
signature_hash=> Str::new().require(true).max_length(64)
effect_phase  => Str::new().require(true).max_length(32).default("ready")
instance_code => Str::new().max_length(128).filterable(true)
serial_number => Str::new().max_length(64)
created_at / updated_at
unique_named("uk_feishu_approval_group_config_key", [config_id, group_key])
```

- 组表必须接入 `FeishuContext`（新增 `approval_groups()` 访问器），并同步全部构造点。
- 任务状态枚举新增 `paused`（Radio 物理列是 VARCHAR，应用层白名单，纯增量）；同步 `reopen`、`delete_config` 活跃状态与前端标签。
- 新表必须在 `schema_anchor::tables()` 登记（表名 + `approval_groups()` 锚）；漏登记会让新仓储的 `where_eq` 站点解析不出归属，架构门禁直接红。

### 4.3 一致性签名与组键

实现：`approval_convert::build_signature` + `approval_attachment::source_identities`。

- 明细子树 = **任意深度**的 `fieldList` 及其全部后代；签名一律排除。
- **顶层公式控件**（`formula`，如付款总金额/合计付款金额）同样排除：它是按成员求和的整单合计，天然随每条记录的明细金额不同而不同；参与签名会让同批记录永远各成一组。
- 非明细、已映射控件按现有转换器产出与提交一致的片段；空值（含必填）统一编码为 `null`，不抛 `MissingRequired`；不支持的控件类型仍返回 `Err`（该记录单独成组）。
- 附件不调用 `transfer`，用源文件 `file_token|name|size`（源顺序）参与签名。
- 规范 JSON 按定义顺序、以 widget_id 为键；`signature_hash = sha256(json)[0..32]`。
- `group_key = sha256(config_id|signature_hash|排序后的成员 record_id)[0..32]`；成员集合变化即换键。
- 签名阶段禁止任何网络副作用。

### 4.4 播种与组维护

入口 `seed_config`。**组维护（维护已有组）与开关无关；开关只决定是否新建组。**

**申请人过滤先于一切写操作**（2026-11-02 追加）：批量点击按配置的申请人字段 id
解析出列名，逐条过滤扫描到的待处理记录。人员列表非空且不含点击人（或单元格是裸
`ou_` 文本）→ **跳过该条**：不建任务行、不 reopen、不参与组维护。空列/普通文本视为
未填，按匹配处理并在创建后覆盖成点击人；`{type,value}` 包装先剥壳再判定。不单独发
「跳过 N 条」通知——轮末汇总只报本轮处理了多少条。单条点击在 Action 里、`admit`/摘组
之前用同一规则**拒绝整次点击**（不留下首份快照、不先摘组），`dispatch_one` 在创建前
再兜一次；组派发在成员级再兜一次（任一成员不匹配即整组暂停，文案同样只报条数）。

批量点击（`RoundTrigger::Manual` 且 `clicked_record_id=None`）按以下顺序执行：

1. **锁配置行**（与 `admit`/`delete_config` 同一把 `SELECT ... FOR UPDATE`）。
2. **单条任务维护**（与非聚合现状一致）：对 `group_id IS NULL` 的既有任务行，按当前点击语义 `reopen`（`pending/review/terminal/unnotified/backfilled`；`backfilled` 且回填列为空时重开）。
3. **组维护**：
   - 已提交组（组行 `instance_code` 非空）且任一成员回填列为空 → `reopen` leader（leader 回到 `pending`、`effect_phase='created'`，组行阶段不动）。
   - `group.effect_phase ∈ {creating, lookup}` 或 leader `state='review'` 且组行无实例码 → `reopen` leader 保留组 uuid，走 `60012`/按 uuid 回捞。
   - `group.effect_phase='ready'`、leader 无未过期租约、且**全组成员都在本次勾选集合内** → 解散：先删 `task_applicant` 行，再删成员行与组行；组内已有自身 `instance_code` 或回填的行摘除 `group_id/group_seq`，保留为单条任务。
   - 任何成员不在本次勾选集合内的组**保持不动**（用户没重新勾选，系统不替他撤回）。
4. **新建组**（仅 `aggregate_batch=true` **且** 本次请求 `aggregate=true`）：候选 = 本次勾选 + 回填列为空 + 任务表无行的记录；在解散之后计算。逐条签名 → `plan_groups` → 一个事务插入组行 + leader/member 行。
5. 签名失败：单独成组（`group_key=hash(config|record|错误摘要)`），让处理期照常暂停报错。
6. 发起人：批量点击的点击人只落到本轮 `touched` 的行（leader 与成员）；创建结果
   未知（`creating`/`lookup`/`backfilling`）或已有实例码的行**只重开执行状态、不进
   `touched`**，身份沿用原受理人（否则恢复出的实例会记到后来者头上）。

单条点击（`clicked_record_id=Some`）不新建组、不解散组、不重组；只走单条派发与 §4.9 的摘除规则。

### 4.5 认领与恢复

- 批量认领：`only_id=None` 时加 `AND group_seq = 0`。
- 单条认领：`only_id=Some` 时不受组过滤限制。
- 组 leader 的 `effect_phase='lookup'` 允许被认领；处理期先按组 `uuid` 回捞，不直接重发 create。
- 单条任务的 `lookup` 维持现状（已知限制，不在本次改）。

### 4.6 处理、组专用确认与收尾

`process_group` 发现 leader 的 `group_id` 非空时执行：

1. 读组行与成员行（按 `group_seq` 升序）。
2. 若组行有 `instance_code`：`get_instance` 恢复，跳过创建、签名复核与成员申请人校验，
   取编号后回填。恢复**不覆盖明确写着别人的非空申请人列**：`write_extended` 只补空列
   （空列按本轮点击人补写，与单条同一产品决策），人工改过的非空归属保持原样。
3. 若 `group.effect_phase='lookup'`：按组 `uuid` `get_instance`；命中则落实例码；`1390003` 未命中则把组行与 leader 阶段重置为 `ready`，有界退避后重发 create。
4. 否则：按 100 条分片 `records/batch_get` → `rekey_cells_by_field_id` → 逐记录解析外部选项并**合并 `label→option_id` 映射**（同 label 冲突判暂停）→ 逐记录预检/搬运附件 → `build_group_form`。
5. 签名复核**仅当组行 `effect_phase='ready'`**（真正从未尝试创建）；不再用 `attempts=0` 代理。
6. 创建前把组行阶段写为 `creating`（提交）；`create` 的 `uuid` 用组行 `uuid`。
7. 成功：组专用确认写入 `group.instance_code`、`group.effect_phase='created'`，leader 阶段 `created` 但 **`instance_code` 保持 NULL**；`60012` 按组 uuid 回捞。
8. 取编号 → `backfill_in_chunks` 给全部成员写同一编号；**检查 `BackfillReport.failed`**：非空返回 `CreatedPendingBackfill` 整组重试，全量成功才 `Backfilled`。
9. 编号落库成功后，逐成员调用 `write_extended`（六个扩展字段 + 发起人列 = leader
   任务行上的受理身份，**不是**无条件本轮点击人；仅旧行缺身份时由本轮点击人补上），
   与单条路径行为一致。
10. 收尾走**组专用 finish**：一个事务内更新 leader（state/last_error/attempts/available_at/清租约，允许 `ready/creating/lookup/created` 阶段）、成员行 state、组行 `serial_number/effect_phase`。**不再经过通用 `outcome_record`**，避免 `Waiting→pending` 把暂停组重新拉回轮询；组路径自行完成 `is_owned` 复核、成功审计、派发指标与 `already_initiated` 判定。
11. 组分支位于 missing-initiator 早退之前；组缺失发起人时落组级 `paused`，不走单条 `Waiting`。
12. 认领前的过期租约清扫必须排除组 leader 的 `lookup` 阶段，否则会在认领前先被扫成 `review`。

组失败分类独立于单条 `classify_failure`：

- 转换/必填/空明细/选项冲突/记录 `absent|forbidden`/飞书明确拒绝（1390001/1390013/1390015）→ 整组 `paused`，原因只进 `last_error` 与通知，**绝不调用 `backfill.write`**。
- 429/1395001/5xx/读记录瞬时失败 → 可重试，leader 保持 `pending`，按现有退避与 attempts 预算，耗尽转 `review`。

### 4.7 事件

- 查询顺序：**先组表、再任务表**。组实例的事件必然有组行，避免 leader 行（可能存过单条实例码）截获。
- `instance_code` 未命中组表 → 任务表（单条）。
- uuid 兜底：先查组表 `uuid`，再查任务表 `uuid`；`MissingTaskResolution` 新增 `FoundGroup { config_id, group_key }`。
- 命中组：取成员行，逐成员构造 `DispatchInput` 调 `write_extended`（顶层控件值相同），更新成员行 `instance_status/last_synced_at`。

### 4.8 通知

- 每组一条；发送前一次性解析成员行号（按表批量取行号，20 页上限内），**不使用逐成员 `RowNumberEdit`**。
- 成功：编号 + 链接 + 组内条数 + 成员行号摘要（前 5 个，超出「等 N 条」）。
- 暂停：失败成员行号 + 原因 + 「修好后请重新点击批量派发」。
- 批量点击的申请人过滤**不发**单独的「跳过 N 条」消息；轮末汇总的「本轮命中 N 条」
  就是本次实际处理的条数。
- 组任务跳过按 `(record_id, config_id)` 查请求日志的 `http_path_notified` 去重，只用 `last_error` 变化去重。

### 4.9 单条点击的共存

- 非组任务行：现状不变。
- 组行（leader 或成员）被单条点击：
  1. 在事务内锁配置行；若组内任一行有未过期租约，返回可重试提示。
  2. 若组行无 `instance_code`：把该行摘出组（清 `group_id/group_seq`）；若它是 leader，把剩余最小 `group_seq` 的成员提升为 leader；若组已无成员，删除组行。
  3. 若组行已有 `instance_code`：同样摘出并提升 leader；组实例继续由新 leader 收尾。
  4. 随后按单条语义建单；本次点击显式带 `force_solo`，跳过「已回填且编号非空即 Queued」的短路，允许同一记录出现第二个实例（决策 7）。编号回填以最后一次写入为准。
- 已提交组的成员被摘出后不再收到组事件，只收到自己的单条事件。

## 五、状态与错误

| 场景 | 处理 | 回填列 | 自动重试 |
|---|---|---|---|
| 必填缺失 / 明细为空 / 值非法 / 选项冲突 | 整组 `paused` | 不写 | 否，等手动批量点击 |
| 批量扫描命中「申请人」不是点击人 | 跳过该条（不播种、不入组、不参与组维护），其余照常提交；轮末汇总只报处理条数 | 不写 | 否；如需提交由本人点击 |
| 组内非明细签名已变化（组行 `ready`） | 整组 `paused`，提示重新点击 | 不写 | 否 |
| 记录 `absent/forbidden`（Fatal） | 整组 `paused`，文案拆开；若组行**已有实例码**，把该成员行置 `terminal` 并摘出组，其余成员继续回填，无剩余成员则组收尾 | 不写该成员 | 否 |
| 读记录瞬时失败 / 429 / 1395001 / 5xx | 维持 `pending`，退避，attempts 递增 | 不写 | 是 |
| 飞书明确拒绝（1390001/1390013/1390015） | 整组 `paused` | 不写 | 否 |
| 回填部分失败 | `effect_phase=created`，整组重试 | 可能部分已写 | 是 |
| 可重试耗尽 | `review`，点击可恢复（保留组 uuid） | 不写 | 手动 |

- `paused` 由组专用 finish 写 leader 与全体成员；不参与认领，轮询零出站。
- `reopen` 把 `paused` 重置为 `pending`；组行 `effect_phase` 不被重置。
- 组任务永不把错误写进任何成员的回填列。

## 六、并发与崩溃窗口

1. 解散、摘除、建组都在持有配置行锁的事务内完成；删除条件带 `state`/`worker_id`/`effect_phase`。
2. 删除任务行前先删 `feishu_approval_task_applicant`（与 `delete_config` 同一口径，避免孤儿 PII）。
3. 创建前先提交组行 `creating`；崩溃后按组 uuid 回捞，不重复建单。
4. 60012 后 `lookup` 未命中按有界退避回 `ready`，不落 `review` 死路。
5. 组 finish 与单条 finish 互斥地清租约；组处理不再调用通用 outcome。

## 七、关键不变量

1. 组 `uuid/instance_code/serial_number` 只存在于组行；leader/成员任务行不写组实例码。
2. 两次批量点击之间组成员不变；批量点击只解散「全组成员都在本次勾选内、`effect_phase=ready`、无租约」的组。
3. 已尝试创建的组永不解散；reopen 只重置 leader 阶段，组行阶段保留。
4. 组内所有成员回填同一编号；跨分片失败整组重试，毒记录最终 `review` 并通知，不静默。
5. 明细行顺序 = 播种扫描顺序（`group_seq`）；顶层 `fieldList` 每条成员一行，进入某行后的嵌套 `fieldList` 只接收该行的数据。顶层公式控件按成员值相加后提交（必填公式任一成员缺合计、可选公式部分成员缺合计 → 整组暂停，禁止提交部分合计；可选公式全员为空 → 省略该控件）。
6. 组内任一成员明细为空 → 整组暂停，不生成缺行表单。
7. `paused` 不参与认领；轮询不自动重试。

## 八、前端与契约

- 开关入口在配置详情「接入数据」tab（与工作流模板同屏）：可写用户直接勾选/取消，走 `update_config { aggregate_batch }`；没有批量范围列时禁用并提示先去编辑向导第 3 步选列。建配置向导**不再**出现该开关（保留范围列选择）。
- 批量请求体模板随开关渲染 `aggregate: true/false`，并说明「请求不带 aggregate 时仍按逐条派发」。
- `api.ts`/`types.ts`/`ApprovalConfigDetails.tsx` 同步字段；`dump_openapi.py` 重新生成契约（配置侧 schema 变，派发端点不在 OpenAPI 内）。
- 任务列表 `ApprovalRequestsPage.tsx` 的 `TASK_STATE_LABELS` 增加 `paused: "已暂停"`（前端不读 Radio 标题）。
- 页面 Vitest：接入数据 tab 默认关 + 勾选发 `update_config`、无范围列时禁用；请求体模板含 `aggregate`。

## 九、测试与验证

前置设施：把 `RoundRunner.transport` 改为 `Arc<dyn OutboundTransport>`（照事件 worker），使真实库测试可注入脚本传输。

单元：
- 签名：明细子树（含 workGroup 内嵌 fieldList）全部排除；顶层公式控件排除（合计不同不拆组）；数值/包装归一；空必填编码 null；附件源标识与顺序；组键含成员集合；组 uuid 36 字符。
- 多行表单：顶层 fieldList N 行；空明细成员返回错误；嵌套 fieldList 用当前行数据；非明细取 leader（顶层公式控件按成员求和、缺任一份即报错，明细行内公式仍逐行）；选项并集；两行文案 9.1330/9.133 命中同一 id。
- 组派发：一次 create 两行；60012 按组 uuid 回捞；组确认不写 leader instance_code；回填失败返回 CreatedPendingBackfill；Fatal 时 `RecordingBackfill.writes()` 为空；组成功后成员扩展列与发起人列已写。
- 认领：批量跳过成员；only_id 命中成员；`paused` 零认领；`lookup` leader 可恢复。
- 事件：组表优先；uuid 兜底；任务行命中但 `group_id` 非空时仍 fan-out。
- 通知：一次渲染、多个成员行号、只调用一次 edit/发送。

真实库 `#[ignore]`（全部登记 `scripts/run_ci.py` 的 INTEGRATION）：
- 播种：同签名合并、不同签名拆组、开关关闭不建组、单条点击不建组、一次点击完成解散+重建、勾选外的组不动、review 组点击恢复、已提交组清空编号补回填。
- 组处理：两成员一单两行 + 同编号回填 + 扩展列；暂停后轮询零认领；60012→NotFound→下一轮命中；100+ 成员分片回填部分失败后重试收敛；成员记录被删的 paused 退出路径。
- 单条共存：点 leader/成员后建单、组由新 leader 收尾；已提交组成员 `force_solo` 产生第二实例。
- delete_config：paused 计入活跃；删除时清组行、成员行与 applicant 行。

真机验证（**开启开关之前**执行，阻塞上线）：
- 2/10/50/100/200 行；空明细、行内缺可选子控件、行子控件不等长。
- 明细内 amount/dateInterval/number/formula/contact/department/telephone/外部选项/附件。
- 组实例 60012 回捞、事件推送、审批端阅读/打印。

## 十、上线与兼容

- 纯增量 schema：`aggregate_batch require+default(false)`、`group_seq require+default(0)+sortable`、`group_id` 可空、组表新表含 `effect_phase default("ready")`；无需手工 ALTER。
- 开关关闭（或批量请求不带 `aggregate`）：不新建组；已有组继续由 leader 收尾（组维护与两者无关）；下一次批量点击会把「全组在本次勾选内、`effect_phase=ready`、无租约」的未提交组解散，随后按逐条派发（回滚），已提交组不动。非组任务行行为不变。
- `delete_config`：未提交的暂停组（组行无 `instance_code`）不再阻塞删除，随配置一起清理；已建实例的暂停组仍算在途，必须先通过重试/人工核查收尾，避免留下孤儿审批。
- 回滚：关开关（或把工作流模板里的 `aggregate` 改回 `false`/删掉）即停止新增聚合；已建组实例照常回填、事件 fan-out 与人工重试。两个开关任一为假都能回滚，互不依赖。
- 部署只需重启应用完成加列/建表。
- **签名口径升级（2026-11-10：顶层公式控件改为排除并按成员求和）**会改变 `signature_hash` → `group_key` → `uuid`。处理期在「新摘要全员一致但与组行存的旧摘要不等」时会按旧口径（`build_legacy_signature`）复核一次，因此升级瞬间仍为 `ready` 的旧组不会被误暂停；但批量点击解散旧组后会用新 uuid 重新播种，若存在 §十一.9 的「create 实际成功但组行被复位 `ready`」旧组，可能重复创建——上线前应先排空这类旧组，或按旧 uuid 人工回捞。

## 十一、已知限制

1. 审批是整单粒度，明细行不能单独审批；驳回退整组。
2. 单条点击会摘组成员并强制独立建单，可能产生两个实例；编号以最后写入为准。
3. 已提交组的成员被摘出后不再收到组事件。
4. 批量扫描每轮上限 20 页 × 500 = 10,000 行；超过即该配置播种失败并给出明确告警，不做静默拆分。
5. 飞书表单体积/行数上限未在文档中给出；只验证过 2 行，真机验证前不得在生产开启。
6. 多行证据只覆盖 radioV2/textarea/checkboxV2/attachmentV2 等长子控件；其余控件组合未验证。
7. 单条任务的 `lookup` 阶段死路维持现状，不在本次修复。
8. 组实例的 `instance_code` 只落组行；控制台任务列表若要做组下钻需后续加 join。
9. **组身份冻结不是持久的**：`60012` 后 `1390003`（实例尚未可查）会把组行复位成
   `ready`，此间批量点击可能把已建实例的组当成「从未尝试创建」而解散/改写 leader
   身份；实例本身仍由 uuid 幂等归属首次创建人。根治需持久 `create_attempted` 标记
   （见派发主设计 §11 限制 9），本次不修。
