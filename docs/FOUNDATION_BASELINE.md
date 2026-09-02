# 基座完备化路线与决策记录

**生成：** 2026-09-03
**目标：** 将 yang-system 从"单 addon 骨架"补齐为可承载后续多种业务的开发基座。
**工作方式：** 按优先级逐阶段推进；每阶段执行前先明确边界与要实现的功能；TDD 开发（先写测试用例再实现）；每阶段一次 git 提交（只提交本仓库，不 push）。

## 背景：审计结论（2026-09-03）

对基座做了多角度审计（后端扩展性 / 前端 / 运维部署），主要结论：

- 契约驱动管线（`fields!` → Schema/路由/OpenAPI/前端 Catalog 投影）全自动，纯 CRUD 业务前端零代码。
- 横切能力（鉴权、authz_version、Outbox、Step-up、审计、声明式 Schema、优雅停机、配置合成）通用且扎实。
- 缺口按优先级：
  - **P0-1** 权限模型未启用：`grant_resolvers` 为空（`src/app.rs:66`），没有任何 Action 声明 `.permissions(...)`，无权限目录/授权存储/角色管理。
  - **P0-2** 授权失效无公共 writer 端口：`infrastructure/authorization/request_validator.rs:117` 反向依赖 account 域函数，`increment_locked_credential_versions` 为 crate 内私有。
  - **P0-3** 无平台管理面：无 admin 域、无权限/授权管理接口。
  - **P0-4** 前端身份硬编码：`frontend/src/stores/identity.ts:10` 持久化身份写死 `"user"`。
  - **P0-5** 无部署产物：无 Dockerfile/编排清单/备份恢复/日志聚合约定。
  - **P1** 无后台任务/调度框架（仅 Outbox worker 特例）、邮件同步发送无重试、SMTP 失败无告警、告警演练文件为空（`ops/prometheus/yang-system.rules.test.yml` 的 `tests: []`）。

## 阶段计划

| 阶段 | 内容 | 对应缺口 | 状态 |
|------|------|----------|------|
| P0 | 本文档：边界、决策、验收标准 | — | ✅ |
| P1 | 权限基础设施：`access` addon（权限目录 + 授权存储 + GrantResolver 实现 + 管理 Action 声明 `.permissions(...)`） | P0-1 | ✅ |
| P2 | 授权失效 writer 公共端口（解除 infrastructure → account 反向依赖） | P0-2 | ✅ |
| P3 | 平台管理面：`access` addon 管理 Action（授权/撤销/查询）+ 权限目录投影 | P0-3 | ✅ |
| P4 | 前端身份扩展（`identity.ts` 改为 catalog 驱动） | P0-4 | ✅ |
| P5 | 部署产物：Dockerfile + 备份恢复 runbook + 日志聚合约定 | P0-5 | ✅ |
| P6 | 运维补齐：SMTP 失败告警、promtool 演练 | P1 | ✅ |
| P7 | 演示 addon 验证"新业务全流程接入"（验证审计结论"新增成本为零"） | 验证 | 待办 |

## 关键决策（ADR 摘要）

- **D1：权限数据归属**：权限目录与授权存储放新 addon `src/addon/access/`（域职责独立，不污染 account；符合"Addon 层目录只承载 mod.rs、module 子目录与 domain/ 的约定）。
- **D2：无最终管理员保持**：系统仍不提供"最终管理员"角色；初始授权由运维经 SQL/工具完成（文档化），应用不提供自提权路径。管理 Action 只要求权限 `access.grants.*`，不做任何"第一个用户自动是管理员"逻辑。
- **D3：权限目录来源**：权限目录 = 运行期从 Catalog 投影所有 Action 声明的 `.permissions(...)` 集合（单一事实来源，不另维护静态清单）。
- **D4：授权存储最小化**：Phase 1 只存"用户 ↔ 权限字符串"直授关系（`authz_grant` 表），不引入角色聚合层；角色仍用固定 `"user"`。后续如需角色再扩展（留接口）。
- **D5：TDD**：每阶段先在目标文件旁 `#[cfg(test)]` 写好失败测试，再实现到通过；新增 Action 必跑 `python scripts/check_architecture.py` 与 `cargo test --lib --locked`；集成测试改动需真实 MySQL/Redis 时按 AGENTS.md 约定运行。
- **D6：提交粒度**：每阶段一次提交，消息格式 `feat(access): ...` / `refactor(authz): ...` / `chore(ops): ...`，body 说明边界与验证方式，便于溯源。
- **D7：不 push**：所有提交仅在本地仓库，不推送远端。

## 验收标准（每阶段通用）

- `python scripts/check_architecture.py` 通过。
- `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings` 通过。
- `cargo test --lib --locked` 通过（含新增单元测试）。
- 有新增行为即有测试覆盖；改动涉及 schema/配置在提交信息与文档中标注。
