# yang-system

一个基于 `yang-base` 的模块化单体基础系统。当前只提供账号、JWT 认证和 HTTP 接口，但边界已经为后续业务模块、单机部署和多机负载均衡统一设计。

## 设计原则

系统先满足四个不可省略的事实：HTTP 请求需要稳定入口，业务需要清晰边界，持久状态必须跨进程共享，数据库结构必须与代码声明一致。因此项目采用以下约束：

- 一个进程、一个可执行程序、一个 `AppRouter`，不是微服务。
- 业务按模块组织；模块拥有 Action、服务、仓储和表声明。
- HTTP 层只负责协议转换，不写账号业务。
- MySQL 保存账号事实，Redis 保存 Token 撤销与轮换状态；应用进程不保存会话状态。
- `config.toml` 是唯一配置文件入口，敏感值通过 `${ENV_NAME}` 注入。
- 不包含 `.sql` 文件。启动监听 HTTP 端口前，`DatabaseInitializer::sync_app_schema` 从模块的 `TableConfig` 增量同步表结构。
- schema 同步只做安全的 additive 变更；已有列的类型、NULL、自增或主键不兼容时直接阻止启动，避免自动破坏数据。

## 项目结构

```text
yang-system/
├── Cargo.toml                  # crate、依赖和 lint 边界
├── config.toml                 # 唯一配置入口，敏感值引用环境变量
├── README.md
└── src/
    ├── main.rs                 # 只解析 APP_CONFIG 并进入启动器
    ├── lib.rs                  # 库模块出口，方便测试与后续复用
    ├── bootstrap.rs            # 按依赖顺序连接资源、同步 schema、启动/关闭 HTTP
    ├── config.rs               # 强类型配置、环境变量展开、启动前校验
    ├── app.rs                  # 组装所有业务模块，生成唯一 AppRouter
    ├── transport/
    │   ├── mod.rs
    │   └── http.rs             # Axum 适配、Catalog 路由投影、错误/健康检查
    └── modules/
        ├── mod.rs
        └── account/
            ├── mod.rs          # 账号模块组合根：Action、route、middleware、table
            ├── entity.rs       # AccountRow 与 accounts 表字段声明
            ├── dto.rs          # 对外输入输出；不暴露 password_hash
            ├── repository.rs   # 仅负责 TableQuery 持久化
            ├── service.rs      # 用户名、密码、状态等业务规则
            ├── password.rs     # Argon2 哈希/校验边界
            └── actions.rs      # 类型化 Action 与认证扩展点
```

### 为什么这样分层

`main.rs` 不应知道数据库表或 HTTP route，否则测试和未来的任务进程都会被可执行入口绑死。`bootstrap.rs` 只处理有顺序的资源生命周期。`app.rs` 只组合模块，所以新增业务通常只需要增加 `modules/<name>` 并在这里注册。

账号内部保持一条单向依赖：

```text
HTTP -> AppRouter/Action -> Service -> Repository -> TableQuery/MySQL
                       \-> TokenManager -> Redis
```

DTO 与实体分离是必要的安全边界：`AccountRow` 含 `password_hash`，`AccountView` 永远不含它。项目没有注册 `yang-base` 的通用表 CRUD Action，避免账号表被通用接口完整读出。

认证路由拆成两个同进程模块：`account_auth` 承载注册、登录、刷新、登出等凭 Token 自证的公开 Action；`account` 承载需要 `TokenAuthMiddleware` 的受保护 Action。它们共享同一个 `AccountService` 和连接池，不是两个服务，也不会产生网络调用。

HTTP route 不是在两处手写：模块把 `RouteDescriptor` 注册进 `AppRouter`，HTTP 适配器再从 `ApiCatalog` 动态生成 Axum route。这样 route、方法、成功状态码和 operation ID 只有一个事实源。

## 启动顺序

1. 读取 `config.toml`，展开环境变量并完成 fail-fast 校验。
2. 初始化 tracing。
3. 创建一个 MySQL 连接池，并以 `Arc<MySqlPool>` 显式共享给仓储。
4. 初始化 `GlobalRedis`；这是 `yang-base` Token 黑名单和刷新令牌轮换的共享状态。
5. 构建 `TokenManager`、账号模块和 `AppRouter`。
6. 调用 `DatabaseInitializer::sync_app_schema(&app_router)`。
7. schema 完全兼容后才监听 HTTP 端口。
8. 收到关闭信号后停止 HTTP，并关闭 Redis/MySQL 连接池。

多个实例同时启动时，基础库使用 MySQL advisory lock 串行化 schema 同步。因此可把同一构建产物部署到多台服务器，再由负载均衡器分发请求；所有实例必须连接同一 MySQL、Redis，并使用同一 Token secret/issuer/audience。

## 本地启动

先创建数据库和 Redis 实例；项目会创建表，但不会创建数据库本身。PowerShell 示例：

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
| `POST` | `/api/v1/accounts/register` | 否 | 注册账号 |
| `POST` | `/api/v1/accounts/login` | 否 | 登录并返回 access/refresh Token |
| `POST` | `/api/v1/accounts/refresh` | 否 | 旋转 refresh Token 并返回新 Token 对 |
| `POST` | `/api/v1/accounts/logout` | Token 自证 | 撤销账号已有 Token |
| `GET` | `/api/v1/accounts/me` | Bearer access Token | 获取当前账号 |
| `GET` | `/health/live` | 否 | 进程存活 |
| `GET` | `/health/ready` | 否 | MySQL 与 Redis 就绪 |

注册与登录示例：

```powershell
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8080/api/v1/accounts/register -ContentType application/json -Body '{"username":"alice","password":"correct-horse-battery-staple"}'
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8080/api/v1/accounts/login -ContentType application/json -Body '{"username":"alice","password":"correct-horse-battery-staple"}'
```

所有业务响应使用 `yang-base::ApiResponse`：成功时 `code = 0`，失败时返回稳定错误码。服务端和基础设施错误不会把底层数据库或 Redis 信息暴露给客户端。

## 扩展新业务

新增 `src/modules/<business>/`，至少声明模块组合根、Action 和表配置；复杂后再按 DTO/Service/Repository 拆分。最后在 `app.rs` 注册 `ModuleRouter`。只要表通过 `with_table_config` 或 `with_schema_table` 挂到模块，启动器就会自动纳入 schema 同步，不需要也不允许新增迁移 SQL 文件。

自动同步适合创建表、增加安全的可空列等 additive 变化。删除列、改类型、收紧 NULL、修改主键等破坏性变更必须先设计显式的数据演进方案，再扩展基础库能力，不能靠启动过程猜测业务意图。
