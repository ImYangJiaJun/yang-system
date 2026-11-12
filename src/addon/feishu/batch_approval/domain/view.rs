//! 批量审批的个人视图：当前用户对某个审批定义的计算字段与列布局。
//!
//! 视图按 `(user_id, approval_code)` 隔离，服务端不提供任何跨用户共享配置。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;

use super::computed::{ComputedRule, Operator};
use crate::addon::feishu::domain::batch_user_view_table::{
    VIEW_APPROVAL_CODE, VIEW_COLUMNS_JSON, VIEW_COMPUTED_FIELDS_JSON, VIEW_USER_ID,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::repository::all_pages;

const MAX_RULES: usize = 20;
const MAX_COLUMNS: usize = 200;
const MAX_NAME: usize = 64;
const MAX_ID: usize = 128;
/// 允许用户排序/隐藏的基础列；标题、当前节点、实例状态是固定列，不在此列。
const BASE_COLUMNS: [&str; 6] = [
    "definition",
    "instanceCode",
    "initiator",
    "summary",
    "createdAt",
    "apiOperate",
];

/// 一列的显示配置。`key` 形如 `form:<widget_id>` 或 `computed:<rule_id>`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ColumnConfig {
    pub(crate) key: String,
    pub(crate) visible: bool,
}

/// 一个用户在一个审批定义下的完整视图。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BatchUserView {
    pub(crate) computed_fields: Vec<ComputedRule>,
    pub(crate) columns: Vec<ColumnConfig>,
}

impl BatchUserView {
    /// 从请求体或数据库中的两个 JSON 值解析并校验。
    pub(crate) fn parse(computed_fields: &Value, columns: &Value) -> Result<Self, BaseError> {
        let computed_fields = parse_rules(computed_fields)?;
        let columns = parse_columns(columns)?;
        let view = Self {
            computed_fields,
            columns,
        };
        view.validate_computed_references()?;
        Ok(view)
    }

    pub(crate) fn from_stored(
        computed_fields_json: &str,
        columns_json: &str,
    ) -> Result<Self, BaseError> {
        let computed: Value = serde_json::from_str(computed_fields_json)
            .map_err(|error| invalid("computed_fields", format!("JSON 无法解析：{error}")))?;
        let columns: Value = serde_json::from_str(columns_json)
            .map_err(|error| invalid("columns", format!("JSON 无法解析：{error}")))?;
        Self::parse(&computed, &columns)
    }

    pub(crate) fn computed_fields_json(&self) -> Result<String, BaseError> {
        serde_json::to_string(&self.computed_fields)
            .map_err(|error| BaseError::Unknown(format!("序列化计算字段失败：{error}")))
    }

    pub(crate) fn columns_json(&self) -> Result<String, BaseError> {
        serde_json::to_string(&self.columns)
            .map_err(|error| BaseError::Unknown(format!("序列化显示字段失败：{error}")))
    }

    pub(crate) fn to_json(&self) -> Value {
        serde_json::json!({
            "computed_fields": self.computed_fields,
            "columns": self.columns,
        })
    }

    /// 保存前检查引用的控件确实属于该审批定义，并拒绝计算列引用不存在的规则。
    pub(crate) fn validate_widgets(&self, widget_ids: &HashSet<String>) -> Result<(), BaseError> {
        for rule in &self.computed_fields {
            for (field, id) in [
                ("left_field_id", &rule.left_field_id),
                ("right_field_id", &rule.right_field_id),
            ] {
                if !widget_ids.contains(id) {
                    return Err(invalid(
                        "computed_fields",
                        format!("{field} 指向的控件 {id} 不属于该审批定义"),
                    ));
                }
            }
        }
        for column in &self.columns {
            if let Some(id) = column.key.strip_prefix("form:") {
                if !widget_ids.contains(id) {
                    return Err(invalid("columns", format!("列 {id} 不属于该审批定义")));
                }
            }
        }
        Ok(())
    }

    fn validate_computed_references(&self) -> Result<(), BaseError> {
        let rule_ids: HashSet<&str> = self
            .computed_fields
            .iter()
            .map(|rule| rule.id.as_str())
            .collect();
        for column in &self.columns {
            if let Some(id) = column.key.strip_prefix("computed:") {
                if !rule_ids.contains(id) {
                    return Err(invalid(
                        "columns",
                        format!("计算列 {id} 引用了不存在的规则"),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn parse_rules(value: &Value) -> Result<Vec<ComputedRule>, BaseError> {
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("computed_fields", "必须是数组"))?;
    if rows.len() > MAX_RULES {
        return Err(invalid(
            "computed_fields",
            format!("最多 {MAX_RULES} 个计算字段"),
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        let rule: ComputedRule = serde_json::from_value(row.clone())
            .map_err(|error| invalid("computed_fields", format!("规则格式错误：{error}")))?;
        validate_rule(&rule)?;
        if !ids.insert(rule.id.clone()) {
            return Err(invalid("computed_fields", "规则 id 不能重复"));
        }
        if !names.insert(rule.name.clone()) {
            return Err(invalid("computed_fields", "规则名称不能重复"));
        }
        rules.push(rule);
    }
    Ok(rules)
}

fn parse_columns(value: &Value) -> Result<Vec<ColumnConfig>, BaseError> {
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("columns", "必须是数组"))?;
    if rows.len() > MAX_COLUMNS {
        return Err(invalid("columns", format!("最多 {MAX_COLUMNS} 列")));
    }
    let mut keys = HashSet::new();
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let column: ColumnConfig = serde_json::from_value(row.clone())
            .map_err(|error| invalid("columns", format!("列格式错误：{error}")))?;
        validate_key(&column.key)?;
        if !keys.insert(column.key.clone()) {
            return Err(invalid("columns", "列 key 不能重复"));
        }
        columns.push(column);
    }
    Ok(columns)
}

fn validate_rule(rule: &ComputedRule) -> Result<(), BaseError> {
    validate_text("computed_fields.id", &rule.id, MAX_ID)?;
    validate_text("computed_fields.name", &rule.name, MAX_NAME)?;
    validate_text("computed_fields.left_field_id", &rule.left_field_id, MAX_ID)?;
    validate_text(
        "computed_fields.right_field_id",
        &rule.right_field_id,
        MAX_ID,
    )?;
    if Operator::parse(&rule.operator).is_none() {
        return Err(invalid(
            "computed_fields.operator",
            "只支持 add/subtract/multiply/divide",
        ));
    }
    if rule.precision > 6 {
        return Err(invalid(
            "computed_fields.precision",
            "小数位必须在 0 到 6 之间",
        ));
    }
    for (field, name) in [
        ("left_field_name", rule.left_field_name.as_deref()),
        ("right_field_name", rule.right_field_name.as_deref()),
    ] {
        if let Some(name) = name {
            validate_text(field, name, MAX_ID)?;
        }
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), BaseError> {
    if let Some(id) = key.strip_prefix("form:") {
        return validate_text("columns.key", id, MAX_ID);
    }
    if let Some(id) = key.strip_prefix("computed:") {
        return validate_text("columns.key", id, MAX_ID);
    }
    if let Some(name) = key.strip_prefix("base:") {
        if BASE_COLUMNS.contains(&name) {
            return Ok(());
        }
        return Err(invalid("columns.key", format!("未知的基础列 {name}")));
    }
    Err(invalid(
        "columns.key",
        "只允许 form:<widget_id>、computed:<rule_id> 或 base:<name>",
    ))
}

fn validate_text(field: &str, value: &str, max: usize) -> Result<(), BaseError> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(invalid(field, format!("不能为空且不能超过 {max} 字符")));
    }
    Ok(())
}

fn invalid(field: &str, message: impl Into<String>) -> BaseError {
    BaseError::ParamInvalid(field.to_string(), message.into())
}

/// 批量读取当前用户多个审批定义的视图；未配置的 code 不在结果里。
pub(crate) async fn load_many(
    context: &FeishuContext,
    user_id: i64,
    approval_codes: &[String],
) -> Result<BTreeMap<String, BatchUserView>, BaseError> {
    if approval_codes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let values: Vec<Value> = approval_codes.iter().cloned().map(Value::String).collect();
    let rows = all_pages(
        context
            .batch_user_views()
            .query()
            .select_fields(&[
                VIEW_APPROVAL_CODE,
                VIEW_COMPUTED_FIELDS_JSON,
                VIEW_COLUMNS_JSON,
            ])?
            .where_eq(VIEW_USER_ID, Value::from(user_id))?
            .where_in(VIEW_APPROVAL_CODE, values)?
            .order_by("id", yang_base::table::SortOrder::Asc)?,
        100,
    )
    .await?;
    let mut views = BTreeMap::new();
    for row in rows {
        let code: String = row.require(VIEW_APPROVAL_CODE)?;
        match view_from_row(&row) {
            Ok(view) => {
                views.insert(code, view);
            }
            Err(error) => {
                tracing::warn!(error = %error, approval_code = %code, "跳过无法解析的批量审批个人视图");
            }
        }
    }
    Ok(views)
}

pub(crate) async fn load(
    context: &FeishuContext,
    user_id: i64,
    approval_code: &str,
) -> Result<Option<(i64, BatchUserView)>, BaseError> {
    let rows = context
        .batch_user_views()
        .query()
        .select_fields(&["id", VIEW_COMPUTED_FIELDS_JSON, VIEW_COLUMNS_JSON])?
        .where_eq(VIEW_USER_ID, Value::from(user_id))?
        .where_eq(VIEW_APPROVAL_CODE, Value::String(approval_code.to_string()))?
        .page(1, 1)?
        .all()
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((row.require("id")?, view_from_row(row)?)))
}

pub(crate) async fn upsert(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    approval_code: &str,
    view: &BatchUserView,
) -> Result<(), BaseError> {
    let computed_fields_json = view.computed_fields_json()?;
    let columns_json = view.columns_json()?;
    let existing = load(context, user_id, approval_code).await?;
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        if let Some((id, _)) = existing {
            let affected = context
                .batch_user_views()
                .query()
                .where_eq("id", json!(id))?
                .where_eq(VIEW_USER_ID, json!(user_id))?
                .update_in_tx(
                    &mut transaction,
                    Record::new()
                        .set(VIEW_COMPUTED_FIELDS_JSON, computed_fields_json)
                        .set(VIEW_COLUMNS_JSON, columns_json),
                )
                .await?;
            if affected != 1 {
                return Err(BaseError::RecordNotFound("视图配置不存在".to_string()));
            }
        } else {
            context
                .batch_user_views()
                .query()
                .insert_in_tx(
                    &mut transaction,
                    Record::new()
                        .set(VIEW_USER_ID, user_id)
                        .set(VIEW_APPROVAL_CODE, approval_code)
                        .set(VIEW_COMPUTED_FIELDS_JSON, computed_fields_json)
                        .set(VIEW_COLUMNS_JSON, columns_json),
                )
                .await?;
        }
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await
}

pub(crate) async fn reset(
    context: &FeishuContext,
    user_id: i64,
    approval_code: &str,
) -> Result<(), BaseError> {
    context
        .batch_user_views()
        .query()
        .where_eq(VIEW_USER_ID, json!(user_id))?
        .where_eq(VIEW_APPROVAL_CODE, json!(approval_code))?
        .delete()
        .await?;
    Ok(())
}

pub(crate) fn view_from_row(row: &Record) -> Result<BatchUserView, BaseError> {
    let computed: String = row.require(VIEW_COMPUTED_FIELDS_JSON)?;
    let columns: String = row.require(VIEW_COLUMNS_JSON)?;
    BatchUserView::from_stored(&computed, &columns)
}

pub(crate) fn normalize_codes(raw: &str) -> Result<Vec<String>, BaseError> {
    let codes: BTreeSet<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect();
    let count = codes.len();
    if count > 50 {
        return Err(invalid(
            "approval_codes",
            format!("一次最多查询 50 个审批定义，实际 {count} 个"),
        ));
    }
    Ok(codes.into_iter().collect())
}

pub(crate) fn normalize_code(raw: &str) -> Result<String, BaseError> {
    let value = raw.trim();
    if value.is_empty() || value.chars().count() > 128 {
        return Err(invalid("approval_code", "不能为空且不能超过 128 字符"));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::{normalize_code, normalize_codes, BatchUserView};
    use serde_json::json;

    fn rule(id: &str, name: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": name,
            "left_field_id": "amount",
            "operator": "multiply",
            "right_field_id": "rate",
            "precision": 2,
            "enabled": true
        })
    }

    #[test]
    fn parses_valid_view_and_columns() {
        let view = BatchUserView::parse(
            &json!([rule("c1", "人民币金额")]),
            &json!([
                {"key":"form:amount","visible":true},
                {"key":"computed:c1","visible":true}
            ]),
        )
        .unwrap_or_else(|error| panic!("应解析成功: {error}"));
        assert_eq!(view.computed_fields[0].id, "c1");
        assert_eq!(view.columns.len(), 2);
    }

    #[test]
    fn accepts_whitelisted_base_columns_and_rejects_fixed_ones() {
        assert!(BatchUserView::parse(
            &json!([]),
            &json!([
                {"key":"base:definition","visible":false},
                {"key":"base:createdAt","visible":true}
            ])
        )
        .is_ok());
        assert!(
            BatchUserView::parse(&json!([]), &json!([{"key":"base:title","visible":true}]))
                .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_rule_ids_names_and_column_keys() {
        assert!(
            BatchUserView::parse(&json!([rule("c1", "A"), rule("c1", "B")]), &json!([])).is_err()
        );
        assert!(
            BatchUserView::parse(&json!([rule("c1", "A"), rule("c2", "A")]), &json!([])).is_err()
        );
        assert!(BatchUserView::parse(
            &json!([rule("c1", "A")]),
            &json!([
                {"key":"computed:c1","visible":true},
                {"key":"computed:c1","visible":false}
            ])
        )
        .is_err());
    }

    #[test]
    fn rejects_bad_operator_precision_and_column_prefix() {
        let mut bad_operator = rule("c1", "A");
        bad_operator["operator"] = json!("power");
        assert!(BatchUserView::parse(&json!([bad_operator]), &json!([])).is_err());

        let mut bad_precision = rule("c1", "A");
        bad_precision["precision"] = json!(7);
        assert!(BatchUserView::parse(&json!([bad_precision]), &json!([])).is_err());

        assert!(BatchUserView::parse(
            &json!([rule("c1", "A")]),
            &json!([{"key":"amount","visible":true}])
        )
        .is_err());
    }

    #[test]
    fn rejects_computed_column_without_rule() {
        assert!(BatchUserView::parse(
            &json!([rule("c1", "A")]),
            &json!([{"key":"computed:missing","visible":true}])
        )
        .is_err());
    }

    #[test]
    fn normalizes_and_deduplicates_codes() {
        let codes = normalize_codes(" A, B,A ,, ").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(codes, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn rejects_blank_code() {
        assert!(normalize_code(" ").is_err());
    }

    #[test]
    fn validates_widget_ids_for_form_columns_and_operands() {
        let view = BatchUserView::parse(
            &json!([rule("c1", "A")]),
            &json!([
                {"key":"form:amount","visible":true},
                {"key":"form:missing","visible":true}
            ]),
        )
        .unwrap_or_else(|error| panic!("应解析成功: {error}"));
        let widgets = ["amount", "rate"]
            .into_iter()
            .map(ToString::to_string)
            .collect();
        assert!(view.validate_widgets(&widgets).is_err());
    }
}
