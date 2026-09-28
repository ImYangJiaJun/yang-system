//! xlsx 解析：嗅探、表头、按列名取值。
//!
//! **只读，不写。** 本模块不碰数据库、不碰网络，是纯函数层，
//! 所以它能在单元测试里被完整覆盖（导入 Action 的那层不行，要真库）。
//!
//! 表头这一层**只认列名**：产出的是「列名 + 1-based 列号」的清单，
//! **不知道 `field_binding` 的存在**——不判断哪一列绑到哪个字段，
//! 也不管列够不够、多一点少一点。那是上层绑定解析的活。
//! 这里只负责把表头如实读出来，并挡住「列名本身就不合法」的输入
//! （重名、空名、超长），以及多文件之间表头不一致这种整体性错误。

#![allow(dead_code)] // 嗅探先落地并自带测试；消费者（xlsx 导入 Action）在后续任务接入。

use calamine::{Cell, DataRef, Reader as _, Xlsx};

/// zip 魔数：xlsx 是 zip，`PK\x03\x04`。
///
/// `allowed_content_types` 只是**客户端自称**（框架 `media.rs` 明写它不能替代
/// 内容校验），所以真正的判定在这里。
pub(crate) fn sniff_is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// 列名长度上限，与 `field_id` 的 `max_length(64)` 对齐。
///
/// 应用层不校验 `field_id` 的长度，所以不在这里挡住的话，
/// 要等插库时才炸，而那时的报错不会告诉你「是列名太长」。
pub(crate) const COLUMN_NAME_LIMIT: usize = 64;

/// 一个文件的表头解析结果。
#[derive(Debug)]
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
#[derive(Debug)]
pub(crate) enum XlsxError {
    /// 魔数不是 zip，根本不是 xlsx。
    NotZip,
    /// zip 结构或 XML 读不出来，附上 calamine 给的原因。
    Unreadable(String),
    /// 第一行没有任何非空表头，或整个工作簿没有 sheet。
    EmptyHeader,
    /// 表头里有两列同名——列名是身份，不唯一就没法按名取值。
    DuplicateColumn { name: String },
    /// 列名超过 [`COLUMN_NAME_LIMIT`]。
    ColumnTooLong { name: String, limit: usize },
    /// 多文件导入时，某个文件的表头集合与第一个文件不一致。
    HeadersDiffer {
        file: String,
        missing: Vec<String>,
        extra: Vec<String>,
    },
}

impl std::fmt::Display for XlsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotZip => write!(f, "文件不是 xlsx（魔数不是 PK\\x03\\x04）"),
            Self::Unreadable(reason) => write!(f, "文件读不出来：{reason}"),
            Self::EmptyHeader => write!(f, "表头是空的"),
            Self::DuplicateColumn { name } => {
                write!(f, "表头有重名列「{name}」——列名是身份，不能重复")
            }
            Self::ColumnTooLong { name, limit } => {
                write!(f, "列名「{name}」超过 {limit} 字符上限")
            }
            Self::HeadersDiffer {
                file,
                missing,
                extra,
            } => {
                write!(
                    f,
                    "文件 {file} 的表头与其它文件不一致：缺 {missing:?}、多 {extra:?}"
                )
            }
        }
    }
}

/// 读出 xlsx 第一张 sheet 的表头行。
///
/// 只认列名，不认识绑定；只读表头那一行，**不扫全表**——探表头要快就快在这里。
pub(crate) fn read_header(bytes: &[u8]) -> Result<SheetHeader, XlsxError> {
    if !sniff_is_zip(bytes) {
        return Err(XlsxError::NotZip);
    }
    // calamine 只能从 Read + Seek 构造。文件是请求作用域的临时文件，
    // 但这一层收 &[u8] 以便单测直接喂夹具字节。
    let cursor = std::io::Cursor::new(bytes);
    let mut workbook =
        Xlsx::new(cursor).map_err(|error| XlsxError::Unreadable(error.to_string()))?;

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
    while let Some(cell) = reader
        .next_cell()
        .map_err(|error| XlsxError::Unreadable(error.to_string()))?
    {
        let (row, column) = cell.get_position();
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
            return Err(XlsxError::DuplicateColumn {
                name: text.to_string(),
            });
        }
        columns.push((text.to_string(), column as usize + 1)); // 1-based 列号
    }

    if columns.is_empty() {
        return Err(XlsxError::EmptyHeader);
    }
    Ok(SheetHeader {
        sheet_name,
        sheet_names,
        header_row,
        columns,
    })
}

/// 校验一组文件的表头**集合完全一致**（顺序可不同——按名取值，不按位置）。
///
/// 不一致时点名哪个文件差哪些列，并整份拒绝：
/// 取交集、或者只导能对上的那个文件，会让用户以为导了 10 万行、实际只有 5 万，
/// 且没有任何提示。
pub(crate) fn require_consistent_headers(
    headers: &[(String, SheetHeader)],
) -> Result<Vec<String>, XlsxError> {
    let Some((_, first)) = headers.first() else {
        return Err(XlsxError::EmptyHeader);
    };
    let reference: std::collections::BTreeSet<&str> = first
        .columns
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    for (file, header) in headers.iter().skip(1) {
        let actual: std::collections::BTreeSet<&str> = header
            .columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        if actual != reference {
            let missing: Vec<String> = reference
                .difference(&actual)
                .map(|name| (*name).to_string())
                .collect();
            let extra: Vec<String> = actual
                .difference(&reference)
                .map(|name| (*name).to_string())
                .collect();
            return Err(XlsxError::HeadersDiffer {
                file: file.clone(),
                missing,
                extra,
            });
        }
    }
    Ok(reference.into_iter().map(str::to_string).collect())
}

/// 单元格取文本。**不做数值/日期推断**（设计 §5.7）：
/// 数值原样按 `DataRef` 的显示取，`联行号` 若被 Excel 存成数值本就会丢前导零，
/// 那是源文件的错，导入器不做「看起来像数字就补零」这类猜测。
///
/// 数值一律走 `to_string()` 而**不是 `{:?}`**——`f64` 的 Debug 会给出 `1.0`，
/// 而真实 Excel 里 `1` 就该是 `1`。
fn cell_text(cell: Cell<DataRef<'_>>) -> String {
    match cell.get_value() {
        DataRef::String(text) => text.clone(),
        DataRef::SharedString(text) => (*text).to_string(),
        DataRef::Float(number) => number.to_string(),
        DataRef::Int(number) => number.to_string(),
        DataRef::Bool(value) => value.to_string(),
        DataRef::DateTime(value) => value.to_string(),
        DataRef::Empty | DataRef::Error(_) | DataRef::DateTimeIso(_) | DataRef::DurationIso(_) => {
            String::new()
        }
    }
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
            header
                .columns
                .iter()
                .all(|(name, _)| !name.trim().is_empty()),
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
    fn shared_strings_and_inline_strings_parse_identically() {
        // 真实 Excel 导出默认走 `xl/sharedStrings.xml`（单元格是 `t="s"` + 指向字符串表的下标），
        // 而其余夹具全是 inlineStr——没有这条，`cell_text` 的 `DataRef::SharedString` 臂
        // 写错（比如返回空串）不会有任何测试转红。
        let inline = read_header(&fixture("bank_1.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let shared = read_header(&fixture("bank_shared_strings.xlsx"))
            .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert_eq!(shared.sheet_name, inline.sheet_name);
        assert_eq!(shared.header_row, inline.header_row);
        assert_eq!(
            shared.columns, inline.columns,
            "同一份内容的两种字符串存储形态，必须解析出同一份表头"
        );
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
        let names =
            require_consistent_headers(&[("bank_1.xlsx".into(), a), ("bank_2.xlsx".into(), b)])
                .unwrap_or_else(|error| panic!("同表头应通过: {error}"));
        assert!(names.contains(&"联行号".to_string()));
    }

    #[test]
    fn consistent_headers_pass_when_column_order_differs() {
        // 上面那条用的 bank_1/bank_2 列顺序**其实是相同的**，所以它证明不了「与顺序无关」——
        // 把 require_consistent_headers 改成按序列比对，它照样绿。
        // `header_reordered` 与 bank_1 列集合相同、顺序不同（联行号在第 2 列），
        // 这条才真的钉住「按名取值、不按位置」。
        let a = read_header(&fixture("bank_1.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let b = read_header(&fixture("header_reordered.xlsx")).unwrap_or_else(|e| panic!("{e}"));
        let ordered = |header: &SheetHeader| -> Vec<String> {
            header
                .columns
                .iter()
                .map(|(name, _)| name.clone())
                .collect()
        };
        assert_ne!(
            ordered(&a),
            ordered(&b),
            "夹具本身就得是不同顺序的，否则这条测试是空的"
        );

        let names = require_consistent_headers(&[
            ("bank_1.xlsx".into(), a),
            ("header_reordered.xlsx".into(), b),
        ])
        .unwrap_or_else(|error| panic!("列集合相同就该通过，与顺序无关: {error}"));
        assert_eq!(names.len(), 8, "8 列都要回来");
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
        assert!(
            message.contains("header_mismatch_two.xlsx"),
            "要点名文件: {message}"
        );
        assert!(message.contains("归属银行"), "要点名缺哪列: {message}");
    }
}
