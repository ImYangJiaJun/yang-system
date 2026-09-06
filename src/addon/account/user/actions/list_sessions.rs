//! 列出当前用户活跃会话并标记当前设备（路线图 C-1d）。

use crate::addon::account::domain::session::SessionView;
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Clone, Default, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListSessionsInput {}

impl ParamInput for ListSessionsInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 会话列表响应；`current` 标记请求发起的当前设备。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct SessionsList {
    pub(crate) sessions: Vec<SessionView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: ListSessionsInput,
    account: Arc<Account>,
) -> Result<SessionsList, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    // 当前请求的 session_id 从 access claims 取（老 Token 无则 None）。
    let current_session_id = account.session_id_from_request(&ctx);
    let mut sessions = account.sessions().list_active(&ctx, user_id).await?;
    for session in &mut sessions {
        session.current = current_session_id
            .as_deref()
            .is_some_and(|session_id| session_id == session.session_id);
    }
    Ok(SessionsList { sessions })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_sessions"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Get, "/api/v1/users/sessions")
        .display_name("登录设备")
        .description("列出当前用户活跃会话并标记当前设备")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_has_no_client_supplied_fields() {
        let params = <ListSessionsInput as ParamInput>::params();
        assert!(params.as_slice().is_empty());
        let injected = serde_json::from_value::<ListSessionsInput>(serde_json::json!({
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}
