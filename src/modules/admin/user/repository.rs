//! 平台账号持久化边界。

use super::model::{AdminAccountPage, AdminAccountView, PageRequest};
use super::{ACTIVE_STATUS, BOOTSTRAP_KEY, IS_ADMIN, NAME, POSITION, STATUS, SYSTEM_ROLE, USER_ID};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

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
        let initialized =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM `admin_user` LIMIT 1)")
                .fetch_one(ctx.tools().mysql()?.pool())
                .await
                .map_err(yang_db::DbError::from)?;
        if initialized {
            return Err(BaseError::PermissionDenied(
                "平台账号已经完成初始化".to_string(),
            ));
        }

        let mut account = Record::new()
            .set(USER_ID, user_id)
            .set(NAME, name)
            .set(STATUS, ACTIVE_STATUS)
            .set(IS_ADMIN, true)
            .set(BOOTSTRAP_KEY, INITIAL_BOOTSTRAP_KEY);
        if let Some(position) = position {
            account = account.set(POSITION, position);
        }
        let (_, id) = match self.trusted_query(ctx)?.insert_returning_id(account).await {
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::PermissionDenied(
                    "平台账号已经完成初始化".to_string(),
                ));
            }
            result => result?,
        };
        i64::try_from(id).map_err(|_| BaseError::Unknown("平台账号主键超出 i64 范围".to_string()))
    }

    pub(super) async fn add(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        name: &str,
        position: Option<&str>,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        let user_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND status = 'active')",
        )
        .bind(user_id)
        .fetch_one(ctx.tools().mysql()?.pool())
        .await
        .map_err(yang_db::DbError::from)?;
        if !user_exists {
            return Err(BaseError::UserNotFound(user_id.to_string()));
        }

        let mut account = Record::new()
            .set(USER_ID, user_id)
            .set(NAME, name)
            .set(STATUS, ACTIVE_STATUS)
            .set(IS_ADMIN, admin);
        if let Some(position) = position {
            account = account.set(POSITION, position);
        }
        let (_, id) = match self.trusted_query(ctx)?.insert_returning_id(account).await {
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::ParamInvalid(
                    USER_ID.to_string(),
                    "该用户已经是平台账号".to_string(),
                ));
            }
            result => result?,
        };
        let id = i64::try_from(id)
            .map_err(|_| BaseError::Unknown("平台账号主键超出 i64 范围".to_string()))?;
        self.find_by_id(ctx, id).await
    }

    pub(super) async fn set_status(
        &self,
        ctx: &ActionContext,
        id: i64,
        status: &str,
    ) -> Result<AdminAccountView, BaseError> {
        let pool = ctx.tools().mysql()?.pool();
        let mut transaction = pool.begin().await.map_err(yang_db::DbError::from)?;
        let target = lock_target(&mut transaction, id).await?;
        if status != ACTIVE_STATUS && target.0 == ACTIVE_STATUS && target.1 {
            ensure_other_active_admin(&mut transaction, id).await?;
        }
        sqlx::query("UPDATE admin_user SET status = ?, updated_at = UNIX_TIMESTAMP() WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(yang_db::DbError::from)?;
        transaction.commit().await.map_err(yang_db::DbError::from)?;
        self.find_by_id(ctx, id).await
    }

    pub(super) async fn set_admin(
        &self,
        ctx: &ActionContext,
        id: i64,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        let pool = ctx.tools().mysql()?.pool();
        let mut transaction = pool.begin().await.map_err(yang_db::DbError::from)?;
        let target = lock_target(&mut transaction, id).await?;
        if !admin && target.0 == ACTIVE_STATUS && target.1 {
            ensure_other_active_admin(&mut transaction, id).await?;
        }
        sqlx::query("UPDATE admin_user SET admin = ?, updated_at = UNIX_TIMESTAMP() WHERE id = ?")
            .bind(admin)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(yang_db::DbError::from)?;
        transaction.commit().await.map_err(yang_db::DbError::from)?;
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

async fn lock_target(
    transaction: &mut sqlx::Transaction<'_, sqlx::MySql>,
    id: i64,
) -> Result<(String, bool), BaseError> {
    sqlx::query_as::<_, (String, bool)>(
        "SELECT status, admin FROM admin_user WHERE id = ? FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(yang_db::DbError::from)?
    .ok_or_else(|| BaseError::RecordNotFound(format!("平台账号 {id}")))
}

async fn ensure_other_active_admin(
    transaction: &mut sqlx::Transaction<'_, sqlx::MySql>,
    target_id: i64,
) -> Result<(), BaseError> {
    let others = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM admin_user \
         WHERE id <> ? AND status = 'active' AND admin = TRUE FOR UPDATE",
    )
    .bind(target_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(yang_db::DbError::from)?;
    if others.is_empty() {
        return Err(BaseError::PermissionDenied(
            "不能停用或降级最后一个启用中的超级管理员".to_string(),
        ));
    }
    Ok(())
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
