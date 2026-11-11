//! 审批评论运行台账列表（前端控制台）。
//!
//! 只读本库，不打飞书。按规则 / 状态 / 目标记录过滤，投影单条评论回写的
//! 状态机结果。

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

/// 评论运行台账查询输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListCommentRunsInput {
    #[serde(default = "default_page")]
    pub(super) page: u32,
    #[serde(default = "default_page_size")]
    pub(super) page_size: u32,
    #[serde(default)]
    pub(super) count_total: bool,
    #[serde(default)]
    pub(super) rule_id: Option<i64>,
    /// pending / commenting / commented / done / retry / failed / skipped / waiting
    #[serde(default)]
    pub(super) state: Option<String>,
    #[serde(default)]
    pub(super) record_id: Option<String>,
}

impl ParamInput for ListCommentRunsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListCommentRunsInput {
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

const RUN_ITEM_COLUMNS: &[&str] = &[
    "id",
    "rule_id",
    "instance_code",
    "record_id",
    "state",
    "payment_fingerprint",
    "comment_id",
    "attempts",
    "next_attempt_at",
    "last_error",
    "created_at",
    "updated_at",
];

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct CommentRunItem {
    id: i64,
    rule_id: i64,
    instance_code: Option<String>,
    record_id: String,
    state: String,
    payment_fingerprint: Option<String>,
    comment_id: Option<String>,
    attempts: i64,
    next_attempt_at: Option<i64>,
    last_error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

/// 注册评论运行台账列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_list_comment_runs"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/comment/runs/query",
        )
        .display_name("审批评论运行记录")
        .description("分页查询审批评论回写台账，可按规则、状态、目标记录过滤")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListCommentRunsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context
        .approval_comment_runs()
        .query()
        .select_fields(RUN_ITEM_COLUMNS)?;
    if let Some(rule_id) = input.rule_id {
        query = query.where_eq("rule_id", serde_json::json!(rule_id))?;
    }
    if let Some(state) = input
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("state", serde_json::json!(state))?;
    }
    if let Some(record_id) = input
        .record_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query = query.where_eq("record_id", serde_json::json!(record_id))?;
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

    let items: Vec<CommentRunItem> = rows
        .iter()
        .map(|record| {
            Ok(CommentRunItem {
                id: record.require("id")?,
                rule_id: record.require("rule_id")?,
                instance_code: record.optional("instance_code")?,
                record_id: record.require("record_id")?,
                state: record.require("state")?,
                payment_fingerprint: record.optional("payment_fingerprint")?,
                comment_id: record.optional("comment_id")?,
                attempts: record.optional("attempts")?.unwrap_or(0),
                next_attempt_at: record.optional("next_attempt_at")?,
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

    #[test]
    fn paging_is_validated() {
        for (page, page_size) in [(0u32, 10u32), (1, 0), (1, 101)] {
            let input = ListCommentRunsInput {
                page,
                page_size,
                count_total: false,
                rule_id: None,
                state: None,
                record_id: None,
            };
            assert!(input.validate().is_err(), "{page}/{page_size}");
        }
    }

    #[test]
    fn filters_are_optional_and_unknown_keys_are_rejected() {
        let input = serde_json::from_value::<ListCommentRunsInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((input.page, input.page_size), (1, 10));
        assert!(input.validate().is_ok());
        assert!(
            serde_json::from_value::<ListCommentRunsInput>(serde_json::json!({
                "write_status": "failed"
            }))
            .is_err(),
            "状态键是 state，不静默忽略"
        );
    }
}
