//! 设置/更换门户口令。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 口令最短长度。
const PASSPHRASE_MIN_LEN: usize = 8;

/// 设置口令的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SetPassphraseInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
    /// 新口令（明文只在这一次请求里出现）。
    pub(super) passphrase: String,
    /// 口令有效期（unix 秒）；为空表示永不过期。
    #[serde(default)]
    pub(super) expires_at: Option<i64>,
}

impl ParamInput for SetPassphraseInput {
    fn params() -> Params {
        Params::new()
    }
}

impl SetPassphraseInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.portal_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "portal_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.passphrase.chars().count() < PASSPHRASE_MIN_LEN {
            return Err(BaseError::ParamInvalid(
                "passphrase".to_string(),
                format!("口令至少 {PASSPHRASE_MIN_LEN} 个字符"),
            ));
        }
        if let Some(expires_at) = self.expires_at {
            if expires_at <= 0 {
                return Err(BaseError::ParamInvalid(
                    "expires_at".to_string(),
                    "有效期必须是 unix 秒（正整数）；不要有效期请省略该字段".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// 注册设置口令端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("set_passphrase"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Put, "/api/v1/feishu/portals/passphrase")
        .display_name("设置门户口令")
        .description("设置或更换门户的访问口令；只存 argon2 哈希，明文不落库")
        .permissions(["feishu.portal.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SetPassphraseInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 哈希在事务外做：argon2 故意慢，塞进事务会把行锁持有到几百毫秒。
    let passphrase_hash = context.password_engine().hash(&input.passphrase).await?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut record = Record::new();
        record.insert("passphrase_hash", serde_json::json!(&passphrase_hash));
        // 显式写 NULL 才能**清掉**旧的有效期：省略即保持原值，而「换成永久口令」
        // 正是要抹掉上一轮设的过期时间。
        record.insert(
            "passphrase_expires_at",
            match input.expires_at {
                Some(expires_at) => serde_json::json!(expires_at),
                None => serde_json::Value::Null,
            },
        );

        let affected = context
            .portals()
            .query()
            .where_eq("id", serde_json::json!(input.portal_id))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("门户不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource_portal", input.portal_id)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("set_passphrase")),
                (
                    "expires_at",
                    serde_json::json!(input.expires_at.map(|value| value.to_string())),
                ),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;

    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "portal_id": input.portal_id,
            "expires_at": input.expires_at,
        }),
        "口令已更新",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SetPassphraseInput {
        SetPassphraseInput {
            portal_id: 5,
            passphrase: "long-enough".to_string(),
            expires_at: None,
        }
    }

    #[test]
    fn accepts_a_long_enough_passphrase() {
        assert!(input().validate().is_ok());
    }

    #[test]
    fn rejects_a_short_passphrase() {
        let mut input = input();
        input.passphrase = "short".to_string();
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_non_positive_portal_id() {
        let mut input = input();
        input.portal_id = 0;
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_non_positive_expiry() {
        let mut input = input();
        input.expires_at = Some(0);
        assert!(
            input.validate().is_err(),
            "0 会被读成「永不过期」还是「已过期」有歧义"
        );

        input.expires_at = Some(1_800_000_000);
        assert!(input.validate().is_ok());
    }
}
