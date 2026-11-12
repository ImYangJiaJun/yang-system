//! 保存当前用户在指定审批定义下的个人视图。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::domain::view::{self, BatchUserView};
use super::field_catalog;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveViewInput {
    approval_code: String,
    #[serde(default)]
    computed_fields: Vec<Value>,
    #[serde(default)]
    columns: Vec<Value>,
}

impl ParamInput for SaveViewInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只保存自己的批量审批视图
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_view_config_save"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/view-config",
        )
        .display_name("保存批量审批视图配置")
        .description("保存当前用户在指定审批定义下的计算字段与显示字段")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SaveViewInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let (code, view) = parse_view_input(input)?;
    let catalog = field_catalog::load_catalog(&ctx, &context, &code).await?;
    view.validate_widgets(&catalog)?;
    view::upsert(&ctx, &context, user_id, &code, &view).await?;

    let event = audit::succeeded_event(
        &ctx,
        None,
        Some(audit::entity("user", user_id)?),
        audit::entity("feishu_batch_user_view", format!("{user_id}:{code}"))?,
        None,
        Some(audit::summary([
            ("approval_code", json!(code)),
            ("computed_fields", json!(view.computed_fields.len())),
            ("columns", json!(view.columns.len())),
        ])?),
    )?;
    audit::append_independent(ctx.tools().mysql()?.pool(), &event).await?;
    ApiResponse::success(json!({ "view": view.to_json() }), "视图配置已保存")
}

fn parse_view_input(input: SaveViewInput) -> Result<(String, BatchUserView), BaseError> {
    let code = view::normalize_code(&input.approval_code)?;
    let view = BatchUserView::parse(
        &Value::Array(input.computed_fields),
        &Value::Array(input.columns),
    )?;
    Ok((code, view))
}

#[cfg(test)]
mod tests {
    use super::{parse_view_input, SaveViewInput};
    use serde_json::json;

    #[test]
    fn accepts_a_valid_view() {
        let input = SaveViewInput {
            approval_code: " CODE ".to_string(),
            computed_fields: vec![json!({
                "id": "c1",
                "name": "参考人民币金额",
                "left_field_id": "amount",
                "operator": "multiply",
                "right_field_id": "rate",
                "precision": 2
            })],
            columns: vec![
                json!({"key": "form:amount", "visible": true}),
                json!({"key": "computed:c1", "visible": true}),
            ],
        };
        let (code, view) = parse_view_input(input).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(code, "CODE");
        assert_eq!(view.computed_fields.len(), 1);
        assert_eq!(view.columns.len(), 2);
    }

    #[test]
    fn rejects_blank_code_and_duplicate_columns() {
        let blank = SaveViewInput {
            approval_code: " ".to_string(),
            computed_fields: Vec::new(),
            columns: Vec::new(),
        };
        assert!(parse_view_input(blank).is_err());

        let duplicate = SaveViewInput {
            approval_code: "CODE".to_string(),
            computed_fields: Vec::new(),
            columns: vec![
                json!({"key": "form:a", "visible": true}),
                json!({"key": "form:a", "visible": false}),
            ],
        };
        assert!(parse_view_input(duplicate).is_err());
    }
}
