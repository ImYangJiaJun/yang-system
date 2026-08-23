//! 平台账号持久化边界。
//! authorization-writer: admin-authorization-facts

use super::model::{AdminAccountPage, AdminAccountView, PageRequest};
use super::{ACTIVE_STATUS, IS_ADMIN, NAME, POSITION, STATUS, SYSTEM_ROLE, USER_ID};
use crate::addon::account::{
    create_password_reset_in_tx, increment_locked_authz_version, lock_user_authorization,
    GeneratedPasswordReset, LockedUserAuthorization, OwnerClaimOutcome, SystemOwnerClaimer,
};
use crate::audit;
use crate::authorization::{resource_authorization_checkpoint, ResourceAuthorizationCheckpoint};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, DbError, QueryBuilder, SortOrder, SqlExpr, Transaction};

/// 数据库只允许这一种非空最终管理员哨兵。
pub(super) const SYSTEM_OWNER_KEY: &str = "system-owner";

/// 无状态的最终管理员声明器。
pub(crate) struct AdminSystemOwnerClaimer;

#[async_trait]
impl SystemOwnerClaimer for AdminSystemOwnerClaimer {
    async fn claim(
        &self,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        let result = transaction
            .table(table!("admin_user"))
            .set_expr(field!("created_at"), SqlExpr::unix_timestamp())
            .set_expr(field!("updated_at"), SqlExpr::unix_timestamp())
            .insert_returning_id(&json!({
                "user_user": user_id,
                "name": username,
                "status": ACTIVE_STATUS,
                "admin": true,
                "owner_key": SYSTEM_OWNER_KEY,
            }))
            .await;

        match result {
            Ok(id) => {
                let admin_id = i64::try_from(id)
                    .map_err(|_| BaseError::Unknown("最终管理员主键超出 i64 范围".to_string()))?;
                Ok(OwnerClaimOutcome::Claimed { admin_id })
            }
            Err(error) => {
                // 唯一哨兵冲突经 DbError 约束错误分类识别，与原 is_unique_violation 判定等价。
                if !matches!(&error, DbError::ConstraintError(_)) {
                    return Err(BaseError::from(error));
                }
                let owner_count = transaction
                    .table(table!("admin_user"))
                    .where_and(field!("owner_key"), CompareOp::Eq, SYSTEM_OWNER_KEY)
                    .count()
                    .await?;
                if owner_count > 0 {
                    Ok(OwnerClaimOutcome::AlreadyClaimed)
                } else {
                    Err(BaseError::from(error))
                }
            }
        }
    }
}

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

/// 平台账号列表/总数共用的 JOIN 与可选搜索过滤。
fn admin_account_join<'a>(pool: &'a sqlx::MySqlPool, pattern: Option<&str>) -> QueryBuilder<'a> {
    let builder = QueryBuilder::from_pool(pool, table!("admin_user")).join(
        table!("users"),
        field!("users.id"),
        field!("admin_user.user_user"),
    );
    match pattern {
        Some(pattern) => builder
            .where_and(
                field!("admin_user.name"),
                CompareOp::Like,
                pattern.to_string(),
            )
            .where_or(
                field!("users.username"),
                CompareOp::Like,
                pattern.to_string(),
            ),
        None => builder,
    }
}

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
        // (? IS NULL OR name LIKE ? OR username LIKE ?) 改写为条件组装：
        // 无搜索词时无 WHERE，与原谓词在 NULL 下恒真的语义一致。
        let rows = admin_account_join(pool, pattern.as_deref())
            .field(field!("admin_user.id"))
            .field(field!("admin_user.user_user"))
            .field(field!("users.username"))
            .field(field!("admin_user.name"))
            .field(field!("admin_user.position"))
            .field(field!("admin_user.status"))
            .field(field!("admin_user.admin"))
            .field(field!("admin_user.created_at"))
            .field(field!("admin_user.updated_at"))
            .order(field!("admin_user.created_at"), SortOrder::Desc)
            .order(field!("admin_user.id"), SortOrder::Desc)
            .limit(request.sql_limit()?)
            .offset(request.offset)
            .select::<AdminRow>()
            .await?;
        let total = admin_account_join(pool, pattern.as_deref()).count().await?;
        let total = usize::try_from(total)
            .map_err(|_| BaseError::Unknown("平台账号总数超出 usize 范围".to_string()))?;
        Ok(AdminAccountPage::new(
            rows.into_iter().map(admin_view).collect(),
            total,
            request,
        ))
    }

    pub(super) async fn add(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        name: &str,
        position: Option<&str>,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        let database = ctx.tools().mysql()?;
        let mut transaction = database.transaction().await?;
        let result = async {
            lock_active_admin_actor(ctx, database.pool(), &mut transaction).await?;
            let locked =
                lock_user_authorization(database.pool(), &mut transaction, user_id).await?;
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

    pub(super) async fn create_password_reset(
        &self,
        ctx: &ActionContext,
        target_user_id: i64,
        requested_by_user_id: i64,
        reset: &GeneratedPasswordReset,
        ttl_seconds: u64,
    ) -> Result<(), BaseError> {
        let database = ctx.tools().mysql()?;
        let mut transaction = database.transaction().await?;
        let result = async {
            lock_active_admin_actor(ctx, database.pool(), &mut transaction).await?;
            let locked =
                lock_user_authorization(database.pool(), &mut transaction, target_user_id).await?;
            ensure_active_user(&locked)?;
            create_password_reset_in_tx(
                &mut transaction,
                target_user_id,
                requested_by_user_id,
                reset,
                ttl_seconds,
            )
            .await?;
            let event = audit::succeeded_event(
                ctx,
                None,
                Some(audit::entity("user", target_user_id)?),
                audit::entity("password_reset", reset.reference().fingerprint())?,
                None,
                Some(audit::summary([
                    ("expires_in_seconds", json!(ttl_seconds)),
                    ("reset_fingerprint", json!(reset.reference().fingerprint())),
                    ("user_id", json!(target_user_id)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await
        }
        .await;
        finish_transaction(transaction, result).await
    }

    pub(super) async fn set_status(
        &self,
        ctx: &ActionContext,
        id: i64,
        status: &str,
    ) -> Result<AdminAccountView, BaseError> {
        let database = ctx.tools().mysql()?;
        let mut transaction = database.transaction().await?;
        let result = async {
            let active_admins = lock_active_admins(database.pool(), &mut transaction).await?;
            ensure_active_admin_actor(ctx, &active_admins)?;
            resource_authorization_checkpoint(
                ctx,
                ResourceAuthorizationCheckpoint::AfterLinearization,
            )
            .await;
            let target = lock_target(database.pool(), &mut transaction, id).await?;
            if status == target.status {
                return Ok(id);
            }
            if status != ACTIVE_STATUS {
                ensure_owner_mutation_allowed(target.owner_key.as_deref(), OwnerMutation::Disable)?;
            }
            if status != ACTIVE_STATUS && target.status == ACTIVE_STATUS && target.admin {
                ensure_not_last_active_admin(&active_admins, id)?;
            }
            let locked =
                lock_user_authorization(database.pool(), &mut transaction, target.user_id).await?;
            transaction
                .table(table!("admin_user"))
                .set_expr(field!("updated_at"), SqlExpr::unix_timestamp())
                .where_and(field!("id"), CompareOp::Eq, id)
                .update(&json!({ "status": status }))
                .await?;
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
        let database = ctx.tools().mysql()?;
        let mut transaction = database.transaction().await?;
        let result = async {
            let active_admins = lock_active_admins(database.pool(), &mut transaction).await?;
            ensure_active_admin_actor(ctx, &active_admins)?;
            resource_authorization_checkpoint(
                ctx,
                ResourceAuthorizationCheckpoint::AfterLinearization,
            )
            .await;
            let target = lock_target(database.pool(), &mut transaction, id).await?;
            if admin == target.admin {
                return Ok(id);
            }
            if !admin {
                ensure_owner_mutation_allowed(target.owner_key.as_deref(), OwnerMutation::Demote)?;
            }
            if !admin && target.status == ACTIVE_STATUS && target.admin {
                ensure_not_last_active_admin(&active_admins, id)?;
            }
            let locked =
                lock_user_authorization(database.pool(), &mut transaction, target.user_id).await?;
            transaction
                .table(table!("admin_user"))
                .set_expr(field!("updated_at"), SqlExpr::unix_timestamp())
                .where_and(field!("id"), CompareOp::Eq, id)
                .update(&json!({ "admin": admin }))
                .await?;
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
        let row = admin_account_join(ctx.tools().mysql()?.pool(), None)
            .field(field!("admin_user.id"))
            .field(field!("admin_user.user_user"))
            .field(field!("users.username"))
            .field(field!("admin_user.name"))
            .field(field!("admin_user.position"))
            .field(field!("admin_user.status"))
            .field(field!("admin_user.admin"))
            .field(field!("admin_user.created_at"))
            .field(field!("admin_user.updated_at"))
            .where_and(field!("admin_user.id"), CompareOp::Eq, id)
            .find::<AdminRow>()
            .await?
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
    owner_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerMutation {
    Disable,
    Demote,
}

fn ensure_owner_mutation_allowed(
    owner_key: Option<&str>,
    mutation: OwnerMutation,
) -> Result<(), BaseError> {
    if owner_key != Some(SYSTEM_OWNER_KEY) {
        return Ok(());
    }
    let operation = match mutation {
        OwnerMutation::Disable => "停用",
        OwnerMutation::Demote => "降级",
    };
    Err(BaseError::PermissionDenied(format!(
        "系统最终管理员不能被{operation}"
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LockedActiveAdmin {
    id: i64,
    user_id: i64,
}

async fn lock_active_admin_actor(
    ctx: &ActionContext,
    pool: &sqlx::MySqlPool,
    transaction: &mut Transaction,
) -> Result<(), BaseError> {
    let actor_user_id = authenticated_actor_user_id(ctx)?;
    let actor = transaction
        .select_for_update::<(i64,)>(
            QueryBuilder::from_pool(pool, table!("admin_user"))
                .field(field!("id"))
                .where_and(field!("user_user"), CompareOp::Eq, actor_user_id)
                .where_and(field!("status"), CompareOp::Eq, ACTIVE_STATUS)
                .where_and(field!("admin"), CompareOp::Eq, true),
        )
        .await?
        .into_iter()
        .next();
    if actor.is_none() {
        return Err(BaseError::PermissionDenied(
            "当前用户在写事务内已不是有效平台超级管理员".to_string(),
        ));
    }
    resource_authorization_checkpoint(ctx, ResourceAuthorizationCheckpoint::AfterLinearization)
        .await;
    Ok(())
}

fn ensure_active_admin_actor(
    ctx: &ActionContext,
    active_admins: &[LockedActiveAdmin],
) -> Result<(), BaseError> {
    let actor_user_id = authenticated_actor_user_id(ctx)?;
    if active_admins
        .iter()
        .any(|admin| admin.user_id == actor_user_id)
    {
        return Ok(());
    }
    Err(BaseError::PermissionDenied(
        "当前用户在写事务内已不是有效平台超级管理员".to_string(),
    ))
}

fn authenticated_actor_user_id(ctx: &ActionContext) -> Result<i64, BaseError> {
    ctx.authenticated_user()
        .map(|user| user.id)
        .ok_or_else(|| BaseError::Unauthorized("平台高权限写入需要已认证用户".to_string()))
}

async fn lock_target(
    pool: &sqlx::MySqlPool,
    transaction: &mut Transaction,
    id: i64,
) -> Result<LockedAdminTarget, BaseError> {
    transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("admin_user"))
                .field(field!("status"))
                .field(field!("admin"))
                .field(field!("user_user"))
                .field(field!("owner_key"))
                .where_and(field!("id"), CompareOp::Eq, id),
        )
        .await?
        .into_iter()
        .next()
        .map(|(status, admin, user_id, owner_key)| LockedAdminTarget {
            status,
            admin,
            user_id,
            owner_key,
        })
        .ok_or_else(|| BaseError::RecordNotFound(format!("平台账号 {id}")))
}

async fn lock_active_admins(
    pool: &sqlx::MySqlPool,
    transaction: &mut Transaction,
) -> Result<Vec<LockedActiveAdmin>, BaseError> {
    let rows = transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("admin_user"))
                .field(field!("id"))
                .field(field!("user_user"))
                .where_and(field!("status"), CompareOp::Eq, ACTIVE_STATUS)
                .where_and(field!("admin"), CompareOp::Eq, true)
                .order(field!("id"), SortOrder::Asc),
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, user_id)| LockedActiveAdmin { id, user_id })
        .collect())
}

fn ensure_not_last_active_admin(
    active_admins: &[LockedActiveAdmin],
    target_id: i64,
) -> Result<(), BaseError> {
    if active_admins.len() < 2 || !active_admins.iter().any(|admin| admin.id == target_id) {
        return Err(BaseError::PermissionDenied(
            "不能停用或降级最后一个启用中的超级管理员".to_string(),
        ));
    }
    Ok(())
}

fn ensure_active_user(locked: &LockedUserAuthorization) -> Result<(), BaseError> {
    if !locked.status().is_active() {
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
    fn system_owner_uses_one_stable_guard_and_cannot_be_mutated() {
        assert_eq!(SYSTEM_OWNER_KEY, "system-owner");
        for field in [USER_ID, NAME, POSITION, STATUS, IS_ADMIN, "owner_key"] {
            assert!(!field.is_empty());
        }

        for mutation in [OwnerMutation::Disable, OwnerMutation::Demote] {
            assert!(matches!(
                ensure_owner_mutation_allowed(Some(SYSTEM_OWNER_KEY), mutation),
                Err(BaseError::PermissionDenied(message))
                    if message.contains("最终管理员")
            ));
        }
        assert!(ensure_owner_mutation_allowed(None, OwnerMutation::Disable).is_ok());
    }
}
