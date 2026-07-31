# ADR：授权版本与失效传播

状态：Accepted（C3-00）
日期：2026-07-26
适用范围：`account`、`admin`、`org` 的认证、授权快照和授权事实写入路径

## 1. 决策背景

当前登录与 refresh 都会从数据库重新计算角色和权限，再把结果写入 JWT。Access Token
在其有效期内只消费签发时的 claims；默认配置的 access TTL 为 3600 秒。因此，下列事实
变化不会主动使已有 Access Token 失效：

- `users.status` 停用；
- `admin_user.status` 或 `admin_user.admin` 变化；
- `org_user` 成员新增、停用、删除或管理员标记变化；
- `org_org.status` 变化导致一组成员失去有效组织；
- 后续新增的角色、策略或授权关系变化。

refresh 会重新读取事实，只能在客户端主动刷新后纠正 claims，不能替代服务端撤销。
逐 Token `jti` 黑名单适合单个会话撤销，但无法简洁覆盖“一个授权事实使该主体全部旧
Access Token 失效”。

当前源码还有两个一致性缺口：

1. 用户状态、授权版本和各领域 grant resolver 不是在同一个数据库快照中读取；
2. `admin_user` 写只有部分操作使用事务，`org_user` 写仍由通用 CRUD 承担，无法把业务
   事实与授权版本递增原子提交。

## 2. 决策

系统采用“数据库单调授权版本 + JWT 版本声明 + Redis 短 TTL 加速 + 事务 outbox”模型。

核心不变量：

1. MySQL 主库是用户状态、授权事实和 `authz_version` 的唯一事实源。
2. 每个用户拥有一个从 1 开始、只递增不回退的 `authz_version`。
3. 任何会改变该用户有效角色、权限或账号可用性的写操作，必须在同一 MySQL 事务内：
   写业务事实、递增版本、写入 outbox。
4. Access Token 必须携带生成授权快照时读取的同一 `authz_version`。
5. 受保护请求在权限判断和租户解析前比较 Token 版本与当前版本；不相等即失败关闭。
6. Redis 只缓存版本，不缓存角色/权限，不成为授权事实源。
7. outbox、Redis 重放、并发 writer 和缓存回填都不能使版本回退。
8. C3-03、C3-04 的所有 writer 完成前，不启用请求期强制比较。

这不是双授权运行时。最终只有一条请求授权链：

`验证 JWT/撤销状态 → 比较 authz_version → 构造 User → 权限检查 → 租户解析 → Action`

## 3. 数据模型

### 3.1 `users.authz_version`

在 `users` 增加：

```sql
authz_version BIGINT NOT NULL DEFAULT 1
```

约束与语义：

- 应用只接受 `authz_version >= 1`；
- Rust 使用 `i64`，递增前检查 `i64::MAX`，溢出时拒绝整个业务事务；
- 字段只允许受信 repository 读取和写入，不进入 `UserView`、表单或普通 CRUD；
- 新用户从 1 开始；迁移时既有用户统一回填 1；
- 不以时间戳代替版本，避免同秒并发、时钟回拨和多实例时钟差。

单一用户版本足以覆盖当前授权模型。若未来组织策略本身成为高频、独立版本化事实，再通过
新 ADR 引入策略版本；当前不预先维护多套版本或兼容链。

### 3.2 `authorization_outbox`

新增专用 outbox 表，至少包含：

| 字段 | 约束/用途 |
|---|---|
| `id` | `BIGINT` 自增主键，稳定队列顺序 |
| `user_id` | 受影响用户 |
| `authz_version` | 本次事务提交后的版本 |
| `state` | `pending / processing / published` |
| `attempts` | 重试次数 |
| `available_at` | 下次允许重试的 Unix 秒 |
| `lease_until` | worker claim 租约截止时间，可空 |
| `worker_id` | 当前 claim worker，可空 |
| `created_at` | 业务事务写入时间 |
| `published_at` | Redis 更新成功时间，可空 |
| `last_error` | 截断后的错误摘要，可空 |

索引和约束：

- `UNIQUE (user_id, authz_version)` 保证相同版本事件幂等；
- `INDEX (state, available_at, id)` 支持队列 claim；
- `INDEX (user_id, authz_version)` 支持主体审计；
- 已发布事件保留 7 天后分批清理，不执行无界单次删除。

目标运行时是 MySQL 8.0。2026-07-26 本地验证引擎为 MySQL Community 8.0.45。
worker 可以把 `FOR UPDATE SKIP LOCKED` 用于 outbox 队列 claim，但不得把它用于授权
一致性读取；MariaDB 在单独验证语义前不属于支持矩阵。

## 4. 授权事实与 writer 覆盖

只有“有效授权结果可能变化”的字段才递增版本。姓名、职务、邮箱、手机等纯展示字段不递增。
无实际值变化的幂等写不递增。

| 事实路径 | 影响版本的变化 | 原子事务要求 |
|---|---|---|
| `users` | `status`；后续直接角色/策略字段 | 用户事实 + 本用户版本 + outbox |
| `admin_user` | 新增、删除、`status`、`admin`、绑定用户变化 | 平台事实 + 目标用户版本 + outbox |
| `org_user` | 新增、删除、`status`、`admin`、`user_user`、`org_org` | 成员事实 + 变更前后用户版本 + outbox |
| `org_org` | `status` 或会改变成员授权的策略 | 组织事实 + 所有受影响用户版本 + 各自 outbox |
| `org.tenant.create` | 创建者获得有效管理员成员关系 | 组织、成员、创建者版本和 outbox 同事务 |

实现约束：

- `admin_user` 的 `bootstrap`、`add`、`set_status`、`set_admin` 全部进入显式 repository
  事务，不能在事务提交后再补版本。
- `org_user` 不再允许通用 CRUD 直接承担 `add/put/del`；保留读 Action，写 Action
  迁移到显式 repository 事务，并保持现有稳定 Action 名、路由与前端契约。
- 所有受影响的 `user_id` 去重后按升序锁定和递增；同一事务对同一用户最多递增一次。
- 多行领域事实也按稳定主键顺序锁定；外部网络调用不得发生在 MySQL 事务内。
- 死锁或 lock wait 只允许对整个事务做有界重试，不能只重试版本或 outbox 的一部分。

### 4.1 资源授权覆盖与线性化点

组织成员 mutation 不再由一个运行时 Action 名单决定是否执行 guard。`add`、`put`、
`del` 各自注册一个持有确定 `ActionRef` 的 middleware；模块测试把除已审计只读 Action
外的所有 Action 默认视为 mutation，因此新增写 Action 未增加对应 authorizer target 时
必须在构建测试阶段失败。

middleware 对当前管理员事实的查询只承担快速拒绝，不能作为写入授权结论。组织和平台
高危写入统一采用以下线性化语义：

> 业务事务内最后一次锁定并复核操作者当前管理员事实，是该写入的授权线性化点。
> 在线性化点前提交的撤权必须使写入失败；线性化点后到达的撤权等待在途事务提交，
> 已线性化的写入可以完成。

组织写事务按“操作者管理员成员行 → 组织行 → 目标成员行 → 受影响 users 行”的顺序
获取锁。平台 `add` 和密码重置签发先锁操作者平台管理员行，再锁目标 users 行；
`set_status` 和 `set_admin` 先按平台账号主键升序锁定全部 active 超级管理员，再锁目标
平台账号和受影响 users 行。检查失败、死锁或 lock wait 都必须回滚整个业务、版本、
outbox 与成功审计事务，禁止只重试其中一段。

单用户递增的逻辑顺序为：

```text
锁定并复核领域事实
  → 锁定 users 行
  → 判断授权相关值是否真实变化
  → 写领域事实
  → authz_version = authz_version + 1
  → 插入 (user_id, new_version) outbox
  → COMMIT
```

## 5. 签发期一致性

### 5.1 授权快照

引入单一 `AuthorizationSnapshot`：

```text
subject + username + active status + authz_version + roles + permissions
```

登录在密码校验成功后、refresh 在 Refresh Token 校验成功后，都调用同一个快照读取器。
读取器在 MySQL 主库的同一个显式 `REPEATABLE READ` 只读事务中：

1. 读取并校验 `users.status`、`users.authz_version`；
2. 让 account/admin/org grant resolver 复用同一事务读取授权事实；
3. 组装稳定排序、去重的 roles/permissions；
4. 提交只读事务后生成 Token 对。

若 writer 在快照前提交，快照读取新版本和新授权；若 writer 在快照后提交，Token 携带旧
版本并会在请求期被拒绝。禁止分别读取版本和 grants 后再“猜测”一致性。

### 5.2 JWT 契约

Access Token 和 Refresh Token 都写入严格的正整数声明：

```json
{
  "authz_version": 7
}
```

- Access Token 的角色、权限和版本必须来自同一个 `AuthorizationSnapshot`。
- refresh 不因自身版本旧而直接拒绝；它必须重新读取当前快照，用户停用时拒绝，否则签发
  当前版本和当前（可能已降级的）权限。
- C3-06 启用后，缺少、非整数、小于 1 或高于当前数据库版本的 Access Token 都失败关闭。
- 现有 access TTL 仍是会话寿命上限，不再是授权撤销延迟上限。
- 单个会话即时撤销继续使用 `jti` 黑名单；它与主体授权版本职责不同。

## 6. 请求期比较与缓存

Redis key：

```text
yang-system:{deployment}:authz:version:{user_id}
```

value 只保存十进制版本，TTL 固定为 5 秒。当前阶段不增加进程内 L1 缓存，避免额外一致性
层和多实例差异。

比较算法：

1. Token 版本无效：拒绝。
2. Redis 命中：
   - `current == token`：通过；
   - `current > token`：旧 Token，立即拒绝；
   - `current < token`：缓存可能落后，回查 MySQL，不能直接通过。
3. Redis miss、值损坏或连接失败：回查 MySQL 主库的用户状态和版本。
4. MySQL 返回当前启用用户且版本相等：使用 Lua “仅当 incoming 更大或 key 不存在时写入”
   Redis，并设置 5 秒 TTL，然后通过。
5. 用户不存在、已停用或版本不等：拒绝；当前版本大于 Token 时记录正常 stale 指标，
   Token 版本大于数据库时记录高优先级异常。
6. Redis 与 MySQL 都无法完成比较：返回可识别的 503 授权检查不可用错误，不进入权限判断。

Lua 更新不得在 incoming 小于或等于现值时延长 TTL，防止乱序事件或重复重放无限延长旧值。
Redis eviction、重启和冷启动都退化为数据库回查，不会恢复旧权限。

## 7. outbox 传播

worker 采用至少一次、可重放模型：

1. 在短 MySQL 事务内用 `FOR UPDATE SKIP LOCKED` claim 小批 pending/租约过期事件，写入
   `processing + worker_id + lease_until` 后提交；
2. 在事务外调用 Redis Lua 单调更新；
3. 成功后用短事务把事件标记为 `published`；
4. worker 崩溃时由租约到期重放；失败使用有上限的指数退避并保留 `last_error`；
5. 重复、乱序和“Redis 成功但 published 标记失败”都由单调 Lua 与唯一键保证安全。

默认目标：

- worker poll 间隔不高于 250 ms；
- 健康状态下授权变更到 Redis 可见的 p99 不高于 2 秒；
- 安全硬上限为数据库提交后 5 秒，由 Redis TTL + MySQL 回查共同保证；
- outbox 最老 pending 年龄超过 2 秒告警，超过 5 秒严重告警。

若 outbox 堵塞，已有 Redis key 最迟在 5 秒后过期并回查数据库，因此 outbox 是传播加速器，
不是安全正确性的唯一依赖。

## 8. 故障策略

| 场景 | 行为 |
|---|---|
| Redis 正常、版本相等 | 通过 |
| Redis 显示 Token 过期 | 401 `AUTHZ_STALE`（错误码 `400009`） |
| Redis miss/损坏/超时，MySQL 正常 | 回查主库；相等才通过 |
| Redis 重启 | 冷缓存回查主库，不使用旧权限 |
| outbox 延迟/worker 停止 | 最多等待 key 剩余 TTL；过期后回查主库 |
| MySQL 不可用但 Redis 有未过期有效版本 | 在 key 剩余 TTL 内按缓存比较；最坏陈旧窗口仍不超过 5 秒 |
| 比较所需的 Redis 与 MySQL 都不可用 | 503 `AUTHZ_CHECK_UNAVAILABLE`（错误码 `400011`），失败关闭 |
| Token 版本高于事实源 | 401 `AUTHZ_VERSION_INVALID`（错误码 `400010`）并告警 |
| MySQL 回查确认用户不存在或停用 | 401，不能由较低缓存版本重新放行 |

“失败关闭”指系统不能完成可信比较时不构造 `User`、不执行权限检查后的 Action；不要求把
Redis 单点故障扩大为全站故障，主库回退是可信比较而不是降级放行。5 秒 TTL 是明确接受的
授权陈旧上限，而不是依赖故障时无限延长的宽限期。

## 9. 可观测性

至少提供以下低基数指标：

- `authz_version_check_total{result=match|stale|invalid|unavailable,source=token|redis|mysql}`
- `authz_version_fallback_total{reason=miss|redis_error|malformed|cache_behind}`
- `authz_outbox_pending`
- `authz_outbox_oldest_age_seconds`
- `authz_outbox_publish_total{result=success|retry}`
- `authz_propagation_seconds`

应用进程在进入 HTTP 服务前校验 `authorization_outbox` 列与关键索引并启动
传播 Worker；退出 HTTP 服务后先停止 Worker，再关闭 Redis/MySQL。Worker 使用
`FOR UPDATE SKIP LOCKED` claim，Redis 调用位于 MySQL 事务外；失败采用有上界的
指数退避，租约过期、重复事件、乱序事件以及“Redis 成功但 DB 完成标记失败”均通过
至少一次重放与 Redis 单调脚本收敛。

日志包含 `request_id`、`user_id`、Token 版本、当前版本和稳定错误码；不记录 JWT、密码、
完整 claims 或 Redis 凭据。对版本高于事实源、版本回退企图和超过 5 秒的传播延迟告警。

## 10. 分阶段实施与启用

| 路线图 | 实施边界 | 启用状态 |
|---|---|---|
| C3-01 | `users.authz_version` Schema、迁移、repository 读取 | 不比较 |
| C3-02 | 统一快照事务；登录/refresh Token 写版本 | 不比较 |
| C3-03 | admin 全部授权 writer 原子递增 | 不比较 |
| C3-04 | org 成员/角色/onboarding writer 原子递增 | 不比较 |
| C3-05 | outbox worker、Redis 单调缓存、指标与重放 | 不比较 |
| C3-06 | writer 覆盖门禁通过后，一次性启用所有受保护请求比较 | 强制 |
| C3-07 | 停用、降级、移除、并发、Redis 重启真实集成矩阵 | 强制 |

C3-06 是破坏性安全切换：启用后，旧版无 `authz_version` 的 Access Token 立即失效。系统不
长期维护“有版本/无版本都接受”的双协议。客户端收到 `AUTHZ_STALE` 后只能用 refresh
重新取得当前快照，并且同一原请求最多自动重试一次。

## 11. 验收矩阵

- 用户停用后旧 Access Token 不超过 5 秒被拒绝，refresh 同样被拒绝；
- 超级管理员降级、平台账号停用后，旧 Token 不能继续平台写；
- 企业管理员降级、成员停用或移除后，旧 Token 不能继续成员写或租户访问；
- 授权提升后旧 Token 也被判 stale，refresh 后才获得新权限；
- 并发 writer 的版本严格递增，outbox 不缺失、不重复生效、不回退 Redis；
- 业务事务回滚时版本和 outbox 一并回滚；
- Redis 重启、eviction、乱序重放、worker 崩溃与 MySQL 回退均失败关闭；
- 架构门禁枚举所有授权事实 writer，禁止通用 CRUD 或未登记 raw SQL 绕过版本递增。

## 12. 代价与被拒绝方案

代价：

- 授权相关写从通用 CRUD 收敛到显式 repository 事务；
- 受保护请求增加一次短 TTL 版本读取，冷缓存或 Redis 故障时增加主库读；
- 引入 outbox worker、重试、清理和告警运维面。

被拒绝：

- 只缩短 Access Token TTL：撤销延迟仍不可控，并增加持续 refresh 压力；
- 只做 `jti` 黑名单：角色变化需要枚举主体全部 Token，状态复杂且易漏；
- 只靠 Redis Pub/Sub：消息不持久，订阅者离线会丢失；
- Redis 保存完整权限并作为事实源：产生第二授权数据库和双写一致性问题；
- 在业务事务提交后再递增版本或写 outbox：进程崩溃会永久漏失失效事件；
- MySQL trigger 隐式递增：隐藏跨领域业务语义，不利于锁顺序、outbox 和代码审查；
- 在 C3-03/C3-04 writer 未覆盖时提前比较：会制造“部分写会失效、部分写不会”的虚假安全。
