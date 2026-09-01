# yang-system 骨架收敛改造记录：只留 account + Action 自包含 register

> 创建日期：2026-08-24
> 状态：**已完成**。addon 清空只留 account、Action 改自包含 register 形态、表声明独立
> `table.rs`、注册表数组化均已落地；本文档同步是收尾动作。
> 性质：改造记录。事实以仓库当前源码为准，本文只做定向索引。

## 一、改造背景

2026-08-22 的架构净化（见
[`2026-08-22-architecture-refactor-handoff.md`](2026-08-22-architecture-refactor-handoff.md)）
把系统整理为五个业务 Addon（account/admin/org/work/observability）+ addon 级 Cargo
feature 的形态。随后产品方向调整：yang-system 回归「账号体系参考骨架」定位，平台管理、
企业租户、个人项目/任务和浏览器错误上报全部移除，只保留账号与会话链路。

本次改造的四项决定：

1. **Addon 清空只留 account**：删除 `src/addon/{admin,org,work,observability}` 及其
   Cargo features；`[features]` 只剩 `default = ["account"]`。企业租户键
   `org_user.org_org`、最终管理员 `system-owner` 哨兵、前端错误上报端点随之消失。
   account 域保留 `SystemOwnerClaimer` 端口但注入 `NoSystemOwnerClaimer`：注册流程照常
   完成，任何账号都不会成为系统最终管理员。
2. **Action 自包含 register**：每个 `src/addon/account/user/actions/<name>.rs` 内含
   `params!`（或手写 Input）+ 恰好一个 `pub(super) async fn handle` + 一个
   `pub(super) fn register(module, account)`；路由/展示元数据与 handler 在同一文件内
   原子绑定，不再集中维护 `actions/mod.rs` 路由表。
3. **表声明独立 `table.rs`**：module 层三件套固定为 `mod.rs`（装配+展示投影）、
   `table.rs`（表/字段声明）、`actions/`；业务机制留在 addon 级 `domain/`。
4. **注册表数组化**：`actions/mod.rs` 只保留 `mod` 清单和
   `const ACTIONS: &[Register]` 数组（经 `action_registry!` 宏展开），新增接口 =
   加一行 `mod` 声明 + 一行数组项；`scripts/new_action.py` 按
   `// scaffold:action-registration` 标记落位。

## 二、终态结构

```text
src/addon/
└── account/
    ├── mod.rs               # addon 层装配入口与对外端口（含 NoSystemOwnerClaimer）
    ├── domain/              # addon 级共享机制：context/repository/authz_version/
    │                        # grants/email_delivery/password_reset/status/policy/claims
    └── user/                # module 层三件套
        ├── mod.rs           # 装配：表 → 上下文 → 中间件 → 注册表 → Step-up → 展示投影
        ├── table.rs         # users 表声明（fields! 唯一事实来源）
        └── actions/         # 10 个自包含 Action：
            ├── mod.rs       #   mod 清单 + ACTIONS 注册表数组
            ├── request_registration_email.rs / register.rs / login.rs / refresh.rs
            ├── change_password.rs / disable_self.rs / reset_password.rs  # 发布开关门控
            ├── logout.rs / step_up.rs / me.rs
```

`account.user` 的 10 个接口：邮箱验证码申请、注册、登录、Refresh、改密、自助停用、
密码重置、全设备退出、Step-up、当前用户。其中改密/自助停用/密码重置受
`credential_mutations_enabled` 发布开关门控，Step-up 受组合根配置条件注册——条件
判断都在 `user/mod.rs` 装配处，注册表本身保持纯数组。

集成测试同步收敛到三个入口：`tests/registration_email_integration.rs`（邮箱验证码
对抗边界）、`tests/refresh_load_benchmark.rs`（Refresh 轮换负载基准）、
`tests/schema_apply_integration.rs`（Schema 并发 apply）；原 bootstrap、
tenant_isolation、tenant_query_benchmark、system_integration 已删除。

## 三、验证方式

- 架构门禁 `python scripts/check_architecture.py`：每 Action 文件恰好一个
  `pub(super) async fn handle` 与一个 `pub(super) fn register`，文件与 `ACTIONS`
  数组一致；授权 writer 仍按 `docs/architecture/authorization-writers.md` 清单收敛在
  account 域两个 allowlist 项。
- `python scripts/run_ci.py quick` / `full` / `integration`：Rust 单测、clippy、
  前端检查链与真实 MySQL/Redis 集成测试。
- 文档同步（本记录同批）：`AGENTS.md`、`README.md`、`docs/AUDIT.md`、
  `docs/OBSERVABILITY.md`、`docs/SLO.md`、`docs/REGISTRATION_EMAIL_VERIFICATION.md`、
  `docs/architecture/authorization-freshness-adr.md` 改写为 account-only 现状；
  `ops/prometheus/yang-system.rules.yml` 删除 `YangSystemFrontendErrorBurst` 告警，
  演练文件清空用例并保留 CI 入口；两份文件经 promtool
  `check rules` / `test rules` 通过（13 条规则）。

## 四、已知遗留

- 前端 `frontend/src/observability/error-reporter.ts` 仍向
  `/api/v1/observability/frontend-errors` 上报，后端端点已删除，上报会静默失败；
  前端侧清理不在本次范围。
- 审计表 `audit_event` 保留 `tenant_id` 列与索引作为 Schema 事实，当前账号域事件
  均为空值；未来引入租户域时按 `docs/AUDIT.md` 契约填写。
- 未来新增外围 Addon（平台/企业等）时，需重新评估首次注册访问限制与最终管理员
  信任根，并在 `authorization-writers.md` 先登记新授权事实 writer 再写代码。
