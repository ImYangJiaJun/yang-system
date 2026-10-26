# 基础设施修复稳定快照全量检查点（2026-10-26）

分支：两仓均为 `codex/infrastructure-remediation-20261025`。应用 HEAD `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`，框架 HEAD `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`，均叠加未提交工作树改动。

本记录验证 `target/infrastructure-remediation/checkpoint/source-frozen.json` 所记录的快照（应用 681 个文件、框架 398 个文件；文档不计入源码一致性比较）。`source-before.json` 在 OpenAPI 重导出之前采集，两者仅差两个生成物（见下）；测试均在该冻结快照上执行。绿色只证明该快照的已执行范围，不表示全部审核工作包已完成，也不代表生产部署或现场验收。

## 执行结果

| 检查 | 结果 | 日志 |
|---|---|---|
| runner 自检 | 退出 0；ignored 清单双向核对通过 | [self-test](../../target/infrastructure-remediation/checkpoint/runner-self-test.log) |
| OpenAPI 与 TypeScript 契约重导出 | 退出 0；仅两处预期差异（`delete_config` 描述、两处 multipart 总上限 17 MiB） | [openapi](../../target/infrastructure-remediation/checkpoint/openapi.log) |
| `python scripts/run_ci.py full` | 退出 0；`CI profile passed: full` | [full](../../target/infrastructure-remediation/checkpoint/full.log) |
| 应用 Rust 全目标测试 | 1300 passed / 0 failed / 176 ignored（主 lib 套件 1324 个：1300 passed、24 ignored；其余集成包 ignored 交 integration） | 同上 |
| 架构、部署语法、部署安全、格式、严格 Clippy | 全部通过；部署安全 32 项 | 同上 |
| 前端 audit / 完整检查 | 无已知漏洞；Prettier、ESLint、TypeScript、Vitest、locale、build、产物、bundle budget（首屏 274.7 kB gzip）、部署契约通过 | 同上 |
| Vitest | 80 个文件、938 项通过 | 同上 |
| Chromium 开发 E2E | 21 passed / 2 skipped（视觉用例另行补跑） | 同上 |
| Chromium 生产构建 E2E | 4 passed | 同上 |
| `cargo test --doc --locked -j 1` | 退出 0；0 passed / 1 ignored，不能计为示例执行通过 | [doctest](../../target/infrastructure-remediation/checkpoint/doctest.log) |
| `python scripts/run_ci.py integration` | 退出 0；38 组、174 passed / 0 failed / 0 ignored、0 零匹配组；369.1 秒 | [integration](../../target/infrastructure-remediation/checkpoint/integration.log) |
| 框架 `yang-base` 全目标全特性测试 | 退出 0；874 passed / 0 failed / 52 ignored | [base-test](../../target/infrastructure-remediation/checkpoint/framework-base-test.log) |
| 框架 `yang-base` 严格 Clippy | 退出 0；仅依赖 `yang-base-derive` 的 Windows linker 提示，不属代码 lint | [base-clippy](../../target/infrastructure-remediation/checkpoint/framework-base-clippy.log) |
| 框架 `yang-runtime` 全目标全特性测试 / Clippy | 退出 0；12 passed / 0 failed；Clippy 退出 0 | [runtime-test](../../target/infrastructure-remediation/checkpoint/framework-runtime-test.log)、[runtime-clippy](../../target/infrastructure-remediation/checkpoint/framework-runtime-clippy.log) |
| 框架改动文件 scoped rustfmt | 6/6 退出 0 | [rustfmt](../../target/infrastructure-remediation/checkpoint/framework-rustfmt.log) |
| 框架 doctest | 退出 0；yang-base 39 passed / 113 ignored、yang-db 65 passed、yang-runtime 0 | [framework-doctest](../../target/infrastructure-remediation/checkpoint/framework-doctest.log) |
| 本地视觉补测 | 2 passed，退出 0；`--update-snapshots=none`，未重录截图 | [visual](../../target/infrastructure-remediation/checkpoint/visual.log) |
| Rust 1.80.1 冷缓存 MSRV | 退出 0；`cargo check --all-targets --locked` 5m17s；实际 `cargo 1.80.1` / `rustc 1.80.1`；临时容器已清理 | [msrv](../../target/infrastructure-remediation/checkpoint/msrv.log) |
| Prometheus 规则 / 演练 | 19 条规则检查与 `promtool test rules` 均 SUCCESS，退出 0 | [rules](../../target/infrastructure-remediation/checkpoint/prometheus-rules.log)、[drill](../../target/infrastructure-remediation/checkpoint/prometheus-rules-test.log) |
| 部署安全变异 | 16 个关键故障变异全部杀死，退出 0 | [deploy-mutations](../../target/infrastructure-remediation/checkpoint/deploy-mutations.log) |
| Nginx 固定镜像证据 | 当前 `nginx.conf` SHA-256 与旧检查点记录一致，按证据复用范围沿用旧 `nginx -t`/运行时日志，未重跑 | [reuse](../../target/infrastructure-remediation/checkpoint/nginx-evidence-reuse.log) |

## 源码一致性

- `source-before.json` → `source-frozen.json`：应用 681 文件仅 `frontend/contracts/openapi.json` 与 `frontend/src/engine/contracts/api-types.ts` 两个 OpenAPI 重导出物不同；框架 398 文件 0 差异。
- `source-frozen.json` → `source-after.json`：应用 681 文件 0 差异，框架 398 文件 0 差异；视觉、MSRV、Prometheus、部署变异均未改动源码。

## 数量汇总

- 应用 Rust 去重执行：**1474 项**（full 1300 + integration 174）；full 阶段另有 176 个 ignored，其中 174 已在 integration 执行，剩余两项现场材料测试保持未执行。
- 前端：Vitest **938 项**；浏览器 **27 项**（开发 21 + 生产 4 + 视觉 2）。
- 框架：yang-base **874** + yang-runtime **12** = **886 项**；doctest 另计 **104 passed / 113 ignored**。

## 边界

- 本记录只证明该未提交工作树的已执行范围，不等于生产部署、真实 Feishu 外发、TLS 现场、Firefox/WebKit、实际灾备恢复或容量证明。
- 两项现场飞书/回执库测试未执行；框架 52 个 ignored 未逐项补跑；视觉仅 Chromium 本地基线。
- 绿色不关闭尚未实现的工作包（P01/P04/P08 批量与通知/P11/P13/P15 等）；它们仍需按计划继续实现和现场验收。
