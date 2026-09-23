//! 首个注册账号声明系统最终管理员的持久化端口。

use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 系统最终管理员声明结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerClaimOutcome {
    /// 当前注册事务成功声明了唯一最终管理员。
    ///
    /// 默认声明器恒不构造该变体，注册流程的 Claimed 分支当前不可达；
    /// 待 access 域重新引入声明器实现后即可移除这里的 allow。
    #[allow(dead_code)]
    Claimed { admin_id: i64 },
    /// 最终管理员已经由另一个已提交或正在提交的事务声明。
    AlreadyClaimed,
}

/// 由平台账号域实现的最终管理员声明端口。
#[async_trait]
pub(crate) trait SystemOwnerClaimer: Send + Sync {
    /// 在创建用户的同一事务中竞争唯一最终管理员哨兵。
    ///
    /// `ctx` 是必需参数：实现方必须经受信 writer 写入授权事实，
    /// 而 writer 需要 `ctx` 取得连接池（`trusted_query`）。
    async fn claim(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError>;
}

/// 不声明最终管理员的默认声明器。
///
/// 当前骨架只保留 account Addon：注册流程照常完成，
/// 但任何账号都不会成为系统最终管理员。
pub(crate) struct NoSystemOwnerClaimer;

#[async_trait]
impl SystemOwnerClaimer for NoSystemOwnerClaimer {
    async fn claim(
        &self,
        _ctx: &ActionContext,
        _transaction: &mut Transaction,
        _user_id: i64,
        _username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        Ok(OwnerClaimOutcome::AlreadyClaimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认声明器恒不夺哨兵，且不读取 `ctx` 的任何状态。
    ///
    /// `#[ignore]`：`claim` 必须收到一个真实的 `Transaction`，而
    /// `Database::transaction()` 走 `pool.begin()`，懒连接池在此也会真正建连——
    /// `cargo test --lib` 这条门禁的既有契约是**不需要任何外部服务**
    /// （与 `context.rs` 的 TOTP Redis 用例同例），故本用例留待真实 MySQL 环境执行。
    #[tokio::test]
    #[ignore = "需要真实 MySQL：claim 需要真实 Transaction，懒连接池在 begin 时仍会建连"]
    async fn default_claimer_never_claims_and_needs_no_context_side_effects() {
        // 默认实现必须恒返回 AlreadyClaimed，且不触碰任何 ctx 状态。
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = yang_db::Database::from_pool(pool, yang_db::DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("{error}"));
        let tools = std::sync::Arc::new(
            yang_base::tools::ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("{error}")),
        );
        let ctx = ActionContext::new(
            yang_base::action::Request::new(serde_json::json!({})),
            tools,
        );
        let mut transaction = ctx
            .tools()
            .mysql()
            .unwrap_or_else(|e| panic!("{e}"))
            .transaction()
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let outcome = NoSystemOwnerClaimer
            .claim(&ctx, &mut transaction, 1, "first")
            .await
            .unwrap_or_else(|e| panic!("默认实现必须成功返回: {e}"));
        assert_eq!(outcome, OwnerClaimOutcome::AlreadyClaimed);
    }
}
