//! 审批定义控件目录：供个人视图选择显示字段和计算字段的操作数。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

use super::super::domain::widget_catalog::{self, WidgetCatalog};
use crate::addon::feishu::domain::approval::get_approval_definition;
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
    let widgets = load_catalog(&ctx, &context, code).await?;
    ApiResponse::success(serde_json::json!({ "widgets": widgets.to_json() }), "ok")
}

pub(super) async fn load_catalog(
    ctx: &ActionContext,
    context: &FeishuContext,
    approval_code: &str,
) -> Result<WidgetCatalog, BaseError> {
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
    Ok(widget_catalog::build(&widgets))
}
