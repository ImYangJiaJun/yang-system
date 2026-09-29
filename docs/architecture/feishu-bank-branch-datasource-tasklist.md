# 飞书外部数据源 xlsx 文件导入 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让飞书外部数据源多出第三种取数方式 `xlsx_import`——上传 xlsx、解析表头、
用户勾列、配父级、导入；**出口与数据使用方式与多维表格完全一致**。

**Architecture:** 只分叉「取数」这一处。数据仍落同一张 `feishu_option`、走同一套
`derive_options` 与 `apply_option_rows`、走同一个出站端点 `approval_options`（按
`source_key` 路由、不区分来源）。新增两个 Action（探表头 / 导入）+ 一个解析模块；
前端新增一个独立向导。

**Tech Stack:** Rust 2021（`yang-base` DSL / `yang-db` Repository）、calamine 0.30.1
（xlsx 流式解析）、MySQL 8.0、React 19 + TypeScript（Vitest）。

**Spec:** [`docs/architecture/feishu-bank-branch-datasource.md`](./feishu-bank-branch-datasource.md)
— 本计划实现它；执行者**两份都要读**。设计文档里凡是标「实测」的性能数字都出自
09-23 那轮本机临时表基准（仓库内无独立出处），本计划的验收不依赖它们。

## Global Constraints

- **不新增 SQL 迁移文件。** 表结构由 `TableSpec` 声明 + 启动期增量同步驱动
  （`AGENTS.md`、`docs/contracts/SCHEMA.md`）。schema 同步**永不删列**。
- **每个 Action 文件恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register`**，
  并在 `actions/mod.rs` 声明与注册（`python scripts/check_architecture.py` 会拒）。
  **`actions/` 目录下不能放无 Action 的辅助文件**。
- **出站响应契约一字不改**：`approval_options` 的 `{code,msg,data}`、`@i18n@<option_id>`、
  `40003`/`40004` 失败码。**本计划不碰 `option/` 与 `approval/` 两个 module 的生产代码。**
- **派生规则不改** → **不要 bump `DERIVE_RULE_VERSION`**（当前 `= 2`）。
- **`field_id` 与 `field_name` 同时写列名**（A9）。漏写 `field_name` 会让审批外部选项
  装配**整批失败**——见 Task 8，那条是硬前提。
- **缺列即拒，多列忽略**（D9）。宁可拒绝，不要静默出错数据。
- **MSRV 1.80 是硬门禁**。新增依赖必须按 `AGENTS.md` 的 docker 冷缓存命令实测，
  **不能只看 CI 绿**（Swatinem 缓存命中会跳过依赖解析）。
  ⚠️ 本机 `docker build`/`docker run` 需要先开 VPN（见仓库记忆）。
- **`config.toml` 被 git 忽略，禁止提交**。可提交的配置只有
  `config.example.toml` / `config.show.toml` / `deploy/config.cloud.example.toml` /
  `.cargo/config.toml`。
- **⚠️ 集成测试会重建业务测试表。** 运行 `python scripts/run_ci.py integration` 或任何
  `--ignored` 集成测试**之前必须获得用户明确许可**——不得自行放行。
  全局规则：数据库默认只读，写操作需用户明确要求 + 二次确认。
- **新集成测试文件必须在 `scripts/run_ci.py` 的 `INTEGRATION` 元组里登记**，
  否则 `run_ci.py --self-test` 的反向发现门禁会失败。

## 常用命令

```bash
# 快速门禁（架构自检 ×3 + fmt + lib 单测 + 前端 typecheck + Vitest）
python scripts/run_ci.py quick

# 全量门禁（含 clippy -D warnings / pnpm check / Playwright ×2）
python scripts/run_ci.py full

# feishu addon 单测
cargo test --lib --locked feishu

# MSRV 冷缓存验证（新增依赖后必做）。**必须带 MSYS_NO_PATHCONV=1**，否则 Git Bash 会把
# `-w /ws/...` 改写成 Windows 路径、docker 直接 exit 125（四个坑的完整清单见根 AGENTS.md
# 的「MSRV 1.80 守护」）。挂载整个 lib_yang 根；在 worktree 里验时 -v 换成工作树根
# （例如 D:/code/lib_yang-wt），否则验的是别人的代码——而且**还要把真 crates 目录再挂一份**
# （worktree 里的 crates/ 是指向主检出的符号链接，Docker 不跟随，实测 exit 101）。
MSYS_NO_PATHCONV=1 docker run --rm \
  -v D:/code/lib_yang:/ws -w /ws/project/yang-system \
  -e CARGO_HOME=/tmp/ch -e CARGO_TARGET_DIR=/tmp/ct -e RUSTUP_TOOLCHAIN=1.80.1 \
  rust:1.80.1-slim cargo check --all-targets --locked

# 契约重生成（前端 TS 类型；不会更新 feishu-projections.json）
python scripts/dump_openapi.py

# 前端
pnpm --dir frontend test
```

## Review Focus

设计文档是愿景文档，它没说到的输入不是「允许它崩」。以下五类是最可能咬到真实用户、
而**任务测试最容易漏掉**的，每条都在对应任务里钉了测试：

1. **用户勾了一列整列为空的列**（银行文件里 `地区名称` 就是 100% 空）→ 该绑定派生 0 个选项
   → 补集停用会把这个**正在被审批使用的控件**的选项全部静默清空。`pull.rs` 的空快照守卫
   是**表级**的，挡不住它。→ Task 10 的逐绑定守卫测试。
2. **上传的多个文件里有一个表头不同** → 必须**整份拒绝**，不能取交集或只导能对上的那个。
   取交集的后果是用户以为导了 10 万行、实际只有 5 万，而且没有任何提示。→ Task 6。
3. **数值型单元格**：`联行号` 若在某份文件里被 Excel 存成数值，读出来会丢前导零、
   大数还会变科学计数法（`1.23E+13`）——**导入的是错值，且不会报错**。
   银行夹具恰好全是文本，掩盖了这条。→ Task 7。
4. **表头重名 / 空名 / 超长（>64 字符）**：列名是身份，重名则身份不唯一；
   空名不是有效身份；超过 `field_id` 的 64 上限会在**插库时**才炸。→ Task 6。
5. **重复点击导入 / 并发导入**：用户手抖点两下，或两个运维同时操作，逐绑定事务会交错，
   两次补集停用互相覆盖。→ Task 11。

---

# 阶段零：地基

四个任务都不碰业务逻辑，各自可独立验收。

## Task 1: xlsx 测试夹具生成

夹具是这一整条链的地基，且有个真问题要先解决：**calamine 只读不写**，
而真实 xlsx（`docs/境内银行网点信息管理-{1,2}.xlsx`）被 `.gitignore` 的 `docs/*.xlsx` 忽略
——**CI 上根本不存在这两个文件**，不能用它们做夹具。

所以自造夹具。xlsx 本质是一个装着几个 XML 的 zip，用 Python 标准库 `zipfile` 就能生成，
不需要引入任何依赖。

**Files:**
- Create: `scripts/make_xlsx_fixtures.py`
- Create: `tests/fixtures/xlsx/`（脚本产物，提交进 git）
- Create: `tests/fixtures/xlsx/README.md`

**Interfaces:**
- Consumes: 无（本任务是起点）
- Produces: `tests/fixtures/xlsx/` 下的夹具文件，Task 2/6/7/9/10 的测试全部读它。
  文件清单与用途：

  | 文件 | 用途 | 关键形状 |
  |---|---|---|
  | `bank_1.xlsx` | 正常主路径 | 8 列（`序号/开户行行名/归属银行/归属银行编码/联行号/开户行地址/地区名称/地区编码`），**3 行**，全文本（`序号` 是数值单元格） |
  | `bank_2.xlsx` | 多文件拼接 | 与 `bank_1` 表头**完全相同**，另 **2 行**，`序号` 接续（4、5） |
  | `header_missing_column.xlsx` | 缺列即拒 | 表头少 `联行号` |
  | `header_extra_column.xlsx` | 多列忽略 | 表头多一列 `备注` |
  | `header_mismatch_two.xlsx` | 两文件互不一致 | 表头少 `归属银行`（与 `bank_1` 配用） |
  | `header_duplicate.xlsx` | 重名拒 | 表头有两个 `联行号` |
  | `header_blank.xlsx` | 空列名跳过 | 第 3 列表头为空串 |
  | `header_spaces.xlsx` | 表头 trim | 表头带首尾空格（`" 序号 "`），其余正常 |
  | `bank_shared_strings.xlsx` | **sharedStrings 形态**（Task 6 补） | 走 `t="s"` + `xl/sharedStrings.xml`，内容与 `bank_1` **等值**。**这是真实 Excel 导出的默认形态**——不加它，`DataRef::SharedString` 那个臂零覆盖 |
  | `header_reordered.xlsx` | **列序无关**（Task 6 补） | 与 `bank_1` **列集合相同、顺序不同**。不加它，「按名取值不按位置」这条性质就只是一句没人验的声明 |
  | `shared_strings_missing.xlsx` | **panic 防护**（Task 7 补） | 含 `t="s"` 单元格但**不带** `xl/sharedStrings.xml` 部件（rels 与 Content_Types 的声明仍在）。命中 calamine `cells_reader.rs:356` 的 `&strings[idx]` **无边界检查** → 直接索引 panic。断言它**返回 `Err(Unreadable)` 而不是 panic** |
  | `header_long.xlsx` | 超长拒 | 一个 65 字符的列名 |
  | `column_all_blank.xlsx` | 整列为空 | `联行号` 列全部为空（配 `column_code_empty` 名称，供 Task 10 的守卫测试） |
  | `numeric_code.xlsx` | 数值不推断 | `联行号` 是数值单元格（非文本） |
  | `only_header.xlsx` | 空快照 | 只有表头、零数据行 |
  | `overlong_value.xlsx` | 超长文案截断 | 表头正常，`开户行行名` 有一个 300 字符的值 |
  | `two_sheets.xlsx` | 只读第一张 | 两个 sheet，第二张表头不同 |
  | `not_a_zip.bin` | 魔数嗅探 | 一段纯文本，不是 zip |

- [ ] **Step 1: 写生成脚本**

创建 `scripts/make_xlsx_fixtures.py`：

> **以仓库里的脚本为准。** 下面这段代码块是初版；实现时经一轮修复补了两处（见 Step 2 后的注记）：
> zip 条目改用固定 `date_time` 的 `ZipInfo`（重跑逐字节可复现），
> 以及给带首尾空白的 `<t>` 加 `xml:space="preserve"`。
> **直接照下面这段抄会得到不可复现的版本**——请打开 `scripts/make_xlsx_fixtures.py` 对照。

```python
"""生成 xlsx 解析测试夹具。

xlsx 是一个装着若干 XML 的 zip。用标准库 zipfile 手写最小结构，
**不引入任何第三方依赖**——夹具必须能在 CI 里被任何环境复现。

为什么不用仓库里那两个真实文件：它们被 .gitignore 的 `docs/*.xlsx` 忽略，
CI 上不存在（见 docs/architecture/feishu-bank-branch-datasource.md §6）。

用法：python scripts/make_xlsx_fixtures.py
产物：tests/fixtures/xlsx/*.xlsx（提交进 git）
"""

from __future__ import annotations

import zipfile
from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "xlsx"

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
{sheet_overrides}
</Types>
"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>
"""


def column_letter(index: int) -> str:
    """0-based 列号 → Excel 列字母（A、B、…、Z、AA）。"""
    letters = ""
    index += 1
    while index > 0:
        index, remainder = divmod(index - 1, 26)
        letters = chr(ord("A") + remainder) + letters
    return letters


def cell_xml(ref: str, value: object) -> str:
    """一个单元格。字符串走 inlineStr，数值走 n——**这个区别正是 numeric_code 夹具的意义**。"""
    if isinstance(value, str):
        escaped = (
            value.replace("&", "&amp;")
            .replace("<", "&lt;")
            .replace(">", "&gt;")
        )
        return f'<c r="{ref}" t="inlineStr"><is><t>{escaped}</t></is></c>'
    return f'<c r="{ref}" t="n"><v>{value}</v></c>'


def sheet_xml(rows: list[list[object]]) -> str:
    lines = []
    for row_index, row in enumerate(rows, start=1):
        cells = "".join(
            cell_xml(f"{column_letter(col_index)}{row_index}", value)
            for col_index, value in enumerate(row)
            if value is not None
        )
        lines.append(f'<row r="{row_index}">{cells}</row>')
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<sheetData>{"".join(lines)}</sheetData></worksheet>'
    )


def write_xlsx(path: Path, sheets: list[tuple[str, list[list[object]]]]) -> None:
    """写一个 xlsx。sheets 是 [(sheet 名, 行数据)]，至少一张。"""
    overrides = "".join(
        f'<Override PartName="/xl/worksheets/sheet{i}.xml" '
        'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        for i in range(1, len(sheets) + 1)
    )
    sheet_entries = "".join(
        f'<sheet name="{name}" sheetId="{i}" r:id="rId{i}"/>'
        for i, (name, _) in enumerate(sheets, start=1)
    )
    workbook = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        f"<sheets>{sheet_entries}</sheets></workbook>"
    )
    workbook_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        + "".join(
            f'<Relationship Id="rId{i}" '
            'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
            f'Target="worksheets/sheet{i}.xml"/>'
            for i in range(1, len(sheets) + 1)
        )
        + "</Relationships>"
    )

    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", CONTENT_TYPES.format(sheet_overrides=overrides))
        archive.writestr("_rels/.rels", ROOT_RELS)
        archive.writestr("xl/workbook.xml", workbook)
        archive.writestr("xl/_rels/workbook.xml.rels", workbook_rels)
        for i, (_, rows) in enumerate(sheets, start=1):
            archive.writestr(f"xl/worksheets/sheet{i}.xml", sheet_xml(rows))


# 银行形状的表头——与设计 §4.6 的真实文件同列同名（但只放几行，夹具要小）
BANK_HEADER: list[object] = [
    "序号", "开户行行名", "归属银行", "归属银行编码",
    "联行号", "开户行地址", "地区名称", "地区编码",
]


def bank_row(seq: int, name: str, bank: str, code: str) -> list[object]:
    return [seq, name, bank, bank, code, "某某路 1 号", "", "510100"]


def bank(**overrides: object) -> list[list[object]]:
    return [list(overrides.get("header", BANK_HEADER)), *overrides.get("rows", [])]


def build() -> None:
    common_rows = [
        bank_row(1, "中国工商银行成都春熙路支行", "中国工商银行", "102651000011"),
        bank_row(2, "中国工商银行成都天府大道支行", "中国工商银行", "102651000012"),
        bank_row(3, "成都农商银行高新支行", "", "314651000013"),
    ]
    tail_rows = [
        bank_row(4, "浙商银行CIPS虚拟行号", "", "316651000014"),
        bank_row(5, "中国人民银行成都分行", "", "001651000015"),
    ]

    write_xlsx(OUT_DIR / "bank_1.xlsx", [("境内银行网点信息管理", bank(rows=common_rows))])
    write_xlsx(OUT_DIR / "bank_2.xlsx", [("境内银行网点信息管理", bank(rows=tail_rows))])

    # 缺列：少 `联行号`
    missing = ["序号", "开户行行名", "归属银行", "归属银行编码", "开户行地址", "地区名称", "地区编码"]
    write_xlsx(
        OUT_DIR / "header_missing_column.xlsx",
        [("Sheet1", [missing, [1, "中国工商银行成都春熙路支行", "中国工商银行", "中国工商银行", "某某路 1 号", "", "510100"]])],
    )

    # 多列：多一列 `备注`（应被忽略）
    extra = [*BANK_HEADER, "备注"]
    write_xlsx(
        OUT_DIR / "header_extra_column.xlsx",
        [("Sheet1", [extra, [*bank_row(1, "中国工商银行成都春熙路支行", "中国工商银行", "102651000011"), "随手记"]])],
    )

    # 与 bank_1 互不一致：少 `归属银行`
    mismatch = ["序号", "开户行行名", "归属银行编码", "联行号", "开户行地址", "地区名称", "地区编码"]
    write_xlsx(
        OUT_DIR / "header_mismatch_two.xlsx",
        [("Sheet1", [mismatch, [1, "中国工商银行成都春熙路支行", "中国工商银行", "102651000011", "某某路 1 号", "", "510100"]])],
    )

    # 重名：两个 `联行号`
    dup = ["序号", "开户行行名", "联行号", "归属银行", "联行号", "开户行地址", "地区名称", "地区编码"]
    write_xlsx(OUT_DIR / "header_duplicate.xlsx", [("Sheet1", [dup, [1, "某支行", "102651000011", "某银行", "102651000012", "", "", "510100"]])])

    # 空列名（第 3 列）
    blank = ["序号", "开户行行名", "", "归属银行", "联行号", "开户行地址", "地区名称", "地区编码"]
    write_xlsx(OUT_DIR / "header_blank.xlsx", [("Sheet1", [blank, [1, "某支行", "忽略我", "某银行", "102651000011", "", "", "510100"]])])

    # 表头带首尾空格（要 trim 后才能匹配）
    spaces = [" 序号 ", "开户行行名 ", " 归属银行", "归属银行编码", "联行号", "开户行地址", "地区名称", "地区编码"]
    write_xlsx(
        OUT_DIR / "header_spaces.xlsx",
        [("Sheet1", [spaces, [1, "中国工商银行成都春熙路支行", "中国工商银行", "中国工商银行", "102651000011", "某某路 1 号", "", "510100"]])],
    )

    # 超长列名：65 字符（超过 field_id 的 max_length=64）
    long_name = "长" * 65
    write_xlsx(
        OUT_DIR / "header_long.xlsx",
        [("Sheet1", [[*BANK_HEADER[:4], long_name, *BANK_HEADER[5:]], [1, "某支行", "某银行", "某银行", "值", "", "", "510100"]])],
    )

    # 整列为空：`联行号` 全空（Task 10 的逐绑定空快照守卫测试用）
    blank_column_rows = [
        [1, "中国工商银行成都春熙路支行", "中国工商银行", "中国工商银行", "", "某某路 1 号", "", "510100"],
        [2, "中国工商银行成都天府大道支行", "中国工商银行", "中国工商银行", "", "某某路 2 号", "", "510100"],
    ]
    write_xlsx(OUT_DIR / "column_all_blank.xlsx", [("Sheet1", [BANK_HEADER, *blank_column_rows])])

    # 数值型联行号：t="n" 而非 inlineStr
    numeric_row = [1, "中国工商银行成都春熙路支行", "中国工商银行", "中国工商银行", 102651000011, "某某路 1 号", "", 510100]
    write_xlsx(OUT_DIR / "numeric_code.xlsx", [("Sheet1", [BANK_HEADER, numeric_row])])

    # 只有表头
    write_xlsx(OUT_DIR / "only_header.xlsx", [("Sheet1", [BANK_HEADER])])

    # 超长文案：表头正常，`开户行行名` 的值有 300 字符
    # （`feishu_option.label` 是 max_length(255)，不截断会在插库时才炸）
    write_xlsx(
        OUT_DIR / "overlong_value.xlsx",
        [("Sheet1", [BANK_HEADER, bank_row(1, "长" * 300, "中国工商银行", "102651000011")])],
    )

    # 两个 sheet：第二张表头不同（验证只读第一张）
    write_xlsx(
        OUT_DIR / "two_sheets.xlsx",
        [
            ("第一张", bank(rows=common_rows)),
            ("第二张", [["完全", "不同", "的", "表头"], ["a", "b", "c", "d"]]),
        ],
    )

    # 非 zip
    (OUT_DIR / "not_a_zip.bin").write_bytes("这不是一个 zip 文件，魔数不是 PK\\x03\\x04".encode("utf-8"))


if __name__ == "__main__":
    build()
    for path in sorted(OUT_DIR.iterdir()):
        print(f"{path.name}\t{path.stat().st_size} bytes")
```

- [ ] **Step 2: 运行脚本，确认产物生成**

```bash
python scripts/make_xlsx_fixtures.py
```

预期：打印 **19 行**——18 个夹具（17 个 `.xlsx` + `not_a_zip.bin`）**加上 `README.md`**
（脚本遍历的是整个目录，README 也在里面）。无异常。

> 其中 `bank_shared_strings.xlsx` 与 `header_reordered.xlsx` 是 Task 6 补的（见夹具表）。
> 脚本要为前者支持 **sharedStrings 形态**：多写 `xl/sharedStrings.xml`、两处关系登记、
> 单元格用 `t="s"` + `<v>下标</v>`。两个坑：**字符串表必须先于 sheet 渲染**
> （表是被单元格填出来的）；**空串也要进表**（去 calamine 源码确认 `read_string`
> 对 `<si><t></t></si>` 返回 `Some("")`，只有无 `<t>` 的 `<si/>` 才不压栈），
> 否则下标整体错位、表头会指向错的文本。新 rel 的 `rId` 取 `len(sheets)+1`，别挤占 sheet 的 `rId1..N`。

> 脚本用**固定 `date_time`** 写 zip 条目，所以重跑**逐字节可复现**：
> 连跑两次后 `git status --short tests/fixtures/xlsx/` 必须为空。
> 不这样做的后果是——计划要求「改夹具必须重跑生成脚本」，而重跑会把 14 个已提交的
> 二进制全弄脏（内容没变、只有时间戳变），评审时得逐个解释。

- [ ] **Step 3: 独立验证产物是合法 xlsx（不依赖 Rust）**

用 Python 自己的 zipfile 读回，逐个断言结构完整：

```bash
python - <<'PY'
import zipfile, pathlib
root = pathlib.Path("tests/fixtures/xlsx")
required = {"[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels"}
for path in sorted(root.glob("*.xlsx")):
    with zipfile.ZipFile(path) as z:
        names = set(z.namelist())
        missing = required - names
        assert not missing, f"{path.name} 缺: {missing}"
        sheets = [n for n in names if n.startswith("xl/worksheets/")]
        assert sheets, f"{path.name} 没有任何 sheet"
        print(f"OK {path.name}: {len(sheets)} sheet(s)")
PY
```

预期：每个 `.xlsx` 都打印 `OK`，没有 `AssertionError`。
（`not_a_zip.bin` 不在 glob 里——它本来就是来验证「不是 zip 会被拒」的。）

- [ ] **Step 4: 写夹具说明**

创建 `tests/fixtures/xlsx/README.md`：

```markdown
# xlsx 解析测试夹具

由 `python scripts/make_xlsx_fixtures.py` 生成，**产物提交进 git**。

## 为什么自造而不用真实文件

仓库里有真实数据 `docs/境内银行网点信息管理-{1,2}.xlsx`（7.4 MB，15.4 万行），
但它们被 `.gitignore` 的 `docs/*.xlsx` 忽略，**CI 上不存在**，
不能做夹具。真实文件只在本地做人工验证用。

## 为什么手写 XML 而不引依赖

xlsx 是装着若干 XML 的 zip。用标准库 `zipfile` 手写最小结构，
夹具就能在任何有 Python 的环境里复现，不引入新的构建依赖。

## 每个夹具的意义

见 `docs/architecture/feishu-bank-branch-datasource-tasklist.md` Task 1 的表格。
**改夹具必须重跑生成脚本**，不要手改二进制。
```

- [ ] **Step 5: 提交**

```bash
git add scripts/make_xlsx_fixtures.py tests/fixtures/xlsx
git commit -m "test(feishu): xlsx 解析夹具与生成脚本（真实文件被 gitignore，CI 上不存在）"
```

---

## Task 2: 引入 calamine 并过 MSRV 冷缓存门禁

**Files:**
- Modify: `Cargo.toml`（`[dependencies]` 段）
- Modify: `Cargo.lock`（由 cargo 更新）
- Create: `src/addon/feishu/domain/xlsx.rs`（本任务只放冒烟函数）

**Interfaces:**
- Consumes: Task 1 的夹具
- Produces: `xlsx` 模块可编译；`pub(crate) fn sniff_is_zip(bytes: &[u8]) -> bool`
  供 Task 6 使用。本任务的模块骨架后续任务会扩展。

- [ ] **Step 1: 写失败的冒烟测试**

创建 `src/addon/feishu/domain/xlsx.rs`：

```rust
//! xlsx 解析：嗅探、表头、按列名取值。
//!
//! **只读，不写。** 本模块不碰数据库、不碰网络，是纯函数层，
//! 所以它能在单元测试里被完整覆盖（导入 Action 的那层不行，要真库）。

/// zip 魔数：xlsx 是 zip，`PK\x03\x04`。
///
/// `allowed_content_types` 只是**客户端自称**（框架 `media.rs` 明写它不能替代
/// 内容校验），所以真正的判定在这里。
pub(crate) fn sniff_is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_magic_is_accepted() {
        assert!(sniff_is_zip(b"PK\x03\x04rest"));
    }

    #[test]
    fn arbitrary_bytes_are_rejected() {
        assert!(!sniff_is_zip(b"not a zip at all"));
        assert!(!sniff_is_zip(b""));
        // 真实夹具：一段纯文本
        assert!(!sniff_is_zip(include_bytes!(
            "../../../../tests/fixtures/xlsx/not_a_zip.bin"
        )));
    }
}
```

在 `src/addon/feishu/domain/mod.rs` 加一行 `pub(crate) mod xlsx;`（按该文件既有的
`mod` 声明风格与排序位置插入）。

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：**编译失败**——`sniff_is_zip` 的 `starts_with` 本身没问题，
但如果模块未声明会报「unresolved module」；确认是「模块或符号不存在」这类错误，
而不是语法错。

- [ ] **Step 3: 加依赖并让测试通过**

在 `Cargo.toml` 的 `[dependencies]` 段末尾追加（照抄同文件里 `lettre` 的注释风格）：

```toml
# xlsx 解析（外部数据源的文件导入）。精确锁定：0.31–0.35 需要 Rust 1.83、
# 0.36.1 需要 1.88，都过不了 MSRV 1.80 那个 job。
calamine = "=0.30.1"
```

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：2 个测试 PASS。`Cargo.lock` 被自动更新（它是被跟踪文件，要一起提交）。

- [ ] **Step 4: 确认现有的配置同步测试没被打破**

`cargo test --lib --locked` 里有一条 `show_config_stays_in_sync_with_settings_schema_and_defaults`，
它靠 `starts_with` 前缀白名单剥离默认值。本任务没动配置，但**先记录它现在是绿的**，
好在 Task 3 出问题时能分辨是不是自己弄坏的：

```bash
cargo test --lib --locked config::tests::show_config_stays_in_sync
```

预期：PASS。

- [ ] **Step 5: MSRV 冷缓存验证（必做，不能跳过）**

⚠️ **本机 docker 需要先开 VPN。**

```bash
docker run --rm -v /d/code/lib_yang:/ws -w /ws/project/yang-system \
  -e CARGO_HOME=/tmp/ch -e CARGO_TARGET_DIR=/tmp/ct -e RUSTUP_TOOLCHAIN=1.80.1 \
  rust:1.80.1-slim cargo check --all-targets --locked
```

预期：编译通过（会拉取并冷编译全部依赖，第一次几分钟）。
**若失败**：说明 `calamine 0.30.1` 的传递依赖里有 MSRV > 1.80 的包。
不要改 `.cargo/config.toml` 的 fallback resolver；改为在 `Cargo.toml` 里
精确锁住那个包（照抄 `lettre` 的既有做法），再重跑本步。

> **为什么不能只看 CI 绿**：CI 的 msrv job 用 Swatinem 缓存（key=`msrv-1.80`），
> 缓存命中会跳过依赖清单解析，掩盖新依赖的不兼容（`AGENTS.md` 里记着这条）。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock src/addon/feishu/domain/xlsx.rs src/addon/feishu/domain/mod.rs
git commit -m "feat(feishu): 引入 calamine 解析 xlsx，含魔数嗅探与 MSRV 冷缓存验证"
```

---

## Task 3: 配置上限调整（body 16 MiB / timeout 60s）

**这个任务改的是默认值，不是只改本地配置。** 理由是启动期 fail-closed：
任一 multipart Action 的 `max_total_bytes` 大于 `max_body_bytes` 时**进程拒绝启动**。
而 xlsx 导入的 `MultipartSpec` 要设 16 MiB（两个银行文件 7.4 MB），
所以任何没有显式抬高这一项的部署，升级后都起不来。

把默认值抬到 16 MiB 让**最小配置也能启动**；显式写了旧值的部署会在启动时
收到一条点名的错误（fail-closed 的文案里同时给出 Action 名与两个数字），
运维据此改配置即可。

**Files:**
- Modify: `src/config/mod.rs`（默认值常量 + `Default` 实现 + 两处内联断言）
- Modify: `config.show.toml`（值 + 注释）
- Modify: `docs/contracts/CONFIGURATION.md`（默认值表格）
- Modify: `config.toml`（**本机、不入库**——但要改，否则本机起不来）

**Interfaces:**
- Consumes: 无
- Produces: 生效的 `http.max_body_bytes = 16777216`、`http.request_timeout_seconds = 60`，
  Task 9/10 的 multipart Action 依赖它才不会在启动期被拒。

- [ ] **Step 1: 写失败的默认值断言**

在 `src/config/mod.rs` 的测试模块里，找到
`minimal_config_uses_safe_defaults_and_derives_namespaces`（约 1752 行），
把那一行断言改掉：

```rust
        assert_eq!(settings.app.name, "yang-system");
        assert_eq!(settings.http.bind, "127.0.0.1:8080");
        // 16 MiB：multipart 导入的 MultipartSpec 需要它，且启动期 fail-closed
        // 要求 max_total_bytes <= max_body_bytes。默认值低于此会让最小配置起不来。
        assert_eq!(settings.http.max_body_bytes, 16 * 1024 * 1024);
        assert_eq!(settings.http.request_timeout_seconds, 60);
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked config::tests::minimal_config_uses_safe_defaults
```

预期：FAIL，`assertion left == right` 显示实际 `1048576` / `30`。

- [ ] **Step 3: 改默认值**

`src/config/mod.rs` 的两个常量：

```rust
const fn default_http_max_body_bytes() -> usize {
    // 16 MiB。xlsx 文件导入的 MultipartSpec.max_total_bytes 取同一上限，
    // 而启动期 fail-closed 要求它不大于本值——默认值更低会让最小配置起不来。
    16 * 1024 * 1024
}

const fn default_http_request_timeout_seconds() -> u64 {
    // 60 秒。导入（xlsx）是唯一的分钟级请求：实测 15.4 万行在旧逐行写路径上
    // 要十几分钟（设计 §5.8），批量写入落地后仍须按实测墙钟显式配置本值——
    // 这条默认值只覆盖普通请求，不要拿它当导入预算。
    60
}
```

`HttpSettings::default()` 不用改（它调的就是这两个函数）。

- [ ] **Step 4: 运行确认通过**

```bash
cargo test --lib --locked config::tests
```

预期：全 PASS。**若 `show_config_stays_in_sync` 失败**，说明 `config.show.toml`
还没同步（下一步做）——它逐行比对展示配置与「剥离默认值后的最小配置」。

- [ ] **Step 5: 同步 `config.show.toml`**

把 `[http]` 段那两行改成新值并同步注释（该文件是给人看的参考，注释必须与内置默认一致）：

```toml
# 请求体大小上限（字节）。
# 默认：16777216（16 MiB），允许 1..=16777216（16 MiB）
max_body_bytes = 16777216

# 单个请求处理超时。
# 默认：60，允许 1..=300
request_timeout_seconds = 60
```

```bash
cargo test --lib --locked config::tests
```

预期：全 PASS（含 `show_config_stays_in_sync`）。

> **注意**：本任务没有**新增** `[http]` 字段，只是改了值，所以
> `show_config_stays_in_sync` 里那串 `starts_with` 前缀白名单**不用动**。

- [ ] **Step 6: 同步配置契约文档**

`docs/contracts/CONFIGURATION.md` 的默认值表格里改两行：

```markdown
| `http.max_body_bytes` | `16777216`（16 MiB，允许至 16 MiB） |
| `http.request_timeout_seconds` | `60` |
```

- [ ] **Step 7: 改本机 `config.toml`（不入库）**

```toml
[http]
bind = "127.0.0.1:8080"
max_body_bytes = 16777216
request_timeout_seconds = 60
max_concurrency = 256
```

确认它**没有**被 git 跟踪：

```bash
git status --short config.toml
```

预期：无输出（`.gitignore` 里有 `/config.toml`）。**若它出现在输出里，立刻停下**——
说明忽略规则被动了，提交它会把本地凭据带进仓库。

- [ ] **Step 8: 提交**

```bash
git add src/config/mod.rs config.show.toml docs/contracts/CONFIGURATION.md
git commit -m "feat(config): http 默认体限抬到 16 MiB、超时抬到 60s，供 xlsx 导入上传"
```

---

## Task 4: `ingest_mode` 第三取值（四处同序）

**取值域在这个仓库里写了四遍**，而且第四处不在 `src/` 也不在 `frontend/src/`。
漏任何一处都有测试会红，但**只有前三处是生产逻辑，第四处是契约**——四处必须同序。

> ⚠️ 后端那条门禁用 `assert_eq!` 对 `Vec` 做**含顺序**的逐位比较，不排序。
> `values` 的顺序必须与表声明 `.options([...])` 的顺序**逐字一致**。
>
> ⚠️ 同一条门禁**不 fail-closed**：契约里表名拼错（例如写成 `feishu_datasources`）
> 时它**静默 return**，整条防线失效而不报错。改完务必确认它真的跑了（见 Step 5）。

**Files:**
- Modify: `src/addon/feishu/datasource/table.rs`（表声明）
- Modify: `src/addon/feishu/datasource/actions/create_datasource_table.rs:74-80`（白名单）
- Modify: `src/addon/feishu/datasource/actions/update_datasource_table.rs:133-139`（白名单）
- Modify: `frontend/contracts/feishu-projections.json:126-129`（契约）
- Modify: `frontend/src/features/feishu/types.ts:63-80`（前端选项表）

**Interfaces:**
- Consumes: 无
- Produces: `ingest_mode` 取值域 = `["push", "pull", "xlsx_import"]`（**这个顺序**），
  Task 9/10 按 `xlsx_import` 分流。

- [ ] **Step 1: 写失败的白名单测试**

在 `create_datasource_table.rs` 的 `#[cfg(test)] mod tests` 里，
找到现有那条 `rejects_an_unknown_ingest_mode`，在它旁边加一条：

```rust
    #[test]
    fn accepts_the_xlsx_import_mode() {
        // 第三种取数方式：文件导入。白名单是手写的 matches!，漏一处就是
        // 「向导建得出来、编辑保存不了」——所以 create 与 update 两处都要有这条断言。
        let mut input = valid_input();
        input.ingest_mode = Some("xlsx_import".to_string());
        assert!(
            input.validate().is_ok(),
            "xlsx_import 必须被 create 路径接受"
        );
    }
```

`update_datasource_table.rs` 的测试模块里加对称的一条（`valid_input()` 换成该文件
自己的 helper 名，先读一眼该文件测试模块里现成的 helper 叫什么）：

```rust
    #[test]
    fn accepts_the_xlsx_import_mode() {
        let mut input = valid_input();
        input.ingest_mode = Some("xlsx_import".to_string());
        assert!(
            input.validate().is_ok(),
            "xlsx_import 必须被 update 路径接受——两处白名单是两份拷贝"
        );
    }
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked accepts_the_xlsx_import_mode
```

预期：FAIL 2 条（`ParamInvalid`）。

- [ ] **Step 3: 改表声明与两处白名单**

`src/addon/feishu/datasource/table.rs`：

```rust
            ingest_mode => Radio::<String>::new()
                .title("取数方式")
                .require(true)
                .varchar(16)
                .options([
                    ("push", "手工推送"),
                    ("pull", "定时拉取"),
                    ("xlsx_import", "文件导入"),
                ])
                .default("push")
                .filterable(true),
```

`create_datasource_table.rs` 与 `update_datasource_table.rs` 各一处：

```rust
            if !matches!(mode, "push" | "pull" | "xlsx_import") {
                return Err(BaseError::ParamInvalid(
                    "ingest_mode".to_string(),
                    "取数方式只能是 push、pull 或 xlsx_import".to_string(),
                ));
            }
```

> **注意**：`Radio` 配了 `.varchar(16)`，所以它编译成 `FieldType::String`，
> **不是 `Enum`**。而 schema 的类型兼容判定对 `String` 只看 varchar/char 与长度、
> **不比较候选值集合**——所以加一个选项值是**零 DDL 变更**，不会生成
> `MODIFY COLUMN`，也不需要动 schema 同步。这一点与 09-23 版设计的说法相反。

- [ ] **Step 4: 改契约文件与前端选项表**

`frontend/contracts/feishu-projections.json`（**顺序必须逐字同 Step 3**）：

```json
      "ingest_mode": {
        "values": ["push", "pull", "xlsx_import"],
        "enforced_by": "backend"
      },
```

`frontend/src/features/feishu/types.ts`：

```typescript
export type IngestMode = "push" | "pull" | "xlsx_import";

export const INGEST_MODE_OPTIONS: ReadonlyArray<{
  value: IngestMode;
  label: string;
  hint: string;
}> = [
  {
    value: "push",
    label: "手工推送",
    hint: "由多维表格自动化工作流推送选项，服务端不主动出网。",
  },
  {
    value: "pull",
    label: "定时拉取",
    hint: "服务端按配置的间隔主动拉取，需要先配好应用凭证与坐标。",
  },
  {
    value: "xlsx_import",
    label: "文件导入",
    hint: "由人上传 xlsx 文件导入选项；服务端不出网，也没有定时同步。",
  },
];
```

- [ ] **Step 5: 确认后端门禁真的跑了（它不 fail-closed）**

```bash
cargo test --lib --locked the_enum_domains_in_the_contract_match_the_table_declarations
```

预期：PASS。**这条必须亲眼看到跑过**——如果契约里表名被写错，它会静默 return
而不报错，表现为「测试绿但防线没生效」。要确认它真的比对了，可以临时把
契约里的 `values` 改成 `["push", "pull"]` 跑一次，**应当 FAIL**，改回来。

- [ ] **Step 6: 跑两侧测试与契约重生成**

```bash
cargo test --lib --locked feishu
pnpm --dir frontend test -- features/feishu/api.test.ts
python scripts/dump_openapi.py
pnpm --dir frontend format
```

预期：全 PASS。`dump_openapi.py` 会重生成
`frontend/contracts/openapi.json` 与 `frontend/src/engine/contracts/api-types.ts`
（**两个生成物禁止手改**，一起提交）。

> ⚠️ **那条 `pnpm format` 不是可选的。** `openapi-typescript` 产的是 **4 空格缩进**，
> 而入库件是 **2 空格**（prettier 格式），脚本自己**不跑 prettier**。
> 所以裸跑 `dump_openapi.py` 会产生**上万行纯缩进 churn**、并让 `pnpm format:check` 变红。
> **先 `dump_openapi.py` 再 `pnpm format`**，然后 `git diff --stat` 看一眼——
> 如果生成物实际没变（本次枚举改动就属这种，`ingest_mode` 在 OpenAPI 里是
> `Option<String>`、取值域只活在运行时表 DSL），prettier 之后的 diff 会是**空的**，
> 那就**不要提交这两个生成物**。

> **`dump_openapi.py` 不会更新 `feishu-projections.json`**——那个是手工维护的，
> 已在 Step 4 改过。别指望脚本替你改。

- [ ] **Step 7: 提交**

```bash
git add src/addon/feishu/datasource/table.rs \
        src/addon/feishu/datasource/actions/create_datasource_table.rs \
        src/addon/feishu/datasource/actions/update_datasource_table.rs \
        frontend/contracts/feishu-projections.json \
        frontend/contracts/openapi.json \
        frontend/src/engine/contracts/api-types.ts \
        frontend/src/features/feishu/types.ts
git commit -m "feat(feishu): ingest_mode 新增 xlsx_import（四处取值域同序）"
```

---

## Task 5: 复合索引 `idx_feishu_option_pick`

设计 §5.10 实测：加索引把出站端点从 375–556 ms 降到 126–278 ms，
但**不加也已在 2500 ms 预算内**——所以它是**建议项，不是硬前提**。
仍然做，因为常规路径快两个数量级，且去掉的 filesort 在并发下才是真危险的部分。

**Files:**
- Modify: `src/addon/feishu/option/table.rs`（`TableSpec` 上声明索引）
- Create: `tests/feishu_option_index_integration.rs`（真库断言索引存在）

**Interfaces:**
- Consumes: 无
- Produces: 数据库上出现索引 `idx_feishu_option_pick (source_key, enabled, sort_order, option_id)`。

- [ ] **Step 1: 写失败的集成测试**

创建 `tests/feishu_option_index_integration.rs`。
**照抄 `tests/feishu_options_integration.rs` 的整体形态**（它专门做 Schema 级验证），
但**不要照抄它去建数据源或调 Action**——那是另一个文件的职责：

```rust
//! # 覆盖什么
//!
//! 出站端点的复合索引 `idx_feishu_option_pick` 必须在真实 MySQL 上真的建出来。
//! 单元测试断言不了索引（`TableDefinition` 不暴露索引），所以只能在真库上验证。
//!
//! # 依赖
//!
//! 需要 `YANG_SYSTEM_TEST_DATABASE_URL`（库名以 `_test` 结尾）
//! 与 `YANG_SYSTEM_TEST_REDIS_URL`（Redis DB 15）。

// 三个 helper 从 tests/feishu_options_integration.rs 复制：
//   connect_test_database()  —— 断言库名以 _test 结尾
//   drop_feishu_tables(&db)  —— 白名单化 DROP，只删飞书那几张表
//   sync_feishu_schema(db)   —— 建表；**内部会关掉连接池，之后必须重新连接**
// 先读 tests/common/ 看有没有现成的可 use；没有就整段复制（仓库既有做法是
// 每个集成测试文件自带 helper，所以复制是符合惯例的）。
```

测试函数本体：

```rust
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn pick_index_exists_with_the_declared_columns_in_order() {
    let database = connect_test_database()
        .await
        .unwrap_or_else(|error| panic!("连接测试库失败: {error:#}"));
    drop_feishu_tables(&database)
        .await
        .unwrap_or_else(|error| panic!("预清理失败: {error:#}"));

    let outcome = async {
        sync_feishu_schema(database).await?;
        // 同步会关池，必须重连（feishu_options_integration.rs 里有这条注释）
        let handle = connect_test_database().await?;
        let columns = index_columns(&handle, "feishu_option", "idx_feishu_option_pick").await?;
        assert_eq!(
            columns,
            vec!["source_key", "enabled", "sort_order", "option_id"],
            "复合索引的列与顺序是设计 §5.10 的前提（前两个等值前缀之后，\
             索引序恰好就是 ORDER BY 的 (sort_order, option_id)）"
        );
        Ok(())
    }
    .await;

    let cleanup = match connect_test_database().await {
        Ok(handle) => drop_feishu_tables(&handle).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("复合索引集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
```

`index_columns` 查询 `information_schema.STATISTICS`（`SEQ_IN_INDEX` 排序）：

```rust
async fn index_columns(
    database: &Database,
    table: &str,
    index: &str,
) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_NAME = ? \
         ORDER BY SEQ_IN_INDEX",
    )
    .bind(table)
    .bind(index)
    .fetch_all(database.pool())
    .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}
```

- [ ] **Step 2: 登记进 `run_ci.py`（不登记会自检失败）**

`scripts/run_ci.py` 的 `INTEGRATION` 元组里加一条（照抄相邻条目的形状）：

```python
    Command(
        "飞书选项复合索引集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_option_index_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
```

```bash
python scripts/run_ci.py --self-test
```

预期：PASS。**这条是反向发现门禁**：`tests/` 下任何文件内容含 `YANG_SYSTEM_TEST_`
就必须登记进 `INTEGRATION`，漏登记或登记了不存在的入口都会失败。

- [ ] **Step 3: 申请许可并运行集成测试，确认失败**

⚠️ **集成测试会重建业务测试表，必须先获得用户明确许可。**

```bash
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
cargo test --test feishu_option_index_integration --locked -- --ignored --test-threads=1
```

预期：FAIL——索引不存在，`columns` 是空 `Vec`。

- [ ] **Step 4: 声明索引**

`src/addon/feishu/option/table.rs` 的 `TableSpec` 链上追加
（`index_named` 在框架 `table/definition.rs`，仓库里已有用法先例，
照抄 `src/infrastructure/schema.rs` 或 `demo/notes/table.rs` 的写法）：

```rust
        // 出站端点的复合索引。前两个等值前缀（source_key, enabled）之后，
        // 索引序恰好是 ORDER BY 的 (sort_order, option_id)，filesort 因此消失。
        // 设计 §5.10：实测端点 375–556 ms → 126–278 ms。**不是硬前提**，
        // 但不加时每请求扫 15 万行的 filesort 在并发下才是真危险的部分。
        .index_named(
            "idx_feishu_option_pick",
            ["source_key", "enabled", "sort_order", "option_id"],
        )
```

> ⚠️ **上面这个形态是错的——`TableSpec::index_named` 收的不是 `&str` 字面量。**
> `["source_key", ...]` 那种写法是 **`Table`（命令式 builder）** 的签名。
> `option/table.rs` 用的是 `TableSpec`，它的 `index_named` 要 **`FieldRef`**：
> 照抄 `demo/notes/table.rs` 的形态（绑 `table_name`、用同一个 `field_ref` helper）。
> 索引名与列清单不变。**以仓库里 `src/addon/feishu/option/table.rs` 的成品为准。**

- [ ] **Step 5: 运行确认通过**

重复 Step 3 的命令，预期 PASS。再跑一次 Schema 集成测试确认没打破别的：

```bash
cargo test --test feishu_options_integration --locked -- --ignored --test-threads=1
```

预期：PASS（它验证 `option_id` 唯一索引等既有事实）。

- [ ] **Step 6: 提交**

```bash
git add src/addon/feishu/option/table.rs tests/feishu_option_index_integration.rs scripts/run_ci.py
git commit -m "perf(feishu): 出站端点复合索引 idx_feishu_option_pick"
```

---

# 阶段一：解析层

纯函数，最好 TDD，也是整条链最大的技术风险。

## Task 6: 表头读取与校验

**Files:**
- Modify: `src/addon/feishu/domain/xlsx.rs`
- Test: `src/addon/feishu/domain/xlsx.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: Task 1 夹具、Task 2 的 `sniff_is_zip`
- Produces:
  ```rust
  /// 一个文件的表头解析结果。
  pub(crate) struct SheetHeader {
      /// 第一张 sheet 的名字。
      pub(crate) sheet_name: String,
      /// 全部 sheet 名（供前端提示「只读了第一张」）。
      pub(crate) sheet_names: Vec<String>,
      /// 表头所在物理行号（1-based）。
      pub(crate) header_row: usize,
      /// 非空表头列的 (列名, 1-based 列号)，按列序。
      pub(crate) columns: Vec<(String, usize)>,
  }

  /// 解析失败的原因。**每一种都要能指名道姓**，不要一个笼统的「格式错误」。
  pub(crate) enum XlsxError {
      NotZip,
      Unreadable(String),
      EmptyHeader,
      DuplicateColumn { name: String },
      ColumnTooLong { name: String, limit: usize },
  }

  pub(crate) fn read_header(bytes: &[u8]) -> Result<SheetHeader, XlsxError>;

  /// 校验一组文件的表头**集合完全一致**（顺序可不同——按名取值，不按位置）。
  /// 不一致时点名哪个文件差哪些列。
  pub(crate) fn require_consistent_headers(
      headers: &[(String, SheetHeader)],
  ) -> Result<Vec<String>, XlsxError>;
  ```

- [ ] **Step 1: 写失败的表头测试**

在 `xlsx.rs` 的测试模块里追加。**每条测试对应 Review Focus 里的一类输入**：

```rust
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/xlsx")
                .join(name),
        )
        .unwrap_or_else(|error| panic!("读夹具 {name} 失败: {error}"))
    }

    #[test]
    fn reads_the_header_row_by_name_and_position() {
        let header = read_header(&fixture("bank_1.xlsx"))
            .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert_eq!(header.sheet_name, "境内银行网点信息管理");
        assert_eq!(header.header_row, 1);
        assert_eq!(
            header.columns,
            vec![
                ("序号".to_string(), 1),
                ("开户行行名".to_string(), 2),
                ("归属银行".to_string(), 3),
                ("归属银行编码".to_string(), 4),
                ("联行号".to_string(), 5),
                ("开户行地址".to_string(), 6),
                ("地区名称".to_string(), 7),
                ("地区编码".to_string(), 8),
            ],
            "列名与 1-based 列号都要对"
        );
    }

    #[test]
    fn rejects_bytes_that_are_not_a_zip() {
        let error = read_header(&fixture("not_a_zip.bin"))
            .err()
            .unwrap_or_else(|| panic!("非 zip 必须被拒"));
        assert!(matches!(error, XlsxError::NotZip));
    }

    #[test]
    fn rejects_a_duplicate_column_name() {
        // 列名是身份；重名则身份不唯一，必须拒绝而不是取第一个。
        let error = read_header(&fixture("header_duplicate.xlsx"))
            .err()
            .unwrap_or_else(|| panic!("重名表头必须被拒"));
        assert!(
            matches!(&error, XlsxError::DuplicateColumn { name } if name == "联行号"),
            "错误里要点名是哪一列重名，实际: {error:?}"
        );
    }

    #[test]
    fn skips_a_blank_column_name() {
        // 空名不是有效身份，跳过（而不是当成一列名叫空串的列）。
        let header = read_header(&fixture("header_blank.xlsx"))
            .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert!(
            header.columns.iter().all(|(name, _)| !name.trim().is_empty()),
            "空表头列不该出现在结果里"
        );
        assert_eq!(header.columns.len(), 7, "8 列里有一列表头为空");
    }

    #[test]
    fn rejects_a_column_name_longer_than_the_field_id_limit() {
        // field_id 是 max_length(64) 且应用层不校验长度——不在这里挡住，
        // 就要等插库时才炸，而那时错误信息不会告诉你「是列名太长」。
        let error = read_header(&fixture("header_long.xlsx"))
            .err()
            .unwrap_or_else(|| panic!("65 字符的列名必须被拒"));
        assert!(
            matches!(&error, XlsxError::ColumnTooLong { limit: 64, .. }),
            "实际: {error:?}"
        );
    }

    #[test]
    fn trims_surrounding_whitespace_in_column_names() {
        // 表格里肉眼看不见的首尾空格极常见，trim 后精确匹配是 D9 的口径。
        let header = read_header(&fixture("header_spaces.xlsx"))
            .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert_eq!(header.columns[0].0, "序号", "表头里的空白要被 trim 掉");
        assert_eq!(header.columns[1].0, "开户行行名");
    }

    #[test]
    fn reads_only_the_first_sheet_but_reports_all_names() {
        let header = read_header(&fixture("two_sheets.xlsx"))
            .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert_eq!(header.sheet_name, "第一张");
        assert_eq!(
            header.sheet_names,
            vec!["第一张".to_string(), "第二张".to_string()],
            "全部 sheet 名要带出来，供前端提示「只读了第一张」"
        );
        assert!(
            header.columns.iter().any(|(name, _)| name == "联行号"),
            "读的是第一张 sheet 的表头"
        );
    }

    #[test]
    fn consistent_headers_pass_regardless_of_column_order() {
        let a = read_header(&fixture("bank_1.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let b = read_header(&fixture("bank_2.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let names = require_consistent_headers(&[("bank_1.xlsx".into(), a), ("bank_2.xlsx".into(), b)])
            .unwrap_or_else(|error| panic!("同表头应通过: {error}"));
        assert!(names.contains(&"联行号".to_string()));
    }

    #[test]
    fn inconsistent_headers_name_the_offending_file_and_columns() {
        // Review Focus 第 2 条：不能取交集，也不能只导能对上的那个文件——
        // 那会让用户以为导了 10 万行、实际只有 5 万，且没有任何提示。
        let a = read_header(&fixture("bank_1.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let b = read_header(&fixture("header_mismatch_two.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let error = require_consistent_headers(&[
            ("bank_1.xlsx".into(), a),
            ("header_mismatch_two.xlsx".into(), b),
        ])
        .err()
        .unwrap_or_else(|| panic!("表头不一致必须整份拒绝"));
        let message = format!("{error:?}");
        assert!(message.contains("header_mismatch_two.xlsx"), "要点名文件: {message}");
        assert!(message.contains("归属银行"), "要点名缺哪列: {message}");
    }
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：编译失败（`read_header` / `XlsxError` / `SheetHeader` 未定义）。

- [ ] **Step 3: 实现**

在 `xlsx.rs` 里实现。要点逐条：

> ⚠️ **下面代码里有一处真错，实施时实测发现**：第 1663 行那句
> `if row_number <= current_row { continue; }` **是错的**——`current_row` 是**随行推进**的变量，
> 于是每行第二个及以后的单元格全被跳过，**只有第一列能被读到**。正确写法是
> `if row_number <= header.header_row { continue; }`（注释「表头行及其之前」才是原意）。
>
> **这一处的危险在于它很安静**：草稿自带的 7 条测试里 4 条会因此转红，但**如果你要的列恰好只有第一列，
> 全部测试照样绿**。以仓库里的 `src/addon/feishu/domain/xlsx.rs` 成品为准。

> ⚠️ **下面这段代码块里的 calamine API 是错的**（实施时实测，三处）：
> ① `calamine::XlsxCell` **不存在**——`next_cell()` 返回的是 `Cell<DataRef<'a>>`；
> ② 没有 `cell.value()`，只有 **`get_value()`**；
> ③ **`DataRef` 比 `Data` 多一个 `SharedString(&str)` 变体**，照抄下面的 `Data` match
> **会因不完备而编译不过**——而那个多出来的臂**恰好是真实 Excel 导出的主路径**
> （默认走 `sharedStrings.xml`，`t="s"`），所以它必须被夹具覆盖到，不能只是「为编译而写」。
>
> **以仓库里的 `src/addon/feishu/domain/xlsx.rs` 成品为准**；下面只表达意图与不变量
> （R7 不加 trim 补偿、数值走 `to_string()` 不走 `{:?}`）。


```rust
use calamine::{Data, Reader as _, Xlsx};

pub(crate) const COLUMN_NAME_LIMIT: usize = 64;

pub(crate) fn read_header(bytes: &[u8]) -> Result<SheetHeader, XlsxError> {
    if !sniff_is_zip(bytes) {
        return Err(XlsxError::NotZip);
    }
    // calamine 只能从 Read + Seek 构造。文件是请求作用域的临时文件，
    // 但这一层收 &[u8] 以便单测直接喂夹具字节。
    let cursor = std::io::Cursor::new(bytes);
    let mut workbook = Xlsx::new(cursor).map_err(|error| XlsxError::Unreadable(error.to_string()))?;

    let sheet_names = workbook.sheet_names().to_vec();
    let sheet_name = sheet_names.first().cloned().ok_or(XlsxError::EmptyHeader)?;

    // **只读表头行，不扫全表**——probe Action 快就快在这里。
    let mut reader = workbook
        .worksheet_cells_reader(&sheet_name)
        .map_err(|error| XlsxError::Unreadable(error.to_string()))?;

    let mut columns: Vec<(String, usize)> = Vec::new();
    let mut header_row = 0usize;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // XlsxCellReader 是**手拉游标、不实现 Iterator**，必须 while let 驱动。
    while let Some(cell) = reader.next_cell().map_err(|error| XlsxError::Unreadable(error.to_string()))? {
        let (row, column) = (cell.get_position().0, cell.get_position().1);
        let text = cell_text(cell);
        let text = text.trim();

        if text.is_empty() {
            continue;
        }
        if header_row == 0 {
            header_row = row as usize + 1; // 0-based → 1-based
        } else if row as usize + 1 != header_row {
            break; // 表头行读完了
        }

        if text.chars().count() > COLUMN_NAME_LIMIT {
            return Err(XlsxError::ColumnTooLong {
                name: text.to_string(),
                limit: COLUMN_NAME_LIMIT,
            });
        }
        if !seen.insert(text.to_string()) {
            return Err(XlsxError::DuplicateColumn { name: text.to_string() });
        }
        columns.push((text.to_string(), column as usize + 1)); // 1-based 列号
    }

    if columns.is_empty() {
        return Err(XlsxError::EmptyHeader);
    }
    Ok(SheetHeader { sheet_name, sheet_names, header_row, columns })
}
```

`cell_text` 把 `Data` 转成 `String`——**这里就是「数值型单元格不推断」的落点**：

```rust
/// 单元格取文本。**不做数值/日期推断**（设计 §5.7）：
/// 数值原样按 `Data` 的显示取，`联行号` 若被 Excel 存成数值本就会丢前导零，
/// 那是源文件的错，导入器不做「看起来像数字就补零」这类猜测。
fn cell_text(cell: calamine::XlsxCell<'_>) -> String {
    match cell.value() {
        Data::String(text) => text.clone(),
        Data::Float(number) => number.to_string(),
        Data::Int(number) => number.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::DateTime(value) => value.to_string(),
        Data::Empty | Data::Error(_) | Data::DateTimeIso(_) | Data::DurationIso(_) => String::new(),
    }
}
```

`require_consistent_headers`：

```rust
pub(crate) fn require_consistent_headers(
    headers: &[(String, SheetHeader)],
) -> Result<Vec<String>, XlsxError> {
    let Some((_, first)) = headers.first() else {
        return Err(XlsxError::EmptyHeader);
    };
    let reference: std::collections::BTreeSet<&str> =
        first.columns.iter().map(|(name, _)| name.as_str()).collect();

    for (file, header) in headers.iter().skip(1) {
        let actual: std::collections::BTreeSet<&str> =
            header.columns.iter().map(|(name, _)| name.as_str()).collect();
        if actual != reference {
            let missing: Vec<&str> = reference.difference(&actual).copied().collect();
            let extra: Vec<&str> = actual.difference(&reference).copied().collect();
            return Err(XlsxError::HeadersDiffer { file: file.clone(), missing: missing.iter().map(|s| s.to_string()).collect(), extra: extra.iter().map(|s| s.to_string()).collect() });
        }
    }
    Ok(reference.into_iter().map(str::to_string).collect())
}
```

同时把 `XlsxError` 补全，并给它实现 `std::fmt::Display`（错误文案要能直接给用户看）：

```rust
pub(crate) enum XlsxError {
    NotZip,
    Unreadable(String),
    EmptyHeader,
    DuplicateColumn { name: String },
    ColumnTooLong { name: String, limit: usize },
    HeadersDiffer { file: String, missing: Vec<String>, extra: Vec<String> },
}

impl std::fmt::Display for XlsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotZip => write!(f, "文件不是 xlsx（魔数不是 PK\\x03\\x04）"),
            Self::Unreadable(reason) => write!(f, "文件读不出来：{reason}"),
            Self::EmptyHeader => write!(f, "表头是空的"),
            Self::DuplicateColumn { name } => write!(f, "表头有重名列「{name}」——列名是身份，不能重复"),
            Self::ColumnTooLong { name, limit } => {
                write!(f, "列名「{name}」超过 {limit} 字符上限")
            }
            Self::HeadersDiffer { file, missing, extra } => {
                write!(f, "文件 {file} 的表头与其它文件不一致：缺 {:?}、多 {:?}", missing, extra)
            }
        }
    }
}
```

- [ ] **Step 4: 运行确认全部通过**

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：全部 PASS（**11 条**表头测试 + Task 2 的 2 条嗅探测试 = 13 条）。
其中两条是补测加的：sharedStrings 与 inlineStr 解析结果**相等**、列重排也能通过一致性校验。

- [ ] **Step 5: 提交**

```bash
git add src/addon/feishu/domain/xlsx.rs
git commit -m "feat(feishu): xlsx 表头读取与校验（重名/空名/超长/多文件一致）"
```

---

## Task 7: 行迭代与按列名取值

**这一层完全不知道「绑定」的存在**——它只认列名。要哪些列、哪些行怎么投影成
`RawValue`，是 Task 10（导入 Action）的事。分层清楚，这一层才能被完整单测覆盖。

**Files:**
- Modify: `src/addon/feishu/domain/xlsx.rs`
- Test: `src/addon/feishu/domain/xlsx.rs`

**Interfaces:**
- Consumes: Task 6 的 `read_header` / `SheetHeader`
- Produces:
  ```rust
  /// 一行：列名 → 单元格文本。只保留 `columns` 里要的列。
  pub(crate) type RowValues = std::collections::HashMap<String, String>;

  /// 一个文件读了多少行（导入回执要按文件报）。
  pub(crate) struct FileRows {
      pub(crate) name: String,
      pub(crate) rows_read: usize,
  }

  /// 一份快照：多个文件**按顺序拼接**的全部行。
  pub(crate) struct ImportSnapshot {
      pub(crate) rows: Vec<RowValues>,
      pub(crate) per_file: Vec<FileRows>,
  }

  /// 读全部数据行。`columns` 是要取回的列名集合（= 全部启用绑定的 `field_id`）。
  /// 表头行本身不算数据行。整行全空的行跳过。
  pub(crate) fn read_snapshot(
      files: &[(String, Vec<u8>)],
      columns: &[String],
  ) -> Result<ImportSnapshot, XlsxError>;
  ```

- [ ] **Step 1: 写失败的行读取测试**

Review Focus 第 3 条（数值型单元格）在这一步钉死：

```rust
    #[test]
    fn reads_data_rows_keyed_by_column_name() {
        let want = vec!["开户行行名".to_string(), "联行号".to_string()];
        let snapshot = read_snapshot(
            &[("bank_1.xlsx".to_string(), fixture("bank_1.xlsx"))],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));

        assert_eq!(snapshot.rows.len(), 3, "bank_1 有 3 个数据行");
        assert_eq!(snapshot.per_file[0].rows_read, 3);
        assert_eq!(
            snapshot.rows[0].get("开户行行名").map(String::as_str),
            Some("中国工商银行成都春熙路支行")
        );
        assert_eq!(
            snapshot.rows[0].get("联行号").map(String::as_str),
            Some("102651000011")
        );
    }

    #[test]
    fn only_the_requested_columns_are_kept() {
        let want = vec!["联行号".to_string()];
        let snapshot =
            read_snapshot(&[("bank_1.xlsx".to_string(), fixture("bank_1.xlsx"))], &want)
                .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert!(
            !snapshot.rows[0].contains_key("开户行行名"),
            "没要的列不该留在内存里——15.4 万行时这是实打实的内存"
        );
    }

    #[test]
    fn numeric_cells_are_not_guessed_into_zero_padded_strings() {
        // Review Focus 第 3 条：Excel 把 `联行号` 存成数值时，前导零已经丢了。
        // 导入器**不做**「看起来像数字就补零」这类猜测——那是源文件的错。
        // 这条测试的价值是**把行为钉住**：读出来就是数值的原样文本，
        // 不是 `102651000011`（补零）也不是 `1.02651000011E11`（科学计数）。
        let want = vec!["联行号".to_string()];
        let snapshot = read_snapshot(
            &[("numeric_code.xlsx".to_string(), fixture("numeric_code.xlsx"))],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));
        let value = snapshot.rows[0].get("联行号").map(String::as_str).unwrap_or("");
        assert!(!value.is_empty(), "数值单元格不应读成空串");
        assert!(
            !value.contains('E') && !value.contains('e'),
            "不该出现科学计数法，实际 {value:?}"
        );
    }

    #[test]
    fn multiple_files_are_concatenated_in_order() {
        let want = vec!["序号".to_string()];
        let snapshot = read_snapshot(
            &[
                ("bank_1.xlsx".to_string(), fixture("bank_1.xlsx")),
                ("bank_2.xlsx".to_string(), fixture("bank_2.xlsx")),
            ],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));

        assert_eq!(snapshot.rows.len(), 5, "3 + 2");
        assert_eq!(snapshot.per_file.len(), 2);
        assert_eq!(snapshot.per_file[0].name, "bank_1.xlsx");
        assert_eq!(snapshot.per_file[0].rows_read, 3);
        assert_eq!(snapshot.per_file[1].rows_read, 2);
        assert_eq!(
            snapshot.rows[0].get("序号").map(String::as_str),
            Some("1"),
            "拼接顺序必须是传入顺序——序号是接续的两半"
        );
        assert_eq!(snapshot.rows[4].get("序号").map(String::as_str), Some("5"));
    }

    #[test]
    fn a_header_only_file_yields_zero_rows() {
        let want = vec!["联行号".to_string()];
        let snapshot = read_snapshot(
            &[("only_header.xlsx".to_string(), fixture("only_header.xlsx"))],
            &want,
        )
        .unwrap_or_else(|error| panic!("只有表头应能解析: {error}"));
        assert!(snapshot.rows.is_empty());
        assert_eq!(snapshot.per_file[0].rows_read, 0);
    }

    #[test]
    fn a_missing_wanted_column_is_reported_by_name() {
        // 夹具 `header_missing_column.xlsx` 没有 `联行号` 列，
        // 但我们要取它 —— 必须报出来，而不是静默读成空串。
        let want = vec!["开户行行名".to_string(), "联行号".to_string()];
        let error = read_snapshot(
            &[("header_missing_column.xlsx".to_string(), fixture("header_missing_column.xlsx"))],
            &want,
        )
        .err()
        .unwrap_or_else(|| panic!("缺列必须被拒"));
        assert!(
            matches!(&error, XlsxError::HeaderMissingColumns { .. }),
            "实际: {error:?}"
        );
        assert!(format!("{error}").contains("联行号"), "要点名缺哪列");
    }

    #[test]
    fn extra_columns_are_ignored() {
        // D9：缺列即拒，**多列忽略**——用户在同一份文件里加了别的列是无害的。
        let want = vec!["开户行行名".to_string(), "联行号".to_string()];
        let snapshot = read_snapshot(
            &[("header_extra_column.xlsx".to_string(), fixture("header_extra_column.xlsx"))],
            &want,
        )
        .unwrap_or_else(|error| panic!("多列应被忽略: {error}"));
        assert_eq!(snapshot.rows.len(), 1);
    }
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：编译失败（`read_snapshot` / `ImportSnapshot` 未定义）。

- [ ] **Step 3: 实现**

先给 `XlsxError` 加一个变体：

```rust
    /// 表头里缺了我们需要的列。`wanted` 是缺的那些（不是全部要的）。
    HeaderMissingColumns { file: String, missing: Vec<String> },
```

`Display` 里加：

```rust
            Self::HeaderMissingColumns { file, missing } => {
                write!(f, "文件 {file} 的表头缺少必需列：{}", missing.join("、"))
            }
```

`read_snapshot` 的实现要点（**这些要点每一条都是坑，照写**）：

```rust
pub(crate) fn read_snapshot(
    files: &[(String, Vec<u8>)],
    columns: &[String],
) -> Result<ImportSnapshot, XlsxError> {
    let wanted: std::collections::HashSet<&str> = columns.iter().map(String::as_str).collect();
    let mut rows = Vec::new();
    let mut per_file = Vec::with_capacity(files.len());

    for (name, bytes) in files {
        // 每个文件都重新解析表头：文件之间的一致性由调用方用
        // `require_consistent_headers` 单独把守（那一层的错误文案更具体）。
        let header = read_header(bytes)?;

        let present: std::collections::HashSet<&str> =
            header.columns.iter().map(|(n, _)| n.as_str()).collect();
        let missing: Vec<String> = columns
            .iter()
            .filter(|column| !present.contains(column.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(XlsxError::HeaderMissingColumns { file: name.clone(), missing });
        }

        let cursor = std::io::Cursor::new(bytes.as_slice());
        let mut workbook =
            Xlsx::new(cursor).map_err(|error| XlsxError::Unreadable(error.to_string()))?;
        let mut reader = workbook
            .worksheet_cells_reader(&header.sheet_name)
            .map_err(|error| XlsxError::Unreadable(error.to_string()))?;

        // 列号 → 列名。用 1-based 列号做键，因为游标只给位置不给名字。
        let by_index: std::collections::HashMap<usize, &str> = header
            .columns
            .iter()
            .map(|(column_name, index)| (*index, column_name.as_str()))
            .collect();

        let mut current_row = header.header_row; // 表头行本身不算数据行
        let mut current: RowValues = RowValues::new();
        let mut rows_read = 0usize;
        let mut pending = false;

        // **XlsxCellReader 是手拉游标、不实现 Iterator**，必须 while let 驱动。
        while let Some(cell) = reader
            .next_cell()
            .map_err(|error| XlsxError::Unreadable(error.to_string()))?
        {
            let (row_index, column_index) = cell.get_position();
            let row_number = row_index as usize + 1;
            let column_number = column_index as usize + 1;

            if row_number <= current_row {
                continue; // 表头行及其之前
            }
            if row_number != current_row {
                // 换行了：把上一行收口。
                if pending {
                    rows_read += 1;
                    rows.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                current_row = row_number;
                pending = false;
            }

            let Some(column_name) = by_index.get(&column_number) else {
                continue; // 没勾选的列，直接不进内存
            };
            // 没要的列也跳过（by_index 已按 header.columns 过滤，双保险）
            if !wanted.contains(*column_name) {
                continue;
            }

            let text = cell_text(cell).trim().to_string();
            if text.is_empty() {
                continue; // 空单元格不进 map —— 省内存，且"缺值"与"空串"同义
            }
            pending = true;
            current.insert((*column_name).to_string(), text);
        }

        // 最后一个文件最后一行也要收口（`while let` 退出时没有"换行"事件）。
        if pending {
            rows_read += 1;
            rows.push(current);
        }

        per_file.push(FileRows { name: name.clone(), rows_read });
    }

    Ok(ImportSnapshot { rows, per_file })
}
```

> **整行全空的行怎么处理的**：它不会置 `pending`，所以换行时被丢掉、不计入
> `rows_read`。这是刻意的——xlsx 文件末尾常有大量空行，把它们当数据行会让
> 「0 行」和「10 万行空行」无法区分，而空快照守卫依赖这个区分。

- [ ] **Step 4: 运行确认全部通过**

```bash
cargo test --lib --locked feishu::domain::xlsx
```

预期：全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/addon/feishu/domain/xlsx.rs
git commit -m "feat(feishu): xlsx 行读取与多文件拼接（表头行不计入、不推断数值）"
```

---

# 阶段二：Action

## Task 8: 建源时写 `field_name`（**blocking 修复**）

**这是整个计划里唯一一条「不改就一定出事故」的修复，且它与导入功能可以独立验收。**

背景：`approval_provision` 对**每一条启用绑定**执行 `require("field_name")`
（`approval_provision.rs:487`）。而建绑定的两条路径
（`create_datasource_table` / `update_datasource_table`）**从不写 `field_name`**
——`FieldBindingInput` 里根本没有这个字段，全仓唯一写它的是 `pull.rs` 的
`persist_binding`，而那只服务 `ingest_mode = "pull"`。

`field_name` 列可空且无默认值，所以 xlsx 绑定会恒为 NULL →
装配时 `require` 走 `BaseError::InvalidFieldType` → 变成 `ProvisionError::Store`
→ **审批外部选项装配整批失败**，而且那个函数扫全表，**失败范围不限于本数据源**。

**契约测试抓不到它**：`feishu-projections.json` 的 `binding.emitted` 已经含
`field_name`，测试全绿——**测试绿不等于功能对**。

**Files:**
- Modify: `src/addon/feishu/domain/field_binding.rs`（`FieldBindingInput` 加可选字段）
- Modify: `src/addon/feishu/datasource/actions/create_datasource_table.rs:169-201`
- Modify: `src/addon/feishu/datasource/actions/update_datasource_table.rs:242-266`（新增分支）
- Modify: `src/addon/feishu/domain/approval_provision.rs`（把 `require` 换成带上下文的判定）

**Interfaces:**
- Consumes: 无
- Produces: `FieldBindingInput` 多一个
  `pub(crate) field_name: Option<String>`（`#[serde(default)]`），
  Task 10 与前端的建源请求都会带上它。

- [ ] **Step 1: 写失败的回归测试**

在 `field_binding.rs` 的测试模块里加：

```rust
    #[test]
    fn a_binding_may_carry_its_column_name() {
        // xlsx 导入的绑定没有「多维表格字段 ID」可解析，列名就是身份。
        // 建源时必须把它同时写进 field_id 与 field_name —— 只写前者会让
        // approval_provision 的 require("field_name") 失败，整批装配挂掉。
        let json = serde_json::json!({
            "field_id": "开户行行名",
            "field_name": "开户行行名",
            "source_key": "bank_branch_name",
            "parent_field_id": null,
        });
        let input: FieldBindingInput =
            serde_json::from_value(json).unwrap_or_else(|error| panic!("应可反序列化: {error}"));
        assert_eq!(input.field_name.as_deref(), Some("开户行行名"));
    }

    #[test]
    fn a_binding_without_a_column_name_still_parses() {
        // 多维表格那条路不给 field_name（它由 pull 每轮解析回写），
        // 所以这个字段必须是可选的，不能把它变成必填。
        let json = serde_json::json!({
            "field_id": "fldXXXXXXXX",
            "source_key": "payment_currency",
        });
        let input: FieldBindingInput =
            serde_json::from_value(json).unwrap_or_else(|error| panic!("应可反序列化: {error}"));
        assert!(input.field_name.is_none());
    }
```

在 `approval_provision.rs` 的测试模块里加一条（**这条是真正的端到端回归**）：

```rust
    #[test]
    fn a_binding_row_with_a_blank_name_is_skipped_not_fatal() {
        // 装配扫的是**全表**的启用绑定，所以任何一条名字为空的绑定
        // 都会把整批装配打挂——包括与本数据源无关的那些。
        // 空名/缺名应当**跳过这一条**（它本来也无从匹配），而不是让整个
        // 建配置流程失败。
        use yang_base::table::Record;
        let mut row = Record::new();
        row.insert("field_name", serde_json::json!(null));
        row.insert("source_key", serde_json::json!("orphan"));
        row.insert("enabled", serde_json::json!(true));
        assert_eq!(
            binding_display_name(&row).unwrap_or_else(|error| panic!("{error}")),
            None,
            "名字为空或缺失的绑定应被跳过"
        );
    }
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked field_binding::tests
cargo test --lib --locked approval_provision::tests
```

预期：第一条编译失败（`FieldBindingInput` 没有 `field_name`），
第二条编译失败（`binding_display_name` 不存在）。

- [ ] **Step 3: 让输入结构体接受列名**

`src/addon/feishu/domain/field_binding.rs`：

```rust
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldBindingInput {
    /// 多维表格字段 ID，或（xlsx 导入时）**列名**。**身份就是它**，改名不能断链。
    pub(crate) field_id: String,
    /// 展示用名字。多维表格那条路留空（由 `pull` 每轮解析回写）；
    /// **xlsx 导入必须给**，与 `field_id` 同值。
    ///
    /// 为什么必须给：`approval_provision` 对每条启用绑定 `require("field_name")`，
    /// 而列可空无默认值——留空会让审批外部选项**整批装配失败**（且失败范围是全表）。
    #[serde(default)]
    pub(crate) field_name: Option<String>,
    /// 进 URL 路径段的数据源标识；全局唯一、创建后不可改。
    pub(crate) source_key: String,
    /// 同表内的父列 `field_id`；无父给 `null` 或省略。
    #[serde(default)]
    pub(crate) parent_field_id: Option<String>,
}
```

- [ ] **Step 4: 建源与更新的新增分支写入它**

`create_datasource_table.rs` 的绑定构造里，在 `field_id` 之后加一行：

```rust
            binding.insert("field_id", serde_json::json!(field.field_id.trim()));
            // 两个都写：`field_id` 是身份，`field_name` 是审批装配要的名字。
            // 留空会让 approval_provision 的 require("field_name") 打挂整批装配。
            if let Some(name) = field
                .field_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                binding.insert("field_name", serde_json::json!(name));
            }
            binding.insert("source_key", serde_json::json!(&source_key));
```

`update_datasource_table.rs` 的「新增绑定」分支里同样加一段（在那个分支的
`binding.insert("field_id", ...)` 之后）。

> **`update` 的「更新已有绑定」分支不动**：它刻意只改 `enabled` 与
> `parent_field_id`，注释里写着碰 `source_key`/凭据等于换 URL、飞书侧已配控件全断。
> `field_name` 也不在那个分支里改——列名变了要重新走一遍配置，见 Task 14 的说明。

> ⚠️ **给 `FieldBindingInput` 加字段会打断所有构造点。** 这个结构体没有
> `#[serde(default)]` 之外的兜底，加了 `field_name` 之后，**所有用结构体字面量
> 构造它的地方都要补一行**。Task 4 刚在这两个文件的测试模块里加了新用例
> （`accepts_the_xlsx_import_mode`），它用的是现成的 `valid_input()` helper——
> 如果那个 helper 里手写了结构体字面量，**这一步要把它一并补齐**。
> `cargo test --lib --locked feishu` 会直接报缺字段，照着编译器给的位置补即可。

- [ ] **Step 5: 让装配对空名宽容**

`approval_provision.rs` 里，把那段 `require("field_name")` 抽成一个可测的小函数，
并改成「缺失或空 → 跳过这一条」：

```rust
/// 取一条绑定的展示名。**缺失或空白一律返回 `None`（跳过），不报错。**
///
/// 为什么不能 `require`：这个函数扫的是**全表**的启用绑定，任何一条名字为空的
/// 绑定（历史数据、或建源时漏写了 `field_name` 的 xlsx 绑定）都会让
/// 「首次派发自动建配置」整批失败。跳过它只是少一个候选列，
/// 而整批失败是功能不可用——两者代价差一个量级。
fn binding_display_name(binding: &Record) -> Result<Option<String>, BaseError> {
    let Some(value) = binding.get("field_name") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let name: String = binding
        .optional("field_name")?
        .unwrap_or_default();
    let name = name.trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(name))
}
```

把调用点（原 `let name: String = binding.require("field_name")?;` 那一段）改成：

```rust
        let Some(name) = binding_display_name(binding)? else {
            // 名字为空/缺失：这条绑定无从参与按名匹配，跳过。
            // 上游的 Task 8 让建源写上 field_name，这里是它失守时的兜底。
            tracing::warn!(
                source_key = %binding.optional::<String>("source_key")?.unwrap_or_default(),
                "字段绑定的 field_name 为空，跳过它——列名是身份，没有名字就无法按名匹配"
            );
            continue;
        };
```

> `binding.get` 与 `binding.optional` 都要求 `Record` 有对应方法；
> 先读一眼同文件里现成的取值写法（`binding.optional("enabled")?`）照着来。

- [ ] **Step 6: 运行确认通过**

```bash
cargo test --lib --locked feishu
```

预期：全 PASS，含 Task 8 新增的三条。

- [ ] **Step 7: 确认既有的契约测试仍然绿**

```bash
cargo test --lib --locked projection
pnpm --dir frontend test -- features/feishu/api.test.ts
```

预期：PASS。**注意这一步是绿的并不证明修复有效**——契约本来就已声明
`field_name` 是 emitted 键，测试对「值为 NULL」无感。真正的回归在第 1 步那三条。

- [ ] **Step 8: 提交**

```bash
git add src/addon/feishu/domain/field_binding.rs \
        src/addon/feishu/domain/approval_provision.rs \
        src/addon/feishu/datasource/actions/create_datasource_table.rs \
        src/addon/feishu/datasource/actions/update_datasource_table.rs
git commit -m "fix(feishu): 建绑定写 field_name，并让装配对空名宽容而非整批失败"
```

---

## Task 9: 探表头 Action

**Files:**
- Create: `src/addon/feishu/datasource/actions/probe_xlsx_headers.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`（声明 + 注册）

**Interfaces:**
- Consumes: Task 6 的 `read_header` / `require_consistent_headers`
- Produces: `POST /api/v1/feishu/datasources/xlsx/probe`，响应形状
  ```json
  { "sheet_name": "…", "sheets": ["…"], "header_row": 1,
    "columns": [{"name": "开户行行名", "index": 2}],
    "files": [{"name": "bank_1.xlsx"}] }
  ```
  Task 13 的前端向导消费它。

- [ ] **Step 1: 写失败的 Action 测试**

在新建的 `probe_xlsx_headers.rs` 底部：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/xlsx")
                .join(name),
        )
        .unwrap_or_else(|error| panic!("读夹具 {name} 失败: {error}"))
    }

    #[test]
    fn a_request_with_no_files_is_rejected() {
        // 一个文件都没有时「表头一致」是空真——必须显式拒绝，
        // 否则前端会拿到一个空列名列表并渲染出一个什么都选不了的向导。
        let input = ProbeInput { files: Vec::new() };
        assert!(input.validate().is_err());
    }

    #[test]
    fn the_response_carries_every_column_with_its_one_based_index() {
        let input = ProbeInput {
            files: vec![UploadedFile::for_test("bank_1.xlsx", fixture("bank_1.xlsx"))],
        };
        let payload = probe(&input).unwrap_or_else(|error| panic!("应能探测: {error}"));
        assert_eq!(payload["sheet_name"], "境内银行网点信息管理");
        assert_eq!(payload["header_row"], 1);
        let columns = payload["columns"].as_array().unwrap_or_else(|| panic!("columns 应存在"));
        assert_eq!(columns.len(), 8, "**全部列**都要带出来，不做任何类型/语义过滤");
        assert_eq!(columns[1]["name"], "开户行行名");
        assert_eq!(columns[1]["index"], 2, "1-based 物理列号");
    }

    #[test]
    fn inconsistent_files_are_rejected_as_a_whole() {
        let input = ProbeInput {
            files: vec![
                UploadedFile::for_test("bank_1.xlsx", fixture("bank_1.xlsx")),
                UploadedFile::for_test("header_mismatch_two.xlsx", fixture("header_mismatch_two.xlsx")),
            ],
        };
        let error = input.validate_shape_only().err();
        // 一致性校验要读文件，所以它在 handle 里而不是 validate 里；
        // 这条测试直接打 probe()，断言整份拒绝。
        assert!(error.is_none(), "形状校验本身应通过（文件非空）");
        let error = probe(&input).err().unwrap_or_else(|| panic!("表头不一致必须整份拒绝"));
        assert!(error.to_string().contains("header_mismatch_two.xlsx"));
    }
}
```

> `probe(&input)` 是本任务要抽出的**纯函数**（收 `&ProbeInput`，返回
> `Result<serde_json::Value, BaseError>`），handle 只做「读文件字节 → 调 probe → 包 ApiResponse」。
> 这样测试不需要构建 `ActionContext`。
> `UploadedFile::for_test` 不存在——**改成你自己的测试构造函数**：
> `ProbeInput` 在这个模块里不要直接用 `Vec<UploadedFile>`，而是
> `Vec<(String, Vec<u8>)>`（文件名 + 字节），handle 负责从 `UploadedFile` 读字节填进去。
> 这样纯函数层完全不依赖框架的临时文件机制。

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked probe_xlsx_headers
```

预期：编译失败（模块不存在）。

- [ ] **Step 3: 实现**

文件结构照抄同目录的 `list_bitable_fields.rs`（use 段 / Input / `ParamInput` impl /
`validate` / `register` / `handle` 六段，链式顺序 `action_fn → route → display_name
→ description → permissions → register`）。关键差异：

```rust
/// 探测输入。文件字段用 `Vec<UploadedFile>`——框架的临时文件是**请求作用域**的，
/// handler 返回即删，所以必须在 handle 内读完。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeInput {
    /// 上传的 xlsx 文件。**不限条数，按总字节封顶**（见 MultipartSpec）。
    pub(super) files: Vec<UploadedFile>,
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("probe_xlsx_headers"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/xlsx/probe")
        .display_name("解析 xlsx 表头")
        .description("读上传文件的表头（只看第一张 sheet），供配置向导勾列；不写库")
        // 与其余配置类端点同权限。
        .permissions(["feishu.datasource.write"])
        .multipart(
            // **必须显式设上限**：MultipartSpec 默认 max_total_bytes = 32 MiB，
            // 超过 AxumTransportConfig.max_body_bytes 时**进程拒绝启动**（启动期
            // fail-closed）。16 MiB 与 Task 3 抬到的新默认值对齐。
            MultipartSpec::new(["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"])
                .max_fields(1)
                .max_files(32)
                .max_file_bytes(16 * 1024 * 1024)
                .max_total_bytes(16 * 1024 * 1024),
        )
        .register()
}
```

handler 只做三件事：

```rust
pub(super) async fn handle(
    _ctx: ActionContext,
    input: ProbeInput,
    _context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 临时文件是请求作用域的，在这里读完（handler 返回后框架就删）。
    let mut loaded: Vec<(String, Vec<u8>)> = Vec::with_capacity(input.files.len());
    for file in &input.files {
        let name = file.original_filename().to_string();
        let bytes = tokio::fs::read(file.path())
            .await
            .map_err(|error| BaseError::ConfigError(format!("读上传文件 {name} 失败：{error}")))?;
        loaded.push((name, bytes));
    }

    let payload = probe(&loaded)?;
    Ok(ApiResponse::success(payload, "解析成功"))
}
```

纯函数 `probe`：

```rust
/// 探测一组文件的表头。**只读表头，不扫全表**——所以它快。
///
/// 抽成纯函数是为了不依赖 `ActionContext` 就能测。
fn probe(files: &[(String, Vec<u8>)]) -> Result<serde_json::Value, BaseError> {
    if files.is_empty() {
        return Err(BaseError::ParamInvalid("files".to_string(), "至少要上传一个文件".to_string()));
    }

    let mut headers = Vec::with_capacity(files.len());
    for (name, bytes) in files {
        let header = xlsx::read_header(bytes).map_err(|error| {
            BaseError::ParamInvalid("files".to_string(), format!("{name}：{error}"))
        })?;
        headers.push((name.clone(), header));
    }
    // 文件之间表头必须完全一致（集合相同，顺序可不同——按名取值，不按位置）。
    xlsx::require_consistent_headers(&headers)
        .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    let first = &headers[0].1;
    Ok(serde_json::json!({
        "sheet_name": first.sheet_name,
        // 全部 sheet 名带出来：多于一张时前端提示「只读了第一张」。
        "sheets": first.sheet_names,
        "header_row": first.header_row,
        // **全部列**，不做任何类型/语义过滤——哪些列适合当外部选项由人判断
        // （与 list_bitable_fields 的口径一致）。
        "columns": first.columns.iter()
            .map(|(name, index)| serde_json::json!({ "name": name, "index": index }))
            .collect::<Vec<_>>(),
        "files": files.iter().map(|(name, _)| serde_json::json!({ "name": name })).collect::<Vec<_>>(),
    }))
}
```

- [ ] **Step 4: 在 `mod.rs` 声明与注册**

`src/addon/feishu/datasource/actions/mod.rs`：按字母序加 `pub(super) mod probe_xlsx_headers;`，
在 `register_all` 里加一行 `let module = probe_xlsx_headers::register(module, Arc::clone(&context));`。

> **不要在 `register_all` 里给它加 `settings.can_pull()` 门控**——那个门控是给
> **出站**端点用的（探表头不出网），xlsx 导入在没有飞书凭证的环境里也必须可用。

- [ ] **Step 5: 运行测试 + 架构门禁**

```bash
cargo test --lib --locked probe_xlsx_headers
python scripts/check_architecture.py
```

预期：测试 PASS；门禁无输出。**若门禁报「必须恰好定义一个 `pub(super) async fn handle`」**，
检查是不是把 `handle` 写成了 `pub async fn` 或漏了 `async`。

- [ ] **Step 6: 提交**

```bash
git add src/addon/feishu/datasource/actions/probe_xlsx_headers.rs \
        src/addon/feishu/datasource/actions/mod.rs
git commit -m "feat(feishu): 探表头端点（只读表头，含多文件一致性校验）"
```

---

## Task 10: 导入 Action

**Files:**
- Create: `src/addon/feishu/datasource/actions/import_xlsx.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Modify: `src/addon/feishu/domain/pull.rs`（把 `parent_linkage` 提升为 `pub(crate)`）
- Create: `tests/feishu_xlsx_import_integration.rs`
- Modify: `scripts/run_ci.py`（登记集成测试）

**Interfaces:**
- Consumes: Task 7 的 `read_snapshot`、Task 8 的 `field_name`、`pull.rs` 的
  `parent_linkage` / `Linkage`、`option_write.rs` 的
  `apply_option_rows` / `disable_option_rows` / `find_foreign_option_owner` / `count_option_rows`、
  `derive.rs` 的 `derive_options` / `snapshot_digest` / `RawValue`
- Produces: `POST /api/v1/feishu/datasources/{datasource_id}/import`

- [ ] **Step 1: 先让 `parent_linkage` 可复用**

`src/addon/feishu/domain/pull.rs`：把 `fn parent_linkage(` 改成 `pub(crate) fn parent_linkage(`。

它接受 `&BoundField` + `&[TableBinding]` + `&[BoundField]`，返回 `Option<Linkage>`，
**带 `visited` 防环**。把它复制一份到导入路径等于复制那条防环守卫——手工改库造出的
`a→b→a` 会在一处被挡、在另一处把请求卡死。

```bash
cargo test --lib --locked feishu::domain::pull
```

预期：PASS（只是放宽可见性，行为不变）。

- [ ] **Step 2: 写失败的集成测试**

创建 `tests/feishu_xlsx_import_integration.rs`。**照抄
`tests/feishu_approval_options_integration.rs` 的三件套**（`build_feishu_app` /
`action_handle` / `dispatch`）与播种 helper，**不要照抄 `feishu_options_integration.rs`**
（那个只做 Schema 级验证、不建上下文、不调 Action）。

必测项（每条对应设计里的一处约束）。先说清共享的种子 helper——
**照抄 `tests/feishu_approval_options_integration.rs` 的 `seed_binding` 形态**，
但要多写一个 `field_name` 列（Task 8 之后它是必需的）：

```rust
const DATASOURCE_TOKEN: &str = "xlsx-import-token";

/// 种一条 xlsx 导入的数据源 + N 条绑定，返回 datasource_id。
/// `fields` 是 `(列名, source_key, 父列名)` —— **列名同时进 field_id 与 field_name**。
async fn seed_xlsx_datasource(
    database: &Database,
    ingest_mode: &str,
    fields: &[(&str, &str, Option<&str>)],
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO `feishu_datasource` (`title`, `status`, `ingest_mode`, `created_at`, `updated_at`) \
         VALUES (?, 'active', ?, NOW(), NOW())",
    )
    .bind("xlsx 夹具源")
    .bind(ingest_mode)
    .execute(database.pool())
    .await
    .context("写入表级数据源失败")?;
    let datasource_id = result.last_insert_id() as i64;

    for (index, (column, source_key, parent)) in fields.iter().enumerate() {
        sqlx::query(
            "INSERT INTO `feishu_datasource_field` \
             (`datasource_id`, `field_id`, `field_name`, `source_key`, `token_hash`, `parent_field_id`, \
              `enabled`, `created_at`, `updated_at`) \
             VALUES (?, ?, ?, ?, SHA2(?, 256), ?, 1, NOW(), NOW())",
        )
        .bind(datasource_id)
        .bind(column)
        .bind(column)
        .bind(source_key)
        .bind(format!("{DATASOURCE_TOKEN}-{index}"))
        .bind(parent)
        .execute(database.pool())
        .await
        .with_context(|| format!("写入绑定 {source_key} 失败"))?;
    }
    Ok(datasource_id)
}

/// 银行形状的两条绑定：行名无父，联行号挂在行名下。
fn bank_bindings() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    vec![
        ("开户行行名", "bank_branch_name", None),
        ("联行号", "bank_branch_code", Some("开户行行名")),
    ]
}

fn xlsx_fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xlsx")
            .join(name),
    )
    .unwrap_or_else(|error| panic!("读夹具 {name} 失败: {error}"))
}

/// 派发一次导入：按**夹具名**读文件（`tests/fixtures/xlsx/<name>`），
/// 按 multipart 塞进请求，打 `import_xlsx`。
///
/// ⚠️ **`todo!` 那一行是本计划唯一没能给出逐字代码的地方**：怎么把文件塞进
/// `yang_base` 的 `Request`，答案在框架侧。请先读
/// `examples/frontend_demo/actions/upload.rs` 的集成测试、
/// 或框架 `crates/yang-base/tests/transport_axum.rs` 里的 multipart 用例，
/// 照抄那里的构造方式填进去。**函数签名与下面所有调用点都已定死**，
/// 所以填完这一处，十个用例都能跑。
async fn dispatch_import(
    app: &Arc<BuiltApp>,
    datasource_id: i64,
    files: &[&str],
) -> Result<ApiResponse, BaseError> {
    let _payload: Vec<(String, Vec<u8>)> =
        files.iter().map(|name| ((*name).to_string(), xlsx_fixture(name))).collect();
    let _ = datasource_id;
    todo!("照抄框架侧的 multipart 用例：把 _payload 塞进 Request 后 dispatch")
}

async fn count_options(database: &Database, source_key: &str, enabled_only: bool) -> anyhow::Result<i64> {
    let sql = if enabled_only {
        "SELECT COUNT(*) FROM `feishu_option` WHERE `source_key` = ? AND `enabled` = 1"
    } else {
        "SELECT COUNT(*) FROM `feishu_option` WHERE `source_key` = ?"
    };
    let (count,): (i64,) = sqlx::query_as(sql).bind(source_key).fetch_one(database.pool()).await?;
    Ok(count)
}

/// 每个用例的公共准备：清库 → 建表 → **重连**（同步会关池）→ 建 Redis → 装配 app。
///
/// 抽成一个 helper 而不是让每个用例各抄五步——既有仓库里
/// `tests/feishu_approval_options_integration.rs` 的 `prepare_app` 就是这个形状。
/// 注意它**不种数据**：每个用例要种的数据各不相同，那是调用方的事。
async fn prepare_app() -> anyhow::Result<(Database, Arc<BuiltApp>)> {
    let database = connect_database().await?;
    drop_feishu_tables(&database).await?;
    sync_feishu_schema(database).await?;
    // **必须重连**：sync_feishu_schema 内部会关掉连接池，
    // 复用同步前的句柄会拿到已关闭的池。
    let database = connect_database().await?;
    let redis = connect_redis().await?;
    let app = build_feishu_app(&database, &redis, None).await?;
    Ok((database, app))
}

/// 读 `ApiResponse` 的 data 段。
fn response_data(response: &ApiResponse) -> anyhow::Result<serde_json::Value> {
    response
        .attachment
        .clone()
        .and_then(|value| value.get("data").cloned())
        .context("响应里没有 data")
}
```

九条测试：

```rust
// 1. 主路径：两条绑定都落库，且父键折对了。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn import_writes_options_for_every_binding() {
    let outcome = async {
        let database = connect_database().await?;
        drop_feishu_tables(&database).await?;
        sync_feishu_schema(database).await?;
        let database = connect_database().await?; // 同步会关池，必须重连
        let redis = connect_redis().await?;
        let datasource_id = seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        let app = build_feishu_app(&database, &redis, None).await?;

        let response = dispatch(
            &app,
            "feishu.datasource",
            "import_xlsx",
            json!({}),
            &[("datasource_id", &datasource_id.to_string())],
            &[],
        )
        .await?;
        let data = response_data(&response)?;

        assert_eq!(data["bindings"].as_array().map(Vec::len), Some(2));
        // 两条绑定共享同一份快照：bank_1 有 3 行、bank_2 有 2 行 → fetched 恒为 5
        for binding in data["bindings"].as_array().unwrap_or(&Vec::new()) {
            assert_eq!(binding["fetched"], 5, "一份快照服务所有绑定");
        }
        // 行名去重后 5 条（夹具里 5 个不同行名），联行号 5 条
        assert_eq!(count_options(&database, "bank_branch_name", true).await?, 5);
        assert_eq!(count_options(&database, "bank_branch_code", true).await?, 5);

        // 父键折对了：子绑定的 parent_key 应等于父行自己的 option_id
        let (parent_key,): (String,) = sqlx::query_as(
            "SELECT `parent_key` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_code' LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(!parent_key.is_empty(), "子绑定必须有父键");
        let (owner,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' AND `option_id` = ?",
        )
        .bind(&parent_key)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(owner, 1, "父键必须能在父绑定里找到对应的行（否则下拉会空）");
        Ok(())
    }
    .await;
    finish(outcome, "主路径").await;
}

// 2. 缺列即拒（D9）：文件里没有 `联行号` → 整个请求被拒，**库里零变化**。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_missing_column_rejects_the_whole_import() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        let error = dispatch_import(&app, datasource_id, &["header_missing_column.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("缺列必须被拒"));
        let message = error.to_string();
        assert!(message.contains("联行号"), "要点名缺哪列: {message}");
        // 整份拒绝 ⇒ 两条绑定都不该有任何行
        assert_eq!(count_options(&database, "bank_branch_name", false).await?, 0);
        assert_eq!(count_options(&database, "bank_branch_code", false).await?, 0);
        Ok(())
    }
    .await;
    finish(outcome, "缺列即拒").await;
}

// 3. 多列忽略（D9）：文件里多一列未勾选的列 → 正常导入。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn extra_columns_are_ignored() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 上传 header_extra_column.xlsx（表头多了「备注」列）——必须正常导入
        dispatch_import(&app, datasource_id, &["header_extra_column.xlsx"]).await?;
        assert_eq!(count_options(&database, "bank_branch_code", true).await?, 1);
        Ok(())
    }
    .await;
    finish(outcome, "多列忽略").await;
}

// 4. 表级空快照守卫：只有表头 + 库里已有启用选项 → 整轮失败且**不停用**。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn an_empty_snapshot_fails_the_round_without_disabling() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先正常导一次，让库里有启用选项
        dispatch_import(&app, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let before = count_options(&database, "bank_branch_code", true).await?;
        assert!(before > 0);

        // 再传一个只有表头的文件：必须整轮失败，且一行都不许停用
        let error = dispatch_import(&app, datasource_id, &["only_header.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("空快照必须整轮失败"));
        assert!(error.to_string().contains("0 行"), "错误要说清是空快照: {error}");
        assert_eq!(
            count_options(&database, "bank_branch_code", true).await?,
            before,
            "空快照下绝不可以停用补集"
        );
        Ok(())
    }
    .await;
    finish(outcome, "表级空快照守卫").await;
}

// 5. **逐绑定空快照守卫（§5.7.1）——本任务最该盯的一条。**
//    绑两条：行名 + 联行号；文件里 `联行号` 整列为空。
//    期望：联行号那条**跳过写库、库里原有选项一行不动**，
//          行名那条照常提交，回执里联行号带 skipped_reason。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_binding_that_derives_nothing_is_skipped_not_emptied() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先用 bank_1 导一次，让两条绑定都有启用选项
        dispatch_import(&app, datasource_id, &["bank_1.xlsx"]).await?;
        let code_before = count_options(&database, "bank_branch_code", true).await?;
        assert!(code_before > 0, "前置条件：联行号已有选项");

        // 再传 column_all_blank.xlsx：表头一致（8 列都在），但 `联行号` 整列为空
        let response = dispatch_import(&app, datasource_id, &["column_all_blank.xlsx"]).await?;
        let data = response_data(&response)?;

        let code_report = data["bindings"].as_array().unwrap_or(&Vec::new()).iter()
            .find(|b| b["source_key"] == "bank_branch_code")
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有联行号"));
        assert_eq!(code_report["derived"], 0);
        assert!(
            code_report["skipped_reason"].is_string(),
            "跳过必须出现在回执里，不能静默: {code_report}"
        );
        assert_eq!(
            count_options(&database, "bank_branch_code", true).await?,
            code_before,
            "**一行都不许停用**——这是 pull.rs 的表级守卫挡不住的情形"
        );

        // 行名那条照常提交（A12 的逐绑定隔离）
        let name_report = data["bindings"].as_array().unwrap_or(&Vec::new()).iter()
            .find(|b| b["source_key"] == "bank_branch_name")
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有行名"));
        assert!(name_report["derived"].as_i64().unwrap_or(0) > 0);
        Ok(())
    }
    .await;
    finish(outcome, "逐绑定空快照守卫").await;
}

// 6. 整体替换：第二个文件的数据完全替换第一个（不是追加）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn re_importing_replaces_the_whole_snapshot() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先导 bank_1（3 个数据行）
        dispatch_import(&app, datasource_id, &["bank_1.xlsx"]).await?;
        assert_eq!(count_options(&database, "bank_branch_name", true).await?, 3);
        // 再导 bank_2（另 2 行、行名完全不同）→ bank_1 的行名必须消失
        let (stale,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' AND `enabled` = 1 \
               AND `label` = '中国工商银行成都春熙路支行'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(stale, 0, "上一轮独有的行必须消失");
        Ok(())
    }
    .await;
    finish(outcome, "整体替换").await;
}

// 7. 逐绑定隔离（A12）：让**后一条**绑定的写入失败，断言它自己回到导入前、
//    前面已提交的绑定保持新值。
//    **不要写成「旧数据一行不少」**——那是单事务的期望，与逐绑定粒度直接冲突。
//
//    怎么让后一条失败：把 `bank_branch_code` 的某条 option_id 预先塞给
//    **另一个 source_key**（造出跨源夺取），跨源预检会拒它。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_failing_binding_does_not_roll_back_its_predecessors() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        // **怎么造出「后一条绑定写失败」**：option_id 的哈希函数是 pub(crate)，
        // 集成测试算不出来。所以先正常导一次，把真实的 option_id 从库里读回来，
        // 再把它改成「属于另一个源」——跨源预检就会拒它。
        dispatch_import(&app, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let (doomed_id,): (String,) = sqlx::query_as(
            "SELECT `option_id` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_code' \
             ORDER BY `option_id` LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "UPDATE `feishu_option` SET `source_key` = 'another_source' WHERE `option_id` = ?",
        )
        .bind(&doomed_id)
        .execute(database.pool())
        .await?;
        // 再停用一条自己的行：摘要不含 `enabled`，所以 `disabled_rows > 0`
        // 会让 `unchanged` 变 false，逼这一轮真的走「预检 → 替换 → 补集」。
        // （不这么做的话，内容没变会被跳过，预检根本不会跑。）
        sqlx::query(
            "UPDATE `feishu_option` SET `enabled` = 0 \
             WHERE `source_key` = 'bank_branch_code' AND `option_id` <> ? LIMIT 1",
        )
        .bind(&doomed_id)
        .execute(database.pool())
        .await?;

        // 绑定按 `id` 升序处理：行名（先种，id 小）在前、联行号在后。
        // 断言的就是这个先后——行名先提交成功，联行号才撞上跨源预检。
        let error = dispatch_import(&app, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("跨源夺取必须被拒"));
        assert!(error.to_string().contains("已属于数据源"), "实际: {error}");

        // 断言顺序：绑定按 `id` 升序，行名在前 —— 它应当已经提交了新数据
        assert!(
            count_options(&database, "bank_branch_name", true).await? > 0,
            "前一条已提交的绑定必须保持新值（逐绑定粒度，不是全量回滚）"
        );
        Ok(())
    }
    .await;
    finish(outcome, "逐绑定隔离").await;
}

// 8. xlsx 源不会被拉取路径碰。
//    自动轮询的机制是 `load_pull_tables` 的 `where_eq("ingest_mode", "pull")`，
//    而那个函数要一个 `&FeishuContext`——集成测试拿不到（它是 Action 内部装配的）。
//    所以这里断言**可观测的那一面**：控制台上的「立即拉取」对 xlsx 源必须被拒。
//    （WHERE 子句本身由 `pull.rs` 的单测覆盖。）
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn the_pull_path_rejects_an_xlsx_source() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        let response = dispatch(
            &app,
            "feishu.datasource",
            "pull_now",
            json!({ "datasource_id": datasource_id }),
            &[],
            &[],
        )
        .await?;
        let data = response_data(&response)?;
        assert_eq!(
            data["code"], 40903,
            "xlsx 源必须落到 NOT_PULLABLE（check_pullable 对非 pull 的判据）"
        );
        Ok(())
    }
    .await;
    finish(outcome, "拉取路径拒绝 xlsx 源").await;
}

// 9. 取数方式不符：对 ingest_mode=pull 的源调导入 → 明确拒绝，不是静默导入。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn importing_into_a_pull_source_is_rejected() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        // 同一个建源 helper，取数方式换成 pull —— 那条源不该接受文件导入。
        let datasource_id = seed_xlsx_datasource(&database, "pull", &bank_bindings()).await?;

        let response = dispatch_import(&app, datasource_id, &["bank_1.xlsx"]).await?;
        let data = response_data(&response)?;
        assert_ne!(data["code"], 0, "取数方式不符必须以失败信封返回");
        assert_eq!(count_options(&database, "bank_branch_name", false).await?, 0);
        Ok(())
    }
    .await;
    finish(outcome, "取数方式不符").await;
}

// 10. 超长取数文案（设计 §5.7）：入库但 `enabled = false` + 原因写 `extra`。
//     不截断的话，`label` 列是 max_length(255)，会在**插库时**才炸，
//     而那时的错误信息不会告诉你是哪一行、什么值。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn an_overlong_label_is_truncated_and_disabled_with_a_reason() {
    let outcome = async {
        let (database, app) = prepare_app().await?;
        // 只种一条绑定（行名）——本用例只关心它。
        let datasource_id = seed_xlsx_datasource(
            &database,
            "xlsx_import",
            &[("开户行行名", "bank_branch_name", None)],
        )
        .await?;

        // `overlong_value.xlsx` 的 `开户行行名` 列有一个 300 字符的值
        // （表头本身正常，见 Task 1 的夹具表）。
        dispatch_import(&app, datasource_id, &["overlong_value.xlsx"]).await?;

        let (label, enabled, extra): (String, bool, Option<String>) = sqlx::query_as(
            "SELECT `label`, `enabled`, `extra` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' \
             ORDER BY CHAR_LENGTH(`label`) DESC LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;

        assert_eq!(label.chars().count(), 255, "截断到 label 的上限");
        assert!(!enabled, "超长值必须置 enabled = false，不喂给飞书");
        let extra = extra.unwrap_or_default();
        assert!(extra.contains("anomaly"), "原因要写进 extra: {extra}");
        Ok(())
    }
    .await;
    finish(outcome, "超长文案").await;
}
```

统一收口（照抄既有集成测试的兜底形态）：

```rust
/// 每个用例的统一清理：无论成败都 drop 掉飞书表。
async fn finish(outcome: anyhow::Result<()>, what: &str) {
    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("{what} 集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
```

---

## Task 10 补充：关于那处 `todo!`

`dispatch_import` 里的 `todo!("照抄框架侧的 multipart 用例…")` 是本计划**唯一**
没有给出逐字代码的地方——它问的是「怎么把文件字节塞进 `yang_base` 的 `Request`」，
答案在框架侧而不是本仓库。三条线索，按优先级：

1. `examples/frontend_demo/actions/upload.rs` —— 仓库里唯一注册过 multipart 的 Action，
   看它的集成测试怎么构造请求（若有）。
2. `crates/yang-base/tests/transport_axum.rs` —— 框架侧对 multipart 的往返测试
   （请求域清理的两条测试也在那里）。这是**最可能有现成构造代码**的地方。
3. `crates/yang-base/src/action/upload.rs` 与 `transport/axum.rs` 的 `decode_multipart`。

**这一处不通，Task 10 的十条用例全跑不了**——所以它是 Task 10 的第一步活，
建议开工时先把它跑通（哪怕先只让「主路径」那条绿），再往下写。

- [ ] **Step 3: 登记集成测试并确认失败**

`scripts/run_ci.py` 的 `INTEGRATION` 元组**追加**一条（Task 5 已经往同一个元组里
加过一条，**不要替换掉它**）：

```python
    Command(
        "飞书 xlsx 导入集成测试",
        (
            "cargo",
            "test",
            "--test",
            "feishu_xlsx_import_integration",
            "--locked",
            "--",
            "--ignored",
            "--test-threads=1",
        ),
    ),
```

```bash
python scripts/run_ci.py --self-test
```

预期：PASS。然后**申请用户许可**后跑（集成测试会重建业务测试表）：

```bash
cargo test --test feishu_xlsx_import_integration --locked -- --ignored --test-threads=1
```

预期：FAIL（Action 未实现）。

- [ ] **Step 4: 实现导入 Action**

文件结构照抄 `list_bitable_fields.rs`。**核心是把 `pull.rs::pull_table_inner` 的
编排逐段搬过来，只替换第 2 步（取数）**。事务获取方式与 `pull.rs` **不同**：

```rust
pub(super) async fn handle(
    ctx: ActionContext,
    input: ImportInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let started = std::time::Instant::now();

    // 数据源与启用绑定：一次取回。
    let datasource = load_datasource(&context, input.datasource_id).await?;
    let bindings = load_enabled_bindings(&context, input.datasource_id).await?;

    // 步骤 1：校验。三个条件各自以**失败信封**返回（不是 BaseError）——
    // 与 pull_now 的 codes 段同风格，也让前端能区分「参数错」与「这条源现在导不了」。
    for (ok, code, message) in [
        (
            datasource.is_some(),
            codes::SOURCE_NOT_FOUND,
            "数据源不存在",
        ),
        (
            datasource.as_ref().is_some_and(|d| d.ingest_mode == "xlsx_import"),
            codes::NOT_IMPORTABLE,
            "这条数据源的取数方式不是文件导入",
        ),
        (
            datasource.as_ref().is_some_and(|d| d.status == "active"),
            codes::DISABLED,
            "这条数据源已停用",
        ),
    ] {
        if !ok {
            return Ok(ApiResponse::fail(code, message.to_string()));
        }
    }
    let Some(datasource) = datasource else {
        // 上面第一个分支已经返回了，这里只是让类型收窄成立。
        return Ok(ApiResponse::fail(codes::SOURCE_NOT_FOUND, "数据源不存在".to_string()));
    };

    // 步骤 2：读文件（**替换 pull 的 list_all_records**）。
    // 必需列 = 全部启用绑定的 field_id（父列也是某条绑定的 field_id——同源内）。
    let columns: Vec<String> = bindings.iter().map(|b| b.field_id.clone()).collect();
    let mut loaded = Vec::with_capacity(input.files.len());
    for file in &input.files {
        let name = file.original_filename().to_string();
        let bytes = tokio::fs::read(file.path()).await
            .map_err(|error| BaseError::ConfigError(format!("读上传文件 {name} 失败：{error}")))?;
        loaded.push((name, bytes));
    }
    // **文件之间**表头必须完全一致——`read_snapshot` 把这条前置条件交给调用方
    // （它只逐个文件比对必需列，从不做文件之间的比对）。**这里必须自己调**：
    // 「重新导入」不经过探表头（详情页直接调导入），只在探表头做校验会漏掉那条路径。
    // 注意与 D9 的「多列忽略」不矛盾：那是「单文件 vs 配置」，这是「文件 vs 文件」。
    let headers: Vec<(String, xlsx::SheetHeader)> = loaded
        .iter()
        .map(|(name, bytes)| {
            xlsx::read_header(bytes)
                .map(|header| (name.clone(), header))
                .map_err(|error| BaseError::ParamInvalid("files".to_string(), format!("{name}：{error}")))
        })
        .collect::<Result<_, _>>()?;
    xlsx::require_consistent_headers(&headers)
        .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    let snapshot = xlsx::read_snapshot(&loaded, &columns)
        .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    // 表级空快照守卫——**与 pull.rs 同款**：解析出 0 行而库里仍有启用选项时，
    // 不停用、且整轮失败。绝不能照常跑补集停用。
    if snapshot.rows.is_empty() {
        let mut suspicious = Vec::new();
        for binding in &bindings {
            if count_active(&context, &binding.source_key).await? > 0 {
                suspicious.push(binding.source_key.clone());
            }
        }
        if !suspicious.is_empty() {
            return Err(BaseError::ConfigError(format!(
                "上传的文件解析出 0 行数据，但本地仍有已启用选项（{}）：拒绝停用补集",
                suspicious.join("、")
            )));
        }
    }

    // 步骤 3-5：逐绑定派生与落库。
    let mut reports = Vec::with_capacity(bindings.len());
    for binding in &bindings {
        let report = import_binding(&ctx, &context, input.datasource_id, binding, &snapshot).await?;
        reports.push(report);
    }

    Ok(ApiResponse::success(
        serde_json::json!({
            "datasource_id": input.datasource_id,
            "elapsed_ms": started.elapsed().as_millis() as i64,
            "files": snapshot.per_file.iter()
                .map(|f| serde_json::json!({ "name": f.name, "rows_read": f.rows_read }))
                .collect::<Vec<_>>(),
            "bindings": reports,
        }),
        "导入完成",
    ))
}
```

`import_binding` 是**逐绑定一个事务**（A12），骨架照抄 `pull.rs::persist_binding`
（`:521-572`）与它上方那段「`!unchanged` 才预检+扫补集」的逻辑（`:424-460`）：

**关于复用 `pull.rs` 的类型**：导入侧**直接复用** `TableBinding` 与 `BoundField`
（两者都是 `pub(crate)`，字段形状恰好就是要的东西），不另造一套——
这样 `parent_linkage` 才能原样调用，防环守卫也就一并复用了。

```rust
/// 从绑定行构造 pull 侧的同名类型。xlsx 没有「解析出的当前名字」这一层，
/// 所以 `field_name` 直接取绑定行上那个（= 列名，Task 8 保证它非空）。
fn to_table_bindings(rows: &[BindingRow]) -> Vec<pull::TableBinding> {
    rows.iter()
        .map(|row| pull::TableBinding {
            id: row.id,
            source_key: row.source_key.clone(),
            field_id: row.field_id.clone(),
            field_name: row.field_name.clone(),
            parent_field_id: row.parent_field_id.clone(),
            snapshot_digest: row.snapshot_digest.clone(),
        })
        .collect()
}

fn to_bound_fields(bindings: &[pull::TableBinding]) -> Vec<pull::BoundField> {
    bindings
        .iter()
        .map(|binding| pull::BoundField {
            field_id: binding.field_id.clone(),
            // 列名就是身份；照抄 pull 的做法用「本轮解析后的名字」，只是
            // xlsx 侧这个名字恒等于 field_id（没有上游可解析）。
            field_name: binding.field_id.clone(),
            parent_field_id: binding.parent_field_id.clone(),
        })
        .collect()
}

async fn import_binding(
    ctx: &ActionContext,
    context: &Arc<FeishuContext>,
    datasource_id: i64,
    binding: &pull::TableBinding,
    all_bindings: &[pull::TableBinding],
    snapshot: &xlsx::ImportSnapshot,
) -> Result<serde_json::Value, BaseError> {
    // 祖先链：**直接复用 pull 的同一条上溯**（它带 visited 防环）。
    // 自己再写一份等于把那条守卫复制两遍——手工改库造出的 a→b→a
    // 会在一处被挡、在另一处把请求卡死。
    let bound = to_bound_fields(all_bindings);
    let field = bound
        .iter()
        .find(|f| f.field_id == binding.field_id)
        .ok_or_else(|| BaseError::ConfigError("绑定集合里找不到自己".to_string()))?;
    let linkage = pull::parent_linkage(field, all_bindings, &bound)?;

    // 投影：本绑定的取数列 + 各级祖先列。祖先列名就是父绑定的 field_id。
    // （完整代码在本节末尾「投影那一段的借用写法」里，含超长截断与异常收集）

    let ancestor_sources: Vec<&str> = linkage.as_ref().map(Linkage::source_keys).unwrap_or_default();
    let derived = derive_options(&binding.source_key, &ancestor_sources, &raw);
    let digest = snapshot_digest(&derived);

    let (existing_rows, active_rows) = count_existing(context, &binding.source_key).await?;
    let disabled_rows = existing_rows.saturating_sub(active_rows);
    let unchanged =
        binding.snapshot_digest.as_deref() == Some(digest.as_str()) && disabled_rows == 0;

    // **§5.7.1 逐绑定空快照守卫（pull.rs 没有这条）**：
    // 整表有行、但这一条绑定派生 0 个选项，而它库里仍有启用行
    // —— 跳过写库，**尤其不执行补集停用**。
    if derived.is_empty() && active_rows > 0 {
        tracing::warn!(
            source_key = %binding.source_key,
            "本轮派生出 0 个选项但本地仍有已启用选项：跳过这条绑定，不动它的补集"
        );
        return Ok(serde_json::json!({
            "source_key": binding.source_key,
            "fetched": snapshot.rows.len(),
            "derived": 0,
            "disabled": 0,
            "unchanged": false,
            "skipped_reason": "本轮派生出 0 个选项，而本地仍有已启用选项——拒绝清空",
        }));
    }

    let mut doomed = Vec::new();
    if !unchanged {
        let ids: Vec<String> = derived.iter().map(|o| o.option_id.clone()).collect();
        if let Some((option_id, owner)) =
            find_foreign_option_owner(context.options(), &binding.source_key, &ids).await?
        {
            return Err(BaseError::ConfigError(format!(
                "选项 id {option_id} 已属于数据源 {owner}，拒绝改写其归属"
            )));
        }
        doomed = find_doomed_for(context, &binding.source_key, &derived).await?;
    }

    // 事务：**用 ctx.begin_transaction()，不要照抄 pull.rs 的 database.transaction()**
    // ——pull.rs 没有 ActionContext（后台 worker 没有 ctx），导入是 Action，有 ctx 就该用 ctx。
    let mut transaction = ctx.begin_transaction().await?;
    let result = async {
        let mut written = (0u64, 0u64);
        if !unchanged {
            let items: Vec<OptionWriteItem> = derived.iter()
                .map(|option| {
                    to_write_item(
                        &binding.source_key,
                        option,
                        anomaly_by_label.get(option.label.as_str()).copied(),
                    )
                })
                .collect();
            let outcome = apply_option_rows(context.options(), &mut transaction, &binding.source_key, &items).await?;
            let disabled = disable_option_rows(context.options(), &mut transaction, &binding.source_key, &doomed).await?;
            written = (outcome.inserted, disabled);

            let event = audit::succeeded_event(
                ctx, None, None,
                audit::entity("feishu_datasource", datasource_id)?,
                None,
                Some(audit::summary([
                    ("outcome_code", serde_json::json!("xlsx_imported")),
                    ("source_key", serde_json::json!(&binding.source_key)),
                    ("option_count", serde_json::json!(derived.len() as i64)),
                    ("disabled_count", serde_json::json!(disabled as i64)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }

        // 绑定状态回写：摘要与「最近导入时间」，与选项行同事务。
        let mut binding_update = Record::new();
        binding_update.insert("snapshot_digest", serde_json::json!(&digest));
        binding_update.insert("last_push_at", serde_json::json!(now_seconds()));
        context.datasource_fields().query()
            .where_eq("id", serde_json::json!(binding.id))?
            .update_in_tx(&mut transaction, binding_update)
            .await?;
        Ok::<_, BaseError>(written)
    }.await;
    let (_, disabled) = FeishuContext::finish_transaction(transaction, result).await?;

    Ok(serde_json::json!({
        "source_key": binding.source_key,
        "fetched": snapshot.rows.len(),
        "derived": derived.len(),
        "disabled": disabled,
        "snapshot_digest": digest,
        "unchanged": unchanged,
        // 异常行逐条可归因。**最多列 100 条**，省略时要给 truncated_details
        // 与真实总数——不做静默截断（设计 §5.11）。
        "anomalies": anomaly_report(&anomaly_by_label),
        "truncated_details": anomaly_by_label.len() > ANOMALY_LIMIT,
    }))
}
```

投影那一段的借用写法**照抄 `pull.rs` 的形态**（`ancestor_refs` 必须先落地，
不能内联进 `as_raw`）：

```rust
    // 祖先列名 = 沿链的各级绑定的 field_id（同源内）
    let ancestor_columns: Vec<String> = linkage.as_ref()
        .map(|l| l.ancestors.iter().map(|level| level.field_name.clone()).collect())
        .unwrap_or_default();

    // 三件套：截断后的文案、各级祖先文案、异常原因（None = 正常）。
    let mut owned: Vec<(String, Vec<String>, Option<String>)> =
        Vec::with_capacity(snapshot.rows.len());
    for row in &snapshot.rows {
        let raw_label = row.get(&binding.field_id).cloned().unwrap_or_default();
        // **设计 §5.7**：取数文案超 `label` 的 255 上限时截断。
        // 不截断的话会在**插库时**才炸（列是 max_length(255)），
        // 而那时的错误不会告诉你是哪一行、什么值。
        let (label, anomaly) = truncate_label(&raw_label, LABEL_LIMIT);
        let ancestors: Vec<String> = ancestor_columns.iter()
            .map(|column| row.get(column).cloned().unwrap_or_default())
            .collect();
        owned.push((label, ancestors, anomaly));
    }
    let ancestor_refs: Vec<Vec<&str>> = owned.iter()
        .map(|(_, ancestors, _)| ancestors.iter().map(String::as_str).collect())
        .collect();
    let raw: Vec<RawValue<'_>> = owned.iter().zip(ancestor_refs.iter())
        .map(|((label, _, _), refs)| RawValue { ancestors: refs, label: label.as_str() })
        .collect();

    // 异常按**截断后的 final label** 索引，写库时按 label 回查。
    // 用 label 而不是 option_id 做键：截断已经发生，`derive_options` 也是按
    // 这个 label 算 id 的，所以「文案超长」这个属性天然属于文案本身。
    let anomaly_by_label: std::collections::HashMap<&str, &str> = owned
        .iter()
        .filter_map(|(label, _, reason)| {
            reason.as_deref().map(|reason| (label.as_str(), reason))
        })
        .collect();
```

配套的两个小函数（放在同文件）：

```rust
/// `feishu_option.label` 的列上限。
const LABEL_LIMIT: usize = 255;

/// 截断到上限，并按需给出异常原因。返回 `(文案, 原因)`。
fn truncate_label(value: &str, limit: usize) -> (String, Option<String>) {
    if value.chars().count() <= limit {
        return (value.to_string(), None);
    }
    let truncated: String = value.chars().take(limit).collect();
    (
        truncated,
        Some(format!(
            "取数文案超过 {limit} 字符（原长 {}），已截断",
            value.chars().count()
        )),
    )
}
```

> **`option_id` 按截断后的值算，这是设计 §5.7 明写的取舍**：
> 否则 `id` 与 `label` 对不上，而「改文案 = 新 id」是补集停用语义的地基。
> 代价认了：两个前 255 字相同的长值会碰撞成同一个 `option_id`，后者被
> `derive_options` 去重掉。**宁可少一个选项并留下 `extra` 里的异常记录，
> 也不要一个 id 与内容不符的行。**

写库的 item 要带上下异常位——**这是导入自己的 `to_write_item`，与 `pull.rs` 的不同**：

```rust
/// 与 `pull::to_write_item` 同形，但异常行置 `enabled = false` 并把原因写进 `extra`。
///
/// 为什么要有这个分支：`pull.rs` 的版本恒写 `enabled = true`，
/// 而「异常行照常入库但**不喂给飞书**」正是设计 §5.7 的 D6。
/// 出站查询本来就 `where_eq("enabled", true)`，所以置 false 就等于藏起来。
fn to_write_item(
    source_key: &str,
    option: &DerivedOption,
    anomaly: Option<&str>,
) -> OptionWriteItem {
    let mut record = Record::new();
    record.insert("option_id", serde_json::json!(option.option_id));
    record.insert("source_key", serde_json::json!(source_key));
    record.insert("label", serde_json::json!(option.label));
    record.insert("sort_order", serde_json::json!(option.sort_order));
    record.insert("enabled", serde_json::json!(anomaly.is_none()));
    record.insert("parent_key", serde_json::json!(option.parent_key));
    record.insert("last_push_at", serde_json::json!(now_seconds()));
    if let Some(reason) = anomaly {
        // `extra` 是 Text 存 JSON 的既有列。
        record.insert("extra", serde_json::json!({ "anomaly": reason }.to_string()));
    }
    OptionWriteItem {
        option_id: option.option_id.clone(),
        record,
    }
}
```

> **注意 `enabled = false` 与「逐绑定空快照守卫」的相互作用**：异常行被置为停用，
> 于是下一轮导入时 `disabled_rows > 0` 会让 `unchanged` 恒为 false
> ——**这是对的**，因为摘要是按「本轮应当是什么」算的、不含 `enabled`，
> 所以必须让每一轮都真的重写一遍，异常行才有机会恢复
> （`pull.rs` 的注释里写着这条不可省的理由）。

`anomaly_report` 是个小函数（放在同文件）：

```rust
/// 回执里的异常清单上限。超出时**不静默截断**——带 `truncated_details` 与总数。
const ANOMALY_LIMIT: usize = 100;

fn anomaly_report(by_label: &std::collections::HashMap<&str, &str>) -> Vec<serde_json::Value> {
    let mut entries: Vec<(&str, &str)> = by_label.iter().map(|(k, v)| (*k, *v)).collect();
    // 排序让回执稳定（HashMap 的迭代顺序不定，回执要可比对）。
    entries.sort_unstable();
    entries
        .into_iter()
        .take(ANOMALY_LIMIT)
        .map(|(label, reason)| serde_json::json!({ "label": label, "reason": reason }))
        .collect()
}
```

> **`find_doomed` 在 `pull.rs` 里是私有的，而且依赖 `PullDeps`**（含出站 transport）。
> 导入不出网，用不上 `PullDeps`，所以**在导入侧写一个同形状但只依赖 `context` 的版本**，
> 并**照抄两个常量与那条编译期断言**：`MAX_COMPLEMENT = 20_000`
> （超限就跳过本轮停用、只告警）与 `SCAN_PAGE_SIZE ≤ MAX_TABLE_QUERY_PAGE_SIZE`。
>
> `pull::to_write_item` **不要**提 `pub(crate)`——导入有自己的那个（多了异常位），
> 两者行为不同，合并反而危险。

> **`now_seconds()`**：`pull.rs` 里的小 helper，同样提为 `pub(crate)` 或就地复制
> （它是 `SystemTime::now()` 的一行包装，复制没有维护风险）。

- [ ] **Step 5: 运行集成测试确认逐条通过**

```bash
cargo test --test feishu_xlsx_import_integration --locked -- --ignored --test-threads=1
```

预期：9 条全 PASS。**第 5 条（逐绑定空快照守卫）与第 7 条（逐绑定隔离）
是本任务最该盯的两条**——它们锁的是 `pull.rs` 没有的新行为。

- [ ] **Step 6: 跑门禁**

```bash
python scripts/run_ci.py quick
```

预期：PASS。**若 `schema_anchor` 报「防线在静默降级」**，说明新 Action 里有
写入站点没被识别到表归属——照 `schema_anchor.rs` 的规则，绑定/选项的 `Record`
必须在 `context.datasource_fields()` / `context.options()` 接收者**旁边**构造，
不要抽成返回 `Record` 的 helper。

- [ ] **Step 7: 提交**

```bash
git add src/addon/feishu/datasource/actions/import_xlsx.rs \
        src/addon/feishu/datasource/actions/mod.rs \
        src/addon/feishu/domain/pull.rs \
        tests/feishu_xlsx_import_integration.rs \
        scripts/run_ci.py
git commit -m "feat(feishu): xlsx 导入端点（逐绑定事务 + 逐绑定空快照守卫）"
```

---

## Task 11: 导入并发互斥

`pull.rs` 是单 worker 串行跑的，天然不会自我并发——**但导入是人触发的，必然会**。
用户手抖点两下、或两个运维同时操作，逐绑定事务会交错，两次补集停用互相覆盖。

**Files:**
- Modify: `src/addon/feishu/datasource/actions/import_xlsx.rs`

**Interfaces:**
- Consumes: Task 10 的 handle
- Produces: 同一 `datasource_id` 的并发导入中，后来者拿到明确失败（不是静默交错）。

- [ ] **Step 1: 写失败的测试**

在 `import_xlsx.rs` 的测试模块里加一条**纯函数层**的测试（真并发留给集成测试）：

```rust
    #[test]
    fn a_second_concurrent_import_is_rejected() {
        let held = ImportGuard::acquire_for_test(7);
        assert!(held.is_ok(), "第一次应拿到");
        let second = ImportGuard::acquire_for_test(7);
        assert!(
            matches!(second, Err(ImportBusy)),
            "同一数据源的第二次并发导入必须被拒"
        );
    }

    #[test]
    fn a_different_datasource_is_not_blocked() {
        let _held = ImportGuard::acquire_for_test(7).unwrap_or_else(|_| panic!("第一次应拿到"));
        assert!(ImportGuard::acquire_for_test(8).is_ok(), "不同数据源互不影响");
    }
```

- [ ] **Step 2: 运行确认失败**

```bash
cargo test --lib --locked import_xlsx
```

预期：编译失败（`ImportGuard` / `ImportBusy` 未定义）。

- [ ] **Step 3: 实现互斥**

用**进程内**的 `Mutex<HashSet<i64>>` 即可——本应用是单实例部署，
而跨实例的分布式锁需要 Redis 且要处理锁超时与续租，超出本任务需要。
（若将来多实例部署，这是一个明确要升级的点，写进注释。）

```rust
/// 导入互斥：同一数据源同时只允许一次导入。
///
/// **进程内**即可：本应用是单实例部署。多实例时要换成 Redis 分布式锁，
/// 并处理锁超时与续租——那时这段注释就是升级说明。
static IMPORTING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<i64>>> =
    std::sync::OnceLock::new();

pub(super) struct ImportGuard {
    datasource_id: i64,
}

pub(super) struct ImportBusy;

impl ImportGuard {
    pub(super) fn acquire(datasource_id: i64) -> Result<Self, ImportBusy> {
        let set = IMPORTING.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        // 中毒的锁不该让导入永久不可用：恢复内部数据继续用。
        let mut guard = set.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.insert(datasource_id) {
            return Err(ImportBusy);
        }
        Ok(Self { datasource_id })
    }
}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        let set = IMPORTING.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        let mut guard = set.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&self.datasource_id);
    }
}
```

> 生产 Rust 禁 `unwrap()`（Clippy `unwrap_used = deny`），所以用
> `unwrap_or_else(|poisoned| poisoned.into_inner())` 而不是 `unwrap()`。
> `Drop` 保证 guard 在所有返回路径（含 `?` 提前返回与 panic 展开）都被释放。

handle 里在**所有校验之后、读文件之前**拿锁：

```rust
    let _guard = ImportGuard::acquire(input.datasource_id).map_err(|ImportBusy| {
        BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "这条数据源正在导入中，请等它跑完再试".to_string(),
        )
    })?;
```

- [ ] **Step 4: 运行确认通过**

```bash
cargo test --lib --locked import_xlsx
```

预期：全 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/addon/feishu/datasource/actions/import_xlsx.rs
git commit -m "feat(feishu): 同一数据源的导入互斥（进程内，人触发的并发必须挡）"
```

---

# 阶段三：前端

## Task 12: 前端 api 层与同步状态

**Files:**
- Modify: `frontend/src/features/feishu/types.ts`（`syncHealth` 加 xlsx 分支）
- Modify: `frontend/src/features/feishu/api.ts`（两个 operation id、`createDatasourceTable`
  支持 `ingest_mode`/`field_name`、新增 `XlsxImportClient`）
- Test: `frontend/tests/features/feishu/sync-health.test.ts`、`api.test.ts`

**Interfaces:**
- Consumes: Task 4 的 `IngestMode` 三取值
- Produces:
  - `syncHealth` 对 `xlsx_import` 返回「文件导入」档，**不再说「由多维表格推送」**
  - `XLSX_OPERATION_IDS = { probe, import }`
  - `XlsxImportClient = { probe, createTable, importFiles }`
  - `CreateTableSubmission` 多两个可选字段：`ingestMode`、`fieldName`（每条 field）

- [ ] **Step 1: 写失败的测试**

`frontend/tests/features/feishu/sync-health.test.ts` 里加：

```typescript
  it("xlsx 源不显示成「由多维表格推送」", () => {
    // 这个档位是运维看到的第一句话。把文件导入的源说成「靠多维表格自动化推送」
    // 会让人去那张根本不存在的多维表格里找自动化日志。
    const health = syncHealth({ ...baseItem, ingestMode: "xlsx_import" });
    expect(health.title).not.toContain("多维表格");
    expect(health.title).toContain("文件导入");
  });
```

`frontend/tests/features/feishu/api.test.ts` 的轴二用例（枚举取值域）不用改
——它从契约读 `values`，Task 4 已把契约与 `INGEST_MODE_OPTIONS` 同步。
但要加一条**正向断言**确认它真的跑到了三值（防止契约写错表名导致门禁静默失效）：

```typescript
  it("ingest_mode 的取值域确实是三项", () => {
    expect(CONTRACT.enums?.feishu_datasource?.ingest_mode?.values).toEqual([
      "push",
      "pull",
      "xlsx_import",
    ]);
  });
```

- [ ] **Step 2: 运行确认失败**

```bash
pnpm --dir frontend test -- features/feishu
```

预期：FAIL 2 条。

- [ ] **Step 3: 改 `syncHealth`**

`frontend/src/features/feishu/types.ts`，在「非 pull」那一档**之前**插入 xlsx 分支
（它必须排在 `!== "pull"` 之前，否则永远走不到）：

```typescript
  if (asIngestMode(item.ingestMode) === "xlsx_import") {
    return {
      tone: "info",
      title: "文件导入",
      detail:
        "这个数据源的选项由人上传 xlsx 文件导入，服务端不出网、也没有定时同步。" +
        "要看它是不是最新的，去「重新导入」那一步看最近一次导入的记录。",
    };
  }

  if (asIngestMode(item.ingestMode) !== "pull") {
    /* 现有那一档不动 */
  }
```

- [ ] **Step 4: 改 api 层**

`frontend/src/features/feishu/api.ts`：

```typescript
export const XLSX_OPERATION_IDS = {
  /// 只读表头：上传文件 → 回列名。不写库。
  probe: "feishu.datasource.probe_xlsx_headers",
  /// 导数据：上传同一批文件 → 建选项。写。
  importFiles: "feishu.datasource.import_xlsx",
} as const;
```

`createDatasourceTable` 把硬编码的 `ingest_mode: "pull"` 改成可传入，
并在 fields 里带上 `field_name`：

```typescript
  const body: Record<string, unknown> = {
    title: input.title,
    // 由调用方决定：多维表格向导用 pull，xlsx 向导用 xlsx_import。
    ingest_mode: input.ingestMode ?? "pull",
    fields: input.fields.map((field) => ({
      field_id: field.fieldId,
      // xlsx 绑定必须带列名：后端对每条启用绑定 require("field_name")，
      // 留空会让审批外部选项**整批装配失败**（且范围是全表，不止本数据源）。
      field_name: field.fieldName,
      source_key: field.sourceKey,
      parent_field_id: field.parentFieldId,
    })),
  };
  // 多维表格专用坐标：xlsx 源两者都不给（给了反而会被解析成坐标）。
  if (input.appToken !== undefined) body.bitable_base_token = input.appToken;
  if (input.tableId !== undefined) body.bitable_table_id = input.tableId;
  if (input.viewId !== undefined && input.viewId.trim() !== "") {
    body.bitable_view_id = input.viewId.trim();
  }
```

`XlsxImportClient`（**照抄 `TableWizardClient` 的注入范式**——向导只能经注入的
client 访问数据，不摸 `useSessionCredentials`/`useUiCatalog`）：

```typescript
export type XlsxImportClient = {
  probe: (files: File[]) => Promise<XlsxHeaderProbe>;
  createTable: (input: CreateTableSubmission) => Promise<CreatedTable>;
  importFiles: (
    datasourceId: number,
    files: File[],
  ) => Promise<XlsxImportReport>;
};

export function useXlsxImportClient(): XlsxImportClient { /* 与 useTableWizardClient 同形 */ }
```

`probe` 与 `importFiles` 都走 `invokeFeishuAction`，**`files` 必须是 values 顶层的
`File[]`**——引擎的 `appendMultipart` 只识别顶层 `File` 或「全是 File 的数组」，
嵌套对象会被 `JSON.stringify` 成文本，`undefined`/`null`/空串的键会被整个跳过：

```typescript
async function probeXlsxHeaders(
  files: File[],
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<XlsxHeaderProbe> {
  const result = await invokeFeishuAction(
    deps,
    XLSX_OPERATION_IDS.probe,
    // 顶层就是 File[]，不要包成 { files: { items: [...] } }
    { files },
    signal,
  );
  /* 解析 result.data：sheet_name / sheets / header_row / columns / files */
}
```

- [ ] **Step 5: 运行确认通过**

```bash
pnpm --dir frontend test
pnpm --dir frontend typecheck
```

预期：PASS。

- [ ] **Step 6: 重生成契约快照**

Task 9 与 Task 10 加了两个新 Action，OpenAPI 快照与 TS 类型**必须跟着重生成**，
否则前端 `requireAction` 按 operation id 找不到它们：

```bash
python scripts/dump_openapi.py
pnpm --dir frontend format
```

预期：`frontend/contracts/openapi.json` 与
`frontend/src/engine/contracts/api-types.ts` 出现
`probe_xlsx_headers` 与 `import_xlsx` 两条。
**这两个文件是生成物，禁止手改**——一起提交。

> ⚠️ **`pnpm format` 不是可选的**（Task 4 实测踩过）：`openapi-typescript` 产 4 空格、
> 入库件是 2 空格，脚本不跑 prettier。**先 `dump_openapi.py` 再 `pnpm format`**，
> 否则是上万行纯缩进 churn + `format:check` 变红。
> 这一次生成物**会真的变**（两个新 Action 要投影进去），所以要提交。

> 顺带确认它们带上了 `request_media_type: "multipart"` 与 `multipart` 限制契约
> （`SchemaField` 与 `appendMultipart` 靠这两项工作）。

- [ ] **Step 7: 提交**

```bash
git add frontend/src/features/feishu/types.ts frontend/src/features/feishu/api.ts \
        frontend/contracts/openapi.json \
        frontend/src/engine/contracts/api-types.ts \
        frontend/tests/features/feishu/sync-health.test.ts \
        frontend/tests/features/feishu/api.test.ts
git commit -m "feat(feishu): 前端 xlsx 导入的 api 层与同步状态档位"
```

---

## Task 13: `XlsxImportWizard` 组件

**Files:**
- Create: `frontend/src/features/feishu/components/XlsxImportWizard.tsx`
- Test: `frontend/tests/features/feishu/components/xlsx-wizard.test.tsx`

**Interfaces:**
- Consumes: Task 12 的 `XlsxImportClient`
- Produces:
  ```typescript
  export type XlsxImportWizardProps = {
    client: XlsxImportClient;
    onCancel: () => void;
    onSubmitted?: (created: CreatedTable, submission: CreateTableSubmission) => void;
    open?: boolean;
  };
  ```
  Task 14 渲染它。

- [ ] **Step 1: 写失败的组件测试**

`frontend/tests/features/feishu/components/xlsx-wizard.test.tsx`，
**照抄 `table-wizard.test.tsx` 的注入范式**（stub client + 逐步骤驱动）：

```typescript
function stubClient(overrides: Partial<XlsxImportClient> = {}): XlsxImportClient {
  return {
    probe: vi.fn().mockResolvedValue({
      sheetName: "境内银行网点信息管理",
      sheets: ["境内银行网点信息管理"],
      headerRow: 1,
      columns: [
        { name: "开户行行名", index: 2 },
        { name: "联行号", index: 5 },
        { name: "地区名称", index: 7 },
      ],
      files: [{ name: "bank_1.xlsx" }],
    }),
    createTable: vi.fn().mockResolvedValue({ datasourceId: 9, credentials: [] }),
    importFiles: vi.fn().mockResolvedValue({
      datasourceId: 9,
      elapsedMs: 1200,
      files: [{ name: "bank_1.xlsx", rowsRead: 5 }],
      bindings: [],
    }),
    ...overrides,
  };
}

function file(name = "bank_1.xlsx"): File {
  return new File(["PK\x03\x04"], name, {
    type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  });
}
```

一条贯穿四步的驱动 helper（**照抄 `table-wizard.test.tsx` 的 `driveToFields`**）：

```typescript
async function driveToStep4(client: XlsxImportClient) {
  render(<XlsxImportWizard client={client} onCancel={vi.fn()} onSubmitted={vi.fn()} />);
  // 第 1 步：填名称 + 选文件
  await userEvent.type(screen.getByLabelText("名称"), "银行网点");
  await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
  await userEvent.click(screen.getByRole("button", { name: "解析表头" }));
  // 等 probe 回来
  await screen.findByText("开户行行名");
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 2 步：勾两列
  await userEvent.click(screen.getByLabelText("开户行行名"));
  await userEvent.click(screen.getByLabelText("联行号"));
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 3 步：配源标识与父列（默认 source_key 已自动派生）
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 4 步
  await screen.findByText(/第 4 步/);
}
```

必测四条：

```typescript
  it("第 1 步传文件后进第 2 步，并把列名渲染成可勾选项", async () => {
    const client = stubClient();
    render(<XlsxImportWizard client={client} onCancel={vi.fn()} />);
    await userEvent.type(screen.getByLabelText("名称"), "银行网点");
    await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
    await userEvent.click(screen.getByRole("button", { name: "解析表头" }));

    // 三个列名都渲染出来，且**未勾选**（默认一个都不勾）
    for (const name of ["开户行行名", "联行号", "地区名称"]) {
      const box = await screen.findByLabelText(name);
      expect(box).not.toBeChecked();
    }
    // 传进去的确实是 File 实例（引擎靠 instanceof File 决定走不走 FormData）
    expect(vi.mocked(client.probe).mock.calls[0]?.[0]?.[0]).toBeInstanceOf(File);
  });

  it("一个列都不勾时不能进第 3 步", async () => {
    // 「勾零列」建出来的数据源一条绑定都没有——它不会出数，
    // 但会在台账里占一行，用户得先勾一列。
    const client = stubClient();
    render(<XlsxImportWizard client={client} onCancel={vi.fn()} />);
    await userEvent.type(screen.getByLabelText("名称"), "银行网点");
    await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
    await userEvent.click(screen.getByRole("button", { name: "解析表头" }));
    await screen.findByText("开户行行名");
    await userEvent.click(screen.getByRole("button", { name: "下一步" }));

    // 已经到第 2 步（勾列），但一个都没勾
    expect(screen.getByText(/第 2 步/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
  });

  it("每列的源标识默认派生成合法 ASCII 键，且中文列名不会直接进 sourceKey", async () => {
    // 后端约束：1..=64 字节、首字节小写 ASCII 字母、其余 [a-z0-9_]。
    // 中文列名直接当 source_key 会被 400 拒掉，所以默认值必须是派生的 ASCII 键，
    // 前端先挡一道，别让用户提交后才吃错误。
    const client = stubClient();
    await driveToStep4(client);
    const sourceKeyInput = screen.getByLabelText("开户行行名 的源标识") as HTMLInputElement;
    expect(sourceKeyInput.value).toMatch(/^[a-z][a-z0-9_]{0,63}$/);
    expect(sourceKeyInput.value).not.toContain("开户行行名");
  });

  it("父列下拉只列同源内已勾选的其它列", async () => {
    // A11：父子关系在同源内（后端 load_parent_source_key 按 datasource_id 过滤）。
    // 列一个跨源的父选项只会让用户在提交后才被拒。
    const client = stubClient();
    await driveToStep4(client);

    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    const options = await screen.findAllByRole("option");
    const labels = options.map((option) => option.textContent);
    expect(labels).toContain("开户行行名");
    expect(labels).not.toContain("地区名称"); // 没勾的列不能当父
    expect(labels).not.toContain("联行号");   // 不能自指
  });

  it("提交时先建源再导入，且导入用的是同一批 File", async () => {
    // D11：服务端零暂存——第 1 步的文件只用于探表头，导入时要重传同一批
    // （浏览器里 File 对象一直在，用户不用重新选）。
    const client = stubClient();
    await driveToStep4(client);

    // 勾了「联行号」且父列选了「开户行行名」，源标识用默认值
    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    await userEvent.click(await screen.findByRole("option", { name: "开户行行名" }));
    await userEvent.click(screen.getByRole("button", { name: "创建并导入" }));

    await waitFor(() => expect(client.importFiles).toHaveBeenCalledTimes(1));

    // 顺序固定：先建源（拿到 datasourceId），再拿它去导入
    const createdId = vi.mocked(client.createTable).mock.results[0]?.value;
    void createdId; // createTable 是 mock，断言调用顺序即可
    expect(vi.mocked(client.createTable).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(client.importFiles).mock.invocationCallOrder[0] ?? 0,
    );

    // **同一批 File 被重传**——这是「用户只选一次文件」的实现方式
    const [datasourceId, passed] = vi.mocked(client.importFiles).mock.calls[0] ?? [];
    expect(datasourceId).toBe(9); // stubClient 里 createTable 回的 datasourceId
    expect(passed?.[0]?.name).toBe("bank_1.xlsx");

    // 建源请求里 field_name 必须带上（漏了会让审批选项装配整批失败）
    const submission = vi.mocked(client.createTable).mock.calls[0]?.[0];
    expect(submission?.ingestMode).toBe("xlsx_import");
    for (const field of submission?.fields ?? []) {
      expect(field.fieldName).not.toBe("");
      expect(field.fieldId).toBe(field.fieldName);
    }
  });
```

- [ ] **Step 2: 运行确认失败**

```bash
pnpm --dir frontend test -- xlsx-wizard
```

预期：FAIL（组件不存在）。

- [ ] **Step 3: 实现组件**

四步，状态机与 `DatasourceTableWizard` 同形（`type Step = 1 | 2 | 3 | 4`）。关键状态：

```typescript
  const [step, setStep] = useState<Step>(1);
  const [title, setTitle] = useState("");
  // **File 对象留在这里**：服务端零暂存，第 4 步要重传同一批。
  const [files, setFiles] = useState<File[]>([]);
  const [probe, setProbe] = useState<XlsxHeaderProbe | null>(null);
  const [selected, setSelected] = useState<string[]>([]); // 列名
  const [configs, setConfigs] = useState<Record<string, RowConfig>>({});
```

`RowConfig` 与多维表格向导同形（`sourceKey` + `parentFieldId`），父列候选
**只在 `selected` 内**（同源内），并保留「取消勾选时顺手清掉以它为父的行」的连带清理。

`sourceKey` 的初值**自动派生**一个合法 ASCII 键，而不是留空等人填：

```typescript
  /// 从列名派生一个合法的 source_key。后端要求 1..=64 字节、
  /// 首字节小写 ASCII 字母、其余 [a-z0-9_] —— 中文列名直接拿去当 source_key
  /// 会被拒，所以这里给一个「列1/列2…」的可用初值，用户可以改。
  function defaultSourceKey(columnIndex: number): string {
    return `col_${columnIndex}`;
  }
```

第 4 步提交（**两个请求，顺序固定**）：

```typescript
  async function submit() {
    setSubmitting(true);
    setSubmitError(null);
    try {
      // 1) 建源 + N 条绑定。此时还没有数据。
      const created = await client.createTable({
        title: title.trim(),
        ingestMode: "xlsx_import",
        fields: selected.map((name) => ({
          fieldId: name,          // 列名即身份（后端 field_id 存列名）
          fieldName: name,        // **必须同时给**，否则审批选项装配整批失败
          type: "",
          sourceKey: configs[name]?.sourceKey.trim() ?? "",
          parentFieldId: configs[name]?.parentFieldId ?? null,
        })),
      });
      // 2) 导入。重传第 1 步那批 File（浏览器里一直留着）。
      const report = await client.importFiles(created.datasourceId, files);
      setReport(report);
      onSubmitted?.(created, submission);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  }
```

> **失败语义要写清楚**：第 1 步成功、第 2 步失败时**不要回滚数据源**——
> 数据源与绑定是配置，导入是数据，两者各自可重试（这正是拆成两个请求的理由）。
> 失败时留在第 4 步并显示「数据源已建好，但导入失败：…，可以重试导入」，
> 重试只重发第 2 步。

同时渲染：
- `probe.sheets.length > 1` 时提示「这份文件有 N 张 sheet，**只读了第一张**」；
- 整列为空的候选列**提示不了**（probe 只读表头，不知道列是否全空）——这是
  设计 §8 未决项 5 的范围，不要在组件里假装能做。

- [ ] **Step 4: 运行确认通过**

```bash
pnpm --dir frontend test -- xlsx-wizard
pnpm --dir frontend lint
pnpm --dir frontend typecheck
```

预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add frontend/src/features/feishu/components/XlsxImportWizard.tsx \
        frontend/tests/features/feishu/components/xlsx-wizard.test.tsx
git commit -m "feat(feishu): xlsx 导入向导组件（四步，File 留在浏览器重传）"
```

---

## Task 14: 入口接线与重新导入

**Files:**
- Modify: `frontend/src/features/feishu/views/DatasourceListPage.tsx`（新建入口二选一）
- Modify: `frontend/src/features/feishu/views/DatasourceDetailPage.tsx`（重新导入按钮）
- Create: `frontend/src/features/feishu/components/XlsxReimportDialog.tsx`
- Test: `frontend/tests/features/feishu/views/datasource-list-page.test.tsx`（扩展现有）

**Interfaces:**
- Consumes: Task 13 的组件、Task 12 的 client
- Produces: 用户可达的两个入口。

- [ ] **Step 1: 写失败的测试**

`datasource-list-page.test.tsx` 里扩展（它已有 `stubFeishuApi` harness，
**未覆盖的请求会抛错**，所以新端点必须在 harness 里补桩）：

```typescript
  it("「添加数据源」先让人选多维表格还是文件导入", async () => {
    stubFeishuApi(); // 无 xlsx 桩：这一步不该发出任何请求
    render(<DatasourceListPage />);

    await userEvent.click(await screen.findByRole("button", { name: "添加数据源" }));

    // 两个选项都在；**此时还没有打开任何向导**
    expect(screen.getByRole("button", { name: /多维表格/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /文件导入/ })).toBeInTheDocument();
    expect(screen.queryByText(/第 1 步／共 4 步/)).not.toBeInTheDocument();
  });

  it("选文件导入才开 xlsx 向导，且不发多维表格的元数据请求", async () => {
    const calls = stubFeishuApi();
    render(<DatasourceListPage />);
    await userEvent.click(await screen.findByRole("button", { name: "添加数据源" }));
    await userEvent.click(screen.getByRole("button", { name: /文件导入/ }));

    expect(await screen.findByText(/xlsx/)).toBeInTheDocument();
    // 关键：没有打 list_bitable_tables/views/fields 中的任何一个
    const metadataOps = [
      "feishu.datasource.list_bitable_tables",
      "feishu.datasource.list_bitable_views",
      "feishu.datasource.list_bitable_fields",
    ];
    for (const operation of metadataOps) {
      expect(calls.some((call) => call.url.includes(operation))).toBe(false);
    }
  });

  it("选多维表格仍开原来的向导（一行没改）", async () => {
    stubFeishuApi({ tables: [{ tableId: "tbl1", name: "表一" }] });
    render(<DatasourceListPage />);
    await userEvent.click(await screen.findByRole("button", { name: "添加数据源" }));
    await userEvent.click(screen.getByRole("button", { name: /多维表格/ }));

    // 原向导的第 1 步文案原样在（它是「填名称与 Base Token，拉取这个 App 下的数据表」）
    expect(await screen.findByText(/Base Token/)).toBeInTheDocument();
  });
```

`harness.ts` 里给两个新 operation 补桩（**不补的话页面测试一打就抛错**，
`stubFeishuApi` 对未覆盖的请求是抛错而不是静默 404）：

```typescript
  // 在 stubFeishuApi 的 options 里加
  xlsxProbe?: { columns: Array<{ name: string; index: number }> };
  xlsxImport?: { bindings: Array<{ source_key: string; derived: number }> };

  // 在请求分发处加两条分支（照抄相邻分支的形状）
  if (url.includes("feishu.datasource.probe_xlsx_headers")) {
    return jsonResponse({
      code: 0,
      msg: "解析成功",
      data: {
        sheet_name: "Sheet1",
        sheets: ["Sheet1"],
        header_row: 1,
        columns: options.xlsxProbe?.columns ?? [
          { name: "开户行行名", index: 2 },
          { name: "联行号", index: 5 },
        ],
        files: [{ name: "bank_1.xlsx" }],
      },
    });
  }
  if (url.includes("feishu.datasource.import_xlsx")) {
    return jsonResponse({
      code: 0,
      msg: "导入完成",
      data: {
        datasource_id: 9,
        elapsed_ms: 1200,
        files: [{ name: "bank_1.xlsx", rows_read: 5 }],
        bindings: options.xlsxImport?.bindings ?? [],
      },
    });
  }
```

- [ ] **Step 2: 运行确认失败**

```bash
pnpm --dir frontend test -- datasource-list-page
```

预期：FAIL。

- [ ] **Step 3: 接线列表页**

`DatasourceListPage.tsx` 现在 `openCreate()` 直接开多维表格向导。改成先弹一个二选一
（`XlsxImportWizard` 与 `DatasourceTableWizard` 各由一个 state 控制）：

```typescript
  type CreateKind = "bitable" | "xlsx";
  const [createKind, setCreateKind] = useState<CreateKind | null>(null);
  const xlsxClient = useXlsxImportClient();

  function openCreate() {
    setEditError(null);
    setActionError(null);
    setCreateKind(null); // 先让人选
    setPickerOpen(true);
  }
```

`submitWizard` 与新的 `submitXlsxWizard` 都复用既有的 `refreshList()` +
`runPrecheck` 形态（**组件自己不做 invalidate**，回读一律由页面负责——
这是既有约定，别在组件里 `invalidateQueries`）。

- [ ] **Step 4: 加详情页的重新导入**

`DatasourceDetailPage.tsx` 在 `ingestMode === "xlsx_import"` 时渲染一个
「重新导入」按钮，打开 `XlsxReimportDialog`（选文件 → 直接调 `importFiles`，
**不走建源**——绑定已经配好了，表头一致即可，这正是严格口径能成立的前提）。

详情页同时把「视图」那一列的现有逻辑改掉：它现在按 `ingestMode === "pull"` 二分，
xlsx 会落到「—」。改成显示「—（文件导入）」，并给「最近导入时间」一格。

- [ ] **Step 5: 运行确认通过**

```bash
pnpm --dir frontend check
```

预期：PASS（含 `verify:locale-contract`——新增的中文文案要进
`shared/lib/product-locale.ts` 的单语言词条，漏了这一步会红）。

- [ ] **Step 6: 提交**

```bash
git add frontend/src/features/feishu/views/DatasourceListPage.tsx \
        frontend/src/features/feishu/views/DatasourceDetailPage.tsx \
        frontend/src/features/feishu/components/XlsxReimportDialog.tsx \
        frontend/src/shared/lib/product-locale.ts \
        frontend/tests/features/feishu/views/datasource-list-page.test.tsx \
        frontend/tests/features/feishu/views/harness.ts
git commit -m "feat(feishu): 数据源新建入口二选一 + 详情页重新导入"
```

---

# 阶段四：收尾

## Task 15: 文档同步与全量门禁

**Files:**
- Modify: `AGENTS.md`（feishu addon 说明补 xlsx 导入）
- Modify: `docs/architecture/feishu-datasource-console.md`（取数方式补 `xlsx_import`）
- Modify: `docs/architecture/feishu-bank-branch-datasource.md`（把「未实现」的措辞改掉）
- Modify: `frontend/AGENTS.md`（新组件的归位，若该文件有组件清单）

- [ ] **Step 1: 改三处文档**

**外加一件：清理因新增第三种取数方式而陈旧的散文描述。** 这些都不影响行为，
但两处是**用户可见的**，而且没有任何其它任务会碰到它们——不在这里收口就会永久留下：

| 位置 | 问题 |
|---|---|
| `src/addon/feishu/datasource/actions/pull_now.rs:12-13` | 「（默认 30 秒）」——默认值已在 Task 3 改成 60 |
| `src/addon/feishu/datasource/actions/list_datasources.rs:130` | 「取数方式：`push`（多维表格工作流推送）/ `pull`（服务端定时拉取）。」——现在是三种 |
| `src/addon/feishu/datasource/actions/list_datasources.rs:132` | 「`push` 数据源这三项为空」——xlsx 源同样为空 |
| `src/addon/feishu/domain/pull.rs:269` | `NotPullable::PushMode::reason()` 对**任何非 pull** 都说「取数方式是『手工推送』…只有『定时拉取』才拉得动」。而「立即拉取」按钮只看目录权限、不看 `ingestMode`，所以运维对 xlsx 源点它会**看到一句假话**。措辞要覆盖「不是定时拉取」这一类，而不是假定只有 push |
| `docs/architecture/feishu-option-ingest.md:163` 与 `:379` | 仍写「默认 30 秒」；`:163` 的锚点也漂了（引 `config/mod.rs:159-161`，实际已推到 `:161-165`） |

- `AGENTS.md` 里那句「`feishu` addon 现有三个 module：…」后面补一句：
  `datasource` 的取数方式有 `push` / `pull` / `xlsx_import` 三种，
  xlsx 导入见 `docs/architecture/feishu-bank-branch-datasource.md`。
- `feishu-datasource-console.md` 的取数方式说明补 `xlsx_import`（设计 §9 已把它列为
  本设计落地后要还的债）。
- `feishu-bank-branch-datasource.md` 的状态行改成「已实施」，
  并把 §4.7 的「calamine **尚未**加入 `Cargo.toml`」、§5.2 的「这是一行改动」等
  实施后就不再成立的措辞改掉。

- [ ] **Step 2: 跑全量门禁**

```bash
python scripts/run_ci.py full
```

预期：PASS（含 clippy `-D warnings`、前端 `pnpm check`、两套 Playwright）。
**若 clippy 报 `unwrap_used`**：生产代码里一律用
`unwrap_or_else(|poisoned| poisoned.into_inner())` 这类写法，不要 `unwrap()`。

- [ ] **Step 3: 复核设计文档里标记「需复测」的实测数字**

设计 §4.4.2 与 §5.10 的性能数字（filesort 前后耗时、索引体积、缓冲池占比）
**在仓库里没有独立出处**，出自 09-23 那轮本机临时表基准。如果本次实施后
手上有真实环境，**重新实测一遍并回写**；没有就保持现状并保留那条可信度分级说明。

- [ ] **Step 4: 提交**

```bash
git add AGENTS.md frontend/AGENTS.md \
        docs/architecture/feishu-datasource-console.md \
        docs/architecture/feishu-bank-branch-datasource.md
git commit -m "docs(feishu): xlsx 导入落地后同步三处文档与设计状态"
```

---

## 完成判据

全部任务完成后，以下每一条都应成立：

- [ ] `python scripts/run_ci.py full` 绿。
- [ ] `docker run … rust:1.80.1-slim cargo check --all-targets --locked` 绿（MSRV）。
- [ ] `python scripts/run_ci.py --self-test` 绿（集成测试登记完整）。
- [ ] 集成测试 `feishu_xlsx_import_integration` 9 条全过（**含逐绑定空快照守卫与
      逐绑定隔离这两条 `pull.rs` 没有的新行为**）。
- [ ] 建 xlsx 数据源后，**审批外部选项装配仍成功**（Task 8 的端到端回归）。
- [ ] 上传两个表头不一致的文件 → **整份拒绝**，错误里点名文件与缺列。
- [ ] 上传一个「某列整列为空」的文件且该列已被勾选 → 那个绑定的选项**一行不少**，
      其余绑定照常更新，回执带 `skipped_reason`。
- [ ] 同一数据源并发两次导入 → 第二次被挡。
- [ ] 对 `ingest_mode = pull` 的源调导入 → 明确拒绝。

---

## 已知未决（**不在本计划范围**，实施时不要顺手做）

设计 §8 列了 9 条未决项，其中会影响实现的四条：

1. **多 sheet 只读第一张**（现定行为）。要支持选 sheet 是对标多维表格「选视图」的独立改动。
2. **表头行不在第一行**（现定「第一个非全空行」）。若真实文件前面有标题行，
   probe 会取到标题行而用户没有修正入口。**实施时先拿真实文件验一下这个假设**——
   若确有这种形状，要么让 probe 返回前 N 行候选，要么请求里带 `header_row`。
3. **父、子绑定的提交先后**。逐绑定事务下存在「父还是旧数据、子已是新数据」的窗口。
   父值解析不出时读端返回 `40004`（不是回退全量），所以窗口期的真实表现要用真机实测，
   再决定是否把父绑定排到前面提交。
4. **测试夹具的最终形态**。本计划选「Python 脚本自造小夹具 + 提交产物」，
   真实文件只在本地做人工验证。若你更想要真实文件进 CI，那是 `git add -f` 7.4 MB
   二进制的取舍，需单独决定。
