//! 创建一个新用户。

use crate::addon::account::domain::policy::{
    normalize_username, validate_password_field, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
    USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH, USERNAME_PATTERN,
};
use crate::addon::account::user::table::UserView;
use crate::addon::account::{Account, OwnerClaimOutcome};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::AuthOperation;
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
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RegisterInput,
    account: Arc<Account>,
) -> Result<UserView, BaseError> {
    let username = normalize_username(&input.username)?;
    validate_password_field("password", &input.password, Some(&username))?;
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
    let password_hash = account.passwords().hash(&input.password).await?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let id = match account
            .users()
            .insert_in_tx(&ctx, &mut transaction, &username, &password_hash)
            .await
        {
            Ok(id) => id,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::ParamInvalid(
                    "registration".to_string(),
                    "用户名已被其他请求注册".to_string(),
                ));
            }
            Err(error) => return Err(error),
        };
        // 引导是「尽力而为的一次性事件」：AlreadyClaimed 是正常结果，
        // 不得阻断注册；只有真实数据库故障才让整个事务回滚。
        let outcome = account
            .claim_system_owner(&ctx, &mut transaction, id, &username)
            .await?;
        if let OwnerClaimOutcome::Claimed { admin_id } = outcome {
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
    fn registration_contract_is_username_and_password_only() {
        let params = <RegisterInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["username", "password"]);
        assert!(params.as_slice().iter().all(|param| param.required));

        let injected = serde_json::from_value::<RegisterInput>(serde_json::json!({
            "username": "alice",
            "password": "correct-horse-battery",
            "email": "alice@example.com",
            "email_code": "123456"
        }));
        assert!(injected.is_err(), "注册不再接受邮箱字段");
    }
}
