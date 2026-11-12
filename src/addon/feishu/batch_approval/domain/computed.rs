//! 批量审批列表的只读计算字段：按审批定义配置二元运算，在服务端算好随列表下发。

use serde_json::Value;
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_export::form::InstanceForm;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComputedRule {
    pub(crate) id: i64,
    pub(crate) approval_code: String,
    pub(crate) name: String,
    pub(crate) left_field_id: String,
    pub(crate) operator: String,
    pub(crate) right_field_id: String,
    pub(crate) precision: u32,
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

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Divide => "divide",
        }
    }
}

impl ComputedRule {
    pub(crate) fn from_record(row: &Record) -> Result<Self, BaseError> {
        let precision = row.require::<i64>("precision")?;
        let precision = u32::try_from(precision)
            .ok()
            .filter(|value| *value <= 6)
            .ok_or_else(|| BaseError::ConfigError("计算字段精度必须在 0 到 6 之间".to_string()))?;
        Ok(Self {
            id: row.require("id")?,
            approval_code: row.require("approval_code")?,
            name: row.require("name")?,
            left_field_id: row.require("left_field_id")?,
            operator: row.require("operator")?,
            right_field_id: row.require("right_field_id")?,
            precision,
        })
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
        .map(|rule| {
            let value = form.as_ref().and_then(|form| evaluate(form, rule));
            serde_json::json!({
                "key": format!("computed_{}", rule.id),
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

fn evaluate(
    form: &InstanceForm,
    rule: &ComputedRule,
) -> Option<crate::addon::feishu::domain::approval_export::form::FixedNumber> {
    let left = form.top(&rule.left_field_id)?.number()?;
    let right = form.top(&rule.right_field_id)?.number()?;
    Operator::parse(&rule.operator)?;
    left.calculate(&rule.operator, right, rule.precision)
}

#[cfg(test)]
mod tests {
    use super::{computed_fields, ComputedRule};
    use serde_json::json;

    fn rule(id: i64, operator: &str, precision: u32) -> ComputedRule {
        ComputedRule {
            id,
            approval_code: "CODE".to_string(),
            name: format!("字段{id}"),
            left_field_id: "amount".to_string(),
            operator: operator.to_string(),
            right_field_id: "rate".to_string(),
            precision,
        }
    }

    #[test]
    fn computes_multiply_with_fixed_precision() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":1234.5},{"id":"rate","type":"number","value":7.1234}]"#
        });
        let fields = computed_fields(&detail, &[rule(1, "multiply", 2)]);
        assert_eq!(fields[0]["key"], "computed_1");
        assert_eq!(fields[0]["value"], json!(8793.84));
        assert_eq!(fields[0]["display"], "8793.84");
    }

    #[test]
    fn missing_value_still_returns_a_column() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":10}]"#
        });
        let fields = computed_fields(&detail, &[rule(1, "multiply", 2)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["value"], json!(null));
        assert_eq!(fields[0]["display"], json!(null));
    }

    #[test]
    fn divide_by_zero_is_null() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":10},{"id":"rate","type":"number","value":0}]"#
        });
        let fields = computed_fields(&detail, &[rule(1, "divide", 4)]);
        assert_eq!(fields[0]["value"], json!(null));
    }

    #[test]
    fn external_datasource_rate_is_parsed_from_option_text() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":100},{"id":"rate","type":"select","value":"fld.x:d1","option":{"key":"fld.x:d1","text":"7.2"}}]"#
        });
        let fields = computed_fields(&detail, &[rule(1, "multiply", 2)]);
        assert_eq!(fields[0]["value"], json!(720));
        assert_eq!(fields[0]["display"], "720.00");
    }
}
