# 授权存储与权限目录契约

**生成：** 2026-09-03
**范围：** `access` Addon（`src/addon/access/`）提供的权限基础设施：权限目录、
授权存储、Token 授权快照扩展与授权管理接口。

## 权限模型

- 权限是点分隔的小写字符串（如 `access.grants.read`），格式由
  `PERMISSION_PATTERN`（`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`，最长 128 字符）约束，
  数据库层以 `chk_authz_grant_permission_format` CHECK 兜底。
- 权限目录 = 运行期从冻结 Catalog 投影的所有 Module/Action 声明的
  `.permissions(...)` 集合（决策 D3，单一事实来源，无静态清单）。
  组合根在 `AppBuilder::build` 后安装投影（`src/app.rs`），运行期只读。
- 授权存储只存"用户 ↔ 权限字符串"直授关系（决策 D4），不引入角色聚合层；
  Token 中的角色仍为账号域固定的 `user`，权限由 access 的
  `AuthzGrantResolver` 在签发/刷新时从 `authz_grant` 读入 claims。
- 只能授予目录中已声明的权限；未声明的权限在管理接口处被拒绝（fail-closed）。

## `authz_grant` 表

| 列 | 类型 | 语义 |
|---|---|---|
| `id` | 自增主键 | 事实行标识 |
| `user_id` | BIGINT | 被授权用户 |
| `permission` | VARCHAR(128) | 权限字符串 |
| `granted_by` | BIGINT | 授权操作人用户 ID（运维 SQL 初始化时填操作者标识或 `0`） |
| `occurred_at` | BIGINT | 授权发生的 Unix 时间戳（插入时自动写入） |

约束：`uk_authz_grant_user_permission (user_id, permission)` 复合唯一索引；
`chk_authz_grant_permission_format` 权限格式 CHECK。表结构由
`src/addon/access/grants/table.rs` 声明，启动时增量同步，不使用 SQL 迁移文件。

## 写入一致性

授权/撤销必须在同一事务中完成三件事（授权 writer 契约）：

1. 写业务事实（插入或删除 `authz_grant` 行，持有目标用户行锁）；
2. 经账号安全版本原语单调递增目标用户 `users.authz_version`（凭据版本不变，
   Refresh 会话保持有效，用户刷新后透明获得新授权快照）；
3. 追加 `authorization_outbox`（由版本原语内建完成）。

writer 边界登记见 `docs/architecture/authorization-writers.md`：
`access-grant-lifecycle` 负责事实行，版本与 Outbox 复用
`account-security-version`，任何代码不得绕过这两个 writer。

## 初始授权（运维）

系统不提供"最终管理员"，应用内没有任何自提权路径（决策 D2）。第一个
`access.grants.write` / `access.grants.read` 授权由运维直接经 SQL 完成，
之后日常授权/撤销走管理接口。运维 SQL 必须遵守与在线 writer 相同的一致性：

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

## 管理接口

| 接口 | 路由 | 所需权限 | Step-up |
|---|---|---|---|
| 授予权限 | `POST /api/v1/access/grants` | `access.grants.write` | 是 |
| 撤销权限 | `POST /api/v1/access/grants/revoke` | `access.grants.write` | 是 |
| 查询用户授权 | `GET /api/v1/access/users/{user_id}/grants` | `access.grants.read` | 否 |
| 查询权限目录 | `GET /api/v1/access/permissions` | `access.grants.read` | 否 |

授权/撤销为幂等语义：重复授予或撤销不存在的权限返回 `changed: false`，
不递增版本、不追加 Outbox。高权限写操作按 `docs/AUDIT.md` 契约记录
append-only 审计，并挂载 Step-up 重认证中间件。
