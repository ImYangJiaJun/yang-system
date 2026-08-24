//! 创建一个新用户。

use crate::addon::account::domain::policy::{
    normalize_username, validate_password, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
    USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH, USERNAME_PATTERN,
};
use crate::addon::account::user::table::UserView;
use crate::addon::account::{Account, OwnerClaimOutcome};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::auth::{normalize_email, AuthOperation, RegistrationEmailVerification};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Password, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RegisterInput {
        username: Str::new()
            .title("用户名")
            .require(true)
            .min_length(USERNAME_MIN_LENGTH)
            .max_length(USERNAME_MAX_LENGTH)
            .pattern(USERNAME_PATTERN),
        password: Password::new()
            .title("登录密码")
            .require(true)
            .min_length(PASSWORD_MIN_LENGTH)
            .max_length(PASSWORD_MAX_LENGTH),
        email: Str::new()
            .title("注册邮箱")
            .require(true)
            .max_length(254)
            .email(),
        email_code: Str::new()
            .title("邮箱验证码")
            .require(true)
            .min_length(6)
            .max_length(6)
            .pattern(r"^[0-9]{6}$"),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RegisterInput,
    account: Arc<Account>,
) -> Result<UserView, BaseError> {
    let username = normalize_username(&input.username)?;
    let email = normalize_email(&input.email)?;
    validate_password(&input.password)?;
    account
        .rate_limiter()
        .check(&ctx, AuthOperation::Register, &username)
        .await?;
    if account.users().username_exists(&ctx, &username).await? {
        return Err(BaseError::ParamInvalid(
            "username".to_string(),
            "用户名已存在".to_string(),
        ));
    }
    RegistrationEmailVerification::from_context(&ctx)?
        .consume(&ctx, &email, &input.email_code)
        .await?;
    let password_hash = account.passwords().hash(&input.password).await?;
    let email_verified_at = current_unix_timestamp()?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let id = match account
            .users()
            .insert_in_tx(
                &ctx,
                &mut transaction,
                &username,
                &password_hash,
                &email,
                email_verified_at,
            )
            .await
        {
            Ok(id) => id,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::ParamInvalid(
                    "registration".to_string(),
                    "用户名或邮箱已被其他请求注册，请重新获取验证码".to_string(),
                ));
            }
            Err(error) => return Err(error),
        };
        // 当前骨架注入的是不声明的默认实现，永不进入 Claimed 分支；
        // 端口保留给未来平台管理 Addon 重新引入最终管理员声明。
        if let OwnerClaimOutcome::Claimed { admin_id } = account
            .claim_system_owner(&mut transaction, id, &username)
            .await?
        {
            let event = audit::succeeded_system_event(
                &ctx,
                "first-registration",
                None,
                Some(audit::entity("user", id)?),
                audit::entity("admin_account", admin_id)?,
                None,
                Some(audit::summary([
                    ("admin", json!(true)),
                    ("system_owner", json!(true)),
                    ("user_id", json!(id)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }
        Ok(id)
    }
    .await;
    let id = Account::finish_transaction(transaction, result).await?;
    account.view_by_id(&ctx, id).await
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
    module
        .action_fn(yang_base::action_name!("register"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/register")
        .display_name("注册用户")
        .description("创建一个新用户")
        .success_status(201)
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn registration_contract_requires_email_ownership_proof() {
        let params = <RegisterInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["username", "password", "email", "email_code"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }
}
