#!/usr/bin/env python3
"""只读校验 deploy/ 下 Shell 与 PowerShell 脚本的语法。

CI 的 full 门禁此前只校验运行时行为，部署脚本写错要等上传到服务器才暴露。
这里只做 `bash -n` / PowerShell `scriptblock::Create` 解析，不执行脚本、不读配置值。
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def shell_binary() -> str | None:
    """返回可用的 bash。Windows 上优先 Git Bash，避免落到 WSL 的 bash。"""
    if os.name == "nt":
        for candidate in (
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files\Git\bin\bash.exe",
        ):
            if Path(candidate).is_file():
                return candidate
        return None
    return shutil.which("bash")

SHELL_SCRIPTS = (
    ROOT / "deploy/deploy-blue-green.sh",
    ROOT / "deploy/verify-feishu-user-identity.sh",
    ROOT / "deploy/verify-portal-suggest.sh",
)
POWERSHELL_SCRIPTS = (
    ROOT / "deploy/deploy.ps1",
    ROOT / "deploy/deploy-finance-prod.ps1",
    ROOT / "deploy/verify-feishu-user-identity.ps1",
    ROOT / "deploy/verify-portal-suggest.ps1",
)


def main() -> int:
    errors: list[str] = []
    bash = shell_binary()
    if bash is None:
        print("bash 不可用：跳过 Shell 语法检查（Linux CI 会执行）")
    for script in SHELL_SCRIPTS:
        if not script.is_file():
            errors.append(f"{script.relative_to(ROOT)}: 文件不存在")
            continue
        if bash is None:
            continue
        completed = subprocess.run(
            [bash, "-n", script.as_posix()],
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            errors.append(
                f"{script.relative_to(ROOT)}: {completed.stderr.strip() or 'bash -n 失败'}"
            )

    pwsh = shutil.which("pwsh") or shutil.which("powershell")
    if pwsh is None:
        print("pwsh 不可用：跳过 PowerShell 语法检查（CI runner 自带 pwsh）")
    else:
        for script in POWERSHELL_SCRIPTS:
            if not script.is_file():
                errors.append(f"{script.relative_to(ROOT)}: 文件不存在")
                continue
            escaped = str(script).replace("'", "''")
            parse = (
                "$ErrorActionPreference='Stop'; "
                f"[void][scriptblock]::Create((Get-Content -Raw -LiteralPath '{escaped}'))"
            )
            completed = subprocess.run(
                [pwsh, "-NoProfile", "-NonInteractive", "-Command", parse],
                check=False,
                capture_output=True,
                text=True,
            )
            if completed.returncode != 0:
                errors.append(
                    f"{script.relative_to(ROOT)}: "
                    f"{completed.stderr.strip() or 'PowerShell 语法解析失败'}"
                )

    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("deploy script syntax: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
