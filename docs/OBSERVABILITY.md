# 可观测性契约

## 结构化日志

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

## Prometheus 指标

`observability.metrics_enabled=true` 时，进程在独立的
`observability.metrics_bind` 地址提供 `GET /metrics`。该端点不复用业务 HTTP
监听器，不承载认证或业务路由；生产部署必须通过 loopback、Sidecar 或网络策略把
访问范围限制为采集端。启动时若监听地址冲突，服务失败关闭，不静默降级。

核心指标包括：

- `yang_system_action_requests_total{operation,result}` 与
  `yang_system_action_duration_seconds{operation,result}`：覆盖认证、租户解析和
  Handler 的完整 Action 链；
- `yang_system_auth_rate_limit_total{operation,result}`：认证限流的允许、拒绝和
  Redis 不可用结果；
- `yang_system_resource_pool_connections{resource,state}`：MySQL/Redis 连接池的
  `max/open/available/waiting` 快照；
- 既有授权传播、关闭阶段和 YANG Action 指标；
- `yang_system_build_info{service,version,environment}`：部署身份。

标签只能来自部署身份、已冻结 Catalog operation 和有限枚举。禁止把
`request_id`、用户/租户 ID、用户名、IP、SQL、Redis key、URL 或错误文本放入指标
标签。所有 histogram 使用固定秒级 buckets，避免实例间聚合语义漂移。

## OpenTelemetry tracing

`observability.traces_enabled=true` 时，服务通过 OTLP/gRPC 向
`observability.traces_otlp_endpoint` 导出 trace，并采用 parent-based
trace-id-ratio 采样。`traceparent` 仅作为 W3C Trace Context 解析，不写入日志；
系统创建的 `action.request` span 继承远端 parent，Handler、MySQL/PostgreSQL 与
Redis span 位于同一条下游链路。

数据库 span 只记录受控的 `db.system`、`db.operation`、`db.collection` 与
`otel.kind=client`。禁止记录 SQL 文本、绑定参数、Redis command/key/value 或 Lua
脚本。关闭 tracer provider 和指标监听器复用进程唯一关闭预算，确保批量 exporter
有界刷新，不另行累加超时。
