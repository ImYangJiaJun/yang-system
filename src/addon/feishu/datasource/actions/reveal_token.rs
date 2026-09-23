//! 凭据回显：把该字段绑定上封存的 Token 明文取出来给运维复制。
//!
//! # 这是纯读
//!
//! 它**不重新生成、不改变任何状态**：同一个 Token 可以一直用、反复复制（这正是最初
//! 的需求）。要换值走 [`super::rotate_token`]。
//!
//! 用独立权限位 `feishu.datasource.secret`（**不是** `...read`）：读的是凭据，
//! 与「能不能看数据源列表」是两件事。每次成功回显都追加一条审计——谁、什么时候、
//! 取走了哪一条凭据的明文。
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

/// 失败码。与 `pull_now` / `health_check` 共用数值域（409 段 = 数据源类）。
mod codes {
    /// 数据源字段绑定不存在。
    pub(super) const FIELD_NOT_FOUND: i32 = 40401;
    /// 这条绑定的 Token 没有密文（运维手填的），我们本来就没有它。
    pub(super) const NOT_REVEALABLE: i32 = 40907;
    /// 密文解不开（被篡改 / 封装密钥换了 / 密文与摘要不同源）。
    pub(super) const CREDENTIAL_UNREADABLE: i32 = 40906;
}

/// 回显输入。
///
/// `source_key` 走 **body**：与 `update_datasource_table` / `delete_datasource_table`
/// 一致（本模块的标识一律走 body，路由不带路径段）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RevealTokenInput {
    /// 目标字段绑定的 `source_key`（进 URL 的数据源标识，全局唯一）。
    pub(super) source_key: String,
}

impl ParamInput for RevealTokenInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RevealTokenInput {
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

/// 为什么这条绑定的明文取不出来。
///
/// 分成两个变体是因为**修法完全不同**：手填的没有密文（只能轮换一次换掉它），
/// 解不开的是凭据本身坏了（轮换可以直接修）。笼统报一句「取不出来」等于没答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CredentialError {
    /// 没有 `token_cipher`：这是运维在旧入口手填的 Token，系统从来没有它的明文。
    NoCipher,
    /// 有密文但解不开。
    Unreadable(String),
}

impl CredentialError {
    fn code(&self) -> i32 {
        match self {
            Self::NoCipher => codes::NOT_REVEALABLE,
            Self::Unreadable(_) => codes::CREDENTIAL_UNREADABLE,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NoCipher => "这条绑定的 Token 是手工填写的，系统没有它的密文，无法回显。\
                 轮换一次即可重新签发一份可回显的凭据"
                .to_string(),
            Self::Unreadable(reason) => {
                format!("凭据无法回显：{reason}。轮换一次即可重新签发一份可回显的凭据")
            }
        }
    }
}

/// 纯函数：从绑定行的两列里取出明文。
///
/// **必须走 [`unseal_verified`]**——它拿同一条记录上的 `token_hash` 当校验和。
/// 只用 `unseal` 的话，密文被篡改 / 换了封装密钥 / 把 A 行密文贴到 B 行这三件事
/// 都只会「大概率」失败。
pub(super) fn open_credential(
    cipher: Option<&str>,
    expected_hash: &str,
    key: &[u8; 32],
) -> Result<String, CredentialError> {
    let Some(cipher) = cipher.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(CredentialError::NoCipher);
    };
    unseal_verified(cipher, expected_hash, key)
        .map_err(|error| CredentialError::Unreadable(error.to_string()))
}

/// 注册凭据回显端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("reveal_token"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/reveal-token")
        .display_name("回显凭据")
        .description("取出该字段绑定上封存的 Token 明文（纯读，不改变任何状态；每次追加审计）")
        // **不是** feishu.datasource.read：读的是凭据，与「能不能看数据源列表」
        // 是两件事。拿到明文就能冒充这条数据源出站，所以它必须是单独的一位。
        .permissions(["feishu.datasource.secret"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RevealTokenInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let source_key = input.source_key.trim().to_string();

    // 封存密钥缺了就解不开任何东西。照 T5（`create_datasource_table`）的口径报
    // 可归因的 `ConfigError`，不静默降级成「这条绑定取不出来」。
    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：凭据回显需要它解开封存的密文".to_string(),
        )
    })?;

    let row = context
        .datasource_fields()
        .query()
        .select_fields(&["id", "datasource_id", "token_hash", "token_cipher"])?
        .where_eq("source_key", serde_json::json!(&source_key))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Ok(ApiResponse::fail(codes::FIELD_NOT_FOUND, "字段绑定不存在"));
    };

    let binding_id: i64 = row.require("id")?;
    let datasource_id: i64 = row.require("datasource_id")?;
    let token_hash: String = row.require("token_hash")?;
    let token_cipher: Option<String> = row.optional("token_cipher")?;

    let plaintext = match open_credential(token_cipher.as_deref(), &token_hash, &wrapping_key) {
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
            Some(audit::entity("feishu_datasource", datasource_id)?),
            audit::entity("feishu_datasource_field", binding_id)?,
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
        let first = open_credential(Some(&cipher), &hash, &key())
            .unwrap_or_else(|error| panic!("应可回显: {error:?}"));
        let second = open_credential(Some(&cipher), &hash, &key())
            .unwrap_or_else(|error| panic!("应可回显: {error:?}"));
        assert_eq!(first, "s3cret-token");
        assert_eq!(first, second, "回显是纯读，两次必须拿到同一个值");
    }

    #[test]
    fn a_hand_entered_token_has_no_cipher_and_says_so() {
        // 手工填的 Token 我们从来没有它的明文（§10.2）。理由必须点明「轮换一次」，
        // 否则运维只会反复点「复制」并以为界面坏了。
        let error = open_credential(None, "whatever", &key())
            .err()
            .unwrap_or_else(|| panic!("没有密文就该拒绝"));
        assert_eq!(error, CredentialError::NoCipher);
        assert_eq!(error.code(), codes::NOT_REVEALABLE);
        assert!(error.message().contains("轮换"), "{}", error.message());

        // 空白串与 NULL 是同一件事
        assert_eq!(
            open_credential(Some("   "), "whatever", &key()).err(),
            Some(CredentialError::NoCipher)
        );
    }

    #[test]
    fn a_cipher_from_another_record_is_refused() {
        // 把 A 行的密文贴到 B 行：密文本身完全合法，只有摘要比对能发现
        let (_, cipher_a) = seal("token-a", &key()).unwrap_or_else(|e| panic!("{e}"));
        let (hash_b, _) = seal("token-b", &key()).unwrap_or_else(|e| panic!("{e}"));
        let error = open_credential(Some(&cipher_a), &hash_b, &key())
            .err()
            .unwrap_or_else(|| panic!("密文与摘要不同源就该拒绝"));
        assert_eq!(error.code(), codes::CREDENTIAL_UNREADABLE);
        assert!(matches!(error, CredentialError::Unreadable(_)));
    }

    #[test]
    fn the_wrong_wrapping_key_is_refused_deterministically() {
        let (hash, cipher) = seal("t", &key()).unwrap_or_else(|e| panic!("{e}"));
        let other = derive_key("a-different-wrapping-key");
        assert!(open_credential(Some(&cipher), &hash, &other).is_err());
    }

    #[test]
    fn rejects_a_blank_source_key() {
        let blank = RevealTokenInput {
            source_key: "   ".to_string(),
        };
        assert!(blank.validate().is_err());
        let ok = RevealTokenInput {
            source_key: "currency".to_string(),
        };
        assert!(ok.validate().is_ok());
    }
}
