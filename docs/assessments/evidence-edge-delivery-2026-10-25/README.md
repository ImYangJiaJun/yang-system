# 边缘与交付审核：本地可重跑证据

日期：2026-10-25。仅本机最小复现，不是生产测量。

基线：应用起始 `0adc5ab1cbd0729dfdf264fe617ae07a029ee585`；共享会话随后提交飞书改动，审核收尾 `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`。框架 `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`。没有修改应用或框架实现。

Rust 复现锁定 axum 0.8.9、tower 0.5.3、tower-http 0.7.0、tokio 1.53.1、tracing 0.1.44，完整传递依赖见本目录 `Cargo.lock`。与框架的关键依赖版本一致；独立 crate，独立 target，不以此声称应用完整测试通过。

从应用仓根目录执行：

```powershell
$env:CARGO_TARGET_DIR='D:/code/lib_yang/project/yang-system/target/edge-delivery-audit-repro'
cargo run --offline --locked --manifest-path docs/assessments/evidence-edge-delivery-2026-10-25/Cargo.toml
python docs/assessments/evidence-edge-delivery-2026-10-25/shell-self-check.py
```

归档的原始输出见 `rust-run-output.txt`、`shell-run-output.txt`（均退出 0）。下面是首次完整成功的代表输出；后续为保存原始输出重跑，耗时略有变化：

```text
separate Router layers: first=200 OK second=408 Request Timeout second_elapsed_ms=658 (each handler 450ms, timeout 650ms)
split LoadShed: first=200 OK second=200 OK second_elapsed_ms=910
combined LoadShed: first=200 OK second=503 Service Unavailable second_elapsed_ms=0
default 404/405 TraceLayer on_request count=2
per-route max_concurrency=1: cross-route peak=2
per-method max_concurrency=1: same-path GET+POST peak=2
shutdown take-then-await + Drop abort: after timeout task still completed=1
```

最后一次 shell 输出（退出 0）：

```text
probe_code with curl exit=28 and status=200 returns=200
stop_all_pairs with live backend still running returns=0 (caller can continue reset)
failed dump pipeline without pipefail returns=0
```

过程限制：最初 Rust 模型漏掉生产代码的 `with_state`，得到 200/200、assert 失败；补齐 `with_state` 后以上断言全部通过，说明不能只看 Layer 名称推断行为。最初 shell 自检被 Windows 的 WSL `bash.exe` 截获而失败；改用 Git Bash 后通过。耗时数字只为本机一次样本，断言验证状态和并发计数，未将这些毫秒值外推线上。

其他实际检查：

- `python scripts/run_ci.py --self-test`：通过。
- `node scripts/verify-deployment-contract.mjs`（cwd=`frontend/`）：通过，8 个对抗变异被拒绝。首次从仓根运行报路径 ENOENT，随后使用脚本要求的 cwd 重跑。
- `C:/Program Files/Git/bin/bash.exe -n deploy/deploy-blue-green.sh`：通过。

shell 自检只抽取函数并替换 curl/docker 相关调用；没有执行部署脚本 main、网络请求、Docker 变更、SSH 或数据库命令。没有生产配置读取、生产故障注入、TLS/证书签发、nginx 容器演练、完整应用测试或真实数据库验证。
