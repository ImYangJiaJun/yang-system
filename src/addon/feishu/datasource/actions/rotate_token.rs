//! 凭据轮换：为一条字段绑定重新签发 Token，并**立即**让旧值失效。
//!
//! # 与回显的分工
//!
//! 回显（[`super::reveal_token`]）是纯读，什么时候点都没有后果；轮换是**写**，
//! 由使用人按需触发（决策 D10）。这是刻意分开的两个端点：做成「复制即轮换」曾
//! 被认真考虑过（那样连密文都不必存），但**误点一次复制就会让已配好的控件失效**。
//!
//! # 后果必须写清楚
//!
//! 轮换后，审批后台里配了该字段的控件会**立即失效**，必须把新的 Token 粘回去。
//! 界面要把这句话放在二次确认框里，不是藏在 tooltip 里（§10.3）。服务端这边能做的
//! 是把「旧 Token 失效」这件事做到确定：摘要与密文**同一个事务**换掉。
//!
//! # 三列必须一起换
//!
//! `token_hash`（校验）、`token_cipher`（回显）、`token_rotated_at`（控制台展示）。
//! 只换其中一列都会造成半坏状态：换了摘要不换密文，回显出来的还是旧 Token——
//! 运维把「复制」的旧值粘回控件，于是永远验不过，且没有任何报错指向真正的原因。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::{issue_token, IssuedToken};
use crate::infrastructure::audit;

/// 失败码。与 `reveal_token` 共用数值域。
mod codes {
    /// 数据源字段绑定不存在。
    pub(super) const FIELD_NOT_FOUND: i32 = 40401;
}

/// 轮换输入。
///
/// `source_key` 走 **body**（与回显一致）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RotateTokenInput {
    /// 目标字段绑定的 `source_key`。
    pub(super) source_key: String,
}

impl ParamInput for RotateTokenInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RotateTokenInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.source_key.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 一次轮换要写回的那几列。收成纯函数，好让「三列必须一起换」有测试钉着。
fn rotation_columns(issued: &IssuedToken, rotated_at: i64) -> Record {
    let mut update = Record::new();
    update.insert("token_hash", serde_json::json!(&issued.hash));
    update.insert("token_cipher", serde_json::json!(&issued.cipher));
    update.insert("token_rotated_at", serde_json::json!(rotated_at));
    update
}

/// 当前时间（unix 秒）。
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// 注册凭据轮换端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("rotate_token"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/rotate-token")
        .display_name("轮换凭据")
        .description("重新签发 Token（旧值立即失效，控件需回审批后台改）；每次追加审计")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RotateTokenInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let source_key = input.source_key.trim().to_string();

    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：轮换后的凭据需要它封存，否则无法回显".to_string(),
        )
    })?;

    let issued = issue_token(&wrapping_key)?;
    let rotated_at = now_seconds();

    // 生成 + 落库 + 返回**同一事务**：不然会出现「返回了但没存上」——那种情况下
    // 控件已经配好新 Token，而服务端手上还是旧的，永远验不过。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<Option<i64>, BaseError> = async {
        // 事务里先读回绑定行：确认它存在，并拿到 datasource_id 给审计挂 subject。
        let row = context
            .datasource_fields()
            .query()
            .select_fields(&["id", "datasource_id"])?
            .where_eq("source_key", serde_json::json!(&source_key))?
            .optional()
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let binding_id: i64 = row.require("id")?;
        let datasource_id: i64 = row.require("datasource_id")?;

        context
            .datasource_fields()
            .query()
            .where_eq("id", serde_json::json!(binding_id))?
            .update_in_tx(&mut transaction, rotation_columns(&issued, rotated_at))
            .await?;

        // 审计：谁、何时、轮换了哪一条绑定。摘要键不得含 `token` / `secret` / `hash`
        // 等敏感词（`audit::AuditSummary` 会拒），故用 `rotated_at`。
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("feishu_datasource", datasource_id)?),
            audit::entity("feishu_datasource_field", binding_id)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("rotated")),
                ("rotated_at", serde_json::json!(rotated_at)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(Some(binding_id))
    }
    .await;
    let Some(_) = FeishuContext::finish_transaction(transaction, result).await? else {
        return Ok(ApiResponse::fail(codes::FIELD_NOT_FOUND, "字段绑定不存在"));
    };

    ApiResponse::success(serde_json::json!({ "token": issued.plaintext }), "轮换成功")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::crypto::{derive_key, seal, unseal_verified};
    use crate::addon::feishu::domain::token::verify_token;

    fn key() -> [u8; 32] {
        derive_key("test-credential-wrapping-key")
    }

    fn issued(plaintext: &str) -> IssuedToken {
        let (hash, cipher) =
            seal(plaintext, &key()).unwrap_or_else(|error| panic!("应可封存: {error}"));
        IssuedToken {
            plaintext: plaintext.to_string(),
            hash,
            cipher,
        }
    }

    #[test]
    fn rotation_rewrites_all_three_columns_together() {
        // 只换其中一列会造出半坏状态：换了摘要不换密文 → 回显出来的还是旧 Token，
        // 运维把旧值粘回控件，于是永远验不过且不报错。
        let token = issued("new-token");
        let columns = rotation_columns(&token, 1_700_000_000);

        let hash: String = columns.require("token_hash").unwrap_or_default();
        let cipher: String = columns.require("token_cipher").unwrap_or_default();
        let rotated_at: i64 = columns.require("token_rotated_at").unwrap_or_default();

        assert_eq!(hash, token.hash);
        assert_eq!(cipher, token.cipher);
        assert_eq!(rotated_at, 1_700_000_000);
        // 三样都要在，缺一列 `require` 就会失败（上面已经断言）
    }

    #[test]
    fn the_new_credential_reveals_to_exactly_the_new_plaintext() {
        // 轮换后「复制」拿到的必须是新值，且旧值立即失效——两件事一起成立
        let old = issued("old-token");
        let new = issued("new-token");

        assert_ne!(new.hash, old.hash);
        assert_ne!(new.cipher, old.cipher);
        assert!(
            !verify_token("old-token", &new.hash),
            "旧 Token 必须立即失效"
        );
        assert_eq!(
            unseal_verified(&new.cipher, &new.hash, &key())
                .unwrap_or_else(|error| panic!("新密文应可解封: {error}")),
            "new-token"
        );
    }

    #[test]
    fn rejects_a_blank_source_key() {
        let blank = RotateTokenInput {
            source_key: String::new(),
        };
        assert!(blank.validate().is_err());
        let ok = RotateTokenInput {
            source_key: "currency".to_string(),
        };
        assert!(ok.validate().is_ok());
    }
}
