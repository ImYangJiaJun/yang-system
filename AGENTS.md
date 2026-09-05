# AGENTS.md

面向 AI 编码代理的项目说明。假设读者对本项目一无所知；所有内容均来自仓库实际文件，修改代码前请以此为准，并以 README 和 `docs/` 下的专门文档为最终契约。

## 项目概览

`yang-system` 是基于 `yang-base` 框架的模块化单体参考应用：一个 Rust (axum) 后端服务 + React 管理控制台。同一份 Addon/Module 定义同时驱动强类型 Action、数据库 Schema、Catalog、Registry、OpenAPI 和前端页面。当前骨架只保留 `account` 一个业务 Addon：账号与会话（邮箱验证码注册、登录、Refresh Cookie、Step-up 重认证、密码重置）、授权失效传播（authz_version + Outbox）、高权限审计和生产可观测性。没有平台管理、企业租户和业务对象域，也没有任何账号会成为系统最终管理员。

本仓库是**独立 Git/Cargo 项目**，但被签出在 `lib_yang` 仓库的 `project/yang-system/` 路径下（`lib_yang` 根 workspace 显式排除它）；`Cargo.toml` 通过相对路径直接依赖同工作树中的基础库：

```toml
yang-base    = { path = "../../crates/yang-base", ... }
yang-db      = { path = "../../crates/yang-db", ... }
yang-runtime = { path = "../../crates/yang-runtime", ... }
```

因此在本目录执行普通 Cargo 命令即可联合调试基础库，不需要 `patch`、绝对路径或恢复 `Cargo.lock`。

### 技术栈

- 后端：Rust 2021（`rust-version = "1.80"`；仓库没有 `rust-toolchain` 文件，1.97.1 工具链由 `scripts/setup_local.ps1` 在本地安装、由 CI 的 `RUST_VERSION` 环境变量固定），axum 0.8、tokio、sqlx (MySQL)、Redis、jsonwebtoken、argon2、lettre (SMTP)、tracing、metrics。
- 前端：React 19 / Vite / Tailwind CSS 4 / shadcn/ui（组件源码入库 `frontend/src/components/ui/`）/ TanStack Query + Table / react-router / zod / Ajv（动态 JSON Schema 校验）/ react-hook-form，TypeScript，pnpm 10.33.1（`frontend/package.json` 的 `packageManager` 字段固定），Node.js 24+。技术选型决策见 `docs/architecture/frontend-rebuild/` ADR 组。浏览器契约：Chrome/Edge ≥ 111、Firefox ≥ 128、Safari ≥ 16.4。
- 依赖服务：MySQL 8.0（`127.0.0.1:3306`）与 Redis 7（`127.0.0.1:6379`），由根目录 `compose.yaml` 提供；`docker/mysql/init/` 会同时创建 `yang_system` 和 `yang_system_test` 两个库。注意 `docker compose down -v` 会永久删除本地数据卷。
- 辅助脚本：Python 3.11+（质量门禁、架构检查、Action 脚手架、本地初始化）。

## 目录结构

```text
src/
├── addon/                   # 业务 Addon；当前只有 account 一个
│   └── account/             # 注册/会话/邮件投递、授权快照、密码重置（domain/）；user/ 是 module 层
│       └── user/            # module 三件套：mod.rs（装配+展示投影）、table.rs（表声明）、actions/（自包含 Action）
├── config/                  # 不可变运行配置、配置源合成（source.rs）
├── infrastructure/          # 审计（audit/）、授权一致性（authorization/）、声明式 Schema（schema.rs）
├── app.rs                   # 所有业务 Addon 的唯一组合根
├── bootstrap.rs             # 配置、Telemetry、Tools、Schema、Worker、HTTP 与关闭顺序
├── lib.rs / main.rs         # main.rs 只是 bootstrap::run("config.toml") 的入口
tests/                       # Rust 集成测试（真实 MySQL/Redis）与 benchmark
examples/frontend_demo/      # 数据库无关的演示后端，供 Playwright 浏览器测试使用
migrations/                  # 空目录：本项目不用 SQL 迁移文件，Schema 由代码声明驱动
scripts/                     # run_ci.py / check_architecture.py / new_action.py / setup_local.ps1 / upgrade_local_config.py
docs/                        # 安全与运行契约：SCHEMA/AUDIT/CONFIGURATION/OBSERVABILITY/SLO/RUNBOOK_BACKUP/LOG_SHIPPING 等
frontend/                    # React 控制台（见下）
ops/prometheus/              # Prometheus 告警规则与演练（CI 用 promtool 校验）
frontend/deploy/             # 生产 Nginx 配置、前端镜像 Dockerfile 与部署契约校验
docker/app/                  # 后端生产镜像 Dockerfile（构建上下文为 lib_yang 仓库根）
```

前端内部约定：API 客户端与会话状态机在 `frontend/src/api/`（SessionController 为纯 TS 会话状态机，`use-session.ts` 是唯一允许 import react 的会话薄壳），契约在 `frontend/src/contracts/`（zod + Ajv 白名单 + OpenAPI 生成类型），业务导航投影在 `frontend/src/catalog/module-pages.ts`，通用解释器在 `frontend/src/renderers/`（table/form/action），shadcn 源码组件在 `frontend/src/components/ui/`，页面编排在 `pages/` 和 `layout/`，应用外壳与路由在 `app/`。需要特殊交互的自定义视图在 `custom/`（`custom/registry.ts` 是静态注册表，禁止按后端字符串动态 import）；单语言产品文案集中在 `lib/product-locale.ts`。`frontend/src/` 只承载生产代码；单元测试集中在 `frontend/tests/`（镜像 src 目录结构，经 `@/` 别名引用被测源码，共享 helper 与 fixture 在 `tests/helpers/`、`tests/fixtures/`，`@test/` 别名指向 `tests/`），Playwright 规格在 `frontend/e2e/` 和 `frontend/e2e-production/`（`*.spec.ts`）。

## 构建、测试与开发命令

本地质量门禁与 `.github/workflows/ci.yml` 共用同一入口 `scripts/run_ci.py`：

- `python scripts/run_ci.py quick` — 提交前：架构检查（含架构检查器、Action 脚手架、本地配置升级三项自检）、`cargo fmt --check`、`cargo test --lib --locked`、前端 typecheck 与 Vitest。
- `python scripts/run_ci.py full` — 推送前：在 quick 之上增加 `cargo test --all-targets --locked`、`cargo clippy --all-targets --all-features --locked -- -D warnings`、前端 `pnpm audit --prod --audit-level moderate`、前端完整 `pnpm check`（format/lint/typecheck/test/locale 契约/build/构建与部署契约校验）、以及端口隔离的 dev-server（前端 5310 / 后端 18310）和 production-build（前端 5311 / 后端 18311）两套 Playwright。
- `python scripts/run_ci.py integration` — 真实依赖集成测试（见“测试策略”）。
- `python scripts/run_ci.py --self-test` — 校验本地门禁与 GitHub Actions workflow 的一致性。

其他常用命令：

- `cargo run --locked` — 启动后端（`http://127.0.0.1:8080`），需要已配置 `config.toml`。Cargo features 只剩 `default = ["account"]`，没有 addon 级 feature 组合。
- `python scripts/check_architecture.py` — 架构门禁：保证 `actions/` 中一个文件只承载一个 Action（恰好一个 `pub(super) async fn handle` + 一个自包含 `pub(super) fn register`）、文件与 `mod.rs` 的 `ACTIONS` 注册表数组一致，裸 SQL 路径有 `// tenant-boundary: <kind> <id>` 边界注释。
- `python scripts/new_action.py <module_actions_dir> <name> --title "..." --method POST --path /api/v1/...` — 新增 Action 脚手架（创建文件、声明 mod、注册）；生成器拒绝覆盖已有文件，生成的代码稳定返回“尚未实现”错误，必须补齐强类型输入、输出和业务逻辑后才算完成。
- `pnpm --dir frontend dev` — 前端开发服务器（`http://localhost:5273`），默认通过 Vite 代理访问 `127.0.0.1:8080` 的真实后端（`VITE_DEV_API_ORIGIN` 可改代理目标；代理禁止开 `changeOrigin`，后端 BrowserSession 同源校验比对 Origin 与 Host）。
- `python scripts/dump_openapi.py` — 导出后端 OpenAPI 3.1 契约快照（`frontend/contracts/openapi.json`）并再生成 TypeScript 类型；后端 Action/输入输出契约变更后必须重跑并提交两个生成物。
- `pnpm --dir frontend check` — 前端完整检查链；单独排查可用 `typecheck` / `lint` / `test` / `e2e` / `e2e:production`。
- 首次环境初始化（Windows PowerShell 7，在 `lib_yang` 仓库根目录执行）：`pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1`，会启动 Compose 依赖、安装前端依赖并生成被 Git 忽略的 `config.toml`；`-CheckOnly` 只检查工具，`-UpgradeLegacyConfig` 迁移旧配置（先备份到 `target/local-config-backups/`）。

## 代码组织与架构约定

- **每个自定义 Action 独占一个文件**，放在所属 Module 的 `actions/` 目录下，形态是「自包含 register」：文件内含输入声明（`params!` 或手写 Input）、恰好一个 `pub(super) async fn handle` 主函数和一个 `pub(super) fn register(module, account)` 自包含注册函数（终结式 builder，spec 与 handler 原子绑定）；`actions/mod.rs` 只保留模块清单和 `ACTIONS` 注册表数组，新增接口加一行 `mod` 声明和一行数组项。禁止在业务代码使用 `#[derive(Action)]` 过程宏（框架内置 CRUD 除外）。业务路径必须显式位于 `/api/v1/`。改动 `actions/` 后必须通过 `python scripts/check_architecture.py`。
- Addon 层目录只承载 `mod.rs`（装配+对外端口）、module 子目录与 `domain/`（addon 级共享机制）；module 层只承载 `mod.rs`（装配+展示投影）、`table.rs`（表声明）和 `actions/`。通用机制（密码哈希、限流、邮箱验证码、浏览器会话 Cookie）由 `yang_base::action::auth` 框架能力提供，应用侧不重复实现。
- 核心链路：`Addon/Module/fields!/params!` → `AppBuilder` → Catalog / Registry / TableDefinition → OpenAPI/UI、HTTP dispatch、Schema。请求处理顺序：auth → authz_version → permission → Step-up guard → Action → 业务事实 + 版本 + Outbox/audit。
- `fields!` 是 Schema、输入输出约束、OpenAPI 和查询策略的唯一字段事实来源；`params!` 同时生成强类型输入与 body/query/path/header 参数契约，请求只反序列化一次。
- `ToolsBuilder -> Tools` 由当前 `BuiltApp` 显式持有 MySQL、Redis、Token 等资源；**禁止引入进程级数据库/Redis/Tools 单例**。
- 当前没有租户域；裸 SQL 路径的边界登记见 `docs/architecture/raw-sql-boundaries.md`。
- 数据库结构由 `src/infrastructure/schema.rs` 的声明统一驱动：启动时先只读计划和旧数据预检，全部安全后才保数据增量同步；冲突会输出表、对象和主键并拒绝启动。**不要新增 SQL 迁移文件**（`migrations/` 为空是有意的）。规则见 `docs/SCHEMA.md`。
- 前端业务导航由后端 Catalog 投影驱动，通用 `ModulePage` 解释 TableView/JSON Schema 表单/操作语义；需要特殊交互的页面必须在 `frontend/src/custom/registry.ts` 静态注册表中显式登记，未登记或加载失败时回退到通用 TableView。**不要根据后端返回的字符串构造动态 import**。

## 代码风格

- Rust 2021，标准 `rustfmt`（四空格缩进）；文件/函数用 `snake_case`，类型用 `PascalCase`，Action 文件名用描述性名称（如 `actions/register.rs`）。
- `Cargo.toml` 已配置：`unsafe_code = "forbid"`、`unused_must_use = "deny"`，Clippy `unwrap_used` / `expect_used` 均为 deny。生产 Rust 代码禁止 `unsafe`、`unwrap()`、`expect()`，用 `anyhow` 传播错误。
- 仓库文档与注释使用中文，新增注释和文档保持中文风格。
- 前端代码必须通过 Prettier、ESLint（`--max-warnings 0`）和 `tsc --noEmit`（strict）；React 组件用 `PascalCase`；产品文案受单语言产品契约门禁（`verify:locale-contract`，在 build 之前执行）约束；首屏 bundle 受 `verify:bundle-budget` 双层预算（目标 350 kB / 硬上限 450 kB gzip）约束。

## 测试策略

- Rust：逻辑旁加 `#[cfg(test)]` 单元测试；`cargo test --lib --locked` 属于 quick 门禁，必须始终通过。
- 前端：改动的契约或状态逻辑要有 `frontend/tests/` 下镜像路径的 `*.test.ts` 回归测试（Vitest）；浏览器行为用 `frontend/e2e/*.spec.ts`（Playwright），E2E 使用 `examples/frontend_demo/` 的无数据库后端，dev-server 与 production-build 两套环境用独立端口隔离，门禁会自行启动演示后端。
- 真实集成测试（`tests/*.rs` 与部分 `--lib --ignored` 测试）要求专用 MySQL 数据库名以 `_test` 结尾、Redis 强制 DB 15，不用 mock 替代：

  ```powershell
  $env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
  $env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
  python scripts/run_ci.py integration
  ```

  覆盖邮箱验证码对抗边界、Refresh 轮换负载基准、Schema 预检/apply 与跨实例并发 apply。集成测试单线程运行（`--test-threads=1`），测试会重建业务测试表与 `b05_schema_*` 专用表。当前 `tests/` 下只有 `registration_email_integration.rs`、`refresh_load_benchmark.rs` 与 `schema_apply_integration.rs` 三个入口。
- 无数值覆盖率门槛，但改变的行为必须有测试覆盖。

## 安全注意事项

- `config.toml` 被 Git 忽略且**禁止提交**；仓库只保留无真实凭据的 `config.example.toml`。配置按 `config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成，契约见 `docs/CONFIGURATION.md`。
- Token、Step-up、邮箱验证码使用各自独立的 keyring/密钥，禁止复用；`config.example.toml` 中的占位密钥会被启动校验拒绝。
- 授权快照（`authz_version`）以 MySQL 为最终事实源，Redis 只做短 TTL 单调加速；授权 writer 必须在同一事务中更新业务事实、单调递增版本并追加 Outbox，Outbox 重放不能让版本回退。
- `security.issue_refresh_credential_version` 是三阶段发布开关，新旧实例混跑时不能提前打开（见 README）。
- 默认不信任任何 `Forwarded`/`X-Forwarded-For`；只为真实反向代理配置最小 `trusted_proxy_cidrs`。
- `app.environment` 支持 `development|test|production`，缺省按 `production` 处理；示例配置仅面向本地开发。
- 高权限操作按契约使用 Step-up 和 append-only 审计（`docs/AUDIT.md`）；Step-up proof 在生产通过 Redis 原子单次消费。

## CI 与部署

- CI（`.github/workflows/ci.yml`）在 push/PR 时运行：quality job 执行 `run_ci.py full` + `run_ci.py integration`，另有用固定版本+摘要的 Nginx 镜像对 `frontend/deploy/nginx.conf` 做只读 `nginx -t`、用固定版本+摘要的 Prometheus 镜像以 promtool 校验 `ops/prometheus/` 规则与告警演练。CI 通过只读 Deploy Key 签出 `lib_yang` 依赖仓库（`persist-credentials: false`）。
- 生产启动：`cargo run --locked --bin yang-system`。启动顺序：配置合成验证 → Telemetry/关闭预算 → 资源创建 → AppBuilder 冻结 Catalog/Registry → Schema 预检与增量同步 → 审计表校验、管理面与 Outbox Worker → readiness 就绪后启动业务路由；停机时先撤销 readiness 再在同一总预算内排空。
- 存活/就绪：`/health/live`、`/health/ready`；独立管理面默认 `http://127.0.0.1:9090`（`/metrics` 与带依赖检查的 `/health/ready`），生产编排应使用管理面 readiness。
- 前端生产部署使用 `frontend/deploy/nginx.conf`（CI 校验语法与部署契约）。
- 生产镜像：后端 `docker/app/Dockerfile`（构建上下文必须是 lib_yang 仓库根：`docker build -f project/yang-system/docker/app/Dockerfile -t yang-system:local .`，配套根目录 `.dockerignore`；工具链与 CI 同为 1.97.1，debian-slim 运行时 + 非 root 用户）；前端 `frontend/deploy/Dockerfile`（构建上下文为 `frontend/`：corepack 按 `packageManager` 固定 pnpm，运行时复用 CI 校验过的同一 Nginx 镜像与 `nginx.conf`）。注意前端 nginx 按契约只监听 loopback，镜像须与后端共享网络命名空间（同 Pod / `--network container:`）运行。
- 备份/恢复与日志聚合运维约定分别见 `docs/RUNBOOK_BACKUP.md`（MySQL 为唯一事实源、Redis 不备份、`down -v` 毁卷警告）与 `docs/LOG_SHIPPING.md`。
- **跨仓库推送顺序**：先推 `lib_yang`、确认推送完成后再推 `yang-system`。CI 在任务开始时按 `LIB_YANG_REF=master` 签出依赖仓库，两边推送间隔过近会拿到旧 master，使旧 `yang-base` 清单与新 `Cargo.lock` 不匹配，`--locked` 直接报 "cannot update the lock file"（2026-08-24 实际踩过，重跑即恢复）。
- **MSRV 1.80 守护**：`.cargo/config.toml` 已配置 `resolver.incompatible-rust-versions = "fallback"`，解析依赖时优先选择兼容 `rust-version = "1.80"` 的版本；新增或升级依赖后必须冷缓存验证 MSRV，不能只信 CI 绿——Swatinem 缓存命中会跳过依赖清单解析，掩盖不兼容（且缓存闲置 7 天会被 GitHub 清除）。验证命令（与 CI 同环境）：

  ```bash
  docker run --rm -v /d/code/lib_yang:/ws -w /ws/project/yang-system \
    -e CARGO_HOME=/tmp/ch -e CARGO_TARGET_DIR=/tmp/ct -e RUSTUP_TOOLCHAIN=1.80.1 \
    rust:1.80.1-slim cargo check --all-targets --locked
  ```

  注意索引里的 `rust_version` 元数据不足以判定兼容性（存在缺元数据但清单声明 `edition2024` 的 crate，如 `ar_archive_writer 0.5.1`），只有用 1.80 实际编译才算数。已知的版本约束：`lettre` 精确锁 `=0.11.19`（0.11.20+ 需要 Rust 1.85）；`async-compression 0.4.33 + compression-codecs 0.4.32` 组合有宏展开缺陷，固定使用 0.4.32 + 0.4.31。

## 提交与 PR

- 遵循 Conventional Commits：`feat(frontend): ...`、`fix(schema): ...`、`refactor(account): ...`，保持提交范围单一。
- PR 需说明行为与验证方式，标注 schema/配置变更，可见的前端变更附截图；请求评审前跑过 `run_ci.py full`。

## 重要参考文档

- `README.md` — 能力清单、本地环境、启动顺序、安全模型的权威说明
- `docs/SCHEMA.md` — 声明式 Schema 演进规则
- `docs/CONFIGURATION.md` — 环境变量、secret provider、keyring 与服务凭据轮换、关闭预算
- `docs/OBSERVABILITY.md` — 指标、readiness、日志与 tracing 契约
- `docs/LOG_SHIPPING.md` — 日志采集接入、字段保留/脱敏与保留期约定
- `docs/RUNBOOK_BACKUP.md` — MySQL 备份/恢复演练、RPO/RTO 与毁卷警告
- `docs/AUDIT.md` — 高权限审计
- `docs/REGISTRATION_EMAIL_VERIFICATION.md` — 注册邮箱验证码边界
- `docs/architecture/` — 授权失效 ADR 与 writer 清单、裸 SQL 边界、会话 TTL
