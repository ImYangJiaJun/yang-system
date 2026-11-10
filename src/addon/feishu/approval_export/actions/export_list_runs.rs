//! 审批回写运行记录列表（前端控制台）。
//!
//! 规则级行（`row_key=__rule__`）与行级行（`row_key=row:N`）同表，本端点按
//! 规则 / 实例 / 状态过滤，投影写入与标记两个状态机的结果。只读本库，不打飞书。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    10
}

/// 运行记录查询输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListExportRunsInput {
    #[serde(default = "default_page")]
    pub(super) page: u32,
    #[serde(default = "default_page_size")]
    pub(super) page_size: u32,
    #[serde(default)]
    pub(super) count_total: bool,
    /// 按规则过滤。
    #[serde(default)]
    pub(super) rule_id: Option<i64>,
    /// 按审批实例过滤。
    #[serde(default)]
    pub(super) instance_code: Option<String>,
    /// 按写入状态过滤（`queued` / `written` / `retry` / `review` / …）。
    #[serde(default)]
    pub(super) write_status: Option<String>,
    /// 按标记状态过滤（`none` / `valid` / `rolled_back` / …）。
    #[serde(default)]
    pub(super) mark_status: Option<String>,
    /// 只看规则级行（`__rule__`）或某条行级行。
    #[serde(default)]
    pub(super) row_key: Option<String>,
}

impl ParamInput for ListExportRunsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListExportRunsInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.page == 0 || self.page_size == 0 || self.page_size > 100 {
            return Err(BaseError::ParamInvalid(
                "page/page_size".to_string(),
                "page>=1, 1<=page_size<=100".to_string(),
            ));
        }
        Ok(())
    }
}

/// 运行行投影列。
const RUN_ITEM_COLUMNS: &[&str] = &[
    "id",
    "rule_id",
    "approval_code",
    "instance_code",
    "trigger_node_id",
    "row_key",
    "generation",
    "write_status",
    "mark_status",
    "applied_label",
    "target_record_id",
    "attempts",
    "next_attempt_at",
    "written_at",
    "marked_at",
    "last_error",
    "updated_at",
];

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ExportRunItem {
    id: i64,
    rule_id: i64,
    approval_code: String,
    instance_code: String,
    /// 规则级行固定 `__rule__`；行级行为触发节点 id。
    trigger_node_id: String,
    /// 规则级行固定 `__rule__`；行级行为 `row:N`。
    row_key: String,
    /// 强制重建的代次；正常写入为 0。
    generation: i64,
    write_status: String,
    mark_status: String,
    /// 已写进目标状态列的文案；`None` = 还没写过。
    applied_label: Option<String>,
    /// 目标多维表格记录 id；`None` = 尚未写入或已强制重建。
    target_record_id: Option<String>,
    attempts: i64,
    next_attempt_at: Option<i64>,
    written_at: Option<i64>,
    marked_at: Option<i64>,
    last_error: Option<String>,
    updated_at: i64,
}

/// 注册运行记录列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_list_runs"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/runs/query",
        )
        .display_name("审批回写运行记录")
        .description("分页查询回写运行台账，可按规则、实例、写入/标记状态过滤")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListExportRunsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context
        .approval_export_runs()
        .query()
        .select_fields(RUN_ITEM_COLUMNS)?;
    if let Some(rule_id) = input.rule_id {
        query = query.where_eq("rule_id", serde_json::json!(rule_id))?;
    }
    if let Some(instance_code) = input
        .instance_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("instance_code", serde_json::json!(instance_code))?;
    }
    if let Some(status) = input
        .write_status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("write_status", serde_json::json!(status))?;
    }
    if let Some(status) = input
        .mark_status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("mark_status", serde_json::json!(status))?;
    }
    if let Some(row_key) = input
        .row_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("row_key", serde_json::json!(row_key))?;
    }

    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    let rows = query
        .order_by("updated_at", SortOrder::Desc)?
        .order_by("id", SortOrder::Desc)?
        .page(input.page as usize, input.page_size as usize)?
        .all()
        .await?;

    let items: Vec<ExportRunItem> = rows
        .iter()
        .map(|record| {
            Ok(ExportRunItem {
                id: record.require("id")?,
                rule_id: record.require("rule_id")?,
                approval_code: record.require("approval_code")?,
                instance_code: record.require("instance_code")?,
                trigger_node_id: record.require("trigger_node_id")?,
                row_key: record.require("row_key")?,
                generation: record.optional("generation")?.unwrap_or(0),
                write_status: record.require("write_status")?,
                mark_status: record.require("mark_status")?,
                applied_label: record.optional("applied_label")?,
                target_record_id: record.optional("target_record_id")?,
                attempts: record.optional("attempts")?.unwrap_or(0),
                next_attempt_at: record.optional("next_attempt_at")?,
                written_at: record.optional("written_at")?,
                marked_at: record.optional("marked_at")?,
                last_error: record.optional("last_error")?,
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

    #[test]
    fn paging_is_validated() {
        for (page, page_size) in [(0u32, 10u32), (1, 0), (1, 101)] {
            let input = ListExportRunsInput {
                page,
                page_size,
                count_total: false,
                rule_id: None,
                instance_code: None,
                write_status: None,
                mark_status: None,
                row_key: None,
            };
            assert!(
                input.validate().is_err(),
                "page={page} page_size={page_size}"
            );
        }
    }

    #[test]
    fn filters_are_optional() {
        let input = serde_json::from_value::<ListExportRunsInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((input.page, input.page_size), (1, 10));
        assert!(input.rule_id.is_none() && input.instance_code.is_none());
        assert!(input.validate().is_ok());
    }

    #[test]
    fn unknown_filter_keys_are_rejected() {
        assert!(
            serde_json::from_value::<ListExportRunsInput>(serde_json::json!({
                "status": "written"
            }))
            .is_err(),
            "状态过滤键是 write_status / mark_status，不静默忽略"
        );
    }
}
