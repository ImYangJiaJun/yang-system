# 审批配置业务视角改版实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将审批配置控制台改为业务人员优先的列表与独立详情视图，同时保留现有创建、编辑、启停、删除、派发和权限行为。

**Architecture:** 后端在现有 `list_configs` 投影中补充可空展示元数据，并继续按稳定 ID 执行业务；前端在 `ApprovalConfigsPage` 内实现列表筛选、详情状态和字段对应标签页，复用现有 Action、Query、Dialog 与 UI 组件。展示名称不参与派发，缺失时回退到明确的“名称待补充”。

**Tech Stack:** Rust/yang-base declarative schema, React 19, TanStack Query, Tailwind/shadcn, Vitest.

**Spec:** `docs/architecture/2026-10-12-feishu-approval-config-business-view-design.md`

## Global Constraints

- `enabled` 只表达配置是否允许受理派发，不表示工作流健康或运行成功。
- `form_snapshot_at` 只表达审批结构缓存时间，不表示检查通过。
- 来源表、审批模板、申请人列、编号回填列仍为不可变坐标；更换它们需新建配置。
- 名称只用于展示和搜索，派发、权限、映射和回填仍使用稳定 ID。
- 同一 `(base_token, table_id)` 保持唯一；多个不同数据表可以共用 `approval_code`。
- 查询必须真实分页，名称搜索不得只在当前页过滤；详情与关联配置不能依赖当前页。
- 不新增工作流编辑器、健康检查、后台服务或 SQL migration。
- 后端 Action 变更后同步 OpenAPI 与前端生成类型；生产代码遵守 Rust/React 目录和权限契约。

## Review Focus

- 缺少展示名称时，页面显示明确回退文案且仍能通过排查信息识别稳定 ID。
- 搜索、状态筛选和分页组合时不会把当前页数量当作全量总数。
- 同一个审批定义关联多个配置时，不会因当前列表页截断而少报。
- `enabled`、快照时间和工作流接通状态在文案上严格分开。
- 既有写权限、编辑校验和固定坐标约束不会被详情页绕过。

### Task 1: 后端展示元数据与查询契约

**Files:**
- Modify: `src/addon/feishu/approval/actions/list_configs.rs`
- Modify: `src/addon/feishu/approval/table.rs` only if declarative nullable fields are needed
- Modify: `frontend/src/features/feishu/types.ts`
- Modify: `frontend/src/features/feishu/api.ts`
- Test: `src/addon/feishu/approval/actions/list_configs.rs` unit tests and existing integration fixture assertions

- [x] Add nullable business display fields to the list response without changing stable coordinates.
- [x] Preserve standard list input, ordering, count and map grouping behavior.
- [x] Parse nullable fields on the frontend and keep older responses compatible.
- [x] Add focused tests for missing metadata and field-map display terminology.

### Task 2: Business-oriented list and independent detail view

**Files:**
- Modify: `frontend/src/features/feishu/views/ApprovalConfigsPage.tsx`
- Modify: `frontend/src/features/feishu/components/ApprovalConfigRowActions.tsx` only if action labels need independent detail navigation
- Test: `frontend/tests/features/feishu/views/approval-configs-page.test.tsx`

- [x] Replace technical columns with configuration name, source table, approval name and status.
- [x] Add enabled/disabled filters and search across config, table and approval names while preserving real pagination.
- [x] Add independent detail view with business flow, applicant/backfill rules, field mapping, usage and troubleshooting tabs.
- [x] Keep create/edit/toggle/delete actions and permission gates unchanged.
- [x] Add loading, error/retry, empty, no-result and missing-name states.

### Task 3: Verification and contract synchronization

**Files:**
- Modify: generated OpenAPI/TypeScript contract files only through repository generators if API changes require them.
- Test: frontend targeted Vitest, Rust lib tests, architecture checks.

- [x] Run focused frontend tests and typecheck.
- [x] Run Rust formatter and targeted list-config tests.
- [x] Run architecture checks and inspect diff/status for scope.
- [x] Record limitations: no live Feishu workflow or cloud deployment verification.

实施及验收证据见[实现验收记录](../../architecture/approval-config-business-view/implementation.md)。最终 `run_ci.py full` 通过；审批派发真实库集成 9/9，名称刷新真实库集成 1/1。最终审查的名称刷新覆盖缺口已补齐并完成摘除变异验证。
