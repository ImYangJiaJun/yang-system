//! 停用当前账号，并撤销此前签发的全部会话。

use crate::addon::account::domain::status::UserStatus;
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
pub(super) struct DisableSelfInput {}

impl ParamInput for DisableSelfInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DisableSelfResult {
    account_disabled: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: DisableSelfInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx.actor()?.user_id();
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "账号停用必须在全部实例签发 Refresh 凭据版本后启用".to_string(),
        ));
    }

    // 持锁事务内停用账号并递增安全版本；当前骨架无外围关系需要级联。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        if !locked.status().is_active() {
            return Err(BaseError::PermissionDenied("账号已经停用".to_string()));
        }
        Account::disable_locked_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            Some(audit::summary([(
                "status",
                json!(UserStatus::Active.as_str()),
            )])?),
            Some(audit::summary([(
                "status",
                json!(UserStatus::Disabled.as_str()),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;
    // 提交后尽力即时收敛 Redis 水位线；失败由 Outbox Worker 兜底。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.disable_self", "user")
        .await?;

    Account::browser_session().clear_response(
        ApiResponse::success(
            DisableSelfResult {
                account_disabled: true,
                immediate_convergence,
                relogin_required: true,
            },
            if immediate_convergence {
                "账号已停用，全部会话已撤销"
            } else {
                "账号已停用，Redis 即时收敛待后台重试"
            },
        )?,
        secure,
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("disable_self"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/disable")
        .display_name("停用当前账号")
        .description("停用当前账号，并撤销此前签发的全部会话")
        .register()
}
