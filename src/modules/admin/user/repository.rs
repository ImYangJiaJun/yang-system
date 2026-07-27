//! 平台账号持久化边界。
//! raw-sql-boundary: domain-repository admin-user-repository

use super::model::{AdminAccountPage, AdminAccountView, PageRequest};
use super::{ACTIVE_STATUS, BOOTSTRAP_KEY, IS_ADMIN, NAME, POSITION, STATUS, SYSTEM_ROLE, USER_ID};
use crate::audit;
use crate::modules::account::{
    increment_locked_authz_version, lock_user_authorization, LockedUserAuthorization,
};
use serde_json::json;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::Transaction;

const INITIAL_BOOTSTRAP_KEY: &str = "initial-admin";

type AdminRow = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    bool,
    i64,
    i64,
);

pub(super) struct AdminRepository {
    accounts: TableDefinition,
}

impl AdminRepository {
    pub(super) fn new(accounts: TableDefinition) -> Self {
        Self { accounts }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.accounts.bind(pool).query([SYSTEM_ROLE]))
    }

    pub(super) async fn list(
        &self,
        ctx: &ActionContext,
        request: PageRequest,
        search: Option<&str>,
    ) -> Result<AdminAccountPage, BaseError> {
        let pattern = search.map(|value| format!("%{value}%"));
        let pool = ctx.tools().mysql()?.pool();
        let rows = sqlx::query_as::<_, AdminRow>(
            "SELECT a.id, a.user_user, u.username, a.name, a.position, a.status, a.admin, \
                    a.created_at, a.updated_at \
             FROM admin_user AS a \
             INNER JOIN users AS u ON u.id = a.user_user \
             WHERE (? IS NULL OR a.name LIKE ? OR u.username LIKE ?) \
             ORDER BY a.created_at DESC, a.id DESC LIMIT ? OFFSET ?",
        )
        .bind(pattern.as_deref())
        .bind(pattern.as_deref())
        .bind(pattern.as_deref())
        .bind(request.sql_limit()?)
        .bind(request.offset)
        .fetch_all(pool)
        .await
        .map_err(yang_db::DbError::from)?;
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM admin_user AS a \
             INNER JOIN users AS u ON u.id = a.user_user \
             WHERE (? IS NULL OR a.name LIKE ? OR u.username LIKE ?)",
        )
        .bind(pattern.as_deref())
        .bind(pattern.as_deref())
        .bind(pattern.as_deref())
        .fetch_one(pool)
        .await
        .map_err(yang_db::DbError::from)?;
        let total = usize::try_from(total)
            .map_err(|_| BaseError::Unknown("平台账号总数超出 usize 范围".to_string()))?;
        Ok(AdminAccountPage::new(
            rows.into_iter().map(admin_view).collect(),
            total,
            request,
        ))
    }

    pub(super) async fn bootstrap(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        name: &str,
        position: Option<&str>,
    ) -> Result<i64, BaseError> {
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let initialized =
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM `admin_user` LIMIT 1)")
                    .fetch_one(executor(&mut transaction)?)
                    .await
                    .map_err(yang_db::DbError::from)?;
            if initialized {
                return Err(BaseError::PermissionDenied(
                    "平台账号已经完成初始化".to_string(),
                ));
            }

            let locked = lock_user_authorization(&mut transaction, user_id).await?;
            ensure_active_user(&locked)?;
            let mut account = Record::new()
                .set(USER_ID, user_id)
                .set(NAME, name)
                .set(STATUS, ACTIVE_STATUS)
                .set(IS_ADMIN, true)
                .set(BOOTSTRAP_KEY, INITIAL_BOOTSTRAP_KEY);
            if let Some(position) = position {
                account = account.set(POSITION, position);
            }
            let (_, id) = match self
                .trusted_query(ctx)?
                .insert_returning_id_in_tx(&mut transaction, account)
                .await
            {
                Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                    return Err(BaseError::PermissionDenied(
                        "平台账号已经完成初始化".to_string(),
                    ));
                }
                result => result?,
            };
            increment_locked_authz_version(&mut transaction, &locked).await?;
            let id = i64::try_from(id)
                .map_err(|_| BaseError::Unknown("平台账号主键超出 i64 范围".to_string()))?;
            append_admin_event(
                &mut transaction,
                ctx,
                id,
                user_id,
                None,
                Some(admin_summary(ACTIVE_STATUS, true, user_id)?),
            )
            .await?;
            Ok(id)
        }
        .await;
        finish_transaction(transaction, result).await
    }

    pub(super) async fn add(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        name: &str,
        position: Option<&str>,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let locked = lock_user_authorization(&mut transaction, user_id).await?;
            ensure_active_user(&locked)?;
            let mut account = Record::new()
                .set(USER_ID, user_id)
                .set(NAME, name)
                .set(STATUS, ACTIVE_STATUS)
                .set(IS_ADMIN, admin);
            if let Some(position) = position {
                account = account.set(POSITION, position);
            }
            let (_, id) = match self
                .trusted_query(ctx)?
                .insert_returning_id_in_tx(&mut transaction, account)
                .await
            {
                Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                    return Err(BaseError::ParamInvalid(
                        USER_ID.to_string(),
                        "该用户已经是平台账号".to_string(),
                    ));
                }
                result => result?,
            };
            increment_locked_authz_version(&mut transaction, &locked).await?;
            let id = i64::try_from(id)
                .map_err(|_| BaseError::Unknown("平台账号主键超出 i64 范围".to_string()))?;
            append_admin_event(
                &mut transaction,
                ctx,
                id,
                user_id,
                None,
                Some(admin_summary(ACTIVE_STATUS, admin, user_id)?),
            )
            .await?;
            Ok(id)
        }
        .await;
        let id = finish_transaction(transaction, result).await?;
        self.find_by_id(ctx, id).await
    }

    pub(super) async fn set_status(
        &self,
        ctx: &ActionContext,
        id: i64,
        status: &str,
    ) -> Result<AdminAccountView, BaseError> {
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let active_admins = if status != ACTIVE_STATUS {
                Some(lock_active_admins(&mut transaction).await?)
            } else {
                None
            };
            let target = lock_target(&mut transaction, id).await?;
            if status == target.status {
                return Ok(id);
            }
            if status != ACTIVE_STATUS && target.status == ACTIVE_STATUS && target.admin {
                ensure_not_last_active_admin(active_admins.as_deref().unwrap_or_default(), id)?;
            }
            let locked = lock_user_authorization(&mut transaction, target.user_id).await?;
            sqlx::query(
                "UPDATE admin_user SET status = ?, updated_at = UNIX_TIMESTAMP() WHERE id = ?",
            )
            .bind(status)
            .bind(id)
            .execute(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
            increment_locked_authz_version(&mut transaction, &locked).await?;
            append_admin_event(
                &mut transaction,
                ctx,
                id,
                target.user_id,
                Some(admin_summary(&target.status, target.admin, target.user_id)?),
                Some(admin_summary(status, target.admin, target.user_id)?),
            )
            .await?;
            Ok(id)
        }
        .await;
        finish_transaction(transaction, result).await?;
        self.find_by_id(ctx, id).await
    }

    pub(super) async fn set_admin(
        &self,
        ctx: &ActionContext,
        id: i64,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let active_admins = if admin {
                None
            } else {
                Some(lock_active_admins(&mut transaction).await?)
            };
            let target = lock_target(&mut transaction, id).await?;
            if admin == target.admin {
                return Ok(id);
            }
            if !admin && target.status == ACTIVE_STATUS && target.admin {
                ensure_not_last_active_admin(active_admins.as_deref().unwrap_or_default(), id)?;
            }
            let locked = lock_user_authorization(&mut transaction, target.user_id).await?;
            sqlx::query(
                "UPDATE admin_user SET admin = ?, updated_at = UNIX_TIMESTAMP() WHERE id = ?",
            )
            .bind(admin)
            .bind(id)
            .execute(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
            increment_locked_authz_version(&mut transaction, &locked).await?;
            append_admin_event(
                &mut transaction,
                ctx,
                id,
                target.user_id,
                Some(admin_summary(&target.status, target.admin, target.user_id)?),
                Some(admin_summary(&target.status, admin, target.user_id)?),
            )
            .await?;
            Ok(id)
        }
        .await;
        finish_transaction(transaction, result).await?;
        self.find_by_id(ctx, id).await
    }

    async fn find_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<AdminAccountView, BaseError> {
        let row = sqlx::query_as::<_, AdminRow>(
            "SELECT a.id, a.user_user, u.username, a.name, a.position, a.status, a.admin, \
                    a.created_at, a.updated_at \
             FROM admin_user AS a \
             INNER JOIN users AS u ON u.id = a.user_user WHERE a.id = ?",
        )
        .bind(id)
        .fetch_optional(ctx.tools().mysql()?.pool())
        .await
        .map_err(yang_db::DbError::from)?
        .ok_or_else(|| BaseError::RecordNotFound(format!("平台账号 {id}")))?;
        Ok(admin_view(row))
    }
}

fn admin_view(row: AdminRow) -> AdminAccountView {
    AdminAccountView {
        id: row.0,
        user_user: row.1,
        username: row.2,
        name: row.3,
        position: row.4,
        status: row.5,
        admin: row.6,
        created_at: row.7,
        updated_at: row.8,
    }
}

struct LockedAdminTarget {
    status: String,
    admin: bool,
    user_id: i64,
}

async fn lock_target(
    transaction: &mut Transaction,
    id: i64,
) -> Result<LockedAdminTarget, BaseError> {
    sqlx::query_as::<_, (String, bool, i64)>(
        "SELECT status, admin, user_user FROM admin_user WHERE id = ? FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?
    .map(|(status, admin, user_id)| LockedAdminTarget {
        status,
        admin,
        user_id,
    })
    .ok_or_else(|| BaseError::RecordNotFound(format!("平台账号 {id}")))
}

async fn lock_active_admins(transaction: &mut Transaction) -> Result<Vec<i64>, BaseError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM admin_user \
         WHERE status = 'active' AND admin = TRUE ORDER BY id FOR UPDATE",
    )
    .fetch_all(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)
    .map_err(Into::into)
}

fn ensure_not_last_active_admin(active_admins: &[i64], target_id: i64) -> Result<(), BaseError> {
    if active_admins.len() < 2 || !active_admins.contains(&target_id) {
        return Err(BaseError::PermissionDenied(
            "不能停用或降级最后一个启用中的超级管理员".to_string(),
        ));
    }
    Ok(())
}

fn ensure_active_user(locked: &LockedUserAuthorization) -> Result<(), BaseError> {
    if locked.status() != ACTIVE_STATUS {
        return Err(BaseError::UserNotFound(locked.user_id().to_string()));
    }
    Ok(())
}

fn admin_summary(
    status: &str,
    admin: bool,
    user_id: i64,
) -> Result<audit::AuditSummary, BaseError> {
    audit::summary([
        ("admin", json!(admin)),
        ("status", json!(status)),
        ("user_id", json!(user_id)),
    ])
}

async fn append_admin_event(
    transaction: &mut Transaction,
    ctx: &ActionContext,
    id: i64,
    user_id: i64,
    before: Option<audit::AuditSummary>,
    after: Option<audit::AuditSummary>,
) -> Result<(), BaseError> {
    let event = audit::succeeded_event(
        ctx,
        None,
        Some(audit::entity("user", user_id)?),
        audit::entity("admin_account", id)?,
        before,
        after,
    )?;
    audit::append_in_tx(transaction, &event).await
}

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "平台授权 writer 事务已结束".to_string(),
        ))
    })
}

async fn finish_transaction<T>(
    transaction: Transaction,
    result: Result<T, BaseError>,
) -> Result<T, BaseError> {
    match result {
        Ok(value) => {
            transaction.commit().await.map_err(BaseError::from)?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!("平台授权 writer 回滚失败: error={}", rollback_error);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_record_uses_stable_database_guard() {
        assert_eq!(INITIAL_BOOTSTRAP_KEY, "initial-admin");
        for field in [USER_ID, NAME, POSITION, STATUS, IS_ADMIN, BOOTSTRAP_KEY] {
            assert!(!field.is_empty());
        }
    }
}
