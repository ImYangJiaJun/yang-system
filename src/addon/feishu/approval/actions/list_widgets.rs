//! 按审批定义列出控件（前端控制台向导的「控件预览」步骤）。
//!
//! 只读语义但**出站调飞书**（`approvals get`）并消耗本应用的频控配额，所以与
//! `list_bitable_*` 同理归到 write 一侧。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::get_approval_definition;
use crate::addon::feishu::domain::approval_provision::parse_form;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 列出控件的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListWidgetsInput {
    /// 审批定义 code。
    pub(super) approval_code: String,
}

impl ParamInput for ListWidgetsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListWidgetsInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.approval_code.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "approval_code".to_string(),
                "不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册控件预览端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_widgets"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/definitions/widgets",
        )
        .display_name("列出审批定义控件")
        .description("按审批定义 code 调 approvals get 并解析表单，返回控件列表供向导预览")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListWidgetsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 出站调飞书：凭证缺了根本走不下去（照 dispatch provision 的处置）。
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };

    let outbound = outbound_setup::build(&ctx, settings)?;
    let definition = get_approval_definition(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        input.approval_code.trim(),
    )
    .await
    .map_err(outbound_setup::outbound_error)?;
    if definition.is_external {
        return Err(BaseError::ParamInvalid(
            "approval_code".to_string(),
            "该 approval_code 是三方审批定义，不能用 instances create 提单".to_string(),
        ));
    }
    let form = definition.form.as_ref().ok_or_else(|| {
        BaseError::ParamInvalid(
            "approval_code".to_string(),
            "审批定义没有返回表单".to_string(),
        )
    })?;
    // 与建配置同一条解析路径（两种形态都认：JSON 字符串 / 数组）。
    let widgets = parse_form(form)
        .map_err(|error| BaseError::ParamInvalid("approval_code".to_string(), error.to_string()))?;

    // 明细父级也返回供分组展示，只有子控件参与列映射。
    let items: Vec<serde_json::Value> =
        crate::addon::feishu::domain::approval_match::preview_widgets(&widgets)
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
            .collect();

    ApiResponse::success(serde_json::json!({ "widgets": items }), "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(code: &str) -> ListWidgetsInput {
        ListWidgetsInput {
            approval_code: code.to_string(),
        }
    }

    #[test]
    fn blank_approval_code_is_rejected() {
        assert!(input("  ").validate().is_err());
        assert!(input("").validate().is_err());
    }

    #[test]
    fn valid_code_passes() {
        assert!(input("CODE-TEST").validate().is_ok());
    }
}
