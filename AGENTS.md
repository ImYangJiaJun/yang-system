# AGENTS.md

面向 AI 编码代理的项目说明。假设读者对本项目一无所知；所有内容均来自仓库实际文件，修改代码前请以此为准，并以 README 和 `docs/` 下的专门文档为最终契约。

## 项目概览

`yang-system` 是基于 `yang-base` 框架的模块化单体参考应用：一个 Rust (axum) 后端服务 + Quasar/Vue 管理控制台。同一份 Addon/Module 定义同时驱动强类型 Action、数据库 Schema、Catalog、Registry、OpenAPI 和前端页面。业务覆盖：账号与会话（邮箱验证码注册、登录、Refresh Cookie、Step-up 重认证）、平台管理（最终管理员）、企业租户与成员、个人项目/任务、授权失效传播（authz_version + Outbox）、高权限审计和生产可观测性。

本仓库是**独立 Git/Cargo 项目**，但被签出在 `lib_yang` 仓库的 `project/yang-system/` 路径下；`Cargo.toml` 通过相对路径直接依赖同工作树中的基础库：

```toml
yang-base    = { path = "../../crates/yang-base", ... }
yang-db      = { path = "../../crates/yang-db", ... }
yang-runtime = { path = "../../crates/yang-runtime", ... }
```

因此在本目录执行普通 Cargo 命令即可联合调试基础库，不需要 `patch`、绝对路径或恢复 `Cargo.lock`。

### 技术栈

- 后端：Rust 2021（rust-version 1.80，仓库固定使用 1.97.1 工具链），axum 0.8、tokio、sqlx (MySQL)、Redis、jsonwebtoken、argon2、lettre (SMTP)、tracing、metrics。
- 前端：Quasar 2 / Vue 3.5 / Vite / Pinia / vue-router / zod，TypeScript，pnpm 10.33.1（`packageManager` 字段固定），Node.js 24+。
- 依赖服务：MySQL 8.0（`127.0.0.1:3306`）与 Redis 7（`127.0.0.1:6379`），由根目录 `compose.yaml` 提供；`docker/mysql/init/` 会同时创建 `yang_system` 和 `yang_system_test` 两个库。
- 辅助脚本：Python 3.11+（质量门禁、架构检查、Action 脚手架、本地初始化）。

## 目录结构

```text
src/
├── addon/                   # 业务 Addon；每个 Addon 下按 Module 拆分，Module 下按 actions/ 拆分
│   ├── account/             # 注册/会话/邮件投递、授权快照、用户生命周期、密码重置
│   ├── admin/               # 首个注册账号的最终管理员与平台账号管理
│   ├── observability/       # 浏览器错误与服务端 request_id 关联上报
│   ├── org/                 # 企业创建/选择（access）、企业资料（organization）、成员（user）
│   └── work/                # 个人项目（project）、任务树（task）
├── config/                  # 不可变运行配置、配置源合成（source.rs）
├── infrastructure/          # 审计（audit/）、授权一致性（authorization/）、声明式 Schema（schema.rs）
├── app.rs                   # 所有业务 Addon 的唯一组合根
├── bootstrap.rs             # 配置、Telemetry、Tools、Schema、Worker、HTTP 与关闭顺序
├── lib.rs / main.rs         # main.rs 只是 bootstrap::run("config.toml") 的入口
tests/                       # Rust 集成测试（真实 MySQL/Redis）与 benchmark
examples/frontend_demo/      # 数据库无关的演示后端，供 Playwright 浏览器测试使用
migrations/                  # 空目录：本项目不用 SQL 迁移文件，Schema 由代码声明驱动
scripts/                     # run_ci.py / check_architecture.py / new_action.py / setup_local.ps1 等
docs/                        # 安全与运行契约：SCHEMA/AUDIT/CONFIGURATION/OBSERVABILITY/SLO 等
frontend/                    # Quasar 控制台（见下）
ops/prometheus/              # Prometheus 告警规则与演练（CI 用 promtool 校验）
frontend/deploy/             # 生产 Nginx 配置与部署契约校验
```

前端内部约定：API 客户端与契约在 `frontend/src/api/` 和 `frontend/src/contracts/`，可复用 UI 在 `components/`，页面编排在 `pages/` 和 `layouts/`，Pinia 状态在 `stores/`。单元测试与源码同目录（`*.test.ts`），Playwright 规格在 `frontend/e2e/`（`*.spec.ts`）。

## 构建、测试与开发命令

本地质量门禁与 `.github/workflows/ci.yml` 共用同一入口 `scripts/run_ci.py`：

- `python scripts/run_ci.py quick` — 提交前：架构检查（含各自检）、`cargo fmt --check`、`cargo test --lib --locked`、前端 typecheck 与 Vitest。
- `python scripts/run_ci.py full` — 推送前：在 quick 之上增加 `cargo test --all-targets --locked`、`cargo clippy --all-targets --all-features --locked -- -D warnings`、前端 `pnpm audit --prod`、前端完整 `pnpm check`（format/lint/typecheck/test/locale 契约/build/构建与部署契约校验）、以及端口隔离的 dev-server 和 production-build 两套 Playwright。
- `python scripts/run_ci.py integration` — 真实依赖集成测试（见“测试策略”）。
- `python scripts/run_ci.py --self-test` — 校验本地门禁与 GitHub Actions workflow 的一致性。

其他常用命令：

- `cargo run --locked` — 启动后端（`http://127.0.0.1:8080`），需要已配置 `config.toml`。
- `python scripts/check_architecture.py` — 架构门禁：保证 `actions/` 中一个文件只承载一个 Action、文件与 `mod.rs` 清单一致、租户边界注释等。
- `python scripts/new_action.py <module_actions_dir> <name> --title "..." --method POST --path /api/v1/...` — 新增 Action 脚手架（创建文件、声明 mod、注册）；生成代码返回“尚未实现”错误，必须补齐业务逻辑后才算完成。
- `pnpm --dir frontend dev` — 前端开发服务器（`http://127.0.0.1:5173`），默认通过 Vite 代理访问 `127.0.0.1:8080` 的真实后端。
- `pnpm --dir frontend check` — 前端完整检查链；单独排查可用 `typecheck` / `lint` / `test` / `e2e` / `e2e:production`。
- 首次环境初始化（Windows PowerShell 7，在 `lib_yang` 仓库根目录执行）：`pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1`，会启动 Compose 依赖、安装前端依赖并生成被 Git 忽略的 `config.toml`；`-CheckOnly` 只检查工具，`-UpgradeLegacyConfig` 迁移旧配置。

## 代码组织与架构约定

- **每个自定义 Action 独占一个文件**，放在所属 Module 的 `actions/` 目录下；`mod.rs` 只用于装配和共享声明。业务路径必须显式位于 `/api/v1/`。改动 `actions/` 后必须通过 `python scripts/check_architecture.py`。
- 核心链路：`Addon/Module/fields!/params!` → `AppBuilder` → Catalog / Registry / TableDefinition → OpenAPI/UI、HTTP dispatch、Schema。请求处理顺序：auth → authz_version → tenant/permission → Step-up/resource guard → Action → 业务事实 + 版本 + Outbox/audit。
- `fields!` 是 Schema、输入输出约束、OpenAPI 和查询策略的唯一字段事实来源；`params!` 同时生成强类型输入与参数契约，请求只反序列化一次。
- `ToolsBuilder -> Tools` 显式持有 MySQL、Redis、Token 等资源；**禁止引入进程级数据库/Redis/Tools 单例**。
- `org_user.org_org` 是租户键；普通上下文缺少 `TenantContext` 时查询 fail-closed，只有显式 system 上下文可绕过。裸 SQL 和租户数据路径的边界见 `docs/architecture/raw-sql-boundaries.md` 与 `tenant-data-paths.md`。
- 数据库结构由 `src/infrastructure/schema.rs` 的声明统一驱动：启动时先只读计划和旧数据预检，全部安全后才保数据增量同步；冲突会拒绝启动。**不要新增 SQL 迁移文件**（`migrations/` 为空是有意的）。规则见 `docs/SCHEMA.md`。
- 前端业务导航由后端 Catalog 投影驱动，通用 `ModulePage` 解释 TableView/JSON Schema 表单/操作语义；需要特殊交互的页面必须在静态自定义视图注册表中显式登记。**不要根据后端返回的字符串构造动态 import**。

## 代码风格

- Rust 2021，标准 `rustfmt`（四空格缩进）；文件/函数用 `snake_case`，类型用 `PascalCase`，Action 文件名用描述性名称（如 `actions/register.rs`）。
- `Cargo.toml` 已配置：`unsafe_code = "forbid"`，Clippy `unwrap_used` / `expect_used` 均为 deny。生产 Rust 代码禁止 `unsafe`、`unwrap()`、`expect()`，用 `anyhow` 传播错误。
- 仓库文档与注释使用中文，新增注释和文档保持中文风格。
- 前端代码必须通过 Prettier、ESLint（`--max-warnings 0`）和 `vue-tsc`；Vue 组件用 `PascalCase`。

## 测试策略

- Rust：逻辑旁加 `#[cfg(test)]` 单元测试；`cargo test --lib --locked` 属于 quick 门禁，必须始终通过。
- 前端：改动的契约或状态逻辑要有同目录 `*.test.ts` 回归测试（Vitest）；浏览器行为用 `frontend/e2e/*.spec.ts`（Playwright），E2E 使用 `examples/frontend_demo/` 的无数据库后端，dev-server 与 production-build 两套环境用独立端口隔离。
- 真实集成测试（`tests/*.rs` 与部分 `--lib --ignored` 测试）要求专用 MySQL 数据库名以 `_test` 结尾、Redis 强制 DB 15，不用 mock 替代：

  ```powershell
  $env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
  $env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
  python scripts/run_ci.py integration
  ```

  覆盖授权缓存单调性/Outbox 并发重放、Schema 预检/apply、跨实例并发 apply、审计/最终管理员、邮箱验证码对抗边界、注册/登录/Refresh/会话失效、租户隔离和业务系统路径。集成测试单线程运行（`--test-threads=1`）。
- 无数值覆盖率门槛，但改变的行为必须有测试覆盖。

## 安全注意事项

- `config.toml` 被 Git 忽略且**禁止提交**；仓库只保留无真实凭据的 `config.example.toml`。配置按 `config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成，契约见 `docs/CONFIGURATION.md`。
- Token、Step-up、邮箱验证码使用各自独立的 keyring/密钥，禁止复用；`config.example.toml` 中的占位密钥会被启动校验拒绝。
- 授权快照（`authz_version`）以 MySQL 为最终事实源，Redis 只做短 TTL 单调加速；授权 writer 必须在同一事务中更新业务事实、单调递增版本并追加 Outbox，Outbox 重放不能让版本回退。
- `security.issue_refresh_credential_version` 是三阶段发布开关，新旧实例混跑时不能提前打开（见 README）。
- 首个注册账号通过数据库唯一 `owner_key=system-owner` 哨兵原子成为最终管理员；首次注册前必须用本机监听/防火墙/反向代理限制不可信访问。
- 默认不信任任何 `Forwarded`/`X-Forwarded-For`；只为真实反向代理配置最小 `trusted_proxy_cidrs`。
- `app.environment` 缺省按 `production` 处理；示例配置仅面向本地开发。
- 高权限操作按契约使用 Step-up、最后管理员保护和 append-only 审计（`docs/AUDIT.md`）；Step-up proof 在生产通过 Redis 原子单次消费。

## CI 与部署

- CI（`.github/workflows/ci.yml`）在 push/PR 时运行：quality job 执行 `run_ci.py full` + `run_ci.py integration`，另有用固定版本+摘要的 Nginx 镜像做生产配置 `nginx -t`、用 promtool 校验 `ops/prometheus/` 规则与告警演练。CI 通过只读 Deploy Key 签出 `lib_yang` 依赖仓库（`persist-credentials: false`）。
- 生产启动：`cargo run --locked --bin yang-system`。启动顺序：配置合成验证 → Telemetry/关闭预算 → 资源创建 → AppBuilder 冻结 Catalog/Registry → Schema 预检与增量同步 → 审计表校验、管理面与 Outbox Worker → readiness 就绪后启动业务路由；停机时先撤销 readiness 再在同一总预算内排空。
- 存活/就绪：`/health/live`、`/health/ready`；独立管理面默认 `http://127.0.0.1:9090`（`/metrics` 与带依赖检查的 `/health/ready`），生产编排应使用管理面 readiness。
- 前端生产部署使用 `frontend/deploy/nginx.conf`（CI 校验语法与部署契约）。

## 提交与 PR

- 遵循 Conventional Commits：`feat(frontend): ...`、`fix(schema): ...`、`refactor(org): ...`，保持提交范围单一。
- PR 需说明行为与验证方式，标注 schema/配置变更，可见的前端变更附截图；请求评审前跑过 `run_ci.py full`。

## 重要参考文档

- `README.md` — 能力清单、本地环境、启动顺序、安全模型的权威说明
- `docs/SCHEMA.md` — 声明式 Schema 演进规则
- `docs/CONFIGURATION.md` — 环境变量、secret provider、keyring 轮换、关闭预算
- `docs/OBSERVABILITY.md` — 指标、readiness、日志与 tracing 契约
- `docs/AUDIT.md` — 高权限审计
- `docs/REGISTRATION_EMAIL_VERIFICATION.md` — 注册邮箱验证码边界
- `docs/architecture/` — 授权失效 ADR、裸 SQL 边界、租户数据路径、会话 TTL 等
