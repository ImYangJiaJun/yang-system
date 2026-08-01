# yang-system

`yang-system` 是 `yang-base` 唯一原生 Interface 的参考应用和 Quasar 管理控制台。当前
应用以同一份 Addon/Module 定义构建强类型 Action、Schema、Catalog、Registry、
OpenAPI 与前端页面，覆盖账号与会话、平台管理、企业成员、个人任务、授权失效传播、
高权限审计和生产可观测性。

## 当前能力

| Addon / Module | 控制台入口 | 当前能力 |
|---|---|---|
| `account.user` | 个人账户 / 用户中心 | 邮箱验证码注册、登录、Refresh Cookie、全设备退出、当前用户、Step-up；启用凭据版本切换后还提供改密、自助停用和密码重置 |
| `admin.user` | 管理平台 / 平台账号 | 首个注册账号原子成为最终管理员；账号查询与新增、状态和超级管理员变更；启用凭据版本切换后可签发单次密码重置凭证 |
| `org.tenant` | 企业账号 / 我的企业 | 发现可访问企业、原子创建企业与首位管理员 |
| `org.org` / `org.user` | 企业资料 / 企业成员 | 当前企业资料、成员查询与增改删、关系选择和租户隔离 |
| `work.project` / `work.task` | 个人账户 / 项目组合、任务规划 | 个人项目、任务树与分页清单、关系选择、批量完成 |
| `system.observability` | 无导航入口 | 已认证的浏览器错误关联上报 |

安全边界不是只依赖前端隐藏按钮：Access Token 的授权快照通过
`authz_version` 与 MySQL/Redis 当前事实比较，授权 writer 在同一事务中更新业务事实、
单调递增版本并追加 Outbox；企业成员写操作还会在持锁事务内复核资源授权。全设备退出、
自助停用、平台账号新增/状态/管理员变更和企业成员增改删按各自契约使用 Step-up、
最后管理员保护、会话撤销或审计事件。

`security.issue_refresh_credential_version` 是三阶段发布开关。示例配置保持 `false`，
此时服务兼容读取旧 Refresh Token，且不会注册依赖新版凭据版本契约的改密、自助停用、
用户密码重置和管理员重置凭证 Action；确认没有旧实例后切换为 `true`，才同时启用
Refresh 凭据版本签发和这些 Action。该开关不能在新旧实例混跑时提前打开。

## 本地环境

推荐在 Windows PowerShell 7 中使用仓库提供的初始化脚本。必需工具为 Rustup、
Python 3.11+、已启动的 Docker Desktop、Node.js 24+ 和 Corepack；脚本会安装仓库
固定的 Rust 1.97.1 组件和 `package.json` 固定的 pnpm 10.33.1。

从 `lib_yang` 仓库根目录执行：

```powershell
# 生成安全配置；后端启动时自动预检旧数据并同步数据库结构
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1

# 旧 config.toml 明确授权升级
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1 -UpgradeLegacyConfig
```

脚本会启动 Compose 中的 MySQL 8.0 与 Redis 7、等待健康检查、安装前端依赖，
并仅在不存在时生成被 Git 忽略的 `config.toml`。
已有配置若缺少当前 JWT keyring、邮箱、授权或可观测字段，脚本默认拒绝覆盖；
`-UpgradeLegacyConfig` 会先备份到 `target/local-config-backups/`，再以当前模板补齐
字段、迁移 `token.secret` 并对齐本仓库 Compose 的 loopback URL。
只检查必需工具时使用：

```powershell
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1 -CheckOnly
```

依赖服务也可以在本目录手工管理：

```powershell
docker compose up -d --wait
docker compose ps
docker compose down
```

MySQL 监听 `127.0.0.1:3306`，Redis 监听 `127.0.0.1:6379`。普通停止会保留数据；
`docker compose down -v` 会永久删除本地 MySQL 与 Redis 数据卷，只应在需要彻底
重置开发数据时执行。

## 相对路径联合调试

本项目是独立 Git/Cargo 项目，根 workspace 显式排除它。`Cargo.toml` 直接使用：

```toml
yang-base = { path = "../../crates/yang-base", ... }
yang-db = { path = "../../crates/yang-db", ... }
```

因此在 `project/yang-system` 目录执行普通 Cargo 命令，就会直接编译同一份 `lib_yang` 工作树中的基础库修改：

```powershell
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

联合调试不需要 Git revision、Cargo `patch`、绝对路径或调试后恢复 `Cargo.lock`。

提交前运行架构门禁，保证 `actions/` 中一个文件只承载一个 Action，且文件与
`mod.rs` 清单一致：

```powershell
python scripts/check_architecture.py
```

检查器自身的反向 fixture 可用 `python scripts/check_architecture.py --self-test`
验证。

本地质量门禁与 `.github/workflows/ci.yml` 共用同一入口：提交前运行
`python scripts/run_ci.py quick`，推送前运行 `python scripts/run_ci.py full`；`full`
还会执行生产依赖审计、前端完整检查，以及端口隔离的 dev-server 和 production-build
两套 Playwright。真实依赖环境准备完成后可运行
`python scripts/run_ci.py integration`。

新增 Action 使用脚手架一次完成文件创建、`mod.rs` 声明和注册；生成器拒绝覆盖
已有文件，并要求业务路径显式位于 `/api/v1/`：

```powershell
python scripts/new_action.py src/modules/org/organization/actions archive `
  --title "归档企业" --method POST --path /api/v1/orgs/archive
```

生成代码会稳定返回“尚未实现”错误，开发者必须补齐强类型输入、输出和业务逻辑后
才能交付，避免脚手架端点被误当作已实现能力。

## BR 生态前端

`frontend/` 使用 Quasar 构建正式控制台。登录、注册、密码重置、身份选择和应用壳由
前端静态维护；登录后的业务导航读取后端 Catalog，将当前身份可访问的 Module 投影
为页面：

| 账号身份 | 后端模块 | 前端页面 |
|---|---|---|
| 个人账户 | `account.user` | 用户中心 |
| 个人账户 | `work.project` | 项目组合 |
| 个人账户 | `work.task` | 任务规划 |
| 企业账号 | `org.tenant` | 我的企业 |
| 企业账号 | `org.org` | 企业资料 |
| 企业账号 | `org.user` | 企业成员 |
| 管理平台 | `admin.user` | 平台账号 |

通用 `ModulePage` 解释 Catalog 中的主 Action、TableView、JSON Schema 表单、关系选项、
工具栏/行/批量操作和确认语义。只有 Action、没有 TableView 的模块也可由主 Action
提供初始数据；需要特殊交互的页面必须在静态自定义视图注册表中显式登记，未登记或
加载失败时回退到通用 TableView。后端权限投影中不存在的模块和操作不会出现在前端，
直接访问无权模块时同样 fail-closed。

右上角账号菜单对应 `user/admin/org` 身份模型。企业身份从
`org.tenant.list` 加载当前用户可访问的企业名称，选择企业后由前端保存其内部
tenant ID 并重新加载 Catalog；用户无需看到或手动输入企业 ID。

后端返回 Step-up challenge 时，通用操作链会打开重认证对话框，用当前账号凭据完成
challenge 后携带一次性 proof 重试原操作。`/workbench` 是仅开发环境开放的 Catalog
契约工作台；生产构建使用身份选择、模块页和业务视图，不把工作台当作正式交付证明。

本地联调前端：

```powershell
cd frontend
pnpm install
pnpm dev
```

`pnpm dev` 默认通过 Vite 代理访问 `http://127.0.0.1:8080` 的真实后端；Playwright
门禁会自行启动数据库无关的 `examples/frontend_demo/`，并用独立端口隔离 dev-server
与 production-build 两套环境。提交前优先从仓库根目录运行
`python scripts/run_ci.py full`；单独排查前端时可运行 `pnpm check`、`pnpm e2e` 和
`pnpm e2e:production`。

## 原生结构

```text
src/
├── main.rs
├── bootstrap.rs             # 配置、Telemetry、Tools、Schema、Worker、HTTP 与关闭顺序
├── app.rs                   # account/admin/system/org/work 的唯一组合根
├── authorization/           # 授权版本缓存、事务 Outbox 与后台发布 Worker
├── audit/                   # append-only 高权限审计事件与启动期 Schema 校验
├── config.rs                # 不可变运行设置及字段级校验
├── config_source.rs         # 本应用环境变量与 secret 白名单，合成机制来自 yang-runtime
└── modules/
    ├── account/              # 注册/会话/邮件投递、授权快照与用户生命周期
    ├── admin/                # 首个注册账号的唯一最终管理员与平台授权保护
    ├── observability/        # 浏览器错误与服务端 request_id 关联
    ├── org/                  # 企业创建/选择、可信租户解析与成员管理
    └── work/                 # 个人项目、任务树、关系与批量完成
```

JSON 日志、Prometheus/OTLP、共享关闭预算和配置源合成由根 workspace 的
`yang-runtime` 提供；可信代理客户端 IP 中间件位于 `yang-base::transport`，应用只保留
指标名、环境变量、secret 和关闭阶段等系统策略。

核心路径只有一条：

```text
Addon/Module/fields!/params!
            │
            ▼
        AppBuilder
       ┌────┼──────────────┐
       ▼    ▼              ▼
    Catalog Registry   TableDefinition/View
       │    │              │
       ▼    ▼              ▼
 OpenAPI/UI  HTTP dispatch  Schema/Tables
                 │
                 ▼
 auth → authz_version → tenant/permission → Step-up/resource guard → Action
                                                               │
                                                               ▼
                                              business fact + version + Outbox/audit
```

- `ToolsBuilder -> Tools` 由当前 `BuiltApp` 显式持有 MySQL、Redis、Token 等资源；没有进程级数据库/Redis/Tools 单例。
- `params!` 同时生成强类型输入与 body/query/path/header 参数契约，请求只反序列化一次。
- `fields!` 是 Schema、输入输出约束、OpenAPI、后台元数据和查询策略的字段事实来源。
- `ActionLink<I, O>` 在 `AppBuilder::build` 绑定 Registry slot；请求期内部调用不查字符串名称、不做 JSON 往返；完整样例位于 `yang-base` 定义层测试。
- `ctx.tables()` 统一执行字段权限、筛选、排序、分页、关系批量加载与租户条件。
- `org_user.org_org` 是租户键；普通上下文缺少 `TenantContext` 时查询 fail-closed，只有显式 system 上下文可绕过。
- Token 授权快照同时包含角色、权限与 `authz_version`；Redis 只做短 TTL 加速，MySQL
  保持最终事实源，Outbox 重放不能让版本回退。
- Step-up challenge/proof 使用独立 keyring，生产 proof 通过 Redis 原子单次消费；它不
  替代 Action 内的事务授权、最后管理员保护或审计写入。

## 生产发布与启动顺序

生产发布直接启动当前版本：

```powershell
cargo run --locked --bin yang-system
```

应用在创建 HTTP 服务前，以当前 TableDefinition 对全部表执行只读计划和旧数据预检；
全部安全后才增量同步结构。任何冲突都会输出表、对象和主键并拒绝启动，因此
`/health/ready` 不可能早于 Schema 同步。规则见 [`docs/SCHEMA.md`](docs/SCHEMA.md)。

应用进程内部启动顺序为：

1. 按 `config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成并验证不可变运行配置。
2. 初始化 JSON 日志与可选 OTLP tracing，并建立整个进程共享的关闭总预算。
3. 创建 MySQL、Redis、Token/Step-up manager、SMTP sender 和授权缓存。
4. 用 `ToolsBuilder` 冻结当前应用独占资源，再由 `AppBuilder` 校验 Addon 依赖、
   关系/Action 引用和 route 冲突，冻结 Catalog/Registry。
5. `DatabaseInitializer` 对完整 TableDefinition 集合执行全局预检和保数据增量同步。
6. 校验 append-only 审计表，启动独立 Prometheus/readiness 管理面和授权 Outbox Worker。
7. readiness 置为就绪后，从同一 Catalog 投影并启动 Axum 业务路由。
8. 停机时先撤销 readiness，再在同一总预算内排空 HTTP、停止 Worker、关闭资源并
   flush Telemetry。

## 本地启动

初始化脚本执行完成后启动后端；全新空库会自动建表，旧库会先预检再增量同步：

```powershell
Set-Location project/yang-system
cargo run --locked
```

后端位于 `http://127.0.0.1:8080`，存活与就绪检查分别位于
`/health/live` 和 `/health/ready`。启用指标时，独立管理面默认位于
`http://127.0.0.1:9090`，提供 `/metrics` 与带生命周期门闩、MySQL/Redis 检查和总预算
的 `/health/ready`；生产编排应使用这个管理面 readiness。前端开发服务器位于
`http://127.0.0.1:5173`。

需要手工配置时，先创建 MySQL 数据库和 Redis，再复制配置示例。应用只管理所选
数据库内的表结构，不创建数据库本身。

```powershell
Copy-Item config.example.toml config.toml
$tokenBytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
$tokenSecret = [Convert]::ToBase64String($tokenBytes)
# 编辑 config.toml：填写 MySQL、Redis、SMTP、独立邮箱验证码密钥与 Token 密钥。
cargo run --locked --bin yang-system
```

`config.toml` 被 Git 忽略；仓库只保留不含真实凭据的 `config.example.toml`。MySQL、
Redis、SMTP、邮箱验证码、Token 等运行参数按
`config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成。
部署时应限制 `config.toml` 的读取权限，并通过部署系统生成或挂载该文件。

注册邮箱验证码的接口、防枚举/重放边界、SMTP/secret provider 配置与真实集成门禁见
[`docs/REGISTRATION_EMAIL_VERIFICATION.md`](docs/REGISTRATION_EMAIL_VERIFICATION.md)。

首个成功提交注册事务的账号会通过数据库唯一 `owner_key=system-owner` 哨兵原子成为
唯一且不可降级、停用或删除的系统最终管理员。已有用户却没有 owner、存在多个 owner
或 owner 状态损坏时启动失败并要求人工修复。首次注册前必须使用本机监听、防火墙或
反向代理限制不可信访问；没有外部身份凭证时，系统无法判断哪个公网注册者是预期所有者。

`app.environment` 支持 `development|test|production`，缺省时按 `production`
处理。示例配置仅面向本地开发，部署配置必须显式复核该标识。

完整环境变量、目录型 secret provider、Token/Step-up keyring 轮换和关闭预算见
[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md)；指标、readiness、日志与 tracing 契约见
[`docs/OBSERVABILITY.md`](docs/OBSERVABILITY.md)。

## 真实依赖集成测试

集成测试要求专用 MySQL 数据库名以 `_test` 结尾，并强制 Redis 使用 DB 15；测试会
重建业务测试表与 `b05_schema_*` 专用表：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
python scripts/run_ci.py integration
```

该门禁覆盖授权缓存单调性与 Outbox 并发重放、迁移
dry-run/version/checksum/幂等与中断重跑、Schema plan/apply/validate、跨实例并发
apply、审计/最终管理员信任根、邮箱验证码对抗边界、注册/登录/Refresh/会话失效、
原子创建企业、租户隔离和业务系统路径，不使用 mock 替代 MySQL 或 Redis。

## 参考能力

| 位置 | 展示能力 |
|---|---|
| `modules/account/user/` | 邮箱注册、Cookie 会话、凭据/授权版本、Step-up、停用与全量撤销 |
| `authorization/` | Redis 单调缓存、MySQL 回源、事务 Outbox 与发布 Worker |
| `modules/admin/` | 首个注册最终管理员、权限快照、最后管理员保护与密码重置凭证 |
| `modules/org/` | 原子建企业、可信租户、成员资源授权和事务内最终复核 |
| `modules/work/` | 个人租户、项目/任务关系、树与分页 View、批量完成 |
| `app.rs` 测试 | Catalog/Registry/OpenAPI 同源、条件 Action、权限、View 与租户 fail-closed |

BR 到 YANG 的完整机械映射和 codemod 使用方式见 `../../docs/br-to-yang-migration.md`。

安全与运行契约继续拆分在专门文档中：授权失效见
[`docs/architecture/authorization-freshness-adr.md`](docs/architecture/authorization-freshness-adr.md)，
高权限审计见 [`docs/AUDIT.md`](docs/AUDIT.md)，Schema 演进见
[`docs/SCHEMA.md`](docs/SCHEMA.md)，注册邮件见
[`docs/REGISTRATION_EMAIL_VERIFICATION.md`](docs/REGISTRATION_EMAIL_VERIFICATION.md)，SLO 见
[`docs/SLO.md`](docs/SLO.md)。
