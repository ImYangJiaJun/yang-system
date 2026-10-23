//! 过期直授的后台收敛（access-infra#13）。
//!
//! 直授行过期后，读端（Token 签发/刷新）会按 `expires_at` 过滤，但**已签发**的
//! Access Token 仍带着旧权限快照，最长要等一个 access TTL 才自然失效。本机制在
//! 后台按批找出「已过期且尚未收敛」的行，对所属用户递增 `authz_version` 并写
//! `authorization_outbox`，使存量 Access Token 在无积压时的一个收敛周期内失效
//!（有积压时按批排空）；行本身保留做审计。
//!
//! 收敛标记 `converged_at` 是幂等凭据：标记后同一行不会再触发版本递增；续期会把它
//! 重置为 NULL，让未来再次过期时重新收敛。版本与 Outbox 的 SQL 不在本文件重复实现，
//! 一律经 [`AuthorizationVersionWriter`](crate::authorization::AuthorizationVersionWriter)
//! 端口。

use super::context::Access;
use super::repository::current_unix_timestamp;
use std::collections::BTreeMap;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;

/// 单轮收敛最多扫描的直授行数。
pub(crate) const DEFAULT_CONVERGENCE_LIMIT: usize = 200;

/// 一轮收敛的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GrantExpiryReport {
    /// 本轮扫描到的过期未收敛行数。
    pub(crate) scanned: usize,
    /// 实际递增了授权版本的启用用户数。
    pub(crate) users: usize,
    /// 成功标记为已收敛的行数（并发续期/撤销的 CAS 失败行不计入）。
    pub(crate) converged: usize,
}

/// 过期直授收敛句柄：后台 Worker 与测试共用的唯一入口。
#[derive(Clone)]
pub(crate) struct GrantExpiryHandle {
    access: Arc<Access>,
}

impl GrantExpiryHandle {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }

    /// 执行一轮收敛。
    ///
    /// 事务内先取过期未收敛行，再按 `user_id` 升序（`BTreeMap` 迭代序）逐个锁定
    /// 用户行；升序是 access 域既有的全局锁序，防止与授予/撤销路径互相死锁。
    /// 已停用用户不递增版本（其 Token 本就不可用），但仍标记收敛，避免每轮重复扫描；
    /// 用户已被并发删除时跳过该用户（`UserNotFound`），不误标其行。
    pub(crate) async fn converge_once(
        &self,
        ctx: &ActionContext,
        limit: usize,
    ) -> Result<GrantExpiryReport, BaseError> {
        let now = current_unix_timestamp()?;
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let expired = self
                .access
                .grants()
                .list_expired_unconverged_in_tx(ctx, &mut transaction, now, limit)
                .await?;
            let scanned = expired.len();
            let mut by_user: BTreeMap<i64, Vec<_>> = BTreeMap::new();
            for grant in expired {
                by_user.entry(grant.user_id).or_default().push(grant);
            }
            let mut users = 0usize;
            let mut converged = 0usize;
            for (user_id, grants) in &by_user {
                let locked = match self
                    .access
                    .authorization()
                    .lock_authorization_version(
                        ctx.tools().mysql()?.pool(),
                        &mut transaction,
                        *user_id,
                    )
                    .await
                {
                    Ok(locked) => locked,
                    // 并发删号窗口：直授行还在、users 行已经没了。跳过该用户，
                    // 不把无人可失效的行误标为已收敛。
                    Err(BaseError::UserNotFound(_)) => continue,
                    Err(error) => return Err(error),
                };
                // 先做 converged_at 的 CAS 标记，再递增版本：两个重叠 Worker 只有
                // 一个能把行标记成功，另一个不会重复 bump/写第二条 Outbox。单实例
                // 部署下这只是幂等强化；多副本仍需额外的 claim（当前部署假设单实例）。
                let mut marked_here = 0usize;
                for grant in grants {
                    if self
                        .access
                        .grants()
                        .mark_converged_in_tx(
                            ctx,
                            &mut transaction,
                            grant.id,
                            grant.expires_at,
                            now,
                        )
                        .await?
                        == 1
                    {
                        marked_here += 1;
                    }
                }
                if marked_here > 0 && locked.is_active() {
                    self.access
                        .authorization()
                        .increment_locked_authorization_version(&mut transaction, &locked)
                        .await?;
                    users += 1;
                }
                converged += marked_here;
            }
            Ok(GrantExpiryReport {
                scanned,
                users,
                converged,
            })
        }
        .await;
        Access::finish_transaction(transaction, result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::domain::groups::tables::{
        group_items_table_spec, system_owner_table_spec, user_group_table_spec,
    };
    use crate::addon::access::domain::groups::GroupRepository;
    use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
    use crate::addon::access::domain::repository::GrantRepository;
    use crate::addon::access::grants::table::grants_table_spec;
    use crate::addon::access::groups::table::groups_table_spec;
    use crate::addon::account::user_table_spec;
    use crate::infrastructure::schema::authorization_outbox;
    use anyhow::{ensure, Context};
    use sqlx::MySqlPool;
    use yang_base::action::Request;
    use yang_base::database::DatabaseInitializer;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    fn test_context(pool: MySqlPool) -> ActionContext {
        let database = Database::from_pool(pool, DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(database)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        ActionContext::new(Request::new(serde_json::json!({})), tools)
    }

    fn test_access() -> Arc<Access> {
        Arc::new(Access::new(
            GrantRepository::new(
                grants_table_spec()
                    .and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("授权表定义应有效: {error}")),
            ),
            GroupRepository::new(
                groups_table_spec()
                    .and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("组表定义应有效: {error}")),
                group_items_table_spec()
                    .and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("组条目表定义应有效: {error}")),
                user_group_table_spec()
                    .and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("用户组表定义应有效: {error}")),
                system_owner_table_spec()
                    .and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("引导哨兵表定义应有效: {error}")),
            ),
            PermissionCatalogHandle::new(),
            crate::addon::account::authorization_port(),
        ))
    }

    async fn converge_twice_and_assert(
        pool: &MySqlPool,
        ctx: &ActionContext,
        handle: &GrantExpiryHandle,
        user_id: i64,
        expired_grant_id: i64,
    ) -> anyhow::Result<()> {
        let first = handle.converge_once(ctx, DEFAULT_CONVERGENCE_LIMIT).await?;
        ensure!(
            first
                == GrantExpiryReport {
                    scanned: 1,
                    users: 1,
                    converged: 1,
                },
            "首轮必须恰好收敛一行: {first:?}"
        );
        let version: i64 = sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
        ensure!(version == 2, "收敛后授权版本必须递增到 2，实际 {version}");
        let outbox: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ? AND authz_version = 2",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await?;
        ensure!(
            outbox == 1,
            "收敛必须写入一条版本 2 的 Outbox，实际 {outbox}"
        );
        let converged_at: Option<i64> =
            sqlx::query_scalar("SELECT converged_at FROM authz_grant WHERE id = ?")
                .bind(expired_grant_id)
                .fetch_one(pool)
                .await?;
        ensure!(converged_at.is_some(), "过期行必须被标记为已收敛");

        let second = handle.converge_once(ctx, DEFAULT_CONVERGENCE_LIMIT).await?;
        ensure!(
            second == GrantExpiryReport::default(),
            "已收敛的行不得再次触发版本递增: {second:?}"
        );
        let version: i64 = sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
        ensure!(version == 2, "第二轮不得改变授权版本，实际 {version}");
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(pool)
                .await?;
        ensure!(outbox == 1, "第二轮不得追加 Outbox，实际 {outbox}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL"]
    async fn real_mysql_converges_expired_grants_once_and_reconverges_after_renew(
    ) -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
            .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect_with_config(
            &url,
            DatabaseConfig::default()
                .with_max_connections(4)
                .with_min_connections(0)
                .with_connect_timeout(10),
        )
        .await?;
        let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        ensure!(
            name.as_deref().is_some_and(|name| name.ends_with("_test")),
            "过期收敛集成测试只能使用 _test 数据库"
        );

        // 先删引用 users 的子表，再删 users，避免外键约束阻断（与现有集成
        // 测试夹具同一份表清单；`IF EXISTS` 允许部分初始化的测试库）。
        for table_name in [
            "user_avatar",
            "password_reset_token",
            "user_session",
            "login_event",
            "audit_event",
            "authorization_outbox",
            "user_group",
            "permission_group_item",
            "permission_group",
            "system_owner",
            "authz_grant",
            "users",
        ] {
            sqlx::query(&format!("DROP TABLE IF EXISTS `{table_name}`"))
                .execute(database.pool())
                .await?;
        }
        let users = user_table_spec()?.table_definition()?;
        let grants = grants_table_spec()?.table_definition()?;
        let outbox = authorization_outbox()?;
        let initializer_database = Database::from_pool(
            database.pool().clone(),
            DatabaseConfig::default().with_max_connections(4),
        )?;
        DatabaseInitializer::new(initializer_database)
            .sync_table_definitions(&[&users, &grants, &outbox])
            .await?;

        let now: i64 = sqlx::query_scalar("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
            .fetch_one(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO users \
             (username, password_hash, status, authz_version, credential_version, created_at, updated_at) \
             VALUES ('grant_expiry_user', 'inert-hash', 'active', 1, 0, ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(database.pool())
        .await?;
        let user_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE username = 'grant_expiry_user'")
                .fetch_one(database.pool())
                .await?;
        for (permission, expires_at) in [
            ("access.grants.read", now - 60),
            ("access.grants.write", now + 3_600),
        ] {
            sqlx::query(
                "INSERT INTO authz_grant (user_id, permission, granted_by, occurred_at, expires_at) \
                 VALUES (?, ?, 0, ?, ?)",
            )
            .bind(user_id)
            .bind(permission)
            .bind(now)
            .bind(expires_at)
            .execute(database.pool())
            .await?;
        }
        let expired_grant_id: i64 = sqlx::query_scalar(
            "SELECT id FROM authz_grant WHERE user_id = ? AND permission = 'access.grants.read'",
        )
        .bind(user_id)
        .fetch_one(database.pool())
        .await?;

        let ctx = test_context(database.pool().clone());
        let access = test_access();
        let handle = GrantExpiryHandle::new(Arc::clone(&access));
        converge_twice_and_assert(database.pool(), &ctx, &handle, user_id, expired_grant_id)
            .await?;

        // 未过期行不参与收敛。
        let fresh_converged_at: Option<i64> = sqlx::query_scalar(
            "SELECT converged_at FROM authz_grant \
             WHERE user_id = ? AND permission = 'access.grants.write'",
        )
        .bind(user_id)
        .fetch_one(database.pool())
        .await?;
        ensure!(
            fresh_converged_at.is_none(),
            "未过期行必须保持 converged_at = NULL"
        );

        // 续期必须清掉收敛标记；未来再次过期时可以重新收敛。
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        access
            .grants()
            .renew_in_tx(
                &ctx,
                &mut transaction,
                user_id,
                "access.grants.read",
                0,
                Some(now + 3_600),
            )
            .await?;
        transaction.commit().await?;
        let renewed_converged_at: Option<i64> =
            sqlx::query_scalar("SELECT converged_at FROM authz_grant WHERE id = ?")
                .bind(expired_grant_id)
                .fetch_one(database.pool())
                .await?;
        ensure!(
            renewed_converged_at.is_none(),
            "续期必须把 converged_at 重置为 NULL"
        );

        sqlx::query("UPDATE authz_grant SET expires_at = ? WHERE id = ?")
            .bind(now - 1)
            .bind(expired_grant_id)
            .execute(database.pool())
            .await?;
        let third = handle
            .converge_once(&ctx, DEFAULT_CONVERGENCE_LIMIT)
            .await?;
        ensure!(
            third
                == GrantExpiryReport {
                    scanned: 1,
                    users: 1,
                    converged: 1,
                },
            "续期后再次过期必须重新收敛: {third:?}"
        );
        let version: i64 = sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(database.pool())
            .await?;
        ensure!(version == 3, "第二次收敛后授权版本必须为 3，实际 {version}");
        let outbox: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ? AND authz_version = 3",
        )
        .bind(user_id)
        .fetch_one(database.pool())
        .await?;
        ensure!(outbox == 1, "第二次收敛必须写入版本 3 的 Outbox");

        // 先删引用 users 的子表，再删 users，避免外键约束阻断（与现有集成
        // 测试夹具同一份表清单；`IF EXISTS` 允许部分初始化的测试库）。
        for table_name in [
            "user_avatar",
            "password_reset_token",
            "user_session",
            "login_event",
            "audit_event",
            "authorization_outbox",
            "user_group",
            "permission_group_item",
            "permission_group",
            "system_owner",
            "authz_grant",
            "users",
        ] {
            sqlx::query(&format!("DROP TABLE IF EXISTS `{table_name}`"))
                .execute(database.pool())
                .await?;
        }
        database.close().await;
        Ok(())
    }
}
