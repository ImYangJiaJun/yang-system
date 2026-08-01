//! 跨账号、平台与企业关系的用户生命周期协调 writer。
//! raw-sql-boundary: domain-service account-user-lifecycle
//! authorization-writer: account-user-lifecycle

use crate::addon::account::{
    disable_locked_user_and_increment_versions, lock_user_credential, UserStatus,
};
use crate::audit;
use serde_json::json;
use std::collections::BTreeMap;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

pub(super) async fn disable_self(ctx: &ActionContext, user_id: i64) -> Result<(), BaseError> {
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result =
        async {
            let active_platform_admins = sqlx::query_as::<_, (i64, i64, Option<String>)>(
                "SELECT id, user_user, owner_key FROM admin_user \
             WHERE status = 'active' AND admin = TRUE ORDER BY id FOR UPDATE",
            )
            .fetch_all(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
            ensure_not_system_owner(active_platform_admins.iter().any(
                |(_, candidate, owner_key)| {
                    *candidate == user_id && owner_key.as_deref() == Some("system-owner")
                },
            ))?;
            if active_platform_admins
                .iter()
                .any(|(_, candidate, _)| *candidate == user_id)
                && active_platform_admins.len() < 2
            {
                return Err(BaseError::PermissionDenied(
                    "不能停用最后一个启用中的平台超级管理员".to_string(),
                ));
            }

            let active_org_admins = sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT candidate.id, candidate.org_org, candidate.user_user \
             FROM org_user AS candidate \
             INNER JOIN ( \
                 SELECT DISTINCT org_org FROM org_user \
                 WHERE user_user = ? AND status = 'active' AND admin = TRUE \
             ) AS subject_org ON subject_org.org_org = candidate.org_org \
             WHERE candidate.status = 'active' AND candidate.admin = TRUE \
             ORDER BY candidate.org_org, candidate.id FOR UPDATE",
            )
            .bind(user_id)
            .fetch_all(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
            let mut org_admin_counts = BTreeMap::<i64, usize>::new();
            for (_, org_id, _) in &active_org_admins {
                *org_admin_counts.entry(*org_id).or_default() += 1;
            }
            let blocked_orgs = active_org_admins
                .iter()
                .filter_map(|(_, org_id, candidate)| {
                    (*candidate == user_id && org_admin_counts.get(org_id) == Some(&1))
                        .then_some(*org_id)
                })
                .collect::<Vec<_>>();
            if !blocked_orgs.is_empty() {
                return Err(BaseError::PermissionDenied(format!(
                    "请先转移企业管理员后再停用账号，受影响企业数: {}",
                    blocked_orgs.len()
                )));
            }

            // 与平台/企业 writer 统一采用“管理员关系锁在前、用户锁在后”的顺序。
            let locked_user = lock_user_credential(&mut transaction, user_id).await?;
            if !locked_user.status().is_active() {
                return Err(BaseError::PermissionDenied("账号已经停用".to_string()));
            }
            let platform_relations_disabled = sqlx::query(
                "UPDATE admin_user SET status = 'disabled' \
             WHERE user_user = ? AND status = 'active'",
            )
            .bind(user_id)
            .execute(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?
            .rows_affected();
            let organization_relations_disabled = sqlx::query(
                "UPDATE org_user SET status = 'disabled' \
             WHERE user_user = ? AND status = 'active'",
            )
            .bind(user_id)
            .execute(executor(&mut transaction)?)
            .await
            .map_err(yang_db::DbError::from)?
            .rows_affected();
            disable_locked_user_and_increment_versions(&mut transaction, &locked_user).await?;
            let event = audit::succeeded_event(
                ctx,
                None,
                Some(audit::entity("user", user_id)?),
                audit::entity("user", user_id)?,
                Some(audit::summary([(
                    "status",
                    json!(UserStatus::Active.as_str()),
                )])?),
                Some(audit::summary([
                    (
                        "organization_relations_disabled",
                        json!(organization_relations_disabled),
                    ),
                    (
                        "platform_relations_disabled",
                        json!(platform_relations_disabled),
                    ),
                    ("status", json!(UserStatus::Disabled.as_str())),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
            Ok(())
        }
        .await;
    finish_transaction(transaction, result).await
}

fn ensure_not_system_owner(system_owner: bool) -> Result<(), BaseError> {
    if system_owner {
        return Err(BaseError::PermissionDenied(
            "系统最终管理员不能停用自己的账号".to_string(),
        ));
    }
    Ok(())
}

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "账号生命周期事务已结束".to_string(),
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
                tracing::error!(error = %rollback_error, "账号生命周期事务回滚失败");
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_owner_cannot_disable_self_even_when_other_admins_exist() {
        assert!(matches!(
            ensure_not_system_owner(true),
            Err(BaseError::PermissionDenied(message)) if message.contains("最终管理员")
        ));
        assert!(ensure_not_system_owner(false).is_ok());
    }
}
