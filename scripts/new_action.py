#!/usr/bin/env python3
"""按 yang-system 约定创建并注册一个强类型 Action。"""

from __future__ import annotations

import argparse
import re
import tempfile
from pathlib import Path


NAME_RE = re.compile(r"^[a-z][a-z0-9_]*$")
METHODS = {"GET", "POST", "PUT", "PATCH", "DELETE"}
REGISTRATION_MARKER = "    // scaffold:action-registration"
MODULE_RE = re.compile(r"^(mod\s+[a-z][a-z0-9_]*;\s*)$", re.MULTILINE)
ACTIONS_MACRO_RE = re.compile(r"yang_base::actions!\[(?P<body>[^\]]*)\]")


def pascal_case(value: str) -> str:
    return "".join(part.capitalize() for part in value.split("_"))


def action_source(
    name: str,
    title: str,
    method: str,
    path: str,
    chained_registration: bool,
) -> str:
    action_type = f"{pascal_case(name)}Action"
    register = (
        "\n"
        "pub(super) fn register(module: ModuleSpec) -> ModuleSpec {\n"
        f"    module.native_action({action_type})\n"
        "}\n"
        if chained_registration
        else ""
    )
    module_import = ", ModuleSpec" if chained_registration else ""
    return f'''//! {title} Action。

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{{Deserialize, Serialize}};
use yang_base::action::{{Action as ActionHandler, ActionContext}};
use yang_base::definition::{{ParamInput, Params{module_import}}};
use yang_base::{{Action, BaseError}};

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct {pascal_case(name)}Input {{}}

impl ParamInput for {pascal_case(name)}Input {{
    fn params() -> Params {{
        Params::new()
    }}
}}

#[derive(Debug, Serialize, JsonSchema)]
struct {pascal_case(name)}Output {{
    accepted: bool,
}}

#[derive(Action)]
#[action(
    name = "{name}",
    display_name = "{title}",
    description = "TODO: 描述 {title} 的业务语义",
    method = "{method}",
    path = "{path}"
)]
pub(super) struct {action_type};

#[async_trait]
impl ActionHandler for {action_type} {{
    type Input = {pascal_case(name)}Input;
    type Output = {pascal_case(name)}Output;

    async fn index(
        &self,
        _ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {{
        Err(BaseError::ConfigError(
            "{action_type} 尚未实现".to_string(),
        ))
    }}
}}
{register}'''


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


def register_in_actions_macro(manifest: str, name: str, action_type: str) -> str:
    match = ACTIONS_MACRO_RE.search(manifest)
    if not match:
        raise ValueError("mod.rs 既没有 actions! 清单，也没有 scaffold 注册标记")
    use_line = f"use {name}::{action_type};\n"
    first_yang_use = manifest.find("use yang_base::")
    if first_yang_use < 0:
        raise ValueError("actions! 风格 mod.rs 缺少 yang_base import")
    manifest = manifest[:first_yang_use] + use_line + manifest[first_yang_use:]
    match = ACTIONS_MACRO_RE.search(manifest)
    if not match:
        raise ValueError("无法重新定位 actions! 清单")
    values = [value.strip() for value in match.group("body").split(",") if value.strip()]
    values.append(action_type)
    replacement = f"yang_base::actions![{', '.join(values)}]"
    return manifest[: match.start()] + replacement + manifest[match.end() :]


def register_in_chain(manifest: str, name: str) -> str:
    if REGISTRATION_MARKER not in manifest:
        raise ValueError("register_all 风格 mod.rs 缺少 scaffold:action-registration 标记")
    registration = f"    let module = {name}::register(module);\n"
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
    action_type = f"{pascal_case(name)}Action"
    chained = REGISTRATION_MARKER in original
    updated = insert_module(original, name)
    if chained:
        updated = register_in_chain(updated, name)
    else:
        updated = register_in_actions_macro(updated, name, action_type)

    target.write_text(
        action_source(name, title, method, path, chained),
        encoding="utf-8",
    )
    manifest_path.write_text(updated, encoding="utf-8")
    return target


def self_test() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        listed = root / "listed"
        listed.mkdir()
        (listed / "mod.rs").write_text(
            "mod list;\n\nuse list::ListAction;\nuse yang_base::definition::Actions;\n"
            "fn all() -> Actions { yang_base::actions![ListAction] }\n",
            encoding="utf-8",
        )
        target = generate(listed, "archive", "归档", "POST", "/api/v1/archive")
        manifest = (listed / "mod.rs").read_text(encoding="utf-8")
        assert target.is_file() and "use archive::ArchiveAction;" in manifest
        assert "actions![ListAction, ArchiveAction]" in manifest

        chained = root / "chained"
        chained.mkdir()
        (chained / "mod.rs").write_text(
            "mod list;\nfn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {\n"
            f"{REGISTRATION_MARKER}\n    Ok(module)\n}}\n",
            encoding="utf-8",
        )
        generate(chained, "disable", "停用", "PATCH", "/api/v1/disable")
        manifest = (chained / "mod.rs").read_text(encoding="utf-8")
        assert "let module = disable::register(module);" in manifest
        try:
            generate(chained, "disable", "停用", "PATCH", "/api/v1/disable")
        except ValueError as error:
            assert "拒绝覆盖" in str(error)
        else:
            raise AssertionError("生成器必须拒绝覆盖")


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
