//! 审批派发任务列表（前端控制台「批量受理下钻」视图）。
//!
//! 按 `config_id` / `state` 过滤（state 枚举：pending / creating / created /
//! backfilled / terminal），由通用 `ListInput` 的 `where` 树承载。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 任务行投影列。
pub(super) const TASK_ITEM_COLUMNS: &[&str] = &[
    "id",
    "config_id",
    "record_id",
    "state",
    "instance_code",
    "serial_number",
    "attempts",
    "last_error",
    "created_at",
    "updated_at",
];

/// 一条审批派发任务。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TaskItem {
    /// 主键。
    id: i64,
    /// 所属配置。
    config_id: i64,
    /// 多维表格记录 id。
    record_id: String,
    /// 状态：pending / creating / created / backfilled / terminal。
    state: String,
    /// 创建成功后的审批实例 code。
    instance_code: Option<String>,
    /// 审批单编号。
    serial_number: Option<String>,
    /// 已尝试次数。
    attempts: i64,
    /// 落库的失败原因（不回灌多维表格）。
    last_error: Option<String>,
    /// 创建时间（unix 秒）。
    created_at: i64,
    /// 更新时间（unix 秒）。
    updated_at: i64,
}

/// 注册任务列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list_tasks"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(HttpMethod::Post, "/api/v1/feishu/approval/tasks/query")
        .display_name("审批派发任务列表")
        .description("分页查询飞书审批派发任务，可按配置与状态过滤")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 同 list_requests：本表没有 `searchable` 字段，`TableQuery::search` 会直接
    // PermissionDenied，刻意不调用（通用 TableView 只会在目录声明 search_fields
    // 时发 search，本表为空）。
    let mut query = context
        .approval_tasks()
        .query()
        .select_fields(TASK_ITEM_COLUMNS)?;

    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    // 客户端没给排序时兜底：分页要有确定性全序，否则翻页会漏行或重复。
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        query = query.order_by("updated_at", SortOrder::Desc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    let items: Vec<TaskItem> = rows
        .iter()
        .map(|record| {
            Ok(TaskItem {
                id: record.require("id")?,
                config_id: record.require("config_id")?,
                record_id: record.require("record_id")?,
                state: record.require("state")?,
                instance_code: record.optional("instance_code")?,
                serial_number: record.optional("serial_number")?,
                attempts: record.require("attempts")?,
                last_error: record.optional("last_error")?,
                created_at: record.require("created_at")?,
                updated_at: record.require("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "items": items,
            "page": input.page,
            "page_size": input.page_size,
            "total": total,
        }),
        "查询成功",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 投影列清单与列表项序列化键逐一对账：多一列少一列都说明漂移。
    #[test]
    fn the_item_keys_match_the_projection() {
        let item = TaskItem {
            id: 1,
            config_id: 7,
            record_id: "rec001".to_string(),
            state: "backfilled".to_string(),
            instance_code: Some("instance-code-1".to_string()),
            serial_number: Some("202609280001".to_string()),
            attempts: 1,
            last_error: None,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        };
        let value =
            serde_json::to_value(&item).unwrap_or_else(|error| panic!("应可序列化: {error}"));
        let keys = value
            .as_object()
            .map(|object| {
                let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
                keys.sort_unstable();
                keys
            })
            .unwrap_or_default();
        let mut expected: Vec<&str> = TASK_ITEM_COLUMNS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "列表项键必须恰好等于投影列");
    }
}
