//! 批量审批列表的只读计算字段：按每个用户自己的规则做运算，在服务端算好随列表下发。
//!
//! 操作数既可以是普通表单控件，也可以是明细控件里某一列的行级聚合
//! （求和/最大/最小/极差/平均）。

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::addon::feishu::domain::approval_export::form::{FixedNumber, InstanceForm};

/// 操作数：顶层控件值，或明细控件某列的行级聚合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Operand {
    Field {
        field_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field_name: Option<String>,
    },
    Detail {
        detail_field_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail_field_name: Option<String>,
        column_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column_name: Option<String>,
        aggregate: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ComputedRule {
    pub(crate) id: String,
    pub(crate) name: String,
    /// 新版操作数；旧配置只有 `left_field_id`/`right_field_id`，解析后会被归一化到这里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) left: Option<Operand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) left_field_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) left_field_name: Option<String>,
    pub(crate) operator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) right: Option<Operand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) right_field_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) right_field_name: Option<String>,
    pub(crate) precision: u32,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
}

impl ComputedRule {
    pub(crate) fn left_operand(&self) -> Option<Operand> {
        self.left.clone().or_else(|| {
            self.left_field_id.as_ref().map(|field_id| Operand::Field {
                field_id: field_id.clone(),
                field_name: self.left_field_name.clone(),
            })
        })
    }

    pub(crate) fn right_operand(&self) -> Option<Operand> {
        self.right.clone().or_else(|| {
            self.right_field_id.as_ref().map(|field_id| Operand::Field {
                field_id: field_id.clone(),
                field_name: self.right_field_name.clone(),
            })
        })
    }

    /// 把旧版 `left_field_id`/`right_field_id` 配置迁移成 `left`/`right`。
    pub(crate) fn normalized(mut self) -> Self {
        self.left = self.left.take().or_else(|| {
            self.left_field_id.take().map(|field_id| Operand::Field {
                field_id,
                field_name: self.left_field_name.take(),
            })
        });
        if self.left.is_some() {
            self.left_field_id = None;
            self.left_field_name = None;
        }
        self.right = self.right.take().or_else(|| {
            self.right_field_id.take().map(|field_id| Operand::Field {
                field_id,
                field_name: self.right_field_name.take(),
            })
        });
        if self.right.is_some() {
            self.right_field_id = None;
            self.right_field_name = None;
        }
        self
    }

    pub(crate) fn is_unary(&self) -> bool {
        self.operator == "none"
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Operator {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "add" => Some(Self::Add),
            "subtract" => Some(Self::Subtract),
            "multiply" => Some(Self::Multiply),
            "divide" => Some(Self::Divide),
            _ => None,
        }
    }
}

/// 明细列的行级聚合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregate {
    Sum,
    Max,
    Min,
    Range,
    Average,
}

impl Aggregate {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "sum" => Some(Self::Sum),
            "max" => Some(Self::Max),
            "min" => Some(Self::Min),
            "range" => Some(Self::Range),
            "average" => Some(Self::Average),
            _ => None,
        }
    }
}

/// 计算一条任务的全部配置字段。
///
/// 实例详情拿不到、控件缺值或除零时仍返回该列（`value`/`display` 为 null），
/// 让前端列集合保持稳定，不会因某一行缺值而整列消失。
pub(crate) fn computed_fields(detail: &Value, rules: &[ComputedRule]) -> Vec<Value> {
    if rules.is_empty() {
        return Vec::new();
    }
    let form = instance_form(detail);
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .map(|rule| {
            let value = form.as_ref().and_then(|form| evaluate(form, rule));
            serde_json::json!({
                "key": format!("computed:{}", rule.id),
                "name": rule.name,
                "value": value.map(|number| number.to_json_value()).unwrap_or(Value::Null),
                "display": value
                    .and_then(|number| number.format_fixed(rule.precision))
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                "precision": rule.precision,
            })
        })
        .collect()
}

fn instance_form(detail: &Value) -> Option<InstanceForm> {
    let raw = match detail.get("form")? {
        Value::String(raw) => raw.clone(),
        Value::Array(_) => serde_json::to_string(detail.get("form")?).ok()?,
        _ => return None,
    };
    InstanceForm::parse(Some(&raw)).ok()
}

fn evaluate(form: &InstanceForm, rule: &ComputedRule) -> Option<FixedNumber> {
    let left = operand_value(form, &rule.left_operand()?)?;
    let Some(right) = rule.right_operand() else {
        // 单操作数：直接取左值（例如「明细金额总和」单独成列）。
        return Some(left);
    };
    Operator::parse(&rule.operator)?;
    let right = operand_value(form, &right)?;
    left.calculate(&rule.operator, right, rule.precision)
}

fn operand_value(form: &InstanceForm, operand: &Operand) -> Option<FixedNumber> {
    match operand {
        Operand::Field { field_id, .. } => form.top(field_id)?.number(),
        Operand::Detail {
            detail_field_id,
            column_id,
            aggregate,
            ..
        } => {
            let rows = form.rows(detail_field_id)?;
            let values: Vec<FixedNumber> = rows
                .iter()
                .filter_map(|row| row.cell(column_id)?.number())
                .collect();
            aggregate_values(&values, Aggregate::parse(aggregate)?)
        }
    }
}

fn aggregate_values(values: &[FixedNumber], aggregate: Aggregate) -> Option<FixedNumber> {
    let first = *values.first()?;
    match aggregate {
        Aggregate::Sum => values[1..]
            .iter()
            .cloned()
            .try_fold(first, |acc, value| acc.calculate("add", value, 6)),
        Aggregate::Max => values
            .iter()
            .cloned()
            .reduce(|best, value| match value.compare(&best) {
                Some(Ordering::Greater) => value,
                _ => best,
            }),
        Aggregate::Min => values
            .iter()
            .cloned()
            .reduce(|best, value| match value.compare(&best) {
                Some(Ordering::Less) => value,
                _ => best,
            }),
        Aggregate::Range => {
            let max = values
                .iter()
                .cloned()
                .reduce(|best, value| match value.compare(&best) {
                    Some(Ordering::Greater) => value,
                    _ => best,
                })?;
            let min = values
                .iter()
                .cloned()
                .reduce(|best, value| match value.compare(&best) {
                    Some(Ordering::Less) => value,
                    _ => best,
                })?;
            max.calculate("subtract", min, 6)
        }
        Aggregate::Average => {
            let sum = values[1..]
                .iter()
                .cloned()
                .try_fold(first, |acc, value| acc.calculate("add", value, 6))?;
            sum.calculate("divide", FixedNumber::from_count(values.len())?, 6)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{computed_fields, Aggregate, ComputedRule, Operand};
    use serde_json::json;

    fn field_rule(id: i64, operator: &str, precision: u32) -> ComputedRule {
        ComputedRule {
            id: format!("c{id}"),
            name: format!("字段{id}"),
            left: Some(Operand::Field {
                field_id: "amount".to_string(),
                field_name: None,
            }),
            left_field_id: None,
            left_field_name: None,
            operator: operator.to_string(),
            right: Some(Operand::Field {
                field_id: "rate".to_string(),
                field_name: None,
            }),
            right_field_id: None,
            right_field_name: None,
            precision,
            enabled: true,
        }
    }

    fn detail_rule(aggregate: &str, precision: u32) -> ComputedRule {
        ComputedRule {
            id: "sum1".to_string(),
            name: "明细金额合计".to_string(),
            left: Some(Operand::Detail {
                detail_field_id: "items".to_string(),
                detail_field_name: Some("明细".to_string()),
                column_id: "price".to_string(),
                column_name: Some("金额".to_string()),
                aggregate: aggregate.to_string(),
            }),
            left_field_id: None,
            left_field_name: None,
            operator: "none".to_string(),
            right: None,
            right_field_id: None,
            right_field_name: None,
            precision,
            enabled: true,
        }
    }

    fn detail() -> serde_json::Value {
        json!({
            "form": r#"[{"id":"amount","type":"amount","value":1234.5},{"id":"rate","type":"number","value":7.1234},{"id":"items","type":"fieldList","value":[[{"id":"price","type":"amount","value":10}],[{"id":"price","type":"amount","value":25.5}],[{"id":"price","type":"amount","value":4.5}]]}]"#
        })
    }

    #[test]
    fn computes_multiply_with_fixed_precision() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":1234.5},{"id":"rate","type":"number","value":7.1234}]"#
        });
        let fields = computed_fields(&detail, &[field_rule(1, "multiply", 2)]);
        assert_eq!(fields[0]["key"], "computed:c1");
        assert_eq!(fields[0]["value"], json!(8793.84));
        assert_eq!(fields[0]["display"], "8793.84");
    }

    #[test]
    fn missing_value_still_returns_a_column() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":10}]"#
        });
        let fields = computed_fields(&detail, &[field_rule(1, "multiply", 2)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["value"], json!(null));
        assert_eq!(fields[0]["display"], json!(null));
    }

    #[test]
    fn divide_by_zero_is_null() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":10},{"id":"rate","type":"number","value":0}]"#
        });
        let fields = computed_fields(&detail, &[field_rule(1, "divide", 4)]);
        assert_eq!(fields[0]["value"], json!(null));
    }

    #[test]
    fn external_datasource_rate_is_parsed_from_option_text() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":100},{"id":"rate","type":"select","value":"fld.x:d1","option":{"key":"fld.x:d1","text":"7.2"}}]"#
        });
        let fields = computed_fields(&detail, &[field_rule(1, "multiply", 2)]);
        assert_eq!(fields[0]["value"], json!(720));
        assert_eq!(fields[0]["display"], "720.00");
    }

    #[test]
    fn detail_aggregates_cover_sum_max_min_range_and_average() {
        let detail = detail();
        let cases = [
            ("sum", json!(40)),
            ("max", json!(25.5)),
            ("min", json!(4.5)),
            ("range", json!(21)),
            ("average", json!(13.333333)),
        ];
        for (aggregate, expected) in cases {
            let fields = computed_fields(&detail, &[detail_rule(aggregate, 6)]);
            assert_eq!(fields[0]["value"], expected, "aggregate={aggregate}");
        }
    }

    #[test]
    fn empty_detail_rows_leave_the_column_null() {
        let detail = json!({
            "form": r#"[{"id":"items","type":"fieldList","value":[]}]"#
        });
        let fields = computed_fields(&detail, &[detail_rule("sum", 2)]);
        assert_eq!(fields[0]["value"], json!(null));
    }

    #[test]
    fn unary_rule_uses_only_the_left_operand() {
        let detail = detail();
        let fields = computed_fields(&detail, &[detail_rule("sum", 2)]);
        assert_eq!(fields[0]["display"], "40.00");
    }

    #[test]
    fn aggregate_parser_rejects_unknown_names() {
        assert_eq!(Aggregate::parse("sum"), Some(Aggregate::Sum));
        assert_eq!(Aggregate::parse("median"), None);
    }
}
