//! 导出当前用户的个人数据（GDPR Art 20 数据可携带权）。

use crate::addon::account::domain::login_event::LoginEventView;
use crate::addon::account::domain::session::SessionView;
use crate::addon::account::user::table::UserView;
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportDataInput {}

impl ParamInput for ExportDataInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 个人数据导出载荷：资料 + 会话 + 登录安全事件（可机器读取的结构化 JSON）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DataExport {
    user: UserView,
    sessions: Vec<SessionView>,
    security_events: Vec<LoginEventView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: ExportDataInput,
    account: Arc<Account>,
) -> Result<DataExport, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    let user = account.view_by_id(&ctx, user_id).await?;
    let sessions = account.sessions().list_active(&ctx, user_id).await?;
    let security_events = account
        .login_events()
        .list_for_user(&ctx, user_id, 1, 100)
        .await?;
    Ok(DataExport {
        user,
        sessions,
        security_events,
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("export_data"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Get, "/api/v1/users/export")
        .display_name("导出个人数据")
        .description("导出当前用户的个人数据（资料、会话、安全事件）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_has_no_client_supplied_fields() {
        let params = <ExportDataInput as ParamInput>::params();
        assert!(params.as_slice().is_empty());
        let injected = serde_json::from_value::<ExportDataInput>(serde_json::json!({
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}
