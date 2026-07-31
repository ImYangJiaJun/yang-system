# yang-system

`yang-system` 是 `yang-base` 唯一原生 Interface 的参考应用，覆盖账号认证、Addon/Module、强类型 Action、Fields/Params、Tables、内部 Action 调用、租户上下文、Schema、Catalog、Registry 与 OpenAPI。

## 本地环境

推荐在 Windows PowerShell 7 中使用仓库提供的初始化脚本。必需工具为 Rustup、
Python 3.11+、已启动的 Docker Desktop、Node.js 24+ 和 Corepack；脚本会安装仓库
固定的 Rust 1.97.1 组件和 `package.json` 固定的 pnpm 10.33.1。

从 `lib_yang` 仓库根目录执行：

```powershell
# 生成 schema.mode = "validate" 的安全配置，不写数据库结构
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1

# 生成/复用配置，并运行版本化迁移与迁移后 Schema 校验
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1 -RunMigrations

# 旧 config.toml 明确授权升级后，再执行迁移与校验
pwsh -NoProfile -File project/yang-system/scripts/setup_local.ps1 `
    -UpgradeLegacyConfig -RunMigrations
```

脚本会启动 Compose 中的 MySQL 8.0 与 Redis 7、等待健康检查、安装前端依赖，
并仅在不存在时生成被 Git 忽略的 `config.toml`。生成配置始终使用 `validate`；
`-RunMigrations` 对新旧配置都执行独立迁移作业，不会把应用启动模式改为 `apply`。
已有配置若缺少当前 JWT keyring、bootstrap、邮箱、授权或可观测字段，脚本默认拒绝覆盖；
`-UpgradeLegacyConfig` 会先备份到 `target/local-config-backups/`，再以当前模板补齐
字段、迁移 `token.secret` 并对齐本仓库 Compose 的 loopback URL。若需要新建
bootstrap 摘要，原始 secret 仍只在当前终端显示一次。
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
`python scripts/run_ci.py quick`，推送前运行 `python scripts/run_ci.py full`；真实依赖
环境准备完成后可运行 `python scripts/run_ci.py integration`。

新增 Action 使用脚手架一次完成文件创建、`mod.rs` 声明和注册；生成器拒绝覆盖
已有文件，并要求业务路径显式位于 `/api/v1/`：

```powershell
python scripts/new_action.py src/modules/org/organization/actions archive `
  --title "归档企业" --method POST --path /api/v1/orgs/archive
```

生成代码会稳定返回“尚未实现”错误，开发者必须补齐强类型输入、输出和业务逻辑后
才能交付，避免脚手架端点被误当作已实现能力。

## BR 生态前端

`frontend/` 使用 Quasar 构建与 BR 生态一致的账号管理入口。前端不维护另一套固定
菜单，而是读取后端 Catalog，将可访问的 Addon/Module 直接投影为页面：

| 后端模块 | 前端页面 | 账号身份 |
|---|---|---|
| `account.user` | 用户中心 | 个人账户 |
| `admin.user` | 平台账号 | 管理平台 |
| `org.tenant` | 我的企业 | 企业账户 |
| `org.org` | 企业资料 | 企业账户 |
| `org.user` | 企业成员 | 企业账户 |

只有 Action、没有 TableView 的模块也会生成页面：列表或详情 Action 负责初始数据，
其余 Action 按 Catalog 中的展示位置生成页面操作或行操作。后端权限投影中不存在的
模块和操作不会出现在前端；直接访问无权模块时同样 fail-closed。

右上角账号菜单对应 BR 的 `user/admin/org` 账号模型。企业身份从
`org.tenant.list` 加载当前用户可访问的企业名称，选择企业后由前端保存其内部
tenant ID 并重新加载 Catalog；用户无需看到或手动输入企业 ID。

本地联调前端：

```powershell
cd frontend
pnpm install
pnpm dev
```

默认通过 Vite 代理访问 `http://127.0.0.1:18080` 的后端。提交前可运行
`pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` 和 `pnpm e2e`。

## 原生结构

```text
src/
├── main.rs
├── bootstrap.rs             # 创建 Tools、同步 Schema、启动 HTTP、优雅关闭
├── app.rs                   # AppBuilder 唯一组装入口
├── transport/http.rs        # Catalog 驱动的 Axum 传输与健康检查
└── modules/
    ├── account/
    │   ├── mod.rs           # account Addon 唯一组装入口
    │   └── user/
    │       ├── mod.rs       # account.user Module 聚合
    │       ├── schema.rs    # 用户表 Schema 与安全 DTO
    │       ├── service.rs   # 跨 Action 共享的领域服务
    │       ├── claims.rs    # Token Claims 到可信用户上下文的投影
    │       └── actions/     # 注册、登录、刷新、退出与当前用户 Action
    └── org/
        ├── mod.rs           # org Addon 唯一组装入口与中间件顺序
        ├── tenant.rs        # 企业成员校验与可信租户解析
        ├── organization/    # org.org Module；每个自定义 Action 独立文件
        └── user/            # org.user Schema、CRUD 与 View
```

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
   OpenAPI HTTP dispatch  Schema/Tables
```

- `ToolsBuilder -> Tools` 由当前 `BuiltApp` 显式持有 MySQL、Redis、Token 等资源；没有进程级数据库/Redis/Tools 单例。
- `params!` 同时生成强类型输入与 body/query/path/header 参数契约，请求只反序列化一次。
- `fields!` 是 Schema、输入输出约束、OpenAPI、后台元数据和查询策略的字段事实来源。
- `ActionLink<I, O>` 在 `AppBuilder::build` 绑定 Registry slot；请求期内部调用不查字符串名称、不做 JSON 往返；完整样例位于 `yang-base` 定义层测试。
- `ctx.tables()` 统一执行字段权限、筛选、排序、分页、关系批量加载与租户条件。
- `org_user.org_org` 是租户键；普通上下文缺少 `TenantContext` 时查询 fail-closed，只有显式 system 上下文可绕过。

## 生产发布与启动顺序

生产发布必须按 `migrate → validate → ready` 执行：

```powershell
cargo run --locked --bin yang-migrate -- plan --config config.toml
cargo run --locked --bin yang-migrate -- apply --config config.toml
cargo run --locked --bin yang-system
```

`plan` 只读，不创建 `_migrations`；`apply` 在数据库级 advisory lock 内执行版本化
清单，复核 checksum/执行状态，并在退出前用当前应用定义完成只读 Schema 校验。
应用的生产配置保持 `app.environment = "production"` 与 `schema.mode = "validate"`；
只有校验成功后才会创建 HTTP 服务，因此 `/health/ready` 不可能早于迁移和校验。
迁移规则、恢复步骤和新增版本流程见 [`docs/MIGRATIONS.md`](docs/MIGRATIONS.md)。

应用进程内部启动顺序为：

1. 按 `config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成并验证不可变运行配置。
2. 创建 MySQL `Database`、Redis `RedisClient` 和 `TokenManager`。
3. 用 `ToolsBuilder` 构建当前应用独占的不可变 `Tools`。
4. 用 `AppBuilder` 校验 Addon 依赖、关系/Action 引用和 route 冲突，冻结 Catalog/Registry。
5. `DatabaseInitializer` 根据 `schema.mode` 对 `BuiltApp::table_definitions()` 执行
   additive 同步、只读校验或显式跳过。
6. 从同一 Catalog 投影 Axum route、OpenAPI 和默认后台 View。
7. 停机时按当前应用资源的生命周期关闭连接。

## 本地启动

初始化脚本执行完成且数据库 Schema 已对齐后，启动后端。全新空库先运行
`setup_local.ps1 -RunMigrations`，或手工执行 `yang-migrate apply`：

```powershell
Set-Location project/yang-system
cargo run --locked
```

后端位于 `http://127.0.0.1:8080`，存活与就绪检查分别位于
`/health/live` 和 `/health/ready`。前端开发服务器位于 `http://127.0.0.1:5173`。

需要手工配置时，先创建 MySQL 数据库和 Redis，再复制配置示例。示例保持安全的
`schema.mode = "validate"`；迁移作业创建业务表，但不会创建数据库本身。

```powershell
Copy-Item config.example.toml config.toml
$tokenBytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
$tokenSecret = [Convert]::ToBase64String($tokenBytes)
# 生成器只在当前终端显示一次原始 bootstrap secret；配置只填写 digest。
cargo run --quiet --locked --bin yang-bootstrap-secret
# 编辑 config.toml：填写 MySQL、Redis、SMTP、独立邮箱验证码密钥与 Token 密钥，
# 再把 bootstrap.secret_digest 替换为生成器输出的 digest；安全保存原始 secret。
cargo run --locked --bin yang-migrate -- apply
cargo run --locked --bin yang-system
```

`config.toml` 被 Git 忽略；仓库只保留不含真实凭据的 `config.example.toml`。MySQL、
Redis、SMTP、邮箱验证码、Token、Bootstrap、Schema 模式等运行参数按
`config.toml < YANG_SYSTEM_* 环境变量 < secret 目录` 合成。
`bootstrap.secret_digest` 只接受带强度边界的 Argon2id PHC 摘要，
原始一次性 secret 不得写入配置、日志或普通响应。缺失、明文、弱参数或非法摘要
都会在连接外部资源前阻止应用启动。部署时应限制 `config.toml` 的读取权限，并通过
部署系统生成或挂载该文件。

注册邮箱验证码的接口、防枚举/重放边界、SMTP/secret provider 配置与真实集成门禁见
[`docs/REGISTRATION_EMAIL_VERIFICATION.md`](docs/REGISTRATION_EMAIL_VERIFICATION.md)。

`admin.user.bootstrap` 仍要求已登录身份，并额外要求请求体携带生成器输出的原始
`secret`。服务端在受并发限制的阻塞线程中执行 Argon2id 常量时间校验；缺失、错误
或未注入 verifier 均失败关闭，只有正确凭证才会进入数据库的一次性初始化事务。

`schema.mode` 支持 `apply|validate|off`。省略整个 `[schema]` 或只省略 `mode` 时均
默认 `validate`：它不执行 DDL，发现任何待应用变更就拒绝启动，适合由独立迁移任务
管理 Schema 的生产环境。`apply` 只保留给本地 Schema 原型调试，不产生版本执行
记录，不得替代 `yang-migrate`；`off` 只应在外部已完成等价校验时使用，二者都必须
显式配置。

`app.environment` 支持 `development|test|production`，缺省时按 `production`
处理。`production` 与 `schema.mode = "apply"` 的组合会在连接数据库前被拒绝；
预发布等面向真实数据的环境也应标记为 `production`。示例配置仅面向本地开发，部署
配置必须显式复核该标识。升级前已有的本地 `apply` 配置还需补充
`app.environment = "development"`，否则会按安全缺省值视为生产环境并拒绝启动。

## 真实依赖集成测试

集成测试要求专用 MySQL 数据库名以 `_test` 结尾，并强制 Redis 使用 DB 15；测试会
重建业务测试表、`_migrations` 与 `b05_schema_*` 专用表：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
python scripts/run_ci.py integration
```

该门禁覆盖迁移 dry-run/version/checksum/幂等与中断重跑、Schema
plan/apply/validate、跨实例并发 apply、锁内失败后跨实例重跑、注册、登录、
refresh、原子创建企业、租户发现和租户作用域查询，不使用 mock 替代 MySQL 或 Redis。

## 参考能力

| 位置 | 展示能力 |
|---|---|
| `modules/account/user/actions/register.rs` | `params!`、类型化 `Action::index`、Fields 约束复用 |
| `modules/account/user/{schema,service,claims}.rs` | 密码保护字段、TableQuery、Token 用户投影 |
| `modules/org/` | 原生 `Addon/Module/actions!`、关系字段、可信租户、Tables 列表与选择器 |
| `app.rs` 测试 | Catalog/Registry 同源、OpenAPI、默认 View、租户 fail-closed |

BR 到 YANG 的完整机械映射和 codemod 使用方式见 `../../docs/br-to-yang-migration.md`。
