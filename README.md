<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo_dark.png" />
  <img src="docs/logo_light.png" alt="YANG System 标识" width="160" />
</picture>

# yang-system

`yang-system` 是 `yang-base` 唯一原生 Interface 的参考应用和 React 管理控制台。当前
应用以同一份 Addon/Module 定义构建强类型 Action、Schema、Catalog、Registry、
OpenAPI 与前端页面。骨架以 `account` 为唯一业务 Addon，覆盖账号与会话、
授权失效传播、高权限审计和生产可观测性；另有 `access` 权限管理面与 `feishu` 集成 Addon。生产便签已退役；无数据库演示保留在 `examples/frontend_demo/`。没有企业租户和业务对象域。

## 当前能力

| Addon / Module | 控制台入口 | 当前能力 |
|---|---|---|
| `account.user` | 个人账户 / 用户中心 / 账号设置 | 邮箱验证码注册、登录（用户名/邮箱/邮箱验证码免密）、头像设置与展示、Refresh Cookie、全设备退出、当前用户、账号中心（资料/改密/改用户名/换邮箱/停用/登录设备/安全事件）、自助密码找回；启用凭据版本切换后还提供改密、自助停用、密码重置、匿名化删除；管理动作（grants 化）：停用/启用用户、签发重置凭证、用户列表 |
| `access.grants` | —（预留端口，未接线） | 授权/权限管理端口：grant_permission / revoke_permission / list_permissions / list_user_grants；无冷启动引导，权限管理尚未交付 |
| `feishu.approval` | 飞书审批 / 配置 | 业务名称搜索、启停筛选、独立配置详情、字段对应、同审批关联配置分页、配置 CRUD 与派发记录；管理员可补充名称快照 |

审批配置默认展示来源数据表、审批名称、申请人来源和编号回填位置，技术标识集中在
「排查信息」。旧配置名称暂缺时显示「名称待补充」，有写权限的用户可点击「补充名称」
或「补充本页名称」（每批最多 10 条）。创建及字段映射更新也会补充名称；获取失败保留
旧值。名称只用于展示和搜索，派发仍使用稳定 ID；启用和表单结构缓存时间均不表示
工作流接通或运行成功。实现与验证边界见
[审批配置业务视角设计](docs/architecture/2026-10-12-feishu-approval-config-business-view-design.md)。

安全边界不是只依赖前端隐藏按钮：Access Token 的授权快照通过
`authz_version` 与 MySQL/Redis 当前事实比较，授权 writer 在同一事务中更新业务事实、
单调递增版本并追加 Outbox。全设备退出、自助停用、改密和密码重置按各自契约使用
会话撤销、凭据版本递增或审计事件；框架提供 Step-up 能力，但当前应用未装配。

设备撤销与 Refresh 当前代次以 MySQL `user_session` 为准，授权缓存命中也会检查
稳定会话。无会话标识的旧 Token 需要重新登录；轮换的条件更新成功前不会返回新令牌。
升级兼容性与 Redis 丢失边界见[会话持久化契约](docs/contracts/SESSION_AUTHORITY.md)。

`security.issue_refresh_credential_version` 是三阶段发布开关。本系统无旧实例，
示例配置已为 `true`：同时启用 Refresh 凭据版本签发与依赖新版凭据版本契约的
改密、自助停用和密码重置 Action。接入存量旧部署时需先保持 `false` 兼容读取旧
Refresh Token，确认没有旧实例后才切换为 `true`；该开关不能在新旧实例混跑时提前打开。

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
yang-base    = { path = "../../crates/yang-base", ... }
yang-db      = { path = "../../crates/yang-db", ... }
yang-runtime = { path = "../../crates/yang-runtime", ... }
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
python scripts/new_action.py src/addon/account/user/actions disable_other `
  --title "停用账号" --method POST --path /api/v1/users/disable-other
```

生成代码会稳定返回“尚未实现”错误，开发者必须补齐强类型输入、输出和业务逻辑后
才能交付，避免脚手架端点被误当作已实现能力。

## BR 生态前端

`frontend/` 使用 React 19 + Vite + Tailwind CSS 4 + shadcn/ui 构建正式控制台
（选型决策见 `docs/architecture/frontend-rebuild/` ADR 组；浏览器契约：
Chrome/Edge ≥ 111、Firefox ≥ 128、Safari ≥ 16.4）。登录、注册、密码重置和应用壳由
前端静态维护；登录后的业务导航读取后端 Catalog，将当前身份可访问的 Module 投影
为页面：

| 账号身份 | 后端模块 | 前端页面 |
|---|---|---|
| 个人账户 | `account.user` | 用户中心 |

通用 `ModulePage` 解释 Catalog 中的主 Action、TableView、JSON Schema 表单、关系选项、
工具栏/行/批量操作和确认语义。只有 Action、没有 TableView 的模块也可由主 Action
提供初始数据；需要特殊交互的页面必须在静态自定义视图注册表中显式登记，未登记或
加载失败时回退到通用 TableView。后端权限投影中不存在的模块和操作不会出现在前端，
直接访问无权模块时同样 fail-closed。

当前应用与前端均未装配 Step-up（没有 challenge/proof 对话框）；框架保留的
Step-up 中间件能力未接线，不能把它当作现网防线。`/workbench` 是仅开发环境开放的 Catalog
契约工作台；生产构建使用模块页和业务视图，不把工作台当作正式交付证明。

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
├── addon/                   # account + access（授权端口）+ feishu（集成）
│   ├── account/             # 注册/会话/邮件投递、授权快照与密码重置；user/ 是 module 层
│   │   └── user/            # module 三件套：mod.rs（装配+展示投影）、table.rs（表声明）、actions/（自包含 Action）
│   ├── access/              # 授权端口（预留，无冷启动引导，权限管理未交付）
├── config/                  # 不可变设置、环境变量与 secret 白名单
├── infrastructure/          # 审计、授权一致性与声明式数据库 Schema
├── app.rs                   # 业务 Addon 的唯一组合根
├── bootstrap.rs             # 配置、Telemetry、Tools、Schema、Worker、HTTP 与关闭顺序
├── lib.rs
└── main.rs
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
 auth → authz_version → permission → Action
                                                     │
                                                     ▼
                                       business fact + version + Outbox/audit
```

- 框架另提供 Step-up guard 中间件，但当前应用未装配（没有任何 Action 注册 `step_up_targets`，也不会返回 428）。
- `ToolsBuilder -> Tools` 由当前 `BuiltApp` 显式持有 MySQL、Redis、Token 等资源；没有进程级数据库/Redis/Tools 单例。
- `params!` 同时生成强类型输入与 body/query/path/header 参数契约，请求只反序列化一次。
- `fields!` 是 Schema、输入输出约束、OpenAPI、后台元数据和查询策略的字段事实来源。
- `ActionLink<I, O>` 在 `AppBuilder::build` 绑定 Registry slot；请求期内部调用不查字符串名称、不做 JSON 往返；完整样例位于 `yang-base` 定义层测试。
- `ctx.tables()` 统一执行字段权限、筛选、排序、分页与关系批量加载。
- Token 授权快照同时包含角色、权限与 `authz_version`；Redis 只做短 TTL 加速，MySQL
  保持最终事实源，Outbox 重放不能让版本回退。
- 框架的 Step-up challenge/proof 使用独立 keyring，生产 proof 通过 Redis 原子单次消费；
  当前应用未装配 Step-up，不能把它当作现网保护，它也不能替代 Action 内的事务授权或审计写入。

## 生产发布与启动顺序

生产发布直接启动当前版本：

```powershell
cargo run --locked --bin yang-system
```

应用在创建 HTTP 服务前，以当前 TableDefinition 对全部表执行只读计划和旧数据预检；
全部安全后才增量同步结构。任何冲突都会输出表、对象和主键并拒绝启动，因此
`/health/ready` 不可能早于 Schema 同步。规则见 [`docs/contracts/SCHEMA.md`](docs/contracts/SCHEMA.md)。

应用进程内部启动顺序为：

1. 按 `config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成并验证不可变运行配置。
2. 初始化 JSON 日志与可选 OTLP tracing，并建立整个进程共享的关闭总预算。
3. 创建 MySQL、Redis、Token manager、SMTP sender 和授权缓存（Step-up 配置仍会校验，但应用未创建其 manager）。
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
`http://localhost:5273`。

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

登录/换绑邮箱验证码的防枚举/重放边界、SMTP/secret provider 配置与真实集成门禁见
[`docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md`](docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md)。
公开注册验证码端点已于 2026-10-20 移除（无消费方，且可被匿名用于向任意邮箱发信）。

全新部署时首个成功注册的账号会在同一事务内被引导为 `system_admin`（内置全权组），
后续注册只创建普通账号；日常授权走控制台，手工 SQL 仅用于哨兵被误删等灾备场景。

`app.environment` 支持 `development|test|production`，缺省时按 `production`
处理。示例配置仅面向本地开发，部署配置必须显式复核该标识。

完整环境变量、目录型 secret provider、Token keyring 轮换和关闭预算见
[`docs/contracts/CONFIGURATION.md`](docs/contracts/CONFIGURATION.md)；指标、readiness、日志与 tracing 契约见
[`docs/contracts/OBSERVABILITY.md`](docs/contracts/OBSERVABILITY.md)。

## 真实依赖集成测试

集成测试要求专用 MySQL 数据库名以 `_test` 结尾，并强制 Redis 使用 DB 15；测试会
重建业务测试表与 `b05_schema_*` 专用表：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
python scripts/run_ci.py integration
```

该门禁覆盖登录/MFA 邮箱验证码对抗边界、Refresh 轮换负载基准、Schema 预检/apply 与跨实例
并发 apply，不使用 mock 替代 MySQL 或 Redis。入口为
`tests/refresh_load_benchmark.rs` 和 `tests/schema_apply_integration.rs`。

## 参考能力

| 位置 | 展示能力 |
|---|---|
| `addon/account/user/` | 邮箱注册、Cookie 会话、凭据/授权版本、停用与全量撤销 |
| `infrastructure/authorization/` | Redis 单调缓存、MySQL 回源、事务 Outbox 与发布 Worker |
| `app.rs` 测试 | Catalog/Registry/OpenAPI 同源与条件 Action |

数据库结构由 [`src/infrastructure/schema.rs`](src/infrastructure/schema.rs) 的声明统一驱动，不依赖 SQL 迁移文件或源码 codemod。

安全与运行契约继续拆分在专门文档中：授权失效见
[`docs/architecture/authorization-freshness-adr.md`](docs/architecture/authorization-freshness-adr.md)，
裸 SQL 边界见
[`docs/architecture/raw-sql-boundaries.md`](docs/architecture/raw-sql-boundaries.md)，
高权限审计见 [`docs/contracts/AUDIT.md`](docs/contracts/AUDIT.md)，Schema 演进见
[`docs/contracts/SCHEMA.md`](docs/contracts/SCHEMA.md)，注册邮件见
[`docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md`](docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md)，SLO 见
[`docs/contracts/SLO.md`](docs/contracts/SLO.md)。
