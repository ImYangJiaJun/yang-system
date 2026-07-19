# yang-system

`yang-system` 是 `yang-base` 唯一原生 Interface 的参考应用，覆盖账号认证、Addon/Module、强类型 Action、Fields/Params、Tables、内部 Action 调用、租户上下文、Schema、Catalog、Registry 与 OpenAPI。

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

## 启动顺序

1. 只读取 `config.toml`；运行配置不接受环境变量覆盖。
2. 创建 MySQL `Database`、Redis `RedisClient` 和 `TokenManager`。
3. 用 `ToolsBuilder` 构建当前应用独占的不可变 `Tools`。
4. 用 `AppBuilder` 校验 Addon 依赖、关系/Action 引用和 route 冲突，冻结 Catalog/Registry。
5. `DatabaseInitializer` 根据 `schema.mode` 对 `BuiltApp::table_definitions()` 执行
   additive 同步、只读校验或显式跳过。
6. 从同一 Catalog 投影 Axum route、OpenAPI 和默认后台 View。
7. 停机时按当前应用资源的生命周期关闭连接。

## 本地启动

先创建数据库和 Redis；应用会创建缺失表和列，但不会创建数据库本身。

```powershell
Copy-Item config.example.toml config.toml
$tokenBytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
$tokenSecret = [Convert]::ToBase64String($tokenBytes)
# 编辑 config.toml：填写 mysql.url、redis.url，并把 token.secret 替换为 $tokenSecret
cargo run
```

`config.toml` 被 Git 忽略；仓库只保留不含真实凭据的 `config.example.toml`。MySQL、
Redis、Token、Schema 模式等运行参数均以该文件为准；修改环境变量不会覆盖配置。
部署时应限制 `config.toml` 的读取权限，并通过部署系统生成或挂载该文件。

`schema.mode` 支持 `apply|validate|off`：`apply` 适合本地开发；`validate` 不执行 DDL，
发现任何待应用变更就拒绝启动，适合由独立迁移任务管理 schema 的生产环境；`off`
只应在外部已完成等价校验时显式使用。

## 真实依赖集成测试

集成测试要求专用 MySQL 数据库名以 `_test` 结尾，并强制 Redis 使用 DB 15；测试会
重建 `users`、`org_org`、`org_user` 三张测试表：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:password@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
cargo test --test system_integration -- --ignored --test-threads=1
```

该测试覆盖 schema plan/apply/validate、注册、登录、refresh、原子创建企业、租户发现
和租户作用域查询，不使用 mock 替代 MySQL 或 Redis。

## 参考能力

| 位置 | 展示能力 |
|---|---|
| `modules/account/user/actions/register.rs` | `params!`、类型化 `Action::index`、Fields 约束复用 |
| `modules/account/user/{schema,service,claims}.rs` | 密码保护字段、TableQuery、Token 用户投影 |
| `modules/org/` | 原生 `Addon/Module/actions!`、关系字段、可信租户、Tables 列表与选择器 |
| `app.rs` 测试 | Catalog/Registry 同源、OpenAPI、默认 View、租户 fail-closed |

BR 到 YANG 的完整机械映射和 codemod 使用方式见 `../../docs/br-to-yang-migration.md`。
