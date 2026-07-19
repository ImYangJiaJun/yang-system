//! pre-tenant 查询的持久化边界。

use super::service::{TenantPage, TenantSummary};
use crate::modules::org::organization::ACTIVE_STATUS as ACTIVE_ORG_STATUS;
use crate::modules::org::user::ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS;
use yang_base::action::ActionContext;
use yang_base::BaseError;

pub(super) struct TenantRepository;

impl TenantRepository {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) async fn list_for_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        page: usize,
        limit: usize,
    ) -> Result<TenantPage, BaseError> {
        let offset = page
            .checked_sub(1)
            .and_then(|value| value.checked_mul(limit))
            .ok_or_else(|| {
                BaseError::ParamInvalid("page".to_string(), "分页偏移量超出范围".to_string())
            })?;
        let limit = u64::try_from(limit).map_err(|_| {
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

        Ok(TenantPage {
            items: rows
                .into_iter()
                .map(|(id, name, code)| TenantSummary { id, name, code })
                .collect(),
            total,
            page,
            limit: usize::try_from(limit)
                .map_err(|_| BaseError::Unknown("分页大小超出 usize 范围".to_string()))?,
        })
    }
}
