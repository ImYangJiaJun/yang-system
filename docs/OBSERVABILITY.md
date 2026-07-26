# 结构化日志契约

服务标准输出固定为一行一个 JSON object，`logging.filter` 只控制目标与级别，不改变
编码格式。日志采集器必须按 JSON 解析，不应依赖中文 message 文本。

每次 Action 派发恰有一条 `Action 执行完成` 规范事件，覆盖公开请求、认证失败、租户
解析失败、业务错误和成功结果。事件顶层字段固定为：

- `service`、`version`、`environment`：低基数部署身份；
- `operation`、`request_id`：可信 Registry 目标与请求关联标识；
- `result`：`success`、`business_error` 或 `error`；
- `error_code`、`error`：成功时分别为 `0` 和空字符串；
- `duration_ms`：完整中间件链与 Handler 的耗时。

JSON 的当前 `dispatch` span 固定携带 `module`、`action`、`request_id`、`actor_id`、
`tenant_scope`、`tenant_id`、`result`、`error_code` 和 `duration_ms`。匿名或 pre-tenant
请求会省略尚不存在的 actor/tenant 值；普通租户记录可信解析后的正整数 ID，系统级
旁路只记录 `tenant_scope=system`，不会把请求 header 当作可信租户。

日志禁止记录请求体、Authorization/Cookie header、密码、Token、bootstrap secret、
数据库连接串或 Redis URL。高权限追责仍以 `audit_event` 为事实源，结构化日志不能
替代事务内审计。
