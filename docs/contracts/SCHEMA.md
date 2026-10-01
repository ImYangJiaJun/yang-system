# 数据库结构演进

`yang-system` 不维护版本化迁移、历史 SQL、迁移命令或 `_migrations` 表。应用每次启动
都会以当前源码中的 TableDefinition 为唯一目标结构：

1. 构建全部业务表与运行支撑表定义；
2. 读取现有 MySQL 元数据并生成增量计划；
3. 在任何 DDL 前统一预检旧数据；
4. 只有全部预检通过才执行建表、加列、显式改名、兼容类型调整、索引、CHECK 和外键；
5. 再次规划必须为空，随后应用才进入 ready。

同步器不会删除数据库中未知的表、列、索引或约束，也不会清空旧数据。列改名必须在字段
上显式声明 `renamed_from`，不按相似名称猜测。

## 旧数据冲突

以下冲突会让启动整体失败，并保证该轮尚未执行任何 DDL：

- 非空列中存在 NULL；
- 字符串超过新长度或枚举存在非法值；
- 新唯一索引存在重复值；
- CHECK 表达式被旧行违反；
- 新外键存在孤儿引用。

错误会给出表名、字段或约束名，以及最多 20 个按主键排序的冲突主键。运维人员手动修复
这些行后重启即可；无需伪造版本号或跳过某个历史脚本。

## 权限相关表清单

以下 5 张表构成权限系统的存储层，声明在 `src/addon/access/` 下，启动时经声明式 Schema
增量同步：

| 表 | 核心字段 | 唯一键 | 外键 |
|---|---|---|---|
| `authz_grant` | `id`, `user_id`, `permission`, `granted_by`, `occurred_at` | `uk_authz_grant_user_permission` (`user_id`, `permission`) | — |
| `permission_group` | `id`, `group_key`, `title`, `description`, `created_by`, `occurred_at` | `uk_permission_group_key` (`group_key`) | — |
| `permission_group_item` | `id`, `group_id`, `permission`, `granted_by`, `occurred_at` | `uk_permission_group_item` (`group_id`, `permission`) | `fk_permission_group_item_group` → `permission_group.id` (RESTRICT) |
| `user_group` | `id`, `user_id`, `group_id`, `granted_by`, `occurred_at` | `uk_user_group` (`user_id`, `group_id`) | `fk_user_group_user` → `users.id` (RESTRICT)； `fk_user_group_group` → `permission_group.id` (RESTRICT) |
| `system_owner` | `id`, `sentinel_key`, `user_id`, `claimed_at` | `uk_system_owner_sentinel` (`sentinel_key`) | — |

`authz_grant` 由 `src/addon/access/grants/table.rs` 声明；其余四张由
`src/addon/access/domain/groups/tables.rs` 的 `infrastructure_definitions()` 输出。
全部走声明式 Schema，零 SQL 迁移文件。权限系统契约详见 `docs/contracts/AUTHZ_GRANTS.md`。

## 边界

当前同步是保数据、fail-closed 的兼容演进工具，不支持自动删列、缩短字符串、任意数值
类型变换、拆表/合表或数据语义重写。需要这些变化时，应先把目标拆成框架可证明安全的
扩展步骤；若无法证明，应用会拒绝启动并要求人工处理，而不是自动丢弃数据。
