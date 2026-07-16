# yang-system

一个基于 `yang-base` 的模块化单体基础系统。当前只提供用户、JWT 认证和 HTTP 接口，但边界已经为后续业务模块、单机部署和多机负载均衡统一设计。

## 仓库位置与联合调试

本项目是独立 Git/Cargo 项目。放在 `lib_yang/project/yang-system` 只是为了方便联合调试，根 workspace 会显式排除它。当前应用需要尚未发布到 crates.io 的 `yang-base 0.1.3` API，因此 `Cargo.toml` 通过 SSH 固定到私有 `lib_yang` 的 Git revision；单独 clone 后，具有该仓库 SSH 权限的环境可直接构建和运行。

独立开发时直接在项目根目录运行：

```powershell
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

放在 `lib_yang/project` 下联合调试未发布的基础库修改时，可临时覆盖 crates.io 依赖；该命令不会把本地 path 写入 `Cargo.toml`：

```powershell
cargo --config 'patch."ssh://git@github.com/ImYangJiaJun/lib_yang.git".yang-base.path="../../crates/yang-base"' `
      --config 'patch."ssh://git@github.com/ImYangJiaJun/lib_yang.git".yang-db.path="../../crates/yang-db"' `
      test --all-targets
git restore Cargo.lock
```

临时 patch 会改写锁文件中的依赖来源，因此联调完成后要恢复 `Cargo.lock`。`yang-base 0.1.3` 发布后，应把 Git revision 依赖切回 crates.io 版本约束，避免长期依赖仓库提交。

## 设计原则

系统先满足四个不可省略的事实：HTTP 请求需要稳定入口，业务需要清晰边界，持久状态必须跨进程共享，数据库结构必须与代码声明一致。因此项目采用以下约束：

- 一个进程、一个可执行程序、一个 `AppRouter`，不是微服务。
- 业务按模块文件夹组织；叶子模块内部只放平级文件，不再建立子文件夹。
- 一个接口对应一个文件；接口文件同时声明 Action 与 RouteDescriptor，避免实现和路由分散。
- 文件不是架构层。模块共享的实体、服务、持久化和安全逻辑先收敛在 `mod.rs`，出现独立变化边界后再拆成平级文件。
- HTTP 层只负责协议转换，不写用户业务。
- MySQL 保存用户事实，Redis 保存 Token 撤销与轮换状态；应用进程不保存会话状态。
- `config.toml` 是唯一配置文件入口，敏感值通过 `${ENV_NAME}` 注入。
- 不包含 `.sql` 文件。启动监听 HTTP 端口前，`DatabaseInitializer::sync_app_schema` 从模块的 `TableConfig` 增量同步表结构。
- schema 同步只做安全的 additive 变更；已有列的类型、NULL、自增或主键不兼容时直接阻止启动，避免自动破坏数据。

## 项目结构

```text
lib_yang/
├── crates/                     # yang-base、yang-db 等基础库
└── project/
    └── yang-system/
        ├── Cargo.toml          # crate、本地基础库依赖和 lint 边界
        ├── .cargo/config.toml  # 通过系统 Git CLI 获取私有基础库依赖
        ├── config.toml         # 唯一配置入口，敏感值引用环境变量
        ├── README.md
        └── src/
            ├── main.rs         # 只解析 APP_CONFIG 并进入启动器
            ├── lib.rs          # 库模块出口，方便测试与后续复用
            ├── bootstrap.rs    # 连接资源、同步 schema、启动/关闭 HTTP
            ├── config.rs       # 强类型配置、环境变量展开、启动前校验
            ├── app.rs          # 组装所有业务模块，生成唯一 AppRouter
            ├── transport/http/ # HTTP 生命周期、Catalog 投影和健康检查
            └── modules/user/   # 用户接口、共享实体、服务、持久化和安全逻辑
```

### 为什么这样分层

`main.rs` 不应知道数据库表或 HTTP route，否则测试和未来的任务进程都会被可执行入口绑死。`bootstrap.rs` 只处理有顺序的资源生命周期。`app.rs` 只组合模块，所以新增业务通常只需要增加 `modules/<name>` 并在这里注册。

用户模块内部保持一条单向依赖，但不为每一层建立文件：

```text
HTTP -> AppRouter -> <interface>.rs -> UserService -> TableQuery/MySQL
                                      \-> TokenManager -> Redis
```

DTO 与实体仍保持类型边界：`UserRow` 含 `password_hash`，`UserView` 永远不含它；它们是否位于不同文件不影响这一安全属性。项目没有注册 `yang-base` 的通用表 CRUD Action，避免用户表被通用接口完整读出。

`user` 是唯一用户领域文件夹。受基础库模块级认证中间件约束，组合根内部生成两个同进程运行时路由器：`user_auth` 承载注册、登录、刷新、登出等公开或凭 Token 自证的 Action；`user` 承载需要 `TokenAuthMiddleware` 的受保护 Action。它们共享同一个 `UserService` 和连接池，不是两个业务模块，也不会产生网络调用。

HTTP route 不是在两处手写：每个 `<interface>.rs` 把 Action 与 `RouteDescriptor` 一起注册进 `AppRouter`，HTTP 适配器再从 `ApiCatalog` 动态生成 Axum route。这样接口实现、路径、方法、成功状态码和 operation ID 都在同一个文件中维护。

## 启动顺序

1. 读取 `config.toml`，展开环境变量并完成 fail-fast 校验。
2. 初始化 tracing。
3. 创建一个 MySQL 连接池，并以 `Arc<MySqlPool>` 显式共享给用户模块。
4. 初始化 `GlobalRedis`；这是 `yang-base` Token 黑名单和刷新令牌轮换的共享状态。
5. 构建 `TokenManager`、用户模块和 `AppRouter`。
6. 调用 `DatabaseInitializer::sync_app_schema(&app_router)`。
7. schema 完全兼容后才监听 HTTP 端口。
8. 收到关闭信号后停止 HTTP，并关闭 Redis/MySQL 连接池。

多个实例同时启动时，基础库使用 MySQL advisory lock 串行化 schema 同步。因此可把同一构建产物部署到多台服务器，再由负载均衡器分发请求；所有实例必须连接同一 MySQL、Redis，并使用同一 Token secret/issuer/audience。

## 本地启动

先创建数据库和 Redis 实例；项目会创建表，但不会创建数据库本身。在 `project/yang-system` 目录启动的 PowerShell 示例：

```powershell
$env:DATABASE_URL = "mysql://root:password@127.0.0.1:3306/yang_system"
$env:REDIS_URL = "redis://127.0.0.1:6379"
$env:TOKEN_SECRET = "replace-with-at-least-32-random-bytes"
cargo run
```

使用其它配置路径：

```powershell
$env:APP_CONFIG = "D:\config\yang-system.toml"
cargo run
```

## HTTP API

| 方法 | 路径 | 认证 | 说明 |
|---|---|---|---|
| `POST` | `/api/v1/users/register` | 否 | 注册用户 |
| `POST` | `/api/v1/users/login` | 否 | 登录并返回 access/refresh Token |
| `POST` | `/api/v1/users/refresh` | 否 | 旋转 refresh Token 并返回新 Token 对 |
| `POST` | `/api/v1/users/logout` | Token 自证 | 撤销用户已有 Token |
| `GET` | `/api/v1/users/me` | Bearer access Token | 获取当前用户 |
| `GET` | `/health/live` | 否 | 进程存活 |
| `GET` | `/health/ready` | 否 | MySQL 与 Redis 就绪 |

注册与登录示例：

```powershell
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8080/api/v1/users/register -ContentType application/json -Body '{"username":"alice","password":"correct-horse-battery-staple"}'
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8080/api/v1/users/login -ContentType application/json -Body '{"username":"alice","password":"correct-horse-battery-staple"}'
```

所有业务响应使用 `yang-base::ApiResponse`：成功时 `code = 0`，失败时返回稳定错误码。服务端和基础设施错误不会把底层数据库或 Redis 信息暴露给客户端。

## 扩展新业务

新增 `src/modules/<business>/`，至少声明 `mod.rs` 和接口文件；每增加一个接口就新增一个平级 `<interface>.rs`，由该文件共同维护 Action 与 RouteDescriptor。共享逻辑先放在 `mod.rs`；只有出现第二种实现、独立复用、独立测试替身或不同变化周期时，才拆成新的平级文件，不建立子文件夹。最后在 `app.rs` 注册 `ModuleRouter`。只要表通过 `with_table_config` 或 `with_schema_table` 挂到模块，启动器就会自动纳入 schema 同步，不需要也不允许新增迁移 SQL 文件。

自动同步适合创建表、增加安全的可空列等 additive 变化。删除列、改类型、收紧 NULL、修改主键等破坏性变更必须先设计显式的数据演进方案，再扩展基础库能力，不能靠启动过程猜测业务意图。
