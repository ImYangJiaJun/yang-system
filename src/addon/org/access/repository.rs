//! pre-tenant 查询的持久化边界。
//! authorization-writer: org-onboarding-authorization-facts

use super::service::TenantSummary;
use crate::addon::account::{increment_locked_authz_version, lock_user_authorization};
use crate::addon::org::organization::{ACTIVE_STATUS as ACTIVE_ORG_STATUS, STATUS as ORG_STATUS};
use crate::addon::org::pagination::{Page, PageRequest};
use crate::addon::org::user::{
    ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS, IS_ADMIN, NAME as MEMBER_NAME, ORG_ID,
    STATUS as MEMBERSHIP_STATUS, USER_ID,
};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder, SortOrder};

/// pre-tenant 租户发现共用的成员↔企业 JOIN 与 actor/状态收敛谓词。
fn tenant_discovery_query(pool: &sqlx::MySqlPool, user_id: i64) -> QueryBuilder<'_> {
    QueryBuilder::from_pool(pool, table!("org_user"))
        .join(
            table!("org_org"),
            field!("org_org.id"),
            field!("org_user.org_org"),
        )
        .where_and(field!("org_user.user_user"), CompareOp::Eq, user_id)
        .where_and(
            field!("org_user.status"),
            CompareOp::Eq,
            ACTIVE_MEMBERSHIP_STATUS,
        )
        .where_and(field!("org_org.status"), CompareOp::Eq, ACTIVE_ORG_STATUS)
}

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
            // tenant-boundary: database pre-tenant-table-database
            .bind(Arc::new(ctx.tools().mysql()?.pool().clone()))
            // tenant-boundary: unscoped-query pre-tenant-table-query
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
        // tenant-boundary: database tenant-discovery-database
        let pool = ctx.tools().mysql()?.pool();
        let rows = tenant_discovery_query(pool, user_id)
            .field(field!("org_org.id"))
            .field(field!("org_org.name"))
            .field(field!("org_org.code"))
            .order(field!("org_org.name"), SortOrder::Asc)
            .order(field!("org_org.id"), SortOrder::Asc)
            .limit(limit)
            .offset(offset)
            .select::<(i64, String, String)>()
            .await?;
        let total = tenant_discovery_query(pool, user_id).count().await?;
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
        // tenant-boundary: database org-onboarding-database
        let database = ctx.tools().mysql()?;
        // tenant-boundary: transaction tenant-onboarding-create
        let mut transaction = ctx.begin_transaction().await?;
        let result = async {
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
            let locked =
                lock_user_authorization(database.pool(), &mut transaction, user_id).await?;
            let membership = Record::new()
                .set(ORG_ID, org_id)
                .set(USER_ID, user_id)
                .set(MEMBER_NAME, username)
                .set(IS_ADMIN, true)
                .set(MEMBERSHIP_STATUS, ACTIVE_MEMBERSHIP_STATUS);
            self.query(ctx, &self.memberships)?
                .insert_in_tx(&mut transaction, membership)
                .await?;
            increment_locked_authz_version(&mut transaction, &locked).await?;
            let event = audit::succeeded_event(
                ctx,
                Some(org_id),
                Some(audit::entity("user", user_id)?),
                audit::entity("organization", org_id)?,
                None,
                Some(audit::summary([
                    ("owner_admin", json!(true)),
                    ("owner_status", json!(ACTIVE_MEMBERSHIP_STATUS)),
                    ("organization_status", json!(ACTIVE_ORG_STATUS)),
                    ("user_id", json!(user_id)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
            Ok(TenantSummary {
                id: org_id,
                name: name.to_string(),
                code: code.to_string(),
            })
        }
        .await;
        finish_transaction(transaction, result).await
    }
}

async fn finish_transaction<T>(
    transaction: yang_db::Transaction,
    result: Result<T, BaseError>,
) -> Result<T, BaseError> {
    match result {
        Ok(value) => {
            transaction.commit().await.map_err(BaseError::from)?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!("租户 onboarding 回滚失败: error={}", rollback_error);
            }
            Err(error)
        }
    }
}
