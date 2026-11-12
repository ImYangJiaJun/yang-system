//! 审批定义控件目录：供个人视图选择显示字段和计算字段的操作数。

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::get_approval_definition;
use crate::addon::feishu::domain::approval_match::preview_widgets;
use crate::addon::feishu::domain::approval_provision::parse_form;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

yang_base::params! {
    pub(super) FieldCatalogInput {
        #[param(source = query)]
        approval_code: yang_base::definition::Str::new().require(true).max_length(128),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户读取审批定义的控件目录，用于配置自己的视图
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_field_catalog"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/field-catalog",
        )
        .display_name("批量审批字段目录")
        .description("列出审批定义的控件，供当前用户配置个人视图")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: FieldCatalogInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    crate::addon::feishu::sso::require_current_user(&ctx)?;
    let code = input.approval_code.trim();
    if code.is_empty() {
        return Err(BaseError::ParamInvalid(
            "approval_code".to_string(),
            "不能为空".to_string(),
        ));
    }
    let widgets = load_widgets(&ctx, &context, code).await?;
    ApiResponse::success(serde_json::json!({ "widgets": widgets }), "ok")
}

async fn load_widgets(
    ctx: &ActionContext,
    context: &FeishuContext,
    approval_code: &str,
) -> Result<Vec<Value>, BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Err(BaseError::ParamInvalid(
            "feishu".to_string(),
            "飞书出站凭证未配置，暂时无法读取审批定义".to_string(),
        ));
    };
    let outbound = outbound_setup::build(ctx, settings)?;
    let definition = get_approval_definition(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        approval_code,
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
    Ok(preview_widgets(&widgets)
        .into_iter()
        .map(|(widget, qualified_name)| {
            serde_json::json!({
                "id": widget.id,
                "name": widget.name,
                "qualified_name": qualified_name,
                "type": widget.r#type,
                "required": widget.required,
            })
        })
        .collect())
}

/// 只取当前审批定义允许被视图引用的控件 ID。
pub(super) async fn widget_ids(
    ctx: &ActionContext,
    context: &FeishuContext,
    approval_code: &str,
) -> Result<HashSet<String>, BaseError> {
    let widgets = load_widgets(ctx, context, approval_code).await?;
    Ok(widgets
        .iter()
        .filter_map(|widget| widget.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect())
}
