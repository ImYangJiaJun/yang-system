//! 用户聚合的唯一持久化边界。
//!
//! 对外 `TableQuery` 始终遵守请求用户的字段权限；只有本 Repository 能以
//! `system` 能力读写密码摘要，避免公开注册和登录被字段权限拦截，也避免把通用
//! 提权查询暴露给 Action。
//! authorization-writer: account-user-facts

use super::status::UserStatus;
use crate::addon::account::user::table::{
    AUTHZ_VERSION, CREDENTIAL_VERSION, EMAIL, EMAIL_VERIFIED_AT, PASSWORD_HASH, STATUS,
    SYSTEM_ROLE, TOTP_ACTIVATED_AT, TOTP_RECOVERY_DIGEST, TOTP_SECRET, USERNAME, USER_ID,
    USER_VIEW_FIELDS,
};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

const USER_CREDENTIAL_FIELDS: &[&str] = &[USER_ID, PASSWORD_HASH, STATUS];
const USER_AUTHORIZATION_FIELDS: &[&str] = &[USERNAME, STATUS, AUTHZ_VERSION, CREDENTIAL_VERSION];
/// TOTP 状态投影：**不**进登录热路径（`USER_CREDENTIAL_FIELDS`），只在
/// MFA 端点与 Step-up 重认证需要时单独拉取。`email` 供登录 MFA 备用
/// 邮箱验证码定位投递地址（只读，不回显给客户端）。
const USER_TOTP_FIELDS: &[&str] = &[
    USER_ID,
    USERNAME,
    STATUS,
    EMAIL,
    TOTP_SECRET,
    TOTP_ACTIVATED_AT,
    TOTP_RECOVERY_DIGEST,
];

/// TOTP 状态记录：`totp_secret` 为 AEAD 密文（Base64），`totp_activated_at`
/// 为空表示未激活；`totp_recovery_digest` 为恢复码摘要 JSON 数组（激活后非空）。
pub(crate) struct TotpStateRecord {
    pub(crate) username: String,
    pub(crate) status: UserStatus,
    /// 已验证邮箱（`chk_users_verified_email_pair` 保证与验证时间成对）；
    /// 匿名化账号为 None。
    pub(crate) email: Option<String>,
    pub(crate) totp_secret: Option<String>,
    pub(crate) totp_activated_at: Option<i64>,
    pub(crate) totp_recovery_digest: Option<String>,
}

impl TryFrom<&Record> for TotpStateRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            username: record.require(USERNAME)?,
            status: UserStatus::from_storage(&record.require::<String>(STATUS)?)?,
            email: record.optional(EMAIL)?,
            totp_secret: record.optional(TOTP_SECRET)?,
            totp_activated_at: record.optional(TOTP_ACTIVATED_AT)?,
            totp_recovery_digest: record.optional(TOTP_RECOVERY_DIGEST)?,
        })
    }
}

pub(crate) struct CredentialRecord {
    pub(crate) id: i64,
    pub(crate) password_hash: String,
    pub(crate) status: UserStatus,
}

impl TryFrom<&Record> for CredentialRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(USER_ID)?,
            password_hash: record.require(PASSWORD_HASH)?,
            status: UserStatus::from_storage(&record.require::<String>(STATUS)?)?,
        })
    }
}

pub(crate) struct AuthorizationStateRecord {
    pub(crate) username: String,
    pub(crate) status: UserStatus,
    pub(crate) authz_version: i64,
    pub(crate) credential_version: i64,
}

impl TryFrom<&Record> for AuthorizationStateRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            username: record.require(USERNAME)?,
            status: UserStatus::from_storage(&record.require::<String>(STATUS)?)?,
            authz_version: record.require(AUTHZ_VERSION)?,
            credential_version: record.require(CREDENTIAL_VERSION)?,
        })
    }
}

pub(crate) struct UserRepository {
    users: TableDefinition,
}

impl UserRepository {
    pub(crate) fn new(users: TableDefinition) -> Self {
        Self { users }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.users.bind(pool).query([SYSTEM_ROLE]))
    }

    pub(crate) async fn username_exists(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    pub(crate) async fn email_exists(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(EMAIL, serde_json::Value::String(email.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    /// 按已验证邮箱定位启用状态的用户 ID；停用用户与不存在一样返回 None，
    /// 供防枚举的自助找回流程使用（不外泄邮箱是否注册）。
    pub(crate) async fn find_active_user_by_email(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<Option<i64>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[USER_ID, STATUS])?
            .where_eq(EMAIL, serde_json::Value::String(email.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first()
            .map(|record| -> Result<Option<i64>, BaseError> {
                let status = UserStatus::from_storage(&record.require::<String>(STATUS)?)?;
                if status.is_active() {
                    Ok(Some(record.require(USER_ID)?))
                } else {
                    Ok(None)
                }
            })
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) async fn find_credentials_by_username(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    /// 按已验证邮箱定位凭据记录（邮箱登录使用，路线图 B-2）。
    ///
    /// 与用户名查找共享 `USER_CREDENTIAL_FIELDS` 投影与 `trusted_query` 信任边界；
    /// 查询以归一化后的邮箱精确匹配（users.email 唯一约束）。
    pub(crate) async fn find_credentials_by_email(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(EMAIL, serde_json::Value::String(email.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    pub(crate) async fn find_credentials_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    /// 分页列出用户（管理查询，路线图 D-3）。
    ///
    /// 使用 system 角色查询以读取 email 等受保护字段；调用方必须持有
    /// `account.users.read` 权限（Action 层校验）。按 id 升序分页。
    pub(crate) async fn list_page(
        &self,
        ctx: &ActionContext,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<Record>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_VIEW_FIELDS)?
            .page(page, page_size)?
            .all()
            .await?;
        Ok(rows)
    }

    pub(crate) async fn find_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<Record>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_VIEW_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }

    pub(crate) async fn find_authorization_state_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
    ) -> Result<Option<AuthorizationStateRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_AUTHORIZATION_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        rows.first()
            .map(AuthorizationStateRecord::try_from)
            .transpose()
    }

    pub(crate) async fn insert_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        username: &str,
        password_hash: &str,
        email: &str,
        email_verified_at: i64,
    ) -> Result<i64, BaseError> {
        let record = Record::new()
            .set(USERNAME, username)
            .set(PASSWORD_HASH, password_hash)
            .set(EMAIL, email)
            .set(EMAIL_VERIFIED_AT, email_verified_at)
            .set(STATUS, UserStatus::Active.as_str());
        let (_, id) = self
            .trusted_query(ctx)?
            .insert_returning_id_in_tx(transaction, record)
            .await?;
        i64::try_from(id).map_err(|_| BaseError::Unknown("用户主键超出 i64 范围".to_string()))
    }

    pub(crate) async fn update_password_hash_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        password_hash: &str,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(transaction, Record::new().set(PASSWORD_HASH, password_hash))
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} 密码摘要更新未精确影响一行"),
            )));
        }
        Ok(())
    }

    /// 在事务内改写已验证邮箱（邮箱换绑 Action 使用）。
    ///
    /// 邮箱与验证时间成对更新（保持 `chk_users_verified_email_pair` 检查约束），
    /// 唯一约束冲突由数据库约束错误返回。该方法是 users 事实的授权 writer 入口之一。
    /// 读取 TOTP 状态（MFA 端点与 Step-up 重认证使用；不污染登录热路径）。
    pub(crate) async fn find_totp_state_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<TotpStateRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_TOTP_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(TotpStateRecord::try_from).transpose()
    }

    /// 事务内写入 TOTP 激活状态：密文密钥、激活时间、恢复码摘要。
    ///
    /// 只在激活流程调用一次；停用走 `deactivate_totp_in_tx` 置空三列。
    pub(crate) async fn activate_totp_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        encrypted_secret: &str,
        activated_at: i64,
        recovery_digests_json: &str,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(
                transaction,
                Record::new()
                    .set(TOTP_SECRET, encrypted_secret)
                    .set(TOTP_ACTIVATED_AT, activated_at)
                    .set(TOTP_RECOVERY_DIGEST, recovery_digests_json),
            )
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} TOTP 激活未精确影响一行"),
            )));
        }
        Ok(())
    }

    /// 事务内清除 TOTP 激活状态：密钥密文、激活时间与恢复码摘要全部置 NULL。
    ///
    /// 只在停用流程调用；调用方必须在同一事务内递增双版本（凭据面变更）。
    pub(crate) async fn deactivate_totp_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(
                transaction,
                Record::new()
                    .set(TOTP_SECRET, serde_json::Value::Null)
                    .set(TOTP_ACTIVATED_AT, serde_json::Value::Null)
                    .set(TOTP_RECOVERY_DIGEST, serde_json::Value::Null),
            )
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} TOTP 停用未精确影响一行"),
            )));
        }
        Ok(())
    }

    /// 解密 TOTP 密钥（AEAD；密钥域来自 `security.totp.aead_key`）。
    pub(crate) async fn decrypt_totp_secret(
        &self,
        ctx: &ActionContext,
        state: &TotpStateRecord,
    ) -> Result<String, BaseError> {
        let stored = state
            .totp_secret
            .as_ref()
            .ok_or_else(|| BaseError::ConfigError("TOTP 已激活但缺少密钥密文".to_string()))?;
        let totp_settings = ctx.tools().config::<crate::config::TotpSettings>()?;
        let cipher = crate::addon::account::domain::mfa::TotpSecretCipher::new(totp_settings)?;
        let plaintext = cipher.decrypt(stored)?;
        String::from_utf8(plaintext)
            .map_err(|_| BaseError::ConfigError("TOTP 密钥密文解码失败".to_string()))
    }

    /// 事务内单次消费一次性恢复码：命中摘要则移除该项回写；
    /// 全部消费完时置 NULL。未命中返回 `Ok(false)`（不做任何修改）。
    pub(crate) async fn consume_recovery_code_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        code: &str,
    ) -> Result<bool, BaseError> {
        // FOR UPDATE 锁定用户行：并发消费同一恢复码时，后到事务阻塞至先到事务提交，
        // 随后读到已移除该摘要的版本并返回 false，修复无锁 SELECT + 无条件回写的
        // TOCTOU 双消费（此前两并发请求可同时读到同一摘要集并各自成功）。
        let pool = ctx.tools().mysql()?.pool().clone();
        let locked = transaction
            .select_for_update::<(i64,)>(
                QueryBuilder::from_pool(&pool, table!("users"))
                    .field(field!("id"))
                    .where_and(field!("id"), CompareOp::Eq, id),
            )
            .await
            .map_err(BaseError::from)?;
        if locked.is_empty() {
            return Ok(false);
        }
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_TOTP_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        let Some(record) = rows.first().map(TotpStateRecord::try_from).transpose()? else {
            return Ok(false);
        };
        let Some(digests_json) = record.totp_recovery_digest.as_deref() else {
            return Ok(false);
        };
        let Some(updated) =
            crate::addon::account::domain::mfa::remove_recovery_digest(digests_json, code)
        else {
            return Ok(false);
        };
        let update = if updated == "[]" {
            Record::new().set(TOTP_RECOVERY_DIGEST, serde_json::Value::Null)
        } else {
            Record::new().set(TOTP_RECOVERY_DIGEST, updated)
        };
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(transaction, update)
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} 恢复码消费未精确影响一行"),
            )));
        }
        Ok(true)
    }

    pub(crate) async fn update_email_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        email: &str,
        email_verified_at: i64,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(
                transaction,
                Record::new()
                    .set(EMAIL, email)
                    .set(EMAIL_VERIFIED_AT, email_verified_at),
            )
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} 邮箱更新未精确影响一行"),
            )));
        }
        Ok(())
    }

    /// 在事务内改写用户名（改用户名 Action 使用）。
    ///
    /// 用户名受唯一约束保护，冲突由数据库约束错误返回；调用方在持锁事务内
    /// 先更新用户名再递增双版本。该方法是 users 事实的授权 writer 入口之一，
    /// 只允许在 `account-security-version` 锁定的同一事务内调用。
    pub(crate) async fn update_username_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        username: &str,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(transaction, Record::new().set(USERNAME, username))
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} 用户名更新未精确影响一行"),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::account::user::table::user_table_spec;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::action::Request;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    #[tokio::test]
    async fn user_repository_owns_the_only_trusted_password_projection() {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool, DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        let definition = user_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let repository = UserRepository::new(definition.clone());
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), tools)
            .with_table_definition(definition);

        assert!(repository
            .trusted_query(&ctx)
            .and_then(|query| {
                query.select_fields(&[PASSWORD_HASH, AUTHZ_VERSION, CREDENTIAL_VERSION])
            })
            .is_ok());
        for field_name in [PASSWORD_HASH, AUTHZ_VERSION, CREDENTIAL_VERSION] {
            assert!(matches!(
                ctx.table_query()
                    .and_then(|query| query.select_fields(&[field_name])),
                Err(BaseError::FieldPermissionDenied(_, field, _)) if field == field_name
            ));
        }
    }
}
