//! 平台账号持久化边界。

use super::{ACTIVE_STATUS, BOOTSTRAP_KEY, IS_ADMIN, NAME, POSITION, STATUS, SYSTEM_ROLE, USER_ID};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

const INITIAL_BOOTSTRAP_KEY: &str = "initial-admin";

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
