//! 审批**实例**表单（`InstanceDetail.form`）的解析。
//!
//! # 为什么不能复用定义快照
//!
//! 审批定义的 `form` 是**控件结构**（`required` / `option[]` / `children`），
//! 而实例详情里的 `form` 是**实例值**：顶层控件直接带 `value`，`fieldList` 的
//! `value` 是「行数组」，每行又是单元格数组。两者字段集不同，拿定义快照的
//! 结构体去解实例值只会得到一堆空值——所以这里独立声明 [`FormNode`]。
//!
//! # 实例回读的三条真机口径
//!
//! 1. 单选/多选的 `value` 是**展示文案**，提交时的选项 id 不在这里；单选另有
//!    兄弟字段 `option: {key, text}`，条件与映射一律取 `option.text`（外部选项
//!    经 API 建单时 `text` 可能为空，此时退回 `value`）。
//! 2. `formula` / `number` / `amount` 的 `value` 是 JSON number。
//! 3. `attachmentV2` 的 `value` 是 **12 小时签名 URL 数组**，不是 file code。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::super::approval::InstanceDetail;
use super::ExportError;

/// 实例表单里的一个控件/单元格。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct FormNode {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default, rename = "type")]
    pub(crate) widget_type: String,
    #[serde(default)]
    pub(crate) value: Value,
    /// 选项控件的回读对象：`{key, text}`（单选）或数组（多选/外部选项）。
    #[serde(default)]
    pub(crate) option: Option<Value>,
    /// `contact` 控件的 open_id 可能与 `value` **同级**（`open_ids` 兄弟字段）。
    /// 不声明它时 serde 会静默丢弃，反向同步会把「有人员」误判成空。
    #[serde(default)]
    pub(crate) open_ids: Vec<String>,
    /// 扩展信息；各控件类型不同，v1 只原样保留。
    #[serde(default)]
    pub(crate) ext: Option<Value>,
}

/// 实例表单里附件/图片控件回读出的一个文件。
///
/// `url` 是 12 小时签名地址，不是可写回多维表的 `file_token`；反向同步必须
/// 重新下载并上传。`size` 在 `ext` 里不一定有，缺失时按「文件名」比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentRef {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) size: Option<i64>,
}

/// `fieldList` 的一行：单元格 id → 单元格。
#[derive(Debug, Clone, Default)]
pub(crate) struct FormRow {
    pub(crate) cells: BTreeMap<String, FormNode>,
}

impl FormRow {
    pub(crate) fn cell(&self, widget_id: &str) -> Option<&FormNode> {
        self.cells.get(widget_id)
    }
}

/// 解析后的实例表单。
///
/// 空表单（`form` 缺失/空串）是合法的「没有值」形态：所有查找返回 `None`，
/// 由条件/映射层记 [`super::Drift`]，而不是在这里直接报错——「没有表单」
/// 与「控件被删了」在处置上都是人工核对，不该让整条实例解析失败。
#[derive(Debug, Clone, Default)]
pub(crate) struct InstanceForm {
    top: BTreeMap<String, FormNode>,
    rows: BTreeMap<String, Vec<FormRow>>,
    /// 位于控件组内的 `fieldList`：整单共享、按 leader 语义处理，不按成员行序匹配。
    nested_detail_ids: std::collections::BTreeSet<String>,
    present: bool,
}

impl InstanceForm {
    /// 从实例详情里的 `form` 原文解析。
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, ExportError> {
        let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Ok(Self::default());
        };
        let nodes: Vec<FormNode> = serde_json::from_str(raw).map_err(|error| {
            ExportError::Fatal(format!("审批实例表单不是合法的 JSON 数组：{error}"))
        })?;
        let mut form = Self {
            present: true,
            ..Self::default()
        };
        for node in nodes {
            form.index_node(node, false);
        }
        Ok(form)
    }

    /// 把一个节点及其控件组子节点、明细行索引进表单。
    ///
    /// 控件组子控件的 id 在实例内唯一，平铺进 `top` 后调用方不必知道父组；
    /// 明细行仍按 `(detail_id, row_index, cell_id)` 访问，避免跨行串值。
    ///
    /// `inside_container` 标记当前节点是否在控件组内：控件组里的 `fieldList`
    /// 是整单共享值，聚合反向同步不能按成员行序要求它每成员一行。
    fn index_node(&mut self, node: FormNode, inside_container: bool) {
        let id = node.id.trim().to_string();
        if id.is_empty() {
            // 说明控件（`text`）没有 id、也不参与取值；缺 id 的节点直接忽略。
            return;
        }
        let rows = node.detail_rows();
        let children = node.child_nodes();
        // 空 `fieldList` 也要进索引：否则「明细行被全部删空」会退化成
        // 「表单里没有这个明细」，聚合对账会按成员数继续写旧值而不是转人工。
        if !rows.is_empty() || node.widget_type == "fieldList" {
            self.rows.entry(id.clone()).or_insert(rows);
            if inside_container {
                self.nested_detail_ids.insert(id.clone());
            }
        }
        let child_inside_container =
            inside_container || super::super::widget_rules::is_control_group(&node.widget_type);
        // 重复 id 保留第一次出现的节点；子节点仍继续索引，避免整棵组丢失。
        if let std::collections::btree_map::Entry::Vacant(slot) = self.top.entry(id) {
            slot.insert(node);
        }
        for child in children {
            self.index_node(child, child_inside_container);
        }
    }

    /// 从 [`InstanceDetail`] 取 `form` 并解析。
    pub(crate) fn from_detail(detail: &InstanceDetail) -> Result<Self, ExportError> {
        Self::parse(detail.form.as_deref())
    }

    /// `form` 是否存在且非空。
    pub(crate) fn is_present(&self) -> bool {
        self.present
    }

    /// 顶层控件。
    pub(crate) fn top(&self, widget_id: &str) -> Option<&FormNode> {
        self.top.get(widget_id)
    }

    /// 明细控件的行。`None` = 表单里没有这个明细控件；`Some(&[])` = 控件存在但零行。
    pub(crate) fn rows(&self, detail_widget_id: &str) -> Option<&[FormRow]> {
        self.rows.get(detail_widget_id).map(Vec::as_slice)
    }

    /// 明细控件某一行里的单元格。
    pub(crate) fn cell(
        &self,
        detail_widget_id: &str,
        row_index: usize,
        widget_id: &str,
    ) -> Option<&FormNode> {
        self.rows
            .get(detail_widget_id)
            .and_then(|rows| rows.get(row_index))
            .and_then(|row| row.cell(widget_id))
    }

    /// 明细行数；明细控件不存在时为 0。
    pub(crate) fn row_count(&self, detail_widget_id: &str) -> usize {
        self.rows.get(detail_widget_id).map_or(0, |rows| rows.len())
    }

    /// 已索引的明细控件 id（用于反向定位控件属于哪个明细）。
    pub(crate) fn detail_ids(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    /// 可作为「每成员一行」处理的明细：排除控件组内的整单共享明细。
    pub(crate) fn member_detail_ids(&self) -> Vec<String> {
        self.rows
            .keys()
            .filter(|id| !self.nested_detail_ids.contains(*id))
            .cloned()
            .collect()
    }

    /// 该明细是否位于控件组内（整单共享，聚合时按 leader/派生模式处理）。
    pub(crate) fn is_nested_detail(&self, detail_widget_id: &str) -> bool {
        self.nested_detail_ids.contains(detail_widget_id)
    }
}

fn parse_rows(value: &Value) -> Vec<FormRow> {
    let rows: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        // 极端形态：`value` 被序列化成 JSON 字符串。与 `parse_form` 一样两种都认。
        Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
        _ => Vec::new(),
    };
    rows.into_iter()
        .filter_map(|row| match row {
            Value::Array(cells) => Some(FormRow {
                cells: parse_cells(&cells),
            }),
            Value::Object(_) => Some(FormRow {
                cells: parse_cells(std::slice::from_ref(&row)),
            }),
            _ => None,
        })
        .collect()
}

fn parse_cells(cells: &[Value]) -> BTreeMap<String, FormNode> {
    cells
        .iter()
        .filter_map(|cell| serde_json::from_value::<FormNode>(cell.clone()).ok())
        .filter(|cell| !cell.id.trim().is_empty())
        .map(|cell| (cell.id.trim().to_string(), cell))
        .collect()
}

impl FormNode {
    /// 原始 `value`。
    pub(crate) fn raw(&self) -> &Value {
        &self.value
    }

    /// 是否选项类控件（取文案而不是原始 `value`）。
    pub(crate) fn is_option_widget(&self) -> bool {
        matches!(
            self.widget_type.as_str(),
            "radio" | "radioV2" | "checkbox" | "checkboxV2" | "select" | "multiSelect"
        )
    }

    fn is_checkbox_widget(&self) -> bool {
        matches!(
            self.widget_type.as_str(),
            "checkbox" | "checkboxV2" | "multiSelect"
        )
    }

    /// 选项回读里的展示文案（单选取第一个；多选按顺序全取）。
    fn option_texts(&self) -> Vec<String> {
        fn text_of_option(value: &Value) -> Option<String> {
            match value {
                Value::String(text) => non_empty(text),
                Value::Object(map) => ["text", "label", "name", "value"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_str))
                    .and_then(non_empty),
                _ => None,
            }
        }
        match &self.option {
            Some(Value::Array(items)) => items.iter().filter_map(text_of_option).collect(),
            Some(value) => text_of_option(value).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// 单选/多选的展示文案。
    ///
    /// 单选优先 `option.text`（真机回读的 `value` 也是文案，但外部选项经 API
    /// 建单时 `value` 与 `text` 都可能为空，此时只能退回 `value`）。
    pub(crate) fn label(&self) -> Option<String> {
        self.option_texts()
            .into_iter()
            .next()
            .or_else(|| non_empty(&text_of_value(&self.value)))
    }

    /// 多选/复选框的全部文案。
    pub(crate) fn labels(&self) -> Vec<String> {
        let from_value: Vec<String> = match &self.value {
            Value::Array(items) => items
                .iter()
                .filter_map(|item| non_empty(&text_of_value(item)))
                .collect(),
            value => non_empty(&text_of_value(value)).into_iter().collect(),
        };
        if self.is_checkbox_widget() && !from_value.is_empty() {
            return from_value;
        }
        let from_option = self.option_texts();
        if from_option.is_empty() {
            from_value
        } else {
            from_option
        }
    }

    /// 面向文本转换器的展示文本。
    pub(crate) fn display_text(&self) -> String {
        if self.is_option_widget() {
            let labels = self.labels();
            if !labels.is_empty() {
                return labels.join("、");
            }
        }
        if matches!(
            self.widget_type.as_str(),
            "attachmentV2" | "attachment" | "image" | "imageV2"
        ) {
            let urls = self.urls();
            if !urls.is_empty() {
                return urls.join("\n");
            }
        }
        if matches!(self.widget_type.as_str(), "contact" | "contacts" | "person") {
            let ids = self.person_ids();
            if !ids.is_empty() {
                return ids.join("、");
            }
        }
        text_of_value(&self.value)
    }

    /// 数字/金额/公式控件的数值。
    pub(crate) fn number(&self) -> Option<FixedNumber> {
        numeric_value(&self.value).or_else(|| self.option.as_ref().and_then(numeric_value))
    }

    /// 日期/时间控件的毫秒时间戳。字符串 `"0"` 按 0 返回，由调用方决定是否当空。
    pub(crate) fn millis(&self) -> Option<i64> {
        match &self.value {
            Value::Number(number) => number
                .as_i64()
                .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| {
                    number
                        .as_f64()
                        .filter(|value| value.fract() == 0.0)
                        .map(|value| {
                            // 毫秒时间戳远小于 2^53，f64 在这里不会丢精度。
                            value as i64
                        })
                }),
            Value::String(text) => text.trim().parse::<i64>().ok(),
            Value::Object(map) => map.get("value").and_then(FormNode::millis_value),
            _ => None,
        }
    }

    fn millis_value(value: &Value) -> Option<i64> {
        match value {
            Value::Number(number) => number
                .as_i64()
                .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
            Value::String(text) => text.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    /// 复选框控件的布尔值；「是/否」「1/0」也认。
    pub(crate) fn bool_value(&self) -> Option<bool> {
        match &self.value {
            Value::Bool(value) => Some(*value),
            Value::Number(number) => match number.as_i64() {
                Some(1) => Some(true),
                Some(0) => Some(false),
                _ => None,
            },
            Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "是" => Some(true),
                "false" | "0" | "no" | "否" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// 附件控件回读的签名 URL（也接受普通字符串/对象里的 url）。
    pub(crate) fn urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        collect_urls(&self.value, &mut urls);
        dedup_keep_order(&mut urls);
        urls
    }

    /// 人员控件里的 open_id（也接受 `open_ids` 数组与用户对象）。
    pub(crate) fn person_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        // `contact` 的 open_ids 在节点**顶层**，不在 `value` 里；先收它。
        ids.extend(self.open_ids.iter().cloned());
        collect_person_ids(&self.value, &mut ids);
        dedup_keep_order(&mut ids);
        ids
    }

    /// 只要 `ou_` 前缀的 open_id；user_id / 工号不能直接写多维表人员列。
    pub(crate) fn person_open_ids(&self) -> Vec<String> {
        self.person_ids()
            .into_iter()
            .filter(|id| id.starts_with("ou_"))
            .collect()
    }

    /// 电话控件的可读文本。审批回读可能是字符串，也可能是
    /// `{countryCode,nationalNumber}` / snake_case 形态。
    pub(crate) fn telephone_text(&self) -> Option<String> {
        fn as_text(value: &Value) -> Option<String> {
            match value {
                Value::String(text) => non_empty(text),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            }
        }
        fn from_value(value: &Value) -> Option<String> {
            match value {
                Value::String(text) => non_empty(text),
                Value::Number(number) => Some(number.to_string()),
                Value::Array(items) => items.iter().find_map(from_value),
                Value::Object(map) => {
                    let country = [
                        "countryCode",
                        "country_code",
                        "country",
                        "areaCode",
                        "area_code",
                    ]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(as_text));
                    let national = [
                        "nationalNumber",
                        "national_number",
                        "number",
                        "phone",
                        "value",
                    ]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(as_text));
                    match (country, national) {
                        (Some(country), Some(national)) => Some(format!("{country} {national}")),
                        (None, Some(national)) => Some(national),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        from_value(&self.value)
    }

    /// 日期控件的 RFC3339 文本 → 绝对毫秒。
    ///
    /// `default_offset_seconds` 是 Base 时区；回读文本自带偏移时以文本为准，
    /// 只有无偏移的 date-only 才用它。
    pub(crate) fn rfc3339_millis(&self, default_offset_seconds: i32) -> Option<i64> {
        let text = text_of_value(&self.value);
        parse_rfc3339_millis(&text, default_offset_seconds)
    }

    /// `dateInterval` 的 `{start,end}` → 两个绝对毫秒。
    pub(crate) fn date_interval_millis(&self, default_offset_seconds: i32) -> Option<(i64, i64)> {
        let value = match &self.value {
            Value::String(text) => serde_json::from_str::<Value>(text).ok()?,
            other => other.clone(),
        };
        let map = value.as_object()?;
        let start = map.get("start").and_then(value_as_text)?;
        let end = map.get("end").and_then(value_as_text)?;
        Some((
            parse_rfc3339_millis(&start, default_offset_seconds)?,
            parse_rfc3339_millis(&end, default_offset_seconds)?,
        ))
    }

    /// 附件/图片控件的文件列表。`ext` 里的文件名与 `value` 的 URL 按顺序配对；
    /// 数量对不上时用 URL 末段兜底，不把整个附件当成空。
    pub(crate) fn attachment_files(&self) -> Vec<AttachmentRef> {
        let urls = attachment_urls(&self.value);
        if urls.is_empty() {
            return Vec::new();
        }
        let names = attachment_names(self.ext.as_ref());
        let sizes = attachment_sizes(self.ext.as_ref());
        urls.into_iter()
            .enumerate()
            .map(|(index, url)| {
                let name = names
                    .get(index)
                    .or_else(|| names.first())
                    .cloned()
                    .unwrap_or_else(|| url_basename(&url));
                let size = sizes.get(index).copied().or_else(|| sizes.first().copied());
                AttachmentRef { name, url, size }
            })
            .collect()
    }

    /// 控件组（`leaveGroupV2` / `workGroup` / `outGroup`）的子控件。
    ///
    /// 官方把子控件放在 `value` 数组里（不是 `children`），每个子节点自身的
    /// 值仍在它的 `value`。
    pub(crate) fn child_nodes(&self) -> Vec<FormNode> {
        if !super::super::widget_rules::is_control_group(&self.widget_type) {
            return Vec::new();
        }
        let value = match &self.value {
            Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
            other => other.clone(),
        };
        match value {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| serde_json::from_value::<FormNode>(item).ok())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `fieldList` 的行；非明细控件返回空。
    pub(crate) fn detail_rows(&self) -> Vec<FormRow> {
        if self.widget_type != "fieldList" {
            return Vec::new();
        }
        parse_rows(&self.value)
    }

    /// 选项控件的 `(option_id, 文案)` 对。
    ///
    /// 外部选项回读的 `option.key` 是本系统 `option_id`，文案在 `option.text`；
    /// 只有文案时返回 `(None, Some(text))`，由调用方按目标列现有选项匹配。
    pub(crate) fn option_pairs(&self) -> Vec<(Option<String>, Option<String>)> {
        fn pair(value: &Value) -> (Option<String>, Option<String>) {
            match value {
                Value::String(text) => (None, non_empty(text)),
                Value::Object(map) => {
                    let id = ["key", "value", "id", "option_id"]
                        .iter()
                        .find_map(|key| map.get(*key).and_then(value_as_text));
                    let text = ["text", "label", "name"]
                        .iter()
                        .find_map(|key| map.get(*key).and_then(value_as_text));
                    (id, text)
                }
                _ => (None, None),
            }
        }
        match &self.option {
            Some(Value::Array(items)) => items.iter().map(pair).collect(),
            Some(value) => vec![pair(value)],
            None => Vec::new(),
        }
    }

    /// 在自身、控件组子控件、明细行单元格里递归查找控件。
    ///
    /// 返回克隆而不是引用：控件组子节点是从 `value` 现解析出来的，借用无法
    /// 穿过局部变量。实例表单很小，克隆成本可忽略。
    pub(crate) fn find_descendant(&self, widget_id: &str) -> Option<FormNode> {
        if self.id == widget_id {
            return Some(self.clone());
        }
        for child in self.child_nodes() {
            if let Some(found) = child.find_descendant(widget_id) {
                return Some(found);
            }
        }
        for row in self.detail_rows() {
            for cell in row.cells.values() {
                if let Some(found) = cell.find_descendant(widget_id) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// 是否有值。`false` 与数字 `0` 都算「有值」，与多维表格的「空」同口径。
    pub(crate) fn is_empty(&self) -> bool {
        // `contact` 的 open_ids 在顶层兄弟字段；只看 value 会把「有人员」判空。
        if !self.open_ids.is_empty() {
            return false;
        }
        if self.option_pairs().iter().any(|(id, text)| {
            id.as_deref().is_some_and(|value| !value.trim().is_empty())
                || text
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        }) {
            return false;
        }
        value_is_empty(&self.value)
    }
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 从控件值里尽量取一个数值。
///
/// 外部数据源控件的值可能是数组/对象（`{value,text}`、`{number:...}`），
/// 控件类型本身也不一定是 `number`；计算字段只关心「能不能解析成数」。
fn numeric_value(value: &Value) -> Option<FixedNumber> {
    match value {
        Value::Number(_) | Value::String(_) => FixedNumber::from_json(value),
        Value::Array(items) => items.iter().find_map(numeric_value),
        Value::Object(map) => ["value", "number", "amount", "text", "name", "label"]
            .iter()
            .find_map(|key| map.get(*key).and_then(numeric_value)),
        _ => None,
    }
}

pub(crate) fn text_of_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(text_of_value)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("、"),
        Value::Object(map) => ["name", "text", "label", "value", "id"]
            .iter()
            .find_map(|key| map.get(*key))
            .map(text_of_value)
            .unwrap_or_default(),
    }
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.iter().all(value_is_empty),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty(text),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 逗号/中文逗号/分号/换行分隔的文案列表；空串返回空列表。
fn split_text_list(text: &str) -> Vec<String> {
    text.split([',', '，', ';', '；', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// 附件/图片控件里的 URL。
///
/// 真机有数组形态，也有把多个 URL 塞进一个逗号串的形态；单个 URL 的查询串
/// 也可能含逗号，所以只有出现多个 `http` 时才按逗号拆。
fn attachment_urls(value: &Value) -> Vec<String> {
    fn collect(value: &Value, urls: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                let parts = if text.matches("http").count() > 1 && text.contains(',') {
                    split_text_list(text)
                } else {
                    vec![text.to_string()]
                };
                urls.extend(parts.into_iter().filter(|part| part.starts_with("http")));
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, urls)),
            Value::Object(map) => {
                if let Some(url) = ["url", "link", "href", "download_url", "tmp_url", "value"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(value_as_text))
                {
                    collect(&Value::String(url), urls);
                }
            }
            _ => {}
        }
    }
    let mut urls = Vec::new();
    collect(value, &mut urls);
    dedup_keep_order(&mut urls);
    urls
}

/// `ext` 里的文件名列表：字符串、逗号串、数组、`{name,text,title}` 对象都认。
fn attachment_names(ext: Option<&Value>) -> Vec<String> {
    fn collect(value: &Value, names: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                let parts = split_text_list(text);
                if parts.is_empty() {
                    if let Some(name) = non_empty(text) {
                        names.push(name);
                    }
                } else {
                    names.extend(parts);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, names)),
            Value::Object(map) => {
                if let Some(name) = ["name", "text", "title", "file_name"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(value_as_text))
                {
                    names.push(name);
                } else if let Some(inner) = map.get("value") {
                    collect(inner, names);
                }
            }
            _ => {}
        }
    }
    let mut names = Vec::new();
    if let Some(ext) = ext {
        collect(ext, &mut names);
    }
    names
}

/// `ext` 里的文件大小列表；没有就返回空，比较时退化为按文件名。
fn attachment_sizes(ext: Option<&Value>) -> Vec<i64> {
    fn collect(value: &Value, sizes: &mut Vec<i64>) {
        match value {
            Value::Number(number) => {
                if let Some(size) = number.as_i64() {
                    sizes.push(size);
                }
            }
            Value::String(text) => {
                for part in split_text_list(text) {
                    if let Ok(size) = part.parse::<i64>() {
                        sizes.push(size);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, sizes)),
            Value::Object(map) => {
                if let Some(size) = ["size", "file_size", "fileSize"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_i64))
                {
                    sizes.push(size);
                }
            }
            _ => {}
        }
    }
    let mut sizes = Vec::new();
    if let Some(ext) = ext {
        collect(ext, &mut sizes);
    }
    sizes
}

fn url_basename(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let name = without_query.rsplit('/').next().unwrap_or("");
    let name = name.replace("%20", " ");
    non_empty(&name).unwrap_or_else(|| "附件".to_string())
}

/// RFC3339 → 绝对毫秒。支持 `Z`、`+08:00`、`-05:30`、小数秒与 date-only。
///
/// 解析失败返回 `None`；调用方把 `0`/`"0"`/空串统一按「空」处理，不写 1970。
fn parse_rfc3339_millis(text: &str, default_offset_seconds: i32) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (date_part, time_part) = if let Some((date, time)) = text.split_once('T') {
        (date, Some(time))
    } else if let Some((date, time)) = text.split_once(' ') {
        (date, Some(time))
    } else {
        (text, None)
    };

    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    if date.next().is_some()
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }

    let (hour, minute, second, millis, offset) = match time_part {
        None => (0, 0, 0, 0, default_offset_seconds),
        Some(time) => parse_time(time, default_offset_seconds)?,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))?;
    let utc_seconds = seconds.checked_sub(i64::from(offset))?;
    utc_seconds
        .checked_mul(1_000)?
        .checked_add(i64::from(millis))
}

fn parse_time(time: &str, default_offset_seconds: i32) -> Option<(u32, u32, u32, u32, i32)> {
    let time = time.trim();
    let (body, offset) =
        if let Some(body) = time.strip_suffix('Z').or_else(|| time.strip_suffix('z')) {
            (body, 0)
        } else if let Some(index) = time.rfind(['+', '-']) {
            let (body, offset) = time.split_at(index);
            (body, parse_offset(offset)?)
        } else {
            // 没有偏移的日期时间按 Base 时区解释；RFC3339 本应带偏移，但真机
            // 可能给本地时间，不能默认当成 UTC（会整体偏 8 小时）。
            (time, default_offset_seconds)
        };

    let (clock, fraction) = match body.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (body, None),
    };
    let mut parts = clock.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(fraction) => {
            let digits: String = fraction.chars().take(3).collect();
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let padded = format!("{digits:0<3}");
            padded.parse::<u32>().ok()?
        }
    };
    Some((hour, minute, second, millis, offset))
}

/// `+08:00` / `-0530` → 秒。符号与小时必填，分钟可缺省。
fn parse_offset(text: &str) -> Option<i32> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if rest.len() == 4 => rest.split_at(2),
        None => (rest, "0"),
    };
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 18 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant 的 `days_from_civil`：公历年月日 → 自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn collect_urls(value: &Value, urls: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if let Some(url) = non_empty(text) {
                urls.push(url);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_urls(item, urls)),
        Value::Object(map) => {
            if let Some(url) = ["url", "link", "href"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .and_then(non_empty)
            {
                urls.push(url);
            } else if let Some(inner) = map.get("value") {
                collect_urls(inner, urls);
            }
        }
        _ => {}
    }
}

fn collect_person_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            for part in text.split([',', ';', ' ', '\n']) {
                if let Some(id) = non_empty(part) {
                    ids.push(id);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_person_ids(item, ids)),
        Value::Object(map) => {
            if let Some(inner) = map.get("open_ids").or_else(|| map.get("ids")) {
                collect_person_ids(inner, ids);
            }
            for key in ["open_id", "id", "user_id"] {
                if let Some(id) = map.get(key).and_then(Value::as_str).and_then(non_empty) {
                    ids.push(id);
                }
            }
            if let Some(inner) = map.get("value") {
                collect_person_ids(inner, ids);
            }
        }
        _ => {}
    }
}

fn dedup_keep_order(values: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

/// 定点十进制数：`units × 10^-scale`。
///
/// 条件比较不使用 `f64`：`0.1 + 0.2` 那类二进制误差会让「金额等于」在边界上
/// 随机为真/假。这里的解析只接受纯十进制（可选正负号、最多一个小数点），
/// 带单位/指数/千分位的文本一律拒绝——那些值不该被猜成数字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FixedNumber {
    units: i128,
    scale: u32,
}

impl FixedNumber {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let (integer, fraction) = rest
            .split_once('.')
            .map_or((rest, ""), |(integer, fraction)| (integer, fraction));
        if (integer.is_empty() && fraction.is_empty())
            || !integer.chars().all(|c| c.is_ascii_digit())
            || !fraction.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        let digits = format!("{integer}{fraction}");
        let mut units = digits.parse::<i128>().ok()?;
        if negative {
            units = -units;
        }
        let mut scale = fraction.len() as u32;
        while scale > 0 && units % 10 == 0 {
            units /= 10;
            scale -= 1;
        }
        Some(Self { units, scale })
    }

    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => Self::parse(&number.to_string()),
            Value::String(text) => Self::parse(text),
            _ => None,
        }
    }

    /// 数值比较；对齐小数位时溢出（只可能来自荒诞大的输入）返回 `None`，
    /// 调用方按「不匹配」处理——比用 `f64` 猜一个结果更安全。
    pub(crate) fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.scale == other.scale {
            return Some(self.units.cmp(&other.units));
        }
        let scale = self.scale.max(other.scale);
        let factor = |from: u32| 10i128.checked_pow(scale - from);
        let left = self.units.checked_mul(factor(self.scale)?)?;
        let right = other.units.checked_mul(factor(other.scale)?)?;
        Some(left.cmp(&right))
    }

    /// 整数形态（scale 为 0）转 `i64`；日期列与整数数字列用。
    pub(crate) fn as_integer(self) -> Option<i64> {
        if self.scale != 0 {
            return None;
        }
        i64::try_from(self.units).ok()
    }

    /// 由计数构造整数，用于「明细字段平均值」这类聚合的分母。
    pub(crate) fn from_count(count: usize) -> Option<Self> {
        Some(Self {
            units: i128::try_from(count).ok()?,
            scale: 0,
        })
    }

    /// 单次二元加减乘除。除法按目标精度四舍五入（远离零），除零返回 `None`。
    pub(crate) fn calculate(self, operator: &str, other: Self, precision: u32) -> Option<Self> {
        let result = match operator {
            "add" | "subtract" => {
                let scale = self.scale.max(other.scale);
                let left = self.rescaled_units(scale)?;
                let right = other.rescaled_units(scale)?;
                let units = if operator == "add" {
                    left.checked_add(right)?
                } else {
                    left.checked_sub(right)?
                };
                Self { units, scale }
            }
            "multiply" => Self {
                units: self.units.checked_mul(other.units)?,
                scale: self.scale.checked_add(other.scale)?,
            },
            "divide" => {
                if other.units == 0 {
                    return None;
                }
                // value = (units / 10^scale) / (other.units / 10^other.scale)
                // 目标 units = value * 10^precision
                let exponent = precision as i32 + other.scale as i32 - self.scale as i32;
                let (numerator, denominator) = if exponent >= 0 {
                    (
                        self.units.checked_mul(pow10(exponent as u32)?)?,
                        other.units,
                    )
                } else {
                    (
                        self.units,
                        other.units.checked_mul(pow10((-exponent) as u32)?)?,
                    )
                };
                Self {
                    units: divide_round(numerator, denominator)?,
                    scale: precision,
                }
            }
            _ => return None,
        };
        result.round_to(precision)
    }

    /// 四舍五入到指定小数位（远离零），并去掉末尾多余的 0。
    pub(crate) fn round_to(self, precision: u32) -> Option<Self> {
        let units = self.rescaled_units(precision)?;
        let mut result = Self {
            units,
            scale: precision,
        };
        while result.scale > 0 && result.units % 10 == 0 {
            result.units /= 10;
            result.scale -= 1;
        }
        Some(result)
    }

    /// 固定小数位的展示文本（补足末尾 0，不丢精度）。
    pub(crate) fn format_fixed(self, precision: u32) -> Option<String> {
        let units = self.rescaled_units(precision)?;
        let negative = units < 0;
        let digits = units.unsigned_abs().to_string();
        let scale = precision as usize;
        let body = if scale == 0 {
            digits
        } else if digits.len() <= scale {
            format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
        } else {
            let split = digits.len() - scale;
            format!("{}.{}", &digits[..split], &digits[split..])
        };
        Some(if negative { format!("-{body}") } else { body })
    }

    fn rescaled_units(self, target_scale: u32) -> Option<i128> {
        if target_scale >= self.scale {
            self.units.checked_mul(pow10(target_scale - self.scale)?)
        } else {
            divide_round(self.units, pow10(self.scale - target_scale)?)
        }
    }

    /// 编码回 JSON number（保留十进制字面量，不经过 `f64`）。
    pub(crate) fn to_json_value(self) -> Value {
        let text = self.canonical_text();
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    fn canonical_text(self) -> String {
        let negative = self.units < 0;
        let digits = self.units.unsigned_abs().to_string();
        let text = if self.scale == 0 {
            digits
        } else {
            let scale = self.scale as usize;
            if digits.len() <= scale {
                format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
            } else {
                let split = digits.len() - scale;
                format!("{}.{}", &digits[..split], &digits[split..])
            }
        };
        if negative {
            format!("-{text}")
        } else {
            text
        }
    }
}

fn pow10(exponent: u32) -> Option<i128> {
    10i128.checked_pow(exponent)
}

/// 整数除法四舍五入（远离零）；除零返回 `None`。
fn divide_round(numerator: i128, denominator: i128) -> Option<i128> {
    if denominator == 0 {
        return None;
    }
    let negative = (numerator < 0) ^ (denominator < 0);
    let numerator = numerator.unsigned_abs();
    let denominator = denominator.unsigned_abs();
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = quotient + u128::from(remainder * 2 >= denominator);
    let rounded = i128::try_from(rounded).ok()?;
    Some(if negative { -rounded } else { rounded })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(raw: &str) -> InstanceForm {
        InstanceForm::parse(Some(raw)).unwrap_or_else(|error| panic!("应可解析: {error}"))
    }

    #[test]
    fn parses_top_values_and_detail_rows() {
        let form = form(
            r#"[
                {"id":"title","name":"标题","type":"input","value":"付款申请"},
                {"id":"kind","name":"类型","type":"radioV2","value":"差旅",
                 "option":{"key":"o1","text":"差旅"}},
                {"id":"tags","name":"标签","type":"checkboxV2","value":["A","B"]},
                {"id":"files","name":"附件","type":"attachmentV2",
                 "value":["https://a/1","https://a/2"]},
                {"id":"detail","name":"明细","type":"fieldList","value":[
                    [{"id":"amount","type":"amount","value":12.50},
                     {"id":"note","type":"input","value":"机票"}],
                    [{"id":"amount","type":"amount","value":7}]
                ]}
            ]"#,
        );

        assert!(form.is_present());
        assert_eq!(
            form.top("title").map(FormNode::display_text).as_deref(),
            Some("付款申请")
        );
        assert_eq!(
            form.top("kind").and_then(FormNode::label).as_deref(),
            Some("差旅"),
            "单选取 option.text，不是提交值"
        );
        assert_eq!(
            form.top("tags").map(FormNode::labels).unwrap_or_default(),
            vec!["A".to_string(), "B".to_string()]
        );
        assert_eq!(
            form.top("files").map(FormNode::urls).unwrap_or_default(),
            vec!["https://a/1".to_string(), "https://a/2".to_string()]
        );

        assert_eq!(form.row_count("detail"), 2);
        let amount = form
            .cell("detail", 0, "amount")
            .and_then(FormNode::number)
            .unwrap_or_else(|| panic!("金额应可解析"));
        assert_eq!(
            amount,
            FixedNumber::parse("12.5").unwrap_or_else(|| panic!(""))
        );
        assert_eq!(
            form.cell("detail", 1, "note"),
            None,
            "行内缺控件必须能区分出来"
        );
    }

    #[test]
    fn control_group_detail_is_shared_not_per_member() {
        let form = form(
            r#"[
                {"id":"group","type":"workGroup","value":[
                    {"id":"inner","type":"fieldList","value":[
                        [{"id":"x","type":"input","value":"a"}]
                    ]}
                ]},
                {"id":"outer","type":"fieldList","value":[
                    [{"id":"y","type":"input","value":"b"}]
                ]}
            ]"#,
        );
        assert!(form.is_nested_detail("inner"));
        assert!(!form.is_nested_detail("outer"));
        assert_eq!(form.member_detail_ids(), vec!["outer".to_string()]);
        assert_eq!(
            form.rows("inner").map(<[FormRow]>::len),
            Some(1),
            "共享明细仍可按 id 读取"
        );
    }

    #[test]
    fn numeric_value_accepts_external_datasource_shapes() {
        let form = form(
            r#"[
                {"id":"text","type":"text","value":{"value":"7.2"}},
                {"id":"array","type":"text","value":[{"text":"1.35"}]},
                {"id":"nested","type":"text","value":{"name":"汇率","value":{"amount":"6.9"}}},
                {"id":"option","type":"select","value":"fld.x:d1","option":{"key":"fld.x:d1","text":"7.25"}}
            ]"#,
        );
        assert_eq!(
            form.top("text").and_then(FormNode::number),
            FixedNumber::parse("7.2")
        );
        assert_eq!(
            form.top("array").and_then(FormNode::number),
            FixedNumber::parse("1.35")
        );
        assert_eq!(
            form.top("nested").and_then(FormNode::number),
            FixedNumber::parse("6.9")
        );
        assert_eq!(
            form.top("option").and_then(FormNode::number),
            FixedNumber::parse("7.25"),
            "外部选项的数值可能在 option.text 里"
        );
    }

    #[test]
    fn missing_form_is_empty_not_an_error() {
        let form = InstanceForm::parse(None).unwrap_or_else(|error| panic!("{error}"));
        assert!(!form.is_present());
        assert!(form.top("anything").is_none());
        assert!(form.rows("detail").is_none());
        assert_eq!(form.row_count("detail"), 0);
    }

    #[test]
    fn invalid_form_json_is_fatal() {
        let error = match InstanceForm::parse(Some("{not json")) {
            Ok(_) => panic!("坏 JSON 必须报错"),
            Err(error) => error,
        };
        assert!(matches!(error, ExportError::Fatal(_)), "{error:?}");
    }

    #[test]
    fn option_label_falls_back_to_value_when_text_is_blank() {
        // 外部选项经 API 建单时 text 为空（真机），只能退回 value。
        let node: FormNode = serde_json::from_value(serde_json::json!({
            "id":"place","type":"radioV2","value":"成都",
            "option":{"key":"fld.x:d1","text":""}
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(node.label().as_deref(), Some("成都"));
    }

    #[test]
    fn empty_detection_matches_bitable_semantics() {
        let null: FormNode = serde_json::from_value(serde_json::json!({"id":"a","value":null}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(null.is_empty());
        let blank: FormNode = serde_json::from_value(serde_json::json!({"id":"a","value":"  "}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(blank.is_empty());
        let zero: FormNode = serde_json::from_value(serde_json::json!({"id":"a","value":0}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!zero.is_empty(), "数字 0 是有值");
        let falsey: FormNode = serde_json::from_value(serde_json::json!({"id":"a","value":false}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!falsey.is_empty(), "布尔 false 是有值");
    }

    #[test]
    fn fixed_numbers_compare_without_binary_float_error() {
        let one = FixedNumber::parse("100.10").unwrap_or_else(|| panic!(""));
        let other = FixedNumber::parse("100.1").unwrap_or_else(|| panic!(""));
        assert_eq!(one.compare(&other), Some(std::cmp::Ordering::Equal));
        assert_eq!(
            FixedNumber::parse("0.3")
                .and_then(|a| FixedNumber::parse("0.1").and_then(|b| a.compare(&b))),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            FixedNumber::parse("9.1330")
                .and_then(|a| FixedNumber::parse("9.133").and_then(|b| a.compare(&b))),
            Some(std::cmp::Ordering::Equal)
        );
        assert!(FixedNumber::parse("1e3").is_none());
        assert!(FixedNumber::parse("7.10 元").is_none());
        assert!(FixedNumber::parse("").is_none());
        assert_eq!(
            FixedNumber::parse("12.50")
                .map(FixedNumber::to_json_value)
                .unwrap_or(Value::Null),
            serde_json::json!(12.5)
        );
    }

    #[test]
    fn fixed_numbers_calculate_with_requested_precision() {
        let left = FixedNumber::parse("100.10").unwrap_or_else(|| panic!(""));
        let right = FixedNumber::parse("87.86").unwrap_or_else(|| panic!(""));
        assert_eq!(
            left.calculate("multiply", right, 2)
                .and_then(|value| value.format_fixed(2)),
            Some("8794.79".to_string())
        );
        assert_eq!(
            FixedNumber::parse("10")
                .and_then(|value| value.calculate("divide", right, 4))
                .and_then(|value| value.format_fixed(4)),
            Some("0.1138".to_string())
        );
        assert!(left
            .calculate(
                "divide",
                FixedNumber::parse("0").unwrap_or_else(|| panic!("")),
                2
            )
            .is_none());
    }

    // ---- 反向同步需要的解析能力 ----

    #[test]
    fn rfc3339_parses_offsets_fractions_and_date_only() {
        let shanghai = 8 * 3_600;
        for (text, expected) in [
            ("2020-01-01T00:00:00Z", 1_577_836_800_000),
            ("2020-01-01T08:00:00+08:00", 1_577_836_800_000),
            ("2020-01-01T00:00:00.5Z", 1_577_836_800_500),
            ("2024-02-29T12:34:56Z", 1_709_210_096_000),
            ("2020-01-01T00:00:00-05:30", 1_577_856_600_000),
            ("2020-01-01T00:00:00", 1_577_808_000_000),
            ("2020-01-01", 1_577_808_000_000),
        ] {
            assert_eq!(
                parse_rfc3339_millis(text, shanghai),
                Some(expected),
                "{text}"
            );
        }
        for invalid in [
            "2024-02-30",
            "2020-13-01",
            "not-a-date",
            "2020-01-01T25:00:00Z",
            "2020-01-01T00:00:00+19:00",
            "0",
            "",
        ] {
            assert!(
                parse_rfc3339_millis(invalid, shanghai).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn contact_node_keeps_top_level_open_ids() {
        let form = form(
            r#"[
                {"id":"person","type":"contact","value":[],
                 "open_ids":["ou_a","ou_b"]},
                {"id":"nested","type":"contact",
                 "value":{"open_ids":["ou_c"],"value":[]}},
                {"id":"user_only","type":"contact",
                 "value":{"value":["u_123"]}}
            ]"#,
        );
        assert_eq!(
            form.top("person")
                .map(FormNode::person_open_ids)
                .unwrap_or_default(),
            vec!["ou_a".to_string(), "ou_b".to_string()]
        );
        assert_eq!(
            form.top("nested")
                .map(FormNode::person_open_ids)
                .unwrap_or_default(),
            vec!["ou_c".to_string()]
        );
        assert!(
            form.top("user_only")
                .map(FormNode::person_open_ids)
                .unwrap_or_default()
                .is_empty(),
            "user_id 不能冒充 open_id"
        );
    }

    #[test]
    fn attachments_parse_arrays_comma_strings_objects_and_names() {
        let form = form(
            r#"[
                {"id":"files","type":"attachmentV2",
                 "value":["https://x/a.pdf","https://x/b.png"],
                 "ext":["a.pdf","b.png"]},
                {"id":"imgs","type":"imageV2",
                 "value":"https://x/a.png,https://x/b.png",
                 "ext":"a.png,b.png"},
                {"id":"objs","type":"attachmentV2",
                 "value":[{"url":"https://x/c.pdf"}],
                 "ext":[{"name":"c.pdf","size":12}]},
                {"id":"query","type":"attachmentV2",
                 "value":"https://x/a.pdf?v=1,2"},
                {"id":"none","type":"attachmentV2","value":[]}
            ]"#,
        );
        let files = form
            .top("files")
            .map(FormNode::attachment_files)
            .unwrap_or_default();
        assert_eq!(
            files
                .iter()
                .map(|file| (file.name.as_str(), file.size))
                .collect::<Vec<_>>(),
            vec![("a.pdf", None), ("b.png", None)]
        );
        let imgs = form
            .top("imgs")
            .map(FormNode::attachment_files)
            .unwrap_or_default();
        assert_eq!(imgs.len(), 2, "逗号串要拆成两个 URL");
        assert_eq!(imgs[0].name, "a.png");
        assert_eq!(imgs[1].name, "b.png");
        let objs = form
            .top("objs")
            .map(FormNode::attachment_files)
            .unwrap_or_default();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].name, "c.pdf");
        assert_eq!(objs[0].size, Some(12));
        let query = form
            .top("query")
            .map(FormNode::attachment_files)
            .unwrap_or_default();
        assert_eq!(query.len(), 1, "单个 URL 的逗号不能被当成多文件");
        assert_eq!(query[0].url, "https://x/a.pdf?v=1,2");
        assert!(form
            .top("none")
            .map(FormNode::attachment_files)
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn control_groups_and_nested_details_are_indexed() {
        let form = form(
            r#"[
                {"id":"leave","type":"leaveGroupV2","value":[
                    {"id":"widgetLeaveGroupType","type":"radioV2","value":"annual",
                     "option":{"text":"年假"}},
                    {"id":"widgetLeaveGroupStartTime","type":"date",
                     "value":"2026-09-28T00:00:00+08:00"}
                ]},
                {"id":"detail","type":"fieldList","value":[[
                    {"id":"nested","type":"fieldList","value":[[
                        {"id":"amount","type":"amount","value":1}
                    ]]},
                    {"id":"group","type":"outGroup","value":[
                        {"id":"widgetOutGroupReason","type":"textarea","value":"外出"}
                    ]}
                ]]}
            ]"#,
        );
        assert_eq!(
            form.top("leave")
                .map(|node| node.child_nodes().len())
                .unwrap_or_default(),
            2
        );
        assert!(form.top("widgetLeaveGroupType").is_some());
        assert_eq!(
            form.top("widgetLeaveGroupType")
                .and_then(|node| node.label())
                .as_deref(),
            Some("年假")
        );
        let nested = form
            .cell("detail", 0, "nested")
            .unwrap_or_else(|| panic!("嵌套明细应可访问"));
        assert_eq!(nested.detail_rows().len(), 1);
        assert_eq!(
            nested
                .detail_rows()
                .first()
                .and_then(|row| row.cell("amount"))
                .and_then(FormNode::number),
            FixedNumber::parse("1")
        );
        assert_eq!(
            form.cell("detail", 0, "group")
                .and_then(|node| node.find_descendant("widgetOutGroupReason"))
                .map(|node| node.display_text())
                .as_deref(),
            Some("外出")
        );
    }

    #[test]
    fn missing_and_empty_are_distinct_from_zero_and_false() {
        let form = form(
            r#"[
                {"id":"null","type":"input","value":null},
                {"id":"blank","type":"input","value":"  "},
                {"id":"empty_array","type":"checkboxV2","value":[]},
                {"id":"zero","type":"number","value":0},
                {"id":"falsey","type":"checkboxV2","value":false}
            ]"#,
        );
        assert!(form.top("missing").is_none(), "缺失必须可区分");
        for id in ["null", "blank", "empty_array"] {
            assert!(form.top(id).is_some_and(FormNode::is_empty), "{id} 应判空");
        }
        for id in ["zero", "falsey"] {
            assert!(
                form.top(id).is_some_and(|node| !node.is_empty()),
                "{id} 是有值"
            );
        }
        assert!(form.top("missing").is_none());
    }
}
