//! 飞书用户身份授权状态（控制台「飞书审批配置」页那张授权卡片的数据源）。
//!
//! 只读、JWT 鉴权、**无条件注册**：控制台不依赖出站凭证存在（与其它控制台
//! Action 同一条纪律）。
//!
//! `reauth_due` 用的是与提醒 worker **同一个**判据（`user_token::reauth_reminder_due`），
//! 免得「页面说还好、邮件已经在催」这种两套口径。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::user_token;

/// 状态查询输入：无参数。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OauthStatusInput {}

impl ParamInput for OauthStatusInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 授权状态投影。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct OauthStatus {
    /// 是否已授权（表里有没有那一行）。
    authorized: bool,
    /// 授权人姓名（授权时从 `/authen/v1/user_info` 取回）。
    name: Option<String>,
    /// 最近一次**完整授权**的时间（unix 秒）；刷新不重置它。
    authorized_at: Option<i64>,
    /// 是否已过「该重新授权」的提醒线（350 天）。
    reauth_due: bool,
}

/// 注册授权状态端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("oauth_status"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/oauth/status")
        .display_name("飞书用户身份授权状态")
        .description("查询本系统的飞书用户身份（OAuth）授权状态")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: OauthStatusInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let now = user_token::now_unix_secs();
    let row = user_token::load_single(context.user_tokens()).await?;
    let status = match row {
        Some(row) => OauthStatus {
            authorized: true,
            name: row.user_name,
            authorized_at: Some(row.authorized_at),
            reauth_due: user_token::reauth_reminder_due(row.authorized_at, now),
        },
        None => OauthStatus {
            authorized: false,
            name: None,
            authorized_at: None,
            reauth_due: false,
        },
    };
    ApiResponse::success(status, "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_projection_serializes_the_console_fields() {
        let value = serde_json::to_value(OauthStatus {
            authorized: true,
            name: Some("杨佳俊".to_string()),
            authorized_at: Some(1_700_000_000),
            reauth_due: false,
        })
        .unwrap_or_else(|error| panic!("应可序列化: {error}"));
        assert_eq!(value["authorized"], serde_json::json!(true));
        assert_eq!(value["name"], serde_json::json!("杨佳俊"));
        assert_eq!(value["reauth_due"], serde_json::json!(false));
    }
}
