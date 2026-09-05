# 新业务 Addon 接入手册

**生成：** 2026-09-02
**范围：** 以 P7 演示 Addon `src/addon/demo/`（便签 notes）为实例，给出"新增一个
业务 Addon"的完整步骤清单、权限与所有权设计、运维授权方式、前端零代码条件与
门禁命令。阅读前请先读 `AGENTS.md` 与 `docs/AUTHZ_GRANTS.md`。

## 验证结论（P7）

- 一个最小但真实的 CRUD 业务（便签：创建/更新/删除/分页列表，所有权隔离 +
  权限管控 + 同事务审计）从空目录到全部门禁通过，**前端零代码**：通用
  `ModulePage`/`TableView` 直接消费后端 Catalog 投影渲染完整 CRUD 页面。
- access 权限管线对新业务即时生效：Action 声明 `.permissions(...)` 后自动进入
  权限目录投影（决策 D3），授权/撤销/Token 快照合并走既有 access 设施，
  新业务不需要任何权限侧代码。

## 步骤清单

### 1. 目录结构（三件套 + domain）

```text
src/addon/<addon>/
├── mod.rs                 # addon 层：唯一对外入口 build_addon()
├── domain/                # addon 级共享机制
│   ├── mod.rs
│   ├── context.rs         # 模块上下文（Action 能力的单一出口 + 事务收尾）
│   └── repository.rs      # 表的唯一持久化边界（受信 writer）
└── <module>/              # module 层
    ├── mod.rs             # 装配定义卡：表→上下文→中间件→Action→展示投影/View
    ├── table.rs           # 表声明（Schema 唯一事实来源）
    └── actions/           # 每个 Action 独占一个自包含文件
        ├── mod.rs         # 只有模块清单 + ACTIONS 注册表数组
        └── <action>.rs
```

架构检查器（`python scripts/check_architecture.py`）机械约束：module 目录只承载
`mod.rs`/`table.rs`/`actions/`；机制代码一律进 `domain/`；`actions/` 下每个文件
恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register`，且在
`actions/mod.rs` 的 `ACTIONS` 数组登记。

### 2. 表声明 `table.rs`

- 用 `yang_base::fields!` 声明全部字段；表名经 `TableName::new` 校验；
  `Key` 主键、`Timestamp::created_at()/updated_at()` 自动时间戳。
- **字段级权限即边界**：客户端永远不能提供的字段（如 `owner_user_id`）声明
  `.writable_by([SYSTEM_ROLE])`，写入只能经 domain 受信 writer（system 角色）。
- 查询能力按需要打开：`searchable/filterable/sortable` 会同时驱动 TableQuery
  校验与 TableView 投影；为高频过滤字段加 `.index_named(...)`。
- Schema 由 `src/infrastructure/schema.rs` 启动期增量同步，**禁止新增 SQL
  迁移文件**（`migrations/` 为空是有意的）。
- 参考：`src/addon/demo/notes/table.rs`（含 schema 单测范式）。

### 3. 机制 `domain/`

- `repository.rs`：表的唯一持久化边界。`TableDefinition::bind(pool).query([SYSTEM_ROLE])`
  构造受信查询；所有权等多租户式不变量在此收敛（demo：所有读/改/删强制
  `WHERE owner_user_id = 当前用户`，写入的归属人取自 `ctx.actor()`）。
- `context.rs`：模块上下文聚合 Repository 并提供 `finish_transaction`
  （提交/回滚收尾，回滚失败只记日志不覆盖原错误）。
- 写业务表（非 `users` 授权事实）不需要进 authorization-writer allowlist；
  只有 raw SQL（`sqlx::query...`）才需要 `//! raw-sql-boundary:` 登记——
  优先用 `TableQuery`，避免 raw SQL。

### 4. Action（`actions/<name>.rs`）

每个文件自包含：输入声明 → `handle` 用例 → `register` 原子绑定。

- 输入：`yang_base::params!`（`#[deny_unknown_fields]` 防内部字段注入）或手写
  Input（实现 `ParamInput`，参考 `list_notes.rs`/内置 `SelectAction` 契约）。
- 权限：`.permissions(["<addon>.<module>.read"|".write"])`。权限字符串须匹配
  `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`；声明即进入权限目录，无需其他登记。
- 路由：显式 `/api/v1/...`（如 `POST /api/v1/demo/notes`）。
- 写操作用例范式：开事务 → 经 domain writer 改事实 →
  `audit::succeeded_event(...)` + `audit::append_in_tx(...)` 同事务审计 →
  `finish_transaction` 收尾。跨所有权目标影响 0 行时统一返回
  `BaseError::RecordNotFound`（不泄漏存在性）。
- 列表 Action：复用内置 `SelectAction` 的标准分页契约
  （`page/page_size/search/where/order_by/count_total` →
  `items/page/page_size/total`），叠加业务强制条件后即可直接作为通用
  TableView 的 `data_action`（demo 在 SelectAction 语义上叠加了所有权过滤）。
- 脚手架可用 `python scripts/new_action.py <module_actions_dir> <name> --title "..." --method POST --path /api/v1/...`。

### 5. module 装配 `<module>/mod.rs`

按分区顺序：表 → 上下文 → `TokenAuthMiddleware`（`user_from_claims` +
`AuthorizationVersionValidator`，`.authenticate_public_actions()`）→
`actions::register_all` → `.presentation(...)` + `.view(...)`。

- `ModulePresentationSpec`：身份（`crate::addon::user_identity()`）、标题、
  图标 token、order、primary_action——前端导航据此生成模块页。
- `ViewSpec`（前端零代码的关键）：`data_action`（标准分页 Action）+
  `field(...)` 列 + `present_action(...)`（Toolbar/Form 创建、Row/Form 编辑
  `.record_parameter("id")`、Row/Invoke 删除 `.confirmation(...)`）+
  `default_sort(...)`。构建期会交叉校验引用与字段能力位。

### 6. 组合根接线（共 3 处小编辑）

1. `src/addon/mod.rs`：加 `pub(crate) mod <addon>;`。
2. `src/addon/<addon>/mod.rs`：暴露 `build_addon(...) -> Result<AddonSpec, BaseError>`。
3. `src/app.rs`：`.addon(<addon>::build_addon(authorization_validator.clone())?
   .middleware(ActionLogMiddleware::new(...)))`；冒烟测试断言模块/表/权限投影。

### 7. 运维授权（决策 D2：无自提权路径）

新业务的权限字符串随 Catalog 冻结自动进入权限目录（可用
`GET /api/v1/access/permissions` 核实）。首个授权由运维 SQL 完成，必须遵守与
在线 writer 相同的一致性（同事务：事实行 + 版本递增 + Outbox），完整模板见
`docs/AUTHZ_GRANTS.md` 的"初始授权（运维）"；只需把权限换成新业务权限，例如：

```sql
INSERT INTO authz_grant (user_id, permission, granted_by, occurred_at)
VALUES (1, 'demo.notes.read', 0, UNIX_TIMESTAMP())
     , (1, 'demo.notes.write', 0, UNIX_TIMESTAMP());
-- 同事务递增 users.authz_version 并追加 authorization_outbox（模板见 AUTHZ_GRANTS.md）
```

之后的日常授权/撤销走 `POST /api/v1/access/grants[/revoke]`（Step-up 保护）。

### 8. 前端零代码条件

同时满足以下条件的业务模块不需要写任何前端代码：

1. 模块声明了 `ModulePresentationSpec`（导航/页面骨架）；
2. 声明了 `ViewSpec` 且 `data_action` 返回标准分页结构；
3. 操作均为"表单提交"或"直接调用"语义（Form/Invoke），可由
   `TableActionDialog` 依据 Action 输入 JSON Schema 通用渲染。

需要特殊交互（向导、图形编辑器、复杂联动）时才在
`frontend/src/features/registry.ts` 静态注册自定义视图；禁止按后端字符串动态
import。demo addon 未触碰 `frontend/` 任何文件。

### 9. 门禁命令

```bash
python scripts/check_architecture.py
cargo fmt --check
cargo test --lib --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

## 本次接入成本度量（P7 实测）

| 维度 | 数值 |
|---|---|
| 新增文件 | 11 个（全部在 `src/addon/demo/`） |
| 新增行数 | 1131 行，其中生产代码 791 行、单元测试 340 行 |
| 修改已有文件 | 2 个：`src/addon/mod.rs`（+1 行）、`src/app.rs`（+77 行，其中组装约 7 行、冒烟测试约 70 行） |
| 前端改动 | 0 |
| 基础库（`../../crates`）改动 | 0 |
| 新增运维/配置项 | 0（表结构启动期增量同步；权限随 Catalog 投影） |
| Action 数 | 4（create/update/delete/list） |

生产代码按职责拆分：表声明 66 行、domain 机制 164 行、4 个 Action 共 419 行、
module/addon 装配 124 行、注册表 18 行。

## 发现的基座摩擦点

1. **应用侧单测无法构造"已认证身份"的请求级投影**：`ActionContext::with_user`
   与 `ctx.user` 均为框架 `pub(crate)`（刻意防绕过认证），请求级 UI Catalog 的
   授权投影只能在框架仓库内测试，应用侧只能断言匿名投影与冻结 Catalog。
   授权链路的端到端证据依赖真实 MySQL/Redis 集成测试或前端 e2e。
2. **`ensure_readable_projection` 是框架 crate 私有**：自定义列表 Action 不能用
   内置 SelectAction 的"默认可读投影"，需要显式 `select_fields(...)` 列清单
   （与 View 字段重复声明一次）。可考虑框架层公开该能力或提供
   "按 View 投影"的查询入口。
3. **手写 Input 与 `params!` 的能力差距**：标准分页输入（`where` 树、
   `order_by`）无法经 `params!` 表达，必须手写结构体 + 空 `Params`；功能正确
   但契约声明分散（`Params` 为空，参数元数据只靠 schemars）。
4. **`OrderByItem` 未实现 `Debug`**：复用内置分页输入类型时外围结构体不能
   derive `Debug`（小问题，去掉 derive 即可）。
5. **ActionLogMiddleware 需逐 addon 手动挂载**：`app.rs` 中每个 addon 都要重复
   `.middleware(ActionLogMiddleware::new(LogIdentity::from_tools(&tools)))`，
   组合根容易漏挂；可考虑 AppBuilder 级默认中间件。
