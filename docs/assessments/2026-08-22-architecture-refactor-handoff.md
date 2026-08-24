# yang-system 架构净化改造：方案与进度暂存

> 创建日期：2026-08-22
> 状态：**进行中**——阶段 1/2 已由后台子代理开工，其余阶段未开始。
> 用途：会话中断后的续接手稿。下次继续任务时，把本文件交给新的会话即可恢复全部上下文。

## 一、改造背景（用户五个痛点与已确认方向）

1. **去过程宏**：`#[derive(Action)]` + `#[action(...)]`（如 `src/addon/admin/user/actions/add.rs` L21-30）不直观 → 改为**函数式 handler + mod.rs 集中路由表**；
2. **addon 纯净**：业务接口与机制文件混杂 → **模块内分 `actions/` + `domain/` 两层，通用机制下沉 yang-base**；
3. **主函数可见**：Action 文件只剩 `params!` 输入 + 一个 `pub(super) async fn` 主函数；
4. **减少裸 SQL**：可换全换 + 给 yang-db 补**服务端时间表达式**能力，方言特性保留为登记逃生口；
5. **Cargo feature 门控 addon**：仿 scs-api，addon 级 feature，default 全开。

以上四条取舍（Action 风格 / 机制文件去向 / SQL 替换力度 / feature 粒度）均已由用户逐项确认，无需再问。

## 二、已核实的关键事实

- `yang-base` 派发链只认 `ActionSpec` + `Arc<dyn DynAction>`；`bind_handler_contract`（`definition/spec.rs:319-336`）在 spec 自带 input/output schema 时跳过 `handler.meta()`——函数式入口在 builder 内用 `schemars::schema_for!` 自行填充 schema，**无需改动 Registry/dispatch/中间件**。中间件按 `ActionRef` 字符串匹配，与 handler 类型无关。
- blanket dispatch 路径（`action/typed.rs:55-86`）：`ParamInput::decode` → `handle_future` → `wrap_dispatch_output(output, "成功")`。函数式 dispatch 必须同构。
- `DynAction::meta()` 消费点：typed.rs blanket dispatch（函数式不走）、spec.rs bind_handler_contract（函数式绕过）、测试。函数式实现需先 Grep 确认 meta() 全部消费点后返回占位静态量。
- `Actions::native`（`definition/interface.rs:31-65`）：组装 ActionSpec → operation_id 补全 `<module>.<name>` → `dyn_action` 压入 `(ActionSpec, Arc<dyn DynAction>)`。函数式入口复用 `dyn_action` 与 operation_id 规则。
- lib_yang workspace AGENTS.md 约定"route/params/权限与 Handler 必须同源"——新 API 用**终结式 builder**（一次调用同时给 spec 与 handler）保持同源语义；yang-base 的 AGENTS.md 需同步更新表述（`src/action/AGENTS.md` 现有 GlobalTools/ModuleRouter 描述已过时）。
- 裸 SQL 共 14 处登记于 `docs/architecture/raw-sql-boundaries.md`（清单完整、无漏登记）：
  - 现有构造器已可表达（约一半）：简单 select、`FOR UPDATE` 行锁（`Transaction::select_for_update`）、CAS update、JOIN+分页+COUNT、audit INSERT；
  - 卡在同一缺口（约 8 处）：`UNIX_TIMESTAMP()` 作 INSERT 值 / UPDATE SET 值 / WHERE 比较项 / SELECT 投影（password_reset、admin/user 更新、authz outbox insert、outbox mark/retry）；
  - 保留逃生口：递归 CTE 防环（`work/task/repository.rs:121`）、`JSON_TABLE`+`FOR UPDATE OF`（同文件 190）、`FOR UPDATE SKIP LOCKED`（outbox claim）、`information_schema` 内省（audit/outbox schema 校验）、无 FROM 时钟采样、`COALESCE(MAX(GREATEST(...)))` 积压指标。
- yang-db 现有基线：`QueryBuilder` 支持投影/where 布尔树/join（单等值 ON）/order/group/having/limit/聚合执行端/increment/decrement/upsert；SET/VALUES 只接受绑定值；`RowLock` 仅经 `Transaction::select_*locked` 使用；`QueryBuilder::from_pool`/`Transaction::table` 偏内部（`#[doc(hidden)]`/`pub(crate)`）。
- 跨 addon 依赖（feature 门控前提）：admin/org/work/observability **均依赖 account**（`user_from_claims`、`GrantResolver`、授权 writer 函数、`AuthRateLimiter`）；account 反向只经 `src/app.rs` 注入 admin 的 `SystemOwnerClaimer`——admin feature 关闭时注入 noop 实现即可；`bootstrap.rs` 依赖 `account::email_delivery`（account 常驻，无影响）。
- `scripts/check_architecture.py` 的 `action_definition_count` 目前识别 `derive(Action)` 与 `ActionSpec::new(`，且扫描 `src/addon` 与 `examples` 下所有 `actions/` 目录——`examples/frontend_demo` 必须同批迁移，否则门禁红。
- `scripts/new_action.py` 现有两种注册风格（register fn / `actions!` 宏），需改为生成函数式新风格。
- scs-api 模式（`D:\code\lib_yang\scs\scs-api`）：每 addon 一个 feature + implies 表 + 组合根三处 cfg（use/mod/注册分支）+ 配置字段也 cfg 门控。yang-system 与它是同思想两代实现，**无代码依赖**。
- **涉及两个 Git 仓库**：yang-base/yang-db/yang-base-derive 属 `D:\code\lib_yang` 仓库，yang-system 是独立仓库。提交时需分别提交（本改造不执行任何 git 提交）。

## 三、目标形态示例

Action 文件（纯净，主函数即业务入口）：

```rust
//! 将现有基础用户绑定为平台账号。

use super::super::domain::model::AdminAccountView;
use super::super::domain::service::AdminService;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Str, Switch};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminAddInput {
        user_user: Int::new().title("用户 ID").require(true),
        name: Str::new().title("姓名").require(true).min_length(1).max_length(50),
        position: Str::new().title("职务").max_length(50),
        admin: Switch::new().title("超级管理员"),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminAddInput,
    service: Arc<AdminService>,
) -> Result<AdminAccountView, BaseError> {
    service
        .add(&ctx, input.user_user, &input.name, input.position.as_deref(), input.admin.unwrap_or(false))
        .await
}
```

模块 mod.rs（集中路由表，一眼看全端点；终结方法名以实现为准，原则是 spec 与 handler 原子绑定）：

```rust
let module = module
    .action_fn("add", {
        let service = Arc::clone(&service);
        move |ctx, input| add::handle(ctx, input, Arc::clone(&service))
    })
    .route(HttpMethod::POST, "/api/v1/admin/users")
    .display_name("添加平台账号")
    .description("将现有启用用户绑定为平台账号")
    .permissions(["admin.user:write"])
    .success_status(201)
    .register();
```

模块目录结构：

```text
src/addon/admin/user/
├── mod.rs        # 组装 + 路由表 + presentation
├── actions/      # 纯接口层：每文件 = doc + params! + 一个 handle 主函数
└── domain/       # service/repository/model/guard 等业务机制
```

Cargo features：

```toml
[features]
default = ["account", "admin", "observability", "org", "work"]
account = []
admin = ["account"]
org = ["account"]
work = ["account"]
observability = ["account"]
```

## 四、七阶段方案

### 阶段 1：yang-base 函数式 Action API（lib_yang 仓库）【进行中】

1. 新增 `crates/yang-base/src/action/functional.rs`：`FnAction<F, I, O>` 手写 `impl DynAction`（dispatch 与 blanket 同构；`TypeId` 支持 `call_boxed` 保住 `Registry::call<I, O>`；`meta()` 占位静态量）。
2. 新增 `ModuleSpec::action_fn(name, f) -> ActionFnBuilder<I, O>`：`.route/.display_name/.description/.permissions/.permission_mode/.public/.success_status/.response_kind/.multipart/.calls` + 终结 `.register()`；构建期校验全部复用 `AppBuilder::build` 现有链，不改 builder.rs 校验逻辑。
3. `#[derive(Action)]` 保留给框架内置 Action（CRUD 六件套、Login/StepUp），业务侧全部迁出；两通道共存。
4. 契约测试：fn action 与 derive action 的 Catalog/OpenAPI/dispatch/内部调用一致性。
5. 文档：重写 `crates/yang-base/src/action/AGENTS.md`，crate 级 AGENTS.md 补充"同源"约定说明。

验证：`cargo test -p yang-base --locked`、`cargo clippy -p yang-base --all-features --locked -- -D warnings`、`cargo fmt --check`。

### 阶段 2：yang-db 服务端时间表达式 + 事务入口（lib_yang 仓库）【进行中】

1. 受控表达式类型（白名单构造，绝不接受任意 SQL 字符串）：`unix_timestamp()`、`unix_timestamp_add(seconds: i64)`（参数走 bind）。
2. QueryBuilder 扩展：UPDATE SET 表达式值、INSERT VALUES 表达式值、WHERE 列↔表达式比较、SELECT 标量函数投影（带安全别名）。
3. `Transaction::table()` 等事务内构造器入口公开化 + 中文 rustdoc。
4. `insert_returning_id`（MySQL `LAST_INSERT_ID()`）。
5. PG 侧按最小改动处理（公共表达式类型则给 PG 渲染，否则明确不支持）。
6. SQL 文本生成单测 + MySQL 集成测试（跟随 crate 现有 `#[ignore]` Docker 模式）。

验证：`cargo test -p yang-db --locked`、clippy、fmt。

### 阶段 3：yang-system 裸 SQL 收敛（依赖阶段 2）

替换清单（逐文件）：

- `account/authz_version.rs`：全部（简单 select、`select_for_update`、CAS update、outbox INSERT 时间表达式）；
- `account/user/lifecycle.rs`：值 UPDATE + 行锁读替换；派生表 JOIN 防最后管理员改写为两步（子查询取 id 集 → `where_in` + `FOR UPDATE`），锁窗口语义逐行核对，由最后管理员并发集成测试背书；
- `account/password_reset/repository.rs`：全部（时间表达式 + `select_for_update`）；
- `admin/grants.rs`：改投影 `owner_key` 列，Rust 侧判断 `== "system-owner"`；
- `admin/guard.rs`：`count()` + Condition；
- `admin/user/repository.rs`：JOIN 列表/单行、行锁替换；EXISTS 复核改 `count() > 0`；INSERT 用 `insert_returning_id`；
- `org/grants.rs`：EXISTS+JOIN 改写为 join + count；
- `org/access/repository.rs`、`org/tenant.rs`、`org/user/repository.rs`：JOIN/行锁替换；
- `work/task/repository.rs`：行锁读替换；递归 CTE、JSON_TABLE 批量锁**保留登记**；
- `infrastructure/audit/repository.rs`：INSERT 换 `Transaction::table()` + builder；
- 保留登记不动：outbox claim（SKIP LOCKED）、`information_schema` 内省、无 FROM 时钟采样、积压指标聚合。

同步更新 `docs/architecture/raw-sql-boundaries.md` 与 `check_architecture.py` raw-sql 规则（被替换文件删除登记项，门禁语义不变）。

验证：`run_ci.py quick` + `integration`。

### 阶段 4：addon 结构净化 + 机制下沉（依赖阶段 1）

1. 框架下沉（yang-base，一次只迁一个机制，保持门禁绿）：
   - `PasswordEngine`（105 行）→ `yang-base::action::auth`；
   - `AuthRateLimiter`（315 行）→ yang-base，解耦 `crate::config::SecuritySettings` 为参数结构；
   - 邮箱验证码（367 行）→ yang-base，SMTP 发送经 trait 注入（lettre 适配留在 account 的 `email_delivery`）；
   - `browser_session`（149 行，Cookie + Same-Origin 校验）→ `yang-base::transport`。
2. 模块目录重组（yang-system）：每 module 只留 `mod.rs` + `actions/` + `domain/`；`claims/lifecycle/status/policy/repository/service/schema` 移入 `domain/`（纯 move + use 调整）；`check_architecture.py` 增加 module 级目录白名单检查。
3. 28 个 Action 文件重写为函数式（删 derive struct/trait impl/register fn 三层仪式）；`login.rs` 等包装框架内置 Action 的改为 handler fn 内直接调用；`actions/mod.rs` 改为集中路由表；条件注册逻辑（`credential_mutations_enabled`）保留在路由表处。
4. 按 addon 串行迁移：account → admin → org → work → observability，每个迁移后跑 `quick`。
5. `examples/frontend_demo/` 同批迁移。

验证：每个 addon 迁移后 `run_ci.py quick`；全部完成后 `full`。

### 阶段 5：Cargo feature 门控（yang-system）

1. `Cargo.toml` 加上文 features 定义。
2. `src/addon/mod.rs`：`#[cfg(feature = "...")] pub mod xxx;`，identity 函数按 feature 门控。
3. `src/app.rs`：addon 注册、grant resolver 装配按 cfg 条件化；admin 关闭时 account 注入 noop `SystemOwnerClaimer`；`build_application` 测试按 `#[cfg(feature)]` 拆分断言。
4. `src/bootstrap.rs`：email_delivery 常驻，检查管理面/worker 的 addon 条件依赖。
5. 配置字段**不门控**（收益小于组合复杂度）。
6. `scripts/run_ci.py` 增加 feature 组合检查：`cargo check --no-default-features --features account`、`cargo clippy --all-targets --all-features`、`cargo test --lib`（default）；`--self-test` 同步。
7. 前端不动（Catalog 驱动自动降级）。

验证：account-only / default / all-features 三种组合编译通过 + quick/full 全绿。

### 阶段 6：脚手架、门禁与文档同步

1. `scripts/new_action.py`：生成函数式新风格（params! + `pub(super) async fn handle` + mod.rs 路由表条目），保留"尚未实现"错误语义，更新自测。
2. `scripts/check_architecture.py`：`action_definition_count` 改识别新形态（每文件恰好一个 `pub(super) async fn handle`）；注册检查改为 mod.rs 必须引用 `<name>::handle`；新增 module 目录白名单；更新 self-test fixture。
3. 文档同步：`AGENTS.md`、`README.md`、`docs/architecture/raw-sql-boundaries.md`、`docs/architecture/tenant-data-paths.md`、lib_yang 侧 yang-base/yang-db 文档。

### 阶段 7：总验证

`run_ci.py quick/full/integration` 全绿；lib_yang workspace 三包 test+clippy；前端 `pnpm check`；feature 组合矩阵冒烟。

## 五、执行顺序与依赖

```text
阶段1 (yang-base fn API) ──┐
                          ├─→ 阶段4 (结构净化，按 addon 串行) ─┐
阶段2 (yang-db 表达式) ──→ 阶段3 (SQL 收敛)                   ├─→ 阶段6 → 阶段7
                          阶段5 (feature 门控) ────────────────┘
```

阶段 1 是阶段 4 的硬前置；阶段 2 是阶段 3 的硬前置；阶段 5 放阶段 4 之后避免与 mod.rs 重写冲突。每阶段结束保持 quick 门禁绿；跨仓库改动（阶段 1/2/4.1 在 lib_yang）单独成批便于分别提交。

## 六、当前进度（2026-08-22 第二次更新）

| 阶段 | 状态 | 提交/说明 |
|---|---|---|
| 阶段 1 | **完成** | lib_yang `89ba536`：`FnAction` + `ModuleSpec::action_fn`/`ActionFnBuilder`；yang-base 578 测试通过 |
| 阶段 2 | **完成** | lib_yang `461988b`：`SqlExpr`（unix_timestamp 系列）、`set_expr`/`select_expr`/`where_expr`/`insert_returning_id`、`from_pool` 公开 |
| 阶段 3 | **完成** | `2958055`：约 10 处裸 SQL 换构造器，逃生口保留 3 个文件（work/task 递归 CTE+JSON_TABLE、outbox SKIP LOCKED、audit/outbox information_schema）；真实库集成测试通过 |
| 阶段 4.1 | **完成** | lib_yang（机制下沉提交）+ `6d45b31`：PasswordEngine/AuthRateLimiter/邮箱验证码/BrowserSession 下沉 `yang_base::action::auth` |
| 阶段 4.2 | **完成** | `4b18ab1`：门禁识别函数式形态（每文件恰好一个 `pub(super) async fn handle`、禁 derive）、module 目录白名单（只允许 mod.rs/actions/domain）、writer allowlist 迁入 domain/、new_action.py 生成函数式模板 |
| 阶段 4.3 | **完成** | `fdcd1a2`：全部业务 Action 函数式重写（actions/mod.rs 变集中路由表）、机制文件迁入各级 domain/、frontend_demo 同步；门禁+106 单测+clippy+fmt+integration 全绿 |
| 阶段 5 | **进行中** | Cargo feature 门控（agent-12 执行中） |
| 阶段 6 | **部分完成** | AGENTS.md/README.md 已同步新结构；剩余：阶段 5 完成后的 feature 说明、handoff 归档 |
| 阶段 7 | 未开始 | 全部完成后跑 quick/full/integration |

**已知遗留（后续优化，不阻塞）**：

- `org/user/domain/fn_action.rs` 与 `work/task/domain/fn_action.rs` 各有一份约 110 行的 `FnAction` 受控桥接副本：CRUD 写覆盖（org.user add/put/del、work.task add/put）的输入是框架 `Record`/`PutInput`（未实现 `ParamInput`），且 Catalog 契约由 `crud_at_with_mutations` 从表定义动态生成，无法用 `action_fn` 表达。若 yang-base 后续公开函数式 CRUD 覆盖入口，可删除这两处桥接。
- 函数式通道的 dispatch 不发 `yang_action_*` handler 级指标（与官方 `FnAction` 语义一致）；内置 CRUD（derive 通道）仍发。若需口径统一，在 yang-base `Registry::dispatch` 统一埋点。
- `observability/actions/report.rs` 的 `no_store_response` 私有辅助留在 Action 文件内（handler 行为的一部分，门禁允许）。

## 七、风险备忘

- derive 与函数式两通道共存是设计内状态（内置 Action 继续用 derive），不设"彻底删除 derive"目标。
- 生命周期派生表 JOIN 改写需核对锁窗口语义，由最后管理员并发集成测试背书。
- feature 组合只承诺 account-only / default / all-features 三种 CI 检查，不穷举。
- `meta()` 占位实现前必须先 Grep 确认全部消费点。
- `examples/frontend_demo` 被架构门禁扫描，必须与 src 同批迁移。
