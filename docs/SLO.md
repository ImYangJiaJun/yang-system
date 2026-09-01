# 生产 SLO 与告警契约

## 目标与口径

| 能力 | SLI | 目标 | 计算口径 |
|---|---|---|---|
| Action 可用性 | 服务错误率 | 30 天不低于 99.9% | `result=error` 计服务失败；参数、权限等 `business_error` 不消耗可用性错误预算 |
| Action 延迟 | p95 完整 Action 耗时 | 5 分钟窗口低于 500 ms | 按 `yang_system_action_duration_seconds` histogram 聚合 |
| Readiness | 单次依赖检查 | 2000 ms 内返回 | starting/stopping、依赖失败和预算耗尽都返回 503 |
| 授权传播 | outbox 创建到 Redis 发布 p99 | 低于 2 秒 | `authz_propagation_seconds` 与 oldest-age 双重观察 |

目标是默认生产基线，不替代按业务等级定义的产品 SLO。低流量环境应结合请求量判断，
避免单个偶发错误触发不稳定百分比。

## 错误预算

99.9% 可用性对应 30 天约 43.2 分钟不可用预算。规则采用多窗口 burn-rate：

- 5 分钟与 1 小时同时超过 14.4 倍：快速消耗，按 critical 处置；
- 6 小时与 3 天同时超过 1 倍：持续消耗，按 warning 处置；
- p95 连续 15 分钟超过 500 ms：延迟告警；
- readiness 连续 2 分钟没有一次成功：实例不可接流量。

机器可加载规则位于 `ops/prometheus/yang-system.rules.yml`。Prometheus 抓取目标必须
提供稳定的 `job`、`instance` 标签；禁止把 request/user/IP/SQL/Redis key
注入目标标签。

## 探针配置

管理面默认只监听 loopback。Kubernetes 若由 kubelet 直接探测 Pod IP，应显式绑定
受网络策略保护的 Pod 地址，例如 `0.0.0.0:9090`：

```yaml
readinessProbe:
  httpGet:
    path: /health/ready
    port: 9090
  periodSeconds: 5
  timeoutSeconds: 3
  failureThreshold: 3
```

应用预算必须小于探针客户端 timeout。liveness 仍使用业务监听器
`/health/live`，它不访问数据库或 Redis，避免依赖故障触发无意义重启。

## 值班响应

1. readiness 先看 `result`：`lifecycle` 表示发布/关闭窗口，`dependency` 表示已完成
   但依赖不健康，`timeout` 表示依赖检查突破预算。
2. 依赖问题结合连接池 available/open/max、outbox oldest age 和 Redis unavailable
   限流指标定位；不要从指标标签寻找用户。
3. 快速 burn 先止损（摘流量、回滚或扩容），慢速 burn 再定位长尾与特定 operation。
4. 告警恢复后保留时间线、发布版本和 trace 证据；高权限事实仍以 `audit_event` 为准。

## 规则加载与告警演练

CI 使用版本与摘要双重固定的 Prometheus 官方镜像执行：

```text
promtool check rules yang-system.rules.yml
promtool test rules yang-system.rules.test.yml
```

规则语法、标签、annotations 和阈值行为由上述机器校验与演练文件共同证明；真实
Alertmanager 接收器、升级路径与值班送达仍必须在目标环境单独演练。
