//! 批量读取当前用户多个审批定义的个人视图。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

use super::super::domain::view;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) ViewConfigsInput {
        #[param(source = query)]
        approval_codes: yang_base::definition::Str::new().require(true).max_length(8192),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只读取自己的批量审批视图
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_view_configs"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/view-configs",
        )
        .display_name("批量审批视图配置列表")
        .description("批量读取当前用户多个审批定义的个人视图")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ViewConfigsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let codes = view::normalize_codes(&input.approval_codes)?;
    let views = view::load_many(&context, user_id, &codes).await?;
    let empty = json!({ "computed_fields": [], "columns": [] });
    let payload: BTreeMap<String, Value> = codes
        .into_iter()
        .map(|code| {
            let body = views
                .get(&code)
                .map(|view| view.to_json())
                .unwrap_or_else(|| empty.clone());
            (code, body)
        })
        .collect();
    ApiResponse::success(json!({ "views": payload }), "ok")
}
