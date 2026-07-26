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
TENANT_BOUNDARY_KINDS = (
    "database",
    "raw-sql",
    "unscoped-query",
    "transaction",
    "relation",
    "batch",
    "background",
)
TENANT_BOUNDARY_KIND_PATTERN = "|".join(re.escape(kind) for kind in TENANT_BOUNDARY_KINDS)
TENANT_CODE_BOUNDARY_RE = re.compile(
    rf"^\s*//\s*tenant-boundary:\s*({TENANT_BOUNDARY_KIND_PATTERN})\s+"
    r"([a-z][a-z0-9-]*)\s*$"
)
TENANT_DOC_BOUNDARY_RE = re.compile(
    rf"<!--\s*tenant-boundary:\s*({TENANT_BOUNDARY_KIND_PATTERN})\s+"
    r"([a-z][a-z0-9-]*)\s*-->"
)
TENANT_RISK_PATTERNS = {
    "database": re.compile(
        r"\.\s*tools\s*\(\s*\)\s*\.\s*(?:optional_)?mysql\s*\("
    ),
    "raw-sql": re.compile(
        r"\bsqlx::query(?:_as|_scalar)?(?:\s*::\s*<[^\n;]*>)?\s*\("
    ),
    "unscoped-query": re.compile(r"\.\s*query\s*\(\s*std::iter::empty"),
    "transaction": re.compile(r"\.(?:begin_transaction|begin)\s*\("),
    "relation": re.compile(r"\bRelationLoader\s*::\s*new\s*\(|\.relations\s*\("),
    "batch": re.compile(
        r"\.(?:(?:insert|update|delete)[_-]?(?:many|batch|bulk)|"
        r"(?:batch|bulk)[_-]?(?:insert|update|delete))\s*\("
    ),
    "background": re.compile(
        r"\b(?:tokio::(?:task::)?spawn|spawn_blocking|JoinSet::spawn)\s*\("
    ),
}
TENANT_BOUNDARY_DOCUMENT = Path("docs/architecture/tenant-data-paths.md")


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


def production_source(source: str) -> str:
    """排除文件尾部单元测试，避免测试并发工具被识别为生产后台任务。"""

    marker = re.search(r"(?m)^\s*#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", source)
    return source[: marker.start()] if marker else source


def preceding_tenant_boundary(
    lines: list[str], line_number: int
) -> tuple[str, str] | None:
    """风险调用必须由紧邻的单行声明解释；不允许文件级宽泛豁免。"""

    for index in range(line_number - 2, max(-1, line_number - 5), -1):
        stripped = lines[index].strip()
        if not stripped:
            continue
        match = TENANT_CODE_BOUNDARY_RE.fullmatch(lines[index])
        if match:
            return match.group(1), match.group(2)
        if stripped.startswith("//"):
            continue
        break
    return None


def tenant_code_boundaries(root: Path) -> tuple[set[tuple[str, str]], list[str]]:
    org_root = root / "src" / "modules" / "org"
    if not org_root.is_dir():
        return set(), []

    errors: list[str] = []
    declared: set[tuple[str, str]] = set()
    used: set[tuple[str, str]] = set()
    owners: dict[str, tuple[str, Path]] = {}
    for path in sorted(org_root.rglob("*.rs")):
        source = production_source(path.read_text(encoding="utf-8"))
        lines = source.splitlines()
        relative = path.relative_to(root)
        for line_number, line in enumerate(lines, start=1):
            declaration = TENANT_CODE_BOUNDARY_RE.fullmatch(line)
            if declaration is None:
                continue
            boundary = (declaration.group(1), declaration.group(2))
            previous = owners.get(boundary[1])
            if previous is not None:
                errors.append(
                    f"{relative}:{line_number}: tenant boundary {boundary[1]} "
                    f"与 {previous[1].relative_to(root)} 的 {previous[0]} 声明重复"
                )
            owners[boundary[1]] = (boundary[0], path)
            declared.add(boundary)

        for kind, pattern in TENANT_RISK_PATTERNS.items():
            for match in pattern.finditer(source):
                line_number = source.count("\n", 0, match.start()) + 1
                boundary = preceding_tenant_boundary(lines, line_number)
                if boundary is None:
                    errors.append(
                        f"{relative}:{line_number}: {kind} 租户旁路缺少紧邻的 "
                        f"`// tenant-boundary: {kind} <id>` 声明"
                    )
                    continue
                if boundary[0] != kind:
                    errors.append(
                        f"{relative}:{line_number}: 风险类型是 {kind}，"
                        f"声明却是 {boundary[0]}"
                    )
                    continue
                used.add(boundary)

    for kind, boundary_id in sorted(declared - used):
        path = owners[boundary_id][1].relative_to(root)
        errors.append(f"{path}: tenant boundary {boundary_id} 未绑定 {kind} 风险调用")
    return declared, errors


def check_tenant_boundaries(root: Path) -> list[str]:
    code_boundaries, errors = tenant_code_boundaries(root)
    org_root = root / "src" / "modules" / "org"
    if not org_root.is_dir():
        return errors

    document = root / TENANT_BOUNDARY_DOCUMENT
    if not document.is_file():
        return [
            *errors,
            f"{TENANT_BOUNDARY_DOCUMENT}: 缺少租户数据路径清单",
        ]
    documented_entries = TENANT_DOC_BOUNDARY_RE.findall(
        document.read_text(encoding="utf-8")
    )
    documented = set(documented_entries)
    documented_ids = [boundary_id for _, boundary_id in documented_entries]
    for boundary_id in sorted(
        {value for value in documented_ids if documented_ids.count(value) > 1}
    ):
        errors.append(
            f"{TENANT_BOUNDARY_DOCUMENT}: tenant boundary {boundary_id} 重复"
        )
    for kind, boundary_id in sorted(code_boundaries - documented):
        errors.append(
            f"{TENANT_BOUNDARY_DOCUMENT}: 缺少代码旁路 {kind} {boundary_id}"
        )
    for kind, boundary_id in sorted(documented - code_boundaries):
        errors.append(
            f"{TENANT_BOUNDARY_DOCUMENT}: 已记录不存在的代码旁路 {kind} {boundary_id}"
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
    errors.extend(check_tenant_boundaries(root))
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

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        repository = root / "src" / "modules" / "org" / "demo" / "repository.rs"
        write(
            repository,
            "async fn load(pool: &sqlx::MySqlPool) {\n"
            '    sqlx::query("SELECT 1").fetch_one(pool).await;\n'
            "}\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("raw-sql 租户旁路缺少" in error for error in errors), (
            "必须拒绝未声明的租户 raw SQL"
        )

        write(
            repository,
            "async fn load(pool: &sqlx::MySqlPool) {\n"
            "    // tenant-boundary: raw-sql demo-lookup\n"
            '    sqlx::query("SELECT 1").fetch_one(pool).await;\n'
            "}\n",
        )
        write(
            root / TENANT_BOUNDARY_DOCUMENT,
            "<!-- tenant-boundary: raw-sql demo-lookup -->\n",
        )
        assert check_tenant_boundaries(root) == [], "完整租户旁路声明应通过"

        write(
            repository,
            "async fn load(pool: &sqlx::MySqlPool) {\n"
            "    // tenant-boundary: raw-sql demo-lookup\n"
            '    sqlx::query("SELECT 1").fetch_one(pool).await;\n'
            "    tokio::spawn(async {});\n"
            "}\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("background 租户旁路缺少" in error for error in errors), (
            "必须拒绝未声明的租户后台任务"
        )

        write(
            repository,
            "fn direct_database(ctx: &ActionContext) {\n"
            "    let _ = ctx.tools().mysql();\n"
            "}\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("database 租户旁路缺少" in error for error in errors), (
            "必须拒绝未声明的无范围数据库 capability"
        )

        write(
            root / TENANT_BOUNDARY_DOCUMENT,
            "<!-- tenant-boundary: raw-sql demo-lookup -->\n"
            "<!-- tenant-boundary: transaction removed-boundary -->\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("已记录不存在" in error for error in errors), (
            "必须拒绝清单中的孤儿旁路"
        )


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
