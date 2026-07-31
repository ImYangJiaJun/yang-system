# yang-system 账号与授权体系第一性原理评审（最终可实施版）

> - 评估对象：`D:\code\lib_yang\project\yang-system` 的 account、org、admin Addon、授权新鲜度链路，以及根仓库 `yang-base` 的 Token/Step-up/中间件契约
> - 原始评估快照：`464f1693fdf447f24bdda1d95d71b818ee839afc`
> - 本次复核快照：`677e127daf18a39a4d7aefdcb0efd4e386d0f90e`
> - 评估日期：2026-07-30
> - 文档性质：基于当前源码的设计评审与实施规范，不表示下述改造已经完成

## 一、结论

当前系统已经具备可信的账号与授权基础，但不能据此认定为“生命周期闭环”或“权限体系已经完整”。

已经成立的核心能力：

- 密码使用 Argon2 路径校验，密码摘要与授权版本受字段角色和私有仓储边界保护；
- Access Token 携带授权快照，`authz_version` 以 MySQL 为事实源、Redis 为短 TTL 单调缓存；
- 授权事实变更、版本递增和 outbox 写入可以处于同一 MySQL 事务；
- Redis 授权版本读取失败时会回源 MySQL；只有事实源也不可用时才 fail-closed；
- Refresh Token 轮换以 Redis `SET NX EX` 原子消费旧 JTI，可阻止并发重复刷新；
- 租户成员身份和企业状态按请求从数据库确认，组织成员写操作另有实时管理员校验；
- 现有 logout 使用 subject 撤销水位，语义实际上是“该用户全部既有 Token 失效”，不是只退出当前设备。

尚未成立的关键能力：

1. `authz_version` 能让旧 Access Token 失效，但当前 Refresh 流程不比较旧 Refresh Token 中的版本；旧 Refresh Token仍可按最新账号与权限重新签发 Token。因此“递增授权版本等于踢掉全部会话”不成立。
2. 组织管理员 guard 仍通过 Action 名称判断覆盖范围，而且校验发生在业务事务之前；这既有漏挂风险，也存在管理员身份在校验后被撤销的并发窗口。
3. 平台管理员高危写操作只有 Token 权限检查，没有与组织成员写路径同等级的实时管理员事实校验。
4. Step-up 基础组件已存在，但生产接入还缺凭据复核、独立密钥、Redis proof 一次性消费、资源指纹、HTTP 428 交互、限流和前端流程，不能按“低成本挂上即可”估算。
5. 用户改密、可信重置、自停用、跨域“最后管理员”不变量和高风险失败审计尚未闭环。
6. `users.status`、外键策略和授权事实写边界仍依赖应用纪律；这类约束应由类型、数据库和架构门禁共同承担。

因此，正确结论是：

> 当前实现是“基础机制较强、语义接缝仍需封口”的系统。优先工作不是继续堆功能，而是先把会话失效、资源授权线性化点、跨域不变量和生产 Step-up 契约定义清楚，再实现账号生命周期。

## 二、评估方法与证据边界

### 2.1 第一性原理

账号与授权系统必须分别回答以下问题，不能用一个版本号或一个中间件模糊替代：

| 问题 | 唯一事实源 | 必须成立的不变量 |
|---|---|---|
| 身份 | `users.id` | 用户 ID 不随用户名或档案变化 |
| 凭证 | `users.password_hash` | 明文不落库、不进日志、不进入审计摘要 |
| 会话 | Token 撤销事实、凭证/会话版本 | 改密或全量撤销后，旧认证会话不能继续刷新 |
| 全局授权 | 角色、权限事实与 `authz_version` | 旧 Access 授权快照不能在事实变化后继续长期生效 |
| 资源授权 | 当前租户、成员和管理员事实 | 高危写入必须在明确的授权线性化点校验当前事实 |
| 状态 | `users.status` 等数据库字段 | 非法状态不能进入事实源 |
| 问责 | 追加式审计事实 | 成功、拒绝、失败均可关联 request/actor/target，且不泄密 |
| 可用性 | Redis/MySQL/密钥与限流依赖 | 故障行为、状态码、降级路径和告警必须可预测 |

会话失效与授权新鲜度是两个不同问题：

- `authz_version` 回答“这份权限快照是否仍然新鲜”；
- subject/JTI 撤销或 `credential_version` 回答“这个认证会话是否仍然允许延续”。

把两者混为一谈，会直接导致改密、logout 和 Refresh 语义出错。

### 2.2 已验证证据

本次复核检查了以下实现：

- `src/modules/account/user/claims.rs`
- `src/modules/account/user/actions/{refresh,logout}.rs`
- `src/modules/account/authz_version.rs`
- `src/authorization/{request_validator,version_cache,worker}.rs`
- `src/modules/org/{tenant.rs,user/guard.rs,user/repository.rs}`
- `src/modules/admin/{mod.rs,user/actions/bootstrap.rs,user/repository.rs}`
- `src/modules/account/user/{schema.rs,repository.rs}`
- 根仓库 `crates/yang-base/src/action/{auth.rs,step_up.rs}`
- 根仓库 `crates/yang-base/src/token/{manager.rs,revocation.rs}`
- 根仓库 `crates/yang-base/src/router/middleware.rs`
- 迁移约束文档 `docs/MIGRATIONS.md`

复核时执行：

```text
python scripts/check_architecture.py
# 通过

cargo test --lib
# 99 个测试：96 passed，3 ignored
```

3 个 ignored 测试依赖真实 MySQL/Redis，不能算作已通过。本次没有连接生产数据库，也没有验证生产 MySQL 精确版本、表规模、锁等待、Redis 容量、真实流量或远程 CI；涉及这些事实的方案均设置了部署前门禁。

## 三、当前运行时真相

### 3.1 Access 授权新鲜度链路

当前授权事实变更链路可以概括为：

```text
授权事实写事务
  ├─ 锁定受影响用户
  ├─ 递增 users.authz_version
  └─ 写 authorization_outbox
        ↓
worker 单调发布 Redis
        ↓
AuthorizationVersionValidator
  ├─ Redis 命中：比较 Token 与缓存版本
  ├─ Redis 故障/未命中：回源 MySQL
  └─ MySQL 也不可用：AuthorizationCheckUnavailable
```

这个设计的事实源、缓存和可靠投递边界是正确的，但需要纠正三个表述：

1. Redis 故障不是立即拒绝请求，而是回源 MySQL；
2. 授权校验不可用在 HTTP 层映射为 503，不是“全站 401”；
3. 对新请求，撤权残余窗口主要受 outbox 发布延迟与当前 5 秒缓存剩余 TTL 影响，通常取两者形成的实际可见时间，而不是“outbox 延迟 + Access Token TTL”。已经进入业务处理的请求是否允许完成，则取决于第 4.2 节定义的线性化语义。

### 3.2 Refresh 与 logout 的真实语义

登录和刷新都会通过 `claims_for_user` 把 `authz_version` 写入 Access/Refresh 自定义声明，但根仓库 `RefreshClaimsResolver` 当前只收到已验证 Token 的 `sub`。应用刷新实现根据 `sub` 读取最新用户和授权，再生成新声明，没有比较旧 Refresh Token 中的 `authz_version`。

所以：

| 操作 | 旧 Access | 旧 Refresh |
|---|---|---|
| 仅递增 `authz_version` | 授权校验后失效 | 仍可刷新；新 Token 使用最新权限 |
| 当前 logout | 通过 subject 撤销水位失效 | 通过 subject 撤销水位失效 |
| 当前 Refresh 轮换 | 不适用 | 旧 JTI 被原子消费，不能并发重复使用 |

“旧 Refresh 仍可刷新为最新权限”一般不会恢复已经撤销的权限，但它意味着认证会话仍然存在。因此改密不能只递增 `authz_version`。

当前 logout 已调用 `TokenManager::revoke_by_subject`，本质上已经是 logout-all。若产品将其展示为“退出当前设备”，文案和行为是不一致的。没有正向会话台账时，系统也无法可靠列出或单独撤销某台设备。

### 3.3 租户与管理员边界

当前租户链路的优点：

- `OrgTenantResolver` 不信任客户端直接声明的管理员身份；
- 它实时查询成员与企业状态，企业 disabled 时新请求会 fail-closed；
- 组织成员增删改还有 `OrgAdminGuardMiddleware` 实时查询操作者是否为 active admin。

当前缺口：

- guard 用 `module == "org.user"` 和 `add | put | del` 名称集合决定覆盖范围；
- guard 查询发生在 handler 之前，而 repository 在之后才启动事务；
- guard 通过后到写事务提交前，操作者的管理员身份可能被另一事务撤销；
- `admin` Addon 的平台管理员写操作目前只有 TokenAuthMiddleware，没有同等级实时管理员 guard。

因此“所有危险写操作都不信 Token 快照”并不成立。组织成员三条既有写路径做了实时预检，但平台管理写路径没有统一做到，而且预检还不是事务内最终授权判断。

### 3.4 持久层隔离

`password_hash` 和 `authz_version` 通过字段角色限制并由私有 repository 操作，这提供了强应用边界和逻辑隔离。它不是数据库物理隔离：应用使用的数据库账号仍可能拥有底层表权限，raw SQL 或错误仓储代码仍能绕开 schema 投影。

正确目标是“三层约束”：

1. Rust 类型与模块可见性限制写入口；
2. 数据库约束拒绝非法事实；
3. 架构门禁禁止授权事实表被任意模块直接写入。

## 四、必须先修的设计缺口

### 4.1 P0：会话与改密语义闭环

#### 问题

仅递增 `authz_version` 不能让旧 Refresh Token 失效。直接实现“改密码后递增授权版本”会得到半闭环：旧 Access 很快失败，但旧 Refresh 仍能刷新。

#### 最终方案

增加独立、持久、单调的 `users.credential_version BIGINT NOT NULL DEFAULT 0`：

- `authz_version`：授权事实版本；
- `credential_version`：认证凭证/全量会话版本；
- Access Token 继续用 `authz_version` 做请求级授权新鲜度；
- Access 与 Refresh Token 都携带 `credential_version`；
- Refresh 前必须比较旧 Token 的 `credential_version` 与 MySQL 当前值；
- 改密、可信密码重置、账号安全全量登出在事务内递增 `credential_version`；
- 改密和账号停用还要递增 `authz_version` 并写授权 outbox，使既有 Access 尽快失效。

根仓库需要扩展刷新契约。推荐将：

```text
RefreshClaimsResolver::resolve_pair(ctx, sub)
```

扩展为能够读取已验证旧 Token 声明的接口，例如：

```text
resolve_pair(ctx, old_claims)
```

或者增加独立的 `validate_refresh_claims(ctx, old_claims)` hook。验证必须发生在签发新 Token 之前；旧 JTI 的原子消费仍由 TokenManager 保证。不要在应用层自行复制 JWT/JTI 轮换逻辑。

#### Token 协议滚动发布

当前 claims 使用严格反序列化，新增字段是协议变更，不能直接混部。采用三阶段发布：

1. 数据库先增加 `credential_version DEFAULT 0`；
2. 全部实例先部署兼容读取版本：新字段在应用 claims 中可缺省为 0，但暂不签发新字段；
3. 确认没有旧实例后，再开启签发和 Refresh 强制校验。

旧 Token 缺字段按 0 解释；某用户首次改密后数据库变为 1，其旧 Refresh 自然失效。等最长 Refresh TTL 过去后，才可考虑把字段改为必填。若无法保证无旧实例，应采用维护窗口切换并显式让所有用户重新登录。

#### 改密码 Action

新增受保护的 `account.user.change_password`，顺序固定为：

1. 从认证上下文取用户 ID，不接受客户端提交目标用户 ID；
2. 按用户和可信客户端维度限流；
3. 读取当前密码摘要并在事务外完成旧密码校验和新 Argon2 摘要计算；
4. 开启事务并 `SELECT ... FOR UPDATE` 锁用户；
5. 比较数据库中的密码摘要是否仍等于第 3 步读取值；不相等则放弃并要求重试；
6. 更新摘要，递增 `credential_version` 与 `authz_version`，写 authorization outbox 和成功审计；
7. 提交后清除浏览器 access 状态与 refresh Cookie，返回 `relogin_required=true`。

昂贵的 Argon2 运算不能持有数据库行锁；事务内摘要复核用于防止两个并发改密请求覆盖彼此。

#### 管理员重置

不得把明文临时密码返回给管理员。使用短 TTL、单次消费的重置凭证：

- 数据库存储重置 Token 摘要、目标用户、过期时间、消费时间和发起者；
- 原始 Token 只在创建响应中出现一次，经受控渠道交付；
- 用户提交新密码时原子消费 Token；
- 完成后递增两个版本并记录审计；
- 创建、尝试和消费都限流；日志和审计只记录 Token 指纹。

#### 验收

- 改密后旧 Access 在授权版本传播后失败；
- 改密后旧 Refresh 即使尚未过期也不能刷新；
- 两个并发改密请求至多一个基于旧摘要成功；
- Redis 短暂不可用不会让凭证版本校验错误地放行；
- 新旧实例混部测试证明协议开关前不会互相拒绝 Token；
- 审计内容不包含密码、Token、摘要或完整 Cookie。

### 4.2 P0：资源授权覆盖与线性化点

#### 问题

权限字符串回答“允许哪类能力”，不能单独回答“能否修改这个具体组织/用户”。当前组织 guard 依赖 Action 名称，且在业务事务前校验，存在覆盖遗漏和 TOCTOU。

#### 可立即落地的方案

不先修改根仓库 Registry API。对每个高危 Action 使用确定的 `ActionRef` 注册专用 middleware，利用现有 `Middleware::target_action`：

- 在同一个模块构造函数中同时创建 Action 与对应 guard；
- 禁止先注册通用写 Action、再维护另一份字符串名单；
- guard 持有目标 `ActionRef`，不再用运行时 `matches!("add" | "put" | "del")`；
- 构建期已会验证目标 Action 存在且属于当前模块；
- 测试枚举模块的所有 mutation Action，断言每个 Action 都有对应资源 authorizer。

仅把 guard 改成读取 `permissions("org.user:write")` 不是最终方案。权限元数据虽然存在，但当前下游中间件没有稳定公开的冻结 Registry 查询契约；更重要的是，同一个粗粒度权限可能对应不同资源解析规则。

#### 最终框架契约

根仓库后续可在 `ActionSpec` 增加显式的资源授权声明，例如 `resource_authorizer = "org_admin"`，由 AppBuilder 校验“声明、middleware、目标 Action”三者一致。不要把资源授权器隐式推导为某个权限字符串。

#### 授权线性化语义

必须在 ADR 中选择并测试以下语义：

> 高危资源写入以业务事务内的最后一次授权事实读取为线性化点。线性化点之后才被撤权的在途请求可以完成；线性化点之前已被撤权的请求必须失败。

实现方式：

- middleware 的事务外查询只作为快速拒绝和上下文解析；
- repository 在同一写事务内再次读取并锁定操作者的当前管理员事实；
- 然后才锁定目标成员/用户并执行写入；
- 所有相关 repository 使用统一锁顺序，避免操作者锁、目标锁和用户授权锁形成死锁环。

平台 `set_admin`、`set_status` 等高危写路径也必须采用同样的事务内管理员事实校验。bootstrap 已额外要求独立的 32+ 字符 operator secret，因此“只窃取 Access Token 即可 bootstrap”并不属实；Step-up 对 bootstrap 是纵深防御，不替代该 secret。

#### 验收

- 新增 mutation Action 但不注册 authorizer 时，构建测试或架构门禁失败；
- 在 guard 预检后、事务校验前撤销操作者管理员身份，写入必须失败；
- 在线性化点之后并发撤权时，行为与 ADR 一致且有测试；
- 平台和组织两类高危写路径均不只依赖 Token 权限快照；
- 压测或并发测试未出现新增死锁；若出现，错误可重试且有指标。

## 五、下一优先级改造

### 5.1 P1：生产级 Step-up 接入

Step-up 不是“已有框架，低成本挂载”。完整接入至少包括：

1. 独立于 Access Token 的 step-up 密钥、issuer、audience 和轮换策略；
2. 可复用的 `CredentialVerifier`，使用账号密码策略与限流；
3. `StepUpCompleteAction` 和明确的 HTTP 428 challenge/proof 契约；
4. 生产多实例必须使用 `RedisStepUpProofStore`；`StepUpMiddleware::new` 默认内存 store 只能用于单进程测试/开发；
5. proof 必须绑定用户、目标 `ActionRef`、资源/关键参数指纹和短过期时间；
6. proof 单次消费，跨用户、跨 Action、跨资源和重放均拒绝；
7. 前端只在收到 428 时弹出重认证，不保存 proof；
8. 内部 Registry 调用按框架设计可绕过 Step-up，因此只能由受信代码发起，并需单独审计。

首批保护目标：

- 平台管理员授予/撤销；
- 用户或管理员状态变更；
- 组织管理员授予/撤销和成员删除；
- 用户自停用；
- 全量会话撤销。

用户改密码已经要求旧密码，通常不再叠加第二次 Step-up。bootstrap 已有独立 operator secret，可在上述路径稳定后再接入。

验收必须覆盖多实例并发消费，而不只是单进程单元测试。

### 5.2 P1：`UserStatus` 类型与数据库约束

当前 `users.status VARCHAR(16)` 能接受任意字符串；代码把未知值按 disabled 处理虽然 fail-closed，但会产生难诊断的锁死。

最终方案：

- 在账号领域定义单一 `UserStatus` 类型，当前只允许 `active`、`disabled`；
- repository、claims 构造、授权校验和 schema 投影只使用该类型；
- 数据库保留 VARCHAR，但增加 `CHECK (status IN ('active','disabled'))`，避免状态值继续散落；
- 新迁移使用下一个空闲版本（当前至少为 0008），不得修改已发布迁移；
- 每个迁移版本只含一个语句，并提供 `information_schema` completion probe。

部署前必须执行：

```sql
SELECT VERSION();
SHOW VARIABLES LIKE 'version_comment';
SELECT status, COUNT(*) FROM users GROUP BY status;
```

门禁：

- MySQL 必须支持并强制执行 CHECK；
- 存量值不收敛时迁移直接停止，先走独立数据修复；
- 在与生产行数和索引规模相当的 staging 表演练 DDL，记录 metadata lock 与最长阻塞；
- 超过变更窗口预算时，改用“新增受约束列 → 回填 → 双写 → 切读 → 后续删除旧列”的多版本 expand/contract，不在一个版本内强行完成。

项目采用 forward-only 迁移。这里的“回滚”是停止发布、恢复兼容代码并通过后续迁移修复，不是执行未定义的 down migration。

### 5.3 P1：授权事实写入边界

对 SQL 文本做“写了表就必须在同一闭包出现递增函数”的正则扫描不可行：

- 用户注册也会 INSERT `users`，但不是授权变更；
- name/phone 等展示字段更新不应递增版本；
- 动态 TableQuery 或通用仓储可能绕过字符串模式；
- 扫描无法证明两个操作处于同一事务。

可执行方案：

1. 建立授权事实清单，至少包括：
   - `admin_user.status/admin/user_user`
   - `org_user.status/admin/org_org/user_user`
   - `users.status`
   - 密码/凭证变更另走 `credential_version`
2. 把这些字段的 DML 收敛到少量私有 typed writer；
3. writer API 接收事务句柄，并在内部完成事实写入、版本递增和 outbox；
4. 禁止其他模块直接获得这些 writer 的底层更新能力；
5. 架构脚本检查“只有 allowlist 文件能写授权事实表/字段”，而不是猜测同一函数中的调用关系；
6. 集成测试逐项枚举授权字段变化，证明版本恰好递增一次；展示字段和幂等更新不得递增。

现有 `every_authorization_field_change_is_detected` 与
`display_only_and_idempotent_updates_do_not_change_authorization` 可作为测试基线。

## 六、需要补齐但不阻塞前述闭环的事项

### 6.1 P2：用户自停用的跨域不变量

直接更新 `users.status='disabled'` 会绕过现有 admin repository 的“最后一个 active superadmin”保护，也可能停用某组织的最后一名 active admin。

自停用必须由账号、平台管理和组织成员领域共同定义：

- Step-up 后进入协调服务；
- 锁定用户及其平台管理员/组织管理员关系；
- 若用户是最后 active 平台管理员，拒绝；
- 若用户是任一组织最后 active admin，要求先转移管理员或按产品规则关闭组织；
- 更新用户状态，递增两个版本，写 outbox 与审计；
- 提交后清除客户端认证状态。

用户名是否允许修改是产品决策，不是安全不变量。审计稳定锚点是不可变用户 ID，不应以“username 是审计锚点”为理由永久禁止改名。

### 6.2 P2：持久化拒绝/失败审计

当前持久化审计主要覆盖成功事件；认证 hook 的失败默认进入 tracing。高风险 Action 的拒绝和失败也应进入可查询的持久事实或耐久 SIEM。

- 成功审计与业务写同事务；
- 被拒绝或事务失败的事件不能写入随后回滚的同一事务，应写独立审计 outbox 或耐久日志管道；
- 审计失败不得把未授权写操作变成成功；
- 记录稳定错误码、actor、target、tenant、request_id 和必要指纹，不记录密码、完整 Token、Cookie 或个人敏感正文；
- 明确审计存储不可用时每类高危操作的 fail-open/fail-closed 策略。

### 6.3 P2：外键策略

`admin_user.user_user`、`org_user.user_user` 和 `org_user.org_org` 当前没有数据库外键。正常 Action 的应用校验不能阻止运维 SQL、缺陷仓储或未来迁移制造孤儿授权事实。

推荐在完成孤儿数据预检和生产规模演练后增加 `RESTRICT` 外键；若因在线 DDL、跨服务所有权或历史导入明确不加，则必须用 ADR 记录原因、替代完整性检查和告警。`audit_event` 作为历史事实不应绑定业务外键级联删除。

### 6.4 P2：租户查询与请求能力

当前 TenantResolver 先查成员再查组织，组织管理员 guard 还会再次查询。可在有性能证据后改成单次 JOIN/EXISTS，返回可信的 request-scoped membership capability。

该 capability 适合减少重复查询，但不能替代第 4.2 节的事务内最终授权检查。是否优化应以 p95、连接池等待和数据库 QPS 为依据。

### 6.5 P2：企业停用语义

当前 `OrgTenantResolver` 实时拒绝 disabled 企业，因此不存在必须立刻“给全体成员批量递增用户版本”才能保证安全的缺口。

未来增加企业停用 Action 时：

- 明确 `active → disabling → disabled` 状态机；
- `disabling` 起即禁止签发新的租户 capability；
- 当前请求按授权线性化 ADR 处理；
- 大组织不要在单事务锁住所有成员并逐个递增版本；
- 若确有 Token 展示收敛、异步任务或非请求消费者需求，再采用租户级版本或可恢复的分批 fanout，并在完成后进入 disabled。

“分批递增用户版本”不是原子停用方案，不能一边异步追赶一边宣称全部成员瞬时失效。

### 6.6 P2：email/phone 的业务语义

`org_user.email/phone` 是组织成员档案字段，不天然是账号找回渠道。无全局唯一约束可能符合“同一人在不同组织拥有不同联系信息”的模型。

需要产品明确：

- 仅作展示：校验格式、字段权限、脱敏、导出、保留期限与删除策略；
- 用作通知：增加投递状态与退信/失效处理；
- 用作可信身份或找回：增加验证流程和 `verified_at`，不能直接信任现有值；
- 无消费者：通过 forward-only expand/contract 删除，不能把“可 down migration”作为验收条件。

### 6.7 P3：注册、会话台账与命名

注册策略由产品边界决定：

- B2B/内部系统优先邀请制和一次性邀请码；
- 必须开放注册时再引入验证码，并把第三方可用性、隐私和失败策略纳入设计；
- 全局注册限流要有容量保护，避免攻击者耗尽全局配额造成自我 DoS；
- “用户名已存在”通常是注册体验与防枚举之间的显式权衡，应记录而非假装完全隐藏。

当前 logout 已是全量撤销。只有产品确实需要“查看设备、退出单设备”时才增加
`refresh_session` 台账，至少记录用户、JTI 摘要、签发/过期/撤销时间和客户端摘要，并设计清理任务。不要存储完整 Token。

`org_org`、`user_user` 和 `admin` 命名可读性一般，但重命名收益不足以单独触发高风险迁移。新字段使用 `*_id`、`is_*`；旧字段只在既定 schema 窗口按 expand/contract 处理。

`bootstrap_key NULL UNIQUE + 'initial-admin'` 的并发守卫目前有效。短期应增加清晰注释和可选 CHECK，不需要为了“看起来更正规”立即改成另一张表。

## 七、Access TTL 与可用性

示例配置当前 Access TTL 为 3600 秒、Refresh TTL 为 2592000 秒。把 Access TTL 从 60 分钟降为 10 分钟，会把稳定登录用户的刷新、MySQL claims 查询和 Redis 轮换负载大约放大 6 倍，不是“零功能成本”。

调整前必须压测和观测：

- refresh 请求 QPS、p95/p99；
- Redis JTI 消费与撤销键容量；
- MySQL claims 查询与连接池等待；
- 客户端并发刷新去重；
- Refresh 失败率和重新登录率；
- 密钥轮换与时钟偏差。

推荐流程：

1. 先完成 Step-up 和会话语义闭环；
2. 在压测环境验证 15 分钟，再按风险决定是否降到 10 或 5 分钟；
3. 分批发布并设置回退阈值；
4. 监控 `authz_version` Redis 回源率和 unavailable 指标；
5. 明确 MySQL 与 Redis 都无法提供授权事实时返回 503，这是安全降级而非 401 认证失败。

## 八、实施顺序与仓库边界

根仓库 `D:\code\lib_yang` 与嵌套仓库
`D:\code\lib_yang\project\yang-system` 是独立 Git/Cargo 边界，必须分别提交、验证和回滚。

### 阶段 A：会话闭环

根仓库：

- 扩展 Refresh hook，使应用能基于旧 Token 完整 claims 做业务验证；
- 保留 TokenManager 对签名、类型、撤销和 JTI 原子轮换的唯一所有权；
- 增加 legacy/new claims 兼容和并发轮换测试。

嵌套仓库：

- 新增 `credential_version` forward-only 迁移与 completion probe；
- 实现兼容签发开关；
- 实现 change_password 和单次重置凭证；
- 明确当前 logout 是 logout-all，修正文案/API 文档；
- 增加 Access/Refresh 失效、改密竞争和审计测试。

完成定义：旧 Refresh 无法跨越凭证版本；滚动发布不会产生节点间 Token 不兼容。

### 阶段 B：资源授权闭环

- 用 `target_action` 重构组织 guard 注册；
- 在组织与平台高危写事务内增加最终管理员事实校验；
- 固化锁顺序和并发语义；
- 增加“新增写 Action 未绑定 authorizer”失败测试；
- 是否扩展根仓库 ActionSpec，在完成本地方案后单独设计，不与本阶段强耦合。

完成定义：覆盖范围由构建期引用和可执行门禁保证；撤权竞争行为可重复、可解释。

### 阶段 C：数据库与写边界

- 引入 `UserStatus`；
- 对生产 MySQL 做版本、脏数据、表规模和锁预算预检；
- 新增状态 CHECK 迁移；
- 收敛 typed authorization writer；
- 增加 allowlist 架构检查和真实 MySQL 集成测试。

完成定义：非法状态进不了数据库；授权事实无法从未批准边界直接写入。

### 阶段 D：Step-up 与纵深防御

- 完成 Redis proof store、凭据复核、428 契约、前端交互、限流和审计；
- 按风险逐个保护平台和组织高危 Action；
- 验证多实例 proof 单次消费；
- 再评估 Access TTL。

完成定义：被盗 Access Token 单独不足以执行已列出的最高危操作。

### 阶段 E：产品能力

- 自停用与最后管理员不变量；
- 持久化 denied/failed 审计；
- 外键决策；
- 企业停用状态机；
- 邀请/开放注册策略；
- 按真实需求决定是否建设会话台账。

## 九、验证矩阵

| 变更 | 必需验证 |
|---|---|
| Refresh hook | legacy/new claims、过期、错误类型、已撤销、并发双刷新、版本落后/领先 |
| 改密 | 错旧密码、弱密码、并发改密、Redis 故障、事务回滚、旧 Access/Refresh |
| 重置 | 过期、重放、并发消费、错误用户、限流、日志脱敏 |
| 资源 guard | 漏绑构建失败、跨租户、管理员撤销竞争、平台最后管理员、死锁重试 |
| 状态迁移 | fresh install、existing upgrade、脏数据拒绝、completion probe、生产规模 DDL 演练 |
| typed writer | 每个授权字段变化、幂等更新、展示字段更新、事务回滚、outbox 恰好一次 |
| Step-up | 跨用户/Action/资源、过期、重放、多实例、Redis 故障、内部调用审计 |
| 自停用 | 最后平台管理员、最后组织管理员、多组织、提交后所有 Token、审计 |

本地门禁：

```powershell
# 根仓库
Set-Location D:\code\lib_yang
cargo fmt --check
cargo test --lib -p yang-base
python scripts/run_ci.py quick

# 嵌套仓库
Set-Location D:\code\lib_yang\project\yang-system
cargo fmt --check
cargo test --lib
python scripts/check_architecture.py
```

涉及 MySQL/Redis 的 ignored 集成测试必须在专用环境单线程执行；根仓库与嵌套仓库的远程 CI 都通过后，才能称为完成。修改 Cargo.lock、feature 或根仓库公共契约时，还要按各自仓库要求执行 full 门禁。

## 十、禁止采用的“伪闭环”

- 只递增 `authz_version` 就宣称旧 Refresh 已失效；
- 再增加一个 `logout_all`，却不承认当前 logout 已按 subject 全量撤销；
- 用 permission 字符串自动推导资源授权规则；
- 保留事务外 guard，却宣称已经消除撤权竞争；
- 生产使用内存 Step-up proof store；
- 管理员重置直接返回明文临时密码；
- 对所有相关 SQL 做正则扫描并把它当作事务正确性证明；
- 大组织停用时在单事务锁住全部成员，或异步分批却宣称瞬时原子失效；
- 修改旧迁移或把“可执行 down migration”写成当前项目验收条件；
- 未压测就把 Access TTL 从 60 分钟降到 5–15 分钟；
- 把应用字段权限描述为数据库物理隔离；
- 把依赖不可用统一描述为 401。

## 十一、最终判断

原分析对以下方向判断正确：MySQL 事实源、Redis 单调缓存、事务 outbox、租户实时解析、Refresh JTI 原子轮换、字段最小化和 fail-closed。

原分析不正确或不完整的部分已经在本版修正：

- 授权版本不等于 Refresh/会话撤销；
- 当前 logout 已是 logout-all；
- Redis 故障会回源 MySQL，双重不可用返回 503；
- 授权残余窗口不等于 outbox 延迟加 Access TTL；
- guard 声明化需要明确的根仓库契约或 `target_action` 落地，不能假设 Registry 已可直接读取；
- Step-up 是跨后端、Redis、配置和前端的完整功能，不是低成本挂载；
- schema 迁移是 forward-only，并受生产版本、脏数据和锁预算约束；
- 企业停用无需为了当前安全性同步锁住全部成员；
- 静态正则扫描不能证明授权版本与事实写入处于同一事务；
- 自停用必须同时保护平台与组织的最后管理员不变量。

按第八节顺序实施后，系统才能真实达到以下状态：

> 身份、凭证、会话、授权与资源边界各有独立事实源；高危写入在事务内完成最终授权判断；改密与全量撤销能阻断旧 Refresh；生产 Step-up 可跨实例防重放；数据库和架构门禁共同约束授权事实；所有结论均有并发、迁移、故障与集成测试支撑。
