#!/usr/bin/env python3
"""检查 yang-system 可机械验证的架构边界。"""

from __future__ import annotations

import argparse
import re
import tempfile
from pathlib import Path


DERIVE_RE = re.compile(r"#\s*\[\s*derive\s*\((.*?)\)\s*\]", re.DOTALL)
ACTION_SPEC_RE = re.compile(r"\bActionSpec\s*::\s*new\s*\(")
MODULE_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z][a-z0-9_]*)\s*;", re.MULTILINE)


def derived_action_count(source: str) -> int:
    count = 0
    for attributes in DERIVE_RE.findall(source):
        derives = [item.strip().split("::")[-1] for item in attributes.split(",")]
        count += derives.count("Action")
    return count


def action_definition_count(source: str) -> int:
    return derived_action_count(source) + len(ACTION_SPEC_RE.findall(source))


def action_directories(root: Path) -> list[Path]:
    directories: set[Path] = set()
    for search_root in (root / "src" / "modules", root / "examples"):
        if search_root.is_dir():
            directories.update(
                path for path in search_root.rglob("actions") if path.is_dir()
            )
    return sorted(directories)


def check_action_directory(root: Path, directory: Path) -> list[str]:
    errors: list[str] = []
    relative = directory.relative_to(root)
    manifest = directory / "mod.rs"
    if not manifest.is_file():
        return [f"{relative}: 缺少 actions/mod.rs 清单"]

    manifest_source = manifest.read_text(encoding="utf-8")
    declared = set(MODULE_RE.findall(manifest_source))
    registrations = MODULE_RE.sub("", manifest_source)
    files = {path.stem: path for path in directory.glob("*.rs") if path.name != "mod.rs"}

    for name, path in sorted(files.items()):
        count = action_definition_count(path.read_text(encoding="utf-8"))
        if count != 1:
            errors.append(
                f"{path.relative_to(root)}: 必须恰好定义一个 Action，实际 {count} 个"
            )
        if name not in declared:
            errors.append(f"{path.relative_to(root)}: 未在 {relative / 'mod.rs'} 中声明")
        elif not re.search(rf"\b(?:use\s+)?{re.escape(name)}\s*::", registrations):
            errors.append(f"{path.relative_to(root)}: 未在 {relative / 'mod.rs'} 中注册")

    for name in sorted(declared - files.keys()):
        errors.append(f"{relative / 'mod.rs'}: mod {name}; 缺少同名 {name}.rs")
    return errors


def check_actions_outside_directories(root: Path) -> list[str]:
    errors: list[str] = []
    modules = root / "src" / "modules"
    if not modules.is_dir():
        return errors
    for path in modules.rglob("*.rs"):
        if "actions" in path.parts:
            continue
        source = path.read_text(encoding="utf-8")
        if derived_action_count(source):
            errors.append(
                f"{path.relative_to(root)}: Action 必须移动到 actions/<action>.rs"
            )
    return errors


def check(root: Path) -> list[str]:
    errors: list[str] = []
    directories = action_directories(root)
    if not directories:
        return ["未找到任何 actions/ 目录，检查路径是否为 yang-system 根目录"]
    for directory in directories:
        errors.extend(check_action_directory(root, directory))
    errors.extend(check_actions_outside_directories(root))
    return errors


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def self_test() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        actions = root / "src" / "modules" / "demo" / "actions"
        write(actions / "mod.rs", "mod list;\n")
        write(actions / "list.rs", "#[derive(Action)]\nstruct ListAction;\n")
        errors = check(root)
        assert any("未在" in error and "注册" in error for error in errors), (
            "必须拒绝只声明未注册的 Action"
        )
        write(actions / "mod.rs", "mod list;\nuse list::ListAction;\n")
        assert check(root) == [], "合法 fixture 应通过"

        write(
            actions / "list.rs",
            "#[derive(Action)]\nstruct A;\n#[derive(Action)]\nstruct B;\n",
        )
        errors = check(root)
        assert any("实际 2 个" in error for error in errors), "必须拒绝多 Action 文件"

        write(actions / "support.rs", "fn helper() {}\n")
        errors = check(root)
        assert any("实际 0 个" in error for error in errors), "必须拒绝非 Action 文件"
        assert any("未在" in error for error in errors), "必须拒绝未登记文件"

        write(
            root / "src" / "modules" / "demo" / "leaked.rs",
            "#[derive(Action)]\nstruct LeakedAction;\n",
        )
        errors = check(root)
        assert any("必须移动到" in error for error in errors), "必须拒绝目录外 Action"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root",
        nargs="?",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="yang-system 根目录（默认自动定位）",
    )
    parser.add_argument("--self-test", action="store_true", help="运行检查器反向测试")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        print("architecture checker self-test: passed")
        return 0

    root = args.root.resolve()
    errors = check(root)
    if errors:
        print("architecture check: failed")
        for error in errors:
            print(f"- {error}")
        return 1
    print("architecture check: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
