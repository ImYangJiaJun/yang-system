//! 企业成员授权事实的显式事务 writer。
//! raw-sql-boundary: domain-repository org-member-repository

use super::{IS_ADMIN, ORG_ID, STATUS, USER_ID};
use crate::audit;
use crate::authorization::{resource_authorization_checkpoint, ResourceAuthorizationCheckpoint};
use crate::modules::account::{increment_locked_authz_versions, lock_user_authorizations};
use crate::modules::org::organization::ACTIVE_STATUS as ACTIVE_ORG_STATUS;
use serde_json::{json, Value};
use yang_base::action::builtin::{AffectedResult, GetByPk, InsertResult, PutInput};
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockedMembership {
    org_id: i64,
    user_id: i64,
    status: String,
    admin: bool,
}

struct MembershipAuthorizationChange {
    next_org_id: i64,
    next_user_id: i64,
    next_status: String,
    next_admin: bool,
    changed: bool,
}

pub(super) async fn add(ctx: &ActionContext, input: Record) -> Result<InsertResult, BaseError> {
    let org_id = add_org_id(ctx, &input)?;
    let user_id = input.require::<i64>(USER_ID)?;
    let status = proposed_string(&input, STATUS, super::ACTIVE_STATUS)?;
    let admin = proposed_bool(&input, IS_ADMIN, false)?;
    // tenant-boundary: database org-member-add-database
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        lock_active_org_admin(ctx, &mut transaction, org_id).await?;
        lock_active_organization(&mut transaction, org_id).await?;
        let locked = lock_user_authorizations(&mut transaction, [user_id]).await?;
        let (affected, id) = ctx
            .table_query()?
            .insert_returning_id_in_tx(&mut transaction, input)
            .await?;
        if affected == 1 {
            increment_locked_authz_versions(&mut transaction, &locked).await?;
            append_membership_event(
                &mut transaction,
                ctx,
                id,
                org_id,
                user_id,
                None,
                Some(membership_summary(org_id, user_id, &status, admin, None)?),
            )
            .await?;
        }
        Ok(InsertResult { affected, id })
    }
    .await;
    finish_transaction(transaction, result).await
}

pub(super) async fn put(ctx: &ActionContext, input: PutInput) -> Result<AffectedResult, BaseError> {
    if input.data.as_map().is_empty() {
        return Err(BaseError::ParamInvalid(
            "data".to_string(),
            "至少需要一个字段".to_string(),
        ));
    }
    let id = primary_key(&input.id)?;
    let org_id = membership_org_id(ctx, id).await?;
    let changed_fields = input
        .data
        .as_map()
        .keys()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    // tenant-boundary: database org-member-put-database
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        lock_active_org_admin(ctx, &mut transaction, org_id).await?;
        lock_active_organization(&mut transaction, org_id).await?;
        let Some(membership) = lock_membership(ctx, &mut transaction, id).await? else {
            return Ok(AffectedResult { affected: 0 });
        };
        let change = authorization_change(&membership, &input.data)?;
        if change.next_org_id != org_id {
            return Err(BaseError::PermissionDenied(
                "企业成员归属不可跨资源授权边界修改".to_string(),
            ));
        }
        let locked = if change.changed {
            lock_user_authorizations(&mut transaction, [membership.user_id, change.next_user_id])
                .await?
        } else {
            Vec::new()
        };
        let affected = ctx
            .table_query()?
            .where_primary_key_eq(input.id)?
            .update_in_tx(&mut transaction, input.data)
            .await?;
        if affected == 1 && change.changed {
            increment_locked_authz_versions(&mut transaction, &locked).await?;
        }
        if affected == 1 {
            append_membership_event(
                &mut transaction,
                ctx,
                id,
                change.next_org_id,
                change.next_user_id,
                Some(membership_summary(
                    membership.org_id,
                    membership.user_id,
                    &membership.status,
                    membership.admin,
                    None,
                )?),
                Some(membership_summary(
                    change.next_org_id,
                    change.next_user_id,
                    &change.next_status,
                    change.next_admin,
                    Some(changed_fields),
                )?),
            )
            .await?;
        }
        Ok(AffectedResult { affected })
    }
    .await;
    finish_transaction(transaction, result).await
}

pub(super) async fn delete(
    ctx: &ActionContext,
    input: GetByPk,
) -> Result<AffectedResult, BaseError> {
    let id = primary_key(&input.id)?;
    let org_id = membership_org_id(ctx, id).await?;
    // tenant-boundary: database org-member-delete-database
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        lock_active_org_admin(ctx, &mut transaction, org_id).await?;
        lock_active_organization(&mut transaction, org_id).await?;
        let Some(membership) = lock_membership(ctx, &mut transaction, id).await? else {
            return Ok(AffectedResult { affected: 0 });
        };
        if membership.org_id != org_id {
            return Err(BaseError::PermissionDenied(
                "企业成员在资源解析后已迁移，必须重新授权".to_string(),
            ));
        }
        let locked = lock_user_authorizations(&mut transaction, [membership.user_id]).await?;
        let affected = ctx
            .table_query()?
            .where_primary_key_eq(input.id)?
            .delete_in_tx(&mut transaction)
            .await?;
        if affected == 1 {
            increment_locked_authz_versions(&mut transaction, &locked).await?;
            append_membership_event(
                &mut transaction,
                ctx,
                id,
                membership.org_id,
                membership.user_id,
                Some(membership_summary(
                    membership.org_id,
                    membership.user_id,
                    &membership.status,
                    membership.admin,
                    None,
                )?),
                None,
            )
            .await?;
        }
        Ok(AffectedResult { affected })
    }
    .await;
    finish_transaction(transaction, result).await
}

async fn membership_org_id(ctx: &ActionContext, membership_id: i64) -> Result<i64, BaseError> {
    if let Ok(tenant) = ctx.tenant() {
        return Ok(tenant.id().get());
    }
    let user = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("企业成员管理需要已认证用户".to_string()))?;
    // tenant-boundary: system-capability org-member-resource-resolve-system
    let capability = ctx.system_tenant()?;
    if capability.actor().user_id() != user.id {
        return Err(BaseError::PermissionDenied(
            "系统租户 capability 与当前操作者不匹配".to_string(),
        ));
    }
    // tenant-boundary: raw-sql org-member-resource-resolve
    sqlx::query_scalar::<_, i64>("SELECT org_org FROM org_user WHERE id = ?")
        .bind(membership_id)
        // tenant-boundary: database org-member-resource-resolve-database
        .fetch_optional(ctx.tools().mysql()?.pool())
        .await
        .map_err(yang_db::DbError::from)?
        .ok_or_else(|| BaseError::RecordNotFound(format!("企业成员 {membership_id}")))
}

async fn lock_active_org_admin(
    ctx: &ActionContext,
    transaction: &mut Transaction,
    org_id: i64,
) -> Result<(), BaseError> {
    let user = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("企业成员管理需要已认证用户".to_string()))?;
    if user.has_role("system") {
        // tenant-boundary: system-capability org-member-linearization-system
        let capability = ctx.system_tenant()?;
        if capability.actor().user_id() != user.id {
            return Err(BaseError::PermissionDenied(
                "系统租户 capability 与当前操作者不匹配".to_string(),
            ));
        }
    } else {
        let authorized =
            // tenant-boundary: raw-sql org-member-admin-linearization
            sqlx::query_scalar::<_, i64>(
                "SELECT id FROM org_user \
                 WHERE org_org = ? AND user_user = ? \
                   AND status = 'active' AND admin = TRUE \
                 FOR UPDATE",
            )
            .bind(org_id)
            .bind(user.id)
            .fetch_optional(executor(transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
        if authorized.is_none() {
            return Err(BaseError::PermissionDenied(
                "当前用户在写事务内已不是该企业的有效管理员".to_string(),
            ));
        }
    }
    resource_authorization_checkpoint(ctx, ResourceAuthorizationCheckpoint::AfterLinearization)
        .await;
    Ok(())
}

fn add_org_id(ctx: &ActionContext, input: &Record) -> Result<i64, BaseError> {
    if let Ok(tenant) = ctx.tenant() {
        let org_id = tenant.id().get();
        if let Some(requested) = input.get(ORG_ID) {
            let requested = value_as_i64(ORG_ID, requested)?;
            if requested != org_id {
                return Err(BaseError::PermissionDenied(
                    "成员归属企业与当前租户不一致".to_string(),
                ));
            }
        }
        return Ok(org_id);
    }
    // tenant-boundary: system-capability org-member-add-system
    ctx.system_tenant()?;
    input.require::<i64>(ORG_ID)
}

async fn lock_active_organization(
    transaction: &mut Transaction,
    org_id: i64,
) -> Result<(), BaseError> {
    let query =
        // tenant-boundary: raw-sql org-member-organization-lock
        sqlx::query_scalar::<_, String>("SELECT status FROM org_org WHERE id = ? FOR UPDATE");
    let status = query
        .bind(org_id)
        .fetch_optional(executor(transaction)?)
        .await
        .map_err(yang_db::DbError::from)?
        .ok_or_else(|| BaseError::RecordNotFound(format!("企业 {org_id}")))?;
    if status != ACTIVE_ORG_STATUS {
        return Err(BaseError::PermissionDenied(format!(
            "企业 {org_id} 当前不可写"
        )));
    }
    Ok(())
}

async fn lock_membership(
    ctx: &ActionContext,
    transaction: &mut Transaction,
    id: i64,
) -> Result<Option<LockedMembership>, BaseError> {
    let row = if let Ok(tenant) = ctx.tenant() {
        // tenant-boundary: raw-sql org-member-tenant-lock
        sqlx::query_as::<_, (i64, i64, i64, String, bool)>(
            "SELECT id, org_org, user_user, status, admin \
             FROM org_user WHERE id = ? AND org_org = ? FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.id().get())
        .fetch_optional(executor(transaction)?)
        .await
        .map_err(yang_db::DbError::from)?
    } else {
        // tenant-boundary: system-capability org-member-lock-system
        ctx.system_tenant()?;
        // tenant-boundary: raw-sql org-member-system-lock
        sqlx::query_as::<_, (i64, i64, i64, String, bool)>(
            "SELECT id, org_org, user_user, status, admin \
             FROM org_user WHERE id = ? FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(executor(transaction)?)
        .await
        .map_err(yang_db::DbError::from)?
    };
    Ok(
        row.map(|(_id, org_id, user_id, status, admin)| LockedMembership {
            org_id,
            user_id,
            status,
            admin,
        }),
    )
}

fn authorization_change(
    membership: &LockedMembership,
    data: &Record,
) -> Result<MembershipAuthorizationChange, BaseError> {
    let next_org_id = proposed_i64(data, ORG_ID, membership.org_id)?;
    let next_user_id = proposed_i64(data, USER_ID, membership.user_id)?;
    let next_status = proposed_string(data, STATUS, &membership.status)?;
    let next_admin = proposed_bool(data, IS_ADMIN, membership.admin)?;
    Ok(MembershipAuthorizationChange {
        next_org_id,
        next_user_id,
        next_status: next_status.clone(),
        next_admin,
        changed: next_org_id != membership.org_id
            || next_user_id != membership.user_id
            || next_status != membership.status
            || next_admin != membership.admin,
    })
}

fn membership_summary(
    org_id: i64,
    user_id: i64,
    status: &str,
    admin: bool,
    changed_fields: Option<Vec<Value>>,
) -> Result<audit::AuditSummary, BaseError> {
    audit::summary([
        ("admin", json!(admin)),
        ("changed_fields", json!(changed_fields)),
        ("org_id", json!(org_id)),
        ("status", json!(status)),
        ("user_id", json!(user_id)),
    ])
}

async fn append_membership_event(
    transaction: &mut Transaction,
    ctx: &ActionContext,
    id: impl ToString,
    org_id: i64,
    user_id: i64,
    before: Option<audit::AuditSummary>,
    after: Option<audit::AuditSummary>,
) -> Result<(), BaseError> {
    let event = audit::succeeded_event(
        ctx,
        Some(org_id),
        Some(audit::entity("user", user_id)?),
        audit::entity("org_membership", id)?,
        before,
        after,
    )?;
    audit::append_in_tx(transaction, &event).await
}

fn proposed_i64(data: &Record, field: &str, current: i64) -> Result<i64, BaseError> {
    data.get(field)
        .map_or(Ok(current), |value| value_as_i64(field, value))
}

fn proposed_string(data: &Record, field: &str, current: &str) -> Result<String, BaseError> {
    data.get(field).map_or_else(
        || Ok(current.to_string()),
        |value| {
            serde_json::from_value(value.clone())
                .map_err(|error| BaseError::InvalidFieldType(field.to_string(), error.to_string()))
        },
    )
}

fn proposed_bool(data: &Record, field: &str, current: bool) -> Result<bool, BaseError> {
    data.get(field).map_or_else(
        || Ok(current),
        |value| {
            serde_json::from_value(value.clone())
                .map_err(|error| BaseError::InvalidFieldType(field.to_string(), error.to_string()))
        },
    )
}

fn value_as_i64(field: &str, value: &Value) -> Result<i64, BaseError> {
    serde_json::from_value(value.clone())
        .map_err(|error| BaseError::InvalidFieldType(field.to_string(), error.to_string()))
}

fn primary_key(value: &Value) -> Result<i64, BaseError> {
    value_as_i64("id", value)
}

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "企业成员授权 writer 事务已结束".to_string(),
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
                tracing::error!("企业成员授权 writer 回滚失败: error={}", rollback_error);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::org::user::ACTIVE_STATUS;

    fn membership() -> LockedMembership {
        LockedMembership {
            org_id: 10,
            user_id: 20,
            status: ACTIVE_STATUS.to_string(),
            admin: false,
        }
    }

    #[test]
    fn display_only_and_idempotent_updates_do_not_change_authorization() {
        let display = authorization_change(&membership(), &Record::new().set("name", "新姓名"))
            .unwrap_or_else(|error| panic!("展示字段应可解析: {error}"));
        assert!(!display.changed);

        let idempotent = authorization_change(
            &membership(),
            &Record::new()
                .set(ORG_ID, 10)
                .set(USER_ID, 20)
                .set(STATUS, ACTIVE_STATUS)
                .set(IS_ADMIN, false),
        )
        .unwrap_or_else(|error| panic!("幂等授权字段应可解析: {error}"));
        assert!(!idempotent.changed);
    }

    #[test]
    fn every_authorization_field_change_is_detected() {
        for data in [
            Record::new().set(ORG_ID, 11),
            Record::new().set(USER_ID, 21),
            Record::new().set(STATUS, "disabled"),
            Record::new().set(IS_ADMIN, true),
        ] {
            assert!(
                authorization_change(&membership(), &data)
                    .unwrap_or_else(|error| panic!("授权字段应可解析: {error}"))
                    .changed
            );
        }
    }
}
