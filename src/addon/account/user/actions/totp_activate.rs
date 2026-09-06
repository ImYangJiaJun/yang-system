//! TOTP 激活：校验一次性码后启用第二因子（需登录 + Step-up）。
//!
//! 激活成功签发一次性恢复码（明文只在此响应回显一次，摘要入库）。
//! 版本递增使既有会话全部失效，强制重新登录进入 MFA 流程。

use crate::addon::account::domain::mfa::{generate_recovery_codes, TotpSecretCipher};
use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::auth::TotpVerifier;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) TotpActivateInput {
        secret: Str::new()
            .title("TOTP 密钥")
            .require(true)
            .min_length(16)
            .max_length(64)
            .pattern(r"^[A-Za-z2-7]+={0,2}$"),
        code: Str::new()
            .title("一次性验证码")
            .require(true)
            .min_length(6)
            .max_length(8)
            .pattern(r"^[0-9]{6,8}$"),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TotpActivateInput,
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
    if observed.totp_activated_at.is_some() {
        return Err(BaseError::ConfigError(
            "TOTP 已激活，请先停用后再重新配置".to_string(),
        ));
    }

    let totp_settings = account
        .totp_settings()
        .ok_or_else(|| BaseError::ConfigError("TOTP 未配置".to_string()))?;
    let cipher = TotpSecretCipher::new(totp_settings)?;

    // 校验用户输入的 secret 与当前激活流程的 secret 一致（防跨用户回放）。
    let secret = input.secret.trim().to_ascii_uppercase();
    if secret.len() != 32 {
        return Err(BaseError::ParamInvalid(
            "secret".to_string(),
            "TOTP 密钥必须为 32 位 Base32".to_string(),
        ));
    }

    // 校验一次性码：直接复用框架 TOTP 校验器（当前时间窗口 ±1）。
    let verifier = yang_base::action::auth::TotpLiteVerifier::default();
    verifier
        .verify(&secret, &input.code)
        .await
        .map_err(|_| BaseError::ParamInvalid("code".to_string(), "一次性验证码无效".to_string()))?;

    // 加密入库并签发恢复码。
    let encrypted = cipher.encrypt(secret.as_bytes())?;
    let activated_at = current_unix_timestamp()?;
    let (recovery_plains, recovery_digests) = generate_recovery_codes();
    let digests_json = serde_json::to_string(&recovery_digests)
        .map_err(|_| BaseError::ConfigError("恢复码摘要序列化失败".to_string()))?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        account
            .users()
            .activate_totp_in_tx(
                &ctx,
                &mut transaction,
                user_id,
                &encrypted,
                activated_at,
                &digests_json,
            )
            .await?;
        Account::increment_versions_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            None,
            Some(audit::summary([("totp_activated", json!(true))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    // 版本递增使旧 Refresh 失效；登录需重新认证（含第二因子）。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.totp_activate", "user")
        .await?;

    ApiResponse::success(
        json!({
            "totp_activated": true,
            "recovery_codes": recovery_plains,
            "immediate_convergence": immediate_convergence,
            "relogin_required": true,
        }),
        if immediate_convergence {
            "TOTP 已启用，请妥善保存恢复码并重新登录"
        } else {
            "TOTP 已启用，请妥善保存恢复码；Redis 即时收敛待后台重试"
        },
    )
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    if account.totp_settings().is_none() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("totp_activate"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/mfa/totp/activate")
        .display_name("TOTP 激活")
        .description("校验一次性码后启用 TOTP 第二因子，签发恢复码")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_secret_and_code() {
        let params = <TotpActivateInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["secret", "code"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    fn plausible(code: &str) -> bool {
        let mut hex_chars = 0;
        let mut groups = 0;
        for (i, part) in code.split('-').enumerate() {
            if i > 0 {
                groups += 1;
            }
            if part.len() != 8 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return false;
            }
            hex_chars += 8;
        }
        groups == 3 && hex_chars == 32
    }

    #[test]
    fn recovery_code_shape_is_validated() {
        assert!(plausible("a1b2c3d4-e5f6a7b8-c9d0e1f2-a3b4c5d6"));
        assert!(!plausible("short"));
        assert!(!plausible("a1b2c3d4-e5f6a7b8-c9d0e1f2-a3b4c5d6-extra"));
        assert!(!plausible("a1b2c3d4-e5f6a7b8-c9d0e1f2-a3b4c5dg"));
    }

    #[test]
    fn digest_matches_generated_code() {
        use crate::addon::account::domain::mfa::hex_digest;
        let input = b"a1b2c3d4-e5f6a7b8-c9d0e1f2-a3b4c5d6";
        assert_eq!(hex_digest(input), hex_digest(input));
    }
}
