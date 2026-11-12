//! 批量审批列表的只读计算字段：按每个用户自己的规则做二元运算，在服务端算好随列表下发。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::addon::feishu::domain::approval_export::form::InstanceForm;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ComputedRule {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) left_field_id: String,
    #[serde(default)]
    pub(crate) left_field_name: Option<String>,
    pub(crate) operator: String,
    pub(crate) right_field_id: String,
    #[serde(default)]
    pub(crate) right_field_name: Option<String>,
    pub(crate) precision: u32,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
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
            id: format!("c{id}"),
            name: format!("字段{id}"),
            left_field_id: "amount".to_string(),
            left_field_name: None,
            operator: operator.to_string(),
            right_field_id: "rate".to_string(),
            right_field_name: None,
            precision,
            enabled: true,
        }
    }

    #[test]
    fn computes_multiply_with_fixed_precision() {
        let detail = json!({
            "form": r#"[{"id":"amount","type":"amount","value":1234.5},{"id":"rate","type":"number","value":7.1234}]"#
        });
        let fields = computed_fields(&detail, &[rule(1, "multiply", 2)]);
        assert_eq!(fields[0]["key"], "computed:c1");
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
