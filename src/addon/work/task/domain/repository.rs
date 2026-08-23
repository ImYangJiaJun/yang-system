//! 任务关系一致性读取边界。
//! raw-sql-boundary: domain-repository work-task-repository

use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::addon::work::task) struct TaskLinks {
    pub(in crate::addon::work::task) project_id: i64,
    pub(in crate::addon::work::task) parent_id: Option<i64>,
}

pub(in crate::addon::work::task) async fn lock_workspace(
    context: &ActionContext,
    transaction: &mut Transaction,
) -> Result<(), BaseError> {
    let owner = context.tenant()?.id().get();
    // 个人任务的关系写入以用户行为串行化，避免两个并发移动分别通过环检测。
    // tenant-boundary: database work-task-workspace-lock-database
    let pool = context.tools().mysql()?.pool();
    let locked_owner = transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("users"))
                .field(field!("id"))
                .where_and(field!("id"), CompareOp::Eq, owner),
        )
        .await?
        .into_iter()
        .next()
        .map(|(id,): (i64,)| id);
    if locked_owner != Some(owner) {
        return Err(BaseError::PermissionDenied(
            "个人工作区不存在或已失效".to_string(),
        ));
    }
    Ok(())
}

pub(in crate::addon::work::task) async fn current_links_in_tx(
    context: &ActionContext,
    transaction: &mut Transaction,
    task_id: i64,
) -> Result<TaskLinks, BaseError> {
    let owner = context.tenant()?.id().get();
    // tenant-boundary: database work-task-current-links-database
    let pool = context.tools().mysql()?.pool();
    let row: (i64, Option<i64>) = transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("work_task"))
                .field(field!("project_project"))
                .field(field!("parent_task"))
                .where_and(field!("id"), CompareOp::Eq, task_id)
                .where_and(field!("owner_user"), CompareOp::Eq, owner),
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::RecordNotFound(format!("任务 {task_id}")))?;
    Ok(TaskLinks {
        project_id: row.0,
        parent_id: row.1,
    })
}

pub(in crate::addon::work::task) async fn validate_task_links_in_tx(
    context: &ActionContext,
    transaction: &mut Transaction,
    project_id: i64,
    parent_id: Option<i64>,
    task_id: Option<i64>,
) -> Result<(), BaseError> {
    if project_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "project_project".to_string(),
            "必须是正整数".to_string(),
        ));
    }
    let owner = context.tenant()?.id().get();
    // tenant-boundary: database work-task-links-validation-database
    let pool = context.tools().mysql()?.pool();
    let locked_project = transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("work_project"))
                .field(field!("id"))
                .where_and(field!("id"), CompareOp::Eq, project_id)
                .where_and(field!("owner_user"), CompareOp::Eq, owner),
        )
        .await?
        .into_iter()
        .next()
        .map(|(id,): (i64,)| id);
    if locked_project != Some(project_id) {
        return Err(BaseError::PermissionDenied(
            "所属项目不存在或不属于当前工作区".to_string(),
        ));
    }

    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if parent_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "parent_task".to_string(),
            "必须是正整数".to_string(),
        ));
    }
    if task_id == Some(parent_id) {
        return Err(BaseError::ParamInvalid(
            "parent_task".to_string(),
            "任务不能成为自己的父任务".to_string(),
        ));
    }
    let parent_project = transaction
        .select_for_update(
            QueryBuilder::from_pool(pool, table!("work_task"))
                .field(field!("project_project"))
                .where_and(field!("id"), CompareOp::Eq, parent_id)
                .where_and(field!("owner_user"), CompareOp::Eq, owner),
        )
        .await?
        .into_iter()
        .next()
        .map(|(project,): (i64,)| project);
    if parent_project != Some(project_id) {
        return Err(BaseError::PermissionDenied(
            "父任务不存在、跨工作区或不属于同一项目".to_string(),
        ));
    }

    if let Some(task_id) = task_id {
        // tenant-boundary: raw-sql work-task-cycle-check
        let creates_cycle: bool = sqlx::query_scalar(
            "WITH RECURSIVE ancestors AS (\
                 SELECT id, parent_task, 1 AS depth FROM work_task \
                 WHERE id = ? AND owner_user = ? \
                 UNION ALL \
                 SELECT task.id, task.parent_task, ancestors.depth + 1 \
                 FROM work_task AS task \
                 INNER JOIN ancestors ON task.id = ancestors.parent_task \
                 WHERE task.owner_user = ? AND ancestors.depth < 100\
             ) \
             SELECT EXISTS(SELECT 1 FROM ancestors WHERE id = ?)",
        )
        .bind(parent_id)
        .bind(owner)
        .bind(owner)
        .bind(task_id)
        .fetch_one(executor(transaction)?)
        .await
        .map_err(yang_db::DbError::from)?;
        if creates_cycle {
            return Err(BaseError::ParamInvalid(
                "parent_task".to_string(),
                "父任务关系会形成环".to_string(),
            ));
        }
    }
    Ok(())
}

pub(in crate::addon::work::task) async fn finish_transaction<T>(
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
                tracing::error!("个人任务 writer 回滚失败: error={}", rollback_error);
            }
            Err(error)
        }
    }
}

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "个人任务 writer 事务已结束".to_string(),
        ))
    })
}

pub(in crate::addon::work::task) async fn lock_tasks_for_completion(
    context: &ActionContext,
    ids: &[i64],
    transaction: &mut Transaction,
) -> Result<(), BaseError> {
    let owner = context.tenant()?.id().get();
    let ids_json =
        serde_json::Value::Array(ids.iter().copied().map(Into::into).collect()).to_string();
    let executor = transaction.executor().ok_or_else(|| {
        BaseError::DatabaseTransactionFailed(yang_db::DbError::TransactionError(
            "批量完成事务已结束".to_string(),
        ))
    })?;
    // tenant-boundary: raw-sql work-task-complete-lock
    let locked: Vec<i64> = sqlx::query_scalar(
        "SELECT task.id FROM work_task AS task \
         INNER JOIN JSON_TABLE(?, '$[*]' COLUMNS(selected_id BIGINT PATH '$')) AS selected \
             ON selected.selected_id = task.id \
         WHERE task.owner_user = ? \
         ORDER BY task.id FOR UPDATE OF task",
    )
    .bind(ids_json)
    .bind(owner)
    .fetch_all(executor)
    .await
    .map_err(yang_db::DbError::from)?;
    if locked.len() != ids.len() {
        return Err(BaseError::PermissionDenied(
            "批量选择包含不存在或不属于当前工作区的任务".to_string(),
        ));
    }
    Ok(())
}
