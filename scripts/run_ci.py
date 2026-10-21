#!/usr/bin/env python3
"""执行与 yang-system GitHub Actions 对齐的本地质量门禁。"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass


@dataclass(frozen=True)
class Command:
    name: str
    argv: tuple[str, ...]
    env: tuple[tuple[str, str], ...] = ()
    # 依赖外网的命令（如 pnpm audit）允许有限重试；仅网络错误重试，
    # 真实失败（如漏洞命中）立即上报，不掩盖。
    network_retries: int = 0


ARCHITECTURE = (
    Command("Architecture checker self-test", ("python", "scripts/check_architecture.py", "--self-test")),
    Command("Action scaffold self-test", ("python", "scripts/new_action.py", "--self-test")),
    Command(
        "Local config upgrade self-test",
        ("python", "scripts/upgrade_local_config.py", "--self-test"),
    ),
    Command("Architecture check", ("python", "scripts/check_architecture.py")),
)

QUICK = (
    *ARCHITECTURE,
    Command("Rust formatting", ("cargo", "fmt", "--", "--check")),
    Command("Rust library tests", ("cargo", "test", "--lib", "--locked")),
    Command("Frontend typecheck", ("pnpm", "--dir", "frontend", "typecheck")),
    Command("Frontend tests", ("pnpm", "--dir", "frontend", "test")),
)

FRONTEND_PRODUCTION_AUDIT = Command(
    "Frontend production dependency audit",
    (
        "pnpm",
        "--dir",
        "frontend",
        "audit",
        "--prod",
        "--audit-level",
        "moderate",
    ),
    # audit 需要向 registry 提交完整依赖树，CI 网络抖动是已知故障源；
    # 放宽 pnpm 抓取超时并允许有限重试（仅网络错误，见 run()）。
    (
        ("npm_config_fetch_retries", "5"),
        ("npm_config_fetch_retry_maxtimeout", "120000"),
        ("npm_config_fetch_timeout", "300000"),
    ),
    2,
)

FRONTEND_DEV_E2E = Command(
    "Frontend isolated dev-server E2E",
    ("pnpm", "--dir", "frontend", "e2e"),
    (
        ("CI", "true"),
        ("YANG_E2E_FRONTEND_PORT", "5310"),
        ("YANG_E2E_BACKEND_PORT", "18310"),
    ),
)

FRONTEND_PRODUCTION_E2E = Command(
    "Frontend isolated production-build E2E",
    ("pnpm", "--dir", "frontend", "e2e:production"),
    (
        ("CI", "true"),
        ("YANG_PRODUCTION_E2E_FRONTEND_PORT", "5311"),
        ("YANG_PRODUCTION_E2E_BACKEND_PORT", "18311"),
    ),
)

FULL = (
    *ARCHITECTURE,
    Command(
        "Deploy script syntax",
        ("python", "scripts/check_deploy_syntax.py"),
    ),
    Command("Rust formatting", ("cargo", "fmt", "--", "--check")),
    Command("Rust all-target tests", ("cargo", "test", "--all-targets", "--locked")),
    Command(
        "Rust clippy",
        (
            "cargo",
            "clippy",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ),
    ),
    FRONTEND_PRODUCTION_AUDIT,
    Command("Frontend full check", ("pnpm", "--dir", "frontend", "check")),
    FRONTEND_DEV_E2E,
    FRONTEND_PRODUCTION_E2E,
)

INTEGRATION = (
    Command(
        "审批配置名称刷新与保旧审计集成测试",
        ("cargo", "test", "--lib", "--locked", "real_mysql_refresh_names_saves_deduplicates_preserves_and_audits", "--", "--ignored", "--test-threads=1"),
    ),
    Command(
        "审批明细外部选项精确查询集成测试",
        ("cargo", "test", "--lib", "--locked", "real_mysql_resolves_detail_options_and_picks_first_on_duplicate_labels", "--", "--ignored", "--test-threads=1"),
    ),
    Command(
        "飞书用户身份凭证读写往返集成测试",
        ("cargo", "test", "--lib", "--locked", "real_mysql_round_trips_secret_token_columns", "--", "--ignored", "--test-threads=1"),
    ),
    Command(
        "Authorization Redis monotonic cache integration",
        (
            "cargo",
            "test",
            "--lib",
            "--locked",
            "authorization::version_cache::tests::real_redis_publish_is_monotonic_and_does_not_refresh_ignored_events",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Authorization outbox worker replay integration",
        (
            "cargo",
            "test",
            "--lib",
            "--locked",
            "authorization::worker::tests::real_outbox_supports_concurrent_claim_retry_and_expired_lease_replay",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Schema apply concurrency and retry integration",
        (
            "cargo",
            "test",
            "--test",
            "schema_apply_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "MFA email code and TOTP deactivate integration",
        (
            "cargo",
            "test",
            "--test",
            "mfa_email_code_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Login email code integration",
        (
            "cargo",
            "test",
            "--test",
            "login_email_code_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Avatar integration",
        (
            "cargo",
            "test",
            "--test",
            "avatar_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Account deletion and anonymization integration",
        (
            "cargo",
            "test",
            "--test",
            "account_deletion_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Session revocation integration",
        (
            "cargo",
            "test",
            "--test",
            "session_revocation_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Permission groups token signing integration",
        (
            "cargo",
            "test",
            "--test",
            "permission_groups_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "直授持有者反向查询集成测试",
        (
            "cargo",
            "test",
            "--test",
            "grant_holders_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "用户查找 authenticated-only 集成测试",
        (
            "cargo",
            "test",
            "--test",
            "user_lookup_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "首账号引导并发仲裁集成测试",
        (
            "cargo",
            "test",
            "--test",
            "system_owner_bootstrap_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Refresh rotation load benchmark",
        (
            "cargo",
            "test",
            "--test",
            "refresh_load_benchmark",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "飞书选项 Schema 集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_options_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "飞书选项复合索引集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_option_index_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "飞书取选项端点集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_approval_options_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "飞书 xlsx 导入集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_xlsx_import_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "审批派发控制台与落记录集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_approval_dispatch_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
)


def executable(name: str) -> str:
    if name == "python":
        return sys.executable
    resolved = shutil.which(name)
    if resolved is None:
        raise RuntimeError(f"缺少命令: {name}")
    return resolved


NETWORK_ERROR_MARKERS = (
    "ERR_SOCKET_TIMEOUT",
    "ETIMEDOUT",
    "ECONNRESET",
    "ECONNREFUSED",
    "EAI_AGAIN",
    "FetchError",
    "socket hang up",
)


def run(command: Command) -> None:
    argv = [executable(command.argv[0]), *command.argv[1:]]
    environment = os.environ.copy()
    environment.update(command.env)
    # 本机 HTTP 用例直连 127.0.0.1；开发者环境常设有 HTTP_PROXY（本机 VPN），
    # 不排除回环地址时请求会被代理吞掉，表现为偶发 "error sending request"
    # 或测试挂起。只放行回环地址，外网请求仍按环境代理走（如 pnpm audit）。
    environment["NO_PROXY"] = "127.0.0.1,localhost"
    environment["no_proxy"] = "127.0.0.1,localhost"
    print(f"\n==> {command.name}\n    {shlex.join(argv)}", flush=True)
    if command.network_retries <= 0:
        subprocess.run(argv, check=True, env=environment)
        return
    for attempt in range(1, command.network_retries + 2):
        completed = subprocess.run(
            argv,
            check=False,
            env=environment,
            capture_output=True,
            text=True,
            # pnpm/npm 的输出是 UTF-8；Windows 默认按 GBK 解码会在出现
            # 非 ASCII 字符（如 ✓/中文提示）时让 reader 线程直接抛
            # UnicodeDecodeError，把成功的审计误报成失败。
            encoding="utf-8",
            errors="replace",
        )
        output = f"{completed.stdout}\n{completed.stderr}"
        if completed.returncode == 0:
            if output.strip():
                print(output, flush=True)
            return
        print(output, flush=True)
        network_error = any(marker in output for marker in NETWORK_ERROR_MARKERS)
        if not network_error or attempt > command.network_retries:
            raise subprocess.CalledProcessError(completed.returncode, argv)
        wait_seconds = 30 * attempt
        print(
            f"    网络错误，{wait_seconds} 秒后重试（第 {attempt} 次）",
            flush=True,
        )
        time.sleep(wait_seconds)


def self_test() -> None:
    workflow = open(".github/workflows/ci.yml", encoding="utf-8").read()
    with open("frontend/package.json", encoding="utf-8") as package_file:
        frontend_package = json.load(package_file)
    frontend_check_steps = frontend_package["scripts"]["check"].split(" && ")
    assert "pnpm verify:locale-contract" in frontend_check_steps, (
        "frontend check 必须执行单语言产品合同门禁"
    )
    assert frontend_check_steps.index("pnpm verify:locale-contract") < frontend_check_steps.index(
        "pnpm build"
    ), "单语言产品合同门禁必须在生产构建前执行"
    assert "python scripts/run_ci.py full" in workflow
    assert "python scripts/run_ci.py integration" in workflow
    required_browser_commands = {
        ("pnpm", "--dir", "frontend", "e2e"),
        ("pnpm", "--dir", "frontend", "e2e:production"),
    }
    actual_full_commands = {command.argv for command in FULL}
    assert required_browser_commands <= actual_full_commands, (
        "full 门禁必须同时执行 dev-server 与 production-build Playwright"
    )
    assert workflow.count(
        "pnpm --dir frontend exec playwright install --with-deps chromium"
    ) == 1, "quality job 必须安装 Playwright Chromium 与系统依赖"
    assert workflow.count(
        "nginx:1.30.4-alpine3.24@sha256:"
    ) == 1, "quality job 必须用版本与摘要双重固定的 Nginx 镜像加载生产部署配置"
    assert workflow.count(
        "frontend/deploy/nginx.conf:/etc/nginx/nginx.conf:ro"
    ) == 1, "Nginx 语法检查必须只读加载仓库内的生产配置"
    assert workflow.count("nginx -t") == 1, "quality job 必须执行真实 Nginx 语法检查"
    assert workflow.count(
        "prom/prometheus:v3.11.3@sha256:"
    ) == 2, "quality job 必须用版本与摘要双重固定的 Prometheus 镜像执行两项校验"
    assert workflow.count("--entrypoint /bin/promtool") == 2, (
        "quality job 必须用真实 promtool 校验规则与告警演练"
    )
    assert "check rules yang-system.rules.yml" in workflow
    assert "test rules yang-system.rules.test.yml" in workflow
    assert FRONTEND_DEV_E2E.env == (
        ("CI", "true"),
        ("YANG_E2E_FRONTEND_PORT", "5310"),
        ("YANG_E2E_BACKEND_PORT", "18310"),
    )
    assert FRONTEND_PRODUCTION_E2E.env == (
        ("CI", "true"),
        ("YANG_PRODUCTION_E2E_FRONTEND_PORT", "5311"),
        ("YANG_PRODUCTION_E2E_BACKEND_PORT", "18311"),
    )
    dev_ports = {value for name, value in FRONTEND_DEV_E2E.env if name.endswith("_PORT")}
    production_ports = {
        value for name, value in FRONTEND_PRODUCTION_E2E.env if name.endswith("_PORT")
    }
    assert dev_ports.isdisjoint(production_ports), "两套浏览器门禁不得复用端口值"
    assert workflow.count("ssh-key: ${{ secrets.LIB_YANG_SSH_KEY }}") == 3, (
        "每个 lib_yang 跨仓库 checkout 都必须使用只读 Deploy Key"
    )
    assert workflow.count("persist-credentials: false") == 3, (
        "lib_yang checkout 不得在 runner 中持久化 Deploy Key 凭据"
    )
    assert any(command.argv[:3] == ("cargo", "test", "--all-targets") for command in FULL)
    assert any(command.argv[:2] == ("pnpm", "--dir") for command in FULL)
    assert FRONTEND_PRODUCTION_AUDIT.argv == (
        "pnpm",
        "--dir",
        "frontend",
        "audit",
        "--prod",
        "--audit-level",
        "moderate",
    )
    assert FULL.index(FRONTEND_PRODUCTION_AUDIT) < next(
        index for index, command in enumerate(FULL) if command.name == "Frontend full check"
    )
    assert FULL.index(FRONTEND_DEV_E2E) < FULL.index(FRONTEND_PRODUCTION_E2E)
    for command in (*QUICK, *FULL, *INTEGRATION):
        if command.argv[0] == "cargo" and command.argv[1] != "fmt":
            assert "--locked" in command.argv, f"Cargo 命令缺少 --locked: {command.name}"
    integration_tests = {
        command.argv[3]
        for command in INTEGRATION
        if command.argv[:3] == ("cargo", "test", "--test")
    }
    # 反向校验：tests/ 下每个依赖容器（YANG_SYSTEM_TEST_*）的集成测试入口都必须登记在
    # INTEGRATION。写死清单只能证明「登记项没写错」，无法发现**漏登记**——历史缺
    # account_deletion_integration / session_revocation_integration /
    # refresh_load_benchmark 三个入口，它们因此从未被任何门禁执行。
    discovered_integration_tests = {
        os.path.splitext(name)[0]
        for name in os.listdir("tests")
        if name.endswith(".rs")
        and "YANG_SYSTEM_TEST_"
        in open(os.path.join("tests", name), encoding="utf-8").read()
    }
    assert integration_tests == discovered_integration_tests, (
        "INTEGRATION 与 tests/ 下依赖容器的集成测试不一致: "
        f"漏登记={sorted(discovered_integration_tests - integration_tests)} "
        f"登记了不存在的入口={sorted(integration_tests - discovered_integration_tests)}"
    )
    # `--lib <filter>` 在测试改名后会匹配 0 个测试并以 0 退出（静默假绿）。
    # 自检直接回到源码里找同名符号：过滤名不存在就立刻失败。
    lib_filter_commands = [
        command
        for command in INTEGRATION
        if command.argv[:3] == ("cargo", "test", "--lib") and len(command.argv) > 4
    ]
    assert lib_filter_commands, "INTEGRATION 必须包含 --lib 过滤测试"
    rust_sources = []
    for source_root, _, filenames in os.walk("src"):
        for filename in filenames:
            if filename.endswith(".rs"):
                with open(
                    os.path.join(source_root, filename), encoding="utf-8"
                ) as source_file:
                    rust_sources.append(source_file.read())
    source_blob = "\n".join(rust_sources)
    for command in lib_filter_commands:
        filter_name = command.argv[4]
        # cargo 的过滤是子串匹配，带模块路径的过滤名在源码里通常只出现叶子函数名。
        leaf_name = filter_name.rsplit("::", 1)[-1]
        assert leaf_name in source_blob, (
            f"--lib 过滤名 {filter_name} 的测试函数 {leaf_name} 已不在 src 中："
            "测试改名后 cargo 会以 0 个测试成功退出"
        )
    authorization_cache = next(
        command
        for command in INTEGRATION
        if command.name == "Authorization Redis monotonic cache integration"
    )
    assert authorization_cache.argv[:4] == (
        "cargo",
        "test",
        "--lib",
        "--locked",
    )
    print("local CI runner self-test: passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "profile",
        nargs="?",
        choices=("quick", "full", "integration"),
        default="quick",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    commands = {"quick": QUICK, "full": FULL, "integration": INTEGRATION}[args.profile]
    for command in commands:
        run(command)
    print(f"\nCI profile passed: {args.profile}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
