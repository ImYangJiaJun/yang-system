//! 飞书 addon 的模块上下文：聚合全部表的 Repository 与集成配置，并提供事务收尾。

use std::sync::Arc;

use yang_base::action::auth::PasswordEngine;
use yang_base::BaseError;
use yang_db::Transaction;

use crate::config::FeishuSettings;

use super::repository::Repository;

/// addon 级共享上下文。
///
/// 九张表的 Repository 都在这里持有——`Registry::dispatch` 只向 Action 注入**所在
/// module 的主表**，所以跨表访问（选项 module 读数据源表、审批派发读五张审批表）
/// 必须经这个上下文。
#[derive(Clone)]
pub(crate) struct FeishuContext {
    datasource: Repository,
    // 消费者（表级数据源的 Action）在后续批次接入。仓库既有先例：
    // `domain/bitable.rs:29`、`domain/outbound.rs:30` 同样先落地能力、后接消费者。
    #[allow(dead_code)]
    datasource_field: Repository,
    option: Repository,
    /// 审批派发配置表。
    #[allow(dead_code)]
    approval_config: Repository,
    /// 审批字段映射表。
    #[allow(dead_code)]
    approval_field_map: Repository,
    /// 审批认领队列表。
    #[allow(dead_code)]
    approval_task: Repository,
    /// 审批聚合组表（组实例身份：uuid / instance_code / effect_phase）。
    #[allow(dead_code)]
    approval_group: Repository,
    /// 审批派发请求记录表。
    #[allow(dead_code)]
    approval_request_log: Repository,
    /// 审批任务发起人表（第二张 task 族表，存解析后的 open_id）。
    #[allow(dead_code)]
    approval_task_applicant: Repository,
    /// 飞书用户身份表（OAuth user_access_token 的单例行）。
    #[allow(dead_code)]
    user_token: Repository,
    /// 查询门户表。
    #[allow(dead_code)]
    portal: Repository,
    /// 查询门户访问日志表。
    #[allow(dead_code)]
    portal_log: Repository,
    /// 审批事件落地表（store-then-process 缓冲）。
    #[allow(dead_code)]
    approval_event: Repository,
    /// 密码哈希引擎（门户口令用）。
    #[allow(dead_code)]
    password_engine: Arc<PasswordEngine>,
    settings: Option<Arc<FeishuSettings>>,
    /// probe 与导入共享容量，成功解析后的写库阶段也占用这一份预算。
    xlsx_parse_slots: Arc<tokio::sync::Semaphore>,
}

impl FeishuContext {
    /// 构造上下文。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        datasource: Repository,
        datasource_field: Repository,
        option: Repository,
        approval_config: Repository,
        approval_field_map: Repository,
        approval_task: Repository,
        approval_group: Repository,
        approval_request_log: Repository,
        approval_task_applicant: Repository,
        user_token: Repository,
        portal: Repository,
        portal_log: Repository,
        approval_event: Repository,
        password_engine: Arc<PasswordEngine>,
        settings: Option<Arc<FeishuSettings>>,
    ) -> Self {
        Self {
            datasource,
            datasource_field,
            option,
            approval_config,
            approval_field_map,
            approval_task,
            approval_group,
            approval_request_log,
            approval_task_applicant,
            user_token,
            portal,
            portal_log,
            approval_event,
            password_engine,
            settings,
            xlsx_parse_slots: Arc::new(tokio::sync::Semaphore::new(1)),
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

    /// 审批派发配置表。
    ///
    /// 消费者（dispatch Action 与派发 worker）在后续任务接入。
    #[allow(dead_code)]
    pub(crate) fn approval_configs(&self) -> &Repository {
        &self.approval_config
    }

    /// 审批字段映射表。消费者同上。
    #[allow(dead_code)]
    pub(crate) fn approval_field_maps(&self) -> &Repository {
        &self.approval_field_map
    }

    /// 审批认领队列表。消费者同上。
    #[allow(dead_code)]
    pub(crate) fn approval_tasks(&self) -> &Repository {
        &self.approval_task
    }

    /// 审批聚合组表（组实例身份的唯一落点）。
    ///
    /// 消费者（组维护、组处理、事件 fan-out）在后续任务接入。
    #[allow(dead_code)]
    pub(crate) fn approval_groups(&self) -> &Repository {
        &self.approval_group
    }

    /// 审批派发请求记录表。消费者（dispatch 落记录）在后续任务接入。
    #[allow(dead_code)]
    pub(crate) fn approval_request_logs(&self) -> &Repository {
        &self.approval_request_log
    }

    /// 审批任务发起人表。消费者（dispatch Action 与 worker）在后续任务接入。
    #[allow(dead_code)]
    pub(crate) fn approval_task_applicants(&self) -> &Repository {
        &self.approval_task_applicant
    }

    /// 飞书用户身份表（OAuth 单例行）。消费者：派发姓名解析、提醒 worker。
    #[allow(dead_code)]
    pub(crate) fn user_tokens(&self) -> &Repository {
        &self.user_token
    }

    /// 查询门户表。消费者（门户 Action）在后续批次接入。
    #[allow(dead_code)]
    pub(crate) fn portals(&self) -> &Repository {
        &self.portal
    }

    /// 查询门户访问日志表。消费者同上。
    #[allow(dead_code)]
    pub(crate) fn portal_logs(&self) -> &Repository {
        &self.portal_log
    }

    /// 审批事件落地表。消费者（事件处理器）在后续批次接入。
    #[allow(dead_code)]
    pub(crate) fn approval_events(&self) -> &Repository {
        &self.approval_event
    }

    /// 密码哈希引擎（门户口令用）。消费者（门户 Action）在后续批次接入。
    #[allow(dead_code)]
    pub(crate) fn password_engine(&self) -> &PasswordEngine {
        &self.password_engine
    }

    /// 集成配置；`None` 表示 `[feishu]` 段缺席。
    pub(crate) fn settings(&self) -> Option<&FeishuSettings> {
        self.settings.as_deref()
    }

    pub(crate) fn acquire_xlsx_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, BaseError> {
        Arc::clone(&self.xlsx_parse_slots)
            .try_acquire_owned()
            .map_err(|_| BaseError::RateLimitExceeded {
                retry_after_seconds: 1,
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 组表仓储必须经上下文可访问，且绑定的确实是组表声明。
    ///
    /// 新增表最常漏的是「构造点没跟上」——那样会直接编译不过；这里再确认
    /// 上下文实际接到的是 `feishu_approval_group` 的字段集（用一个不存在的列
    /// 验证声明边界），而不是别的表。
    #[tokio::test]
    async fn approval_groups_repository_is_reachable_from_the_context() {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = |spec: yang_base::definition::TableSpec| {
            spec.table_definition()
                .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
        };
        let repository = |spec: Result<yang_base::definition::TableSpec, _>| {
            Repository::new(
                definition(spec.unwrap_or_else(|error| panic!("{error}"))),
                Arc::clone(&pool),
            )
        };
        let context = FeishuContext::new(
            repository(crate::addon::feishu::datasource::table::table_spec()),
            repository(crate::addon::feishu::datasource::domain::field_table::table_spec()),
            repository(crate::addon::feishu::option::table::table_spec()),
            repository(crate::addon::feishu::approval::table::table_spec()),
            repository(crate::addon::feishu::approval::domain::field_map_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::group_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::request_log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_applicant_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::user_token_table::table_spec()),
            repository(crate::addon::feishu::portal::table::table_spec()),
            repository(crate::addon::feishu::portal::domain::log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::event_table::table_spec()),
            Arc::new(
                PasswordEngine::new(1)
                    .unwrap_or_else(|error| panic!("PasswordEngine 应可构造: {error}")),
            ),
            None,
        );
        assert!(
            context
                .approval_groups()
                .query()
                .select_fields(&["uuid"])
                .is_ok(),
            "组表仓储应可发起查询"
        );
        assert!(
            context
                .approval_groups()
                .query()
                .select_fields(&["no_such_column"])
                .is_err(),
            "未知列必须被组表声明拒绝（确认绑定的确实是组表）"
        );
    }
}
