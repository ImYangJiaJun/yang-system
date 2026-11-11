//! 拉取当前用户的飞书待办，并按审批定义分组。

use std::collections::BTreeMap;
use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::api;
use super::access_token_or_error;
use crate::addon::feishu::domain::context::FeishuContext;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListTasksInput {}

impl ParamInput for ListTasksInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_list_tasks"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/batch-approval/tasks")
        .display_name("批量审批待办")
        .description("列出当前用户的飞书待办并按审批定义分组")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: ListTasksInput,
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
    let list = api::list_pending_tasks(&ctx, settings, &token)
        .await
        .map_err(|error| error.to_base())?;
    let mut definitions: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for task in &list.tasks {
        let code = task
            .definition_code
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let name = task
            .definition_name
            .clone()
            .unwrap_or_else(|| "未命名审批".to_string());
        definitions
            .entry(code)
            .and_modify(|entry| {
                entry.1 += 1;
                if entry.0.is_empty() {
                    entry.0 = name.clone();
                }
            })
            .or_insert((name, 1));
    }
    let definitions: Vec<serde_json::Value> = definitions
        .into_iter()
        .map(|(code, (name, count))| {
            serde_json::json!({ "code": code, "name": name, "count": count })
        })
        .collect();
    ApiResponse::success(
        serde_json::json!({
            "tasks": list.tasks,
            "definitions": definitions,
            "truncated": list.truncated,
        }),
        "ok",
    )
}
