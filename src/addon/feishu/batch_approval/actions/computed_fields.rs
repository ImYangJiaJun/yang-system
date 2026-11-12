//! 批量审批列表的只读计算字段配置（管理端）。

use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params, Str};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use super::super::domain::computed::{ComputedRule, Operator};
use crate::addon::feishu::domain::approval::get_approval_definition;
use crate::addon::feishu::domain::approval_match::preview_widgets;
use crate::addon::feishu::domain::approval_provision::parse_form;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::repository::all_pages;
use crate::infrastructure::audit;

const RULE_FIELDS: &[&str] = &[
    "id",
    "approval_code",
    "name",
    "left_field_id",
    "left_field_name",
    "operator",
    "right_field_id",
    "right_field_name",
    "precision",
    "enabled",
    "sort_order",
    "updated_at",
];

yang_base::params! {
    pub(super) ListComputedFieldsInput {
        #[param(source = query)]
        approval_code: Str::new().require(false).max_length(128),
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateComputedFieldInput {
    approval_code: String,
    name: String,
    left_field_id: String,
    left_field_name: Option<String>,
    operator: String,
    right_field_id: String,
    right_field_name: Option<String>,
    precision: Option<i64>,
    enabled: Option<bool>,
    sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateComputedFieldInput {
    id: i64,
    approval_code: String,
    name: String,
    left_field_id: String,
    left_field_name: Option<String>,
    operator: String,
    right_field_id: String,
    right_field_name: Option<String>,
    precision: Option<i64>,
    enabled: Option<bool>,
    sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteComputedFieldInput {
    id: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComputedFieldView {
    id: i64,
    approval_code: String,
    name: String,
    left_field_id: String,
    left_field_name: Option<String>,
    operator: String,
    right_field_id: String,
    right_field_name: Option<String>,
    precision: i64,
    enabled: bool,
    sort_order: i64,
    updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct Draft {
    approval_code: String,
    name: String,
    left_field_id: String,
    left_field_name: Option<String>,
    operator: String,
    right_field_id: String,
    right_field_name: Option<String>,
    precision: i64,
    enabled: bool,
    sort_order: i64,
}

impl ParamInput for CreateComputedFieldInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ParamInput for UpdateComputedFieldInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ParamInput for DeleteComputedFieldInput {
    fn params() -> Params {
        Params::new()
    }
}

impl CreateComputedFieldInput {
    fn draft(&self) -> Result<Draft, BaseError> {
        draft(
            &self.approval_code,
            &self.name,
            &self.left_field_id,
            self.left_field_name.as_deref(),
            &self.operator,
            &self.right_field_id,
            self.right_field_name.as_deref(),
            self.precision,
            self.enabled,
            self.sort_order,
        )
    }
}

impl UpdateComputedFieldInput {
    fn draft(&self) -> Result<Draft, BaseError> {
        if self.id <= 0 {
            return Err(BaseError::ParamInvalid(
                "id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        draft(
            &self.approval_code,
            &self.name,
            &self.left_field_id,
            self.left_field_name.as_deref(),
            &self.operator,
            &self.right_field_id,
            self.right_field_name.as_deref(),
            self.precision,
            self.enabled,
            self.sort_order,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn draft(
    approval_code: &str,
    name: &str,
    left_field_id: &str,
    left_field_name: Option<&str>,
    operator: &str,
    right_field_id: &str,
    right_field_name: Option<&str>,
    precision: Option<i64>,
    enabled: Option<bool>,
    sort_order: Option<i64>,
) -> Result<Draft, BaseError> {
    let approval_code = approval_code.trim();
    if approval_code.is_empty() || approval_code.chars().count() > 128 {
        return Err(BaseError::ParamInvalid(
            "approval_code".to_string(),
            "审批定义 Code 不能为空且不能超过 128 字符".to_string(),
        ));
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(BaseError::ParamInvalid(
            "name".to_string(),
            "字段名不能为空且不能超过 64 字符".to_string(),
        ));
    }
    let left_field_id = non_empty_id("left_field_id", left_field_id)?;
    let right_field_id = non_empty_id("right_field_id", right_field_id)?;
    let operator = Operator::parse(operator).ok_or_else(|| {
        BaseError::ParamInvalid(
            "operator".to_string(),
            "运算符只支持 add/subtract/multiply/divide".to_string(),
        )
    })?;
    let precision = precision.unwrap_or(2);
    if !(0..=6).contains(&precision) {
        return Err(BaseError::ParamInvalid(
            "precision".to_string(),
            "小数位必须在 0 到 6 之间".to_string(),
        ));
    }
    let sort_order = sort_order.unwrap_or(0);
    if sort_order < 0 {
        return Err(BaseError::ParamInvalid(
            "sort_order".to_string(),
            "排序不能为负数".to_string(),
        ));
    }
    Ok(Draft {
        approval_code: approval_code.to_string(),
        name: name.to_string(),
        left_field_id,
        left_field_name: trimmed(left_field_name),
        operator: operator.as_str().to_string(),
        right_field_id,
        right_field_name: trimmed(right_field_name),
        precision,
        enabled: enabled.unwrap_or(true),
        sort_order,
    })
}

fn non_empty_id(field: &str, value: &str) -> Result<String, BaseError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            "控件 ID 不能为空且不能超过 128 字符".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    let context_list = Arc::clone(&context);
    let context_create = Arc::clone(&context);
    let context_update = Arc::clone(&context);
    let context_delete = Arc::clone(&context);
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_computed_list"),
            move |ctx, input| list_handle(ctx, input, Arc::clone(&context_list)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/computed-fields",
        )
        .display_name("批量审批计算字段列表")
        .description("列出批量审批表格中的只读计算字段配置")
        .permissions(["feishu.approval.read"])
        .register()
        .action_fn(
            yang_base::action_name!("feishu_batch_computed_create"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context_create)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/computed-fields",
        )
        .display_name("新增批量审批计算字段")
        .description("为指定审批定义配置一个只读二元计算字段")
        .permissions(["feishu.approval.write"])
        .register()
        .action_fn(
            yang_base::action_name!("feishu_batch_computed_update"),
            move |ctx, input| update_handle(ctx, input, Arc::clone(&context_update)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/computed-fields/update",
        )
        .display_name("修改批量审批计算字段")
        .description("修改只读计算字段的控件、运算、精度或启用状态")
        .permissions(["feishu.approval.write"])
        .register()
        .action_fn(
            yang_base::action_name!("feishu_batch_computed_delete"),
            move |ctx, input| delete_handle(ctx, input, Arc::clone(&context_delete)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/computed-fields/delete",
        )
        .display_name("删除批量审批计算字段")
        .description("删除只读计算字段配置")
        .permissions(["feishu.approval.write"])
        .register()
}

async fn list_handle(
    _ctx: ActionContext,
    input: ListComputedFieldsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let approval_code = input.approval_code.as_deref().map(str::trim);
    let mut query = context
        .batch_computed_fields()
        .query()
        .select_fields(RULE_FIELDS)?
        .order_by("sort_order", SortOrder::Asc)?
        .order_by("id", SortOrder::Asc)?;
    if let Some(code) = approval_code.filter(|code| !code.is_empty()) {
        query = query.where_eq("approval_code", serde_json::json!(code))?;
    }
    let rows = all_pages(query, 100).await?;
    let fields = rows
        .iter()
        .map(view_from_record)
        .collect::<Result<Vec<_>, _>>()?;
    ApiResponse::success(serde_json::json!({ "fields": fields }), "ok")
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateComputedFieldInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let draft = input.draft()?;
    validate_fields_exist(&ctx, &context, &draft).await?;
    ensure_name_available(&context, &draft, None).await?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<i64, BaseError> = async {
        let (_, raw_id) = context
            .batch_computed_fields()
            .query()
            .insert_returning_id_in_tx(&mut transaction, record_from_draft(&draft))
            .await?;
        let id = i64::try_from(raw_id)
            .map_err(|_| BaseError::Unknown("计算字段主键超出 i64".to_string()))?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            audit_subject(&ctx)?,
            audit::entity("feishu_batch_computed_field", id)?,
            None,
            Some(audit::summary([
                ("approval_code", serde_json::json!(draft.approval_code)),
                ("name", serde_json::json!(draft.name)),
                ("operator", serde_json::json!(draft.operator)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(id)
    }
    .await;
    let id = FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(serde_json::json!({ "id": id }), "计算字段已创建")
}

async fn update_handle(
    ctx: ActionContext,
    input: UpdateComputedFieldInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let draft = input.draft()?;
    let existing = load_rule(&context, input.id).await?;
    validate_fields_exist(&ctx, &context, &draft).await?;
    ensure_name_available(&context, &draft, Some(input.id)).await?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let affected = context
            .batch_computed_fields()
            .query()
            .where_eq("id", serde_json::json!(input.id))?
            .update_in_tx(&mut transaction, record_from_draft(&draft))
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("计算字段不存在".to_string()));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            audit_subject(&ctx)?,
            audit::entity("feishu_batch_computed_field", input.id)?,
            Some(audit::summary([
                ("name", serde_json::json!(existing.name)),
                ("operator", serde_json::json!(existing.operator)),
            ])?),
            Some(audit::summary([
                ("name", serde_json::json!(draft.name)),
                ("operator", serde_json::json!(draft.operator)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(serde_json::json!({ "id": input.id }), "计算字段已更新")
}

async fn delete_handle(
    ctx: ActionContext,
    input: DeleteComputedFieldInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "必须是正整数".to_string(),
        ));
    }
    let existing = load_rule(&context, input.id).await?;
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let affected = context
            .batch_computed_fields()
            .query()
            .where_eq("id", serde_json::json!(input.id))?
            .delete_in_tx(&mut transaction)
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("计算字段不存在".to_string()));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            audit_subject(&ctx)?,
            audit::entity("feishu_batch_computed_field", input.id)?,
            Some(audit::summary([
                ("name", serde_json::json!(existing.name)),
                ("operator", serde_json::json!(existing.operator)),
            ])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(serde_json::json!({ "id": input.id }), "计算字段已删除")
}

async fn validate_fields_exist(
    ctx: &ActionContext,
    context: &FeishuContext,
    draft: &Draft,
) -> Result<(), BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Err(BaseError::ParamInvalid(
            "feishu".to_string(),
            "飞书出站凭证未配置，暂时无法校验审批控件".to_string(),
        ));
    };
    let outbound = outbound_setup::build(ctx, settings)?;
    let definition = get_approval_definition(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &draft.approval_code,
    )
    .await
    .map_err(outbound_setup::outbound_error)?;
    let form = definition.form.as_ref().ok_or_else(|| {
        BaseError::ParamInvalid(
            "approval_code".to_string(),
            "审批定义没有返回表单".to_string(),
        )
    })?;
    let widgets = parse_form(form)
        .map_err(|error| BaseError::ParamInvalid("approval_code".to_string(), error.to_string()))?;
    let available: HashSet<String> = preview_widgets(&widgets)
        .into_iter()
        .map(|(widget, _)| widget.id.clone())
        .collect();
    for (field, id) in [
        ("left_field_id", &draft.left_field_id),
        ("right_field_id", &draft.right_field_id),
    ] {
        if !available.contains(id) {
            return Err(BaseError::ParamInvalid(
                field.to_string(),
                format!("审批定义里不存在控件 {id}"),
            ));
        }
    }
    Ok(())
}

async fn ensure_name_available(
    context: &FeishuContext,
    draft: &Draft,
    except_id: Option<i64>,
) -> Result<(), BaseError> {
    let rows = context
        .batch_computed_fields()
        .query()
        .select_fields(&["id"])?
        .where_eq("approval_code", serde_json::json!(draft.approval_code))?
        .where_eq("name", serde_json::json!(draft.name))?
        .page(1, 1)?
        .all()
        .await?;
    if let Some(row) = rows.first() {
        let id: i64 = row.require("id")?;
        if Some(id) != except_id {
            return Err(BaseError::ParamInvalid(
                "name".to_string(),
                "同一审批定义下字段名不能重复".to_string(),
            ));
        }
    }
    Ok(())
}

async fn load_rule(context: &FeishuContext, id: i64) -> Result<ComputedRule, BaseError> {
    let rows = context
        .batch_computed_fields()
        .query()
        .select_fields(&[
            "id",
            "approval_code",
            "name",
            "left_field_id",
            "operator",
            "right_field_id",
            "precision",
        ])?
        .where_eq("id", serde_json::json!(id))?
        .page(1, 1)?
        .all()
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| BaseError::RecordNotFound("计算字段不存在".to_string()))?;
    ComputedRule::from_record(row)
}

fn audit_subject(ctx: &ActionContext) -> Result<Option<audit::AuditEntity>, BaseError> {
    ctx.authenticated_user()
        .map(|user| audit::entity("user", user.id))
        .transpose()
}

fn record_from_draft(draft: &Draft) -> Record {
    Record::new()
        .set("approval_code", draft.approval_code.clone())
        .set("name", draft.name.clone())
        .set("left_field_id", draft.left_field_id.clone())
        .set("left_field_name", draft.left_field_name.clone())
        .set("operator", draft.operator.clone())
        .set("right_field_id", draft.right_field_id.clone())
        .set("right_field_name", draft.right_field_name.clone())
        .set("precision", draft.precision)
        .set("enabled", draft.enabled)
        .set("sort_order", draft.sort_order)
}

fn view_from_record(row: &Record) -> Result<ComputedFieldView, BaseError> {
    Ok(ComputedFieldView {
        id: row.require("id")?,
        approval_code: row.require("approval_code")?,
        name: row.require("name")?,
        left_field_id: row.require("left_field_id")?,
        left_field_name: row.optional("left_field_name")?,
        operator: row.require("operator")?,
        right_field_id: row.require("right_field_id")?,
        right_field_name: row.optional("right_field_name")?,
        precision: row.require("precision")?,
        enabled: row.require("enabled")?,
        sort_order: row.require("sort_order")?,
        updated_at: row.optional("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_of(operator: &str, precision: i64) -> Result<Draft, BaseError> {
        draft(
            " CODE ",
            " 参考人民币金额 ",
            " amount ",
            Some(" 原币金额 "),
            operator,
            " rate ",
            Some(" 汇率 "),
            Some(precision),
            None,
            Some(10),
        )
    }

    #[test]
    fn draft_trims_and_defaults() {
        let draft = draft_of("multiply", 2).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(draft.approval_code, "CODE");
        assert_eq!(draft.name, "参考人民币金额");
        assert_eq!(draft.left_field_name.as_deref(), Some("原币金额"));
        assert!(draft.enabled);
        assert_eq!(draft.sort_order, 10);
    }

    #[test]
    fn rejects_unknown_operator_and_bad_precision() {
        assert!(draft_of("power", 2).is_err());
        assert!(draft_of("multiply", 7).is_err());
        assert!(draft_of("multiply", -1).is_err());
    }

    #[test]
    fn rejects_same_or_empty_field_id() {
        assert!(draft(
            "CODE",
            "名称",
            " amount ",
            None,
            "multiply",
            " amount ",
            None,
            Some(2),
            None,
            None
        )
        .is_ok());
        assert!(draft(
            "CODE",
            "名称",
            "",
            None,
            "multiply",
            "rate",
            None,
            Some(2),
            None,
            None
        )
        .is_err());
    }
}
