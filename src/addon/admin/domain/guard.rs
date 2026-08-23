//! 最终管理员启动不变量。

use serde_json::json;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, Predicate, QueryBuilder};

/// 校验最终管理员与用户数据之间的启动不变量。
pub(crate) async fn validate_system_owner_state(pool: &sqlx::MySqlPool) -> Result<(), BaseError> {
    let user_count = QueryBuilder::from_pool(pool, table!("users"))
        .count()
        .await?;
    let owner_count = QueryBuilder::from_pool(pool, table!("admin_user"))
        .where_and(field!("owner_key"), CompareOp::Eq, "system-owner")
        .count()
        .await?;
    let invalid_owner_count = QueryBuilder::from_pool(pool, table!("admin_user"))
        .where_not_null(field!("owner_key"))
        .where_predicate(&Predicate::Or(vec![
            Predicate::Compare(
                field!("owner_key").clone(),
                CompareOp::Ne,
                json!("system-owner"),
            ),
            Predicate::Compare(field!("admin").clone(), CompareOp::Ne, json!(true)),
            Predicate::Compare(field!("status").clone(), CompareOp::Ne, json!("active")),
        ]))?
        .count()
        .await?;
    if invalid_owner_count > 0 {
        return Err(BaseError::ConfigError(format!(
            "最终管理员数据不满足 owner_key/admin/status 不变量，问题记录数: {invalid_owner_count}"
        )));
    }
    validate_owner_counts(user_count, owner_count)
}

fn validate_owner_counts(user_count: i64, owner_count: i64) -> Result<(), BaseError> {
    match (user_count, owner_count) {
        (0, 0) => Ok(()),
        (users, 1) if users > 0 => Ok(()),
        (users, 0) => Err(BaseError::ConfigError(format!(
            "系统已有 {users} 个用户但没有最终管理员；拒绝自动选择，请人工指定 owner_key"
        ))),
        (_, owners) => Err(BaseError::ConfigError(format!(
            "系统存在 {owners} 个最终管理员；必须人工修复为唯一记录"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_state_is_fail_closed_for_missing_or_duplicate_owner() {
        assert!(validate_owner_counts(0, 0).is_ok());
        assert!(validate_owner_counts(5, 1).is_ok());
        assert!(validate_owner_counts(0, 1).is_err());
        assert!(matches!(
            validate_owner_counts(5, 0),
            Err(BaseError::ConfigError(message)) if message.contains("5")
        ));
        assert!(matches!(
            validate_owner_counts(5, 2),
            Err(BaseError::ConfigError(message)) if message.contains("2")
        ));
    }
}
