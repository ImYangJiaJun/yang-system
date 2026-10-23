//! 审批事件列表（前端控制台「事件排查」视图）。
//!
//! 事件表是 store-then-process 的落地缓冲：`processed_at IS NULL` 是待处理队列，
//! 连续失败达 [`MAX_EVENT_RETRY_COUNT`] 后 worker 会置终态并停止自动重试。终态行
//! 以前只能翻库发现，本端点把它们变成可查询事实；`terminal_only` 直接对应
//! 「需要人工处置」的那一档。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::approval::domain::event_table::MAX_EVENT_RETRY_COUNT;
use crate::addon::feishu::domain::context::FeishuContext;

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    10
}

/// 事件列表输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListEventsInput {
    /// 页码（1 起步），缺省 1。
    #[serde(default = "default_page")]
    pub(super) page: u32,
    /// 每页条数，缺省 10，必须 1..=100。
    #[serde(default = "default_page_size")]
    pub(super) page_size: u32,
    /// 只看失败终态（`processed_at` 非空且 `retry_count >= 上限`）。
    #[serde(default)]
    pub(super) terminal_only: bool,
}

impl ParamInput for ListEventsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListEventsInput {
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

/// 事件行投影列。
pub(super) const EVENT_ITEM_COLUMNS: &[&str] = &[
    "id",
    "event_id",
    "event_type",
    "instance_code",
    "status",
    "received_at",
    "processed_at",
    "retry_count",
    "last_error",
];

/// 一条审批事件。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct EventItem {
    /// 主键。
    id: i64,
    /// 飞书事件唯一标识（幂等键）。
    event_id: String,
    /// 事件类型：approval_instance / approval_task。
    event_type: String,
    /// 审批实例编码。
    instance_code: String,
    /// 事件中的状态。
    status: Option<String>,
    /// 本服务接收时间（unix 秒）。
    received_at: i64,
    /// 处理完成时间（unix 秒）；空 = 待处理。
    processed_at: Option<i64>,
    /// 已重试次数。
    retry_count: i64,
    /// 最后一次错误信息。
    last_error: Option<String>,
}

/// 注册事件列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list_events"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(HttpMethod::Post, "/api/v1/feishu/approval/events/query")
        .display_name("审批事件列表")
        .description("分页查询飞书审批事件，可按失败终态过滤")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListEventsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context
        .approval_events()
        .query()
        .select_fields(EVENT_ITEM_COLUMNS)?;
    if input.terminal_only {
        query = query
            .where_not_null("processed_at")?
            .where_gte("retry_count", serde_json::json!(MAX_EVENT_RETRY_COUNT))?;
    }
    let rows = query
        .order_by("id", SortOrder::Desc)?
        .page(input.page as usize, input.page_size as usize)?
        .all()
        .await?;

    let items: Vec<EventItem> = rows
        .iter()
        .map(|record| {
            Ok(EventItem {
                id: record.require("id")?,
                event_id: record.require("event_id")?,
                event_type: record.require("event_type")?,
                instance_code: record.require("instance_code")?,
                status: record.optional("status")?,
                received_at: record.require("received_at")?,
                processed_at: record.optional("processed_at")?,
                retry_count: record.optional("retry_count")?.unwrap_or(0),
                last_error: record.optional("last_error")?,
            })
        })
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "items": items,
            "page": input.page,
            "page_size": input.page_size,
        }),
        "查询成功",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_keys_match_the_projection() {
        let item = EventItem {
            id: 1,
            event_id: "evt_001".to_string(),
            event_type: "approval_instance".to_string(),
            instance_code: "INST-1".to_string(),
            status: Some("APPROVED".to_string()),
            received_at: 1_700_000_000,
            processed_at: None,
            retry_count: 0,
            last_error: None,
        };
        let value =
            serde_json::to_value(&item).unwrap_or_else(|error| panic!("应可序列化: {error}"));
        let mut keys: Vec<&str> = value
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        let mut expected: Vec<&str> = EVENT_ITEM_COLUMNS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "列表项键必须恰好等于投影列");
    }

    #[test]
    fn terminal_only_defaults_to_false_and_paging_is_validated() {
        let input = serde_json::from_value::<ListEventsInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("空对象应可解析: {error}"));
        assert!(!input.terminal_only);
        assert_eq!((input.page, input.page_size), (1, 10));
        assert!(input.validate().is_ok());

        for (page, page_size) in [(0u32, 10u32), (1, 0), (1, 101)] {
            let input = ListEventsInput {
                page,
                page_size,
                terminal_only: true,
            };
            assert!(
                input.validate().is_err(),
                "page={page} page_size={page_size} 应被拒绝"
            );
        }
    }

    #[tokio::test]
    async fn declares_read_permission_and_route() {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let repository = |spec: Result<yang_base::definition::TableSpec, _>| {
            crate::addon::feishu::domain::repository::Repository::new(
                spec.unwrap_or_else(|error| panic!("{error}"))
                    .table_definition()
                    .unwrap_or_else(|error| panic!("{error}")),
                Arc::clone(&pool),
            )
        };
        let context = Arc::new(FeishuContext::new(
            repository(crate::addon::feishu::datasource::table::table_spec()),
            repository(crate::addon::feishu::datasource::domain::field_table::table_spec()),
            repository(crate::addon::feishu::option::table::table_spec()),
            repository(crate::addon::feishu::approval::table::table_spec()),
            repository(crate::addon::feishu::approval::domain::field_map_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::request_log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_applicant_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::user_token_table::table_spec()),
            repository(crate::addon::feishu::portal::table::table_spec()),
            repository(crate::addon::feishu::portal::domain::log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::event_table::table_spec()),
            Arc::new(
                yang_base::action::auth::PasswordEngine::new(1)
                    .unwrap_or_else(|error| panic!("PasswordEngine 应可构造: {error}")),
            ),
            None,
        ));
        let module = register(
            ModuleSpec::new(
                yang_base::definition::ModuleName::new("feishu.approval")
                    .unwrap_or_else(|error| panic!("{error}")),
            ),
            context,
        );
        let action = module
            .actions()
            .iter()
            .find(|spec| spec.route.path == "/api/v1/feishu/approval/events/query")
            .unwrap_or_else(|| panic!("事件查询路由必须注册"));
        assert_eq!(action.permissions, vec!["feishu.approval.read".to_string()]);
    }
}
