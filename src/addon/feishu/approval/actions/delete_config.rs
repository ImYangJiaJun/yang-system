//! 删除审批派发配置（前端控制台）。
//!
//! # 同事务清理三件事
//!
//! 配置行 + 它的字段映射行 + **该配置的全部任务行**。任务表以全局唯一的
//! `record_id` 作并发去重键；若删除配置时保留已完结任务，重建配置后对同一记录
//! 重新派发会撞上旧的 `record_id` 唯一键，播种永久卡住（feishu-approval#31）。
//! 任务流水属于已删除的配置，因此这里一并清掉；配置的请求流水仍保留在
//! `feishu_approval_request_log`。
//!
//! # 保留未完成任务的恢复证据
//!
//! 待执行、在途或未知结果任务阻止删除。删除与持久受理锁同一配置行，
//! 避免检查后插入任务；未知远端写入不能通过删除重建绕过人工核查。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_aggregation::repository as aggregation;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;
use std::collections::HashMap;

/// 删除配置的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteConfigInput {
    /// 目标配置的 `id`。
    pub(super) config_id: i64,
}

impl ParamInput for DeleteConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteConfigInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "config_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册删除配置端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_config"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/delete")
        .display_name("删除审批派发配置")
        .description("无待执行、在途或待核查任务时删除配置、字段映射与任务行；请求流水保留")
        .permissions(["feishu.approval.write"])
        .register()
}

/// 删除配置时在一个事务内完成的清理摘要（审计与响应同源）。
#[derive(Debug, Default)]
struct DeletionSummary {
    deleted_field_maps: u64,
    deleted_pending_tasks: u64,
    deleted_tasks: u64,
    deleted_applicants: u64,
    deleted_groups: u64,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 守卫、删配置、删映射、删任务与组行、写审计：同一事务，缺一不可。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<DeletionSummary, BaseError> = async {
        let summary = delete_config_in_tx(&mut transaction, &context, input.config_id).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", input.config_id.to_string())?,
            None,
            Some(audit::summary([
                ("config_id", serde_json::json!(input.config_id)),
                (
                    "deleted_field_maps",
                    serde_json::json!(summary.deleted_field_maps),
                ),
                (
                    "deleted_pending_tasks",
                    serde_json::json!(summary.deleted_pending_tasks),
                ),
                ("deleted_tasks", serde_json::json!(summary.deleted_tasks)),
                (
                    "deleted_applicants",
                    serde_json::json!(summary.deleted_applicants),
                ),
                ("deleted_groups", serde_json::json!(summary.deleted_groups)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(summary)
    }
    .await;
    let summary = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "config_id": input.config_id,
            "deleted_field_maps": summary.deleted_field_maps,
            "deleted_pending_tasks": summary.deleted_pending_tasks,
            "deleted_tasks": summary.deleted_tasks,
            "deleted_applicants": summary.deleted_applicants,
            "deleted_groups": summary.deleted_groups,
        }),
        "配置已删除",
    )
}

/// 在调用方事务内执行删除判定与全部清理（不提交）。
///
/// # 为什么拆出来
///
/// 「暂停组何时阻塞删除」只有在真实库上才验得出来，而 `handle` 需要
/// `ActionContext`（JWT、审计与工具资源），单测构造不出来。
///
/// # 暂停组的判定
///
/// `paused` 进入活跃白名单，但**未提交**的暂停组（组行没有 `instance_code`）不再
/// 阻塞删除：它没有绑定任何外部事实，随配置一起清理是安全且必要的（否则暂停组会
/// 永久卡住配置删除）。已建实例的暂停组仍算在途——删了配置就留下孤儿审批（设计
/// §十）。组行在**本事务内** `FOR UPDATE` 读取，避免「读到无实例 → 同时被确认创建
/// → 仍按无实例删除」。
async fn delete_config_in_tx(
    transaction: &mut crate::db::Transaction,
    context: &FeishuContext,
    config_id: i64,
) -> Result<DeletionSummary, BaseError> {
    // 与受理事务锁同一配置行，避免检查通过后又插入一个在途任务。
    crate::db::select_for_update::<yang_base::table::Record>(
        transaction,
        yang_db::QueryBuilder::from_pool(
            context.approval_configs().pool(),
            yang_db::table!("feishu_approval_config"),
        )
        .fields(&[yang_db::field!("id")])
        .where_and(yang_db::field!("id"), yang_db::CompareOp::Eq, config_id)
        .map_err(BaseError::from)?,
    )
    .await?;
    let active = context
        .approval_tasks()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_in(
            "state",
            ["pending", "creating", "created", "review", "paused"]
                .into_iter()
                .map(serde_json::Value::from)
                .collect(),
        )?
        .all_in_tx(transaction)
        .await?;
    if !active.is_empty() {
        let group_instances = lock_group_instances_in_tx(transaction, context, config_id).await?;
        for row in &active {
            let state: String = row.require("state")?;
            let group_key: Option<String> = row.optional("group_id")?;
            // 只有「暂停 + 属于未提交组」才放行；其余一律是恢复证据。
            let unsubmitted_paused = state == "paused"
                && group_key
                    .as_deref()
                    .is_some_and(|key| group_instances.get(key) == Some(&false));
            if !unsubmitted_paused {
                return Err(BaseError::ParamInvalid(
                    "config_id".into(),
                    "配置仍有待执行、在途或结果未知任务；请先完成或核查，不能删除恢复证据".into(),
                ));
            }
        }
    }
    let deleted_maps = context
        .approval_field_maps()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .delete_in_tx(transaction)
        .await?;
    // 先收掉任务的属性扩展行：task_applicant 没有外键，不主动清会留下
    // 指向已删除任务（乃至已删除配置）的孤儿 PII。
    let task_ids: Vec<i64> = context
        .approval_tasks()
        .query()
        .select_fields(&["id"])?
        .where_eq("config_id", serde_json::json!(config_id))?
        .all_in_tx(transaction)
        .await?
        .iter()
        .map(|record| record.require("id"))
        .collect::<Result<_, BaseError>>()?;
    let deleted_applicants = if task_ids.is_empty() {
        0
    } else {
        let values: Vec<serde_json::Value> =
            task_ids.iter().map(|id| serde_json::json!(id)).collect();
        context
            .approval_task_applicants()
            .query()
            .where_in("task_id", values)?
            .delete_in_tx(transaction)
            .await?
    };
    let deleted_pending_tasks = context
        .approval_tasks()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("state", serde_json::json!("pending"))?
        .delete_in_tx(transaction)
        .await?;
    // 已完结任务也必须清：task 表的 record_id 是全局唯一键，保留旧配置的
    // 完结行会让重建配置后的同记录播种永久卡住（#31）。请求流水仍在
    // feishu_approval_request_log，不受影响。
    let deleted_finished_tasks = context
        .approval_tasks()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .delete_in_tx(transaction)
        .await?;
    let deleted_tasks = deleted_pending_tasks + deleted_finished_tasks;
    let deleted_groups =
        aggregation::delete_groups_for_config_in_tx(transaction, context, config_id)
            .await
            .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let deleted_configs = context
        .approval_configs()
        .query()
        .where_eq("id", serde_json::json!(config_id))?
        .delete_in_tx(transaction)
        .await?;
    if deleted_configs == 0 {
        return Err(BaseError::RecordNotFound("审批派发配置不存在".to_string()));
    }
    Ok(DeletionSummary {
        deleted_field_maps: deleted_maps,
        deleted_pending_tasks,
        deleted_tasks,
        deleted_applicants,
        deleted_groups,
    })
}

/// 事务内锁住本配置的组行，返回 `组键 → 是否已有实例码`。
async fn lock_group_instances_in_tx(
    transaction: &mut crate::db::Transaction,
    context: &FeishuContext,
    config_id: i64,
) -> Result<HashMap<String, bool>, BaseError> {
    let rows = crate::db::select_for_update::<yang_base::table::Record>(
        transaction,
        yang_db::QueryBuilder::from_pool(
            context.approval_groups().pool(),
            yang_db::table!("feishu_approval_group"),
        )
        .fields(&[
            yang_db::field!("group_key"),
            yang_db::field!("instance_code"),
        ])
        .where_and(
            yang_db::field!("config_id"),
            yang_db::CompareOp::Eq,
            config_id,
        )
        .map_err(BaseError::from)?,
    )
    .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let key: String = row.require("group_key")?;
        let instance: Option<String> = row.optional("instance_code")?;
        map.insert(key, instance.is_some_and(|value| !value.trim().is_empty()));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_id_is_rejected() {
        let input = DeleteConfigInput { config_id: 0 };
        assert!(input.validate().is_err());
    }

    #[test]
    fn positive_config_id_is_accepted() {
        let input = DeleteConfigInput { config_id: 42 };
        assert!(input.validate().is_ok());
    }

    /// 真实库 + 同 schema 的上下文（测试只碰 MySQL，不碰 Redis/飞书）。
    async fn real_mysql_database_and_context(
    ) -> anyhow::Result<(yang_db::Database, Arc<FeishuContext>)> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许独立测试库");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let database = yang_db::Database::connect(&url).await?;
        let context = crate::addon::feishu::build_context(
            Arc::new(database.pool().clone()),
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        Ok((database, context))
    }

    async fn insert_config(context: &FeishuContext, namespace: &str) -> anyhow::Result<i64> {
        Ok(sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('删除测试','appDelete',?,'DEF-DELETE','fldPerson','fldNumber','Asia/Shanghai',true,NOW(),NOW())")
            .bind(format!("tbl{namespace}"))
            .execute(context.approval_configs().pool())
            .await?
            .last_insert_id() as i64)
    }

    async fn insert_group_row(
        context: &FeishuContext,
        config_id: i64,
        group_key: &str,
        instance_code: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO feishu_approval_group (config_id,group_key,uuid,signature_hash,effect_phase,instance_code,created_at,updated_at) VALUES (?,?,?,?,?,?,NOW(),NOW())")
            .bind(config_id)
            .bind(group_key)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind("sig")
            .bind(if instance_code.is_some() { "created" } else { "ready" })
            .bind(instance_code)
            .execute(context.approval_groups().pool())
            .await?;
        Ok(())
    }

    async fn insert_task_row(
        context: &FeishuContext,
        config_id: i64,
        group_key: &str,
        seq: i64,
        record_id: &str,
        state: &str,
    ) -> anyhow::Result<i64> {
        let task_id = sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,group_id,group_seq,created_at,updated_at) VALUES (?,?,?,?,?,?,NOW(),NOW())")
            .bind(config_id)
            .bind(record_id)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(state)
            .bind(group_key)
            .bind(seq)
            .execute(context.approval_tasks().pool())
            .await?
            .last_insert_id() as i64;
        sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?, 'ou_delete_test')")
            .bind(task_id)
            .execute(context.approval_task_applicants().pool())
            .await?;
        Ok(task_id)
    }

    async fn count_rows(
        context: &FeishuContext,
        config_id: i64,
    ) -> anyhow::Result<(i64, i64, i64, i64)> {
        let configs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_config WHERE id=?")
                .bind(config_id)
                .fetch_one(context.approval_configs().pool())
                .await?;
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(context.approval_groups().pool())
                .await?;
        let tasks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE config_id=?")
                .bind(config_id)
                .fetch_one(context.approval_tasks().pool())
                .await?;
        let applicants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task_applicant a JOIN feishu_approval_task t ON t.id=a.task_id WHERE t.config_id=?")
            .bind(config_id)
            .fetch_one(context.approval_task_applicants().pool())
            .await?;
        Ok((configs, groups, tasks, applicants))
    }

    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库"]
    async fn delete_config_removes_group_rows_members_and_applicants() -> anyhow::Result<()> {
        let (database, context) = real_mysql_database_and_context().await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_config(&context, &namespace).await?;
        let group_key = format!("gk{namespace}");
        insert_group_row(&context, config_id, &group_key, Some("INST-DELETE")).await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            0,
            &format!("{namespace}-leader"),
            "backfilled",
        )
        .await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            1,
            &format!("{namespace}-member"),
            "backfilled",
        )
        .await?;

        let mut transaction = crate::db::begin_mysql(&database).await?;
        let summary = delete_config_in_tx(&mut transaction, &context, config_id).await?;
        transaction.commit().await?;
        assert_eq!(summary.deleted_groups, 1);
        assert_eq!(summary.deleted_tasks, 2);
        assert_eq!(summary.deleted_applicants, 2);
        assert_eq!(
            count_rows(&context, config_id).await?,
            (0, 0, 0, 0),
            "配置、组行、任务行与 applicant 行都必须清干净"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库"]
    async fn delete_config_allows_unsubmitted_paused_group() -> anyhow::Result<()> {
        let (database, context) = real_mysql_database_and_context().await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_config(&context, &namespace).await?;
        let group_key = format!("gk{namespace}");
        // 暂停组且组行没有实例码：没有绑定任何外部事实，可以随配置一起清理。
        insert_group_row(&context, config_id, &group_key, None).await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            0,
            &format!("{namespace}-leader"),
            "paused",
        )
        .await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            1,
            &format!("{namespace}-member"),
            "paused",
        )
        .await?;

        let mut transaction = crate::db::begin_mysql(&database).await?;
        let summary = delete_config_in_tx(&mut transaction, &context, config_id).await?;
        transaction.commit().await?;
        assert_eq!(summary.deleted_groups, 1);
        assert_eq!(summary.deleted_tasks, 2);
        assert_eq!(count_rows(&context, config_id).await?, (0, 0, 0, 0));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库"]
    async fn delete_config_blocks_paused_group_with_instance() -> anyhow::Result<()> {
        let (database, context) = real_mysql_database_and_context().await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_config(&context, &namespace).await?;
        let group_key = format!("gk{namespace}");
        // 已建实例的暂停组仍算在途：删配置会留下孤儿审批。
        insert_group_row(&context, config_id, &group_key, Some("INST-BLOCK")).await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            0,
            &format!("{namespace}-leader"),
            "paused",
        )
        .await?;
        insert_task_row(
            &context,
            config_id,
            &group_key,
            1,
            &format!("{namespace}-member"),
            "paused",
        )
        .await?;

        let mut transaction = crate::db::begin_mysql(&database).await?;
        let result = delete_config_in_tx(&mut transaction, &context, config_id).await;
        transaction.rollback().await?;
        assert!(
            matches!(result, Err(BaseError::ParamInvalid(_, _))),
            "已建实例的暂停组必须阻止删除: {result:?}"
        );
        assert_eq!(
            count_rows(&context, config_id).await?,
            (1, 1, 2, 2),
            "拒绝删除不得丢失任何恢复证据"
        );
        Ok(())
    }
}
