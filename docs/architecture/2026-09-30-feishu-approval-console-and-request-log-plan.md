# 审批派发控制台与派发请求记录 实施计划

> 承接 `2026-09-28-feishu-approval-dispatch-plan.md` 的 Task 13（其范围缺口分析见该计划
> 「Task 13 的范围缺口」小节），并新增用户追加需求「派发请求逐次落库记录」。
> 设计依据：`docs/architecture/2026-09-30-feishu-approval-console-and-request-log-design.md`
> （下称「本设计」）。设计已做第一性原理逐条核验（34 条断言，6 条 REFUTED 的处置见
> 设计 §2.2），本计划的任务步骤与核验发现一一对应。

> **分支**：本次全部改动落在新分支 `feat/feishu-approval-console`（从 `main` 切出；
> 工作树多会话共用，动手前 `git branch --show-current` 确认，见记忆
> `check-branch-not-just-status`）。最终合入 main 时走 review。

## 执行进度

| 任务 | 状态 | 提交 |
|---|---|---|
| Task A 新表 + Context 接线 + schema_anchor | ☐ | |
| Task B dispatch 落记录 + requested_by | ☐ | |
| Task C 七个控制台 Action + 注册表重构 | ☐ | |
| Task D 前端：配置页 + 向导 + 记录页 | ☐ | |
| Task E 契约再生成 | ☐ | |
| Task F 文档同步 | ☐ | |

## 落库时序与全局约束（新增项，其余沿用原计划）

1. **请求记录落库不参与派发事务**（本设计 §3）：独立连接 `append_independent` 同构
   **自动提交**，写失败降级 `tracing::error!`，绝不影响派发结果；不挂管理 Token 中间件。
2. **新 7 个控制台 Action 无条件注册**（本设计 §5）：`can_pull()` 门禁之前；
   保持 `dispatch` 及其 `ManagementTokenMiddleware` 在门禁之后——重构 `register_all`
   时不得把 dispatch 从门槛后挪出来。
3. **权限**：新键 `feishu.approval.read` / `feishu.approval.write`，经 `.permissions()`
   声明即自动投影目录；**不进** `sensitive_permissions.rs` 管理员等价清单。
4. **写 Action 的审计**：`audit::succeeded_event`（user actor，与 delete_options 的
   system 变体不同）+ `append_in_tx` 与业务同事务；Succeeded 事件必须带 after_summary。
5. **create_config 唯一冲突**：在 `insert_plan` 调用处 `match
   BaseError::DatabaseExecuteFailed(DbError::ConstraintError(_))` → 可行动文案
   （本设计 §2.2-4；仓库先例 `change_username.rs:72-77`）。
6. **契约再生成是显式步骤**（CI 无最新性检查，本设计 §2.2-6）：Task E 单独成任务，
   后端完成后立即执行；生成物提交入库。
7. **集成测试登记**：`scripts/run_ci.py` 的 INTEGRATION 命令清单按名登记二进制，
   新增测试入口需同步加一行（run_ci.py:272-311 是 feishu 各入口的登记处）。

---

## Task A：新表 `feishu_approval_request_log` + Context 接线 + schema_anchor

**Files:**
- Create: `src/addon/feishu/approval/domain/request_log_table.rs`
- Modify: `src/addon/feishu/approval/mod.rs`（`build_request_log_module()` + module 名函数）
- Modify: `src/addon/feishu/mod.rs`（挂新 module + `build_context` 第 7 个 Repository）
- Modify: `src/addon/feishu/domain/context.rs`（新字段 `approval_request_log: Repository` + 访问器 `approval_request_logs()`）
- Modify: `src/addon/feishu/domain/schema_anchor.rs`（`tables()` 注册新表；顺带修正「三张表」陈旧注释）
- Modify: 测试调用点 3 处：`src/addon/feishu/datasource/mod.rs`（[cfg(test)]）、
  `src/addon/feishu/approval/actions/mod.rs`（测试）、`src/addon/feishu/approval/actions/dispatch.rs`（e2e_context）

> **核验发现（本设计 §2.2-5）**：`FeishuContext::new` 共 4 处调用点
> （mod.rs:72 生产 + 3 处测试），加参数要一次性同步完，编译会兜底。

- [ ] **Step 1: 表声明**——照 `task_table.rs` 形态：`TableSpec` + `fields!`，
      字段与 filterable/sortable/require/default 完全按本设计 §3 的表定义；
      不声明外键（沿用 config 表既有纪律）。
- [ ] **Step 2: module 归位**——`build_request_log_module()`（纯绑定表、无 Action，
      照 `build_task_module()`）。
- [ ] **Step 3: Context**——第 7 个 Repository + 访问器 `approval_request_logs()`；
      同步 4 处调用点（生产 1 + 测试 3，见上）。
- [ ] **Step 4: schema_anchor**——`tables()` 硬编码注册表登记新表（否则 dispatch
      写站点落宽松档、被 `the_weakly_checked_sites_are_exactly_the_recorded_ones`
      测试钉红）。
- [ ] **Step 5: 表声明单测**——照 task_table 六条形态（表名/主键/必需列/枚举选项/
      默认值/标签非退化）。
- [ ] **Step 6: 门禁**——`python scripts/run_ci.py quick`。

> 验证标准：quick 全绿，schema_anchor 测试通过（新表在强档），4 处调用点编译通过。

---

## Task B：dispatch 落记录 + requested_by

**Files:**
- Modify: `src/addon/feishu/approval/actions/dispatch.rs`
- Create: `src/addon/feishu/approval/domain/request_log_writer.rs`（纯逻辑：outcome 归类 + 组装 Record；可单测）

> 注：`frontend/contracts/feishu-projections.json` 是 datasource 列表投影的专项对账
> 文件，与审批族无关，本任务不触碰。

- [ ] **Step 1: `DispatchInput` 加 `requested_by: Option<String>`**（`#[serde(default)]`；
      trim 后空串按缺失处理；≤128 字符；同时补 `#[derive(Serialize)]`——request_body 落库需要）。
- [ ] **Step 2: 测试夹具**——`valid_input` 补字段；`first_call_input` 用
      `..valid_input()` 自动继承（核验发现：只有 valid_input 一处要显式补）。
- [ ] **Step 3: 落记录纯逻辑**——`request_log_writer.rs`：
      `fn outcome_for(result: &DispatchResult) -> Outcome`（succeeded/waiting/accepted/failed 四桶）；
      Record 组装（坐标/requested_by/record_id/config_id/message/serial_number/response_body）。
      validate 失败也落（outcome=failed）。
- [ ] **Step 4: handle 接线**——每个出口落一条（含 `validate()?` 失败——把校验移到
      能拿到 Record 的位置）；**落库失败降级 warn/error，不改变返回值**。
- [ ] **Step 5: 单测**——outcome 归类表驱动测试；未知/边界输入（空 requested_by、
      超长 requested_by）。
- [ ] **Step 6: 门禁**——quick。

> 验证标准：单测覆盖四桶映射；集成测试见 Task C 的 Step 8（真实 MySQL 全出口）。
> 注意：dispatch 测试里 `valid_input` 是 struct literal，`requested_by` 用
> `Some("测试触发人".into())` 或 None 各覆盖一次。

---

## Task C：七个控制台 Action + 注册表重构

**Files:**
- Create: `src/addon/feishu/approval/actions/list_configs.rs`
- Create: `src/addon/feishu/approval/actions/create_config.rs`
- Create: `src/addon/feishu/approval/actions/update_config.rs`
- Create: `src/addon/feishu/approval/actions/delete_config.rs`
- Create: `src/addon/feishu/approval/actions/list_widgets.rs`
- Create: `src/addon/feishu/approval/actions/list_requests.rs`
- Create: `src/addon/feishu/approval/actions/list_tasks.rs`
- Modify: `src/addon/feishu/approval/actions/mod.rs`

> 一 Action 一文件、自包含 register（架构门禁）；每个文件链式 `.route().display_name()
> .description().permissions(...).register()`。分页用共享 `ListInput`（与通用 TableView
> 同构，`domain/list_input.rs`，勿忘 `deny_unknown_fields` 六字段兼容）。

- [ ] **Step 1: 注册表重构**（`actions/mod.rs`）——7 个新 Action 在 `can_pull()` 门禁
      **之前**注册；dispatch + 管理 Token 中间件保持在门禁之后。加注册路由单测
      （有/无凭证两态：7 个新路由两态都注册；dispatch 仅带凭证，照既有
      `dispatch_route_is_registered_with_credentials` 形态扩展）。
- [ ] **Step 2: `list_configs`**（`feishu.approval.read`）——ListInput 分页；
      映射按 config 分组一次 `where_in` 带回（照 `group_bindings` 防 N+1 模式）；
      投影不含 `form_snapshot`（大字段显式排除列清单）。
- [ ] **Step 3: `create_config`**（`feishu.approval.write`）——入参六件套
      （base_token/table_id/approval_code/applicant_field/backfill_field/base_timezone）；
      `outbound_setup::build` + `build_plan` + `insert_plan` 全部复用（provision 链路）
      ——**不新写校验**；`ConstraintError` → 「该多维表格已配置，请先查询或删除后重建」；
      审计 `succeeded_event`（user actor，target=feishu_approval_config，after=config_id）+ `append_in_tx`；
      凭证缺失 50301（照 dispatch provision 的处置）。
- [ ] **Step 4: `update_config`**（`feishu.approval.write`）——仅 title/enabled/base_timezone
      三字段更新（全可省、至少一个）；before/after 摘要进审计；无该配置 → 明确 404 语义。
- [ ] **Step 5: `delete_config`**（`feishu.approval.write`）——同事务：删配置行 +
      field_map 行 + `state=pending` 的 task 行；backfilled/terminal 任务行保留作流水；
      **不加未完结守卫**（核验发现：worker 对配置缺失是优雅的 terminal 降级，见设计 §2.2-1/2）；
      审计 after_summary 带 config_id 与删除行数。
- [ ] **Step 6: `list_widgets`**（`feishu.approval.write`——出站归 write 侧，
      与 list_bitable_* 同理）——按 approval_code 调 `approvals get` + `parse_form`
      返回控件列表（id/名称/类型/required）。
- [ ] **Step 7: `list_requests` + `list_tasks`**（`feishu.approval.read`）——
      ListInput 分页；`list_requests` 随行带 request_body/response_body（体量小可直接展开）；
      `list_tasks` 按 config_id/state 过滤。
- [ ] **Step 8: 集成测试**——新增 `tests/feishu_approval_dispatch_integration.rs`
      （或扩展既有 `feishu_approval_options_integration.rs` 同级），真实 MySQL 下：
      dispatch 全出口落记录（validate 失败/单条成功/单条等待/批量受理/provision 失败各一行、
      outcome 正确）；create_config → list_configs → update → delete 的 CRUD 闭环；
      delete 后 pending task 被清、backfilled 保留。在 `scripts/run_ci.py` 的
      INTEGRATION 登记处加一行。`run_ci.py integration` 绿。
- [ ] **Step 9: 门禁**——quick（含 clippy 零告警、架构门禁、schema_anchor）。
- [ ] **Step 10: 契约再生成（Task E 前置）**——`python scripts/dump_openapi.py` +
      `pnpm --dir frontend gen:contracts`，提交 openapi.json 与 api-types.ts 快照。

> 验证标准：quick + integration 全绿；7 条新路由在契约里（Task E 核对）；
> dispatch 仍在契约外（门禁机制，非 public 规则——见设计 §2.2-6 的认知修正）。

---

## Task D：前端（配置页 + 向导 + 记录页）

**Files:**
- Create: `frontend/src/features/feishu/views/ApprovalConfigsPage.tsx`
- Create: `frontend/src/features/feishu/views/ApprovalRequestsPage.tsx`
- Create: `frontend/src/features/feishu/components/ApprovalConfigDialog.tsx`（建配置向导，四步）
- Create: `frontend/src/features/feishu/components/ApprovalConfigRowActions.tsx`（启停/删除/映射明细）
- Create: `frontend/src/features/feishu/components/ApprovalRequestRow.tsx`（展开看请求/返回体）
- Modify: `frontend/src/features/feishu/api.ts`（`APPROVAL_OPERATION_IDS` + 调用函数）
- Modify: `frontend/src/features/feishu/types.ts`（新类型）
- Modify: `frontend/src/shell/routes.tsx`（两条 lazy 路由 + RouteFallback）
- Modify: `frontend/src/shell/AppLayout.tsx`（侧边栏「审批派发」「派发记录」两入口，Catalog 门控）
- Create: `frontend/tests/features/feishu/views/approval-configs-page.test.tsx`
- Create: `frontend/tests/features/feishu/views/approval-requests-page.test.tsx`
- Create: `frontend/tests/features/feishu/components/approval-config-dialog.test.tsx`

- [ ] **Step 1: api/types**——`APPROVAL_OPERATION_IDS` 常量（7 个，逐个对服务端
      `action_name!` 核）；调用函数走目录驱动（照 TableWizard 相关函数形态），
      类型手写进 `types.ts`（仓库惯例：契约只生成骨架，页面类型手写对齐）。
- [ ] **Step 2: 路由 + 侧边栏**——两条 lazy 路由（`/feishu/approval/configs`,
      `/feishu/approval/requests`）+ RouteFallback；「飞书集成」组两入口，
      门控 `feishu.approval.list_configs` / `list_requests`（照 canReadFeishuDatasources）。
- [ ] **Step 3: 配置页**——列表（名称/坐标/Code/时区/启用开关/快照时间），
      启停/删除（ConfirmDialog）/映射明细（展开），「新建配置」按钮开向导。
- [ ] **Step 4: 建配置向导**——四步：坐标（复用 list_bitable_tables/views/fields 端点，
      工具提示说明需 `feishu.approval.write` + `feishu.datasource.write`）→ Code +
      控件预览（list_widgets）→ 申请人/回填列 + 时区 → 提交（服务端一次报全原因，
      展示在最后一步）。
- [ ] **Step 5: 记录页**——列表（时间/请求人/坐标/record_id/outcome 色标/message），
      行展开请求体与返回体 JSON 原文；批量行（accepted）提供「查看任务」
      下钻 `list_tasks`（按 config_id 过滤）。
- [ ] **Step 6: 测试**——两页 view 测试 + 向导测试（照 datasources 页测试形态，
      含权限门控两组断言）。
- [ ] **Step 7: 门禁**——`pnpm --dir frontend check`。

> 验证标准：check 绿；`frontend/tests/engine/contracts/openapi-contract.test.ts`
> 静态编译通过（Task E 的契约快照与前端类型必须同批提交）。

---

## Task E：契约再生成

- [ ] **Step 1**：`python scripts/dump_openapi.py`（前后端同源）。
- [ ] **Step 2**：`pnpm --dir frontend gen:contracts`。
- [ ] **Step 3**：人工核对 ——`frontend/contracts/openapi.json` 出现
      `/api/v1/feishu/approval/configs/{query,create,update,delete}`、
      `/definitions/widgets`、`/requests/query`、`/tasks/query`；
      `/api/v1/feishu/approval/dispatch` **不在**（门禁机制）。
- [ ] **Step 4**：`frontend/tests/engine/contracts/openapi-contract.test.ts` 与
      `pnpm --dir frontend check` 通过。
- [ ] **Step 5**：提交快照（`frontend/contracts/openapi.json` +
      `frontend/src/engine/contracts/api-types.ts`）。

---

## Task F：文档同步

**Files:**
- Modify: `AGENTS.md`（新 module/表/7 Action/权限键/requested_by/向导权限说明）
- Modify: `docs/contracts/AUDIT.md`（「必须覆盖的高权限变化」登记 3 个新审计 Action）
- Modify: `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`
  （§4.1 模板补 requested_by、补请求记录一节、M 表标 M11（09-28 设计 M10 已被占用，待实测项顺延 M11））
- Modify: `docs/architecture/2026-09-28-feishu-approval-dispatch-plan.md`
  （进度表：Task 13 标记完成/承接；或登记到本计划）

- [ ] **Step 1**: AGENTS.md 事实同步（单一事实源，硬要求）。
- [ ] **Step 2**: AUDIT.md 登记 3 个审计事件。
- [ ] **Step 3**: 原设计/计划文档交叉同步。
- [ ] **Step 4**: `run_ci.py full` 全量门禁（本计划 7 个任务完成后）。

---

## 落地顺序（不可乱）

1. Task A（表+Context+anchor）→ 2. Task B（dispatch 落记录）→
3. Task C（Action + 集成测试，Step 10 顺手跑 Task E）→
4. Task D（前端，依赖 Task C/E 的契约快照）→ 5. Task F（文档收尾）。

Task A/B/C 彼此有依赖（B 依赖 A 的表与 Context；C 依赖 A 的表访问器与 B 的
requested_by——B 会先给 `DispatchInput` 加 Serialize，C 的集成测试断言落记录）。
Task D 与 E 是硬依赖（页面类型对齐契约快照），不可并行切分。