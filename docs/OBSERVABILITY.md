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
`observability.metrics_bind` 地址提供 `GET /metrics` 与预算化
`GET /health/ready`。该管理面不复用业务 HTTP 监听器，不承载认证或业务路由；
生产部署必须通过 loopback、Sidecar 或网络策略把访问范围限制为采集端与探针。
启动时若监听地址冲突，服务失败关闭，不静默降级。

核心指标包括：

- `yang_system_action_requests_total{operation,result}` 与
  `yang_system_action_duration_seconds{operation,result}`：覆盖认证、租户解析和
  Handler 的完整 Action 链；
- `yang_system_auth_rate_limit_total{operation,result}`：认证限流的允许、拒绝和
  Redis 不可用结果；
- `yang_system_registration_email_total{result}` 与
  `yang_system_registration_email_verify_total{result}`：注册邮件的投递/抑制/限流/失败
  及验证码消费/拒绝结果；标签只使用冻结的有限枚举，绝不包含邮箱、IP 或验证码；
- `yang_system_resource_pool_connections{resource,state}`：MySQL/Redis 连接池的
  `max/open/available/waiting` 快照；
- `yang_system_readiness_*`：管理面探针结果、耗时和各依赖健康状态；
- 既有授权传播、关闭阶段和 YANG Action 指标；
- `yang_system_build_info{service,version,environment}`：部署身份。

标签只能来自部署身份、已冻结 Catalog operation 和有限枚举。禁止把
`request_id`、用户/租户 ID、用户名、IP、SQL、Redis key、URL 或错误文本放入指标
标签。所有 histogram 使用固定秒级 buckets，避免实例间聚合语义漂移。

## Readiness 总预算

生产编排应把 readiness 指向管理面 `/health/ready`。进程在 Schema/audit 校验与
后台 worker 启动完成前保持 `lifecycle` 未就绪，收到关闭信号后先撤销 readiness，
再进入 HTTP drain。依赖检查共享 `observability.readiness_budget_ms` 一个总预算；
不会给 MySQL、Redis 各自累加一份 timeout。

响应原因只有 `lifecycle`、`dependency`、`timeout` 三个有限值，不暴露连接串或底层
错误文本。默认预算为 2000 ms，允许 50..=10000 ms。编排器的客户端 timeout 应略
大于应用预算，例如应用 2 秒、Kubernetes `timeoutSeconds: 3`。SLO 与告警规则见
[`SLO.md`](SLO.md) 和 `ops/prometheus/yang-system.rules.yml`。

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

## 浏览器错误关联

前端在 Quasar boot 阶段统一接入 Vue error handler、`window.error` 与
`unhandledrejection`，API client 还会捕获已被页面正常处理的 API/合同/网络错误。
只有内存中存在已认证 Access Token 时才向
`POST /api/v1/observability/frontend-errors` 上报；该 Action 本身也要求有效身份，
上报失败不会递归产生第二次上报。

事件只包含有界的 `event_id`、错误种类、Vue route name、稳定 operation id、
16 位错误指纹、HTTP status/error code，以及可选的原后端 `related_request_id`。
协议明确不接受错误 message、stack、URL/query、请求/响应 body、Token 或 Cookie；
错误 message 和非白名单 Error name 也不参与指纹。后端用 `deny_unknown_fields`
拒绝额外字段。相同指纹在单标签页 10 秒内只发送一次，去重表最多保留 256 项，
避免异常循环放大内存、日志和指标。服务端再按已认证用户限制为每实例每分钟 30 次，
并把跟踪表限制为 4096 个活跃用户；超限统一返回 429，浏览器静默丢弃而不递归上报。

后端在当前上报 Action 的 trace span 中写一条 `event_type=frontend.error` 结构化事件：

- `request_id` 是错误上报 Action 自己的请求标识；
- `related_request_id` 是原失败 Action 响应头中的标识，可直接检索原
  `Action 执行完成` 日志及其 trace；
- `frontend_*` 字段只保存经过白名单/长度校验的路由、operation、状态、错误码和指纹；
- `yang_system_frontend_errors_total{kind,linked}` 只使用有限枚举标签，不把 request id、
  route、operation 或指纹放入指标标签。

排障时先用告警时间窗和 `kind/linked` 指标确认范围，再从 `frontend.error` 日志取得
`related_request_id`，检索同值的原 Action 规范事件，最后进入该 Action span 的
下游 MySQL/Redis trace。机器可加载告警与 firing/silent 演练分别位于
`ops/prometheus/yang-system.rules.yml` 和
`ops/prometheus/yang-system.rules.test.yml`。
