//! 读取当前用户在指定审批定义下的个人视图。

use std::sync::Arc;

use serde_json::json;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

use super::super::domain::view;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) ViewConfigInput {
        #[param(source = query)]
        approval_code: yang_base::definition::Str::new().require(true).max_length(128),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只读取自己的批量审批视图
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_view_config"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/batch-approval/view-config")
        .display_name("批量审批视图配置")
        .description("读取当前用户在指定审批定义下的个人视图")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ViewConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let code = view::normalize_code(&input.approval_code)?;
    let saved = view::load(&context, user_id, &code).await?;
    ApiResponse::success(
        json!({
            "approval_code": code,
            "view": saved
                .map(|(_, view)| view.to_json())
                .unwrap_or_else(|| json!({ "computed_fields": [], "columns": [] })),
        }),
        "ok",
    )
}
