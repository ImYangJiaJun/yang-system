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

pub(crate) struct PasswordResetReference {
    digest: String,
    fingerprint: String,
}

impl PasswordResetReference {
    pub(crate) fn parse(raw_token: &str) -> Result<Self, BaseError> {
        let bytes = decode_hex(raw_token).ok_or_else(invalid_reset_token)?;
        Self::from_bytes(&bytes)
    }

    pub(crate) fn from_bytes(bytes: &[u8; RAW_TOKEN_BYTES]) -> Result<Self, BaseError> {
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

    /// 入库用的 SHA-256 摘要（hex，64 字符）。
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

/// 一次新签发的自助密码重置凭证。
///
/// 明文只随本结构离开领域层（仅用于拼装一次性邮件链接），入库只存摘要与指纹。
pub(crate) struct IssuedPasswordReset {
    raw_token: String,
    reference: PasswordResetReference,
}

impl IssuedPasswordReset {
    /// 生成 32 字节密码学随机凭证，并派生入库用的摘要与指纹。
    pub(crate) fn generate() -> Result<Self, BaseError> {
        let mut bytes = [0_u8; RAW_TOKEN_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Ok(Self {
            raw_token: encode_hex(&bytes)?,
            reference: PasswordResetReference::from_bytes(&bytes)?,
        })
    }

    /// 明文凭证（hex，64 字符）；禁止入库或写日志。
    pub(crate) fn raw_token(&self) -> &str {
        &self.raw_token
    }

    /// 摘要前缀指纹，用于日志与审计关联（不泄露明文）。
    pub(crate) fn fingerprint(&self) -> &str {
        self.reference.fingerprint()
    }

    /// 入库用的摘要引用。
    fn reference(&self) -> &PasswordResetReference {
        &self.reference
    }
}

/// 以数据库时钟写入一条自助签发的密码重置凭证（普通 INSERT，不持锁）。
///
/// 同一用户重复请求时直接插入新凭证；旧凭证由消费端成功消费时一并作废，
/// 见 [`consume_in_tx`]。`requested_by_user` 恒为 NULL：自助场景没有请求者。
pub(crate) async fn insert_issued(
    pool: &MySqlPool,
    user_id: i64,
    issued: &IssuedPasswordReset,
    ttl_seconds: u64,
) -> Result<(), BaseError> {
    insert_issued_by(pool, user_id, issued, ttl_seconds, None).await
}

/// 以数据库时钟写入一条管理签发的密码重置凭证（路线图 D-2）。
///
/// `requested_by_user` 写入操作者 ID（审计管理动作），其余与自助签发一致。
pub(crate) async fn insert_issued_by(
    pool: &MySqlPool,
    user_id: i64,
    issued: &IssuedPasswordReset,
    ttl_seconds: u64,
    requested_by_user: Option<i64>,
) -> Result<(), BaseError> {
    let ttl = i64::try_from(ttl_seconds)
        .map_err(|_| BaseError::ConfigError("密码重置凭证 TTL 超出 i64 范围".to_string()))?;
    QueryBuilder::from_pool(pool, table!("password_reset_token"))
        .set_expr(field!("created_at"), SqlExpr::unix_timestamp())
        .set_expr(field!("expires_at"), SqlExpr::unix_timestamp_add(ttl))
        .insert(&insert_data(user_id, issued.reference(), requested_by_user))
        .await?;
    Ok(())
}

/// 在调用方事务内签发一条管理重置凭证（与审计同事务原子提交）。
///
/// 与 [`insert_issued_by`] 相同的列值，但走事务连接，供管理签发 Action 把
/// 凭证插入与成功审计事件绑定在同一事务（避免崩溃丢审计）。
pub(crate) async fn insert_issued_by_in_tx(
    transaction: &mut Transaction,
    user_id: i64,
    issued: &IssuedPasswordReset,
    ttl_seconds: u64,
    requested_by_user: Option<i64>,
) -> Result<(), BaseError> {
    let ttl = i64::try_from(ttl_seconds)
        .map_err(|_| BaseError::ConfigError("密码重置凭证 TTL 超出 i64 范围".to_string()))?;
    transaction
        .table(table!("password_reset_token"))
        .set_expr(field!("created_at"), SqlExpr::unix_timestamp())
        .set_expr(field!("expires_at"), SqlExpr::unix_timestamp_add(ttl))
        .insert(&insert_data(user_id, issued.reference(), requested_by_user))
        .await?;
    Ok(())
}

/// 组装签发 INSERT 的列值；时间两列由 `set_expr` 以数据库时钟写入。
fn insert_data(
    user_id: i64,
    reference: &PasswordResetReference,
    requested_by_user: Option<i64>,
) -> serde_json::Value {
    serde_json::json!({
        "token_digest": reference.digest(),
        "token_fingerprint": reference.fingerprint(),
        "user_user": user_id,
        "requested_by_user": requested_by_user.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
    })
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

pub(crate) async fn find_target_user(
    pool: &MySqlPool,
    reference: &PasswordResetReference,
) -> Result<Option<i64>, BaseError> {
    QueryBuilder::from_pool(pool, table!("password_reset_token"))
        .where_and(
            field!("token_digest"),
            CompareOp::Eq,
            reference.digest.as_str(),
        )?
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
                )?,
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
        .where_and(field!("id"), CompareOp::Eq, locked.id)?
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
        .where_and(field!("user_user"), CompareOp::Eq, locked.user_id)?
        .where_and(field!("id"), CompareOp::Ne, locked.id)?
        .where_null(field!("consumed_at"))
        .where_null(field!("invalidated_at"))
        .update(&serde_json::json!({}))
        .await?;
    Ok(())
}

/// 在事务内作废某用户的全部未消费重置凭证（匿名化删除前置清理，路线图 E-2a）。
///
/// 幂等：已消费/已作废的凭证不受影响；该用户的待用凭证立即不可用，
/// 避免删除/匿名化后残留可消费的重置入口。
pub(crate) async fn invalidate_all_for_user_in_tx(
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<(), BaseError> {
    transaction
        .table(table!("password_reset_token"))
        .set_expr(field!("invalidated_at"), SqlExpr::unix_timestamp())
        .where_and(field!("user_user"), CompareOp::Eq, user_id)?
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

    #[test]
    fn issued_token_is_64_hex_chars_and_digest_matches_plaintext() {
        let issued = IssuedPasswordReset::generate()
            .unwrap_or_else(|error| panic!("签发凭证应成功: {error}"));

        assert_eq!(issued.raw_token().len(), RAW_TOKEN_CHARS);
        assert!(issued
            .raw_token()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));

        // 入库摘要必须是对明文凭证字节的 SHA-256，且消费端 parse 同一明文得到同一摘要。
        let parsed = PasswordResetReference::parse(issued.raw_token())
            .unwrap_or_else(|error| panic!("自签发的明文凭证应可解析: {error}"));
        assert_eq!(parsed.digest(), issued.reference().digest());
        assert_eq!(parsed.fingerprint(), issued.reference().fingerprint());

        // 固定向量锚定摘要算法：SHA-256(32 个零字节)。
        let zero = PasswordResetReference::from_bytes(&[0_u8; RAW_TOKEN_BYTES])
            .unwrap_or_else(|error| panic!("固定输入应可派生摘要: {error}"));
        assert_eq!(
            zero.digest(),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
        assert!(zero.digest().starts_with(zero.fingerprint()));
    }

    #[test]
    fn insert_data_stores_only_digest_and_marks_self_service_requester_as_null() {
        let reference = PasswordResetReference::from_bytes(&[7_u8; RAW_TOKEN_BYTES])
            .unwrap_or_else(|error| panic!("固定输入应可派生摘要: {error}"));
        let data = insert_data(42, &reference, None);

        assert_eq!(data["user_user"], serde_json::json!(42));
        assert!(data["requested_by_user"].is_null());
        let digest = data["token_digest"]
            .as_str()
            .unwrap_or_else(|| panic!("token_digest 必须是字符串"));
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, reference.digest());
        let fingerprint = data["token_fingerprint"]
            .as_str()
            .unwrap_or_else(|| panic!("token_fingerprint 必须是字符串"));
        assert_eq!(fingerprint.len(), FINGERPRINT_CHARS);
        assert!(digest.starts_with(fingerprint));
        assert!(!data.to_string().contains("07070707"));
    }
}
