//! 批量审批 Action 注册表。

mod attachment;
pub(crate) mod cancel_job;
mod create_job;
mod detail_rows;
mod field_catalog;
mod get_job;
mod list_tasks;
mod task_detail;
mod user_search;
mod view_config_get;
mod view_config_reset;
mod view_config_save;
mod view_configs;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;

pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    let module = list_tasks::register(module, Arc::clone(&context));
    let module = task_detail::register(module, Arc::clone(&context));
    let module = detail_rows::register(module, Arc::clone(&context));
    let module = user_search::register(module, Arc::clone(&context));
    let module = create_job::register(module, Arc::clone(&context));
    let module = get_job::register(module, Arc::clone(&context));
    let module = attachment::register(module, Arc::clone(&context));
    let module = field_catalog::register(module, Arc::clone(&context));
    let module = view_configs::register(module, Arc::clone(&context));
    let module = view_config_get::register(module, Arc::clone(&context));
    let module = view_config_save::register(module, Arc::clone(&context));
    let module = view_config_reset::register(module, Arc::clone(&context));
    cancel_job::register(module, context)
}

/// 统一的「取当前用户飞书 access token」入口：未绑定/需重新授权时返回可行动
/// 业务码，前端据此显示「重新授权飞书」。
pub(super) async fn access_token_or_error(
    ctx: &yang_base::action::ActionContext,
    context: &FeishuContext,
    user_id: i64,
) -> Result<String, yang_base::action::ApiResponse> {
    let Some(settings) = context.settings() else {
        return Err(yang_base::action::ApiResponse::fail(
            50301,
            "飞书集成未配置",
        ));
    };
    match super::super::sso::credentials::access_token_for_user(ctx, context, settings, user_id)
        .await
    {
        Ok(token) => Ok(token),
        Err(error) => Err(yang_base::action::ApiResponse::fail(
            40901,
            error.user_message(),
        )),
    }
}
