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
    /// 扩展信息；各控件类型不同，v1 只原样保留。
    #[serde(default)]
    pub(crate) ext: Option<Value>,
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
            let id = node.id.trim().to_string();
            if id.is_empty() {
                // 说明控件（`text`）没有 id、也不参与取值；缺 id 的节点直接忽略。
                continue;
            }
            if node.widget_type == "fieldList" {
                form.rows.insert(id.clone(), parse_rows(&node.value));
            }
            form.top.insert(id, node);
        }
        Ok(form)
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
        match &self.value {
            Value::Number(number) => FixedNumber::parse(&number.to_string()),
            Value::String(text) => FixedNumber::parse(text),
            Value::Object(map) => map.get("value").and_then(FixedNumber::from_json),
            _ => None,
        }
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
        collect_person_ids(&self.value, &mut ids);
        dedup_keep_order(&mut ids);
        ids
    }

    /// 是否有值。`false` 与数字 `0` 都算「有值」，与多维表格的「空」同口径。
    pub(crate) fn is_empty(&self) -> bool {
        value_is_empty(&self.value)
    }
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn text_of_value(value: &Value) -> String {
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
}
