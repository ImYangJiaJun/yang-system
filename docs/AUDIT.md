# 高权限业务审计契约

`audit_event` 是高权限业务变化的数据库事实源，不是 tracing 的副本。tracing 可以
采样、丢失或按短周期清理；审计事件必须与成功的业务变化在同一 MySQL 事务提交。
事件模型、Schema、保留边界和事务接入已经固定，在线代码没有独立连接或独立事务的
审计写入口。

## 事件模型

每条事件固定包含：

- 128 bit 随机 `event_id` 和当前 `schema_version`；
- 非匿名 `actor_type + actor_id`；
- 可空 `tenant_id`，平台级事件保持空值，租户事件必须填写；
- 稳定 `action`；
- 可空且成对出现的 `subject_type + subject_id`；
- 必填 `target_type + target_id`；
- 白名单 `before_summary` / `after_summary`；
- 32 位十六进制 `request_id`、`occurred_at` 与 `result`。

摘要只允许有界标量或标量数组，禁止嵌套对象；字段名命中 password、secret、token、
nonce、credential、authorization、cookie 或 hash 时在进入数据库前失败。密码、
Token、Cookie 和完整请求体不得通过改名绕过该边界。事件
Debug 只展示摘要字段名，不展示值。

数据库 CHECK 约束固定 event/request ID 形状、tenant 正数语义以及 subject 字段成对
出现。索引分别覆盖 actor、subject、target、tenant、request_id，并用
`(occurred_at,id)` 提供稳定保留游标。表不建立业务外键，避免用户或组织删除级联
破坏历史事实；资源标识按事件发生时的值永久保存。

## 必须覆盖的高权限变化

以下成功变化必须接入审计，且审计插入失败时业务事务必须整体回滚：

- 首个超级管理员 bootstrap；
- 平台管理员新增、启停、授予或撤销超级管理员；
- 用户启停；
- 企业成员新增、移除、启停以及组织管理员授予/撤销；
- 任何显式 system tenant capability 执行的业务写。

当前 writer 从 Registry 注入的可信 `module + action` 生成事件 action，不接受请求体
提供 action；平台账号、企业成员和租户 onboarding 都只能调用 `append_in_tx` 并传入
自己持有的业务事务。幂等请求没有产生数据库变化时不生成成功事件。企业成员展示字段
发生实际变化时只记录字段名，不记录姓名、邮箱、电话等值。

拒绝和失败事件允许独立写入，但不能伪装成已提交的业务变化，也不能依赖已经回滚的
事务。向 SIEM 投递属于派生副本；数据库事件始终是本系统事实源。

## 数据库权限

生产环境使用三个相互独立的数据库主体：

| 主体 | `audit_event` 权限 | 用途 |
|---|---|---|
| application owner | DDL | 只在启动期声明式 Schema 同步使用 |
| application runtime | `SELECT, INSERT` | 在线服务只读、追加 |
| retention/export job | `SELECT, DELETE` | 归档校验和到期批量清理 |

运行主体不得拥有 `UPDATE` 或 `DELETE`，保留主体不得被应用进程使用。实际账号名由
部署系统决定；权限门禁应通过 `SHOW GRANTS` 核对，而不是把账号或密码写入仓库。
架构门禁同时拒绝在线 Rust 源码出现针对 `audit_event` 的 UPDATE、DELETE 或
TRUNCATE；这不能替代数据库授权，但能阻止普通代码评审遗漏重新引入修改路径。

## 保留与归档

默认在线保留期为 365 天，部署可以延长但不能缩短；处于 legal hold 的事件禁止清理。
到期事件必须先导出到不可变/WORM 存储，并为每个导出批次记录起止
`(occurred_at,id)`、事件数和内容校验和。只有导出校验通过后，独立 retention job
才能按同一游标小批量 DELETE；禁止无条件全表删除、TRUNCATE 或业务级联删除。

应用不内置定时删除器，避免在线进程同时拥有写事实和销毁事实的权限。归档介质的保留
时长由部署所在地法规和组织制度决定，但不得短于在线 365 天基线。
