# yang-system 基础系统架构与生产成熟度评估

> 评估对象：`D:\code\lib_yang\project\yang-system` 后端
> 初评源码快照：`450372f20ab54ceb9dec230b87b5ecf5780bd54b`
> 复评源码快照：`84c14fad57b972e0e6a25cab603ae1f1e83555fe`
> 评估日期：2026-07-26
> 基础库关系：消费 `yang-base`、`yang-db` 的本地 path dependency
> 说明：分数用于风险排序和阶段决策，不代表经过生产流量认证。

## 一、结论先行

`yang-system` 已经具备基础系统应有的主要纵向切面：账号、平台管理、组织/租户、认证授权、受保护数据访问、Schema 同步、配置、健康检查、真实数据库集成测试和配套前端 Catalog。模块边界清楚，关键并发写使用事务与行锁，整体可读性明显高于“控制器 + service + 任意 SQL”式后台。

综合复评为 **4.1/5（L4 入口，受控生产候选）**：

- 初评识别的授权新鲜度、租户旁路证明和 bootstrap 信任根三个 P0 已全部形成代码、真实依赖测试和架构门禁闭环；
- 生产 Schema 已改为默认 validate、非开发/测试环境禁止 apply，并具备独立版本化迁移作业；
- Tools 构建后的成功与失败路径已统一关闭，授权 outbox worker 也与服务生命周期绑定；
- 对受控内网或边界清楚的部署，可以进入生产候选验证；经反向代理的互联网部署、无中断 JWT 密钥轮换、强审计和完整 SLO 仍有明确缺口。

这次上调不是因为“测试变多”，而是原来跨模块的不变量已经从约定变成了可执行边界。它仍不是高保障生产系统：代理信任、key ring、不可变业务审计、可观测闭环和灾备演练需要按部署等级继续完成。

生产成熟度的下一步重点不是增加更多业务模块，而是补上 **代理信任、密钥轮换、审计、可观测性和发布/恢复演练**。

### 复评增量

| 初评问题 | 复评状态 | 关键落地证据 | 仍需守住的边界 |
|---|---|---|---|
| S-01 授权新鲜度 | **已完成** | 事务版本 writer、outbox、单调 Redis 发布、共享请求校验器、真实 MySQL/Redis 故障矩阵 | 指标告警与传播延迟 SLO |
| S-02 租户全路径证明 | **已完成（当前范围）** | 路径清单、scoped/system capability、架构门禁、CRUD/事务/批量/关联双租户负例 | 新后台任务和新 raw SQL 必须继续纳入清单 |
| S-03 bootstrap 信任根 | **已完成** | 运维 secret 的 Argon2id 摘要、并发受限校验、HTTP 强制验证、并发/重放集成矩阵 | 生产 secret 交付与轮换 runbook |
| S-05 Schema 发布边界 | **已完成（基础版本）** | 默认 validate、生产 apply 拒绝、版本/checksum/恢复说明/完成探针迁移作业 | expand/backfill/contract 的真实演练 |
| S-06 生命周期关闭 | **统一出口已完成** | Tools 后统一 cleanup，保留主错误；outbox worker 显式 shutdown | 增加进程级有界 shutdown 预算 |

原 P0 清零后，架构重心已从“补安全事实”转为“证明长期运营”：S-04、S-07、S-08、S-09 和灾备不应被已有绿色测试掩盖。

## 二、第一性原理：基础系统必须守住什么

1. **身份是事实，Token 只是事实的有期限快照。** 用户被停用、管理员被降级、组织成员被移除后，旧快照必须在可定义的时间内失效。
2. **租户隔离必须默认生效。** 不能依赖每条 SQL 都记得追加 `org_id`。
3. **高风险状态迁移必须原子化。** “最后一个超级管理员”“组织与创始成员”“唯一初始化管理员”不能在并发下失守。
4. **启动失败也必须正确释放资源。** 从连接建立到应用退出的任何 `?` 都不应绕过统一清理。
5. **Schema 变化是发布行为，不是普通请求行为。** 生产环境必须可预演、可审计、可停止，不能把启动期增量同步当成完整迁移系统。
6. **所有外部身份信息都要有信任边界。** 代理转发 IP、租户 header、请求 id、用户 claims 都不能无条件信任。
7. **高权限变化必须留下可追溯、抗业务侧篡改的审计。** 普通日志不是审计账本；若要求密码学不可抵赖，还需哈希链、签名或独立 WORM 存储。
8. **可运维性是架构的一部分。** readiness、结构化日志、指标、trace、密钥轮换和部署回滚都必须有明确契约。
9. **任何租户旁路都必须显式授权。** 原生 SQL、事务、关联加载和后台任务必须携带非可选 tenant capability；系统级访问必须单独授予并可审计。
10. **安全事实与失效传播要原子衔接。** 不能在数据库提交后“尽力写 Redis”，而要用同事务版本更新或可靠 outbox。

## 三、成熟度评分

| 维度 | 评分 | 判断 |
|---|---:|---|
| 模块化与装配 | 4.3 | account/admin/org Addon 清楚，组合根集中 |
| 事务与数据一致性 | 4.4 | 高风险写、授权版本与 outbox 同事务，唯一约束和行锁边界清楚 |
| 认证与授权 | 4.3 | 登录/刷新与请求期授权版本闭环，缓存损坏和依赖故障策略明确 |
| 租户隔离 | 4.3 | scoped capability、路径清单、架构门禁和真实双租户矩阵已闭环 |
| 配置与密钥 | 3.3 | 严格校验和 bootstrap 摘要成熟，仍缺环境覆盖与 JWT key ring |
| 可观测与运维 | 3.4 | health/tracing/request id/outbox metrics 已有，审计、SLO 和恢复演练不足 |
| 测试与质量门禁 | 4.5 | quick/full/integration 分层，原 P0 与缓存/数据库故障矩阵已进入门禁 |
| 可读性与逻辑简洁性 | 4.2 | 共享授权校验器与 scoped capability 收敛职责，模块主线仍清楚 |
| **整体** | **4.1** | **L4 入口的受控生产候选，尚非互联网/强审计场景的完整基线** |

## 四、当前架构的成熟点

### 4.1 组合根清楚，业务按能力纵向切分

`src/app.rs` 集中装配 account、admin、org Addon 和复合权限解析器；业务代码位于 `src/modules/<feature>/`，每个自定义 Action 独立文件，`mod.rs` 只做组装。仓库的架构检查又把这一约定变成可执行门禁。

这一结构有三个好处：

- 变更影响范围容易定位；
- 模块内部可保持领域语言；
- 业务扩展直接沿基础库唯一 Addon 主线进入，不需要额外适配层。

### 4.2 认证实现关注了真实攻击面

账号模块已经具备：

- Argon2 密码哈希放入 `spawn_blocking`；
- 通过 Semaphore 限制昂贵哈希并发；
- Redis Lua 原子实现 IP 与用户名双维度限流；
- 密码字段脱敏；
- 注册唯一键竞态处理；
- refresh 时重新读取用户状态并重算角色/权限；
- 登录/刷新审计 hook 和 request id。

这不是演示级登录接口，而是有资源消耗、竞态和错误泄露意识的实现。

### 4.3 关键状态变化有事务与数据库约束

组织创建会在同一事务中写入组织和创始成员；失败时依赖事务回滚。平台管理员服务对“最后一个有效超级管理员”使用事务和行锁，避免并发降级或停用把系统锁死。首次管理员初始化使用数据库唯一约束处理并发竞争。

这些防线位于数据库一致性边界，而不是单纯依赖“先查再写”的应用逻辑。

### 4.4 租户上下文与受保护查询已经贯通

组织中间件按认证、租户解析、组织管理员授权的顺序执行。租户解析会验证成员关系和组织状态，再把可信 tenant context 注入请求。基础库 `TableQuery` 随后自动施加 tenant key。

这条链实现了：

`外部租户选择 → 成员资格验证 → 可信上下文 → 数据查询自动加域`

相比在 repository 中手写 `WHERE org_id = ?`，它更接近不可绕过的安全默认值。

### 4.5 Schema 同步采取了保守策略

`src/schema.rs` 的同步逻辑使用数据库 advisory lock 串行化多实例竞争，只自动创建缺失表、字段、主键和索引；对不兼容结构进行诊断，而不是静默删除或修改已有数据结构。

这使它适合作为本地开发、首次部署或“验证 + 保守补全”工具。成熟点在于克制，而不是自动化 DDL 的数量。

### 4.6 集成测试覆盖了一条有业务价值的真实旅程

`tests/system_integration.rs` 不是只测 health：

- Schema plan/apply/idempotence；
- 注册、登录、一次性管理员初始化；
- refresh 后获得平台管理员权限；
- 组织创建；
- 旧 Token 无法直接获得新组织权限；
- refresh 后获得 org_admin；
- 添加成员；
- tenant discovery 和租户域数据访问。

集成环境还要求数据库名以 `_test` 结尾、Redis DB 15，降低误伤开发/生产数据的风险。

复评后，这条旅程已扩展为多个独立证据面：bootstrap 信任根、租户旁路矩阵、版本化迁移作业、Schema 并发恢复、授权缓存单调性、outbox 租约/重放，以及授权新鲜度故障矩阵。测试不再只证明 happy path，而是开始证明错误依赖组合下的失败方式。

## 五、关键风险与改进建议

### S-01：授权事实版本与 Access Token 新鲜度（已完成）

**原优先级：P0；复评状态：实现、门禁与真实依赖矩阵闭环。**

当前实现以 MySQL `user_user.authz_version` 为事实源。登录与 refresh 在事务快照内签发版本；用户状态、平台管理员状态/角色、组织成员/角色变化在同一事务内只在授权事实真正变化时递增版本，并写入授权 outbox。

请求期由共享 `RequestAuthorizationValidator` 覆盖 account user、admin user、org tenant、org org 和 org user 模块：

- Redis 是单调发布的快速路径，版本落后、缺失、损坏或错误类型时回退 MySQL；
- Redis 整体不可用但 MySQL 可用时仍以数据库事实判定并尝试回填；
- MySQL 不可用时，只有格式正确且足以完成判定的缓存值可继续服务；需要数据库才能判定时返回稳定的 `AUTHORIZATION_FRESHNESS_UNAVAILABLE`，不会使用未知旧权限；
- Token 版本落后或缓存版本领先时返回稳定的 stale 错误；Token 版本超前、缺失或 subject 非法同样失败关闭；
- outbox 使用租约、重试和单调 `MAX` 发布，多副本不会把缓存版本回退。

`64aa12a` 还修复了真实 MySQL prepared statement 复用时数据库时间边界冻结的问题，避免 worker 冷启动空轮询后永远看不到后续事件。

**已完成路径：**

1. `authz_version` 持久化、事务签发与 writer 覆盖；
2. 事务 outbox、租约 worker、单调缓存发布；
3. 基础库 application claims validator 与稳定错误分类；
4. 系统共享 freshness validator 与五个敏感模块装配；
5. 并发、损坏缓存、Redis/MySQL 独立故障和 outbox 连续性的真实依赖测试。

**持续验收条件：**

- 所有新增授权 writer 必须同事务递增版本并入 outbox；
- 所有新增依赖 claims 的敏感模块必须装配共享 validator；
- 继续监测版本不匹配、传播延迟、outbox backlog、重试和 Redis/MySQL 故障；
- 高价值写若要求小于轮询窗口的硬实时撤销，可在版本链之上叠加 `jti` 黑名单，不得建立第二套授权事实源。

### S-02：租户隔离“所有数据路径”证明（已完成，持续治理）

**原优先级：P0；复评状态：当前数据路径闭环。**

当前仓库已建立租户表、Action、CRUD、raw SQL、事务、Join、批量和系统路径清单；业务路径接收非可选 `ScopedTenantCapability`，系统级路径使用独立 `SystemTenantCapability`。架构检查会拒绝租户模块直接取得无范围数据库入口，真实 MySQL 双租户测试覆盖主路径与已枚举旁路。

**已完成路径：**

- `2b6326d` 固化数据路径清单；
- `f130fc8`、`1be3dc9` 覆盖 CRUD 与旁路负例；
- `da331fb` 将证据集合锁入架构门禁；
- `f451b3d` 配合基础库 `86e7219` 将 scoped 与 system capability 分离。

**持续验收条件：**

- 新增 raw SQL、RelationLoader、导入或后台任务时必须先更新路径清单；
- 每条新路径都有 tenant scope 或显式 system capability，并增加双租户负例；
- system capability 的调用者保持最小集合并进入审计。

### S-03：首个超级管理员的运维信任根（已完成）

**原优先级：P0；复评状态：信任根与重放矩阵闭环。**

系统现在要求同时具备已登录身份与运维持有的高熵 bootstrap secret。应用只加载经过强度和资源边界校验的 Argon2id PHC 摘要；运行期校验在受控阻塞线程执行并受并发量限制，Debug 和错误均不泄露 secret 或摘要。

数据库唯一约束与事务继续保证并发最多一次成功；一旦首个管理员存在，HTTP bootstrap 永久关闭，原 secret 重放失败。

**已完成路径：**

- `9a91103` 增加摘要类型、生成工具与严格配置；
- `b45c6e1` 在 Action 边界强制校验运维 secret；
- `6d5bf6d` 用真实 MySQL 覆盖无 secret、错误 secret、正确 secret、并发和成功后重放。

**持续验收条件：**

- 生产部署通过 secret mount/provider 交付摘要，不把原始 secret 或摘要写入日志；
- 初始化事件进入 S-08 审计账本；
- 若威胁模型要求 HTTP 面完全无初始化能力，可在部署层改为一次性运维 Job。

### S-04：代理后的客户端 IP 信任边界不完整

**优先级：P1；影响：限流正确性与可绕过性。**

当前限流从直接 peer `SocketAddr` 取得 IP。在反向代理后，所有用户可能表现为同一个代理地址；如果未来简单改成无条件信任 `X-Forwarded-For`，攻击者又可以伪造来源。

**建议路径：**

- 配置受信代理 CIDR；
- 只有直接 peer 位于受信代理范围时，才解析标准 `Forwarded` 或约定的 `X-Forwarded-For`；
- 从右向左剥离受信代理，得到第一个非受信地址；
- 对头长度、地址数量和非法值设置上限；
- 将解析后的 client IP 作为受信 request extension 注入，认证限流只消费该值。

**验收条件：**

- 直连、单层代理、多层受信代理、伪造头、超长头都有测试；
- 未配置代理时完全忽略转发头；
- metrics 能区分解析失败与限流触发。

### S-05：生产 Schema 发布与启动期同步分离（已完成基础闭环）

**优先级：P1；影响：发布安全、回滚、可审计性。**

当前默认 schema 模式已经改为 `validate`，非 development/test 环境会在启动期拒绝 `apply`。独立 migration job 维护有序版本、checksum、前置条件、恢复说明、执行记录与 DDL 完成状态探针；应用副本只验证最终 Schema，不再承担生产迁移。

**已完成路径：**

- `29e4544` 默认 validate；
- `c8e3d60` 在配置验证期阻止生产 apply；
- `21c07b8` 增加版本化迁移 CLI/job、manifest、checksum、恢复元数据和真实 MySQL 中断重试测试。

**持续验收条件：**

- 每个新 migration 必须声明版本、checksum、前置条件、恢复说明和必要的完成探针；
- expand → backfill → switch → contract 的破坏性步骤保持拆分和审批；
- 发布流水线先执行 migration job，再由应用 `validate` 阻止 Schema 漂移。

### S-06：启动与失败路径统一资源收尾（统一出口已完成）

**优先级：P1；影响：测试、优雅退出、未来资源扩展。**

`run_then_cleanup` 现在包围 Tools 创建后的完整操作阶段：应用构建、Schema、地址解析/绑定、serve 失败和正常退出都会恰好执行一次 `tools.close()`，且保留原始业务错误。授权 outbox worker 在 HTTP 服务退出后显式 shutdown。

**已完成路径：**

- `3a21588` 将 Tools 后流程收敛到统一 cleanup 边界，并覆盖 build/schema/bind/serve/成功路径；
- outbox worker 作为显式句柄随服务停止，不依赖进程退出隐式丢弃。

**剩余与持续验收条件：**

- 新增 worker、exporter、租约或插件资源时必须接入同一生命周期；
- 在进程级 shutdown budget 内为 worker 与资源关闭增加统一超时；关闭必须幂等，且不能覆盖原始运行错误。

### S-07：配置和 Token 密钥缺少生产级治理

**优先级：P1；影响：部署、轮换、泄露响应。**

当前配置使用 `deny_unknown_fields`、资源上限和脱敏 Debug，bootstrap secret 也只保存 Argon2id 摘要；但 `Settings::load` 仍直接读取 TOML 文件，JWT 使用单个 HS256 secret，缺少明确的环境/secret-provider 覆盖、`kid`、验证 key ring 和轮换窗口。

**建议路径：**

- 明确优先级：默认值 < 配置文件 < 环境变量 < secret provider；
- secret 不进入普通配置 dump；
- Token header 携带 `kid`，签发只用 active key，验证接受 active + retiring keys；
- 建立轮换和紧急吊销 runbook；
- 第一阶段不做配置热更新，避免把一致性问题带入运行期。

**验收条件：**

- 无需修改镜像或落盘明文即可注入密钥；
- 轮换期间旧 Token 按策略继续验证，窗口结束后失效；
- 未知 `kid`、弱 secret 和空 secret 在启动期失败。

### S-08：业务审计仍停留在日志层

**优先级：P1；影响：追责、合规、事故恢复。**

认证 hook 会输出 tracing，但 admin/org 的高权限变化没有不可变审计账本。普通日志可能丢失、重采样或被运维系统按保留期删除，也不保证和业务提交一致。

**建议路径：**

- 建立 append-only audit event：actor、subject/target、tenant、action、before/after 摘要、request_id、时间和结果；
- 高风险业务写与 outbox/audit 事件在同一数据库事务提交；
- 异步投递到日志/SIEM，但数据库记录是事实源；
- 对敏感字段做白名单摘要，不记录密码、Token、bootstrap nonce。

**验收条件：**

- 管理员授予/撤销、用户停用、组织角色变化、bootstrap 均有事件；
- 业务提交成功却没有审计记录的状态不可出现；
- 可按 actor、target、tenant、request_id 检索；
- 审计表只追加，修改/删除权限独立控制。

### S-09：可观测性尚未形成运行闭环

**优先级：P1；影响：故障定位和容量管理。**

当前有 tracing、request id、live/ready 和慢查询基础，但系统没有完整启用/导出基础库 metrics，也缺少结构化生产日志、trace exporter 和关键 SLI。

**建议路径：**

- 日志采用 JSON，固定 service、version、environment、request_id、action、result；
- 接入 OpenTelemetry trace 或等价方案；
- 暴露请求量、错误率、延迟、连接池、Redis、限流、授权版本不匹配、Schema 状态；
- readiness 检查设置总预算，避免依赖逐个串行阻塞；
- 定义告警而非只暴露指标。

**验收条件：**

- 单个前端报错可凭 request id 追到 Action、SQL/Redis 边界和结果；
- 有延迟/错误率 SLO 与告警；
- readiness 在依赖退化时有界返回；
- 指标标签无 user id、tenant id 等高基数值。

### S-10：领域 SQL 逃生口应保留，但要集中

**优先级：P2；影响：可读性和安全审查。**

组织成员 Join、列表和管理员行锁使用 raw sqlx 是合理的：通用 `TableQuery` 不应被迫表达所有领域查询与锁语义。问题只在于这些 SQL 是否散落、是否都有租户/权限前置、是否能静态校验。

**建议路径：**

- raw SQL 只允许在模块 repository/service 边界；
- 参数全部绑定，表/列名不得来自请求；
- 每个跨租户查询写明隔离不变量并有负例测试；
- 条件允许时启用 sqlx offline metadata；
- 不建立一套抽象 repository trait 只为隐藏 sqlx。

**验收条件：**

- tenant 模块的 raw SQL 均位于可枚举边界；
- 每条查询只使用绑定参数，动态标识符不能来自请求；
- 跨租户 Join、锁和批量路径具有双租户负例；
- 本项在 S-02 清单中持续受架构检查约束。

### S-11：ActionContext 耦合可以按测试压力渐进拆分

**优先级：P2；影响：单元测试和领域复用。**

部分 service 直接接收 `ActionContext`，方便访问事务、身份和 Tools，但也把 HTTP/派发上下文带入领域逻辑。

**建议路径：**

- 先提取很小的 `UseCaseContext`：actor、tenant、request_id、UnitOfWork；
- 只在需要独立单测或后台任务复用的 service 使用；
- 不为每张表创建一套 repository interface；
- Action 继续负责输入适配和错误投影。

**验收条件：**

- 只有存在独立测试或复用压力的 use case 才迁移；
- 新上下文不暴露无边界数据库入口；
- 重构前后授权、事务和错误投影行为一致；
- 本项是演进建议，不作为近期生产准入门槛。

## 六、测试矩阵状态

原 P0 测试已完成，剩余测试仍按风险而非覆盖率数字排序：

| 状态 | 优先级 | 场景 | 证据/必须证明 |
|---|---|---|---|
| **已完成** | 原 P0 | 用户停用/管理员撤销/成员移除与缓存故障 | 旧 token 失败；并发只增一次；Redis/MySQL 独立故障时按契约降级或失败关闭 |
| **已完成** | 原 P0 | bootstrap 抢占与重放 | 无/错误 secret 失败，并发仅一次成功，成功后永久关闭 |
| **已完成** | 原 P0 | 跨租户全路径负例 | CRUD、Join、事务、批量、关联和系统能力边界均有双租户证据 |
| **已覆盖** | P1 | 最后一个管理员并发变更 | 任意调度下至少保留一个有效超级管理员 |
| **已覆盖** | P1 | 组织创建中途失败 | 组织和成员同事务，不残留半成品 |
| **已完成基础版** | P1 | Schema 多步失败/重跑 | 迁移 checksum、完成探针和重试不误记成功 |
| **待完成** | P1 | 受信代理 IP | 转发头不能伪造，代理用户不会全部共享同一身份 |
| **待完成** | P2 | readiness 依赖超时 | 有界返回且状态可诊断 |
| **待完成** | P2 | audit outbox 重投 | 至少一次投递不产生重复业务事实 |

## 七、分阶段改进路径

### 阶段 0：安全封口（1—2 周）

1. ✅ 已实现 `authz_version` 失效链与故障矩阵。
2. ✅ 已完成租户数据路径清单和双租户负例矩阵。
3. ✅ 已给 bootstrap 增加运维持有的高熵 secret 与摘要校验。
4. ✅ 上述三项已进入真实 MySQL/Redis 集成门禁。
5. ✅ 生产 Schema 默认并强制 `validate`；`apply` 只允许 development/test。

**退出条件：已达到。** 权限撤销、全路径租户隔离和初始化抢占三个 P0 场景均有自动化失败/成功证据。

### 阶段 1：生命周期与边界（2—4 周）

1. ✅ 已统一 bootstrap 失败/关机资源清理。
2. 实现受信代理 client IP。
3. 建立配置覆盖和 JWT key ring。
4. ◐ 租户 raw SQL 已收敛到 scoped/system capability 并持续执行负例；非租户领域 SQL 继续按 S-10 治理。

**退出条件：** 多副本部署的代理、密钥、Schema、关闭策略有可执行 runbook。

### 阶段 2：可审计、可观测（4—8 周）

1. 建立事务内 audit/outbox。
2. 输出结构化日志、metrics、trace。
3. 定义 SLO、告警和 readiness 预算。
4. 为 Schema 建立版本化 deploy migration。

**退出条件：** 高权限变化可追责；一次请求可端到端定位；部署变更可预演和恢复。

### 阶段 3：规模化前再优化（8 周以后）

1. 用压测确定连接池、Argon2 并发、Redis 和数据库瓶颈；
2. 需要时加入 API 幂等键和 outbox 消费；
3. 只有测试压力明确时再提取 `UseCaseContext`；
4. 不提前微服务化。

## 八、建议的生产准入门槛

| 场景 | 当前建议 |
|---|---|
| 本地开发/演示 | 可用 |
| 受控内网试运行 | 可用；bootstrap 信任根已关闭抢占窗口 |
| 受控生产候选 | S-01、S-03、S-05 与 S-06 统一出口已完成；需按部署完成 S-07、S-09、有界 shutdown 与恢复演练 |
| 多租户生产 | S-02 当前路径已闭环；新增路径必须持续扩展证据清单 |
| 经反向代理的互联网生产 | **仍需 S-04**，并完成适用的 S-07、S-09 |
| 强审计/高价值权限系统 | 共同基线 + S-08 + 灾备/恢复演练 |
| 微服务拆分 | 当前没有必要 |

## 九、最终判断

`yang-system` 的主体架构已经从“成熟方向”推进到“受控生产候选”：模块边界、事务、租户能力、初始化信任根、生产 Schema 边界、资源生命周期和授权新鲜度都不需要根本性返工。原 P0 已关闭，接下来限制更高等级的是运营与部署信任边界，而不是业务骨架。

建议保持模块化单体和 YANG 原生单运行时，按 S-04 → S-07 → S-08/S-09 的风险顺序补齐代理、密钥、审计与可观测性，再做滚动发布和恢复演练。不要因为允许框架级重构就提前微服务化；当前最简路径是继续强化现有组合根和不可绕过边界。

## 十、验证记录与结论边界

### 本机新鲜验证

| 命令 | 结果 | 可见证据 |
|---|---|---|
| `python scripts/run_ci.py full`（初评） | 通过 | 架构/脚手架检查、rustfmt、Rust 单元测试、Clippy `-D warnings`、前端完整检查和 production build |
| `python scripts/run_ci.py integration`（初评） | 通过 | 隔离的 `yang_system_test` + Redis DB 15；真实 MySQL/Redis 生命周期旅程 |
| `pnpm --dir frontend e2e`（初评） | 通过 | Chromium 18/18；使用 demo backend 和 Quasar dev server |
| `python scripts/run_ci.py full`（复评） | 通过 | 62 个 Rust 单测通过、3 个真实依赖测试按设计 ignored；4 个 migration contract 通过；Clippy all-targets/all-features；生产依赖审计无已知漏洞；前端 15 文件/71 测试与 Quasar 2.22.0 production build |
| `python scripts/run_ci.py integration`（复评） | 通过 | 9 个真实依赖测试：授权缓存、outbox、迁移、Schema 竞争/恢复、bootstrap、租户隔离与系统旅程全部通过 |
| `cargo test --test system_integration -- --ignored`（复评） | 连续 3 次通过 | 授权新鲜度共享模块、并发 writer、缓存缺失/损坏/落后/领先/错误类型、Redis/MySQL 独立故障、outbox 连续性 |
| `cargo clippy --all-targets -- -D warnings`（复评） | 通过 | 当前授权与集成测试代码无 Clippy warning |

复评使用本机 Compose 管理且仅绑定 loopback 的 MySQL/Redis：测试只连接名称以 `_test` 结尾的 `yang_system_test` 和 Redis DB 15。集成脚本逐项串行运行会修改这些隔离测试依赖，不触碰开发或生产数据。

复评已经补上授权链中的 Redis/MySQL 组合故障注入，但以下内容在没有对应实测前仍不作通过声明：

- 高并发下的吞吐和 p95；
- 多副本滚动发布与故障恢复；
- 受信代理链和 JWT key ring 轮换；
- 不可变业务审计与端到端 SLO 告警；
- 灾备 RPO/RTO。
