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
- `ActionLink<I, O>` 在 `AppBuilder::build` 绑定 Registry slot；请求期内部调用不查字符串名称、不做 JSON 往返。
- `ctx.tables()` 统一执行字段权限、筛选、排序、分页、关系批量加载与租户条件。
- `org_user.org_org` 是租户键；普通上下文缺少 `TenantContext` 时查询 fail-closed，只有显式 system 上下文可绕过。

## 启动顺序

1. 读取 `config.toml`，再用 `DATABASE_URL`、`REDIS_URL`、`TOKEN_SECRET` 做结构化覆盖。
2. 创建 MySQL `Database`、Redis `RedisClient` 和 `TokenManager`。
3. 用 `ToolsBuilder` 构建当前应用独占的不可变 `Tools`。
4. 用 `AppBuilder` 校验 Addon 依赖、关系/Action 引用和 route 冲突，冻结 Catalog/Registry。
5. `DatabaseInitializer` 根据 `BuiltApp::table_definitions()` 执行 additive Schema 同步。
6. 从同一 Catalog 投影 Axum route、OpenAPI 和默认后台 View。
7. 停机时按当前应用资源的生命周期关闭连接。

## 本地启动

先创建数据库和 Redis；应用会创建缺失表和列，但不会创建数据库本身。

```powershell
Copy-Item config.example.toml config.toml
$env:DATABASE_URL = "mysql://root:password@127.0.0.1:3306/yang_system"
$env:REDIS_URL = "redis://127.0.0.1:6379"
$tokenBytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
$env:TOKEN_SECRET = [Convert]::ToBase64String($tokenBytes)
cargo run
```

`config.toml` 被 Git 忽略；仓库只保留不含真实凭据的 `config.example.toml`。环境变量按字段覆盖，不会作为 TOML 源码插值，因此 URL 或密钥中的引号和反斜杠不会破坏配置解析。

其它配置文件：

```powershell
$env:APP_CONFIG = "D:\config\yang-system.toml"
$env:DATABASE_URL = "mysql://app:password@db.internal:3306/yang_system"
$env:REDIS_URL = "redis://cache.internal:6379"
$env:TOKEN_SECRET = "从密钥管理服务注入的随机密钥"
cargo run
```

## 参考能力

| 位置 | 展示能力 |
|---|---|
| `modules/account/user/actions/register.rs` | `params!`、类型化 `Action::index`、Fields 约束复用 |
| `modules/account/user/actions/register_via_plugin.rs` | `ActionLink`、构建期绑定、`ctx.plugins()` 零 JSON 内部调用 |
| `modules/account/user/{schema,service,claims}.rs` | 密码保护字段、TableQuery、Token 用户投影 |
| `modules/org/` | 原生 `Addon/Module/actions!`、关系字段、可信租户、Tables 列表与选择器 |
| `app.rs` 测试 | Catalog/Registry 同源、OpenAPI、默认 View、租户 fail-closed |

BR 到 YANG 的完整机械映射和 codemod 使用方式见 `../../docs/br-to-yang-migration.md`。
