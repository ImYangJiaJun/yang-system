# 授权存储与权限目录契约

**生成：** 2026-09-03
**更新：** 2026-09-26（新增「管理员等价权限」显式清单与授予闸门；账号管理拆分出独立的凭据签发权限 `account.users.reset_credentials`）；
2026-09-24（首账号引导成为初始授权主路径；新增一层权限组）
**范围：** `access` Addon（`src/addon/access/`）提供的权限基础设施：权限目录、
直授存储、权限组、Token 授权快照扩展与授权管理接口。

## 权限模型

- 权限是点分隔的小写字符串（如 `access.grants.read`），格式由
  `PERMISSION_PATTERN`（`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`，最长 128 字符）约束，
  数据库层以 `chk_authz_grant_permission_format` / `chk_permission_group_item_permission_format`
  两条 CHECK 兜底。权限字符串**不允许通配**：`*` 与 `system.*` 均非法。
- 权限目录 = 运行期从冻结 Catalog 投影的**并集**（决策 D3，单一事实来源，无静态清单）：
  Module 的 `default_permissions` 与 Module 内全部 Action 声明的 `.permissions(...)`
  （`src/addon/access/domain/permission_catalog.rs` 的 `project_permissions`，两者取并集后
  按权限字符串稳定排序，声明者按操作 ID 稳定排序去重）。组合根在 `AppBuilder::build`
  后安装投影（`src/app.rs`），运行期只读；目录未安装时 `entries()` / `ensure_declared`
  一律 fail-closed（`ConfigError`）。每条条目另带一个**危害面标记** `admin_equivalent`
  （见「管理员等价权限」节），随目录读接口 `GET /api/v1/access/permissions` 一起返回。
- **有效权限 = 直授 ∪ 组权限**：`authz_grant` 的直授行，以及该用户所属全部权限组的
  贡献（普通组取条目与目录的交集；内置全权组取**整个目录**，见下）。Token 中的角色
  仍为账号域固定的 `user`（组名不写入 `roles`），权限由 access 的
  `AuthzGrantResolver` 与 `GroupGrantResolver` 在签发/刷新时分别读入 claims。
- 只能授予目录中已声明的权限；未声明的权限在管理接口处被拒绝（fail-closed）。
- 新增权限：`access.groups.read`、`access.groups.write`（权限组管理面，见「权限组」）。

### 账号管理的两个写权限（凭据签发已拆分）

账号域管理面曾把「日常启停」与「凭据签发」混在同一个 `account.users.manage` 下。二者
危害面完全不同：前者只改账号状态，后者对**路径参数指定的任意账号**签发一次性密码重置
凭证——凭该凭证可重置口令并登录成目标（含系统管理员），一步拿到全部权限，于是
`account.users.manage` 实质等价于 root。现拆分为两条独立权限：

| 权限 | 覆盖的 Action | 危害面 |
|---|---|---|
| `account.users.manage` | `admin_disable_user`、`admin_enable_user` | 改变账号启用状态；受 spec §8.1 admin-only 守卫与 §8.2 最后管理员守卫约束 |
| `account.users.reset_credentials` | `admin_issue_password_reset` | 为任意账号签发密码重置凭证，可夺取该账号（含系统管理员）；受 Step-up 保护 |

拆分后两条权限各自独立授予：只持 `account.users.manage` 不再能签发重置凭证。二者都由
Action 的 `.permissions(...)` 声明并经冻结 Catalog 自动投影进权限目录，内置全权组
（`system_admin`）按目录自动持有二者。两条中只有 `account.users.reset_credentials` 是
「管理员等价权限」（见下节），`account.users.manage` 不是。

### 管理员等价权限（清单与授予闸门）

有些权限的危害面不是「能改某个业务对象」，而是**能获得或夺取其他主体的凭据/身份，或能绕过
其余一切授权检查**。把这样的权限授予任何人（包括授予别人），都等于把管理员身份转授出去，
因此不能让只持 `access.grants.write` / `access.groups.write` 的委派者自行完成。

**清单是代码侧的显式、可评审事实**（`src/addon/access/domain/sensitive_permissions.rs` 的
`ADMIN_EQUIVALENT_PERMISSIONS`，唯一事实来源），当前恰有 3 条，每条附中文理由（理由会拼进
403 的拒绝信息，因此在生产路径上真的被读取）：

| 管理员等价权限 | 为什么等价 |
|---|---|
| `account.users.reset_credentials` | 对任意账号签发密码重置凭证：凭此重置其口令并登录成他，即夺取该账号的身份与全部权限 |
| `feishu.datasource.secret` | 回显数据源封存的 Token 明文；该 Token 是数据源主体的入站凭据，拿到即可冒充该数据源调用本系统，不再经任何授权检查 |
| `feishu.datasource.write` | 创建与轮换数据源都会把新凭据的明文返回给调用者（`create_datasource_table` / `rotate_token`），签发即持有，与直接读取凭据等价 |

清单刻意**不含** `access.grants.write` / `access.groups.write` / `account.users.manage`：前两者能
改动授权事实，但授予侧的闸门让它们无论如何都授不出管理员等价权限，是「危害面被闸门封顶的
委派权限」，一并列入只会把正常运营彻底锁死；`account.users.manage` 只能停用/启用账号（受
「只有全权组成员能修改全权组成员」守卫约束），既不签发凭据也夺不走身份。

**目录标记**：`project_permissions` 对每条已声明权限查该清单，把结果写进
`PermissionEntry::admin_equivalent`，随目录读接口返回。前端据此把危害面显示出来——
「可配置的前提是每个权限的危害面可见」。

**授予闸门**：向任何主体授予或传递管理员等价权限时，要求调用者**本身是内置全权组
`system_admin` 的成员**（判据是组成员身份，而不是「调用者是否持有该权限」）。三条路径都被
覆盖：

| 路径 | 接口 | 守卫 |
|---|---|---|
| 直接授予 | `POST /api/v1/access/grants` | `ensure_may_grant_permission_in_tx`（判据看本次要授予的权限） |
| 把该权限加进组 | `POST /api/v1/access/groups/items` | 同上（判据看本次要加的权限） |
| 把用户加进**已持有**该权限的组 | `POST /api/v1/access/groups/members` | `ensure_may_modify_members_of_group_in_tx`（判据看目标组的条目：变的是成员，转授的是组已有的全部权限） |

被拒时返回 `PermissionDenied` → **403**，消息点名该权限与理由。**fail-closed**：内置全权组
不存在时视为无人在组内，一律拒绝——此时没有任何主体有资格授予管理员等价权限。首账号引导
（`SystemOwnerClaimer`）**不经这条闸门**（它直接建组成员行，不「授予权限」），因此闸门不会挡住
引导。权限改名后清单会静默失效，故两处测试钉住它：单测
`every_listed_permission_is_declared_by_the_catalog` 用真实冻结 Catalog 断言清单每条仍被声明；
集成测试 `the_permission_catalog_marks_exactly_the_admin_equivalent_permissions` 从目录读接口
整体比对「被标记集合 == 清单」。

## `authz_grant` 表

| 列 | 类型 | 语义 |
|---|---|---|
| `id` | 自增主键 | 事实行标识 |
| `user_id` | BIGINT | 被授权用户 |
| `permission` | VARCHAR(128) | 权限字符串 |
| `granted_by` | BIGINT | 授权操作人用户 ID（灾备 SQL 引导时填操作者标识或 `0`） |
| `occurred_at` | BIGINT | 授权发生的 Unix 时间戳（插入时自动写入） |

约束：`uk_authz_grant_user_permission (user_id, permission)` 复合唯一索引；
`chk_authz_grant_permission_format` 权限格式 CHECK。表结构由
`src/addon/access/grants/table.rs` 声明，启动时增量同步，不使用 SQL 迁移文件。

## 权限组

一层聚合：**组 = 权限的命名集合**。用户可属于多个组，有效权限是各组贡献与直授的并集。
**不允许组嵌套组，也不做角色继承**（决策 D4 的边界，理由见设计 §3.2）。

### 三张表

`permission_group`（module `access.groups` 的表，唯一具备独立 UI 语义的一张）：

| 列 | 类型 | 语义 |
|---|---|---|
| `id` | 自增主键 | 组标识 |
| `group_key` | VARCHAR(64) | 稳定机器标识，`^[a-z][a-z0-9_]*$`；UNIQUE `uk_permission_group_key` |
| `title` | VARCHAR(128) | 展示名，可改；不作为引用锚点 |
| `description` | VARCHAR(255) 可空 | |
| `created_by` | BIGINT 必填 | 创建人 `users.id` |
| `occurred_at` | 时间戳 | `created_at()` 自动写入 |

`permission_group_item`（运行支撑表）：`id`、`group_id`（必填）、`permission`
（`PERMISSION_PATTERN` + `chk_permission_group_item_permission_format`）、`granted_by`、
`occurred_at`；UNIQUE `uk_permission_group_item (group_id, permission)`。
**该表刻意不声明外键**，因此删除组时由应用层在同一事务内显式清理条目行
（`delete_items_of_group_in_tx`），数据库不会替我们清。

`user_group`（运行支撑表）：`id`、`user_id`、`group_id`、`granted_by`、`occurred_at`；
UNIQUE `uk_user_group (user_id, group_id)`；两条外键 `fk_user_group_user → users.id`、
`fk_user_group_group → permission_group.id`，规则为框架默认的 **RESTRICT**——删除仍有
成员的组会被数据库直接拒绝，与应用层前置检查构成纵深防御。

`system_owner`（运行支撑表，语义上的单行哨兵）：`id`、`sentinel_key`
（UNIQUE `uk_system_owner_sentinel` + CHECK `chk_system_owner_sentinel`，锁死取值
`'system-owner'`）、`user_id`、`claimed_at`。**并发仲裁机制**：唯一约束与 CHECK 的组合
使第二个插入者必然违反其一，引导不依赖任何「判空」判断。

三张运行支撑表由 `src/addon/access/domain/groups/tables.rs` 声明，进入
`infrastructure_definitions()`（数组长度由 6 扩为 9），`permission_group` 由
`src/addon/access/groups/table.rs` 声明。四张表全部走声明式 Schema，零 SQL 迁移文件。

### `system_admin` 内置全权组

- `group_key = 'system_admin'` 是代码常量（`SYSTEM_ADMIN_GROUP_KEY`），不另设 `kind` 列，
  唯一性由 `uk_permission_group_key` 保证。
- **该组的有效权限 = 整个权限目录**，是**解析期规则**，不落任何存储行
  （`resolve_group_permissions`）。因此未来新增 Action 声明的权限自动纳入管理员权限；
  反过来说，若把当时的权限物化进表，管理员会静默失去对新模块的访问。
- 组的条目表里没有可展示的授权事实：列表接口以 `is_builtin`、详情接口以
  `effective_all: true` 显式表达，前端必须特判。
- 内置组受保护：`update` / `delete` / `items` / `items/remove` 对它的调用一律拒绝
  （`ParamInvalid`，说明该组权限由目录计算）；修改它的**成员**要求调用者**已经是该组
  成员**（防「刚被移出全权组的账户立刻把自己加回去」）。
- 目录未安装时全权组解析 fail-closed（`ConfigError`），不会退化成空集或全权集。

### 失效传播（同步扇出）

| 变更 | 事务内动作 |
|---|---|
| 加/删组成员 | 锁该成员行 → 递增其 `authz_version` → 追加 `authorization_outbox` |
| 加/删组权限 | 锁**全部成员**（按 `user_id` 升序固定锁序，防死锁的唯一手段）→ 逐个递增 → 逐个追加 outbox |
| 改 `title` / `description` | **不触发失效**（展示字段不是授权事实） |
| 建组 | 不触发失效（组本身不是权限） |
| 删除组 | 先要求组内无成员，否则拒绝 |

`MAX_GROUP_MEMBERS = 200`：加/删组权限会先检查成员数，超限时**明确报错并要求分批处理**，
而不是静默做一次 O(N) 行锁事务。已停用用户在扇出时跳过（其 Token 本就不可用）。

### 防自提权不变量（决策 D2 的实质）

**不变量**：任何一次组管理操作，都不得使调用者自身的有效权限集合增大。

- 机械校验：`before = 调用者当前有效权限`，`after = 模拟本次操作后的有效权限`，
  若 `after ⊄ before` 则拒绝（`PermissionDenied`，403）。两个快照必须与
  `GroupGrantResolver` 走同一条解析路径（`effective_permissions_of_in_tx` /
  `simulate_after_join`），避免校验与实际解析漂移。
- 覆盖的两条路径：把自己加入一个组；给自己**已属于**的组添加一条自己尚不持有的权限。
- **不受此限**：修改**他人**的成员关系、修改自己**不所属**的组（这些是正常的授权管理
  行为）。`system_admin` 成员本就持有全部权限，`after ⊄ before` 恒不成立，因此不受约束。
- 移出成员**不做**该校验：移出只会减少权限，且管理员必须能退出全权组。

### 最后一名管理员守卫

**管理员的定义**：属于 `system_admin` 组、且 `status = 'active'` 的用户。

| 入口 | 守卫 |
|---|---|
| `disable_self.rs`（自助停用） | 操作后系统若将无 active 管理员则拒绝 |
| `delete_account.rs`（自助删除） | 同上守卫；并清理该用户的 `authz_grant` 与 `user_group` 行（防孤儿授权） |
| `admin_disable_user.rs`（管理停用） | 保留既有自指防护，另加「最后一个」防护 |
| `POST /api/v1/access/groups/members/remove` | 移出 `system_admin` 组的操作受同一守卫 |

### 幂等语义

重复添加成员/条目、重复移除、移出本就不在组内的用户，都返回 `changed: false`：
**不递增任何人的授权版本、不写 Outbox、不写审计事件**（与 grants 的既有契约一致）。
建组时 `group_key` 撞唯一键由 `From<DbError>` 折算成 `ParamInvalid`。

### 错误码表

优先复用既有 `BaseError` 变体与 HTTP 映射（`crates/yang-base/src/transport/axum.rs`），
不为个别用例扩展框架错误类型。

| 场景 | 映射 | HTTP |
|---|---|---|
| 组不存在 | `RecordNotFound` | 404 |
| 目标用户不存在（加成员前的前置读取） | `UserNotFound` | 404 |
| `group_key` 重复 | `ParamInvalid`（唯一键冲突经 `From<DbError>` 折算，索引名 `uk_permission_group_key`） | 400 |
| 组内权限未在目录声明 | `ParamInvalid("permission")`（经 `ensure_declared`） | 400 |
| 内置 `system_admin` 组被改/删/增删条目 | `ParamInvalid("group_id")` | 400 |
| 删除仍有成员的组 | `ParamInvalid("group_id")`，消息含实际成员数 | 400 |
| 组成员数超过上限 | `ParamInvalid("member_count")`，消息含实际数与上限 | 400 |
| 移除最后一名 active 管理员（移出组 / 停用 / 删除） | `ParamInvalid("user_id")` | 400 |
| 自提权尝试（子集校验失败） | `PermissionDenied`，消息点名新增的权限 | 403 |
| 非 `system_admin` 成员修改该组成员 | `PermissionDenied` | 403 |
| 非 `system_admin` 成员授予/传递管理员等价权限（直授、加组条目、加组成员） | `PermissionDenied`，消息含权限名与理由 | 403 |
| 目录未安装 | `ConfigError` | 500（fail-closed） |

**两处口径提示**：其一，设计 §9.3 曾把「删除仍有成员的组」「移除最后一名管理员」
「成员数超上限」列为 `Conflict`/409，但 `yang_base::BaseError` **没有 `Conflict` 变体**
（409 只由 `ErrorCategory::Conflict` 的极少变体触发），且同节要求不为个别用例扩展框架
错误类型，因此这三类「资源状态冲突」统一沿用 `ParamInvalid` → **400**。其二，
`From<DbError>` 会把可解析的唯一键冲突改写为 `ParamInvalid(索引名, "该值已存在")`，
索引名取 MySQL `库.表.键` 的最后一段，因此前端**不能依赖固定字段名做分支**，需按错误码处理。

## 写入一致性

授权事实的变更必须在同一事务中完成三件事（授权 writer 契约）：

1. 写业务事实（`authz_grant` 事实行；或组事实与成员关系的增删，持有目标用户行锁）；
2. 经账号安全版本原语单调递增目标用户的 `users.authz_version`（凭据版本不变，
   Refresh 会话保持有效，用户刷新后透明获得新授权快照）；
3. 追加 `authorization_outbox`（由版本原语内建完成）。

writer 边界登记见 `docs/architecture/authorization-writers.md`：
`access-grant-lifecycle`（直授事实行）与 `access-group-lifecycle`（组、组条目、
用户-组关系的事实行）负责事实写入，版本与 Outbox 复用 `account-security-version`，
任何代码不得绕过这些 writer。

## 初始授权：应用引导（主路径）

**首个成功注册的账号在同一事务内被引导为系统管理员**（决策 D2 修订版），不再需要运维
先手插一条授权。`register.rs` 事务内的固定顺序：

1. `INSERT users`；
2. `claimer.claim(tx, ctx, user_id, username)`：
   `INSERT system_owner(sentinel_key = 'system-owner', user_id)`
   - 唯一约束/CHECK 冲突 → `AlreadyClaimed`：**正常业务结果，降级为普通用户，事务照常提交**
     （绝不 `?` 上抛，否则「首个用户因哨兵竞争失败而注册失败」）；只有真实数据库故障才回滚；
   - 插入成功 → `Claimed`；
3. `Claimed` 时：确保 `system_admin` 组存在（按 `group_key` 惰性 INSERT）→ 插入
   `user_group` 成员行 → 递增 `authz_version` + 追加 Outbox → 写 `first-registration`
   审计事件。

并发下多个注册请求只有一个能持有哨兵行，因此**恰好产生一个管理员**；仲裁发生在数据库层，
应用层不判空。运维可经 `GET /api/v1/access/groups`（内置组的 `is_builtin` /
`effective_all`）与 `GET /api/v1/access/groups/{id}` 的成员列表确认引导结果。

**与管理员等价权限闸门的关系**：管理员等价权限（见前文「管理员等价权限」节）只能由
`system_admin` 组成员授予。引导恰好在同一事务内建立 `system_admin` 组、把首个账号加为成员，
因此引导完成后系统**总有一位有资格授予这类权限的成员**，不会陷入「需要管理员才能造出管理员」
的死锁；而引导完成前（无全权组成员）任何管理员等价权限都授不出去，这是 fail-closed 的预期
行为（正好挡住「先靠一条伪造/误授的管理员等价权限自举成管理员」）。灾备 SQL 路径直接写真事实行、
不经应用层闸门；但若改用管理接口授予这类权限，操作者同样必须是该组成员。

## 灾备路径（运维 SQL）

当哨兵行被误删、或需要在数据库被清空后重新引导时，运维仍可**直接经 SQL** 完成初始授权。
这条路径是灾备，不是主路径，但必须遵守与在线 writer 相同的一致性要求——同一事务三件事：

```sql
START TRANSACTION;

-- 1. 锁定目标用户并观察当前授权版本（假设目标用户 id = 1）
SELECT id, authz_version FROM users WHERE id = 1 AND status = 'active' FOR UPDATE;

-- 2. 写入直授事实（granted_by 填执行运维的操作者标识，无账号时用 0）
INSERT INTO authz_grant (user_id, permission, granted_by, occurred_at)
VALUES (1, 'access.grants.write', 0, UNIX_TIMESTAMP())
     , (1, 'access.grants.read', 0, UNIX_TIMESTAMP());

-- 3. 单调递增授权版本（带上一步观察到的版本做乐观校验）
UPDATE users SET authz_version = <观察值 + 1> WHERE id = 1 AND authz_version = <观察值>;

-- 4. 追加授权 Outbox（与在线 writer 相同的事实形态）
INSERT INTO authorization_outbox
    (user_id, authz_version, state, attempts, available_at, created_at)
VALUES (1, <观察值 + 1>, 'pending', 0, UNIX_TIMESTAMP(), UNIX_TIMESTAMP());

COMMIT;
```

目标用户的存量 Access Token 随即失效，刷新后获得包含新权限的 claims。
撤销运维授权同理：同事务 `DELETE` 事实行 + 递增版本 + 追加 Outbox。

**重新引导管理员**同理，但要在同一事务里同时写三处事实（否则会留下「有哨兵、无管理员」
的半成品）：`system_owner` 哨兵行、`system_admin` 组的 `user_group` 成员行、递增版本
（该组的有效权限由目录计算，无需写条目）。已有哨兵行时，重复插入会先被数据库拒绝，
这是预期行为——重新引导前必须先显式删除旧哨兵行。

## 管理接口

| 接口 | 路由 | 所需权限 | Step-up |
|---|---|---|---|
| 授予权限 | `POST /api/v1/access/grants` | `access.grants.write` | 是 |
| 撤销权限 | `POST /api/v1/access/grants/revoke` | `access.grants.write` | 是 |
| 查询用户授权 | `GET /api/v1/access/users/{user_id}/grants` | `access.grants.read` | 否 |
| 查询权限目录 | `GET /api/v1/access/permissions` | `access.grants.read` | 否 |
| 建组 | `POST /api/v1/access/groups` | `access.groups.write` | 是 |
| 改组展示信息 | `POST /api/v1/access/groups/update` | `access.groups.write` | 是 |
| 删组 | `POST /api/v1/access/groups/delete` | `access.groups.write` | 是 |
| 组列表 | `GET /api/v1/access/groups` | `access.groups.read` | 否 |
| 组详情 | `GET /api/v1/access/groups/{group_id}` | `access.groups.read` | 否 |
| 加组权限 | `POST /api/v1/access/groups/items` | `access.groups.write` | 否 |
| 移组权限 | `POST /api/v1/access/groups/items/remove` | `access.groups.write` | 否 |
| 加组成员 | `POST /api/v1/access/groups/members` | `access.groups.write` | 否 |
| 移组成员 | `POST /api/v1/access/groups/members/remove` | `access.groups.write` | 否 |

说明：

- **Step-up 覆盖范围是建/改/删组三个入口**（`access/groups/mod.rs` 的 `step_up_targets`，
  与 grants 只覆盖 grant/revoke 同例）。组条目与组成员的变更**不挂重认证中间件**，其防线
  是「防自提权子集校验 + 最后管理员守卫 + Step-up 保护的组生命周期」——即攻击者无法凭空
  获得一个自己能写的新组，也无法越过后两条不变量。
- **管理员等价权限的授予闸门**：`POST /access/grants`、`POST /access/groups/items`、
  `POST /access/groups/members` 三条路径在涉及管理员等价权限（见「管理员等价权限」节）时，
  要求调用者是内置全权组 `system_admin` 成员，否则 403。这三条接口本身仍分别要求
  `access.grants.write` / `access.groups.write`，闸门是在其之上追加的一层主体判据。
- **全部写操作**（组生命周期、条目、成员）都写 append-only 审计，按
  `docs/contracts/AUDIT.md` 契约记录。
- 加组权限经 `ensure_declared` fail-closed；**移组权限刻意不做目录校验**，已从 Catalog
  移除的权限也必须能清理（对齐 grants 的反向宽容语义）。
- 组条目详情中「不在当前权限目录内」的条目以 `is_orphan` 标记，列表给出 `orphan_item_count`：
  **只报告不自动清理**（孤儿条目在解析期被静默丢弃，因此是 fail-closed 的，不会放大权限）。

## 飞书集成的初始授权

`feishu.datasource.read` / `feishu.datasource.write` / `feishu.option.read` 三个权限随
Catalog 冻结自动进入权限目录（可用 `GET /api/v1/access/permissions` 核实）。**首个授权
不再需要手工 SQL**：被引导为系统管理员的账号持有全部权限（内置全权组的权限等于目录），
可直接经 `POST /api/v1/access/grants` 授予其它账号；只有在引导不可用（哨兵行被误删、
数据库刚被清空）时才走上文「灾备路径（运维 SQL）」，把权限换成：

```sql
INSERT INTO authz_grant (user_id, permission, granted_by, occurred_at)
VALUES (1, 'feishu.datasource.read',  0, UNIX_TIMESTAMP())
     , (1, 'feishu.datasource.write', 0, UNIX_TIMESTAMP())
     , (1, 'feishu.option.read',      0, UNIX_TIMESTAMP());
```

日常授权/撤销走 `POST /api/v1/access/grants[/revoke]`（Step-up 保护）。

**注意**：`approval_options` / `upsert_options` / `delete_options` 三条机器入口是
`public` Action，**不经过权限体系**——它们的凭证分别是「按数据源存储的 Token 摘要」与
`[feishu].management_api_token`，因此不需要（也不应该）在这里授权。给它们授权不会
产生任何效果，反而会掩盖「凭证到底由谁校验」这个问题。
