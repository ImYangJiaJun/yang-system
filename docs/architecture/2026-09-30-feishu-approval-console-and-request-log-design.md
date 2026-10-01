# 审批派发控制台与派发请求记录 — 设计

**2026-09-30 · 承接 `2026-09-28-feishu-approval-dispatch-design.md` 未排期的 Task 13，并新增「派发请求逐次落库记录」需求**

> 本文所有「已实测 / 已核验」结论均以代码行号或实测记录为证。链路背景（多维表格按钮 →
> 工作流 HTTP → `POST /api/v1/feishu/approval/dispatch` → 创建审批实例 → 回填编号）见
> 原设计文档。

## 一、目标

1. **Task 13a**：`feishu.approval` 模块的控制台后端——配置的创建 / 更新 / 删除 / 列表、
   审批定义控件预览、派发记录与任务列表，7 个受保护 Action 进 OpenAPI 契约。
2. **Task 13b**：前端两个页面——审批派发配置（含建配置向导）与派发记录（含逐条任务下钻）。
3. **新需求（用户追加，非原计划）**：**每次打到派发端点的请求落一行记录**（一次请求一记，
   批量与单条都记），字段为请求人、请求数据、请求结果、返回结果。

## 二、决策记录

### 2.1 用户已确认的三条

| 问题 | 决策 | 含义 |
|---|---|---|
| 记录粒度 | **入口请求一记** | 每次 `POST /dispatch` 一行。单条同步请求记最终结果（编号或失败原因）；批量请求记「已受理」。逐条的处理过程与结果由既有 `feishu_approval_task` 承载，控制台记录页可下钻查看 |
| 请求人来源 | **请求体加 `requested_by` 字段** | 工作流模板在 `raw_body` 里带上触发人 `$.step_btn.user`（行内与页面按钮的触发器输出均有 `user`）。未带的旧工作流不破坏，落库记 `feishu-workflow` |
| 查询页面 | **一起做** | Task 13b 含配置管理与派发记录两个页面 |
| 13a 范围 | **完整含建配置向导** | 手动建配置复用 provision 校验链路，控制台可完全不依赖工作流首调建配置 |

### 2.2 设计自审（第一性原理逐条核验）修正 —— 6 处 REFUTED 的处置

对 34 条技术断言做了并行核验（7 维度 × 各 4-5 条），以下为推翻或修正设计的结论：

1. **worker 认领不写 `creating`/`created` 状态**（`feishu_approval_worker.rs:578` 只按
   `state=pending` 认领、`record_outcome:810-815` 只在处理结束时写回
   backfilled/pending/terminal）。设计与 `task_table.rs:10-20` 注释描述的状态机
   **是文档意图，不是代码现状**。
   → **删除守卫设计作废**：delete_config 不需要禁止删除。worker 对配置缺失本就优雅：
   `load_task:640-647` 读不到启用配置即返回 None，认领侧 `:598-604` 把任务标 terminal
   「所属配置不存在或已停用」，不重试、不坏数据。删除动作改为**同事务清理该配置的
   `state=pending` 任务行**（在途行此时必然仍为 pending——状态只在结束时写回），
   backfilled/terminal 行保留作流水。
2. **配置读取只发生在认领时（快照）**：`process_one`（`:702-797`）只消费 `ClaimedTask`
   快照，处理中删配置无任何竞态路径。佐证 1 的结论。
3. **field_map 表零唯一约束**（`field_map_table.rs:35-74` 只有 `config_id` indexed）。
   → 映射行只在建配置时由 `insert_plan` 整写一次，无独立 upsert 路径；重复写入由配置表
   复合唯一索引 `uk_feishu_approval_config_coords`（`table.rs:106-112`）挡住。本设计
   **不带映射编辑**（可编辑 = 删配置重建，uuid 幂等 + 60012 回捞兜底，见 §7）。
4. **insert 路径的唯一冲突绕过友好分类**：`yang_db` 按 SQLSTATE 23000 映射为
   `ConstraintError`，但 `insert_plan` 的 `insert` 路径用 `map_err(BaseError::DatabaseExecuteFailed)`
   直包（`write.rs:93/:146`），不吃 `error/mod.rs:437-454` 的「唯一冲突→ParamInvalid」。
   → `create_config` 必须在插入层 `match DatabaseExecuteFailed(DbError::ConstraintError(_))`
   转成可行动文案「该多维表格已配置，请先查询或删除后重建」。仓库先例 4 处
   （`change_username.rs:72-77`、`register.rs:85`、`change_email.rs:80`、`add_group_member.rs:296`）。
   注意 `ConstraintError` 同时覆盖唯一键(1062)与外键(1452)的混叠——`feishu_approval_config`
   无外键列，风险可控。
5. **FeishuContext::new 调用点共 4 处**（不只 2 处）：`feishu/mod.rs:72`（生产装配）+
   测试 3 处 `datasource/mod.rs:130`、`approval/actions/mod.rs:89`、`dispatch.rs:913`。
   → 新增第 7 个 Repository 需同步 4 处。
6. **CI 没有契约快照最新性检查**（`run_ci.py` 不调 dump_openapi；唯一相关测试
   `openapi-contract.test.ts` 只做前端可编译性静态断言）。契约同步全手动。
   → 实施计划把「重新生成契约 + 提交快照」设为显式任务步骤。同时修正认知：**public
   Action 同样进契约**（`login` 在 `openapi.json:7279`）；dispatch 缺席只因注册被
   `can_pull()` 门禁（`approval/actions/mod.rs:39-48`），`openapi-dump` 用的元数据 app
   无飞书凭证。本设计的 7 个控制台 Action **无条件注册**，故必然进契约。

### 2.3 额外采纳

- **新增第 7 个 Action `list_tasks`**：批量派发记录行只有「已受理」，逐条成败在
  `feishu_approval_task`；控制台需要看到（§5.3）。
- 保留原设计 §12 之后追加的待实测项：**M11** `$.step_btn.user` 形态（纯字符串或对象，
  对象则取子字段；真机联调时确认）。

## 三、数据模型：新表 `feishu_approval_request_log`

第四张审批族表。**一次入口请求一行**——这是「请求记录」表的全部语义，与
`feishu_approval_task`（逐条处理状态机）分工明确。

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | Key, filterable, sortable | |
| `requested_by` | Str(128), filterable | 请求人：请求体 `requested_by`，未带落 `feishu-workflow` |
| `base_token` | Str(128), require, filterable | 冗余存坐标——配置未建成（40401/35600）的失败请求也要能记、能按表筛 |
| `table_id` | Str(128), require, filterable | 同上 |
| `config_id` | Int, indexed, 可空 | 配置存在时关联；首调失败时为空 |
| `record_id` | Str(128), 可空 | 单条请求的记录 id，关联 task 行 |
| `request_body` | Text, require | 请求数据：`DispatchInput` 序列化（坐标/三件套/请求人） |
| `outcome` | Radio<String>: `succeeded` / `waiting` / `accepted` / `failed`, require, filterable | **请求结果**。succeeded=单条创建成功；waiting=单条必填缺失、本轮未处理（等待态）；accepted=批量受理；failed=失败（含参数校验/配置/凭证/业务失败） |
| `message` | Str, require | 结果说明（复用响应 message；失败时为可行动原因） |
| `serial_number` | Str(64), 可空 | 单条成功时的审批单编号，独立成列便于筛选 |
| `response_body` | Text, 可空 | **返回结果**：实际返回的 `data` JSON 原文 |
| `created_at` | Timestamp, created_at, sortable | |

- **module 归属**：纯绑定表 module `feishu.approval_request_log`（照抄
  `approval::build_task_module` 模式，无 Action）。「一张表 = 一个 module」是框架硬形状。
- **写入点**：`dispatch.rs::handle` 的**每一个出口**（含 `validate()` 失败——在返回
  `Err` 之前主动写）。落库用独立连接（`append_independent` 同构），**写失败仅降级
  tracing::error，绝不影响派发结果**——派发已提交到 task 表与飞书，不能被日志拖死。
- **边界**：管理 Token 鉴权失败在中间件、进不了 handler，不落此表（属接入层日志）；
  serde 反序列化失败的 400 同理。本表记「通过鉴权的业务请求」。
- **schema_anchor 同步**：`domain/schema_anchor.rs:45-100` 的 `tables()` 硬编码注册表
  必须登记新表（现 6 张 feishu 表，注释「三张表」已过时，顺带修正），否则 dispatch 的
  写站点落「宽松档」且 `the_weakly_checked_sites_are_exactly_the_recorded_ones`
  （`:811-850`）会钉红。
- **不设清理**：表增长 = 按钮点击数，量级极小；与 task 表一致，后续需要归档另行排期。

## 四、派发端点改造

### 4.1 请求体加 `requested_by`

```jsonc
{ "base_token": "...", "table_id": "...", "record_id": "...",     // 结构不变
  "requested_by": "张三" }                                        // 新增，可缺省
```

- `DispatchInput` 加 `#[serde(default)] requested_by: Option<String>`（trim 非空校验，
  ≤128 字符）。`deny_unknown_fields` 下旧工作流报文不受影响。
- 工作流模板（原设计 §4.1）更新：`raw_body` 增加 `"requested_by":"$.step_btn.user"`
  段。M11 未实测前先按原样引用，联调确认形态（09-28 设计 §12 的 M10 已被 isEmpty 占用，故顺延 M11）。
- 测试夹具：`valid_input`（`dispatch.rs:585-594`）补字段；`first_call_input` 用
  `..valid_input()` 结构体更新语法自动继承。
- **注意**：`DispatchInput` 目前无 `Serialize`，`request_body` 落库需要补 derive。

### 4.2 逐出口落记录

`handle()` 内所有返回路径（含 `Err(BaseError)` 冒泡出口）统一落一条：
`succeeded`（单条 Backfilled）/ `waiting`（Waiting）/ `accepted`（批量受理、
单条 Retryable 失败不算受理，归 failed）/ `failed`（40401 / 35600 / 50301 /
Terminal / Retryable / validate 失败 / 内部 Err）。`dispatch_single` 内部另有
50001/50002/50003/50201/50004/40402 等业务失败出口，一并归类 failed。

实现形态建议：把 `handle` 的业务主体抽成内部函数返回结构化 outcome，外层统一
组装响应并落记录（一处收口，避免逐个出口复制）。

## 五、控制台后端：7 个 Action

**注册位置**：全部**无条件注册**（在 `register_all` 的 `can_pull()` 门禁**之前**，
与 dispatch 相反——控制台不依赖飞书凭证存在；`create_config` 与 `list_widgets`
运行时按其自身逻辑校验凭证并返回 50301）。中间件不挂管理 Token（控制台走 JWT）。

**权限**：`feishu.approval.read` / `feishu.approval.write`——经 `.permissions([...])`
声明即自动投影进权限目录（`permission_catalog.rs:52-86`），无登记环节；**不是管理员
等价权限**（不签发凭据、不回显秘密），`sensitive_permissions.rs` 清单不动。

### 5.1 配置 CRUD（除 `list_configs` 为 read 外，写操作均为 `feishu.approval.write`）

| Action | 路由 | 说明 |
|---|---|---|
| `list_configs` | `POST /api/v1/feishu/approval/configs/query` | `feishu.approval.read`；分页用共享 `ListInput`（与通用 TableView 同构）；映射按 config 分组带出（一次 `where_in` 防 N+1，照 `list_datasources`）；**不投影 `form_snapshot` 大字段** |
| `create_config` | `POST /api/v1/feishu/approval/configs/create` | `feishu.approval.write`；**复用 `approval_provision::build_plan + insert_plan` 整条校验链**——手动建与首调自动建同一套保证；入参 `base_token/table_id/approval_code/applicant_field/backfill_field/base_timezone`（与 `DispatchInput` 三件套同语义，标题由 approval_name 自动生成）；在 `insert_plan` 层 match `ConstraintError` → 「该多维表格已配置，请先查询或删除后重建」（§2.2-4） |
| `update_config` | `POST /api/v1/feishu/approval/configs/update` | `feishu.approval.write`；仅 `title` / `enabled` / `base_timezone` 三项。坐标与三件套不可改（改 = 删了重建，见 §7） |
| `delete_config` | `POST /api/v1/feishu/approval/configs/delete` | `feishu.approval.write`；同事务删配置行 + field_map 行 + **`state=pending` 的 task 行**（backfilled/terminal 保留作流水）。无需未完结守卫（§2.2-1/2） |

三个写 Action 在**同一事务**里落 `audit_event`：`audit::succeeded_event`（user actor，
取自 `ctx.actor()?.user_id()`——与 delete_options 用 system 变体不同，控制台是登录用户），
`append_in_tx` 业务事务内原子提交（`AUDIT.md` 契约：审计失败整体回滚）。Succeeded
事件必须有 after_summary（如 config_id / outcome）。`docs/contracts/AUDIT.md` 的
「必须覆盖的高权限变化」一节按事件逐个登记，**三个新审计 Action 必须手工补进该节**
（`AUDIT.md:31-41` 是显式清单，无自动机制）。

### 5.2 控件预览与记录查询

| Action | 路由 | 权限 | 说明 |
|---|---|---|---|
| `list_widgets` | `POST /api/v1/feishu/approval/definitions/widgets` | `feishu.approval.write` | 按 `approval_code` 调 `approvals get`，`parse_form` 解析后返回控件列表（id/名称/类型/required）。归 write 侧与 `list_bitable_*` 同理：只读语义但出站耗配额 |
| `list_requests` | `POST /api/v1/feishu/approval/requests/query` | `feishu.approval.read` | 派发记录分页；`request_body`/`response_body` 随行返回（体量小），前端本地展开 |
| `list_tasks` | `POST /api/v1/feishu/approval/tasks/query` | `feishu.approval.read` | task 分页（按 config_id/state 过滤，options：pending/creating/created/backfilled/terminal），批量记录的下钻视图 |

### 5.3 前端

- **路由**（`routes.tsx` lazy + RouteFallback，照 datasources 模式）：
  `/feishu/approval/configs`（配置页）、`/feishu/approval/requests`（记录页）。
- **侧边栏**（`AppLayout.tsx`，「飞书集成」组，照 `canReadFeishuDatasources` 模式）：
  「审批派发」与「派发记录」两个入口，门控 = Catalog 中存在
  `feishu.approval.list_configs` / `list_requests`。
- **配置页**：列表（配置名/坐标/审批 Code/时区/启用开关/快照时间/可行动状态）；
  **建配置向导**（四步：选多维表格坐标（复用 `bitable-tables/views/fields` 端点）→
  填审批 Code + 控件预览（`list_widgets`）→ 选申请人/回填列 + 时区 → 提交，
  服务端一次报全校验原因）；行操作：启停/删除（ConfirmDialog）/映射明细（展开）。
- **记录页**：列表（时间/请求人/坐标/record_id/outcome 色标/信封消息），行内展开
  看请求体与返回体 JSON 原文；批量行可下钻该配置的 `list_tasks`。
- **向导权限提示**：`list_bitable_tables/fields` 声明 `feishu.datasource.write`——
  运营审批派发控制台的账号需同时持有 `feishu.approval.write` 与
  `feishu.datasource.write`（或为系统管理员）。页面在工具提示中说明。
- 契约：`python scripts/dump_openapi.py` 再生成 `openapi.json` 与 `api-types.ts`
  （`pnpm gen:contracts`），提交快照。**CI 无最新性检查（§2.2-6），任务步骤显式列出**。

## 六、文档同步（Task 14 售后）

- `AGENTS.md`：新表、新 module、7 个 Action、`requested_by`、权限键、向导权限说明；
  顺带修正已过时的「三张表」类措辞（agent 核验发现 `datasource/mod.rs` 与
  `schema_anchor.rs:42/:832` 有陈旧注释「三张表」，实际 6 张——只改涉及本次的，
  其余「找茬注释」不顺手清理，遵守精准局部修改）。
- `docs/contracts/AUDIT.md`：登记 3 个新审计事件（create/update/delete config）。
- 原设计文档 `2026-09-28-feishu-approval-dispatch-design.md`：§4.1 模板补
  `requested_by`、补「请求记录」一节与 M11、M 表同步；实施计划文档顶部进度表
  标记 Task 13 完成与本次承接。

## 七、非目标与既有语义（写明，避免实现期漂移）

- **不做手动逐控件编辑映射**。理由：① 使用方契约=列名与控件名严格对应
  （原设计 §6.1），按名匹配是唯一推导源；② 手动映射编辑器会成为第二条写路径，
  与 provision 的校验不变式（converter、固定选项 option_map、必填覆盖）分叉；
  ③ 改映射 = 删配置 → 向导重建：uuid 幂等 + 60012 回捞保证不重复建单、
  已 backfilled 记录重播种后安全（该链路 mock 实测与真机实测均通过）。将来要
  逐控件指定异名列，加一个 Action 即可，不堵路。
- **不设请求记录清理**（§3）。**不做任务级重放/撤回**（沿用原设计 §13 回滚边界）。
- **遵循既有既成事实**：worker 实际状态机只有 pending→(backfilled/pending/terminal)
  三种写回（creating/created 是注释理想）；`load_task` 对「无字段映射」也返回 None，
  与 `:666-669` 注释「应回 pending 等待态」的语义不符——既有行为，本次不修，只在本
  设计记录该差异，避免未来把「配置缺失」与「映射缺失」混为一谈时误伤。

## 八、验证清单

后端：表声明单测（照 task_table 六条形态）；各 Action 入参校验单测；
`create_config` 的 ConstraintError 映射单测；`dispatch` 落记录的全出口集成测试
（真实 MySQL，含 validate 失败、单条成功/等待/失败、批量受理、provision 失败）；
`delete_config` 清 pending 任务与保留历史行的单测。
前端：两页 view 测试（照 datasources 页测试形态）+ 向导分步测试 + 契约对账。
门禁：每任务 `python scripts/run_ci.py quick`；新表与写站点过 schema_anchor；
领先 `frontend/tests/engine/contracts/openapi-contract.test.ts` 的静态编译。