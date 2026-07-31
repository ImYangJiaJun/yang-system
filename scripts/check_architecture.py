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
    "system-capability",
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
    "system-capability": re.compile(
        r"\.\s*(?:system_tenant|system_table_query|system_tables)\s*\("
    ),
}
TENANT_FORBIDDEN_PATTERNS = {
    "TenantContext::system()": re.compile(r"\bTenantContext\s*::\s*system\s*\("),
    "TenantContext.is_system()": re.compile(r"\.\s*is_system\s*\("),
    "Option<TenantContext>": re.compile(r"\bOption\s*<\s*TenantContext\s*>"),
}
TENANT_BOUNDARY_DOCUMENT = Path("docs/architecture/tenant-data-paths.md")
TENANT_ISOLATION_TEST = Path("tests/tenant_isolation_integration.rs")
TENANT_TEST_RE = re.compile(
    r"#\s*\[\s*tokio::test(?:\([^]]*\))?\s*\]\s*"
    r"#\s*\[\s*ignore\s*=\s*\"[^\"]+\"\s*\]\s*"
    r"async\s+fn\s+([a-z][a-z0-9_]*)\s*\(",
    re.DOTALL,
)
TENANT_TEST_FUNCTION_RE = re.compile(
    r"(?m)^async\s+fn\s+([a-z][a-z0-9_]*)\s*\("
)
TENANT_EVIDENCE_RE = re.compile(
    r"(?m)^\s*//\s*tenant-evidence:\s*([a-z][a-z0-9-]*)\s*$"
)
TENANT_DOC_EVIDENCE_RE = re.compile(
    r"<!--\s*tenant-evidence:\s*([a-z][a-z0-9_]*)\s+"
    r"([a-z][a-z0-9-]*)\s*-->"
)
REQUIRED_TENANT_EVIDENCE = {
    "tenant_crud_and_object_ids_are_isolated_end_to_end": {
        "crud-tenant-injection",
        "crud-own-scope",
        "crud-list-scope",
        "crud-object-id-hidden",
        "crud-cross-mutation-zero",
        "crud-explicit-tenant-rejected",
        "crud-tenant-move-rejected",
        "crud-context-switch-rejected",
        "crud-cross-effects-zero",
    },
    "tenant_join_relation_batch_and_transaction_bypasses_are_closed": {
        "join-user-scope",
        "relation-selected-scope",
        "batch-add-rejected",
        "batch-mutation-rejected",
        "transaction-rollback",
    },
}
TENANT_EVIDENCE_OWNERS = {
    "tenant_crud_and_object_ids_are_isolated_end_to_end": "run_isolation_matrix",
    "tenant_join_relation_batch_and_transaction_bypasses_are_closed": "run_bypass_matrix",
}
AUDIT_FORBIDDEN_MUTATION_RE = re.compile(
    r"(?i)\b(?:"
    r"UPDATE\s+`?audit_event`?"
    r"|DELETE\s+FROM\s+`?audit_event`?"
    r"|TRUNCATE(?:\s+TABLE)?\s+`?audit_event`?"
    r")\b"
)
RAW_SQL_BOUNDARY_KINDS = (
    "domain-repository",
    "domain-service",
    "infrastructure-repository",
    "schema-validator",
)
RAW_SQL_BOUNDARY_KIND_PATTERN = "|".join(
    re.escape(kind) for kind in RAW_SQL_BOUNDARY_KINDS
)
RAW_SQL_CODE_BOUNDARY_RE = re.compile(
    rf"(?m)^//!\s*raw-sql-boundary:\s*({RAW_SQL_BOUNDARY_KIND_PATTERN})\s+"
    r"([a-z][a-z0-9-]*)\s*$"
)
RAW_SQL_DOC_BOUNDARY_RE = re.compile(
    rf"<!--\s*raw-sql-boundary:\s*({RAW_SQL_BOUNDARY_KIND_PATTERN})\s+"
    r"([a-z][a-z0-9-]*)\s+([a-zA-Z0-9_./-]+)\s*-->"
)
RAW_SQL_INVOCATION_RE = re.compile(
    r"\bsqlx::query(?:_as|_scalar)?(?:\s*::\s*<[^\n;]*>)?\s*\("
)
RAW_SQL_BOUNDARY_DOCUMENT = Path("docs/architecture/raw-sql-boundaries.md")


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
    tenant_roots = [
        root / "src" / "modules" / name
        for name in ("org", "work")
        if (root / "src" / "modules" / name).is_dir()
    ]
    if not tenant_roots:
        return set(), []

    errors: list[str] = []
    declared: set[tuple[str, str]] = set()
    used: set[tuple[str, str]] = set()
    owners: dict[str, tuple[str, Path]] = {}
    paths = sorted(path for tenant_root in tenant_roots for path in tenant_root.rglob("*.rs"))
    for path in paths:
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

        for legacy, pattern in TENANT_FORBIDDEN_PATTERNS.items():
            for match in pattern.finditer(source):
                line_number = source.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{relative}:{line_number}: 禁止旧租户绕过表达 {legacy}，"
                    "repository 必须使用非可选普通租户或显式系统 capability"
                )

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


def check_tenant_isolation_evidence(root: Path) -> list[str]:
    """锁定真实库租户矩阵的测试入口、证据点和文档映射。"""

    if not (root / "src" / "modules" / "org").is_dir():
        return []

    errors: list[str] = []
    test_path = root / TENANT_ISOLATION_TEST
    if not test_path.is_file():
        return [f"{TENANT_ISOLATION_TEST}: 缺少真实库租户隔离矩阵"]
    source = test_path.read_text(encoding="utf-8")
    declared_tests = set(TENANT_TEST_RE.findall(source))
    required_tests = set(REQUIRED_TENANT_EVIDENCE)
    for name in sorted(required_tests - declared_tests):
        errors.append(
            f"{TENANT_ISOLATION_TEST}: {name} 必须是 #[ignore] 的 Tokio 真实库测试"
        )
    for name in sorted(declared_tests - required_tests):
        errors.append(f"{TENANT_ISOLATION_TEST}: 未登记的租户隔离测试 {name}")

    ordered_functions = list(TENANT_TEST_FUNCTION_RE.finditer(source))
    function_matches = {match.group(1): match for match in ordered_functions}
    owned_evidence: set[tuple[str, str]] = set()
    for name, required_evidence in REQUIRED_TENANT_EVIDENCE.items():
        owner = TENANT_EVIDENCE_OWNERS[name]
        match = function_matches.get(owner)
        if match is None:
            errors.append(f"{TENANT_ISOLATION_TEST}: {name} 缺少证据函数 {owner}")
            continue
        end = next(
            (
                candidate.start()
                for candidate in ordered_functions
                if candidate.start() > match.start()
            ),
            len(source),
        )
        evidence_entries = TENANT_EVIDENCE_RE.findall(source[match.start() : end])
        actual_evidence = set(evidence_entries)
        for evidence_id in sorted(
            {value for value in evidence_entries if evidence_entries.count(value) > 1}
        ):
            errors.append(
                f"{TENANT_ISOLATION_TEST}: {name} 的证据 {evidence_id} 重复"
            )
        for evidence_id in sorted(required_evidence - actual_evidence):
            errors.append(f"{TENANT_ISOLATION_TEST}: {name} 缺少证据 {evidence_id}")
        for evidence_id in sorted(actual_evidence - required_evidence):
            errors.append(
                f"{TENANT_ISOLATION_TEST}: {name} 包含未登记证据 {evidence_id}"
            )
        owned_evidence.update((name, evidence_id) for evidence_id in actual_evidence)

        test_match = function_matches.get(name)
        if test_match is not None:
            test_end = next(
                (
                    candidate.start()
                    for candidate in ordered_functions
                    if candidate.start() > test_match.start()
                ),
                len(source),
            )
            if not re.search(rf"\b{re.escape(owner)}\s*\(", source[test_match.start() : test_end]):
                errors.append(
                    f"{TENANT_ISOLATION_TEST}: {name} 未执行证据函数 {owner}"
                )

    all_evidence = TENANT_EVIDENCE_RE.findall(source)
    for evidence_id in sorted(
        {value for value in all_evidence if all_evidence.count(value) > 1}
    ):
        errors.append(f"{TENANT_ISOLATION_TEST}: tenant evidence {evidence_id} 重复")
    owned_ids = {evidence_id for _, evidence_id in owned_evidence}
    for evidence_id in sorted(set(all_evidence) - owned_ids):
        errors.append(f"{TENANT_ISOLATION_TEST}: 测试函数外存在孤儿证据 {evidence_id}")

    document = root / TENANT_BOUNDARY_DOCUMENT
    if not document.is_file():
        return errors
    documented_entries = TENANT_DOC_EVIDENCE_RE.findall(
        document.read_text(encoding="utf-8")
    )
    documented = set(documented_entries)
    required_documented = {
        (name, evidence_id)
        for name, evidence_ids in REQUIRED_TENANT_EVIDENCE.items()
        for evidence_id in evidence_ids
    }
    for entry in sorted(
        {value for value in documented_entries if documented_entries.count(value) > 1}
    ):
        errors.append(f"{TENANT_BOUNDARY_DOCUMENT}: tenant evidence {entry[1]} 重复")
    for name, evidence_id in sorted(required_documented - documented):
        errors.append(
            f"{TENANT_BOUNDARY_DOCUMENT}: 缺少测试证据 {name} {evidence_id}"
        )
    for name, evidence_id in sorted(documented - required_documented):
        errors.append(
            f"{TENANT_BOUNDARY_DOCUMENT}: 已记录不存在的测试证据 {name} {evidence_id}"
        )
    return errors


def check_audit_append_only(root: Path) -> list[str]:
    """在线 Rust 代码不得获得修改或销毁审计事实的路径。"""

    errors: list[str] = []
    source_root = root / "src"
    if not source_root.is_dir():
        return errors
    for path in sorted(source_root.rglob("*.rs")):
        source = production_source(path.read_text(encoding="utf-8"))
        for match in AUDIT_FORBIDDEN_MUTATION_RE.finditer(source):
            line_number = source.count("\n", 0, match.start()) + 1
            errors.append(
                f"{path.relative_to(root)}:{line_number}: audit_event 只允许追加；"
                "在线代码禁止 UPDATE/DELETE/TRUNCATE"
            )
    return errors


def raw_sql_boundary_path_allowed(kind: str, relative: Path) -> bool:
    """只允许领域 repository/service 与两个显式基础设施边界持有原始 SQL。"""

    value = relative.as_posix()
    if kind == "domain-repository":
        return (
            value.startswith("src/modules/")
            and relative.name == "repository.rs"
        )
    if kind == "domain-service":
        return (
            value.startswith("src/modules/")
            and relative.stem in {"authz_version", "grants", "guard", "service"}
        )
    if kind == "infrastructure-repository":
        return value in {
            "src/audit/repository.rs",
            "src/authorization/outbox.rs",
        }
    if kind == "schema-validator":
        return value in {"src/audit/schema.rs", "src/migrations.rs"}
    return False


def raw_sql_argument_is_literal(source: str, invocation_end: int) -> bool:
    """生产查询文本必须直接来自字符串字面量，禁止请求值参与 SQL 结构。"""

    argument = source[invocation_end:]
    return re.match(r'\s*(?:"|r#+")', argument) is not None


def raw_sql_code_boundaries(
    root: Path,
) -> tuple[set[tuple[str, str, str]], list[str]]:
    source_root = root / "src"
    if not source_root.is_dir():
        return set(), []

    errors: list[str] = []
    boundaries: set[tuple[str, str, str]] = set()
    owners: dict[str, Path] = {}
    for path in sorted(source_root.rglob("*.rs")):
        source = production_source(path.read_text(encoding="utf-8"))
        relative = path.relative_to(root)
        matches = list(RAW_SQL_INVOCATION_RE.finditer(source))
        declarations = RAW_SQL_CODE_BOUNDARY_RE.findall(source)

        if not matches:
            if declarations:
                errors.append(
                    f"{relative}: 声明了 raw SQL 边界但生产代码没有 sqlx 查询"
                )
            continue
        if len(declarations) != 1:
            errors.append(
                f"{relative}: 含 {len(matches)} 条生产 sqlx 查询，必须恰好声明一个 "
                "`//! raw-sql-boundary: <kind> <id>`"
            )
            continue

        kind, boundary_id = declarations[0]
        if not raw_sql_boundary_path_allowed(kind, relative):
            errors.append(
                f"{relative}: raw SQL 边界类型 {kind} 不允许出现在该路径"
            )
        previous = owners.get(boundary_id)
        if previous is not None:
            errors.append(
                f"{relative}: raw SQL boundary {boundary_id} 与 {previous} 重复"
            )
        owners[boundary_id] = relative
        boundary = (kind, boundary_id, relative.as_posix())
        boundaries.add(boundary)

        for match in matches:
            if raw_sql_argument_is_literal(source, match.end()):
                continue
            line_number = source.count("\n", 0, match.start()) + 1
            errors.append(
                f"{relative}:{line_number}: 生产 sqlx 查询必须直接使用静态字符串字面量；"
                "参数必须通过 bind 传入，动态标识符不得来自请求"
            )
    return boundaries, errors


def check_raw_sql_boundaries(root: Path) -> list[str]:
    code_boundaries, errors = raw_sql_code_boundaries(root)
    source_root = root / "src"
    if not source_root.is_dir():
        return errors

    document = root / RAW_SQL_BOUNDARY_DOCUMENT
    if not document.is_file():
        if not code_boundaries and not errors:
            return []
        return [*errors, f"{RAW_SQL_BOUNDARY_DOCUMENT}: 缺少 raw SQL 边界清单"]
    documented_entries = RAW_SQL_DOC_BOUNDARY_RE.findall(
        document.read_text(encoding="utf-8")
    )
    documented = set(documented_entries)
    documented_ids = [boundary_id for _, boundary_id, _ in documented_entries]
    for boundary_id in sorted(
        {value for value in documented_ids if documented_ids.count(value) > 1}
    ):
        errors.append(
            f"{RAW_SQL_BOUNDARY_DOCUMENT}: raw SQL boundary {boundary_id} 重复"
        )
    for kind, boundary_id, path in sorted(code_boundaries - documented):
        errors.append(
            f"{RAW_SQL_BOUNDARY_DOCUMENT}: 缺少代码边界 {kind} {boundary_id} {path}"
        )
    for kind, boundary_id, path in sorted(documented - code_boundaries):
        errors.append(
            f"{RAW_SQL_BOUNDARY_DOCUMENT}: 已记录不存在的代码边界 "
            f"{kind} {boundary_id} {path}"
        )
    return errors


def check_frontend_boundaries(root: Path) -> list[str]:
    frontend = root / "frontend" / "src"
    if not frontend.is_dir():
        return []

    errors: list[str] = []
    app = frontend / "App.vue"
    app_source = app.read_text(encoding="utf-8") if app.is_file() else ""
    if app_source.count("startApplication(") != 1:
        errors.append("frontend/src/App.vue: 应由应用根节点唯一启动全局生命周期")
    application_start = frontend / "application" / "startApplication.ts"
    application_start_source = (
        application_start.read_text(encoding="utf-8")
        if application_start.is_file()
        else ""
    )
    if application_start_source.count("useApplicationLifecycleStore().start()") != 1:
        errors.append(
            "frontend/src/application/startApplication.ts: "
            "应用启动器必须且只能启动一次全局生命周期"
        )

    for directory in ("layouts", "pages"):
        for source in sorted((frontend / directory).glob("*.vue")):
            if ".start()" in source.read_text(encoding="utf-8"):
                errors.append(
                    f"{source.relative_to(root).as_posix()}: 页面或布局不得启动全局生命周期"
                )

    table_view = frontend / "components" / "table" / "TableView.vue"
    if table_view.is_file():
        source = table_view.read_text(encoding="utf-8")
        if len(source.splitlines()) > 400:
            errors.append(
                "frontend/src/components/table/TableView.vue: 顶层编排组件不得超过 400 行"
            )
        required_composables = {
            "useTableQuery",
            "useRelationOptions",
            "useTableSelection",
            "useTableActions",
            "useColumnPreferences",
        }
        for composable in sorted(required_composables):
            if f"{composable}(" not in source:
                errors.append(
                    "frontend/src/components/table/TableView.vue: "
                    f"缺少行为边界 {composable}"
                )
        for forbidden in ("invokeAction(", "new AbortController("):
            if forbidden in source:
                errors.append(
                    "frontend/src/components/table/TableView.vue: "
                    f"顶层组件不得直接拥有异步状态机 {forbidden}"
                )

    routes = frontend / "router" / "routes.ts"
    routes_source = routes.read_text(encoding="utf-8") if routes.is_file() else ""
    if (
        'path: "/workbench"' in routes_source
        and "import.meta.env.DEV" not in routes_source
    ):
        errors.append(
            "frontend/src/router/routes.ts: Workbench 必须由开发构建条件门控"
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
    errors.extend(check_tenant_isolation_evidence(root))
    errors.extend(check_audit_append_only(root))
    errors.extend(check_raw_sql_boundaries(root))
    errors.extend(check_frontend_boundaries(root))
    return errors


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def self_test() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write(
            root / "frontend/src/App.vue",
            "<script setup>\nstartApplication(router)\n</script>\n",
        )
        write(
            root / "frontend/src/application/startApplication.ts",
            "useApplicationLifecycleStore().start()\n",
        )
        write(root / "frontend/src/layouts/MainLayout.vue", "<template />\n")
        write(
            root / "frontend/src/components/table/TableView.vue",
            "<script setup>\n"
            "useTableQuery(); useRelationOptions(); useTableSelection();\n"
            "useTableActions(); useColumnPreferences();\n"
            "</script>\n",
        )
        write(
            root / "frontend/src/router/routes.ts",
            'const workbench = import.meta.env.DEV ? [{ path: "/workbench" }] : [];\n',
        )
        assert check_frontend_boundaries(root) == [], (
            "合法前端生命周期和 Table 编排边界应通过"
        )
        write(
            root / "frontend/src/layouts/MainLayout.vue",
            "<script setup>store.start()</script>\n",
        )
        errors = check_frontend_boundaries(root)
        assert any("不得启动全局生命周期" in error for error in errors), (
            "必须拒绝布局重复启动全局生命周期"
        )

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
        write(
            root / "src" / "audit_mutation.rs",
            'fn mutate() { sqlx::query("DELETE FROM `audit_event`"); }\n',
        )
        errors = check_audit_append_only(root)
        assert any("audit_event 只允许追加" in error for error in errors), (
            "必须拒绝在线代码修改或销毁审计事实"
        )
        write(
            root / "src" / "audit_insert.rs",
            'fn append() { sqlx::query("INSERT INTO `audit_event` (`id`) VALUES (?)"); }\n',
        )
        (root / "src" / "audit_mutation.rs").unlink()
        assert check_audit_append_only(root) == [], "审计 INSERT 必须允许"

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        repository = root / "src" / "modules" / "demo" / "repository.rs"
        write(
            repository,
            "//! raw-sql-boundary: domain-repository demo-read\n"
            "async fn load(pool: &sqlx::MySqlPool) {\n"
            '    sqlx::query("SELECT value FROM demo WHERE id = ?")\n'
            "        .bind(1_i64).fetch_one(pool).await;\n"
            "}\n",
        )
        write(
            root / RAW_SQL_BOUNDARY_DOCUMENT,
            "<!-- raw-sql-boundary: domain-repository demo-read "
            "src/modules/demo/repository.rs -->\n",
        )
        assert check_raw_sql_boundaries(root) == [], "已登记静态查询边界应通过"

        write(
            repository,
            "//! raw-sql-boundary: domain-repository demo-read\n"
            "async fn load(pool: &sqlx::MySqlPool, sql: &str) {\n"
            "    sqlx::query(sql).fetch_one(pool).await;\n"
            "}\n",
        )
        errors = check_raw_sql_boundaries(root)
        assert any("静态字符串字面量" in error for error in errors), (
            "必须拒绝运行时拼接或传入的 SQL"
        )

        write(
            repository,
            "async fn load(pool: &sqlx::MySqlPool) {\n"
            '    sqlx::query("SELECT 1").fetch_one(pool).await;\n'
            "}\n",
        )
        errors = check_raw_sql_boundaries(root)
        assert any("必须恰好声明一个" in error for error in errors), (
            "必须拒绝未登记的生产 SQL"
        )

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
            repository,
            "fn system_access(ctx: &ActionContext) {\n"
            "    let _ = ctx.system_tenant();\n"
            "}\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("system-capability 租户旁路缺少" in error for error in errors), (
            "必须拒绝未声明的系统租户 capability"
        )

        write(
            repository,
            "fn system_access(ctx: &ActionContext) {\n"
            "    // tenant-boundary: system-capability demo-system-access\n"
            "    let _ = ctx.system_tenant();\n"
            "}\n",
        )
        write(
            root / TENANT_BOUNDARY_DOCUMENT,
            "<!-- tenant-boundary: system-capability demo-system-access -->\n",
        )
        assert check_tenant_boundaries(root) == [], "完整系统 capability 声明应通过"

        write(
            repository,
            "fn legacy_access() {\n"
            "    let _ = TenantContext::system();\n"
            "}\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("禁止旧租户绕过表达" in error for error in errors), (
            "必须拒绝旧 Option + bool 系统绕过模型"
        )

        write(
            repository,
            "fn system_access(ctx: &ActionContext) {\n"
            "    // tenant-boundary: system-capability demo-system-access\n"
            "    let _ = ctx.system_tenant();\n"
            "}\n",
        )
        write(
            root / TENANT_BOUNDARY_DOCUMENT,
            "<!-- tenant-boundary: system-capability demo-system-access -->\n"
            "<!-- tenant-boundary: transaction removed-boundary -->\n",
        )
        errors = check_tenant_boundaries(root)
        assert any("已记录不存在" in error for error in errors), (
            "必须拒绝清单中的孤儿旁路"
        )

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        (root / "src" / "modules" / "org").mkdir(parents=True)
        test_source = ""
        document_source = ""
        for test_name, evidence_ids in REQUIRED_TENANT_EVIDENCE.items():
            owner = TENANT_EVIDENCE_OWNERS[test_name]
            test_source += f"async fn {owner}() {{\n"
            for evidence_id in sorted(evidence_ids):
                test_source += f"    // tenant-evidence: {evidence_id}\n"
                document_source += (
                    f"<!-- tenant-evidence: {test_name} {evidence_id} -->\n"
                )
            test_source += "}\n\n"
            test_source += (
                '#[tokio::test(flavor = "current_thread")]\n'
                '#[ignore = "需要真实数据库"]\n'
                f"async fn {test_name}() {{\n"
                f"    {owner}().await;\n"
            )
            test_source += "}\n\n"
        write(root / TENANT_ISOLATION_TEST, test_source)
        write(root / TENANT_BOUNDARY_DOCUMENT, document_source)
        assert check_tenant_isolation_evidence(root) == [], (
            "完整租户隔离证据契约应通过"
        )

        missing_test, missing_evidence = next(iter(REQUIRED_TENANT_EVIDENCE.items()))
        missing_id = sorted(missing_evidence)[0]
        write(
            root / TENANT_ISOLATION_TEST,
            test_source.replace(
                f"    // tenant-evidence: {missing_id}\n",
                "",
                1,
            ),
        )
        errors = check_tenant_isolation_evidence(root)
        assert any("缺少证据" in error and missing_id in error for error in errors), (
            "必须拒绝缺失的真实库证据"
        )

        write(
            root / TENANT_ISOLATION_TEST,
            test_source + "// tenant-evidence: orphan-proof\n",
        )
        errors = check_tenant_isolation_evidence(root)
        assert any("孤儿证据" in error for error in errors), "必须拒绝函数外证据"

        write(root / TENANT_ISOLATION_TEST, test_source)
        write(
            root / TENANT_BOUNDARY_DOCUMENT,
            document_source.replace(
                f"<!-- tenant-evidence: {missing_test} {missing_id} -->\n",
                "",
                1,
            ),
        )
        errors = check_tenant_isolation_evidence(root)
        assert any(
            "缺少测试证据" in error and missing_id in error for error in errors
        ), "必须拒绝缺失的文档证据映射"

        write(root / TENANT_BOUNDARY_DOCUMENT, document_source)
        write(
            root / TENANT_ISOLATION_TEST,
            test_source.replace('#[ignore = "需要真实数据库"]\n', "", 1),
        )
        errors = check_tenant_isolation_evidence(root)
        assert any("必须是 #[ignore]" in error for error in errors), (
            "必须拒绝失去真实库属性的矩阵测试"
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
