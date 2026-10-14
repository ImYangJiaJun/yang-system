//! 审批派发配置的访问 Token 回显：把封存的明文取出来给运维复制。
//!
//! # 这是纯读
//!
//! 它**不重新生成、不改变任何状态**：同一个 Token 可以一直用、反复复制。要换值走
//! [`super::rotate_config_token`]——那条会让旧值立即失效，是**破坏性**的，不能拿它
//! 当「看一眼 Token」的入口。
//!
//! # 为什么必须有这一条
//!
//! 明文只在签发/轮换的响应里出现一次，之后库里只有 `token_hash`（校验）与
//! `token_cipher`（加密）。没有回显，运维想再抄一次 Token 就只能轮换——而那会
//! 打断正在跑的工作流。数据源侧早就有 `reveal_token` 与 `rotate_token` 这一对，
//! 这里补齐前一半。
//!
//! # 权限键用 `feishu.approval.write`
//!
//! 数据源侧用的是独立的 `feishu.datasource.secret`。这里刻意**不**新增一位：
//! 能轮换的人本来就能从轮换响应里拿到明文，回显不给他们任何新能力——多一位
//! 管理员等价权限只会多一道授予闸门，换不来实际收敛。
//!
//! # 为什么必须过 `unseal_verified`
//!
//! CBC 只保密、不保证完整性：单靠 PKCS#7 填充校验判断「密文被篡改 / 钥匙不对 /
//! 贴错了行」是**概率性**的（错钥匙下约 1/256 恰好合法）。同一条记录上的
//! `token_hash` 是随手可得的校验和，用它把三类事故都变成**确定性**失败。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::unseal_verified;
use crate::infrastructure::audit;

/// 失败码。与 `dispatch.rs` / `rotate_config_token.rs` 的数值域同段不重号。
mod codes {
    /// 配置不存在。
    pub(super) const CONFIG_NOT_FOUND: i32 = 40401;
    /// 这条配置还没签发过凭据（`token_hash` / `token_cipher` 为空）。
    pub(super) const NOT_ISSUED: i32 = 40907;
    /// 密文解不开（被篡改 / 封装密钥换了 / 密文与摘要不同源）。
    pub(super) const CREDENTIAL_UNREADABLE: i32 = 40906;
}

/// 回显输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RevealConfigTokenInput {
    /// 目标配置的 `config_id`（控制台列表/详情已知）。
    pub(super) config_id: i64,
}

impl ParamInput for RevealConfigTokenInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RevealConfigTokenInput {
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

/// 为什么这条配置的明文取不出来。
///
/// 与数据源侧的 `CredentialError` 同构，但少一个变体：审批配置的 Token 没有
/// 「运维手工填」这条路径，所以「没有密文」与「没有摘要」是同一件事——都等于
/// **还没签发**，修法也一样（点一次「轮换访问 Token」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CredentialError {
    /// 还没签发：`token_hash` 或 `token_cipher` 为空。
    NotIssued,
    /// 有密文但解不开。
    Unreadable(String),
}

impl CredentialError {
    fn code(&self) -> i32 {
        match self {
            Self::NotIssued => codes::NOT_ISSUED,
            Self::Unreadable(_) => codes::CREDENTIAL_UNREADABLE,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NotIssued => {
                "这条配置还没有签发过访问 Token，点一次「轮换访问 Token」即可签发".to_string()
            }
            Self::Unreadable(reason) => {
                format!("访问 Token 无法回显：{reason}。轮换一次即可重新签发一份可回显的凭据")
            }
        }
    }
}

/// 纯函数：从配置行的两列里取出明文。
///
/// **必须走 [`unseal_verified`]**——它拿同一条记录上的 `token_hash` 当校验和。
/// 只用 `unseal` 的话，密文被篡改 / 换了封装密钥 / 把 A 行密文贴到 B 行这三件事
/// 都只会「大概率」失败。
pub(super) fn open_credential(
    cipher: Option<&str>,
    expected_hash: Option<&str>,
    key: &[u8; 32],
) -> Result<String, CredentialError> {
    // 两列都可空（存量行建于自动签发之前）。任一为空都按「还没签发」处置——
    // 那正是可行动的那句话。
    let cipher = cipher.map(str::trim).filter(|value| !value.is_empty());
    let expected_hash = expected_hash
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (Some(cipher), Some(expected_hash)) = (cipher, expected_hash) else {
        return Err(CredentialError::NotIssued);
    };
    unseal_verified(cipher, expected_hash, key)
        .map_err(|error| CredentialError::Unreadable(error.to_string()))
}

/// 注册访问 Token 回显端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("reveal_config_token"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/configs/reveal-token",
        )
        .display_name("回显访问 Token")
        .description(
            "取出这条配置上封存的工作流访问 Token 明文（纯读，不改变任何状态；每次追加审计）",
        )
        // 复用 write 而不是新增一位 secret 权限：能轮换的人本来就能拿到明文，
        // 回显不新增能力（见模块文档）。
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RevealConfigTokenInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let config_id = input.config_id;

    // 封存密钥缺了就解不开任何东西。报可归因的 `ConfigError`，不静默降级成
    // 「这条配置取不出来」。
    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：访问 Token 回显需要它解开封存的密文".to_string(),
        )
    })?;

    // ⚠️ 两个凭据列都标了 `secret`（= `hidden`），**不进默认投影**：不显式点名
    // 就查不出来（报「字段未找到」）。本仓为此踩过一次（`user_token` 的
    // `load_single`）。
    let row = context
        .approval_configs()
        .query()
        .select_fields(&["id", "token_hash", "token_cipher"])?
        .where_eq("id", serde_json::json!(config_id))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Ok(ApiResponse::fail(codes::CONFIG_NOT_FOUND, "配置不存在"));
    };

    let token_hash: Option<String> = row.optional("token_hash")?;
    let token_cipher: Option<String> = row.optional("token_cipher")?;

    let plaintext = match open_credential(
        token_cipher.as_deref(),
        token_hash.as_deref(),
        &wrapping_key,
    ) {
        Ok(plaintext) => plaintext,
        Err(error) => return Ok(ApiResponse::fail(error.code(), error.message())),
    };

    // 每次成功回显都追加一条审计。**纯读**：这条事务里只写审计，不碰任何业务状态。
    // 明文本身绝不进审计摘要——摘要是给人查的，不是第二份凭据。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", config_id)?,
            None,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("revealed"),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(serde_json::json!({ "token": plaintext }), "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::crypto::{derive_key, seal};

    fn key() -> [u8; 32] {
        derive_key("test-credential-wrapping-key")
    }

    #[test]
    fn reveal_returns_the_stored_plaintext_not_a_fresh_one() {
        // 「一直可以复制同一个值」是硬需求：回显必须**解密出原值**，不是现场新生成
        let (hash, cipher) = seal("s3cret-token", &key()).unwrap_or_else(|e| panic!("{e}"));
        let first = open_credential(Some(&cipher), Some(&hash), &key())
            .unwrap_or_else(|error| panic!("应可回显: {error:?}"));
        let second = open_credential(Some(&cipher), Some(&hash), &key())
            .unwrap_or_else(|error| panic!("应可回显: {error:?}"));
        assert_eq!(first, "s3cret-token");
        assert_eq!(first, second, "回显是纯读，两次必须拿到同一个值");
    }

    #[test]
    fn an_unissued_config_says_so_with_the_actionable_hint() {
        // 存量行（建于自动签发之前）两列都是 NULL：理由必须点明「点一次轮换」，
        // 否则运维只会反复点「复制」并以为界面坏了。
        let error = open_credential(None, None, &key())
            .err()
            .unwrap_or_else(|| panic!("还没签发就该拒绝"));
        assert_eq!(error, CredentialError::NotIssued);
        assert_eq!(error.code(), codes::NOT_ISSUED);
        assert!(error.message().contains("轮换"), "{}", error.message());

        // 空白串与 NULL 是同一件事；只有一半（有密文没摘要）同样按未签发处置。
        assert_eq!(
            open_credential(Some("   "), Some("   "), &key()).err(),
            Some(CredentialError::NotIssued)
        );
        assert_eq!(
            open_credential(Some("some-cipher"), None, &key()).err(),
            Some(CredentialError::NotIssued)
        );
    }

    #[test]
    fn a_cipher_from_another_record_is_refused() {
        // 把 A 行的密文贴到 B 行：密文本身完全合法，只有摘要比对能发现
        let (_, cipher_a) = seal("token-a", &key()).unwrap_or_else(|e| panic!("{e}"));
        let (hash_b, _) = seal("token-b", &key()).unwrap_or_else(|e| panic!("{e}"));
        let error = open_credential(Some(&cipher_a), Some(&hash_b), &key())
            .err()
            .unwrap_or_else(|| panic!("密文与摘要不同源就该拒绝"));
        assert_eq!(error.code(), codes::CREDENTIAL_UNREADABLE);
        assert!(matches!(error, CredentialError::Unreadable(_)));
    }

    #[test]
    fn the_wrong_wrapping_key_is_refused_deterministically() {
        let (hash, cipher) = seal("t", &key()).unwrap_or_else(|e| panic!("{e}"));
        let other = derive_key("a-different-wrapping-key");
        assert!(open_credential(Some(&cipher), Some(&hash), &other).is_err());
    }

    #[test]
    fn rejects_a_non_positive_config_id() {
        assert!(RevealConfigTokenInput { config_id: 0 }.validate().is_err());
        assert!(RevealConfigTokenInput { config_id: 7 }.validate().is_ok());
    }
}
