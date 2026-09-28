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
        # 首尾空白只有配上 xml:space="preserve" 才是**显著**的（OOXML 的规定）：否则消费者
        # 可以合法地把它 trim 掉，`header_spaces` 就再也测不出 read_header 的 trim（空测试）。
        # 真实 Excel 写带空格的值时也是这么写的。
        preserve = ' xml:space="preserve"' if value != value.strip() else ""
        return f'<c r="{ref}" t="inlineStr"><is><t{preserve}>{escaped}</t></is></c>'
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


def zip_entry(name: str) -> zipfile.ZipInfo:
    """一个 zip 条目：时间戳固定、压缩方式显式声明。

    `writestr(name, data)` 默认把**当前时间**写进条目头，于是「内容一个字没变，重跑一次脚本
    14 个已提交的二进制全变 dirty」——评审时得逐个解释为什么二进制变了其实什么都没变。
    固定到 zip 的纪元下限 (1980-01-01 00:00:00) 就没有这个噪声：同样内容 → 同样字节。
    """
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    return info


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
        archive.writestr(
            zip_entry("[Content_Types].xml"), CONTENT_TYPES.format(sheet_overrides=overrides)
        )
        archive.writestr(zip_entry("_rels/.rels"), ROOT_RELS)
        archive.writestr(zip_entry("xl/workbook.xml"), workbook)
        archive.writestr(zip_entry("xl/_rels/workbook.xml.rels"), workbook_rels)
        for i, (_, rows) in enumerate(sheets, start=1):
            archive.writestr(zip_entry(f"xl/worksheets/sheet{i}.xml"), sheet_xml(rows))


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
