# 入站传输修复与验证

日期：2026-10-25。分支：`codex/infrastructure-remediation-20261025`。范围为 `yang-base` Axum 入站传输、应用 HTTP 配置与 xlsx 上传总量合同；不表示生产部署已完成。

## 修复

| 条目 | 最终行为 |
| --- | --- |
| D1-2 / D8-8 | `AxumTransportConfig.max_body_bytes` 保持总量上限，新增可选 `max_json_body_bytes`，未设置时沿用既有总量值，避免框架调用方被隐式降限。应用配置为 HTTP/multipart 总量 17 MiB、JSON 16 MiB；两个 xlsx Action 保持单文件 16 MiB，总体增加至 17 MiB。JSON 在 `to_bytes` 实际读体时计数，不能只修改 `DefaultBodyLimit`。 |
| D1-4 | 同一 `ServiceBuilder` 内组装 `HandleError → LoadShed → GlobalConcurrencyLimit`。所有路径、方法和 Router 克隆共享 semaphore，满载立即 503。超时仍为 408，结束后释放 permit。没有为公开 readiness 绕过此全局容量。 |
| D1-5 | 传输层 408/503、默认 404/405 各自在产生处构造 `ApiResponse`，404/405 保留状态与 `Allow`。最外层统一生成/沿用 request ID，413 解码失败、超时、过载、fallback 均有响应 ID。未按状态码扫描改写业务响应。 |
| D1-7 | 替换会记录原始 URI 的默认 TraceLayer，用仅含 request ID、固定方法名、匹配路由模板、status、耗时的 INFO span/访问日志。未知路由固定为 `__unmatched`，未知方法归为 `OTHER`。应用默认过滤串加入 `yang_base::transport=info`。 |
| 入站指标 | 新增 `yang_http_inbound_requests_total` 与 `yang_http_inbound_request_duration_seconds`；标签为 `method/route/status`，覆盖正常响应、body 拒绝、404/405、408、503。与既有 Action 指标分离，避免把未进入 Action 的失败遗漏或重复归入业务执行。 |

涉及框架文件：`Cargo.toml`（Tower 已有依赖启用 load-shed）、`crates/yang-base/src/transport/axum.rs`、`tests/transport_axum.rs`、新增 `tests/transport_inbound_observability.rs`。全特性 Clippy 另移除 `database/__tests__/schema_sync_test.rs` 中未使用的 `TableSyncPlan` 导入及 `database/schema_sync/mod.rs` 中仅供测试、同样未使用的重导出。未修改 yang-runtime、认证或数据库/schema 的生产行为。

涉及应用文件：`src/config/mod.rs`、`src/config/source.rs`、`config.show.toml`、`docs/contracts/CONFIGURATION.md`、两个 xlsx Action 的 `.max_total_bytes`；`src/bootstrap.rs` 的配置消费由总协调同步。

## 验证证据

- 修复前真实 Router 回归确认：404/405 缺少 request ID；同路由满载会排队，未快速卸载。另一次定向红测证明仅设置 JSON `DefaultBodyLimit` 无效：16 MiB+1 的 JSON 仍返回 201。
- 修复后 `cargo test -p yang-base --no-default-features --features transport-axum,metrics --test transport_axum --test transport_inbound_observability --locked`：53 + 1 项通过。
- JSON 恰好 16 MiB 通过、超出 1 字节返回严格 413；multipart 单文件恰好 16 MiB 通过、超出 1 字节拒绝，总 body 恰好 17 MiB 通过、超出 1 字节拒绝。multipart 覆盖有 Content-Length 与无长度、未知 size_hint、多帧 body；单文件和总量分别断言。人工字节经真实解码器流式落临时文件，未用 mock 代替传输限制。
- 阻塞读体占用唯一 permit，随后同路径、不同方法、不同路径、健康及 fallback 均立即 503；第一请求超时 408 后健康请求恢复 200。业务自行返回的 404/405/408/413/503 的 code、message、Allow 保持原样。
- 独立测试进程同时捕获 tracing 与 metrics，逐个证明 200/404/405/408/413/503 的访问日志、计数与耗时各按实际请求记录，并验证 URL/token 哨兵不出现在日志或指标标签中。独立进程避免与其他测试的 subscriber/callsite 缓存相互干扰。
- 8 项可编译变异均实际测试失败后恢复：移除 JSON 读体限制、快速卸载、全局共享、408 信封、外层 request ID、计数、耗时、安全访问日志。直接只删除 LoadShed 引起类型错误的尝试不计通过；后续同时调整失去错误来源的适配器，验证实际回归断言变红。日志位于根仓库忽略的 `target/inbound-remediation/mutation-logs/`。
- 冻结后固定摘要 Nginx 实测再次通过：gzip、Vary、已压缩资源、16 MiB 文件、17 MiB 总体边界（Content-Length/chunked）、502 上游诊断和日志脱敏。最终留存日志：`target/infrastructure-remediation/verification/nginx-runtime-final.log`（退出 0）。
- 冻结后 `cargo test -p yang-base --all-targets --all-features --locked -j 1`：退出 0，23 套汇总 874 passed、0 failed、52 ignored，benchmark smoke 也通过。首轮并发构建触发 Windows `LNK1102` 内存不足及 rustc 异常退出；改为框架/应用 Cargo 串行且 `-j 1` 后完成，没有删除缓存或据失败产物声称测试通过。日志：`target/infrastructure-remediation/verification/framework-all-features-tests-j1.log`。
- `cargo clippy -p yang-base --all-targets --all-features --locked -j 1 -- -D warnings`：退出 0。首次两轮暴露上述测试代码的无效导入/重导出，经总协调授权移除后通过；最终日志：`target/infrastructure-remediation/verification/framework-clippy-final-2.log`。MSVC 生成过程宏导入库的既有 `linker_messages` 警告仍存在，该 lint 不受 `-D warnings` 影响。应用 full/integration/OpenAPI 由主线程统一运行；上述 52 项 ignored 未在此框架测试命令中实际执行。
- 框架 5 个变更 Rust 文件以 `rustfmt --check --edition 2021 --config skip_children=true` 单独检查通过，退出 0；日志：`target/infrastructure-remediation/verification/framework-rustfmt-scoped.log`。此检查不递归检查未改动的子模块。
- 架构检查与本范围 `git diff --check` 通过。新配置边界/环境变量回归已入库，但新增配置验证 guard 未做单独源码摘除；全量验证期间保持源码冻结。

## 兼容性与边界

显式旧配置 `http.max_body_bytes = 16777216` 需要改为 `17825792` 或删除该覆盖；新的 17 MiB multipart 合同会拒绝在更小总量上限下启动。若使用更小自定义总量，JSON 上限也必须显式不大于它。升级脚本保留运维显式数值，本次未读取或改写真实含密钥配置。框架新增 public struct 字段会影响没有使用 `..Default::default()` 的完整 struct literal。

这些结果证明本地 Router、配置源与边缘合同。未验证真实生产 TLS、业务 xlsx 内容可被完整导入或飞书平台交互。并发 permit 和总超时覆盖读入请求、Handler 至响应头产生阶段；流式附件的完整网络传输不在本次预算扩展范围。超时取消 Future 不等于回滚已发生的外部副作用。日志记录到响应头产生时的处理耗时，不应解释为客户端收齐响应的耗时。
