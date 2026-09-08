//! TOTP 停用：关闭第二因子并作废全部恢复码（需登录 + Step-up）。
//!
//! 停用是安全降级操作，必须经 Step-up 重认证；已激活账号的 Step-up 本身
//! 要求出示第二因子（动态码/恢复码/邮箱验证码，见 `step_up.rs`），即
//! 「必须证明仍持有第二因子才能关闭它」。版本递增使既有会话全部失效，
//! 强制重新登录（无 MFA）。

use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TotpDeactivateInput {}

impl ParamInput for TotpDeactivateInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: TotpDeactivateInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;

    let observed = account
        .users()
        .find_totp_state_by_id(&ctx, user_id)
        .await?
        .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
    Account::ensure_active(observed.status)?;
    if observed.totp_activated_at.is_none() {
        return Err(BaseError::ConfigError("TOTP 未激活，无需停用".to_string()));
    }

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        account
            .users()
            .deactivate_totp_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::increment_versions_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            None,
            Some(audit::summary([("totp_activated", json!(false))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    // 版本递增使旧 Refresh 失效；登录回到单因子（仅密码）。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.totp_deactivate", "user")
        .await?;

    ApiResponse::success(
        json!({
            "totp_activated": false,
            "immediate_convergence": immediate_convergence,
            "relogin_required": true,
        }),
        if immediate_convergence {
            "双重验证已关闭，请重新登录"
        } else {
            "双重验证已关闭；Redis 即时收敛待后台重试，请重新登录"
        },
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 未配置 TOTP 密钥域时不注册（与 setup/activate 同一开关）。
    if account.totp_settings().is_none() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("totp_deactivate"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/mfa/totp/deactivate")
        .display_name("TOTP 停用")
        .description("关闭 TOTP 第二因子并作废全部恢复码，既有会话失效")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_accepts_empty_body_only() {
        let params = <TotpDeactivateInput as ParamInput>::params();
        assert!(params.as_slice().is_empty());
        assert!(serde_json::from_str::<TotpDeactivateInput>("{}").is_ok());
        assert!(serde_json::from_str::<TotpDeactivateInput>(r#"{"force":true}"#).is_err());
    }
}
