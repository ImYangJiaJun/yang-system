//! 飞书 addon 的模块上下文：聚合三张表的 Repository 与集成配置，并提供事务收尾。

use std::sync::Arc;

use yang_base::BaseError;
use yang_db::Transaction;

use crate::config::FeishuSettings;

use super::repository::Repository;

/// addon 级共享上下文。
///
/// 三张表的 Repository 都在这里持有——`Registry::dispatch` 只向 Action 注入**所在
/// module 的主表**，所以跨表访问（选项 module 读数据源表）必须经这个上下文。
#[derive(Clone)]
pub(crate) struct FeishuContext {
    datasource: Repository,
    // 消费者（表级数据源的 Action）在后续批次接入。仓库既有先例：
    // `domain/bitable.rs:29`、`domain/outbound.rs:30` 同样先落地能力、后接消费者。
    #[allow(dead_code)]
    datasource_field: Repository,
    option: Repository,
    settings: Option<Arc<FeishuSettings>>,
}

impl FeishuContext {
    /// 构造上下文。
    pub(crate) fn new(
        datasource: Repository,
        datasource_field: Repository,
        option: Repository,
        settings: Option<Arc<FeishuSettings>>,
    ) -> Self {
        Self {
            datasource,
            datasource_field,
            option,
            settings,
        }
    }

    /// 数据源表（表级）。
    pub(crate) fn datasources(&self) -> &Repository {
        &self.datasource
    }

    /// 数据源字段绑定表。
    ///
    /// 一条绑定 = 一个多维表格字段 = 一个 `source_key` = 一个审批控件；
    /// 级联的父指针也落在这一层。
    ///
    /// 消费者（表级数据源的 Action）在后续批次接入，故暂标 `dead_code`。
    #[allow(dead_code)]
    pub(crate) fn datasource_fields(&self) -> &Repository {
        &self.datasource_field
    }

    /// 选项表。
    pub(crate) fn options(&self) -> &Repository {
        &self.option
    }

    /// 集成配置；`None` 表示 `[feishu]` 段缺席。
    pub(crate) fn settings(&self) -> Option<&FeishuSettings> {
        self.settings.as_deref()
    }

    /// 外部选项接口的 AES 密钥；未配置 Key 时返回 `None`（表示明文返回）。
    pub(crate) fn encryption_key(&self) -> Option<[u8; 32]> {
        self.settings()
            .and_then(|settings| settings.encryption_key.as_deref())
            .map(super::crypto::derive_key)
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
