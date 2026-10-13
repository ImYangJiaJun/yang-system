//! 审批派发配置的访问 Token 轮换。
//!
//! 与 datasource 侧的 `rotate_token` 是**同一套语义**：轮换是**写**、由使用人
//! 按需触发（决策 D10 的对齐），摘要与密文**同一个事务**换掉，明文只在响应里
//! 出现一次，之后只能靠 `unseal_verified`（用同一条记录上的 `token_hash` 当校验和）
//! 取回。
//!
//! # 轮换即失效
//!
//! 轮换后，工作流里配的旧 Token 立即失效——必须把新值粘回飞书工作流。界面要把
//! 这句话放在二次确认框里，不是藏在 tooltip 里。服务端这边能做的是把「旧 Token
//! 失效」这件事做到确定：`config_key` 不变（路由稳定，改的是凭据），只换
//! `token_hash` / `token_cipher` / `token_rotated_at` 三列。
//!
//! # 与 `create_config` 的分工
//!
//! `create_config` 在建配置时**顺带**签发一次（否则「建好了却没签发」会让工作流
//! 请求落在 40402 而控制台看不到提示）；本端点只做轮换，不重复建配置的其它事务。

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

/// 失败码。与 `dispatch.rs` 的鉴权码同域不重号。
mod codes {
    /// 配置不存在。
    pub(super) const CONFIG_NOT_FOUND: i32 = 40401;
}

/// 轮换输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RotateConfigTokenInput {
    /// 目标配置的 `config_id`（控制台列表/详情已知）。
    pub(super) config_id: i64,
}

impl ParamInput for RotateConfigTokenInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RotateConfigTokenInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "config_id".to_string(),
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

/// 注册配置 Token 轮换端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("rotate_config_token"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/configs/rotate-token",
        )
        .display_name("轮换访问 Token")
        .description("为一条审批派发配置重新签发工作流访问 Token（旧值立即失效，工作流需粘新值）；每次追加审计")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RotateConfigTokenInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let config_id = input.config_id;

    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：轮换后的访问 Token 需要它封存，否则无法回显".to_string(),
        )
    })?;

    let issued = issue_token(&wrapping_key)?;
    let rotated_at = now_seconds();

    // 生成 + 落库 + 返回**同一事务**：不然会出现「返回了但没存上」——那种情况下
    // 工作流已经配好新 Token，而服务端手上还是旧的，永远验不过。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<Option<i64>, BaseError> = async {
        let updated = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(config_id))?
            .update_in_tx(&mut transaction, rotation_columns(&issued, rotated_at))
            .await?;
        if updated == 0 {
            return Ok(None);
        }

        // 审计：谁、何时、轮换了哪一条配置。摘要键不得含 `token` / `secret` / `hash`
        // 等敏感词（`audit::AuditSummary` 会拒），故用 `rotated_at`。
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", config_id)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("rotated")),
                ("rotated_at", serde_json::json!(rotated_at)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(Some(config_id))
    }
    .await;
    let Some(_) = FeishuContext::finish_transaction(transaction, result).await? else {
        return Ok(ApiResponse::fail(codes::CONFIG_NOT_FOUND, "配置不存在"));
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
        // 运维把旧值粘回工作流，于是永远验不过且不报错。
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
    fn rejects_a_non_positive_config_id() {
        let blank = RotateConfigTokenInput { config_id: 0 };
        assert!(blank.validate().is_err());
        let ok = RotateConfigTokenInput { config_id: 7 };
        assert!(ok.validate().is_ok());
    }
}
