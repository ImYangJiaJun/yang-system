//! 递增持久会话版本并撤销当前账号此前签发的全部 Access 与 Refresh Token。

use crate::addon::account::Account;
use crate::audit;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserLogoutInput {}

impl ParamInput for BrowserLogoutInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BrowserLogoutResult {
    revoked_all_sessions: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: BrowserLogoutInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx.actor()?.user_id();
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "全量会话撤销必须在全部实例签发 Refresh 凭据版本后启用".to_string(),
        ));
    }

    // 持久撤销：同一把用户行锁内递增两个安全版本并写授权 Outbox。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        Account::increment_versions_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("session_set", user_id)?,
            None,
            Some(audit::summary([
                ("relogin_required", json!(true)),
                ("revocation_requested", json!(true)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;
    // 提交后尽力即时收敛 Redis 水位线；失败由 Outbox Worker 兜底。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.logout", "session_set")
        .await?;

    Account::browser_session().clear_response(
        ApiResponse::success(
            BrowserLogoutResult {
                revoked_all_sessions: true,
                immediate_convergence,
                relogin_required: true,
            },
            if immediate_convergence {
                "已撤销全部会话"
            } else {
                "持久会话已撤销，Redis 即时收敛待后台重试"
            },
        )?,
        secure,
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("logout"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/logout")
        .display_name("退出全部会话")
        .description("递增持久会话版本并撤销当前账号此前签发的全部 Access 与 Refresh Token")
        .register()
}
