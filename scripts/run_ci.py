#!/usr/bin/env python3
"""执行与 yang-system GitHub Actions 对齐的本地质量门禁。"""

from __future__ import annotations

import argparse
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


ARCHITECTURE = (
    Command("Architecture checker self-test", ("python", "scripts/check_architecture.py", "--self-test")),
    Command("Action scaffold self-test", ("python", "scripts/new_action.py", "--self-test")),
    Command("Architecture check", ("python", "scripts/check_architecture.py")),
)

QUICK = (
    *ARCHITECTURE,
    Command("Rust formatting", ("cargo", "fmt", "--", "--check")),
    Command("Rust library tests", ("cargo", "test", "--lib", "--locked")),
    Command("Frontend typecheck", ("pnpm", "--dir", "frontend", "typecheck")),
    Command("Frontend tests", ("pnpm", "--dir", "frontend", "test")),
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
    Command("Frontend full check", ("pnpm", "--dir", "frontend", "check")),
)

INTEGRATION = (
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
    print(f"\n==> {command.name}\n    {shlex.join(argv)}", flush=True)
    subprocess.run(argv, check=True, env=os.environ.copy())


def self_test() -> None:
    workflow = open(".github/workflows/ci.yml", encoding="utf-8").read()
    assert "python scripts/run_ci.py full" in workflow
    assert "python scripts/run_ci.py integration" in workflow
    assert any(command.argv[:3] == ("cargo", "test", "--all-targets") for command in FULL)
    assert any(command.argv[:2] == ("pnpm", "--dir") for command in FULL)
    for command in (*QUICK, *FULL, *INTEGRATION):
        if command.argv[0] == "cargo" and command.argv[1] != "fmt":
            assert "--locked" in command.argv, f"Cargo 命令缺少 --locked: {command.name}"
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
