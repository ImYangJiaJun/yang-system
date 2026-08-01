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
from dataclasses import dataclass


@dataclass(frozen=True)
class Command:
    name: str
    argv: tuple[str, ...]
    env: tuple[tuple[str, str], ...] = ()


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
        "Versioned migration job integration",
        (
            "cargo",
            "test",
            "--test",
            "migration_job_integration",
            "--locked",
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
        "System-owner registration integration",
        (
            "cargo",
            "test",
            "--test",
            "bootstrap_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Tenant isolation evidence integration",
        (
            "cargo",
            "test",
            "--test",
            "tenant_isolation_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Registration email verification integration",
        (
            "cargo",
            "test",
            "--test",
            "registration_email_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
    Command(
        "Real MySQL/Redis system integration",
        (
            "cargo",
            "test",
            "--test",
            "system_integration",
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


def run(command: Command) -> None:
    argv = [executable(command.argv[0]), *command.argv[1:]]
    environment = os.environ.copy()
    environment.update(command.env)
    print(f"\n==> {command.name}\n    {shlex.join(argv)}", flush=True)
    subprocess.run(argv, check=True, env=environment)


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
    assert integration_tests == {
        "bootstrap_integration",
        "migration_job_integration",
        "registration_email_integration",
        "schema_apply_integration",
        "system_integration",
        "tenant_isolation_integration",
    }
    tenant_isolation = next(
        command
        for command in INTEGRATION
        if command.argv[:4]
        == ("cargo", "test", "--test", "tenant_isolation_integration")
    )
    assert tenant_isolation.argv == (
        "cargo",
        "test",
        "--test",
        "tenant_isolation_integration",
        "--locked",
        "--",
        "--ignored",
        "--test-threads=1",
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
