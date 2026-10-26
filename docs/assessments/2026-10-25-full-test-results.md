# 全量测试执行结果

本文件保留修复前的历史基线。修复分支的新一轮结果见[基础设施修复分支全量测试](2026-10-25-remediation-full-test-results.md)，请勿将下文的视觉失败和旧用例数量当作修复后现状。

日期：2026-10-25。**标准 `full` 与 `integration` 门禁通过；扩展到 CI 默认跳过的本地视觉测试后，仍有 1 项稳定失败，因此不能称全部测试通过。** 本轮没有修改生产代码、测试断言、截图基线或依赖清单，没有提交或部署。

本记录补充[全基础设施对抗性审核](2026-10-25-all-infrastructure-adversarial-review.md)的验证范围；已有测试通过不代表其中新增风险已经修复或都有回归覆盖。

## 基线与环境

- 应用：`5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`；框架：`e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`，执行前后均未变化。
- Windows / PowerShell；Rust 1.97.1、Node 24.21.0、pnpm 10.33.1、Python 3.14.7。
- 集成测试使用本轮单独创建的 MySQL 8.0.46 与 Redis 7 容器、`yang_system_test` 数据库和 Redis DB 15。仅绑定动态 loopback 端口，数据放在 tmpfs；按创建时记录的容器 ID 并复核名称后已删除。既有本地服务保持运行。
- MSRV 使用独立 `rust:1.80.1-slim` 容器，源码只读挂载，显式设置 `RUSTUP_TOOLCHAIN=1.80.1`、`CARGO_HOME=/tmp/ch`、`CARGO_TARGET_DIR=/tmp/ct`；冷缓存检查后容器删除。
- 原始日志与失败截图保存在仓库被忽略的 `target/full-test-2026-10-25/`；清理 target 会同时清除这些原始证据。

## 结果

| 检查 | 结果 | 日志 |
|---|---|---|
| `python scripts/run_ci.py --self-test` | 退出 0 | [runner-self-test.log](../../target/full-test-2026-10-25/runner-self-test.log) |
| `python scripts/run_ci.py full` | 使用下述进程环境覆盖后退出 0 | [full-official-registry.log](../../target/full-test-2026-10-25/full-official-registry.log) |
| Rust 全目标测试 | 1274 passed；156 个需外部依赖/专门条件的测试在此阶段 ignored，后续单独执行 | 同上 |
| 架构及脚手架自检、部署语法、rustfmt、全目标全特性 Clippy | 全部通过 | 同上 |
| 前端依赖审计、Prettier、ESLint、TypeScript | 全部通过；官方 registry 返回 No known vulnerabilities found | 同上 |
| Vitest | 78 个测试文件、903 个用例通过 | 同上 |
| 产品文案、构建、生产产物、bundle、部署契约 | 全部通过；首屏 JS gzip 273.4 kB；8 个部署契约变异被拒绝 | 同上 |
| 开发服务器 Chromium E2E | 21 passed、2 skipped；两项视觉测试随后单独补跑 | 同上 |
| 生产构建 Chromium E2E | 4 passed | 同上 |
| `python scripts/run_ci.py integration` | 23 组命令、152 个用例通过，退出 0；含 16 个 tests/ 入口及 7 个库测试 | [integration.log](../../target/full-test-2026-10-25/integration.log) |
| 补跑 TOTP 原子消费测试 | 1 passed，退出 0 | [supplement-totp.log](../../target/full-test-2026-10-25/supplement-totp.log) |
| 补跑 Outbox 预编译时间边界测试 | 1 passed，退出 0 | [supplement-outbox.log](../../target/full-test-2026-10-25/supplement-outbox.log) |
| `cargo test --doc --locked` | 退出 0；0 passed、1 个显式 ignore 示例，不能计为文档示例执行成功 | [doctest.log](../../target/full-test-2026-10-25/doctest.log) |
| Rust 1.80.1 冷缓存 `cargo check --all-targets --locked` | 退出 0，1m48s；实际 rustc/cargo 均为 1.80.1 | [msrv.log](../../target/full-test-2026-10-25/msrv.log) |
| 固定摘要 Nginx `nginx -t` | 退出 0 | [infra-nginx.log](../../target/full-test-2026-10-25/infra-nginx.log) |
| 固定摘要 promtool 规则检查与告警演练 | 退出均为 0；19 条规则、演练 SUCCESS | [规则](../../target/full-test-2026-10-25/infra-promtool-check.log)、[演练](../../target/full-test-2026-10-25/infra-promtool-test.log) |
| 本地视觉测试 | 1 passed、1 failed；失败项单独复跑仍失败 | [visual.log](../../target/full-test-2026-10-25/visual.log)、[visual-confirm.log](../../target/full-test-2026-10-25/visual-confirm.log) |

Rust 实际通过合计 **1428**（1274 + 152 + 2），不把多次环境重跑重复计数。两个补跑的库测试目前未登记在 `INTEGRATION`；现有 runner 自检验证了已登记过滤名存在，但没有发现这两个漏登记项。

## 失败与环境问题

1. **本地视觉测试稳定失败**：`frontend/e2e/visual.spec.ts:73` 的 `action-dialog-light.png` 两次均有 47,547 像素差异，Playwright 显示比例约 0.06，超过 0.02 阈值。对话框高度、留白和分隔线、品牌/文案、背景数据与基线不同。当前截图右上角关闭 X 不可见，DOM 仍有 Close 按钮；尚未验证点击命中，不能直接把所有差异接受为新基线。此次始终使用 `--update-snapshots=none`。
2. **首次 full 被输出编码中断**：Python 在 `run_ci.py:440` 打印 pnpm 审计输出时因 GBK 无法编码 U+2009 抛错。设置本次进程 `PYTHONUTF8=1` 后，显示真实的下一层环境错误。首次日志：[full.log](../../target/full-test-2026-10-25/full.log)。
3. **第二次 full 被 registry 缺失接口中断**：当前镜像的 `https://registry.npmmirror.com/-/npm/v1/security/audits/quick` 不存在。设置本次进程 `npm_config_registry=https://registry.npmjs.org` 后审计和整个 full 通过。没有改全局 npm 配置或仓库文件。第二次日志：[full-utf8.log](../../target/full-test-2026-10-25/full-utf8.log)。
4. **非阻断的浏览器诊断**：批量删除 E2E 通过，但日志有 React `Cannot update a component (TableView) while rendering a different component (DataGrid)`。本轮未进一步定位或修复，不能因用例绿色而忽略该诊断。

确认复跑的[实际截图](../../target/full-test-2026-10-25/visual-confirm-artifacts/visual-工作台表格页与-Action-对话框明暗主题-chromium/action-dialog-light-actual.png)、[差异图](../../target/full-test-2026-10-25/visual-confirm-artifacts/visual-工作台表格页与-Action-对话框明暗主题-chromium/action-dialog-light-diff.png)与 [trace](../../target/full-test-2026-10-25/visual-confirm-artifacts/visual-工作台表格页与-Action-对话框明暗主题-chromium/trace.zip)均保留。

## 补跑命令与未执行项

完整门禁的成功调用仅增加进程环境变量：

```powershell
$env:PYTHONUTF8 = '1'
$env:npm_config_registry = 'https://registry.npmjs.org'
python scripts/run_ci.py full
```

在 integration 完成后，沿用独立测试库和 Redis DB 15 环境，串行补跑：

```powershell
cargo test --lib --locked addon::account::domain::context::tests::totp_step_consumption_is_atomic_and_monotonic -- --ignored --exact --test-threads=1
cargo test --lib --locked infrastructure::authorization::outbox::tests::claim_refreshes_prepared_statement_time_boundary -- --ignored --exact --test-threads=1
cargo test --doc --locked
```

视觉测试取消进程 CI 标志后执行 `pnpm --dir frontend exec playwright test e2e/visual.spec.ts --update-snapshots=none`；确认复跑仅选 `e2e/visual.spec.ts:40`，输出另存，未覆盖首次失败材料。

剩余两个 ignored 库测试未运行：`real_app_sends_rich_feedback_and_confirms_sender_and_content` 会读取真实应用凭据并向真实飞书用户发送消息，执行全量本地测试不包含对外发信授权；`saved_rich_feedback_receipt_preserves_title_content_styles_and_sender` 虽完全只读，但所需三份历史回执均不存在，未伪造材料或发送新消息补齐。

未执行生产环境、真实飞书/TLS、Firefox/WebKit、完整容量压测或框架仓自身的全工作区测试。应用测试会编译链接当前框架依赖；这不等于执行框架全部测试。浏览器门禁使用演示后端，集成测试使用真实本地 MySQL/Redis，不替代生产验收。此前基础设施审核中的未修复风险仍然存在。
