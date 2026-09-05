"""导出后端 OpenAPI 3.1 契约快照并再生成 frontend 的 TypeScript 类型（ADR-4 §2.2）。

用法（仓库根目录）：python scripts/dump_openapi.py
步骤：
  1. cargo run --locked --example openapi-dump frontend/contracts/openapi.json
  2. 语义保持的 definitions 提升：后端 input/output schema 是自包含的 draft-07
     文档（definitions 嵌在子 Schema 内，$ref 写作 "#/definitions/X" 的局部引用），
     openapi-typescript 无法解析这种局部引用。生成类型前在临时副本上把本地
     definitions 提升为 components.schemas 命名类型并重写 $ref。
     入库的 openapi.json 保持后端原始输出，不做改写。
  3. pnpm --dir frontend exec openapi-typescript <临时副本> -o src/engine/contracts/api-types.ts

openapi-typescript 只生成类型，不生成客户端运行时；openapi.json 与 api-types.ts
均入库作为契约快照，前端 HTTP 客户端保持自有实现（frontend/src/api/）。
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = ROOT / "frontend" / "contracts" / "openapi.json"
OUTPUT = ROOT / "frontend" / "src" / "engine" / "contracts" / "api-types.ts"


def run(argv: tuple[str, ...], cwd: Path = ROOT) -> None:
    print(f"+ {' '.join(argv)}", flush=True)
    # Windows 上 cargo/pnpm 多为 .cmd shim，CreateProcess 不能直接执行，经 cmd 解释。
    subprocess.run(("cmd", "/c", *argv) if os.name == "nt" else argv, cwd=cwd, check=True)


def rebase_local_definition_refs(document: object) -> object:
    """把各子 Schema 的本地 definitions 提升为 components.schemas 并重写引用。

    后端 input/output schema 是自包含的 draft-07 文档（definitions 嵌在子 Schema 内，
    $ref 写作 "#/definitions/X" 的子 Schema 局部引用）。若仅重写为子 Schema 的完整
    Pointer，openapi-typescript 会生成自引用路径类型（TS2502 循环）；提升为
    components.schemas 的命名类型后引用成为正常的命名类型（WhereCondition 自递归
    也合法）。同名 definitions 内容一致时复用（去重），不一致时加序号后缀。
    """

    def canonical(schema: object) -> str:
        return json.dumps(schema, sort_keys=True, ensure_ascii=False)

    components = document.setdefault("components", {}).setdefault("schemas", {})

    def ensure_component(name: str, schema: object) -> str:
        target = name
        suffix = 2
        while target in components and canonical(components[target]) != canonical(schema):
            target = f"{name}__{suffix}"
            suffix += 1
        components.setdefault(target, schema)
        return target

    def rewrite_refs(node: object, mapping: dict[str, str]) -> None:
        if isinstance(node, dict):
            ref = node.get("$ref")
            if isinstance(ref, str) and ref.startswith("#/definitions/"):
                name = ref.removeprefix("#/definitions/")
                node["$ref"] = f"#/components/schemas/{mapping.get(name, name)}"
            for value in node.values():
                rewrite_refs(value, mapping)
        elif isinstance(node, list):
            for value in node:
                rewrite_refs(value, mapping)

    def walk(node: object) -> None:
        if isinstance(node, dict):
            for value in node.values():
                walk(value)
            definitions = node.get("definitions")
            if isinstance(definitions, dict):
                mapping = {
                    name: ensure_component(name, schema)
                    for name, schema in definitions.items()
                }
                # 提升的 components 与本地 definitions 共享同一对象，重写会同步生效。
                rewrite_refs(node, mapping)
                del node["definitions"]
        elif isinstance(node, list):
            for value in node:
                walk(value)

    walk(document)
    return document


def main() -> int:
    run(("cargo", "run", "--locked", "--example", "openapi-dump", str(SPEC)))
    document = json.loads(SPEC.read_text(encoding="utf-8"))
    rebased = rebase_local_definition_refs(document)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", suffix=".openapi.json", delete=False
    ) as temp:
        json.dump(rebased, temp, ensure_ascii=False)
        temp_path = temp.name
    try:
        run(
            (
                "pnpm",
                "--dir",
                "frontend",
                "exec",
                "openapi-typescript",
                temp_path,
                "-o",
                str(OUTPUT),
            )
        )
    finally:
        os.unlink(temp_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
