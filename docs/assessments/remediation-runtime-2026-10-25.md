# 运行期与只读冒烟修复（2026-10-25）

范围：P10 / R-01 / R-05 与 D7 中关闭所有权、启动绑定次序，以及部署冒烟入口。
仅修改本地源码，未进行生产连接、部署或写入。编译与真实依赖测试使用共享 target、
`-j1`，由主代理串行分配窗口。

## 已确认的根因与实现

- 七类 worker 的 `shutdown(self)` 在 `take(JoinHandle)` 后等待；总预算取消 future
  会丢弃裸句柄并分离任务，owner Drop 已经拿不到它。新增 `OwnedTask` 保留句柄，
  `join(&mut self)` 借用等待，Drop 请求 abort。管理面任务应用同一所有权规则。
- 关闭先撤销 readiness，并同时通知全部 worker 和管理面；HTTP、七 worker 和管理面
  并行 drain，共用一个绝对截止时间。总预算的 10%（最多 1 秒）预留给 Tools/trace；
  trace 同步关闭收到实际剩余时间。启动中途错误也会丢弃并中止已有 owner。
- 业务监听器在 worker 启动和 `mark_ready` 前绑定，端口占用失败不会先公开 ready。
- Grant expiry 已有用户行锁 + expires_at / converged_at CAS，只有消费成功的实例 bump
  授权版本。仅修正旧“多实例必然重复递增”注释，没有重写现有并发协议。
- 会话清理先读取最多 1,000 个过期候选，再按候选 ID 删除；DELETE 使用数据库时钟
  复核保留期，保护读取候选后已被 refresh 更新的会话。满批后等待 100 ms 继续处理，
  批次间检查关闭信号；无积压时仍沿用 6 小时间隔，保留期未改变。
- `--smoke-check` 在构建完整应用后只读 schema/旧数据预检及依赖检查，返回版本化 JSON
  后退出；不创建 telemetry exporter、HTTP/管理面监听器或任一业务 worker。
  部署拒绝普通模式、未知 JSON、失败退出、参数/镜像/配置漂移；前端单独静态冒烟。
  `prepare` 不再建库，首次部署须先显式 `infra-up`。

## 验证记录

1. 真实红测 `cancelled_shutdown_aborts_inflight_cleanup`：旧实现下关闭 future 在 10 ms
   后被取消，任务持有的资源在后续 1 秒内仍未释放，断言失败。
2. 部署命令替身测试 32 项通过、16 项摘除变异被杀死，覆盖只读契约、退出状态、参数、
   镜像/config 身份、禁止 smoke 建库，以及 readiness 与 edge 共用 60 秒总截止时间。
3. 真实 loopback MySQL 随机 `*_test` scratch 库：业务端口被另一个 listener 占用时，
   完整应用 `ReadOnlySmoke` 仍通过，查询确认 schema 保持 0 表；库已清理。
4. 真实 MySQL batch 红测：旧实现一次删除 1,001 行，违反单批 1,000 行断言。
   分批实现已完成。运行期基线绿测：`yang-runtime --lib` 10 项、真实 MySQL batch 1 项、
   smoke scratch 1 项通过。6 项摘除变异（abort 所有权、收尾预算、batch 上限、smoke 分支、
   刷新复核、积压短间隔）均被对应断言杀死；恢复原始字节后 5 项绿测通过
   （runtime 10 项、session 单测 3 项、session ignored 2 项、smoke 1 项、startup mode 1 项），
   三文件 SHA256 与变异前一致。日志见 `target/infrastructure-remediation/verification/`。

## 明确边界

Tokio abort 为协作取消：不证明已终止不 yield 的代码或已运行的 `spawn_blocking`。
OwnedTask 的 panic 通过 join 返回，不提供服务运行期 panic/stall 监督。探针新鲜度、
外部 Prometheus 消费者与通知送达不是本报告的验证结果。Histogram 的有限桶扩展至
300 秒，不替代慢阶段告警验收。

`smoke.json` 仅为该镜像和配置的一次只读预检；不是锁内 schema 执行证明、旧写者兼容
证明或正常业务运行证明。`ROLLBACK_SCHEMA_COMPATIBLE=yes` 仍只是操作人声明。
