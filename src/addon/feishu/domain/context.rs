//! 飞书 addon 的模块上下文：聚合两张表的 Repository，并提供事务收尾。

//! # 临时豁免
//!
//! `#![allow(dead_code)]` 是**临时**的：本模块要被尚未落地的端点与写入 API 消费。
//! 它们提交时**必须删除这一行**。
#![allow(dead_code)]
use yang_base::BaseError;
use yang_db::Transaction;

use super::repository::Repository;

/// addon 级共享上下文。
///
/// 两张表的 Repository 都在这里持有——`Registry::dispatch` 只向 Action 注入**所在
/// module 的主表**，所以跨表访问（选项 module 读数据源表）必须经这个上下文。
#[derive(Clone)]
pub(crate) struct FeishuContext {
    datasource: Repository,
    option: Repository,
}

impl FeishuContext {
    /// 构造上下文。
    pub(crate) fn new(datasource: Repository, option: Repository) -> Self {
        Self { datasource, option }
    }

    /// 数据源表。
    pub(crate) fn datasources(&self) -> &Repository {
        &self.datasource
    }

    /// 选项表。
    pub(crate) fn options(&self) -> &Repository {
        &self.option
    }

    /// 事务收尾：成功提交、失败回滚。
    ///
    /// 回滚失败只记日志、不覆盖原错误——业务错误比回滚错误更值得冒泡。
    /// 写成固有关联函数（而不是 `Transaction` 的方法）是仓库既有惯例：
    /// `Transaction` 的 `commit`/`rollback` 都消耗所有权，这里统一收口一次。
    pub(crate) async fn finish_transaction<T>(
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
                    tracing::error!(error = %rollback_error, "飞书 addon 事务回滚失败");
                }
                Err(error)
            }
        }
    }
}
