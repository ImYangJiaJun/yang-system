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
//!
//! 行这一层**同样只认列名**：`read_snapshot` 交出的是「列名 → 单元格文本」，
//! 要哪几列由调用方给（= 全部启用绑定的 `field_id`），但这里**不知道绑定是什么**——
//! 不判断哪一列绑到哪个字段，也不把行投影成 `RawValue`，更不管空快照该不该拒绝。
//! 那些都是 Task 10 导入 Action 的活。分层清楚，这一层才能被完整单测覆盖。
//!
//! **对 calamine 的每一处入口调用（`Xlsx::new` / `worksheet_cells_reader` / `next_cell`）
//! 都过 [`guarded`]**：calamine 0.30.1 读 `t="s"` 单元格时是 `&strings[idx]`，
//! **没有边界检查**（`xlsx/cells_reader.rs`）——一个「有 `t="s"` 却没有
//! `xl/sharedStrings.xml` 部件」的 zip（用户上传即可构造）会让它下标越界 panic。
//! `guarded` 把 panic 变成 [`XlsxError::Unreadable`]：畸形文件的结果是「这一份导入失败」，
//! 而不是一次 panic。

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
    /// 表头里缺了我们需要的列。`missing` 是缺的那些（不是全部要的）。
    HeaderMissingColumns { file: String, missing: Vec<String> },
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
            Self::HeaderMissingColumns { file, missing } => {
                write!(f, "文件 {file} 的表头缺少必需列：{}", missing.join("、"))
            }
        }
    }
}

/// 把一个对 calamine 的调用关进 `catch_unwind`：panic → [`XlsxError::Unreadable`]。
///
/// 为什么需要这层：calamine 0.30.1 读 `t="s"` 单元格时做的是 `&strings[idx]`，
/// **没有边界检查**（`xlsx/cells_reader.rs` 的 `read_v`）。一个 zip 只要写了
/// `<c r="A1" t="s"><v>0</v></c>` 却**不带** `xl/sharedStrings.xml` 部件，`strings`
/// 就是空的，`&strings[0]` 直接下标越界 panic——而这是**用户上传即可触发**的输入面
/// （畸形或缺部件的 xlsx）。仓库没有 `panic = "abort"`，所以后果是丢连接 + 日志里一段
/// panic 栈，不是挂进程：严重度中等，但防护的代价几乎为零。
///
/// `catch_unwind` 要求 `UnwindSafe`，而 `&mut XlsxCellReader` 不是；这里用
/// [`std::panic::AssertUnwindSafe`] 包住。断言成立的理由是我们**不读** panic 之后可能
/// 处于不一致状态的任何内存：一旦捕获到 panic 就整份放弃这个文件，直接返回错误。
/// 这不是 `unsafe`（`unsafe_code = "forbid"` 管不到它），只是对编译器的一个承诺。
///
/// **panic 的默认 hook 仍然会把栈打给 stderr**——这里只是把它变成可返回的错误，
/// **没有静音**。日志里留痕是可接受的；静音反而会藏掉不该藏的 bug。
fn guarded<T>(what: &str, call: impl FnOnce() -> Result<T, XlsxError>) -> Result<T, XlsxError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(result) => result,
        Err(payload) => Err(XlsxError::Unreadable(format!(
            "{what}时解析库内部 panic：{}",
            panic_reason(payload.as_ref())
        ))),
    }
}

/// panic 载荷转文案。`panic!` 的载荷是 `&'static str`，`format!` 出来的是 `String`；
/// 两者都不是就给个兜底——**绝不能在这里再 panic 一次**。
fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "未知原因".to_string()
    }
}

// 三处 calamine 入口（`Xlsx::new` / `worksheet_cells_reader` / `next_cell`）都用
// `guarded(...)` 就地包一个闭包。`next_cell()` 那处没法抽成公共函数：`XlsxCellReader`
// 在 calamine 0.30.1 里**取不到名字**（`mod xlsx` 私有，`pub use
// cells_reader::XlsxCellReader` 也跟着不可见），而 `Cell<DataRef<'a>>` 的返回值必须和
// 游标借出的那个 `'a` 对上，`impl Trait` 也糊不过去。好在它返回的 `'a` 借自 workbook
// 的字符串表、**不是** `&mut reader` 这次调用的借出，所以闭包返回值不牵连捕获的借用。

/// 读出 xlsx 第一张 sheet 的表头行。
///
/// 只认列名，不认识绑定。**代价不是「只读一行」**：`Xlsx::new` 会**急切读完整个
/// `xl/sharedStrings.xml`**（真实 Excel 导出默认就走 sharedStrings 形态），所以这一层是
/// O(整份字符串表)，只是不读 sheetData 的数据行——比全量导入便宜，但不是 O(表头行)。
/// 与 `probe_xlsx_headers` 的模块文档同一口径。
pub(crate) fn read_header(bytes: &[u8]) -> Result<SheetHeader, XlsxError> {
    if !sniff_is_zip(bytes) {
        return Err(XlsxError::NotZip);
    }
    // calamine 只能从 Read + Seek 构造。文件是请求作用域的临时文件，
    // 但这一层收 &[u8] 以便单测直接喂夹具字节。
    let cursor = std::io::Cursor::new(bytes);
    let mut workbook = guarded("打开工作簿", || {
        Xlsx::new(cursor).map_err(|error| XlsxError::Unreadable(error.to_string()))
    })?;

    let sheet_names = workbook.sheet_names().to_vec();
    let sheet_name = sheet_names.first().cloned().ok_or(XlsxError::EmptyHeader)?;

    // 只驱动表头那一行就 `break`（`header_row` 一旦确定，后续行直接跳出循环）——
    // 省下的是 sheetData 的遍历，字符串表那一段已经在 `Xlsx::new` 里读完。
    let mut reader = guarded("打开工作表", || {
        workbook
            .worksheet_cells_reader(&sheet_name)
            .map_err(|error| XlsxError::Unreadable(error.to_string()))
    })?;

    let mut columns: Vec<(String, usize)> = Vec::new();
    let mut header_row = 0usize;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // XlsxCellReader 是**手拉游标、不实现 Iterator**，必须 while let 驱动。
    while let Some(cell) = guarded("遍历单元格", || {
        reader
            .next_cell()
            .map_err(|error| XlsxError::Unreadable(error.to_string()))
    })? {
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

/// 一行：列名 → 单元格文本。只保留 `columns` 里要的列。
pub(crate) type RowValues = std::collections::HashMap<String, String>;

/// 一个文件读了多少行（导入回执要按文件报）。
///
/// `rows_read` **只数非空行**：整行全空的行被丢掉、不计入。xlsx 文件末尾常有大量空行，
/// 把它们当数据行会让「一个文件 0 行」和「一个文件 10 万行空行」无法区分，
/// 而逐绑定的空快照守卫（「这一列全空 → 不派生任何选项」）正是靠这个区分。
#[derive(Debug)]
pub(crate) struct FileRows {
    pub(crate) name: String,
    pub(crate) rows_read: usize,
}

/// 一份快照：多个文件**按顺序拼接**的全部行。
#[derive(Debug)]
pub(crate) struct ImportSnapshot {
    pub(crate) rows: Vec<RowValues>,
    pub(crate) per_file: Vec<FileRows>,
}

/// 读全部数据行。`columns` 是要取回的列名集合（= 全部启用绑定的 `field_id`）。
/// 表头行本身不算数据行；整行全空的行跳过。
///
/// **只认列名，不认识绑定**：这里不知道哪列绑到哪个字段，也不做 `RawValue` 投影。
/// 缺列即拒（[`XlsxError::HeaderMissingColumns`]，点名缺哪列），多列忽略（D9）。
///
/// 内存行为：**只把 `columns` 里的列放进 `RowValues`**，没要的列连 `String` 都不分配；
/// 单元格为空则连键都不插（「缺值」与「空串」同义），整行全空则整行丢弃。
/// 15.4 万行的真实文件下，这三点就是「几十 MB」和「几百 MB」的差别。
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
            return Err(XlsxError::HeaderMissingColumns {
                file: name.clone(),
                missing,
            });
        }

        let cursor = std::io::Cursor::new(bytes.as_slice());
        let mut workbook = guarded("打开工作簿", || {
            Xlsx::new(cursor).map_err(|error| XlsxError::Unreadable(error.to_string()))
        })?;
        let mut reader = guarded("打开工作表", || {
            workbook
                .worksheet_cells_reader(&header.sheet_name)
                .map_err(|error| XlsxError::Unreadable(error.to_string()))
        })?;

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
        while let Some(cell) = guarded("遍历单元格", || {
            reader
                .next_cell()
                .map_err(|error| XlsxError::Unreadable(error.to_string()))
        })? {
            let (row_index, column_index) = cell.get_position();
            let row_number = row_index as usize + 1;
            let column_number = column_index as usize + 1;

            // 表头行及其之前全部跳过。比的是 `header.header_row` 而**不是** `current_row`：
            // `current_row` 在下面会被更新成「当前正在收口的这一行」，拿它比 `<=` 的话，
            // 同一行里第二个及以后的单元格都会落进这里被 continue 掉——
            // 结果只剩每行的第一列能被读到（只勾第一列时测试照样绿，是个很安静的错）。
            if row_number <= header.header_row {
                continue;
            }
            if row_number != current_row {
                // 换行了：把上一行收口。
                if pending {
                    rows_read += 1;
                    rows.push(std::mem::take(&mut current));
                } else {
                    // 整行全空：不置 `pending`，这里把它丢掉，也**不计入 `rows_read`**。
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

        per_file.push(FileRows {
            name: name.clone(),
            rows_read,
        });
    }

    Ok(ImportSnapshot { rows, per_file })
}

/// 单元格取文本。**不做数值推断**（设计 §5.7）：
/// 数值原样按 `DataRef` 的显示取，`联行号` 若被 Excel 存成数值本就会丢前导零，
/// 那是源文件的错，导入器不做「看起来像数字就补零」这类猜测。
///
/// 数值一律走 `to_string()` 而**不是 `{:?}`**——`f64` 的 Debug 会给出 `1.0`，
/// 而真实 Excel 里 `1` 就该是 `1`。
///
/// **日期那一臂曾经是坏的**（两处，都在下面标了「回归」）：
/// `DateTime` 打的是一串裸序列号（`ExcelDateTime` 的 `Display` 就是 `{value}`，
/// calamine 0.30.1 `datatype.rs`），而 `DateTimeIso` / `DurationIso` 被并进空串那一臂
/// ——于是 `t="d"` 的单元格**整列静默读成空**：那一列派生 0 个选项，逐绑定空快照守卫
/// 把这一轮说成「我跳过了它」，没有任何一句话能把它与「我的日期列没导进来」联系起来。
/// 设计 §5.7 只替**数值**丢前导零辩护过，日期整类没被提到。
///
/// 日期**不是推断**：`t="d"` 的单元格里本来就是 ISO 8601 文本，原样透传；
/// 数值型日期（Excel 存的是「序列号 + 日期格式」，calamine 据 number format 判成
/// `DateTime`）交给 calamine 自己的 `as_datetime()` 换成可读形态
/// （`NaiveDateTime` 的 `Display`，形如 `2024-01-01 00:00:00`）。
/// 换算不出来（序列号越界）时退回序列号本身——**宁可难看，不可编**。
fn cell_text(cell: Cell<DataRef<'_>>) -> String {
    match cell.get_value() {
        DataRef::String(text) => text.clone(),
        DataRef::SharedString(text) => (*text).to_string(),
        DataRef::Float(number) => number.to_string(),
        DataRef::Int(number) => number.to_string(),
        DataRef::Bool(value) => value.to_string(),
        // 回归：`t="d"` 的单元格本来就是字符串，**原样透传**。
        DataRef::DateTimeIso(text) => text.clone(),
        // 回归：数值型日期走 `as_datetime()`，不是裸 f64。
        DataRef::DateTime(value) => value
            .as_datetime()
            .map(|stamp| stamp.to_string())
            .unwrap_or_else(|| value.as_f64().to_string()),
        // xlsx 侧产不出它（`cells_reader` 的 `t="d"` 只走 `DateTimeIso`），
        // 一并透传是让这一臂不再是**静默丢弃**——与上面那条同一个道理。
        DataRef::DurationIso(text) => text.clone(),
        DataRef::Empty | DataRef::Error(_) => String::new(),
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
        let snapshot = read_snapshot(
            &[("bank_1.xlsx".to_string(), fixture("bank_1.xlsx"))],
            &want,
        )
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
            &[(
                "numeric_code.xlsx".to_string(),
                fixture("numeric_code.xlsx"),
            )],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));
        let value = snapshot.rows[0]
            .get("联行号")
            .map(String::as_str)
            .unwrap_or("");
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
    fn a_blank_row_between_data_rows_is_not_counted() {
        // `rows_read` 只数**非空行**，而这个区分是承重的：逐绑定空快照守卫（「这一列
        // 整列为空 → 不派生任何选项 → 跳过写库」）与表级守卫都拿它判「本轮是不是空快照」。
        // 空行若被当成数据行，一份「只有空行」的文件会报 `rows_read = N` 而非 0，
        // 守卫**静默失效**——不报错，只是本该拒绝的那一轮照常跑补集停用。
        //
        // 夹具里的空行是「每个单元格都有、但都是空串」（不是整行缺单元格）：后者在 XML
        // 里根本不产生单元格，钉不住 `cell_text` 那条空串分支。
        let want = vec!["开户行行名".to_string(), "联行号".to_string()];
        let snapshot = read_snapshot(
            &[(
                "blank_row_between.xlsx".to_string(),
                fixture("blank_row_between.xlsx"),
            )],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));
        assert_eq!(snapshot.rows.len(), 2, "两份数据行");
        assert_eq!(
            snapshot.per_file[0].rows_read, 2,
            "夹在中间的那个整行空行**不计入** rows_read"
        );
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
            &[(
                "header_missing_column.xlsx".to_string(),
                fixture("header_missing_column.xlsx"),
            )],
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
            &[(
                "header_extra_column.xlsx".to_string(),
                fixture("header_extra_column.xlsx"),
            )],
            &want,
        )
        .unwrap_or_else(|error| panic!("多列应被忽略: {error}"));
        assert_eq!(snapshot.rows.len(), 1);
    }

    #[test]
    fn a_shared_string_cell_without_the_string_table_is_an_error_not_a_panic() {
        // 畸形输入（用户上传即可触发）：单元格写成 `t="s"` + 下标，包里却**没有**
        // `xl/sharedStrings.xml` 部件（夹具里连 `[Content_Types].xml` 和 rels 的声明都还在）。
        // calamine 0.30.1 在 `src/xlsx/cells_reader.rs` 里是 `&strings[idx]`、**没有边界检查**
        // ——空字符串表 + 下标 0 就是一次下标越界 panic。
        //
        // 这条断言为什么**就是**「没 panic」的证据：`read_header` / `read_snapshot` 把对
        // calamine 的入口调用（`Xlsx::new` / `worksheet_cells_reader` / `next_cell`）包进了
        // `guarded`，panic 在那一层被转成 `Err(Unreadable)`。所以能走到下面 `matches!` 这一行，
        // 本身就说明 panic 已经被接住了——否则测试线程早在 `read_header` 里就因 panic 失败，
        // 压根执行不到这两条断言。
        let want = vec!["联行号".to_string()];
        let bytes = fixture("shared_strings_missing.xlsx");

        let header_error = read_header(&bytes)
            .err()
            .unwrap_or_else(|| panic!("缺 sharedStrings 部件必须报错，不能假装读成功"));
        assert!(
            matches!(&header_error, XlsxError::Unreadable(_)),
            "实际: {header_error:?}"
        );

        let error = read_snapshot(&[("shared_strings_missing.xlsx".to_string(), bytes)], &want)
            .err()
            .unwrap_or_else(|| panic!("缺 sharedStrings 部件必须报错"));
        assert!(
            matches!(&error, XlsxError::Unreadable(_)),
            "实际: {error:?}"
        );
    }

    #[test]
    fn read_snapshot_swallows_a_missing_string_table_in_the_data_rows() {
        // 上一条钉的是 `read_header` 那道守卫（夹具的表头自己就是 `t="s"`，它在
        // `read_header` 里就 panic 了）。`read_snapshot` 里那道 `next_cell` 守卫
        // **因此是零覆盖的**——把它删掉，全套测试照样绿。
        //
        // 这份夹具把两行拆开：表头写成 inlineStr，数据行的**第一个**单元格写成数值。
        // `read_header` 读到那个数值时按「换行了」跳出循环，正常返回；`read_snapshot`
        // 继续往后读到 B2 的 `t="s"`，才撞上同一处越界。
        let bytes = fixture("shared_strings_missing_data_row.xlsx");

        // 前提：这一份必须**读得进** read_header，否则它和上面那份夹具没有区别，
        // 这条测试就退化成同一道守卫的第二个副本（对 read_snapshot 那道依然零覆盖）。
        let header = read_header(&bytes)
            .unwrap_or_else(|error| panic!("表头是 inlineStr，应能正常解析: {error}"));
        assert_eq!(
            header.header_row, 1,
            "表头必须仍然被认成第 1 行——下沉说明数据行的第一个单元格没被当成换行"
        );

        let want = vec!["开户行行名".to_string()];
        let error = read_snapshot(
            &[("shared_strings_missing_data_row.xlsx".to_string(), bytes)],
            &want,
        )
        .err()
        .unwrap_or_else(|| panic!("数据行缺 sharedStrings 部件必须报错，不能假装读成功"));
        assert!(
            matches!(&error, XlsxError::Unreadable(_)),
            "实际: {error:?}"
        );
    }

    #[test]
    fn date_cells_read_as_readable_text_not_raw_serial_numbers() {
        // 日期两臂曾经都是坏的：`DataRef::DateTime` 打的是裸序列号（`ExcelDateTime` 的
        // `Display`），`DataRef::DateTimeIso` 被并进空串那一臂——`t="d"` 的单元格整列
        // 静默读成空，那一列派生 0 个选项，用户只在「这一轮跳过了它」里看见结果。
        // 设计 §5.7 只替数值丢前导零辩护过，日期整类没被提到。
        let want = vec!["开户日".to_string()];
        let snapshot = read_snapshot(
            &[("date_cells.xlsx".to_string(), fixture("date_cells.xlsx"))],
            &want,
        )
        .unwrap_or_else(|error| panic!("应能解析: {error}"));

        assert_eq!(snapshot.rows.len(), 2, "两行都是数据行");
        // 数值型日期（`s="1"` + 内置日期格式）：45292 = 2024-01-01。
        // 断言写成**精确值**而不是「不含数字」：后者对一串乱码也成立。
        assert_eq!(
            snapshot.rows[0].get("开户日").map(String::as_str),
            Some("2024-01-01 00:00:00"),
            "数值型日期要换成可读形态，不是 45292 这种序列号"
        );
        // `t="d"` 的单元格：本来就是 ISO 文本，原样透传。
        assert_eq!(
            snapshot.rows[1].get("开户日").map(String::as_str),
            Some("2024-01-15T00:00:00"),
            "ISO 形态的日期不能读成空串——整列变空会被读成「这一列没数据」"
        );
    }
}
