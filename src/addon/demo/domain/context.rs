//! demo 模块上下文 `Demo`：便签持久化边界与事务收尾的单一出口。
//!
//! 业务用例流程内联在各 Action 文件的 `handle` 中；Action 只从 `Demo`
//! 获取能力，与 access 的 `Access` 上下文同构。

use super::repository::NoteRepository;
use yang_base::BaseError;
use yang_db::Transaction;

/// demo 模块上下文：聚合便签持久化边界。
pub(crate) struct Demo {
    notes: NoteRepository,
}

impl Demo {
    pub(crate) fn new(notes: NoteRepository) -> Self {
        Self { notes }
    }

    /// 便签事实表的唯一持久化边界（所有权规则在 Repository 内收敛）。
    pub(crate) fn notes(&self) -> &NoteRepository {
        &self.notes
    }

    /// 提交或回滚一个业务事务，回滚失败只记录日志不覆盖原错误。
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
                    tracing::error!(error = %rollback_error, "demo 用例事务回滚失败");
                }
                Err(error)
            }
        }
    }
}
