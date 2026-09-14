//! TOTP 配置初始化：生成共享密钥与 `otpauth://` URI（需登录 + Step-up）。
//!
//! 只生成密钥并展示 URI，**不激活**——用户用认证器扫描 URI 后，
//! 以 [`super::totp_activate`] 验码激活。

use crate::addon::account::domain::mfa::{generate_totp_secret, otpauth_uri};
use crate::addon::account::Account;
use crate::config::TotpSettings;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TotpSetupInput {}

impl ParamInput for TotpSetupInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: TotpSetupInput,
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

    // 已激活的账号不允许重复 setup（必须先 disable 或保持现状）。
    if observed.totp_activated_at.is_some() {
        return Err(BaseError::ConfigError(
            "TOTP 已激活，请先停用后再重新配置".to_string(),
        ));
    }

    let totp = ctx.tools().config::<TotpSettings>()?;
    let secret = generate_totp_secret();
    // 展示用 URI 以登录用户身份（username）为标签。
    let uri = otpauth_uri("yang-system", &observed.username, &secret, totp.digits);

    ApiResponse::success(
        json!({
            "secret": secret,
            "otpauth_uri": uri,
            "activated": false,
            "digits": totp.digits,
        }),
        "请在认证器应用中扫描二维码或手动输入密钥",
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 未配置 TOTP 密钥域时不注册。
    if account.totp_settings().is_none() {
        return module;
    }
    module
        .action_fn(yang_base::action_name!("totp_setup"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/mfa/totp/setup")
        .display_name("TOTP 配置初始化")
        .description("生成 TOTP 共享密钥与 otpauth URI（未激活）")
        .register()
}
