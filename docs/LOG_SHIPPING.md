# 日志聚合与采集约定

本文是 [`OBSERVABILITY.md`](OBSERVABILITY.md)「结构化日志」章节的运维侧配套：
约定容器/进程 stdout 的 JSON 日志如何被采集器（Fluent Bit / Vector）接入、
字段如何保留与脱敏、保留期与轮转策略。应用侧字段契约以 OBSERVABILITY.md 为
唯一事实源，本文不重新定义字段语义。

## stdout JSON 契约（对采集器的要求）

- 服务标准输出固定为**一行一个 JSON object**；`logging.filter` 只控制目标与
  级别，不改变编码格式。采集器必须按 JSON 解析（Fluent Bit 的 `json` parser /
  Vector 的 `decoding.codec = "json"`），不要用正则提取，也不要依赖中文
  `message` 文本做路由。
- 每次 Action 派发恰有一条 `Action 执行完成` 规范事件；顶层固定字段
  `service` / `version` / `environment` / `operation` / `request_id` /
  `result` / `error_code` / `error` / `duration_ms`，以及 span 字段
  `module` / `action` / `actor_id`。这些字段必须**原样保留**，不允许采集器
  重命名（下游告警与检索直接引用它们）。
- 采集器应补充的自有元数据（k8s namespace/pod、容器 ID、节点名）放在独立的
  顶层命名空间（如 `kubernetes.*`、`host.*`），不得与应用字段平铺混写。

## 接入方式

容器化部署下应用只写 stdout/stderr，不落日志文件：

- Docker：默认 `json-file` driver 即可，Fluent Bit/Vector 以容器日志路径
  或 Docker socket 方式采集；Kubernetes：节点级 DaemonSet 采集
  `/var/log/containers/*.log`。
- 裸机/systemd：journald → Vector `journald` source，同样按 JSON 解析
  `MESSAGE` 字段。

最小 Vector 配置示例：

```yaml
sources:
  yang_system:
    type: docker_logs          # 或 kubernetes_logs / journald
    include_labels:
      - "app=yang-system"
transforms:
  parse:
    type: remap
    inputs: [yang_system]
    source: |
      parsed, err = parse_json(.message)
      if err == null { . = merge!(., parsed) }
sinks:
  warehouse:
    type: elasticsearch        # 或 loki / clickhouse / s3
    inputs: [parse]
    # ...endpoint 与索引策略按环境填写
```

## 字段保留与脱敏

- 应用侧已保证**不记录**请求体、Authorization/Cookie header、密码、Token、
  数据库连接串、Redis URL（见 OBSERVABILITY.md）。采集器与管道**不得**通过
  sidecar 抓包、access log 镜像等方式把这些内容补回来——Nginx 边缘的 access
  log 如开启，必须关闭对 `Cookie`/`Authorization` 的记录（Nginx 默认
  `combined` 格式不含这两个头，保持默认即可）。
- `actor_id`、`operation`、`request_id` 是排障与审计关联的必要字段，予以保留；
  如落地平台有合规要求，可对 `actor_id` 做带盐哈希，但必须在管道内统一完成，
  且与 `audit_event` 的关联能力要在切换前评估（高权限追责的事实源是
  MySQL `audit_event` 表，不是日志）。
- `error` 字段是应用内错误摘要，不含堆栈与 SQL；不要在采集端再对其做正则
  脱敏重写，避免破坏可检索性。

## Trace 关联

- 日志的关联键是 `request_id`（每个请求一条 Action 规范事件）。
- 启用 OpenTelemetry 时（`observability.traces_enabled=true`），trace 经
  OTLP/gRPC 独立导出到 Collector；`traceparent` 只作为 W3C Trace Context 被
  解析继承，**不写入日志**。需要日志 ↔ trace 互查时，在 Collector 侧按
  `request_id` 与时间窗关联，或在上游网关注入并透传 `x-request-id`。
- 指标标签禁止携带 `request_id`/用户 ID 等高基数值（见 OBSERVABILITY.md），
  日志是这些高基数关联键的唯一归宿。

## 保留期与轮转建议

| 层级 | 建议 | 说明 |
|---|---|---|
| 容器本地（json-file driver） | `max-size=50m`、`max-file=3` | 只作缓冲，不当作存储；防磁盘打满 |
| 热存储（可检索） | 14–30 天 | 覆盖绝大多数排障窗口 |
| 冷归档（对象存储） | 90 天–12 个月 | 合规需要时启用；审计追责以 `audit_event` 表为准，不依赖日志保留期 |

- 索引/流按 `service=yang-system` + `environment` 划分，避免多环境混查；
  `operation`、`result`、`error_code` 建为关键字字段用于告警聚合。
- 日志丢失不得影响业务：采集端故障时允许丢弃缓冲，禁止为保日志阻塞应用
  stdout（容器运行时已满足该语义，自建管道不得破坏）。

## 不要做的事

- 不要把日志当审计：`audit_event`（事务内 append-only）才是追责事实源。
- 不要在应用内增加第二套文件日志或动态日志注册表；配置只在启动期合成一次。
- 不要让采集器把原始请求/响应体、连接串写入日志管道。
