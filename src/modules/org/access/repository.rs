//! pre-tenant 查询的持久化边界。

use super::service::TenantSummary;
use crate::modules::org::organization::{ACTIVE_STATUS as ACTIVE_ORG_STATUS, STATUS as ORG_STATUS};
use crate::modules::org::pagination::{Page, PageRequest};
use crate::modules::org::user::{
    ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS, IS_ADMIN, NAME as MEMBER_NAME, ORG_ID,
    STATUS as MEMBERSHIP_STATUS, USER_ID,
};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

pub(super) struct TenantRepository {
    organizations: TableDefinition,
    memberships: TableDefinition,
}

impl TenantRepository {
    pub(super) fn new(organizations: TableDefinition, memberships: TableDefinition) -> Self {
        Self {
            organizations,
            memberships,
        }
    }

    fn query(&self, ctx: &ActionContext, table: &TableDefinition) -> Result<TableQuery, BaseError> {
        Ok(table
            .bind(Arc::new(ctx.tools().mysql()?.pool().clone()))
            .query(std::iter::empty::<&str>()))
    }

    pub(super) async fn list_for_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        request: PageRequest,
    ) -> Result<Page<TenantSummary>, BaseError> {
        let offset = request
            .page
            .checked_sub(1)
            .and_then(|value| value.checked_mul(request.limit))
            .ok_or_else(|| {
                BaseError::ParamInvalid("page".to_string(), "分页偏移量超出范围".to_string())
            })?;
        let limit = u64::try_from(request.limit).map_err(|_| {
            BaseError::ParamInvalid("limit".to_string(), "参数超出范围".to_string())
        })?;
        let offset = u64::try_from(offset)
            .map_err(|_| BaseError::ParamInvalid("page".to_string(), "参数超出范围".to_string()))?;
        let pool = ctx.tools().mysql()?.pool();
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT o.id, o.name, o.code \
             FROM org_user AS m \
             INNER JOIN org_org AS o ON o.id = m.org_org \
             WHERE m.user_user = ? AND m.status = ? AND o.status = ? \
             ORDER BY o.name ASC, o.id ASC LIMIT ? OFFSET ?",
        )
        .bind(user_id)
        .bind(ACTIVE_MEMBERSHIP_STATUS)
        .bind(ACTIVE_ORG_STATUS)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(yang_db::DbError::from)?;
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) \
             FROM org_user AS m \
             INNER JOIN org_org AS o ON o.id = m.org_org \
             WHERE m.user_user = ? AND m.status = ? AND o.status = ?",
        )
        .bind(user_id)
        .bind(ACTIVE_MEMBERSHIP_STATUS)
        .bind(ACTIVE_ORG_STATUS)
        .fetch_one(pool)
        .await
        .map_err(yang_db::DbError::from)?;
        let total = usize::try_from(total)
            .map_err(|_| BaseError::Unknown("租户总数超出 usize 范围".to_string()))?;

        Ok(Page::new(
            rows.into_iter()
                .map(|(id, name, code)| TenantSummary { id, name, code })
                .collect(),
            total,
            request,
        ))
    }

    pub(super) async fn create_for_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        username: &str,
        name: &str,
        code: &str,
    ) -> Result<TenantSummary, BaseError> {
        let mut transaction = ctx.begin_transaction().await?;
        let organization = Record::new()
            .set("name", name)
            .set("code", code)
            .set(ORG_STATUS, ACTIVE_ORG_STATUS);
        let (_, org_id) = self
            .query(ctx, &self.organizations)?
            .insert_returning_id_in_tx(&mut transaction, organization)
            .await?;
        let org_id = i64::try_from(org_id)
            .map_err(|_| BaseError::Unknown("企业主键超出 i64 范围".to_string()))?;
        let membership = Record::new()
            .set(ORG_ID, org_id)
            .set(USER_ID, user_id)
            .set(MEMBER_NAME, username)
            .set(IS_ADMIN, true)
            .set(MEMBERSHIP_STATUS, ACTIVE_MEMBERSHIP_STATUS);
        self.query(ctx, &self.memberships)?
            .insert_in_tx(&mut transaction, membership)
            .await?;
        transaction
            .commit()
            .await
            .map_err(BaseError::DatabaseTransactionFailed)?;

        Ok(TenantSummary {
            id: org_id,
            name: name.to_string(),
            code: code.to_string(),
        })
    }
}
