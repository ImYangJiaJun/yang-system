//! 门户访问日志（前端控制台）。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 日志行的投影列。
///
/// 提成常量是为了让 `select_fields` 与输出结构体的字段列表读同一个来源。
pub(super) const LOG_ITEM_COLUMNS: &[&str] = &[
    "id",
    "portal_id",
    "query_value_hash",
    "result_count",
    "ip_address",
    "user_agent",
    "access_mode_used",
    "status",
    "created_at",
];

/// 一条访问日志。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct PortalLogItem {
    /// 主键。
    id: i64,
    /// 所属门户。
    portal_id: i64,
    /// 查询值的 SHA-256（明文不落库）。
    query_value_hash: String,
    /// 返回行数。
    result_count: i64,
    /// 来源 IP。
    ip_address: Option<String>,
    /// 浏览器 User-Agent。
    user_agent: Option<String>,
    /// 本次使用的访问模式。
    access_mode_used: String,
    /// 查询状态：hit / miss / rate_limited / forbidden。
    status: String,
    /// 发生时间（unix 秒）。
    created_at: i64,
}

/// 访问日志查询的输入：门户 `id` + 标准分页契约。
///
/// 分页部分整体复用 [`ListInput`]，与通用 `TableView` 发出的六个键同构。
#[derive(Deserialize, JsonSchema)]
pub(super) struct PortalLogInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
    /// 标准分页 / 筛选 / 排序输入。
    #[serde(flatten)]
    pub(super) page: ListInput,
}

impl ParamInput for PortalLogInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 注册访问日志端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("portal_access_log"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/portals/log")
        .display_name("门户访问日志")
        .description("按时间倒序分页查询某个门户的访问日志")
        .permissions(["feishu.portal.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: PortalLogInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.page.validate()?;

    let mut query = context
        .portal_logs()
        .query()
        .select_fields(LOG_ITEM_COLUMNS)?
        .where_eq("portal_id", serde_json::json!(input.portal_id))?;
    if let Some(tree) = input.page.where_clause.clone() {
        query = query.where_tree(tree)?;
    }
    let total = if input.page.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    let mut ordered = false;
    for OrderByItem { field, direction } in &input.page.order_by {
        query = query.order_by(field, *direction)?;
        ordered = true;
    }
    if !ordered {
        // 兜底排序：`created_at` 同一秒内可以有很多行，只按它排没有全序，
        // 翻页会漏行或重复。补 `id` 收尾。
        query = query.order_by("created_at", SortOrder::Desc)?;
        query = query.order_by("id", SortOrder::Desc)?;
    }
    query = query.page(input.page.page as usize, input.page.page_size as usize)?;
    let rows = query.all().await?;

    let items = rows
        .iter()
        .map(record_to_item)
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "items": items,
            "page": input.page.page,
            "page_size": input.page.page_size,
            "total": total,
        }),
        "查询成功",
    ))
}

/// 把数据库行转为输出结构体。
fn record_to_item(record: &Record) -> Result<PortalLogItem, BaseError> {
    Ok(PortalLogItem {
        id: record.require("id")?,
        portal_id: record.require("portal_id")?,
        query_value_hash: record.require("query_value_hash")?,
        result_count: record.optional("result_count")?.unwrap_or(0),
        ip_address: record.optional("ip_address")?,
        user_agent: record.optional("user_agent")?,
        access_mode_used: record
            .optional("access_mode_used")?
            .unwrap_or_else(|| "public".to_string()),
        status: record.require("status")?,
        created_at: record.require("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_accepts_the_standard_select_contract_plus_portal_id() {
        let input = serde_json::from_value::<PortalLogInput>(serde_json::json!({
            "portal_id": 9,
            "page": 2,
            "page_size": 20,
            "search": "1.2.3.4",
            "order_by": [{"field": "created_at", "direction": "Desc"}],
            "count_total": true
        }))
        .unwrap_or_else(|error| panic!("标准 select 输入必须可解析: {error}"));
        assert_eq!(input.portal_id, 9);
        assert_eq!(input.page.page, 2);
        assert_eq!(input.page.page_size, 20);
        assert!(input.page.count_total);
    }

    #[test]
    fn the_input_accepts_a_bare_portal_id() {
        let input = serde_json::from_value::<PortalLogInput>(serde_json::json!({"portal_id": 9}))
            .unwrap_or_else(|error| panic!("只给 portal_id 应可解析: {error}"));
        assert_eq!(input.page.page, 1);
        assert_eq!(input.page.page_size, 10);
    }

    /// 控制台与后端兜底都按 `created_at` 收尾排序，而 DSL 的 `sortable` 是 fail-closed。
    ///
    /// 这条不是「测一个布尔位」：`TableQuery::order_by` 对不可排序的字段直接返回
    /// `FieldPermissionDenied`，HTTP 边界把它映射成 **403**——日志页不是「排序不生效」，
    /// 是整个请求恒失败。
    #[tokio::test]
    async fn the_order_clauses_the_console_always_sends_are_applicable() {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = crate::addon::feishu::portal::domain::log_table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        let mut query = definition.bind(pool).query(["system"]);
        for (field, order) in [("created_at", SortOrder::Desc), ("id", SortOrder::Desc)] {
            query = query.order_by(field, order).unwrap_or_else(|error| {
                panic!(
                    "按 {field} 排序被拒：{error}——不可排序的字段得到 \
                     FieldPermissionDenied，在 HTTP 边界上就是 403"
                )
            });
        }
    }

    #[test]
    fn the_projection_covers_every_output_field() {
        // 投影列与输出结构体的键必须一一对应：少一列时 `require` 在运行期才报错。
        for column in [
            "id",
            "portal_id",
            "query_value_hash",
            "result_count",
            "ip_address",
            "user_agent",
            "access_mode_used",
            "status",
            "created_at",
        ] {
            assert!(
                LOG_ITEM_COLUMNS.contains(&column),
                "输出字段 {column} 未在投影列里"
            );
        }
    }
}
