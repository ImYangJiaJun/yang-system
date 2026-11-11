//! 转交/加签选人：飞书通讯录搜索。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::api;
use super::access_token_or_error;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) UserSearchInput {
        #[param(source = query)]
        q: Str::new().require(true).max_length(64),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_user_search"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/users/search",
        )
        .display_name("飞书通讯录搜索")
        .description("按关键词搜索飞书用户，供转交/加签选人")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UserSearchInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let token = match access_token_or_error(&ctx, &context, user_id).await {
        Ok(token) => token,
        Err(response) => return Ok(response),
    };
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书集成未配置"));
    };
    let users = api::search_users(&ctx, settings, &token, &input.q)
        .await
        .map_err(|error| error.to_base())?;
    ApiResponse::success(users, "ok")
}
