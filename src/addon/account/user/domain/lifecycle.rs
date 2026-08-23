//! 跨账号、平台与企业关系的用户生命周期协调 writer。
//! authorization-writer: account-user-lifecycle

use crate::addon::account::{
    disable_locked_user_and_increment_versions, lock_user_credential, UserStatus,
};
use crate::audit;
use serde_json::json;
use std::collections::BTreeMap;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder, SortOrder, Transaction};

pub(super) async fn disable_self(ctx: &ActionContext, user_id: i64) -> Result<(), BaseError> {
    let database = ctx.tools().mysql()?;
    let pool = database.pool();
    let mut transaction = database.transaction().await?;
    let result =
        async {
            let active_platform_admins = transaction
                .select_for_update::<(i64, i64, Option<String>)>(
                    QueryBuilder::from_pool(pool, table!("admin_user"))
                        .field(field!("id"))
                        .field(field!("user_user"))
                        .field(field!("owner_key"))
                        .where_and(field!("status"), CompareOp::Eq, UserStatus::ACTIVE)
                        .where_and(field!("admin"), CompareOp::Eq, true)
                        .order(field!("id"), SortOrder::Asc),
                )
                .await?;
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

            // 原派生表 JOIN 改写为两步：先以非锁定一致读取出本人担任启用管理员的企业
            // 集合，再对这些企业的启用管理员行统一加 FOR UPDATE 锁。两步之间不加锁；
            // 资格复核发生在持锁读取的当前值上，并发资格变化由第二步的锁内复核吸收。
            let subject_orgs = transaction
                .select::<(i64,)>(
                    QueryBuilder::from_pool(pool, table!("org_user"))
                        .field(field!("org_org"))
                        .distinct()
                        .where_and(field!("user_user"), CompareOp::Eq, user_id)
                        .where_and(field!("status"), CompareOp::Eq, UserStatus::ACTIVE)
                        .where_and(field!("admin"), CompareOp::Eq, true),
                )
                .await?;
            let subject_org_ids = subject_orgs
                .into_iter()
                .map(|(org_id,)| org_id)
                .collect::<Vec<_>>();
            let active_org_admins: Vec<(i64, i64, i64)> = if subject_org_ids.is_empty() {
                Vec::new()
            } else {
                transaction
                    .select_for_update(
                        QueryBuilder::from_pool(pool, table!("org_user"))
                            .field(field!("id"))
                            .field(field!("org_org"))
                            .field(field!("user_user"))
                            .where_in(field!("org_org"), subject_org_ids)
                            .where_and(field!("status"), CompareOp::Eq, UserStatus::ACTIVE)
                            .where_and(field!("admin"), CompareOp::Eq, true)
                            .order(field!("org_org"), SortOrder::Asc)
                            .order(field!("id"), SortOrder::Asc),
                    )
                    .await?
            };
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
            let locked_user = lock_user_credential(pool, &mut transaction, user_id).await?;
            if !locked_user.status().is_active() {
                return Err(BaseError::PermissionDenied("账号已经停用".to_string()));
            }
            let platform_relations_disabled = transaction
                .table(table!("admin_user"))
                .where_and(field!("user_user"), CompareOp::Eq, user_id)
                .where_and(field!("status"), CompareOp::Eq, UserStatus::ACTIVE)
                .update(&json!({ "status": UserStatus::DISABLED }))
                .await?;
            let organization_relations_disabled = transaction
                .table(table!("org_user"))
                .where_and(field!("user_user"), CompareOp::Eq, user_id)
                .where_and(field!("status"), CompareOp::Eq, UserStatus::ACTIVE)
                .update(&json!({ "status": UserStatus::DISABLED }))
                .await?;
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
