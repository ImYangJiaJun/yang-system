# 基础设施修复分支全量测试

日期：2026-10-25。分支：两仓均为 `codex/infrastructure-remediation-20261025`。

本记录验证 `source-after-msrv.json` 所记录的未提交修复快照，补充[历史全量测试记录](2026-10-25-full-test-results.md)。测试绿色不等于[全量修复计划](../superpowers/plans/2026-10-25-infrastructure-remediation.md)已经完成；未实现的审核项仍保持开放。

该检查点之后已继续实施身份会话、XLSX 与 worker 生命周期修复；这些后续改动不在下列绿色结果内，须重新验证。

最新暂停状态见[交接文档](2026-10-25-infrastructure-remediation-handoff.md)：配置回归停在红测阶段（74 通过、5 失败），生产修正尚未实施；后续审批持久化批次亦未完成场景验证。本文下列结果仅属于旧稳定快照。

## 版本与环境

- 应用 HEAD：`5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`；框架 HEAD：`e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`，均叠加工作树改动。
- Windows / PowerShell；Rust 1.97.1、Node 24.21.0、pnpm 10.33.1、Python 3.14.7。
- 日志目录：`target/infrastructure-remediation/verification/`，其中 `source-before.json` 保存本轮开始时 674 个应用文件和 398 个框架文件的 SHA-256；文档不计入源码一致性比较。
- 应用集成测试使用专用 `identity_remediation_test` MySQL 库及独立 Redis DB 15，容器仅发布到 loopback；与日常本地服务、飞书分工测试容器和生产隔离。
- MSRV 使用只读挂载源码的 `rust:1.80.1-slim` 临时容器，明确设置 `RUSTUP_TOOLCHAIN=1.80.1`、`CARGO_HOME=/tmp/ch`、`CARGO_TARGET_DIR=/tmp/ct`；实际打印 rustc/cargo 1.80.1，退出后已清理该容器。

## 执行结果

应用 `full`、`integration`、视觉补测及最终源码的冷缓存 MSRV 复验均通过；本轮纳入的自动化门禁无剩余失败。

| 检查 | 结果 | 日志 |
|---|---|---|
| runner 自检 | 退出 0；ignored 清单双向核对通过 | [自检](../../target/infrastructure-remediation/verification/runner-self-test-final.log) |
| OpenAPI 与 TypeScript 契约重生成 | 退出 0；两处 multipart 总上限变为 17 MiB，类型无差异 | [生成日志](../../target/infrastructure-remediation/verification/contracts.log) |
| `python scripts/run_ci.py full` | 退出 0，约 6 分 37 秒 | [完整日志](../../target/infrastructure-remediation/verification/full-final.log)、[退出状态](../../target/infrastructure-remediation/verification/full-final-status.json) |
| 应用 Rust 全目标测试 | 1282 passed；161 ignored 在本阶段未执行 | 同上 |
| 架构、部署语法、格式与严格 Clippy | 全部通过 | 同上 |
| 前端审计与完整检查 | 审计无已知漏洞；Prettier、ESLint、TypeScript、产品文案、构建、产物与部署契约通过；首屏 JS gzip 274.7 kB，9 个部署契约变异被拒绝 | 同上 |
| Vitest | 80 个文件、938 项通过 | 同上 |
| Chromium 开发 E2E | 21 passed、2 skipped；两项视觉另行补跑 | 同上 |
| Chromium 生产构建 E2E | 4 passed | 同上 |
| `python scripts/run_ci.py integration` | 退出 0；30 组命令、159 项通过，无零匹配组；含 16 个集成入口、14 个库测试 | [日志](../../target/infrastructure-remediation/verification/integration.log)、[退出状态](../../target/infrastructure-remediation/verification/integration-status.json) |
| 本地视觉补测 | 2 passed，退出 0；使用 `--update-snapshots=none`，本轮未重录截图 | [日志](../../target/infrastructure-remediation/verification/visual.log) |
| `cargo test --doc --locked` | 退出 0；0 passed、1 ignored，不能计为示例执行通过 | [日志](../../target/infrastructure-remediation/verification/doctest.log) |
| 框架 `yang-base` 全目标全特性测试 | 退出 0；874 passed、52 ignored，benchmark smoke 成功 | [日志](../../target/infrastructure-remediation/verification/framework-all-features-tests-j1.log) |
| 框架全目标全特性 Clippy / 变更文件格式 | 均退出 0；测试期无用导入修正后重新检查 | [Clippy](../../target/infrastructure-remediation/verification/framework-clippy-final-2.log)、[格式](../../target/infrastructure-remediation/verification/framework-rustfmt-scoped.log) |
| Rust 1.80.1 最终快照冷缓存检查 | `cargo check --all-targets --locked` 单任务运行 4 分 08 秒，退出 0；实际 rustc/cargo 均为 1.80.1；临时容器清理退出 0 | [最终日志](../../target/infrastructure-remediation/verification/msrv-final.log)、[首次日志](../../target/infrastructure-remediation/verification/msrv.log) |
| 部署安全测试 / 变异 / 语法 | 25 项行为、10 项关键变异及脚本语法通过 | [行为](../../target/infrastructure-remediation/verification/deploy-safety-tests.log)、[变异](../../target/infrastructure-remediation/verification/deploy-safety-mutations.log)、[语法](../../target/infrastructure-remediation/verification/deploy-syntax.log) |
| 固定摘要 Nginx | `nginx -t` 及运行时验证通过；覆盖 gzip、16 MiB 文件、17 MiB 整体、Content-Length/chunked、日志脱敏与上游诊断 | [语法](../../target/infrastructure-remediation/verification/nginx-fixed-image.log)、[运行时](../../target/infrastructure-remediation/verification/nginx-runtime-final.log) |
| 固定摘要 Prometheus | 19 条规则检查、告警演练通过 | [规则](../../target/infrastructure-remediation/verification/prometheus-rules.log)、[演练](../../target/infrastructure-remediation/verification/prometheus-alert-drill.log) |
| TLS 模板 / 备份加密原语 | 临时自签证书下 Nginx 语法通过；一次性 CMS 加解密字节相同 | [日志](../../target/infrastructure-remediation/verification/tls-template-and-cms.log) |

应用 Rust 去重后实际执行 **1441 项**（1282 + 159）；前端单测 **938 项**、浏览器 **27 项**（21 + 4 + 2）。框架 **874 项**另计，不把重跑次数叠加。应用全目标阶段的 161 个 ignored 中，159 个已在集成阶段执行，剩余两项现场测试见下文。

成功的 full 开始前保存 `source-final-before.json`，其后与 `source-after.json` 逐文件核对一致：[源码一致性日志](../../target/infrastructure-remediation/verification/source-consistency.log)。两仓 `git diff --check` 均退出 0。所有测试结果对应未提交工作树，不是仅对应上述 HEAD。

## 中断、修正与复跑

1. 首轮应用 `full` 与框架全目标测试并行编译时耗尽 Windows 提交内存：应用 `mmap` 报 `os error 1455`，框架链接报 `LNK1102`。应用尚未进入测试阶段；保留 [full.log](../../target/infrastructure-remediation/verification/full.log) 和 [framework-all-features-tests.log](../../target/infrastructure-remediation/verification/framework-all-features-tests.log)，改为串行 Cargo，进程设置 `CARGO_BUILD_JOBS=1`。未清空构建缓存或修改系统分页文件。
2. 框架严格 Clippy 拒绝 `TableSyncPlan` 的测试期死导入及其 `#[cfg(test)]` 转导出。只移除这两处引用，同一严格命令复跑退出 0，没有加 lint 豁免。测试断言和生产调用路径均未改变。
3. 应用第二轮 1282 项测试通过，但 Clippy 报 `feishu_event_worker.rs` 新测试的 `MutexGuard` 跨 `await`。将显式 `drop(writes)` 改为词法作用域后，重新运行整个 `full` 并通过；该 ignored 测试也在后续 integration 中实际通过。修前证据：[full-j1.log](../../target/infrastructure-remediation/verification/full-j1.log)。
4. 首次冻结后、成功 full 之前只有上述三处测试代码发生变化；成功 full 及后续验证期间源码哈希保持一致。MSRV 重新校验最终快照。

日志仍有非阻断提示：Vite `advancedChunks` 废弃提示、Node 颜色环境变量提示、Windows linker 的提示信息，以及 Cargo 1.80 不识别现有 resolver 配置键。没有把这些提示计为测试失败或消除它们来改变测试标准。

## 门禁修正与证据边界

`scripts/run_ci.py` 增加部署安全回归和 7 项原先遗漏/本轮新增的 ignored 库测试；通过源码双向清单发现漏登记，并明确排除两项现场材料测试。自检摘除一个登记项或部署安全门禁均失败。Windows 输出统一 UTF-8；强制 GBK 父进程的捕获输出回归通过，摘除 UTF-8 修正后复现 `UnicodeEncodeError`。依赖审计仅为该命令使用官方 npm registry，不修改全局设置。

两项现场库测试未执行：真实飞书外发需要单独授权；只读回执测试需要的 `feedback-live-post-receipt.json`、`feedback-live-post-compact-receipt.json`、`feedback-live-post-links-receipt.json` 均不存在。未通过外发或伪造回执补齐。

框架全目标运行中的 52 个 ignored 未在本轮补跑，也没有运行整个 `lib_yang` workspace 的 full/integration；本轮框架执行范围是受改动影响的 `yang-base` 全目标全特性门禁。

生产环境、真实飞书、可信公网 TLS、实际灾备恢复、Firefox/WebKit 和完整容量测试不在本轮本地验证范围。浏览器门禁使用演示后端，真实 MySQL/Redis 测试不替代生产验收。未提交、推送或部署；修复任务使用的隔离 MySQL/Redis 容器保留供后续工作，MSRV 临时容器独立清理。
