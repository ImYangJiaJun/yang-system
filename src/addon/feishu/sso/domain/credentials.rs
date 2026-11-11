//! 每用户飞书凭证的存取与刷新。
//!
//! 与管理员单例 `feishu_user_token` 分开：本模块的锁粒度为 `user_id`，刷新令牌
//! 一次性，必须在同一用户的并发请求之间串行。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use yang_base::action::ActionContext;
use yang_base::BaseError;

use crate::addon::account::ExternalIdentityView;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::tenant_token::FeishuCredentials;
use crate::addon::feishu::domain::user_token::{self, TokenBundle};
use crate::config::FeishuSettings;

use crate::addon::feishu::domain::user_credential_table::{
    CREDENTIAL_ACCESS_EXPIRES_AT, CREDENTIAL_ACCESS_TOKEN, CREDENTIAL_ACTIVE,
    CREDENTIAL_AUTHORIZED_AT, CREDENTIAL_ID, CREDENTIAL_IDENTITY_ID, CREDENTIAL_LAST_REFRESHED_AT,
    CREDENTIAL_OPEN_ID, CREDENTIAL_REAUTH_REQUIRED, CREDENTIAL_REFRESH_EXPIRES_AT,
    CREDENTIAL_REFRESH_TOKEN, CREDENTIAL_SCOPE, CREDENTIAL_STATUS, CREDENTIAL_USER_ID,
};

const ACCESS_REFRESH_MARGIN_SECONDS: i64 = 300;

static USER_LOCKS: LazyLock<Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
pub(crate) struct CredentialRow {
    pub(crate) id: i64,
    pub(crate) identity_id: i64,
    pub(crate) open_id: String,
    pub(crate) access_token: String,
    pub(crate) access_token_expires_at: i64,
    pub(crate) refresh_token: String,
    pub(crate) status: String,
}

impl TryFrom<&yang_base::table::Record> for CredentialRow {
    type Error = BaseError;

    fn try_from(record: &yang_base::table::Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(CREDENTIAL_ID)?,
            identity_id: record.require(CREDENTIAL_IDENTITY_ID)?,
            open_id: record.require(CREDENTIAL_OPEN_ID)?,
            access_token: record.require(CREDENTIAL_ACCESS_TOKEN)?,
            access_token_expires_at: record.require(CREDENTIAL_ACCESS_EXPIRES_AT)?,
            refresh_token: record.require(CREDENTIAL_REFRESH_TOKEN)?,
            status: record.require(CREDENTIAL_STATUS)?,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CredentialError {
    NotLinked,
    ReauthRequired { message: String },
    Failed { message: String },
}

impl CredentialError {
    pub(crate) fn user_message(&self) -> String {
        match self {
            Self::NotLinked => "尚未绑定飞书身份，请先在账号设置绑定飞书".to_string(),
            Self::ReauthRequired { message } => {
                format!("飞书授权已失效，请重新授权：{message}")
            }
            Self::Failed { message } => format!("飞书凭证不可用：{message}"),
        }
    }
}

pub(crate) async fn upsert_credential(
    _ctx: &ActionContext,
    context: &FeishuContext,
    identity: &ExternalIdentityView,
    bundle: &TokenBundle,
    now: i64,
) -> Result<(), BaseError> {
    let query = context.user_credentials().query();
    let existing = query
        .clone()
        .select_fields(&[CREDENTIAL_ID])?
        .where_eq(CREDENTIAL_IDENTITY_ID, Value::Number(identity.id.into()))?
        .page(1, 1)?
        .all()
        .await?;
    let record = yang_base::table::Record::new()
        .set(CREDENTIAL_IDENTITY_ID, identity.id)
        .set(CREDENTIAL_USER_ID, identity.user_id)
        .set(CREDENTIAL_OPEN_ID, identity.external_id.as_str())
        .set(CREDENTIAL_ACCESS_TOKEN, bundle.access_token.as_str())
        .set(CREDENTIAL_ACCESS_EXPIRES_AT, bundle.access_token_expires_at)
        .set(CREDENTIAL_REFRESH_TOKEN, bundle.refresh_token.as_str())
        .set(
            CREDENTIAL_REFRESH_EXPIRES_AT,
            bundle.refresh_token_expires_at,
        )
        .set(
            CREDENTIAL_SCOPE,
            bundle
                .scope
                .as_deref()
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null),
        )
        .set(CREDENTIAL_AUTHORIZED_AT, now)
        .set(CREDENTIAL_STATUS, CREDENTIAL_ACTIVE)
        .set(CREDENTIAL_LAST_REFRESHED_AT, now);
    if let Some(row) = existing.first() {
        let id: i64 = row.require(CREDENTIAL_ID)?;
        let affected = query
            .where_eq(CREDENTIAL_ID, Value::Number(id.into()))?
            .update(record)
            .await?;
        if affected != 1 {
            return Err(BaseError::Unknown(format!(
                "飞书凭证 {id} 更新未精确影响一行"
            )));
        }
        return Ok(());
    }
    match query.insert(record.clone()).await {
        Ok(_) => Ok(()),
        Err(error) if crate::db::is_constraint_violation(&error) => {
            // 并发授权：另一个请求已插入，按 identity_id 覆盖。
            let rows = context
                .user_credentials()
                .query()
                .select_fields(&[CREDENTIAL_ID])?
                .where_eq(CREDENTIAL_IDENTITY_ID, Value::Number(identity.id.into()))?
                .page(1, 1)?
                .all()
                .await?;
            let Some(row) = rows.first() else {
                return Err(error);
            };
            let id: i64 = row.require(CREDENTIAL_ID)?;
            let affected = context
                .user_credentials()
                .query()
                .where_eq(CREDENTIAL_ID, Value::Number(id.into()))?
                .update(record)
                .await?;
            if affected != 1 {
                return Err(BaseError::Unknown(format!(
                    "飞书凭证 {id} 并发覆盖未精确影响一行"
                )));
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn delete_credential(
    _ctx: &ActionContext,
    context: &FeishuContext,
    identity_id: i64,
) -> Result<u64, BaseError> {
    let affected = context
        .user_credentials()
        .query()
        .where_eq(CREDENTIAL_IDENTITY_ID, Value::Number(identity_id.into()))?
        .delete()
        .await?;
    Ok(affected)
}

pub(crate) async fn load_credential(
    _ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
) -> Result<Option<CredentialRow>, BaseError> {
    let rows = context
        .user_credentials()
        .query()
        .select_fields(&[
            CREDENTIAL_ID,
            CREDENTIAL_IDENTITY_ID,
            CREDENTIAL_OPEN_ID,
            CREDENTIAL_ACCESS_TOKEN,
            CREDENTIAL_ACCESS_EXPIRES_AT,
            CREDENTIAL_REFRESH_TOKEN,
            CREDENTIAL_STATUS,
        ])?
        .where_eq(CREDENTIAL_USER_ID, Value::Number(user_id.into()))?
        .page(1, 1)?
        .all()
        .await?;
    rows.first().map(CredentialRow::try_from).transpose()
}

fn user_lock(user_id: i64) -> Arc<AsyncMutex<()>> {
    let mut locks = USER_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        locks
            .entry(user_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
}

/// 取一个可用的用户 access token；快过期就先刷新。
///
/// 刷新按 `user_id` 串行，锁后复查，避免两个并发请求撞 `20073` 并把新凭证写丢。
pub(crate) async fn access_token_for_user(
    ctx: &ActionContext,
    context: &FeishuContext,
    settings: &FeishuSettings,
    user_id: i64,
) -> Result<String, CredentialError> {
    let lock = user_lock(user_id);
    let _guard = lock.lock().await;
    let now = user_token::now_unix_secs();
    let Some(row) = load_credential(ctx, context, user_id)
        .await
        .map_err(|error| CredentialError::Failed {
            message: format!("读取飞书凭证失败：{error}"),
        })?
    else {
        return Err(CredentialError::NotLinked);
    };
    if row.status != CREDENTIAL_ACTIVE {
        return Err(CredentialError::ReauthRequired {
            message: "凭证已被标记为需要重新授权".to_string(),
        });
    }
    if row.access_token_expires_at - ACCESS_REFRESH_MARGIN_SECONDS > now {
        return Ok(row.access_token);
    }
    let outbound =
        outbound_setup::build(ctx, settings).map_err(|error| CredentialError::Failed {
            message: format!("构造飞书出站传输失败：{error}"),
        })?;
    let credentials = FeishuCredentials {
        app_id: settings.app_id.clone().unwrap_or_default(),
        app_secret: settings.app_secret.clone().unwrap_or_default(),
    };
    let bundle = user_token::refresh_tokens(
        outbound.transport(),
        outbound.sleeper(),
        &credentials,
        &row.refresh_token,
        now,
    )
    .await
    .map_err(|error| {
        if error.needs_reauth() {
            CredentialError::ReauthRequired {
                message: error.hint().unwrap_or(error.message.as_str()).to_string(),
            }
        } else {
            CredentialError::Failed {
                message: format!("{}（错误码 {}）", error.message, error.code),
            }
        }
    })?;
    let affected = context
        .user_credentials()
        .query()
        .where_eq(CREDENTIAL_ID, Value::Number(row.id.into()))
        .map_err(|error| CredentialError::Failed {
            message: format!("刷新飞书凭证定位失败：{error}"),
        })?
        .update(
            yang_base::table::Record::new()
                .set(CREDENTIAL_ACCESS_TOKEN, bundle.access_token.as_str())
                .set(CREDENTIAL_ACCESS_EXPIRES_AT, bundle.access_token_expires_at)
                .set(CREDENTIAL_REFRESH_TOKEN, bundle.refresh_token.as_str())
                .set(
                    CREDENTIAL_REFRESH_EXPIRES_AT,
                    bundle.refresh_token_expires_at,
                )
                .set(
                    CREDENTIAL_SCOPE,
                    bundle
                        .scope
                        .as_deref()
                        .map(|value| Value::String(value.to_string()))
                        .unwrap_or(Value::Null),
                )
                .set(CREDENTIAL_LAST_REFRESHED_AT, now)
                .set(CREDENTIAL_STATUS, CREDENTIAL_ACTIVE),
        )
        .await
        .map_err(|error| CredentialError::Failed {
            message: format!("刷新飞书凭证写库失败：{error}"),
        })?;
    if affected != 1 {
        return Err(CredentialError::Failed {
            message: format!("飞书凭证 {} 刷新写库未精确影响一行", row.id),
        });
    }
    Ok(bundle.access_token)
}

pub(crate) async fn mark_reauth_required(
    _ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
) -> Result<(), BaseError> {
    context
        .user_credentials()
        .query()
        .where_eq(CREDENTIAL_USER_ID, Value::Number(user_id.into()))?
        .update(yang_base::table::Record::new().set(CREDENTIAL_STATUS, CREDENTIAL_REAUTH_REQUIRED))
        .await?;
    Ok(())
}
