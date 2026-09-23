//! 「字段名 ↔ 表 schema」的离线对账——结构防线第二件。
//!
//! # 为什么它能离线跑
//!
//! DSL 的字段校验是**急切且离线**的：`select_fields` / `where_eq` / `where_in` /
//! `order_by` 在**构造查询那一刻**就查表定义（`table_query/filters.rs` 的
//! `validate_read_field` / `validate_filter_field` / `validate_order_field`），
//! 不需要数据库。所以「某个站点用了这张表上没有的列」可以在测试里逐个站点复现成
//! `FieldNotFound`(400) 或 `FieldPermissionDenied`(403)——而那正是本仓四次同类 bug
//! 的形态（出站读端查已删列、详情页按 source_key 查一张没有该列的表……）。
//!
//! # 它比投影契约更基础
//!
//! 投影契约管「后端 emit ↔ 前端 read」，两边都是**手写的**（可以一起改错）。
//! 这一层管「代码里用的字段名 ↔ `table_spec()` 这份唯一事实源」——拿的是声明本身。
//!
//! # 为什么是扫源码，不是维护一张清单
//!
//! 现有一百来个站点分布在十几个文件里，手写清单必然腐烂：**漏掉一个站点＝防线失效**，
//! 而且腐烂是静默的。扫描是穷尽的，新写的站点自动进入射程。
//!
//! # 归属怎么定（决定检查的严格程度）
//!
//! 语句内最近的那个 `datasources()` / `datasource_fields()` / `options()` 决定这个站点
//! 作用在哪张表上：
//!
//! - **能定 → 严格**：列必须存在，且按动词要求的能力位必须开着
//!   （`where_*` 要 `filterable`，`order_by` 要 `sortable`——fail-closed 的就是这两位）。
//! - **定不了 → 宽松**：列必须至少在**三张表之一**上存在（抓拼写，放行跨表）。
//!   共享 helper 的表由调用方给（例如 `option_write::apply_option_rows` 的
//!   `options: &Repository`），静态解析不到。
//!
//! 宽松那一档的站点会被列出来——**不静默降级**：降级本身要看得见。
#![cfg(test)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use yang_base::definition::TableSpec;

/// 三张表：`(表名, 声明, 代码里认它的接收者写法)`。
///
/// 接收者写法是**字面匹配**的锚：语句里出现它，就把这个站点归到这张表。
fn tables() -> Vec<(&'static str, TableSpec, &'static str)> {
    let spec = |result: Result<TableSpec, yang_base::BaseError>, what: &str| {
        result.unwrap_or_else(|error| panic!("{what} 的表声明应有效: {error}"))
    };
    vec![
        (
            "feishu_datasource",
            spec(
                crate::addon::feishu::datasource::table::table_spec(),
                "表级行",
            ),
            "datasources()",
        ),
        (
            "feishu_datasource_field",
            spec(
                crate::addon::feishu::datasource::domain::field_table::table_spec(),
                "字段绑定",
            ),
            "datasource_fields()",
        ),
        (
            "feishu_option",
            spec(crate::addon::feishu::option::table::table_spec(), "选项"),
            "options()",
        ),
    ]
}

/// 会做字段校验的动词，以及它对能力位的要求。
///
/// 只收**第一个字符串字面量是字段名**的那些（`where_tree` 收的是条件树，跳过）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verb {
    /// 读投影：列必须存在（可读性默认对所有人开放，不额外要求能力位）。
    SelectFields,
    /// 筛选：列必须存在且 `filterable`。
    WhereEq,
    /// 同 `WhereEq`。
    WhereIn,
    /// 同 `WhereEq`。
    WhereNe,
    /// 排序：列必须存在且 `sortable`。
    OrderBy,
    /// 写：`Record::insert` 的键必须存在（不存在的键 ⇒ 静默写不进去 / INSERT 被拒）。
    RecordInsert,
    /// 读：`Record::require` 的键必须存在（不存在的键 ⇒ 运行期 MissingField）。
    RecordRequire,
    /// 同 `RecordRequire`。
    RecordOptional,
}

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Verb::SelectFields => "select_fields",
            Verb::WhereEq => "where_eq",
            Verb::WhereIn => "where_in",
            Verb::WhereNe => "where_ne",
            Verb::OrderBy => "order_by",
            Verb::RecordInsert => "insert",
            Verb::RecordRequire => "require",
            Verb::RecordOptional => "optional",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "select_fields" => Some(Verb::SelectFields),
            "where_eq" => Some(Verb::WhereEq),
            "where_in" => Some(Verb::WhereIn),
            "where_ne" => Some(Verb::WhereNe),
            "order_by" => Some(Verb::OrderBy),
            "insert" => Some(Verb::RecordInsert),
            "require" => Some(Verb::RecordRequire),
            "optional" => Some(Verb::RecordOptional),
            _ => None,
        }
    }

    /// 这个动词需要哪个能力位（`None` = 只要求列存在）。
    fn requires(self) -> Option<&'static str> {
        match self {
            Verb::SelectFields => None,
            Verb::WhereEq | Verb::WhereIn | Verb::WhereNe => Some("filterable"),
            Verb::OrderBy => Some("sortable"),
            // `Record` 的键只校验**存在**：读写权限由框架的角色判定兜住
            // （secret 列声明成 `readable_by([system])`，而四个仓库入口恒以该角色发起），
            // 在这条扫描里再判一次是重复，而且会变成空断言。
            Verb::RecordInsert | Verb::RecordRequire | Verb::RecordOptional => None,
        }
    }

    /// 这个动词是否只对 `Record` 变量生效（靠接收者名筛掉 map / header 的同名方法）。
    fn needs_record_receiver(self) -> bool {
        matches!(
            self,
            Verb::RecordInsert | Verb::RecordRequire | Verb::RecordOptional
        )
    }
}

/// 一个站点：某文件某行用某个动词碰了某个字段名。
#[derive(Debug)]
struct Site {
    file: String,
    line: usize,
    verb: Verb,
    field: String,
    /// 解析出的归属表 `(表名, 接收者写法)`；`None` = 定不下来。
    owner: Option<(String, String)>,
}

impl Site {
    fn where_(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// 源码与其「结构掩码」。
///
/// 掩码里：注释字符与**字符串内容**都换成空格，换行与引号保留。这样定位动词、
/// 配平括号、找语句边界都不会被注释或字符串里的 `(` `;` `//` 骗到，而行号仍对得上。
struct Masked {
    text: String,
    mask: String,
}

fn mask(source: &str) -> Masked {
    let bytes = source.as_bytes();
    let mut mask: Vec<u8> = bytes.to_vec();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    mask[index] = b' ';
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while index < bytes.len() {
                    if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                        depth += 1;
                        mask[index] = b' ';
                        mask[index + 1] = b' ';
                        index += 2;
                        continue;
                    }
                    if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        depth -= 1;
                        mask[index] = b' ';
                        mask[index + 1] = b' ';
                        index += 2;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    if bytes[index] != b'\n' {
                        mask[index] = b' ';
                    }
                    index += 1;
                }
            }
            b'"' => {
                // 普通字符串字面量：把**内容**抹掉，引号留着（引号的位置仍可定位）。
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        mask[index] = b' ';
                        if index + 1 < bytes.len() {
                            mask[index + 1] = b' ';
                        }
                        index += 2;
                        continue;
                    }
                    if bytes[index] == b'"' {
                        break;
                    }
                    if bytes[index] != b'\n' {
                        mask[index] = b' ';
                    }
                    index += 1;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    Masked {
        text: source.to_string(),
        mask: String::from_utf8(mask).unwrap_or_else(|error| panic!("掩码应是 UTF-8: {error}")),
    }
}

impl Masked {
    fn line_of(&self, offset: usize) -> usize {
        self.mask[..offset].matches('\n').count() + 1
    }

    /// 从 `open`（掩码里的 `(`）找到配平的 `)`，返回它的偏移。
    fn close_paren(&self, open: usize) -> Option<usize> {
        let bytes = self.mask.as_bytes();
        let mut depth = 0usize;
        for (offset, byte) in bytes.iter().enumerate().skip(open) {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 从 `open`（掩码里的 `{`）找到配平的 `}`，返回它的偏移。
    fn close_brace(&self, open: usize) -> Option<usize> {
        let bytes = self.mask.as_bytes();
        let mut depth = 0usize;
        for (offset, byte) in bytes.iter().enumerate().skip(open) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 语句起点：掩码里 `match_start` 之前最近的 `;` `{` `}`。
    fn statement_start(&self, match_start: usize) -> usize {
        let bytes = self.mask.as_bytes();
        let mut offset = match_start;
        while offset > 0 {
            offset -= 1;
            if matches!(bytes[offset], b';' | b'{' | b'}') {
                return offset + 1;
            }
        }
        0
    }

    /// 语句内所有字符串字面量的内容（按出现顺序）。
    fn literals_between(&self, start: usize, end: usize) -> Vec<String> {
        let text = self.text.as_bytes();
        let mut out = Vec::new();
        let mut offset = start;
        while offset < end && offset < text.len() {
            if text[offset] != b'"' {
                offset += 1;
                continue;
            }
            let mut value = String::new();
            offset += 1;
            while offset < text.len() {
                match text[offset] {
                    b'\\' => {
                        out.push(String::new());
                        let escape = text.get(offset + 1).copied().unwrap_or(b'\\');
                        let decoded = match escape {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            other => other as char,
                        };
                        value.push(decoded);
                        offset += 2;
                        continue;
                    }
                    b'"' => {
                        offset += 1;
                        break;
                    }
                    other => {
                        value.push(other as char);
                        offset += 1;
                    }
                }
            }
            out.push(value);
        }
        out
    }
}

/// 扫一个文件里所有会做字段校验的站点。
/// 函数体跨度：`(起点, 终点, 这段里出现过的锚点)`。
///
/// 为什么需要它：本仓最常见的形态是
/// ```ignore
/// let mut query = context.options().query().select_fields(&[..])?;
/// // ……若干行……
/// query = query.where_eq("source_key", ..)?;
/// ```
/// 语句窗口只看得见第二行，于是归属解析不出来——而**历史上那个 bug 正是这个形状**
/// （在表级行的 query 上筛属于绑定层的列）。所以先在**函数体**里数锚点：
/// 只有一个锚点 ⇒ 归属无歧义；有多个才退回语句窗口。
fn function_spans(
    masked: &Masked,
    tables: &[(&'static str, TableSpec, &'static str)],
) -> Vec<(usize, usize, Vec<&'static str>)> {
    let mask = masked.mask.as_bytes();
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while let Some(hit) = masked.mask[offset..].find("fn ") {
        let start = offset + hit;
        let Some(brace) = mask[start..].iter().position(|byte| *byte == b'{') else {
            break;
        };
        let open = start + brace;
        let Some(close) = masked.close_brace(open) else {
            break;
        };
        let anchors: Vec<&'static str> = tables
            .iter()
            .filter(|(_, _, anchor)| masked.mask[open..close].contains(anchor))
            .map(|(_, _, anchor)| *anchor)
            .collect();
        spans.push((open, close, anchors));
        offset = open + 1;
    }
    spans
}

/// 归属解析：**函数体唯一锚点**优先，其次语句窗口；都不行则 `None`（走宽松检查）。
fn resolve_owner(
    masked: &Masked,
    spans: &[(usize, usize, Vec<&'static str>)],
    bindings: &[(usize, String, String)],
    site: usize,
    receiver: Option<&str>,
    tables: &[(&'static str, TableSpec, &'static str)],
) -> Option<(String, String)> {
    // ⓪ 写调用的锚最准：`record` 被交给了哪张表的 `insert/update`，它就是哪张表的。
    if let Some(receiver) = receiver {
        if let Some((_, _, table)) = spans
            .iter()
            .rfind(|(open, close, _)| *open <= site && site < *close)
            .and_then(|(open, _, _)| {
                bindings
                    .iter()
                    .find(|(binding_open, ident, _)| binding_open == open && ident == receiver)
            })
        {
            let anchor = tables
                .iter()
                .find(|(name, _, _)| name == table)
                .map(|(_, _, anchor)| (*anchor).to_string())
                .unwrap_or_default();
            return Some((table.clone(), anchor));
        }
    }
    let table_of = |anchor: &str| {
        tables
            .iter()
            .find(|(_, _, candidate)| *candidate == anchor)
            .map(|(name, _, _)| name.to_string())
    };
    // 最内层的那个函数体（`next_back` 取最后找到的包含它的跨度）
    if let Some((_, _, anchors)) = spans
        .iter()
        .rfind(|(open, close, _)| *open <= site && site < *close)
    {
        if anchors.len() == 1 {
            return table_of(anchors[0]).map(|table| (table, anchors[0].to_string()));
        }
    }
    let statement = masked.statement_start(site);
    let prefix = &masked.mask[statement..site];
    let hits: Vec<&'static str> = tables
        .iter()
        .map(|(_, _, anchor)| *anchor)
        .filter(|anchor| prefix.contains(anchor))
        .collect();
    if hits.len() == 1 {
        return table_of(hits[0]).map(|table| (table, hits[0].to_string()));
    }
    None
}

/// 这个文件里所有 `Record` 变量的名字（`let [mut] <ident> = Record::new()`）。
///
/// 有它才能把 `record.insert("x")` 与 `fields.insert("后".to_string())`（i18n map）、
/// `.insert("x-ogw-ratelimit-reset".to_string())`（HTTP 头）区分开——后者不是列名，
/// 误判就是**假红**，而假红会让人把整条扫描删掉。
///
/// 名字取自源码，所以新起一个名字的 `Record` 变量会自动进集合；
/// `the_record_receivers_...` 那条测试再把它钉住，免得连命名都换了却没人注意。
fn record_receivers(masked: &Masked) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut offset = 0usize;
    while let Some(hit) = masked.mask[offset..].find("Record::new()") {
        let at = offset + hit;
        let start = masked.statement_start(at);
        let prefix = &masked.mask[start..at];
        if let Some(eq) = prefix.rfind('=') {
            let ident = prefix[..eq]
                .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .rfind(|part| !part.is_empty() && *part != "mut" && *part != "let");
            if let Some(ident) = ident {
                out.insert(ident.to_string());
            }
        }
        offset = at + "Record::new()".len();
    }
    out
}

/// 写调用的锚：`(函数体起点, Record 变量名) → 表名`。
///
/// 为什么不能用「函数里唯一锚点」那招：写路径的函数**同时碰两张表**是常态
/// （建源一次写表级行 + N 条绑定，拉取一次回写绑定 + 选项）。但写调用本身带着锚——
/// `<仓库>.query()….insert_in_tx(&mut tx, row)`——所以 `row` 属于哪张表是**确定的**，
/// 不需要猜。
///
/// 定不下来的只剩共享 helper（表由调用方传，例如 `option_write::apply_option_rows`）。
/// `#[cfg(test)] mod ... { ... }` 的跨度。
///
/// 扫测试代码只会制造噪声：夹具里的 `row.insert("source_key", …)` 构造的是**数据**，
/// 不是出站路径——它没有写调用可锚，必然落进宽松档。而宽松档一旦被噪声塞满，
/// 它就不再是信号了。
fn test_spans(masked: &Masked) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while let Some(hit) = masked.mask[offset..].find("#[cfg(test)]") {
        let start = offset + hit;
        let Some(brace) = masked.mask[start..].find('{') else {
            break;
        };
        let open = start + brace;
        let Some(close) = masked.close_brace(open) else {
            break;
        };
        spans.push((start, close));
        offset = close;
    }
    spans
}

/// `offset` 所在的**最内层**函数体的 `{` 偏移。
fn enclosing_function_open(masked: &Masked, offset: usize) -> Option<usize> {
    let mask = masked.mask.as_bytes();
    let mut found = None;
    let mut cursor = 0usize;
    while let Some(hit) = masked.mask[cursor..].find("fn ") {
        let start = cursor + hit;
        let Some(brace) = mask[start..].iter().position(|byte| *byte == b'{') else {
            break;
        };
        let open = start + brace;
        if open >= offset {
            break;
        }
        found = Some(open);
        cursor = open + 1;
    }
    found
}

fn write_bindings(
    masked: &Masked,
    receivers: &BTreeSet<String>,
    tables: &[(&'static str, TableSpec, &'static str)],
) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    for verb in ["insert_in_tx", "update_in_tx", "insert_returning_id_in_tx"] {
        let needle = format!(".{verb}(");
        let mut offset = 0usize;
        while let Some(hit) = masked.mask[offset..].find(&needle) {
            let at = offset + hit;
            offset = at + needle.len();
            let statement = masked.statement_start(at);
            let prefix = &masked.mask[statement..at];
            let Some((table, _)) = tables
                .iter()
                .filter(|(_, _, anchor)| prefix.contains(anchor))
                .map(|(name, _, anchor)| (name.to_string(), anchor.to_string()))
                .next_back()
            else {
                continue;
            };
            let Some(close) = masked.close_paren(at + needle.len() - 1) else {
                continue;
            };
            let args = &masked.mask[at + needle.len()..close];
            // 函数体起点：用它 + 变量名当键（同名变量在不同函数里可以指向不同表）
            let Some(function_open) = enclosing_function_open(masked, statement) else {
                continue;
            };
            for ident in receivers {
                if args.contains(ident.as_str()) {
                    out.push((function_open, ident.clone(), table.clone()));
                }
            }
        }
    }
    out
}

/// 扫一段源码里所有会做字段校验的站点。
///
/// 收源码而不是收路径，是为了让**正对照**（喂一段故意写错的源码，断言它被抓到）
/// 成为可能——扫描器坏了比扫描报错更危险。
fn scan_source(
    relative: &str,
    source: &str,
    tables: &[(&'static str, TableSpec, &'static str)],
) -> Vec<Site> {
    let masked = mask(source);
    let bytes = masked.mask.as_bytes();
    let spans = function_spans(&masked, tables);
    let receivers = record_receivers(&masked);
    let bindings = write_bindings(&masked, &receivers, tables);
    let tests = test_spans(&masked);

    let mut sites = Vec::new();
    let mut offset = 0usize;
    while let Some(relative_hit) = masked.mask[offset..].find(".") {
        let dot = offset + relative_hit;
        let name_start = dot + 1;
        let name_end = bytes[name_start..]
            .iter()
            .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .map(|length| name_start + length)
            .unwrap_or(bytes.len());
        let name = &masked.mask[name_start..name_end];
        let Some(verb) = Verb::from_name(name) else {
            offset = name_end.max(dot + 1);
            continue;
        };
        if bytes.get(name_end) != Some(&b'(') {
            offset = name_end;
            continue;
        }
        // 接收者标识 = `.` 之前紧邻的那串标识符。
        let receiver = masked.mask[..dot]
            .trim_end()
            .rsplit(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .next()
            .unwrap_or_default()
            .to_string();
        if verb.needs_record_receiver() && !receivers.contains(&receiver) {
            // 不是 `Record` 变量上的同名方法（map / HTTP 头）：跳过。
            offset = name_end;
            continue;
        }
        let Some(close) = masked.close_paren(name_end) else {
            offset = name_end;
            continue;
        };
        let args_start = name_end + 1;
        let literals = masked.literals_between(args_start, close);
        let fields: Vec<String> = if verb == Verb::SelectFields {
            literals
        } else {
            literals.into_iter().take(1).collect()
        };
        if tests
            .iter()
            .any(|(start, close)| *start <= dot && dot < *close)
        {
            // 测试夹具：不是出站路径，跳过（理由见 `test_spans`）。
            offset = close + 1;
            continue;
        }
        let owner = resolve_owner(&masked, &spans, &bindings, dot, Some(&receiver), tables);
        let line = masked.line_of(dot);
        for field in fields {
            sites.push(Site {
                file: relative.to_string(),
                line,
                verb,
                field,
                owner: owner.clone(),
            });
        }
        offset = close + 1;
    }
    sites
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("应可读 {}: {error}", directory.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("目录项应可读: {error}"));
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// 字段在表声明里存不存在、能力位开没开。
fn field_state(spec: &TableSpec, name: &str) -> Option<(bool, bool)> {
    spec.fields
        .iter()
        .find(|field| field.name.as_str() == name)
        .map(|field| (field.access.filterable, field.access.sortable))
}

fn addon_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/addon/feishu")
}

/// **轴二：枚举取值域。** 契约里 `enforced_by: backend` 的取值域与表声明逐字一致。
///
/// 与主断言的分工：主断言管**列名与能力位**（能不能用），这一条管**取值**（值合不合法）。
/// 两者都是 fail-closed：值越界在运行期同样被拒，而那时已经晚了。
#[test]
fn the_enum_domains_in_the_contract_match_the_table_declarations() {
    let contract = crate::addon::feishu::domain::projection_contract::contract();
    for (name, spec, _) in tables() {
        crate::addon::feishu::domain::projection_contract::assert_enum_domains_match(
            &contract, name, &spec,
        );
    }
}

/// **主断言：每个站点的字段名都能通过它那张表的声明。**
///
/// 这一条跨三个文件都不需要数据库——DSL 的校验本来就是急切离线的。
#[test]
fn every_field_used_on_a_table_is_declared_there_with_the_required_capability() {
    let root = addon_root();
    let tables = tables();
    let mut violations = Vec::new();
    let mut weak = BTreeSet::new();
    let mut checked = 0usize;

    for path in rust_files(&root) {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("应可读 {}: {error}", path.display()));
        for site in scan_source(&relative, &source, &tables) {
            checked += 1;
            let required = site.verb.requires();
            match &site.owner {
                Some((table, _)) => {
                    let (_, spec, _) = tables
                        .iter()
                        .find(|(name, _, _)| name == table)
                        .unwrap_or_else(|| panic!("归属表 {table} 不在三张表里"));
                    match field_state(spec, &site.field) {
                        None => violations.push(format!(
                            "{}  {} 用了 {table} 上**不存在**的列 `{}` ⇒ 运行期 FieldNotFound(400)",
                            site.where_(),
                            site.verb.name(),
                            site.field
                        )),
                        Some((filterable, sortable)) => {
                            let ok = match required {
                                None => true,
                                Some("filterable") => filterable,
                                Some("sortable") => sortable,
                                Some(other) => panic!("未知能力位 {other}"),
                            };
                            if !ok {
                                violations.push(format!(
                                    "{}  {} 用了 {table} 上未声明 `{}` 的列 `{}` ⇒ 运行期 \
                                     FieldPermissionDenied(403)（DSL 是 fail-closed）",
                                    site.where_(),
                                    site.verb.name(),
                                    required.unwrap_or("?"),
                                    site.field
                                ));
                            }
                        }
                    }
                }
                None => {
                    // 归属定不下来：只要求它在三张表之一上存在（抓拼写）。
                    let known = tables
                        .iter()
                        .any(|(_, spec, _)| field_state(spec, &site.field).is_some());
                    if !known {
                        violations.push(format!(
                            "{}  {} 用了三张表上都不存在的列 `{}`（归属也没解析出来）",
                            site.where_(),
                            site.verb.name(),
                            site.field
                        ));
                    }
                    weak.insert(format!(
                        "{}  {} `{}`",
                        site.where_(),
                        site.verb.name(),
                        site.field
                    ));
                }
            }
        }
    }

    assert!(
        checked > 0,
        "一个站点都没扫到——扫描器坏了比扫描报错更危险，先修它"
    );
    if !violations.is_empty() {
        panic!(
            "字段名与表声明对不上（共 {} 条）：\n{}",
            violations.len(),
            violations.join("\n")
        );
    }
}

/// 归属解析不出来、只做了「存在」检查的站点，**必须恰好是记录下来的这一批**。
///
/// 宽松档不是「允许」，而是**枚举下来的例外**（同投影契约里的 `backend_only`）。
/// 少了它会怎样：新写的站点一旦落进宽松档，防线就在那里静默降级——而静默降级
/// 正是这一整类 bug 能活下来的方式。
///
/// 键是 `文件:动词:字段`（不含行号：行号会随无关改动漂移，那会让这条断言变成噪音）。
#[test]
fn the_weakly_checked_sites_are_exactly_the_recorded_ones() {
    let root = addon_root();
    let tables = tables();
    let mut weak = BTreeSet::new();
    for path in rust_files(&root) {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("应可读 {}: {error}", path.display()));
        for site in scan_source(&relative, &source, &tables) {
            if site.owner.is_none() {
                weak.insert(format!("{}:{}:{}", site.file, site.verb.name(), site.field));
            }
        }
    }

    // 为什么是这些：`option_write.rs` 的表由**调用方**传进来（`options: &Repository`），
    // 静态解析不到；其余几处的函数体同时碰了两张表（既有绑定表又有选项表），
    // 函数级锚点不唯一、语句窗口也没有锚点——都退到「至少在三张表之一上存在」。
    // 它们用的列（`option_id` / `source_key` / `parent_key` / `title` / `id`）都确实
    // 在那张表上，所以目前没有假红；**但这份清单只能靠改代码变短，不能悄悄变长**。
    // 弱档按**文件**记账，理由写在文件上：逐站点钉 50 多条三元组只会变成噪声，
    // 而「有没有**新文件**掉进弱档」才是静默降级的真正入口。
    let expected: BTreeSet<&str> = [
        // 共享 helper：表由调用方传进来（`options: &Repository`），静态解析不到。
        "domain/option_write.rs",
        "domain/pull.rs",
        "option/actions/approval_options.rs",
        "option/actions/upsert_options.rs",
        "datasource/actions/list_datasources.rs",
        "datasource/actions/rotate_token.rs",
    ]
    .into_iter()
    .collect();
    let weak_files: BTreeSet<&str> = weak
        .iter()
        .filter_map(|entry| entry.split(':').next())
        .collect();
    let added: Vec<&&str> = weak_files.difference(&expected).collect();
    let gone: Vec<&&str> = expected.difference(&weak_files).collect();
    assert!(
        added.is_empty(),
        "有站点落进了宽松档而没人注意到（防线在静默降级）：{added:?}
         要么把归属解析做准，要么在 `expected` 里记账并写明理由"
    );
    assert!(
        gone.is_empty(),
        "这些站点已经不在宽松档里了（好事）——把它从 `expected` 里删掉，让清单保持真实：{gone:?}"
    );
}

/// **正对照**：拿一段故意写错的源码喂给扫描器，它必须报出来。
///
/// 没有这条，扫描器「不报错」这件事就分不清是「代码干净」还是「扫描器瞎了」——
/// 而一个瞎掉的对账器比没有更危险：它给人已经查过的错觉。
#[test]
fn the_scanner_actually_flags_a_field_that_is_not_on_that_table() {
    let tables = tables();
    let cases = [
        // ① 表级行上没有 source_key（它在绑定表上）——**历史 bug 的形状**
        (
            "demo.rs",
            r#"fn demo(ctx: C) {
    let _ = ctx.datasources().query().where_eq("source_key", v);
}"#,
            "不存在",
        ),
        // ② 列存在但能力位没开（表级行的 status 只开了 filterable，不是 sortable）
        (
            "demo.rs",
            r#"fn demo(ctx: C) {
    let _ = ctx.datasources().query().order_by("status", o);
}"#,
            "未声明",
        ),
    ];
    for (file, source, expected) in cases {
        let sites = scan_source(file, source, &tables);
        assert_eq!(sites.len(), 1, "应恰好扫到一个站点：{sites:?}");
        let site = &sites[0];
        assert!(
            site.owner.is_some(),
            "这段源码的归属应解析得出来（否则这条正对照测的是宽松档）"
        );
        let owner = site
            .owner
            .as_ref()
            .unwrap_or_else(|| panic!("这条正对照要求归属能解析出来"));
        let (table, spec, _) = tables
            .iter()
            .find(|(name, _, _)| *name == owner.0)
            .unwrap_or_else(|| panic!("归属表应存在"));
        let state = field_state(spec, &site.field);
        let rejected = match (state, site.verb.requires()) {
            (None, _) => "不存在",
            (Some((filterable, sortable)), required) => {
                let ok = match required {
                    None => true,
                    Some("filterable") => filterable,
                    Some("sortable") => sortable,
                    Some(other) => panic!("未知能力位 {other}"),
                };
                if ok {
                    ""
                } else {
                    "未声明"
                }
            }
        };
        assert_eq!(
            rejected,
            expected,
            "{} 上的 `{}`（{}）应被判定为「{expected}」",
            table,
            site.field,
            site.verb.name()
        );
    }
}
