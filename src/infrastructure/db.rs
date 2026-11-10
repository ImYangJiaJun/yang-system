//! 数据库事务的统一句柄与 MySQL 专用操作。
//!
//! 框架自 yang-base 0.4 之后把事务抽象为 `AnyTransaction`（MySQL/PostgreSQL
//! 按运行时后端分派），`TableQuery` 的 `*_in_tx` 系列都接收这个类型。应用当前
//! 只启用 MySQL，但类型必须跟随框架；集中在本模块，避免各仓储再次各自导入
//! 旧类型 `yang_db::Transaction` 后与框架漂移。

pub(crate) use yang_base::database::AnyTransaction as Transaction;
use yang_base::BaseError;

/// 从 MySQL `Database` 开启一个统一事务句柄。
///
/// Action 路径优先用 `ActionContext::begin_transaction()`；本函数服务 worker、
/// pull 与测试里直接持有 `yang_db::Database` 的场景。
pub(crate) async fn begin_mysql(database: &yang_db::Database) -> Result<Transaction, BaseError> {
    database
        .transaction()
        .await
        .map(Transaction::MySql)
        .map_err(BaseError::from)
}

/// 从 MySQL `Database` 开启一个只读统一事务句柄。
pub(crate) async fn begin_mysql_read_only(
    database: &yang_db::Database,
) -> Result<Transaction, BaseError> {
    database
        .read_only_transaction()
        .await
        .map(Transaction::MySql)
        .map_err(BaseError::from)
}

/// 取得底层 MySQL 事务；非 MySQL 后端失败关闭。
pub(crate) fn mysql_transaction(
    transaction: &mut Transaction,
) -> Result<&mut yang_db::mysql::Transaction, BaseError> {
    transaction.as_mysql().ok_or_else(|| {
        BaseError::ConfigError("当前数据库后端不是 MySQL，无法执行 MySQL 专用操作".to_string())
    })
}

/// 在 MySQL 事务上执行 `SELECT ... FOR UPDATE`。
///
/// 行锁是 MySQL 后端专有能力；当前应用只启用 MySQL，若运行时拿到非 MySQL
/// 事务则失败关闭，绝不退化成无锁读取。
pub(crate) async fn select_for_update<T>(
    transaction: &mut Transaction,
    query: yang_db::QueryBuilder<'_>,
) -> Result<Vec<T>, BaseError>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::mysql::MySqlRow> + Send + Unpin,
{
    mysql_transaction(transaction)?
        .select_for_update(query)
        .await
        .map_err(BaseError::from)
}
