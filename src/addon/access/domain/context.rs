//! access 模块上下文 `Access`：授权存储、权限目录与事务收尾的单一出口。
//!
//! 业务用例流程内联在各 Action 文件的 `handle` 中；Action 只从 `Access`
//! 获取能力，与 account 的 `Account` 上下文同构。

use super::permission_catalog::PermissionCatalogHandle;
use super::repository::GrantRepository;
use crate::authorization::AuthorizationPort;
use yang_base::BaseError;
use yang_db::Transaction;

/// access 模块上下文：聚合授权存储、权限目录投影与授权失效公共端口。
pub(crate) struct Access {
    grants: GrantRepository,
    permission_catalog: PermissionCatalogHandle,
    authorization: AuthorizationPort,
}

impl Access {
    pub(crate) fn new(
        grants: GrantRepository,
        permission_catalog: PermissionCatalogHandle,
        authorization: AuthorizationPort,
    ) -> Self {
        Self {
            grants,
            permission_catalog,
            authorization,
        }
    }

    /// 授权事实表的唯一持久化边界。
    pub(crate) fn grants(&self) -> &GrantRepository {
        &self.grants
    }

    /// 运行期权限目录投影（决策 D3：Catalog 是唯一事实来源）。
    pub(crate) fn permission_catalog(&self) -> &PermissionCatalogHandle {
        &self.permission_catalog
    }

    /// 授权失效公共端口：授权事实变更后在同事务使目标用户 Access Token 失效。
    pub(crate) fn authorization(&self) -> &AuthorizationPort {
        &self.authorization
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
                    tracing::error!(error = %rollback_error, "access 用例事务回滚失败");
                }
                Err(error)
            }
        }
    }
}
