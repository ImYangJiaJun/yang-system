#!/usr/bin/env python3
"""检查并显式升级旧版 Windows 本地配置，不输出任何敏感值。"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import tempfile
import tomllib
import unittest
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any


SECTION_PATTERN = re.compile(r"^\[([^\]]+)\]$")
SCALAR_PATTERN = re.compile(r"^([A-Za-z0-9_]+)\s*=\s*(.+)$")
TOKEN_PLACEHOLDER = "replace-with-at-least-32-random-bytes"
STEP_UP_PLACEHOLDER = "replace-with-independent-step-up-secret"
MYSQL_PLACEHOLDER = "mysql://root:password@127.0.0.1:3306/yang_system"
LOCAL_MYSQL_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system"
LOCAL_REDIS_URL = "redis://127.0.0.1:6379"
MISSING = object()


@dataclass(frozen=True)
class Inspection:
    """仅包含配置形状，不包含任何配置值。"""

    current: bool
    has_legacy_token_secret: bool


def parse_document(raw: str) -> dict[str, Any]:
    """解析 TOML，错误消息不回显可能包含密钥的原文。"""

    try:
        document = tomllib.loads(raw)
    except tomllib.TOMLDecodeError as error:
        raise ValueError("config.toml 不是有效 TOML") from error
    if not isinstance(document, dict):
        raise ValueError("config.toml 顶层必须是 TOML table")
    return document


def scalar_values(raw: str) -> dict[str, str]:
    """读取单行 scalar 的原始 RHS，以便无损保留字符串和数字表示。"""

    values: dict[str, str] = {}
    section = ""
    for line in raw.splitlines():
        stripped = line.strip()
        section_match = SECTION_PATTERN.fullmatch(stripped)
        if section_match:
            section = section_match.group(1)
            continue
        scalar_match = SCALAR_PATTERN.fullmatch(stripped)
        if scalar_match:
            values[f"{section}.{scalar_match.group(1)}"] = scalar_match.group(2)
    return values


def nested_value(document: dict[str, Any], path: str) -> Any:
    current: Any = document
    for part in path.split("."):
        if not isinstance(current, dict) or part not in current:
            return MISSING
        current = current[part]
    return current


def inspect_config(raw: str, template_raw: str) -> Inspection:
    """判断配置是否包含当前本地模板要求的完整形状。"""

    document = parse_document(raw)
    template_document = parse_document(template_raw)
    required = set(scalar_values(template_raw))
    token_secret = nested_value(document, "token.active_secret")
    step_up_secret = nested_value(document, "step_up.active_secret")
    mysql_url = nested_value(document, "mysql.url")
    token_table = nested_value(document, "token")
    has_legacy_token_secret = (
        isinstance(token_table, dict) and "secret" in token_table
    )
    current = (
        all(nested_value(document, path) is not MISSING for path in required)
        and not has_legacy_token_secret
        and token_secret not in (MISSING, TOKEN_PLACEHOLDER)
        and step_up_secret not in (MISSING, STEP_UP_PLACEHOLDER)
        and mysql_url not in (MISSING, MYSQL_PLACEHOLDER)
    )
    if not isinstance(template_document, dict):
        raise AssertionError("模板解析结果必须是 table")
    return Inspection(
        current=current,
        has_legacy_token_secret=has_legacy_token_secret,
    )


def quote_toml_string(value: str) -> str:
    """当前字段均为普通 TOML basic string；JSON 转义与其兼容。"""

    return json.dumps(value, ensure_ascii=False)


def upgrade_text(
    raw: str,
    template_raw: str,
) -> str:
    """以当前模板为骨架迁移已知旧字段，并保留仍有效的旧 scalar。"""

    document = parse_document(raw)
    template_document = parse_document(template_raw)
    values = scalar_values(raw)
    template_values = scalar_values(template_raw)

    if "token.active_secret" not in values:
        legacy_secret = values.get("token.secret")
        if legacy_secret is None:
            raise ValueError("旧配置缺少 token.secret/active_secret，拒绝自动升级")
        values["token.active_secret"] = legacy_secret

    values["mysql.url"] = quote_toml_string(LOCAL_MYSQL_URL)
    values["redis.url"] = quote_toml_string(LOCAL_REDIS_URL)
    existing_step_up_secret = nested_value(document, "step_up.active_secret")
    if existing_step_up_secret in (MISSING, STEP_UP_PLACEHOLDER):
        values["step_up.active_secret"] = quote_toml_string(secrets.token_urlsafe(48))

    # 对无法用单行 RHS 无损表达的自定义数组/table 拒绝静默降级为模板默认值。
    for path in template_values:
        existing = nested_value(document, path)
        template = nested_value(template_document, path)
        if path not in values and existing is not MISSING and existing != template:
            raise ValueError(f"配置项 {path} 使用复杂 TOML 结构，请手工迁移")

    upgraded: list[str] = []
    section = ""
    for line in template_raw.splitlines():
        stripped = line.strip()
        section_match = SECTION_PATTERN.fullmatch(stripped)
        if section_match:
            section = section_match.group(1)
            upgraded.append(line)
            continue
        scalar_match = SCALAR_PATTERN.fullmatch(stripped)
        if scalar_match:
            key = scalar_match.group(1)
            path = f"{section}.{key}"
            if path in values:
                indent = line[: len(line) - len(line.lstrip())]
                upgraded.append(f"{indent}{key} = {values[path]}")
                continue
        upgraded.append(line)

    result = "\n".join(upgraded) + "\n"
    if not inspect_config(result, template_raw).current:
        raise ValueError("升级后的配置仍不满足当前本地模板")
    return result


def upgrade_file(
    config_path: Path,
    template_path: Path,
) -> Path:
    """备份后原子替换配置，返回不含敏感值的备份路径。"""

    raw = config_path.read_text(encoding="utf-8")
    template_raw = template_path.read_text(encoding="utf-8")
    upgraded = upgrade_text(raw, template_raw)

    backup_directory = config_path.parent / "target" / "local-config-backups"
    backup_directory.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    backup_path = backup_directory / f"config.toml.{timestamp}.bak"
    shutil.copy2(config_path, backup_path)

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=config_path.parent,
            prefix=".config.toml.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary.write(upgraded)
            temporary_path = Path(temporary.name)
        os.replace(temporary_path, config_path)
    finally:
        if temporary_path is not None and temporary_path.exists():
            temporary_path.unlink()
    return backup_path


class UpgradeLocalConfigTests(unittest.TestCase):
    """旧配置升级的最小回归矩阵。"""

    @classmethod
    def setUpClass(cls) -> None:
        cls.template = (
            Path(__file__).resolve().parent.parent / "config.example.toml"
        ).read_text(encoding="utf-8")

    def legacy_config(self) -> str:
        return """
[app]
name = "legacy"

[schema]
mode = "validate"

[http]
bind = "127.0.0.1:8181"
max_body_bytes = 1048576
request_timeout_seconds = 30
max_concurrency = 64

[mysql]
url = "mysql://root:legacy@127.0.0.1:3306/yang"
max_connections = 7
min_connections = 1
connect_timeout_seconds = 10
idle_timeout_seconds = 600
max_lifetime_seconds = 1800
test_before_acquire = true

[redis]
url = "redis://127.0.0.1:6380"
max_connections = 7
min_connections = 1
connect_timeout_seconds = 5
wait_timeout_seconds = 10
idle_timeout_seconds = 300
max_lifetime_seconds = 1800
test_before_acquire = true

[token]
secret = "legacy-token-secret-with-more-than-32-bytes"
issuer = "legacy"
audience = "legacy-api"
access_ttl_seconds = 300
refresh_ttl_seconds = 3600

[security]
argon2_max_concurrency = 2
auth_rate_limit_window_seconds = 60
auth_rate_limit_ip_attempts = 20
auth_rate_limit_username_attempts = 8

[logging]
filter = "yang_system=debug"
""".lstrip()

    def test_upgrade_preserves_compatible_values_and_adds_current_contract(
        self,
    ) -> None:
        legacy = self.legacy_config()
        self.assertFalse(inspect_config(legacy, self.template).current)
        upgraded = upgrade_text(legacy, self.template)
        values = scalar_values(upgraded)
        self.assertEqual(
            values["token.active_secret"],
            '"legacy-token-secret-with-more-than-32-bytes"',
        )
        self.assertIn("step_up.active_secret", values)
        self.assertNotEqual(
            values["step_up.active_secret"], values["token.active_secret"]
        )
        self.assertNotEqual(values["step_up.active_secret"], quote_toml_string(STEP_UP_PLACEHOLDER))
        self.assertNotIn("token.secret", values)
        self.assertEqual(values["http.bind"], '"127.0.0.1:8181"')
        self.assertEqual(values["mysql.max_connections"], "7")
        self.assertEqual(values["mysql.url"], quote_toml_string(LOCAL_MYSQL_URL))
        self.assertEqual(values["redis.url"], quote_toml_string(LOCAL_REDIS_URL))
        self.assertIn("authorization.deployment", values)
        self.assertIn("observability.readiness_budget_ms", values)
        self.assertTrue(inspect_config(upgraded, self.template).current)

    def test_upgrade_backs_up_original_before_atomic_replace(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "config.toml"
            template_path = root / "config.example.toml"
            original = self.legacy_config()
            config_path.write_text(original, encoding="utf-8")
            template_path.write_text(self.template, encoding="utf-8")
            backup_path = upgrade_file(config_path, template_path)
            self.assertEqual(backup_path.read_text(encoding="utf-8"), original)
            self.assertTrue(
                inspect_config(
                    config_path.read_text(encoding="utf-8"),
                    self.template,
                ).current
            )

    def test_complex_custom_array_is_not_silently_discarded(self) -> None:
        legacy = self.legacy_config().replace(
            "[security]",
            '[[token.retiring_keys]]\n'
            'key_id = "retiring-1"\n'
            'secret = "retiring-token-secret-with-more-than-32-bytes"\n\n'
            "[security]",
        )
        with self.assertRaisesRegex(ValueError, "复杂 TOML 结构"):
            upgrade_text(legacy, self.template)

    def test_current_config_accepts_multiline_retiring_key_array(self) -> None:
        current = (
            self.template.replace(TOKEN_PLACEHOLDER, "active-secret-32-bytes-or-more-value")
            .replace(
                STEP_UP_PLACEHOLDER,
                "independent-step-up-secret-32-bytes-or-more-value",
            )
            .replace(MYSQL_PLACEHOLDER, LOCAL_MYSQL_URL)
            .replace("retiring_keys = []\n", "", 1)
            + '\n[[token.retiring_keys]]\n'
            'key_id = "retiring-1"\n'
            'secret = "retiring-token-secret-with-more-than-32-bytes"\n'
        )
        self.assertTrue(inspect_config(current, self.template).current)


def run_self_test() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        UpgradeLocalConfigTests
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    subparsers = parser.add_subparsers(dest="command")

    inspect_parser = subparsers.add_parser("inspect")
    inspect_parser.add_argument("--config", type=Path, required=True)
    inspect_parser.add_argument("--template", type=Path, required=True)

    upgrade_parser = subparsers.add_parser("upgrade")
    upgrade_parser.add_argument("--config", type=Path, required=True)
    upgrade_parser.add_argument("--template", type=Path, required=True)

    args = parser.parse_args()
    if args.self_test:
        return run_self_test()
    if args.command == "inspect":
        inspection = inspect_config(
            args.config.read_text(encoding="utf-8"),
            args.template.read_text(encoding="utf-8"),
        )
        print(
            json.dumps(
                {
                    "current": inspection.current,
                    "has_legacy_token_secret": inspection.has_legacy_token_secret,
                }
            )
        )
        return 0
    if args.command == "upgrade":
        backup_path = upgrade_file(args.config, args.template)
        print(f"backup={backup_path}")
        return 0
    parser.error("必须指定 --self-test、inspect 或 upgrade")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
