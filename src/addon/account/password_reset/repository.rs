//! 密码重置凭证的生成、摘要与持久化仓储边界。

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::MySqlPool;
use std::fmt::Write;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder, SqlExpr, Transaction};

const RAW_TOKEN_BYTES: usize = 32;
const RAW_TOKEN_CHARS: usize = RAW_TOKEN_BYTES * 2;
const FINGERPRINT_CHARS: usize = 16;

pub(crate) struct GeneratedPasswordReset {
    raw_token: String,
    reference: PasswordResetReference,
}

impl GeneratedPasswordReset {
    pub(crate) fn generate() -> Result<Self, BaseError> {
        let mut random = [0_u8; RAW_TOKEN_BYTES];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| BaseError::Unknown("密码重置凭证随机源不可用".to_string()))?;
        let raw_token = encode_hex(&random)?;
        let reference = PasswordResetReference::from_bytes(&random)?;
        Ok(Self {
            raw_token,
            reference,
        })
    }

    pub(crate) fn raw_token(&self) -> &str {
        &self.raw_token
    }

    pub(crate) fn reference(&self) -> &PasswordResetReference {
        &self.reference
    }
}

pub(crate) struct PasswordResetReference {
    digest: String,
    fingerprint: String,
}

impl PasswordResetReference {
    pub(crate) fn parse(raw_token: &str) -> Result<Self, BaseError> {
        let bytes = decode_hex(raw_token).ok_or_else(invalid_reset_token)?;
        Self::from_bytes(&bytes)
    }

    fn from_bytes(bytes: &[u8; RAW_TOKEN_BYTES]) -> Result<Self, BaseError> {
        let digest = encode_hex(&Sha256::digest(bytes))?;
        let fingerprint = digest
            .get(..FINGERPRINT_CHARS)
            .ok_or_else(|| BaseError::Unknown("密码重置凭证指纹生成失败".to_string()))?
            .to_string();
        Ok(Self {
            digest,
            fingerprint,
        })
    }

    pub(crate) fn attempt_fingerprint(raw_token: &str) -> Result<String, BaseError> {
        let digest = encode_hex(&Sha256::digest(raw_token.as_bytes()))?;
        digest
            .get(..FINGERPRINT_CHARS)
            .map(str::to_owned)
            .ok_or_else(|| BaseError::Unknown("密码重置尝试指纹生成失败".to_string()))
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

pub(crate) struct LockedPasswordReset {
    id: i64,
    user_id: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
    invalidated_at: Option<i64>,
    database_now: i64,
}

impl LockedPasswordReset {
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    pub(crate) fn is_usable(&self) -> bool {
        self.consumed_at.is_none()
            && self.invalidated_at.is_none()
            && self.expires_at > self.database_now
    }
}

pub(crate) async fn create_in_tx(
    transaction: &mut Transaction,
    target_user_id: i64,
    requested_by_user_id: i64,
    reset: &GeneratedPasswordReset,
    ttl_seconds: u64,
) -> Result<(), BaseError> {
    let ttl_seconds = i64::try_from(ttl_seconds)
        .map_err(|_| BaseError::ConfigError("密码重置 TTL 超出 MySQL 范围".to_string()))?;
    transaction
        .table(table!("password_reset_token"))
        .set_expr(field!("invalidated_at"), SqlExpr::unix_timestamp())
        .where_and(field!("user_user"), CompareOp::Eq, target_user_id)
        .where_null(field!("consumed_at"))
        .where_null(field!("invalidated_at"))
        .update(&serde_json::json!({}))
        .await?;
    transaction
        .table(table!("password_reset_token"))
        .set_expr(
            field!("expires_at"),
            SqlExpr::unix_timestamp_add(ttl_seconds),
        )
        .set_expr(field!("created_at"), SqlExpr::unix_timestamp())
        .insert(&serde_json::json!({
            "token_digest": reset.reference.digest,
            "token_fingerprint": reset.reference.fingerprint(),
            "user_user": target_user_id,
            "requested_by_user": requested_by_user_id,
        }))
        .await?;
    Ok(())
}

pub(crate) async fn find_target_user(
    pool: &MySqlPool,
    reference: &PasswordResetReference,
) -> Result<Option<i64>, BaseError> {
    QueryBuilder::from_pool(pool, table!("password_reset_token"))
        .where_and(
            field!("token_digest"),
            CompareOp::Eq,
            reference.digest.as_str(),
        )
        .value::<i64>(field!("user_user"))
        .await
        .map_err(BaseError::from)
}

/// 锁定凭证行；`pool` 只用于构建查询，语句仍在事务连接上以 `FOR UPDATE` 执行。
pub(crate) async fn lock_in_tx(
    pool: &MySqlPool,
    transaction: &mut Transaction,
    reference: &PasswordResetReference,
) -> Result<LockedPasswordReset, BaseError> {
    transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("password_reset_token"))
                .field(field!("id"))
                .field(field!("user_user"))
                .field(field!("expires_at"))
                .field(field!("consumed_at"))
                .field(field!("invalidated_at"))
                .select_expr(SqlExpr::unix_timestamp(), field!("database_now"))
                .where_and(
                    field!("token_digest"),
                    CompareOp::Eq,
                    reference.digest.as_str(),
                ),
        )
        .await?
        .into_iter()
        .next()
        .map(
            |(id, user_id, expires_at, consumed_at, invalidated_at, database_now)| {
                LockedPasswordReset {
                    id,
                    user_id,
                    expires_at,
                    consumed_at,
                    invalidated_at,
                    database_now,
                }
            },
        )
        .ok_or_else(invalid_reset_token)
}

pub(crate) async fn consume_in_tx(
    transaction: &mut Transaction,
    locked: &LockedPasswordReset,
) -> Result<(), BaseError> {
    if !locked.is_usable() {
        return Err(invalid_reset_token());
    }
    let consumed = transaction
        .table(table!("password_reset_token"))
        .set_expr(field!("consumed_at"), SqlExpr::unix_timestamp())
        .where_and(field!("id"), CompareOp::Eq, locked.id)
        .where_null(field!("consumed_at"))
        .where_null(field!("invalidated_at"))
        .where_expr(
            field!("expires_at"),
            CompareOp::Gt,
            SqlExpr::unix_timestamp(),
        )?
        .update(&serde_json::json!({}))
        .await?;
    if consumed != 1 {
        return Err(invalid_reset_token());
    }
    transaction
        .table(table!("password_reset_token"))
        .set_expr(field!("invalidated_at"), SqlExpr::unix_timestamp())
        .where_and(field!("user_user"), CompareOp::Eq, locked.user_id)
        .where_and(field!("id"), CompareOp::Ne, locked.id)
        .where_null(field!("consumed_at"))
        .where_null(field!("invalidated_at"))
        .update(&serde_json::json!({}))
        .await?;
    Ok(())
}

pub(crate) fn invalid_reset_token() -> BaseError {
    BaseError::Unauthorized("密码重置凭证无效或已过期".to_string())
}

fn encode_hex(bytes: &[u8]) -> Result<String, BaseError> {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| BaseError::Unknown("密码重置凭证编码失败".to_string()))?;
    }
    Ok(encoded)
}

fn decode_hex(value: &str) -> Option<[u8; RAW_TOKEN_BYTES]> {
    if value.len() != RAW_TOKEN_CHARS || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0_u8; RAW_TOKEN_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        decoded[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_high_entropy_shaped_and_database_value_is_only_a_digest() {
        let reset = GeneratedPasswordReset::generate()
            .unwrap_or_else(|error| panic!("应生成重置凭证: {error}"));
        assert_eq!(reset.raw_token().len(), RAW_TOKEN_CHARS);
        assert!(reset
            .raw_token()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
        assert_ne!(reset.raw_token(), reset.reference.digest);
        assert_eq!(reset.reference.fingerprint().len(), FINGERPRINT_CHARS);
        let reparsed = PasswordResetReference::parse(reset.raw_token())
            .unwrap_or_else(|error| panic!("生成的凭证应可解析: {error}"));
        assert_eq!(reparsed.digest, reset.reference.digest);
        assert_eq!(reparsed.fingerprint, reset.reference.fingerprint);
    }

    #[test]
    fn malformed_and_expired_or_consumed_tokens_fail_closed() {
        for malformed in ["", "abcd", &"g".repeat(RAW_TOKEN_CHARS)] {
            assert!(PasswordResetReference::parse(malformed).is_err());
        }
        let usable = LockedPasswordReset {
            id: 1,
            user_id: 7,
            expires_at: 101,
            consumed_at: None,
            invalidated_at: None,
            database_now: 100,
        };
        assert!(usable.is_usable());
        assert!(!LockedPasswordReset {
            expires_at: 100,
            ..usable
        }
        .is_usable());
    }
}
