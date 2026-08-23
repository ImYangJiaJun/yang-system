#!/usr/bin/env python3
"""按 yang-system 约定创建并注册一个函数式 Action。"""

from __future__ import annotations

import argparse
import re
import tempfile
from pathlib import Path


NAME_RE = re.compile(r"^[a-z][a-z0-9_]*$")
METHODS = {"GET", "POST", "PUT", "PATCH", "DELETE"}
REGISTRATION_MARKER = "    // scaffold:action-registration"
MODULE_RE = re.compile(r"^(mod\s+[a-z][a-z0-9_]*;\s*)$", re.MULTILINE)


def pascal_case(value: str) -> str:
    return "".join(part.capitalize() for part in value.split("_"))


def http_method_variant(method: str) -> str:
    return method.capitalize()


def action_source(name: str, title: str) -> str:
    action_type = f"{pascal_case(name)}Action"
    return f'''//! {title} Action。

use schemars::JsonSchema;
use serde::{{Deserialize, Serialize}};
use yang_base::action::ActionContext;
use yang_base::definition::{{ParamInput, Params}};
use yang_base::BaseError;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct {pascal_case(name)}Input {{}}

impl ParamInput for {pascal_case(name)}Input {{
    fn params() -> Params {{
        Params::new()
    }}
}}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct {pascal_case(name)}Output {{
    accepted: bool,
}}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: {pascal_case(name)}Input,
) -> Result<{pascal_case(name)}Output, BaseError> {{
    Err(BaseError::ConfigError(
        "{action_type} 尚未实现".to_string(),
    ))
}}
'''


def insert_module(manifest: str, name: str) -> str:
    declarations = list(MODULE_RE.finditer(manifest))
    line = f"mod {name};\n"
    if not declarations:
        return line + manifest
    position = declarations[-1].end()
    prefix = manifest[:position]
    if not prefix.endswith("\n"):
        prefix += "\n"
    return prefix + line + manifest[position:]


def register_in_route_table(manifest: str, name: str, title: str, method: str, path: str) -> str:
    if REGISTRATION_MARKER not in manifest:
        raise ValueError("mod.rs 缺少 scaffold:action-registration 标记")
    registration = (
        "    let module = module\n"
        f'        .action_fn(action_name("{name}")?, {name}::handle)\n'
        f'        .route(HttpMethod::{http_method_variant(method)}, "{path}")\n'
        f'        .display_name("{title}")\n'
        f'        .description("TODO: 描述 {title} 的业务语义")\n'
        "        .register();\n"
    )
    return manifest.replace(
        REGISTRATION_MARKER,
        registration + REGISTRATION_MARKER,
        1,
    )


def generate(
    actions_dir: Path,
    name: str,
    title: str,
    method: str,
    path: str,
) -> Path:
    if not NAME_RE.fullmatch(name):
        raise ValueError("Action 名必须是 snake_case 小写标识符")
    if method not in METHODS:
        raise ValueError(f"method 必须是 {', '.join(sorted(METHODS))}")
    if not path.startswith("/api/v1/"):
        raise ValueError("业务 Action 路径必须以 /api/v1/ 开头")
    manifest_path = actions_dir / "mod.rs"
    target = actions_dir / f"{name}.rs"
    if not manifest_path.is_file():
        raise ValueError(f"缺少 {manifest_path}")
    if target.exists():
        raise ValueError(f"拒绝覆盖已存在文件 {target}")

    original = manifest_path.read_text(encoding="utf-8")
    updated = insert_module(original, name)
    updated = register_in_route_table(updated, name, title, method, path)

    target.write_text(action_source(name, title), encoding="utf-8")
    manifest_path.write_text(updated, encoding="utf-8")
    return target


def self_test() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        listed = root / "listed"
        listed.mkdir()
        (listed / "mod.rs").write_text(
            "mod list;\nfn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {\n"
            f"{REGISTRATION_MARKER}\n    Ok(module)\n}}\n",
            encoding="utf-8",
        )
        target = generate(listed, "archive", "归档", "POST", "/api/v1/archive")
        manifest = (listed / "mod.rs").read_text(encoding="utf-8")
        source = target.read_text(encoding="utf-8")
        assert target.is_file() and "mod archive;" in manifest
        assert '.action_fn(action_name("archive")?, archive::handle)' in manifest
        assert '.route(HttpMethod::Post, "/api/v1/archive")' in manifest
        assert "pub(super) async fn handle(" in source
        assert "尚未实现" in source
        try:
            generate(listed, "archive", "归档", "POST", "/api/v1/archive")
        except ValueError as error:
            assert "拒绝覆盖" in str(error)
        else:
            raise AssertionError("生成器必须拒绝覆盖")

        no_marker = root / "no_marker"
        no_marker.mkdir()
        (no_marker / "mod.rs").write_text("mod list;\n", encoding="utf-8")
        try:
            generate(no_marker, "disable", "停用", "PATCH", "/api/v1/disable")
        except ValueError as error:
            assert "scaffold:action-registration" in str(error)
        else:
            raise AssertionError("缺少注册标记的 mod.rs 必须被拒绝")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("actions_dir", nargs="?", type=Path)
    parser.add_argument("name", nargs="?")
    parser.add_argument("--title")
    parser.add_argument("--method", default="POST", type=str.upper)
    parser.add_argument("--path")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("action scaffold self-test: passed")
        return 0
    if args.actions_dir is None or args.name is None:
        parser.error("actions_dir 和 name 为必填参数")
    title = args.title or pascal_case(args.name)
    path = args.path or f"/api/v1/{args.name.replace('_', '-')}"
    try:
        target = generate(args.actions_dir.resolve(), args.name, title, args.method, path)
    except ValueError as error:
        parser.error(str(error))
    print(f"created {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
