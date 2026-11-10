//! 回写规则的条件 DSL：解析与求值。
//!
//! # 形态
//!
//! ```json
//! {"match":"all"|"any","items":[
//!   {"scope":"instance","widget_id":"...","op":"eq|ne|in|gt|gte|lt|lte|contains|is_empty|between","value":...},
//!   {"scope":"detail","detail_widget_id":"...","match":"all|any|none","items":[
//!     {"widget_id":"...","op":"...","value":...}
//!   ]}
//! ]}
//! ```
//!
//! - 顶层 `match` 组合全部条目；`scope=detail` 的条目在实例级求值时的语义是
//!   「存在至少一行满足该行条件」，每一行则按自己的 `match` 判定；
//! - 数字/金额/公式一律走 [`FixedNumber`] 定点比较，**不用 `f64`**；
//! - 单选/多选的文案用 [`labels_equivalent`]（`9.1330` 与 `9.133` 等价）；
//! - 引用的控件在实例表单里不存在时结果为 `false`，并记 [`Drift`]；
//!   `is_empty` 只在**控件存在且值为空**时为真。

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::Value;

use super::form::{FixedNumber, FormNode, FormRow, InstanceForm};
use super::{Drift, ExportError};

use super::super::approval_convert::labels_equivalent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchMode {
    All,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailMatchMode {
    All,
    Any,
    None,
}

impl DetailMatchMode {
    fn combine(self, results: &[bool]) -> bool {
        match self {
            Self::All => results.iter().all(|matched| *matched),
            Self::Any => results.iter().any(|matched| *matched),
            Self::None => !results.iter().any(|matched| *matched),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompareOp {
    Eq,
    Ne,
    In,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    IsEmpty,
    Between,
}

impl CompareOp {
    fn parse(raw: &str) -> Result<Self, ExportError> {
        match raw.trim() {
            "eq" => Ok(Self::Eq),
            "ne" => Ok(Self::Ne),
            "in" => Ok(Self::In),
            "gt" => Ok(Self::Gt),
            "gte" => Ok(Self::Gte),
            "lt" => Ok(Self::Lt),
            "lte" => Ok(Self::Lte),
            "contains" => Ok(Self::Contains),
            "is_empty" => Ok(Self::IsEmpty),
            "between" => Ok(Self::Between),
            other => Err(ExportError::Fatal(format!(
                "未知的条件操作符「{other}」：支持 eq / ne / in / gt / gte / lt / lte / \
                 contains / is_empty / between"
            ))),
        }
    }
}

/// 一个叶子条件（顶层控件或明细行内控件）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LeafCondition {
    pub(crate) widget_id: String,
    pub(crate) op: CompareOp,
    pub(crate) value: Value,
}

/// 明细行级条件：每个明细行分别求值。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetailCondition {
    pub(crate) detail_widget_id: String,
    pub(crate) match_mode: DetailMatchMode,
    pub(crate) items: Vec<LeafCondition>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConditionItem {
    Instance(LeafCondition),
    Detail(DetailCondition),
}

/// 一条规则的全部条件。空条件 = 恒真。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Condition {
    pub(crate) match_mode: MatchMode,
    pub(crate) items: Vec<ConditionItem>,
}

impl Default for Condition {
    fn default() -> Self {
        Self {
            match_mode: MatchMode::All,
            items: Vec::new(),
        }
    }
}

/// 条件求值结果：是否命中 + 这轮求值发现的定义漂移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConditionReport {
    pub(crate) matched: bool,
    pub(crate) drifts: Vec<Drift>,
}

impl Condition {
    /// 解析 `condition_json` 列。空白 = 无条件（恒真）。
    pub(crate) fn parse(raw: &str) -> Result<Self, ExportError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self::default());
        }
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| ExportError::Fatal(format!("条件 JSON 解析失败：{error}")))?;
        Self::from_value(&value)
    }

    pub(crate) fn from_value(value: &Value) -> Result<Self, ExportError> {
        let object = value
            .as_object()
            .ok_or_else(|| ExportError::Fatal("条件必须是 JSON 对象".to_string()))?;
        let match_mode = parse_match_mode(object.get("match"), MatchMode::All)?;
        let raw_items = match object.get("items") {
            Some(Value::Array(items)) => items.clone(),
            Some(_) => return Err(ExportError::Fatal("条件的 `items` 必须是数组".to_string())),
            None => Vec::new(),
        };
        let mut items = Vec::with_capacity(raw_items.len());
        for raw_item in &raw_items {
            let item = raw_item
                .as_object()
                .ok_or_else(|| ExportError::Fatal("条件条目必须是 JSON 对象".to_string()))?;
            let scope = item
                .get("scope")
                .and_then(Value::as_str)
                .ok_or_else(|| ExportError::Fatal("条件条目缺少 `scope`".to_string()))?;
            items.push(match scope {
                "instance" => ConditionItem::Instance(parse_leaf(item)?),
                "detail" => {
                    let detail_widget_id = required_str(item, "detail_widget_id")?;
                    let match_mode = parse_detail_match_mode(item.get("match"))?;
                    let raw_leaves =
                        item.get("items").and_then(Value::as_array).ok_or_else(|| {
                            ExportError::Fatal("明细条件缺少 `items` 数组".to_string())
                        })?;
                    let mut leaves = Vec::with_capacity(raw_leaves.len());
                    for raw_leaf in raw_leaves {
                        let leaf = raw_leaf.as_object().ok_or_else(|| {
                            ExportError::Fatal("明细条件的条目必须是 JSON 对象".to_string())
                        })?;
                        leaves.push(parse_leaf(leaf)?);
                    }
                    ConditionItem::Detail(DetailCondition {
                        detail_widget_id,
                        match_mode,
                        items: leaves,
                    })
                }
                other => {
                    return Err(ExportError::Fatal(format!(
                        "未知的条件范围「{other}」：只支持 instance / detail"
                    )))
                }
            });
        }
        Ok(Self { match_mode, items })
    }

    /// 只求值实例级条目，忽略行级条件。
    ///
    /// `row_filter=all_rows` 的语义是「实例条件命中后写全部明细行」；此时
    /// 行级条件不参与门槛，否则用户配了行条件却选「全部行」会被它悄悄挡住。
    /// 没有任何实例级条目时视为命中（没有门槛）。
    pub(crate) fn evaluate_instance(&self, form: &InstanceForm) -> ConditionReport {
        let mut drifts = BTreeSet::new();
        let results: Vec<bool> = self
            .items
            .iter()
            .filter_map(|item| match item {
                ConditionItem::Instance(leaf) => Some(evaluate_top_leaf(leaf, form, &mut drifts)),
                ConditionItem::Detail(_) => None,
            })
            .collect();
        if results.is_empty() {
            return ConditionReport {
                matched: true,
                drifts: drifts.into_iter().collect(),
            };
        }
        ConditionReport {
            matched: combine(self.match_mode, &results),
            drifts: drifts.into_iter().collect(),
        }
    }

    /// 实例级求值：`scope=detail` 的条目在「任一行满足」时为真。
    pub(crate) fn evaluate(&self, form: &InstanceForm) -> ConditionReport {
        let mut drifts = BTreeSet::new();
        let results: Vec<bool> = self
            .items
            .iter()
            .map(|item| match item {
                ConditionItem::Instance(leaf) => evaluate_top_leaf(leaf, form, &mut drifts),
                ConditionItem::Detail(detail) => evaluate_detail_any_row(detail, form, &mut drifts),
            })
            .collect();
        ConditionReport {
            matched: combine(self.match_mode, &results),
            drifts: drifts.into_iter().collect(),
        }
    }

    /// 行级求值：`scope=instance` 的条目仍按实例值，`scope=detail` 按指定行。
    ///
    /// `matched_rows` 行筛选与行级条件共用这一条路径，保证「实例条件命中」
    /// 与「这一行要不要写」不会各行其是。
    pub(crate) fn evaluate_row(&self, form: &InstanceForm, row_index: usize) -> ConditionReport {
        let mut drifts = BTreeSet::new();
        let results: Vec<bool> = self
            .items
            .iter()
            .map(|item| match item {
                ConditionItem::Instance(leaf) => evaluate_top_leaf(leaf, form, &mut drifts),
                ConditionItem::Detail(detail) => {
                    evaluate_detail_on_row(detail, form, row_index, &mut drifts)
                }
            })
            .collect();
        ConditionReport {
            matched: combine(self.match_mode, &results),
            drifts: drifts.into_iter().collect(),
        }
    }
}

fn parse_match_mode(value: Option<&Value>, default: MatchMode) -> Result<MatchMode, ExportError> {
    match value.and_then(Value::as_str) {
        None => Ok(default),
        Some("all") => Ok(MatchMode::All),
        Some("any") => Ok(MatchMode::Any),
        Some(other) => Err(ExportError::Fatal(format!(
            "未知的 `match`「{other}」：只支持 all / any"
        ))),
    }
}

fn parse_detail_match_mode(value: Option<&Value>) -> Result<DetailMatchMode, ExportError> {
    match value.and_then(Value::as_str) {
        None | Some("all") => Ok(DetailMatchMode::All),
        Some("any") => Ok(DetailMatchMode::Any),
        Some("none") => Ok(DetailMatchMode::None),
        Some(other) => Err(ExportError::Fatal(format!(
            "未知的明细 `match`「{other}」：只支持 all / any / none"
        ))),
    }
}

fn required_str(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, ExportError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ExportError::Fatal(format!("条件条目缺少 `{key}`")))
}

fn parse_leaf(object: &serde_json::Map<String, Value>) -> Result<LeafCondition, ExportError> {
    let widget_id = required_str(object, "widget_id")?;
    let op_raw = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| ExportError::Fatal("条件条目缺少 `op`".to_string()))?;
    let op = CompareOp::parse(op_raw)?;
    let value = object.get("value").cloned().unwrap_or(Value::Null);
    validate_operand(op, &value)?;
    Ok(LeafCondition {
        widget_id,
        op,
        value,
    })
}

fn validate_operand(op: CompareOp, value: &Value) -> Result<(), ExportError> {
    match op {
        CompareOp::IsEmpty => Ok(()),
        CompareOp::In => match value {
            Value::Array(_) => Ok(()),
            _ => Err(ExportError::Fatal("`in` 的 value 必须是数组".to_string())),
        },
        CompareOp::Between => match value {
            Value::Array(items)
                if items.len() == 2
                    && FixedNumber::from_json(&items[0]).is_some()
                    && FixedNumber::from_json(&items[1]).is_some() =>
            {
                Ok(())
            }
            _ => Err(ExportError::Fatal(
                "`between` 的 value 必须是两个数字组成的数组".to_string(),
            )),
        },
        CompareOp::Gt | CompareOp::Gte | CompareOp::Lt | CompareOp::Lte => {
            if FixedNumber::from_json(value).is_some() {
                Ok(())
            } else {
                Err(ExportError::Fatal(format!("`{op:?}` 的 value 必须是数字")))
            }
        }
        CompareOp::Eq | CompareOp::Ne | CompareOp::Contains => {
            if value.is_null() {
                Err(ExportError::Fatal(
                    "条件缺少 `value`（判空请用 is_empty）".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn combine(mode: MatchMode, results: &[bool]) -> bool {
    match mode {
        MatchMode::All => results.iter().all(|matched| *matched),
        MatchMode::Any => results.iter().any(|matched| *matched),
    }
}

fn evaluate_top_leaf(
    leaf: &LeafCondition,
    form: &InstanceForm,
    drifts: &mut BTreeSet<Drift>,
) -> bool {
    match form.top(&leaf.widget_id) {
        Some(node) => compare(leaf, node),
        None => {
            drifts.insert(Drift::MissingTopWidget {
                widget_id: leaf.widget_id.clone(),
            });
            false
        }
    }
}

fn evaluate_detail_any_row(
    detail: &DetailCondition,
    form: &InstanceForm,
    drifts: &mut BTreeSet<Drift>,
) -> bool {
    let Some(rows) = form.rows(&detail.detail_widget_id) else {
        drifts.insert(Drift::MissingDetailWidget {
            detail_widget_id: detail.detail_widget_id.clone(),
        });
        return false;
    };
    rows.iter()
        .any(|row| evaluate_detail_items(detail, row, drifts))
}

fn evaluate_detail_on_row(
    detail: &DetailCondition,
    form: &InstanceForm,
    row_index: usize,
    drifts: &mut BTreeSet<Drift>,
) -> bool {
    let Some(row) = form
        .rows(&detail.detail_widget_id)
        .and_then(|rows| rows.get(row_index))
    else {
        if form.rows(&detail.detail_widget_id).is_none() {
            drifts.insert(Drift::MissingDetailWidget {
                detail_widget_id: detail.detail_widget_id.clone(),
            });
        }
        return false;
    };
    evaluate_detail_items(detail, row, drifts)
}

fn evaluate_detail_items(
    detail: &DetailCondition,
    row: &FormRow,
    drifts: &mut BTreeSet<Drift>,
) -> bool {
    let results: Vec<bool> = detail
        .items
        .iter()
        .map(|leaf| match row.cell(&leaf.widget_id) {
            Some(cell) => compare(leaf, cell),
            None => {
                drifts.insert(Drift::MissingDetailCell {
                    detail_widget_id: detail.detail_widget_id.clone(),
                    widget_id: leaf.widget_id.clone(),
                });
                false
            }
        })
        .collect();
    detail.match_mode.combine(&results)
}

fn compare(leaf: &LeafCondition, node: &FormNode) -> bool {
    if leaf.op == CompareOp::IsEmpty {
        return node.is_empty();
    }
    match leaf.op {
        CompareOp::Eq => equals(node, &leaf.value),
        CompareOp::Ne => !equals(node, &leaf.value),
        CompareOp::In => leaf
            .value
            .as_array()
            .is_some_and(|items| items.iter().any(|item| equals(node, item))),
        CompareOp::Contains => contains(node, &leaf.value),
        CompareOp::Gt => {
            numeric_compare(node, &leaf.value).is_some_and(|ordering| ordering == Ordering::Greater)
        }
        CompareOp::Gte => {
            numeric_compare(node, &leaf.value).is_some_and(|ordering| ordering != Ordering::Less)
        }
        CompareOp::Lt => {
            numeric_compare(node, &leaf.value).is_some_and(|ordering| ordering == Ordering::Less)
        }
        CompareOp::Lte => {
            numeric_compare(node, &leaf.value).is_some_and(|ordering| ordering != Ordering::Greater)
        }
        CompareOp::Between => {
            let Some(items) = leaf.value.as_array().filter(|items| items.len() == 2) else {
                return false;
            };
            let lower_ok =
                numeric_compare(node, &items[0]).is_some_and(|ordering| ordering != Ordering::Less);
            let upper_ok = numeric_compare(node, &items[1])
                .is_some_and(|ordering| ordering != Ordering::Greater);
            lower_ok && upper_ok
        }
        CompareOp::IsEmpty => unreachable!("上面已处理 is_empty"),
    }
}

fn equals(node: &FormNode, expected: &Value) -> bool {
    let numeric_widget = matches!(
        node.widget_type.as_str(),
        "number" | "amount" | "formula" | "date"
    );
    if numeric_widget || expected.is_number() {
        if let (Some(left), Some(right)) = (node.number(), FixedNumber::from_json(expected)) {
            return left.compare(&right) == Some(Ordering::Equal);
        }
        if numeric_widget {
            return false;
        }
    }
    if let Value::Bool(expected) = expected {
        return node.bool_value() == Some(*expected);
    }
    let expected_text = expected_text(expected);
    if node.is_option_widget() {
        return node
            .labels()
            .iter()
            .any(|label| labels_equivalent(label, &expected_text));
    }
    if node.widget_type == "checkbox" || node.widget_type == "checkboxV2" {
        return node.bool_value() == Some(expected_text == "true");
    }
    node.display_text() == expected_text
}

fn contains(node: &FormNode, expected: &Value) -> bool {
    let expected_text = expected_text(expected);
    if node.is_option_widget() {
        return node
            .labels()
            .iter()
            .any(|label| labels_equivalent(label, &expected_text));
    }
    node.display_text().contains(&expected_text)
}

fn numeric_compare(node: &FormNode, expected: &Value) -> Option<Ordering> {
    let left = node.number()?;
    let right = FixedNumber::from_json(expected)?;
    left.compare(&right)
}

fn expected_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().to_string(),
        Value::Array(items) => items
            .iter()
            .map(expected_text)
            .collect::<Vec<_>>()
            .join("、"),
        Value::Object(map) => ["name", "text", "label", "value"]
            .iter()
            .find_map(|key| map.get(*key))
            .map(expected_text)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> InstanceForm {
        InstanceForm::parse(Some(
            r#"[
                {"id":"title","type":"input","value":"付款"},
                {"id":"amount","type":"number","value":100.10},
                {"id":"kind","type":"radioV2","value":"差旅","option":{"key":"o1","text":"差旅"}},
                {"id":"empty","type":"input","value":""},
                {"id":"detail","type":"fieldList","value":[
                    [{"id":"money","type":"amount","value":80},
                     {"id":"note","type":"input","value":"机票"}],
                    [{"id":"money","type":"amount","value":20},
                     {"id":"note","type":"input","value":"酒店"}]
                ]}
            ]"#,
        ))
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn condition(json: &str) -> Condition {
        Condition::parse(json).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn evaluate_instance_ignores_detail_conditions() {
        let form = form();
        let gate = condition(
            r#"{"match":"all","items":[
                {"scope":"instance","widget_id":"kind","op":"eq","value":"差旅"},
                {"scope":"detail","detail_widget_id":"detail","match":"all","items":[
                    {"widget_id":"money","op":"gt","value":1000}
                ]}
            ]}"#,
        );
        assert!(
            !gate.evaluate(&form).matched,
            "明细行没有满足 >1000 的行，实例级求值应被行条件挡住"
        );
        assert!(
            gate.evaluate_instance(&form).matched,
            "all_rows 只看实例条件，不应被行条件挡住"
        );
    }

    #[test]
    fn evaluate_instance_without_instance_items_is_always_true() {
        let gate = condition(
            r#"{"match":"any","items":[
                {"scope":"detail","detail_widget_id":"detail","match":"all","items":[
                    {"widget_id":"money","op":"gt","value":1000}
                ]}
            ]}"#,
        );
        assert!(
            gate.evaluate_instance(&form()).matched,
            "没有实例级条目 = 没有门槛"
        );
    }

    #[test]
    fn numeric_comparison_is_fixed_point_not_float() {
        let form = form();
        for json in [
            r#"{"items":[{"scope":"instance","widget_id":"amount","op":"eq","value":100.1}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"amount","op":"gte","value":"100.10"}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"amount","op":"lt","value":100.11}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"amount","op":"between","value":[100.1,100.1]}]}"#,
        ] {
            assert!(condition(json).evaluate(&form).matched, "{json}");
        }
        assert!(
            !condition(r#"{"items":[{"scope":"instance","widget_id":"amount","op":"eq","value":100.1001}]}"#)
                .evaluate(&form)
                .matched
        );
    }

    #[test]
    fn option_labels_use_the_display_text() {
        let form = form();
        let matched = condition(
            r#"{"items":[{"scope":"instance","widget_id":"kind","op":"eq","value":"差旅"}]}"#,
        )
        .evaluate(&form);
        assert!(matched.matched);
        assert!(matched.drifts.is_empty());
    }

    #[test]
    fn missing_widget_is_false_and_drifts() {
        let form = form();
        let report = condition(
            r#"{"items":[{"scope":"instance","widget_id":"removed","op":"eq","value":"x"}]}"#,
        )
        .evaluate(&form);
        assert!(!report.matched);
        assert_eq!(
            report.drifts,
            vec![Drift::MissingTopWidget {
                widget_id: "removed".to_string()
            }]
        );
    }

    #[test]
    fn is_empty_requires_the_widget_to_exist() {
        let form = form();
        assert!(
            condition(r#"{"items":[{"scope":"instance","widget_id":"empty","op":"is_empty"}]}"#)
                .evaluate(&form)
                .matched
        );
        let missing =
            condition(r#"{"items":[{"scope":"instance","widget_id":"removed","op":"is_empty"}]}"#)
                .evaluate(&form);
        assert!(!missing.matched, "不存在的控件不能算空");
        assert_eq!(missing.drifts.len(), 1);
    }

    #[test]
    fn detail_rows_support_all_any_and_none() {
        let form = form();
        let any = condition(
            r#"{"items":[{"scope":"detail","detail_widget_id":"detail","match":"any",
                 "items":[{"widget_id":"money","op":"gt","value":50}]}]}"#,
        );
        assert!(any.evaluate(&form).matched, "任一行命中");
        assert!(any.evaluate_row(&form, 0).matched);
        assert!(!any.evaluate_row(&form, 1).matched);

        let none = condition(
            r#"{"items":[{"scope":"detail","detail_widget_id":"detail","match":"none",
                 "items":[{"widget_id":"money","op":"gt","value":50}]}]}"#,
        );
        assert!(!none.evaluate_row(&form, 0).matched);
        assert!(none.evaluate_row(&form, 1).matched);

        let all = condition(
            r#"{"items":[{"scope":"detail","detail_widget_id":"detail","match":"all",
                 "items":[{"widget_id":"money","op":"gte","value":20},
                          {"widget_id":"note","op":"contains","value":"酒店"}]}]}"#,
        );
        assert!(all.evaluate_row(&form, 1).matched);
        assert!(!all.evaluate_row(&form, 0).matched);
        assert!(all.evaluate(&form).matched, "实例级=存在一行满足");
    }

    #[test]
    fn top_level_match_any_combines_instance_and_detail_items() {
        let form = form();
        let report = condition(
            r#"{"match":"any","items":[
                 {"scope":"instance","widget_id":"title","op":"eq","value":"不匹配"},
                 {"scope":"detail","detail_widget_id":"detail","match":"all",
                  "items":[{"widget_id":"money","op":"eq","value":20}]}
               ]}"#,
        )
        .evaluate(&form);
        assert!(report.matched);
    }

    #[test]
    fn missing_detail_widget_drifts_and_is_false() {
        let form = form();
        let report = condition(
            r#"{"items":[{"scope":"detail","detail_widget_id":"gone","match":"all","items":[]}]}"#,
        )
        .evaluate(&form);
        assert!(!report.matched);
        assert_eq!(
            report.drifts,
            vec![Drift::MissingDetailWidget {
                detail_widget_id: "gone".to_string()
            }]
        );
    }

    #[test]
    fn empty_condition_is_always_true() {
        let form = form();
        assert!(
            Condition::parse("  ")
                .unwrap_or_default()
                .evaluate(&form)
                .matched
        );
        assert!(condition(r#"{"items":[]}"#).evaluate(&form).matched);
    }

    #[test]
    fn malformed_conditions_are_fatal() {
        for json in [
            r#"{"items":"no"}"#,
            r#"{"items":[{"scope":"unknown"}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"a","op":"wat","value":1}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"a","op":"in","value":1}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"a","op":"between","value":[1]}]}"#,
            r#"{"items":[{"scope":"instance","widget_id":"a","op":"eq"}]}"#,
            r#"{"match":"all","items":[]} trailing"#,
        ] {
            assert!(
                matches!(Condition::parse(json), Err(ExportError::Fatal(_))),
                "应拒绝：{json}"
            );
        }
    }
}
