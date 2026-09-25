# 权限组与首账号引导：设计与决策修订

> - 文档性质：**设计规格（spec）**。本文件描述待实现的目标状态，不表示下述能力已经完成。
> - 撰写日期：2026-09-24
> - 代码快照：撰写时对 `src/addon/access/`、`src/addon/account/`、`src/infrastructure/`、`src/app.rs` 与 `crates/yang-base` 逐文件核实，证据以 `file:line` 标注。
> - 关联文档：`docs/architecture/foundation-baseline.md`（决策 D1–D7）、`docs/contracts/AUTHZ_GRANTS.md`、`docs/architecture/authorization-writers.md`、`docs/architecture/account-system-roadmap.md`、`docs/assessments/2026-09-14-account-completeness-adversarial-review.md`

## 一、背景

`access` Addon 已经交付了权限基础设施的绝大部分：权限目录投影、授权事实存储、管理 Action、Step-up、审计、以及「授权事实变更与业务写同事务」的版本失效链路。但它缺两样东西，导致整个权限管理面在当前系统中**不可达**：

1. **没有冷启动引导**。没有任何账户持有 `access.grants.write`，而授予接口自身受该权限保护，形成自锁。文档自陈「权限管理面整体不可达、属预留端口」（`docs/architecture/account-system-roadmap.md:32`）。
2. **没有角色/权限组聚合层**。只有「用户 ↔ 权限字符串」直授，权限行数随用户数线性膨胀，无法批量维护权限方案。

本规格同时修订一条与之冲突的既有决策（D2），并明确区分两条改动的性质：

| 目标 | 与既有决策的关系 | 性质 |
|---|---|---|
| ② 可配置权限组 | D4 明文预留了扩展口（`foundation-baseline.md:39`「后续如需角色再扩展（留接口）」） | **执行既定路线** |
| ① 第一个账号为系统管理员 | D2 明文禁止（`foundation-baseline.md:37`「不做任何『第一个用户自动是管理员』逻辑」） | **修订既有决策** |

## 二、现状事实基础

以下均为源码核实结论，构成本规格的设计前提。

### 2.1 已具备的能力（可直接复用）

| 能力 | 位置 |
|---|---|
| 权限目录：从冻结 Catalog 投影，运行期只读，`ensure_declared` fail-closed | `src/addon/access/domain/permission_catalog.rs:40-72,88-115` |
| 权限字符串契约：`PERMISSION_PATTERN`、`PERMISSION_MAX_LENGTH` | `src/addon/access/domain/permission_catalog.rs:14,16` |
| 授权事实表与唯一 writer：`GrantRepository`，登记为 `access-grant-lifecycle` | `src/addon/access/domain/repository.rs`、`grants/table.rs:20-64` |
| 同事务三件事原语：`FOR UPDATE` 锁用户 → 单调递增 `authz_version` → 追加 `authorization_outbox` | `src/addon/account/domain/authz_version.rs`；端口抽象在 `src/infrastructure/authorization/ports.rs` |
| Token 授权快照扩展端口，已按 `Vec` 装配 | `src/addon/account/domain/grants.rs:56-93`；装配点 `src/app.rs:106` |
| 授予/撤销 Action（幂等 + Step-up + 审计） | `src/addon/access/grants/actions/{grant,revoke}_permission.rs` |
| 声明式 Schema：启动时增量同步，无 SQL 迁移文件 | `src/infrastructure/schema.rs`；规则见 `docs/contracts/SCHEMA.md` |
| 首账号管理员的**半成品**：端口、枚举、事务内调用点、审计埋点均已存在 | `src/addon/account/domain/system_owner.rs`、`register.rs:95-113`、`context.rs:169-178` |

### 2.2 关键结构约束

- **一 Module 一表**：`ModuleSpec.table` 的类型是 `Option<TableSpec>`（`crates/yang-base/src/definition/spec.rs:492,527`），全部 module 的表被汇总进一个 `BTreeMap<TableName, TableDefinition>`（`crates/yang-base/src/definition/builder/compile.rs:693`）。因此 N 张表需要 N 个 module，或走别的注册路径。
- **运行支撑表是定长数组**：`infrastructure_definitions() -> Result<[TableDefinition; 6], BaseError>`（`src/infrastructure/schema.rs:47-56`），并有精确断言 6 张表名的测试（同文件 `:289-303`）。该数组的既定定位是「非 UI 运行支撑表」。
- **外键规则恒为 `RESTRICT` 且不可变**：`foreign_key_named` 不接受 `ON DELETE`/`ON UPDATE`（`crates/yang-base/src/table/definition.rs:628-655`）；`schema_sync` 只增不删，永不删除表、列、索引或约束（`docs/contracts/SCHEMA.md:12,30`）。→ **外键一旦声明就永久存在**。
- **权限字符串不允许通配**：`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$` 与 DB CHECK `chk_authz_grant_permission_format` 双重约束（`grants/table.rs:60-63`）。`*` 与 `system.*` 均非法。
- **已声明的权限全集（10 条）**：`access.grants.read`、`access.grants.write`、`account.users.read`、`account.users.manage`、`demo.notes.read`、`demo.notes.write`、`feishu.datasource.read`、`feishu.datasource.write`、`feishu.datasource.secret`、`feishu.option.read`。

### 2.3 直接阻断目标实现的四个缺口

1. **`SystemOwnerClaimer::claim` 缺少 `&ActionContext`**（`system_owner.rs:21-29`），而唯一受信 writer 的每个方法都需要 `ctx`（`access/domain/repository.rs:45-48` 用 `trusted_query(&ctx)`）。→ adapter 无法复用唯一 writer。
2. **哨兵载体表不存在**。历史实现在 `admin_user` 表的 `bootstrap_key NULL UNIQUE + CHECK`，随 `e3b34e1`（删除 admin Addon）一并移除。`authz_grant` 的唯一键是 `(user_id, permission)`，**无法仲裁「全局仅一个」**。
3. **没有任何「最后一名管理员」守卫**。`disable_self.rs` 不判断管理员；`delete_account.rs` 既不判断、**也不清理 `authz_grant` 行**（留孤儿授权）；`admin_disable_user.rs:33-41` 有自指防护，但没有「最后一个」防护。→ 管理员可自我消灭，系统进入零管理员态，只能裸 SQL 恢复。
4. **引导审计事件无读取面**。`Claimed` 分支写 `first-registration` 审计事件（`register.rs:99-112`），但全仓 `audit_event` 没有任何查询 Action。

另有两处既有缺陷与本设计相邻。二者的处置不同，必须区分：

- **`claim` 失败会连带回滚注册**（`register.rs:70-117` 在同一事务闭包内用 `?` 上抛，`AlreadyClaimed` 与真实数据库故障被同等对待）。→ **本次修复**，见 §7.2 第 2 条。
- **字段级角色通道与 Action 级权限通道互不相通**。`users` 表的 `email`/`email_verified_at`/`password_hash` 等字段声明 `readable_by([SYSTEM_ROLE])`（`src/addon/account/user/table.rs:101-150`，`SYSTEM_ROLE = "system"`），而**没有任何账户持有 `system` 角色**（账号域固定签发 `user`，`access/domain/resolver.rs:71` 断言不附加角色）。后果：即使持有 `account.users.read`，也无法经表查询面读到邮箱列。→ **本次不修复**，列为已知限制（§15 第 6 条），因为修复它意味着让组名进入 Token 的 `roles`，那是独立的一条决策。

## 三、决策修订

### 3.1 D2 修订：从「无最终管理员」到「引导式一次性管理员」

**原 D2**（`foundation-baseline.md:37`）：系统不提供最终管理员角色；初始授权由运维经 SQL/工具完成；不做任何「第一个用户自动是管理员」逻辑。

**修订后 D2**：系统仍**不提供不可降权的超然账号**，应用内仍**不存在自提权路径**；但第一个成功注册的账号由应用在同一事务内**引导为管理员**，该身份是普通、可降权、可停用、可删除的授权事实。

修订的实质边界：

| 原 D2 保护的性质 | 修订后是否保留 | 如何保留 |
|---|---|---|
| 无自提权路径 | **保留** | 见 §7.1 子集校验 |
| 没有不可审计的超级身份 | **保留** | 管理员 = 内置全权组成员，是一条可查询的 `user_group` 事实 |
| 初始授权不依赖运维人肉操作 | **放宽** | 由应用引导一次；运维 SQL 降级为灾备路径 |

修订的关键论证：原 D2 真正要防的是「**用业务层 check-then-act 判断（用户表为空则本次注册者晋升）导致的并发提权**」——这正是业界反复出事的地方（Open WebUI CVE-2026-45675、Appsmith GHSA-9wcp-79g5-5c3c 在并发下产出多个管理员）。本设计**不使用**任何「判空」逻辑，改用数据库唯一约束仲裁：哨兵行的 `UNIQUE` + `CHECK` 组合让第二个插入者必然失败。这不削弱原决策的防护，反而把仲裁从应用层挪到了数据库层。

**不做**（明确排除）：
- 不引入 AWS root / GCP 超级管理员式的「标志位 + 鉴权短路 + 永久不可降权」身份。云厂商自身文档明确警告 root 身份不得日常使用；把 break-glass 账号做成日常账号是反模式。
- 不引入「角色继承角色」的多层图。

### 3.2 D4 修订：行使其预留的扩展口

**原 D4**（`foundation-baseline.md:39`）：只存「用户 ↔ 权限字符串」直授，不引入角色聚合层；角色仍用固定 `"user"`；**后续如需角色再扩展（留接口）**。

**修订后 D4**：在直授之外增加**一层**权限组聚合（组 = 权限的命名集合）。Token 中仍只承载具体权限字符串，`permissions_match` 语义不变。

明确不做三层：**不允许组嵌套组**。依据是云厂商的一致结论（AWS 明确 group 不能嵌套、不能作 Principal；GCP 无角色继承）与 Keycloak 的反例（官方文档自认 *"Composite Roles 与 Groups 功能相同，只是概念不同"*，且复合角色有实测 `O(C×M×D)` 性能问题，keycloak#51531）。组嵌套会在第一周就带来环检测、求值顺序、审计归因三项负债，而收益（复用）可由「一个组包含更多权限」直接获得。

## 四、目标与非目标

### 4.1 目标

1. 第一个成功注册的账号在同一事务内成为系统管理员，拥有**全部**权限，包括未来新增 Action 声明的权限。
2. 提供可配置的权限组：组是权限的命名集合，用户可属于多个组，有效权限 = 所有组成员权限的并集。
3. 组的权限/成员变更能让已签发 Token 失效。
4. 在引入上述能力的同时，保留 D2 的核心性质：应用内不存在自提权路径。
5. 权限管理面从「不可达」变为可用，并补齐「最后一名管理员」不变量的守卫。

### 4.2 非目标

- 不做数据范围/行级权限（按数据源、按表、按部门）。本次不引入任何资源级授权器。
- 不做角色继承、不做组嵌套。
- 不做 ABAC / 条件策略（时间窗、IP、金额阈值）。
- **不引入任何外部授权服务或策略引擎**。理由见 §10。
- 不改变「Token 内烧录权限快照 + 请求期内存比对」的运行模型。
- 不改变 `access.grants` 直授通道（保留为逃生舱）。

## 五、数据模型

四张新表，全部走声明式 Schema，**零 SQL 迁移文件**。

### 5.1 `permission_group` — 组本体

| 列 | 类型 | 约束与语义 |
|---|---|---|
| `id` | `Key` 自增 | 主键 |
| `group_key` | `Str(64)` | 稳定机器标识。`pattern ^[a-z][a-z0-9_]*$`；UNIQUE `uk_permission_group_key` |
| `title` | `Str(128)` | 展示名，可改；不作为引用锚点 |
| `description` | `Str(255)` 可空 | |
| `created_by` | `Int` 必填 | 创建人 `users.id` |
| `occurred_at` | `Timestamp` | `created_at()` 自动写入 |

字段级 `readable_by`/`writable_by` 全部限 `[SYSTEM_ROLE]`，读写只能经受信 Repository。

**内置全权组**：`group_key = 'system_admin'` 是代码常量（`SYSTEM_ADMIN_GROUP_KEY`），不另设 `kind` 列。唯一性由 `uk_permission_group_key` 保证；「该组的有效权限 = 整个权限目录」是**解析期规则**，不落存储。该组不可删除、不可改名、不可增删条目（由常量守卫）。

### 5.2 `permission_group_item` — 组 → 权限

| 列 | 类型 | 约束与语义 |
|---|---|---|
| `id` | `Key` 自增 | 主键 |
| `group_id` | `Int` 必填 | FK → `permission_group.id` |
| `permission` | `Str(128)` | `PERMISSION_PATTERN` + CHECK `chk_permission_group_item_permission_format` |
| `granted_by` | `Int` 必填 | |
| `occurred_at` | `Timestamp` | |

UNIQUE `uk_permission_group_item (group_id, permission)`。

### 5.3 `user_group` — 用户 → 组

| 列 | 类型 | 约束与语义 |
|---|---|---|
| `id` | `Key` 自增 | 主键 |
| `user_id` | `Int` 必填 | FK → `users.id` |
| `group_id` | `Int` 必填 | FK → `permission_group.id` |
| `granted_by` | `Int` 必填 | |
| `occurred_at` | `Timestamp` | |

UNIQUE `uk_user_group (user_id, group_id)`。两个 FK 均为 `RESTRICT`（框架默认），这正是「删除仍有成员的组会被数据库拒绝」所需的语义。

### 5.4 `system_owner` — 引导哨兵（单行）

| 列 | 类型 | 约束与语义 |
|---|---|---|
| `id` | `Key` 自增 | 主键 |
| `sentinel_key` | `Str(32)` | UNIQUE `uk_system_owner_sentinel` + CHECK `chk_system_owner_sentinel` (`sentinel_key = 'system-owner'`) |
| `user_id` | `Int` 必填 | 被引导的用户 |
| `claimed_at` | `Timestamp` | |

**并发仲裁机制**：这是一张语义上的单行表。`UNIQUE(sentinel_key)` 与 `CHECK(sentinel_key = 'system-owner')` 两个约束的组合，使得第二次插入必然违反其一。不依赖自增主键被显式赋值，也不依赖应用层判空。该设计是历史实现（`admin_user.bootstrap_key NULL UNIQUE + CHECK`）的原样恢复。

### 5.5 表的放置位置

**决定**：`permission_group` 放在新建 module `access.groups`（唯一具备独立 UI 语义、并承载全部新增 Action 的表）；其余三张表放入 `infrastructure_definitions()`，把返回类型从 `[TableDefinition; 6]` 扩为 `[TableDefinition; 9]`，并同步 `src/infrastructure/schema.rs:289-303` 的精确表名断言。

**理由**：该数组的既定定位就是「非 UI 运行支撑表」——`authorization_outbox`、`audit_event`、`user_session`、`login_event`、`user_avatar` 均在此处，两张 join 表与一张哨兵表确实没有独立 UI 语义。替代方案是每张表建一个 module，代价是前端 Catalog 导航凭空多出 3 个空模块。

**代价（如实记录）**：该数组是定长类型，每新增一张运行支撑表都要同时改类型签名与该断言测试。这不是缺陷，是刻意让「新增支撑表」成为一次显式改动。

## 六、运行时解析与失效传播

### 6.1 解析：新增 `GroupGrantResolver`

在 `src/addon/access/domain/` 新增 `GroupGrantResolver`，实现现成的 `GrantResolver` trait（`src/addon/account/domain/grants.rs:56-93`），并加入 `src/app.rs:106` 的 `grant_resolvers: Vec`：

```text
Token 签发/刷新路径
  AuthorizationGrants::user()                      ← 角色恒为 "user"，不变
    .extend(AuthzGrantResolver.resolve(...))       ← authz_grant 直授，不变
    .extend(GroupGrantResolver.resolve(...))       ← 新增
                    │
                    ├─ 读该用户的 user_group 行
                    ├─ static 组 → 展开 permission_group_item
                    └─ system_admin 组 → 取整个 PermissionCatalogHandle
                                              ↑ 未来新增 Action 的权限自动纳入
```

关键性质：

- **Token 中仍只承载具体权限字符串**。`permissions_match`（`crates/yang-base/src/router/middleware.rs:252-268`）一行不改。
- **不向 Token 的 roles 写入组名**。角色保持账号域固定的 `user`，与 `access/domain/resolver.rs:71` 的既有断言一致。
- **目录未安装时 fail-closed**：`PermissionCatalogHandle::entries()` 在未安装时返回 `ConfigError`（`permission_catalog.rs:95-100`），因此全权组解析在目录未就绪时会拒绝而非放行。

### 6.2 `system_admin` 组如何表达「全部权限」

「全部权限」不落在任何一行存储里，而是解析期取整个权限目录。这样做的原因：

- `*` 与 `system.*` 会被 `PERMISSION_PATTERN` 与 DB CHECK 拒绝（见 §2.2）。
- **若改为在引导时把当时全部权限物化进表，则此后新增 Action 的权限不会自动纳入**，管理员会静默失去对新模块的访问——这是最难排查的失败模式。
- 保留「Token 只装具体权限字符串」这一性质，审计与推理都保持可读。

### 6.3 失效传播：同步扇出

| 变更 | 事务内动作 |
|---|---|
| 加/删组成员 | 锁目标组组行 + 该成员/组成员的 users 行（升序，见锁序表）→ 递增其 `authz_version` → 追加 outbox |
| 加/删组权限 | 锁**全部成员**（按 `user_id` 升序固定锁序）→ 逐个递增 → 逐个追加 outbox |
| 删除组 | 先要求组内无成员，否则拒绝（见 §8.3） |
| 重命名组（仅 `title`） | **不触发失效**（`title` 不是授权事实） |

语义与今天的 `grant_permission` 完全一致，`docs/architecture/authorization-writers.md` 的契约形状不变。选择同步扇出而非复合版本的理由：不触碰校验热路径——该路径受 `tests/refresh_load_benchmark.rs` 零错误基准守护；且 `authz_version` 保持「per-user 单调递增」的既有文档语义。

**必须写死的约束**：定义常量 `MAX_GROUP_MEMBERS = 200` 作为单个组的成员数上限。加/删组权限时若成员数超过该上限，**明确报错并要求先分批处理**，而不是静默地做一次 O(N) 行锁事务。该值随扇出事务的实测耗时调整，调整需附压测数据。

`add_group_member` 是唯一能**增加**成员的路径，因此上限判定另有两处口径要求（收尾回合补）：

1. **判定必须建立在加锁后的成员数上**。非锁定快照下，两名不同操作者并发加**不同**成员时事务锁批互不相交、互不阻塞，各自读到「199 名」而双双放行，成员数越过 200。
2. **幂等分支不得被上限误拒**。上限必须按「本次是否真的会新增」计时：重复添加一个**已在组内**的成员不新增任何人，即便组已满 200 人也必须按幂等成功返回。

**锁序（收尾回合改正，全仓统一）**：组事实的事务只允许按下面的**先后**取锁，同一层内 users 行一律升序——这是防死锁的唯一手段，需有并发测试证明。

| 序 | 锁 | 位置 | 纪律 |
|---|---|---|---|
| 1 | **users 行锁** | `lock_users_ascending_in_tx`、守卫里逐成员的 `lock_authorization_version`、扇出的 `invalidate_users_in_tx` | 一律按 `user_id` **升序整批**获取；整个事务不得出现第二把乱序的 users 行锁 |
| 2 | **目标组行锁**：`permission_group` 主键行 | `GroupRepository::lock_group_in_tx`（`FOR UPDATE` 记录锁） | 成员变更的**串行化点**：锁住它才能让「读成员名单 → 判据 → 写成员行」整段串行化，也让上限判定拿到一份不漂移的成员数。必须出现在 users 行锁**之后**（理由见下条） |
| 3 | 其余 DML 的隐式索引锁 | `user_group` 的 DELETE/INSERT、`permission_group_item`、`audit_event`、`authorization_outbox` | 只能落在最后 |

**为什么是「users 行 → 组行」而不是反过来**：`add_group_item` 先按升序锁 users 行（`{操作者} ∪ {组成员}`），再在插入条目时由 `permission_group_item` 的外键取**父组行**的 S 锁，次序正是「users 行 → 组行」。成员变更若反过来先取组行 X 锁、再取 users 行，就会与它构成 ABBA 环（一方持组行等 users 行、另一方持 users 行等父外键 S 锁）。因此 `remove_group_member` 在事务前先做一次只读**探查**确定 users 行锁集合（成员 id 必须先于加锁知道）；探针只为定锁集，允许陈旧，判据读在组行锁之后。

**关于「拿成员行当串行化点」（收尾回合真库实测推翻，勿回退）**：曾经的实现用 `SELECT ... WHERE group_id = ? FOR UPDATE` 直接锁 `user_group` 的成员行。对**成员集合为空**的组，该语句只会命中彼此兼容的**间隙锁**，根本没有串行化；两个并发加成员都通过它、都读到空名单，随后一方持 users 行等对方、另一方持间隙锁等对方的插入意向锁，死锁（真库 40001 → 500）。组行记录锁（主键等值命中，只加记录锁不加间隙锁）对空组同样互斥，才是可用的串行化点。

**例外及其理由**：账号生命周期路径（`disable_self` / `admin_disable_user` / `delete_account`）先经 `lock_credential_in_tx` 锁住目标 users 行、再进最后管理员守卫。它们与成员变更同向（都是「users 行 → …」），因此守卫在那里**不必也不该**再取组行锁。这三条路径的守卫继续读非锁定成员名单，其正确性来自「成员变更必须先按升序锁住该组全部成员的 users 行，而生命周期目标本身就是成员」这条推理；反方向（并发入组）只会让启用管理员变多。

**已知边界（如实记录）**：`delete_account` 经 `purge_user_facts_in_tx` 删除成员行时**不**取组行锁（它按用户维度跨所有组清理）。它只在持有目标 users 行锁之后删除成员行，与成员变更同向（都是「users 行 → 成员行」），不构成逆向锁序；且它只**减少**成员关系，不触碰成员上限与管理员计数两个不变量。真正落在组行锁保护范围之外、因而不受上限约束的写入只有**直接写库**（运维 SQL、集成测试夹具）与**引导声明**（`system_owner` 哨兵保证至多一次的固定写入）。

## 七、引导流程

### 7.1 `register.rs` 事务内的固定顺序

```text
1. INSERT users
2. claimer.claim(tx, ctx, user_id, username)          ← 端口签名新增 &ActionContext
     try INSERT system_owner(sentinel_key='system-owner', user_id)
       ├─ 唯一约束冲突 → AlreadyClaimed → 降级为普通用户，事务照常提交
       └─ 插入成功     → Claimed
3. if Claimed:
     a. 确保 system_admin 组存在（按 group_key 惰性 INSERT；冲突即忽略）
     b. INSERT user_group(user_id, system_admin_group_id)
     c. 递增 authz_version + 追加 outbox
     d. 写 first-registration 审计事件
```

### 7.2 对现有代码的三处修正（均为必需）

1. **`SystemOwnerClaimer::claim` 签名增加 `&ActionContext`**（`system_owner.rs:21-29`）。否则 adapter 无法调用 `GrantRepository` 的 `trusted_query(&ctx)` 路径，只能绕过唯一 writer 直写——那会直接违反 `authorization-writers.md` 的硬约束。

2. **`AlreadyClaimed` 不得 `?` 上抛**。当前 `register.rs:70-117` 在同一事务闭包内传播错误，会让「首个用户因哨兵竞争失败而注册失败」。`AlreadyClaimed` 是**正常业务结果**，必须降级；只有真实数据库故障才回滚整个注册事务。

3. **引导结果必须有读取面**。`audit_event` 当前没有任何查询 Action，运维无法确认引导是否成功。最低要求：`system_admin` 组及其成员在权限组管理页可见。

### 7.3 灾备路径（保留）

`docs/contracts/AUTHZ_GRANTS.md:49-107` 的运维 SQL 模板**保留**，但定位从「主路径」改为「灾备路径」：当哨兵行被误删、或需要在数据库被清空后重新引导时使用。该章节需同步改写，明确两条路径的优先级与一致性要求（仍必须遵守同一事务三件事）。

## 八、不变量与防线

### 8.1 防自提权（保住 D2 实质的核心）

**不变量**：**任何一次组管理操作，都不得使调用者自身的有效权限集合增大。**

这个表述比「不能给自己加组」更强，而且必须更强——只看前者会漏掉一条同样可利用的路径：调用者对自己**已属于**的组添加一条自己尚不持有的权限，同样完成了自提权。两条路径必须由同一条不变量覆盖。

**机械校验方式**：在组管理 Action 的事务内，计算

```text
after  = 模拟本次操作后，调用者的有效权限集合
before = 调用者当前的有效权限集合
若 after ⊄ before 则拒绝（403）
```

其中「有效权限集合」由同一个解析函数产出（与 `GroupGrantResolver` 共用），避免校验与实际解析出现两套实现而漂移。

**不受此限的操作**：修改**他人**的组成员关系、修改**自己不所属**的组。这些是正常的授权管理行为，由 `access.groups.write` 持有者执行。但注意：如果调用者同时是 `system_admin` 成员，它本就持有全部权限，`after ⊄ before` 恒不成立，因此不受约束——这是正确的，管理员理应能授予任何权限。

**附加规则**：修改 `system_admin` 组的成员（含把自己加回去），要求调用者**已经是该组成员**。防的是「刚被移出全权组的账户立即把自己加回」。

该不变量必须有单元测试（覆盖上述两条提权路径）与集成测试（真实 Action 调用被拒）。

### 8.2 最后一名管理员守卫

**管理员的定义（写死）**：属于 `system_admin` 组的 `status = 'active'` 用户。

三处必须增加守卫，缺一不可：

| Action | 现状 | 需增加 |
|---|---|---|
| `disable_self.rs` | 不判断管理员 | 若操作后系统将无 active 管理员则拒绝 |
| `delete_account.rs` | 不判断管理员，**且不清理 `authz_grant` 行** | 同上守卫 + 清理 `user_group` 与 `authz_grant` 行 |
| `admin_disable_user.rs` | 有自指防护（`:33-41`），无「最后一个」防护 | 增加「最后一个」防护 |
| `POST /access/groups/members/remove` | 不适用 | 移出 `system_admin` 组的操作需同一守卫 |

**成员名单必须在组行锁之后读（收尾回合改正，本条是「不可达」的机械前提）**：上面四条守卫只有一个事实来源——「`system_admin` 组此刻的成员是谁」。这份名单**必须**在守卫所在事务内、且在持有目标组**组行锁**（`GroupRepository::lock_group_in_tx`）之后读出，并把同一份名单交给计数与 admin-only 判定。

理由：移出一个成员改的是 `user_group` 的成员行、**不是 `users.status`**，所以守卫原先持有的 users 行锁根本串行化不了成员变更；两个并发移出会各自停在事务开始时的快照上、各自读到同一份陈旧名单 `[A,B]`、各自数出「还有两名启用管理员」而双双放行，组被清零——正是本节声称「不可达」的那个状态。组行锁是成员变更的串行化点（次序见 §6.3），锁住它之后同一组的成员变更才真正排队。

真库实测（收尾回合）：同一名管理员并发发两条 `members/remove`（一条移出自己、一条移出另一名管理员），缺陷版本 5/5 轮响应 `[200,200]`、启用管理员 0、`user_group` 成员行 0；成员读改到组行锁之后 5/5 轮 `[400,200]`、启用管理员 1、成员行 1。

### 8.3 引用完整性

新表全部声明 FK，规则为框架默认的 `RESTRICT`：

- `user_group.user_id → users.id`
- `user_group.group_id → permission_group.id`
- `permission_group_item.group_id → permission_group.id`

**收益**：删除仍有成员的组会被数据库直接拒绝，而不是留下悬空指派。这与 §6.3 的应用层前置检查构成纵深防御：应用层给出可读错误，数据库层兜底。

**代价（如实记录）**：`schema_sync` 只增不删，**FK 一旦声明即永久存在**；且建库时若存量数据有孤儿引用，启动预检会拒绝启动。因此实施时需先确认 `permission_group` 与 `users` 无孤儿。

**`authz_grant` 孤儿问题（本次一并修）**：`authz_grant` 刻意不带 FK（`grants/table.rs:50-63` 只有 unique + check），账号删除后授权事实残留。本次在 `delete_account.rs` 的清理逻辑中顺带删除该用户的 `authz_grant` 行——理由是这次本来就要改该文件（§8.2 的守卫），边际成本接近零，而残留的授权事实是审计口径上的实质缺陷。

### 8.4 目录收缩产生的孤儿权限

某个 Action 被移除后，组里那条权限字符串即成孤儿。解析时会自然失配（匹配不到任何 Action），因此是 **fail-closed** 的，不会放大权限。但仍需提供清理/报告路径，否则配置债会静默累积。**本规格要求**：`GET /access/groups` 的返回中标记「不在当前权限目录内」的条目。

## 九、接口契约

### 9.1 新增权限

`access.groups.read`、`access.groups.write`。只要 Action 声明 `.permissions(...)`，即自动进入权限目录（`permission_catalog.rs:40-72`），也自动包含在 `system_admin` 组的有效权限内。

### 9.2 Action 清单（挂在 `access.groups`）

全部为 `POST`/`GET` 显式路径，并写 append-only 审计（对齐 `docs/contracts/AUDIT.md`）。**Step-up 只挂写操作**：变更授权事实的 Action（建/改/删组、加/移除条目、加/移出成员）逐个挂 `StepUpMiddleware`；两个只读 GET（`list_groups` / `get_group`）**不挂**。

| 接口 | 所需权限 | 说明 |
|---|---|---|
| `POST /api/v1/access/groups` | `access.groups.write` | 建组（`group_key` + `title` + `description`） |
| `POST /api/v1/access/groups/update` | `access.groups.write` | 改 `title`/`description`（不改 `group_key`） |
| `POST /api/v1/access/groups/delete` | `access.groups.write` | 删组；组内非空则拒绝（400，错误语义见 §9.3） |
| `GET /api/v1/access/groups` | `access.groups.read` | 列表：成员数、权限数、是否内置、孤儿条目数 |
| `GET /api/v1/access/groups/{id}` | `access.groups.read` | 详情：`group_key`、`title`、条目、成员（含孤儿标记与 `effective_all` 布尔） |
| `POST /api/v1/access/groups/items` | `access.groups.write` | 加权限（经 `ensure_declared` fail-closed） |
| `POST /api/v1/access/groups/items/remove` | `access.groups.write` | 移除权限（**不做**目录校验，对齐 `revoke_permission.rs:48` 的反向宽容语义） |
| `POST /api/v1/access/groups/members` | `access.groups.write` | 加成员（受 §8.1 子集校验约束） |
| `POST /api/v1/access/groups/members/remove` | `access.groups.write` | 移出成员（受 §8.2 守卫约束） |

> **修订（收尾回合，精确化 Step-up 挂载范围）**：本节原写「全部 Action 挂 Step-up」，
> 把重认证的保护面放大了。重认证保护的是**授权事实的变更**——只有写操作会改这份事实；
> 对只读的浏览 Action 也返回 428，会把「看一眼有哪些权限组」这种日常操作变成每一步都要
> 密码重认证，收益为零、摩擦很大。故精确化为「**写操作必挂、只读不挂**」。
>
> 判据不靠人工列举：**声明了非空且全部以 `.read` 结尾权限的 Action 视为只读**，权限为空
> （公开或未声明）或含非 `.read` 权限的一律按写操作 fail-closed 处理。该判据在
> `src/addon/access/groups/mod.rs` 的守卫测试里从冻结 Catalog 自动推出；「登记清单 ==
> 写 Action 集合」由单测锁定，「中间件真的被逐项挂上」由集成用例
> `every_group_write_action_without_step_up_is_rejected` 在真实装配路径上逐个 Action
> 取证（框架不对外暴露可查询的中间件列表，故只能以「不带 proof 必须被拒为 428」取证）。

**幂等语义**：与会话既有契约一致——重复添加成员/条目返回 `changed: false`，不递增版本、不写 outbox（对齐 `docs/contracts/AUTHZ_GRANTS.md:75-77`）。

**内置组保护**：对 `group_key = 'system_admin'` 的 `update`/`delete`/`items`/`items/remove` 一律拒绝（400，说明该组权限由目录计算）。

### 9.3 错误语义

优先复用既有 `BaseError` 变体与 HTTP 映射（`crates/yang-base/src/transport/axum.rs:1229-1257`），**不为个别用例扩展框架错误类型**：

| 场景 | 映射 | HTTP |
|---|---|---|
| 组不存在 | `RecordNotFound` | 404 |
| `group_key` 重复 | `ParamInvalid`（重复键经 `From<DbError>` 会映射为 `ParamInvalid(索引名)`） | 400 |
| 组内权限未在目录声明 | `ParamInvalid("permission")`（经 `ensure_declared`） | 400 |
| 删除仍有成员的组 | 应用层前置检查返回 `ParamInvalid("group_id", ...)` | 400 |
| 移除最后一名管理员 | `ParamInvalid("user_id", ...)` | 400 |
| 组权限变更时成员数超上限（§6.3） | `ParamInvalid("member_count", ...)` | 400 |
| 自提权尝试（子集校验失败） | `PermissionDenied` | 403 |
| 写操作缺少/无效/已消费的 Step-up proof | `StepUpRequired` | 428 |
| 目录未安装 | `ConfigError` | 500（fail-closed） |

只读 Action（`list_groups` / `get_group`）按 §9.2 修订不挂 Step-up，因此永不返回 428。

> **修订理由（对齐实现的事实订正）**：本表原先把「删除仍有成员的组」「移除最后一名管理员」定成
> HTTP 冲突码，这是**文档错、代码对**：`yang_base::BaseError` **没有 `Conflict` 变体**
> （框架只有 `ErrorCategory::Conflict` 这个错误分类），照写会直接编译不过。本着本节开头
> 「不为个别用例扩展框架错误类型」的原则，实现统一落在既有的 `ParamInvalid` → 400；
> 这三类都是「资源当前状态不允许该操作」的前置拒绝，与「用户名已存在」同一条路径，
> 前端按错误码而非字段名分支即可（见本节末尾的既有实现细节）。

**注意一处既有实现细节**：`From<DbError>` 会把可解析的唯一键冲突改写为 `ParamInvalid(索引名, "该值已存在")`，索引名取 MySQL `库.表.键` 的最后一段（`crates/yang-base/src/error/mod.rs:400-420`）。因此前端不能依赖固定字段名做分支，需按错误码而非字段名处理。

## 十、未采纳的方案与理由

本设计**不引入任何外部授权库或服务**。全部候选方案在第一性原理上撞同一堵墙：**本系统最稀缺、最难重建的性质是「授权事实变更与业务写处于同一个 MySQL 事务内」**（锁用户 → 写事实 → 递增版本 → 写 outbox，四步原子）。授权事实一旦外置，这条性质必须放弃，改造成两阶段提交或补偿同步。

| 方案 | Rust 就绪度 | 新增故障域 | 不兼容点 |
|---|---|---|---|
| **casbin-rs** 2.20（Apache 孵化） | 生产可用 | 否 | 鉴权语义进入运行时配置字符串（Rust 侧 rhai 求值），无法静态检查与 diff 评审；无类型、无引用完整性；`Enforcer` 非线程安全需 `Arc<RwLock<_>>`；多实例策略一致性只有 1 star 的 Redis watcher。与本仓库「构建期冻结 Catalog + 类型化权限」的方向相反 |
| **Zanzibar 系**（OpenFGA / SpiceDB / Ory Keto） | Rust 仅社区客户端 | **是**（独立 PDP + 独立库） | 授权写入无法加入现有 sqlx 事务，直接牺牲 §2.1 的同事务原子性；官方建议库与实例同机同网且独占。对扁平、单租户、无资源级分享的场景属过度设计 |
| **Cedar** 4.12（AWS） | 原生、活跃 | 否 | 通用 permit/forbid 引擎，不提供权限组的生命周期与审计；需维护 typed schema 并在每次判权构造 entity graph；不支持递归继承 |
| **Keycloak** | — | 是（Java） | role/group 双轨设计被官方自认冗余（*"Composite Roles 与 Groups 功能相同"*）；复合角色有 `O(C×M×D)` 实测性能问题；运维重 |
| **云 IAM 模型**（AWS/GCP/Azure） | — | — | **只借抽象，不借实现**：本设计的「权限集合 + 粘合表」两层结构、组不可嵌套、内置全权身份三点直接来自其共识结论 |

「全部权限」身份的业界做法（AWS root / GCP 超级管理员：标志位 + 鉴权短路 + 不可降权）也被明确排除，理由见 §3.1。

## 十一、前端

### 11.1 需要的改动

- 新增自定义视图承载「组 + 权限矩阵 + 成员列表」。通用 `TableView` 无法表达该结构，需在 `frontend/src/features/registry.ts` 的**静态注册表**中登记（该文件是字面量白名单，禁止按后端字符串动态 import）。
- 侧边栏/导航入口与路由。
- `frontend/tests/` 下镜像路径的 Vitest 回归测试；按契约变更重跑 `python scripts/dump_openapi.py` 并提交 `frontend/contracts/openapi.json` 与生成类型两个产物。

### 11.2 建议的顺带改进

`hasOperation(catalog, operationId)`（现位于 `frontend/src/features/feishu/api.ts:121-129`）建议下沉到 `engine/`，供权限组页面与后续业务域复用，避免每个域各自硬编码 operation_id 常量。

### 11.3 命名警告

**不要把新概念命名为 `PermissionGroup`**。框架已有同名结构（`crates/yang-base/src/router/middleware.rs:83-101`），语义是「某个 Action 要求的一组权限 + `All`/`Any` 匹配模式」，与本设计的「可指派给账户的权限集合」同名异物。新增类型统一使用 `Group` / `PermissionBundle` 词根。

## 十二、文档同步清单

本仓库对文档与代码同步要求严格，以下必须与实现同批提交：

| 文件 | 改动 |
|---|---|
| `docs/architecture/foundation-baseline.md:37` | **D2 修订**（§3.1） |
| `docs/architecture/foundation-baseline.md:39` | **D4 标注**已按预留接口扩展（§3.2） |
| `docs/contracts/AUTHZ_GRANTS.md` | 重写「初始授权」章节（引导为主路径、运维 SQL 为灾备）；新增权限组契约章节；更新权限目录来源描述（当前漏述 `module.default_permissions` 来源） |
| `docs/architecture/authorization-writers.md` | 登记新 writer：组生命周期、哨兵声明 |
| `docs/contracts/SCHEMA.md` | 无需改动（声明式 Schema 不变） |
| `AGENTS.md:7`、`AGENTS.md:35` | 「没有任何账号会成为系统最终管理员」「access 为预留端口、权限管理未交付」两处口径 |
| `docs/architecture/account-system-roadmap.md:32` | 「权限管理面整体不可达、属预留端口」的更正 |
| `frontend/AGENTS.md` | 新自定义视图的登记与分层归位 |

## 十三、验证矩阵

按 D5（TDD）：每阶段先在目标文件旁写失败的 `#[cfg(test)]` 测试，再实现到通过。集成测试需真实 MySQL 与 Redis（测试库名以 `_test` 结尾、Redis DB 15、单线程 `--test-threads=1`）。

| 变更 | 必需验证 |
|---|---|
| 组数据层 | `group_key`/`permission` 的 pattern 与 DB CHECK 生效；重复入组幂等且不递增版本 |
| 全权组解析 | `system_admin` 组的有效权限恒等于整个权限目录；**新增一个 Action 后该组自动覆盖新权限**（回归测试，防「物化」退化） |
| 失效传播 | 加组权限后成员旧 Token 在 outbox 窗口内失效；并发扇出无死锁（锁序测试）；成员数超上限时明确报错 |
| 引导 | **并发首注册恰好产生一个 owner**（多连接真实 MySQL 对抗测试）；`AlreadyClaimed` 降级不阻断注册；哨兵行已存在时不再产生第二个管理员 |
| 防自提权 | 为自己添加超集权限组被拒（403）；非 `system_admin` 成员不能修改该组成员；`system_admin` 成员的移出受限 |
| 最后管理员 | 最后一名 active 管理员不可被停用、不可被自我删除、不可被移出全权组；两管理员时可移除其一 |
| **成员关系锁序（收尾回合补）** | 零管理员态不可达：同一管理员并发移出自己 + 另一名、两名管理员互相移出，各若干轮（真库、多条独立连接、`tokio::sync::Barrier`），终态必须仍有 >=1 名启用管理员且两个请求不得都成功；并发加成员不得越过 `MAX_GROUP_MEMBERS`；**并发加两名不同成员到空组两次都必须 200（无死锁）**；满员时重复加已在组内的成员仍幂等成功；并发重复加同一成员两次都幂等成功（无死锁）；并发「移出成员」与「加权限」无死锁 |
| **Step-up 挂载（收尾回合补）** | `access.groups` 的**每个写 Action** 不带 proof 的调用必须被拒为 428（从冻结 Catalog 枚举、含此前零调用的 `update_group`），两个只读 GET 不得返回 428；单测另锁「Step-up 登记清单 == 冻结 Catalog 的写 Action 集合」 |
| 引用完整性 | 删除仍有成员的组被拒（400，见 §9.3）；账号删除后无孤儿 `authz_grant` 与 `user_group` 行 |
| 错误语义 | 各场景 HTTP 码与 §9.3 一致；前端不依赖字段名分支 |
| **非回归红线** | `tests/refresh_load_benchmark.rs` 不受影响（本设计不触碰校验热路径） |

## 十四、实施阶段与顺序

每阶段一次提交（D6），消息格式 `feat(access): ...`，body 说明边界与验证方式。跨仓库改动遵守「先推 lib_yang 再推 yang-system」。

| 阶段 | 内容 | 完成定义 |
|---|---|---|
| **1. 组数据层** | 三张组表 + `access.groups` module + 受信 Repository（writer 登记）+ 目录校验 | 建表启动成功；writer allowlist 门禁通过 |
| **2. 解析与失效** | `GroupGrantResolver` + 同步扇出 + 成员上限 + 锁序 | 成员旧 Token 在窗口内失效；并发扇出无死锁 |
| **3. 引导** | `system_owner` 表 + `SystemOwnerClaimer` 实现 + 端口加 `&ActionContext` + `AlreadyClaimed` 降级 | 并发首注册恰好一个 owner |
| **4. 防线与接口** | 子集校验 + 最后管理员守卫 + `delete_account` 清理孤儿授权 + 全部 Action | 提权尝试被拒；零管理员态不可达 |
| **5. 前端与管理面** | 自定义视图 + 路由 + 契约重生成 + 测试 | `run_ci.py quick` 与前端 `pnpm check` 通过 |
| **6. 文档同步** | §12 全部条目 | 文档与代码口径一致 |

**门禁**：每阶段 `python scripts/run_ci.py quick`；提交前 `cargo fmt --check` 与 `python scripts/check_architecture.py`；涉及真实依赖行为补 `run_ci.py integration`。

## 十五、已知限制

1. **组变更的失效窗口非零**。同步扇出发生在管理请求的事务内，但 `authz_version` 经 outbox 传播到 Redis 存在延迟，与今天 `grant_permission` 的窗口同级。已进入业务处理的在途请求不受影响。
2. **一个组的成员数有上限**。超限时加/删组权限会被拒绝并要求分批，这是刻意的可预测降级，而非静默性能劣化。
3. **`system_admin` 组在 UI 上无条目可展示**（其权限由目录计算）。接口以 `effective_all: true` 显式表达，前端需特判。
4. **孤儿权限条目不自动清理**，只在列表中标记。自动清理需要额外定义「何时算孤儿」的运维语义，本规格不做。
5. **不做组嵌套与角色继承**。若未来出现真实的层级归属需求（部门树、项目组），正确方向是评估资源级关系模型，而不是给权限组加嵌套。
6. **字段级可见性不随权限组改善**。本次交付后，`users` 表的 `email`/`email_verified_at`/`password_hash` 等字段仍只对 `system` 伪角色可读，而没有任何账户持有该角色——**因此持有 `account.users.read` 的管理员依然读不到邮箱列**（机制见 §2.3）。这是既有缺陷，不是本次引入的回归，但它会让「系统管理员」在用户列表页看不到邮箱，产品上大概率不可接受。两条修复路径：(a) 让权限组的 `group_key` 同时进入 Token 的 `roles`，使 `readable_by([...])` 能引用组名——这需要先决策「角色通道是否承载组语义」；(b) 把字段规则改为不依赖角色，另建权限语义。二者都是独立决策，本规格不预设，但**建议在阶段 5（前端）之前解决**，否则管理页会出现「有权限却看不到字段」的困惑。

## 十六、待确认事项

以下三点已在本规格中给出倾向与理由，但属于可推翻的选择，请在评审时确认：

1. **表的放置位置**（§5.5）：1 个 module + 扩 `infrastructure_definitions` 到 9。替代方案是每张表一个 module，代价是前端多出 3 个空模块。**本规格已按前者写定**。
2. **`authz_grant` 孤儿授权清理**（§8.3）：本次随 `delete_account.rs` 一并修。替代方案是单独排期。**本规格已按前者写定**。
3. **字段级可见性与管理员能否看到邮箱**（§15 第 6 条）：本次**不修**，但需要你决定是否把它提前到阶段 5 之前。若维持不修，交付后「系统管理员」在用户列表页看不到邮箱——这是既有缺陷，非本次回归。
