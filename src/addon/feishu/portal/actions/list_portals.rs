//! 门户列表（前端控制台）。

use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 门户行的**投影列**。
///
/// 提成常量是为了让 `select_fields` 与输出结构体的字段列表**读同一个来源**，
/// 避免「查询选的列」与「结构体声明的键」各自漂移。
pub(super) const PORTAL_ITEM_COLUMNS: &[&str] = &[
    "id",
    "datasource_id",
    "title",
    "slug",
    "custom_slug",
    "query_field_id",
    "query_field_source_key",
    "access_mode",
    "exposed_columns",
    "result_labels",
    "empty_message",
    "rate_limit_per_min",
    "fuzzy_enabled",
    "fuzzy_min_digits",
    "enabled",
    "created_at",
    "updated_at",
    // `passphrase_hash` / `passphrase_expires_at` **不投影**：前者是 secret 字段，
    // 后者只在管理侧设置页用，列表不需要。
];

/// 列表项：**刻意不含任何敏感信息**（`passphrase_hash` 不投影）。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct PortalItem {
    /// 主键。
    id: i64,
    /// 所属数据源 ID。
    datasource_id: i64,
    /// 门户名称。
    title: String,
    /// UUID slug（公开 URL 标识）。
    slug: String,
    /// 自定义短链（可选）。
    custom_slug: Option<String>,
    /// 查询键 field_id。
    query_field_id: String,
    /// 查询键 source_key（冗余）。
    query_field_source_key: String,
    /// 访问模式：public / passphrase / login。
    access_mode: String,
    /// 开放列（JSON 数组）。
    exposed_columns: String,
    /// 展示名映射（JSON 对象，可选）。
    result_labels: Option<String>,
    /// 未找到提示（可选）。
    empty_message: Option<String>,
    /// 每分钟限速。
    rate_limit_per_min: i64,
    /// 是否启用模糊匹配。
    fuzzy_enabled: bool,
    /// 触发模糊匹配所需的最少连续数字位数。
    fuzzy_min_digits: i64,
    /// 启用/停用。
    enabled: bool,
    /// 创建时间（unix 秒）。
    created_at: i64,
    /// 更新时间（unix 秒）。
    updated_at: i64,
}

/// 注册门户列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_portals"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/portals/query")
        .display_name("门户列表")
        .description("分页查询飞书数据源查询门户")
        .permissions(["feishu.portal.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context
        .portals()
        .query()
        .select_fields(PORTAL_ITEM_COLUMNS)?
        .search(input.search.as_deref())?;

    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    // 客户端没给排序时必须兜底：分页要有确定性全序，否则翻页会漏行或重复。
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        // 兜底排序：先用 title，再补 id——只按 title 时同名行之间没有全序。
        query = query.order_by("title", SortOrder::Asc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    let items: Vec<PortalItem> = rows
        .iter()
        .map(record_to_item)
        .collect::<Result<_, BaseError>>()?;

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

/// 把数据库行转为输出结构体。
fn record_to_item(record: &Record) -> Result<PortalItem, BaseError> {
    Ok(PortalItem {
        id: record.require("id")?,
        datasource_id: record.require("datasource_id")?,
        title: record.require("title")?,
        slug: record.require("slug")?,
        custom_slug: record.optional("custom_slug")?,
        query_field_id: record.require("query_field_id")?,
        query_field_source_key: record.require("query_field_source_key")?,
        access_mode: record
            .optional("access_mode")?
            .unwrap_or_else(|| "public".to_string()),
        exposed_columns: record.require("exposed_columns")?,
        result_labels: record.optional("result_labels")?,
        empty_message: record.optional("empty_message")?,
        rate_limit_per_min: record.optional("rate_limit_per_min")?.unwrap_or(30),
        fuzzy_enabled: record.optional("fuzzy_enabled")?.unwrap_or(true),
        fuzzy_min_digits: record.optional("fuzzy_min_digits")?.unwrap_or(4),
        enabled: record.optional("enabled")?.unwrap_or(true),
        created_at: record.require("created_at")?,
        updated_at: record.require("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 控制台与后端兜底都按 `title` + `id` 收尾排序，而 DSL 的 `sortable` 是 fail-closed。
    ///
    /// 这条不是「测一个布尔位」：`TableQuery::order_by` 对不可排序的字段直接返回
    /// `FieldPermissionDenied`，HTTP 边界把它映射成 **403**，于是 `/portals/query`
    /// 每次请求都失败——列表页根本打不开。
    #[tokio::test]
    async fn the_order_clauses_the_console_always_sends_are_applicable() {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = crate::addon::feishu::portal::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        let mut query = definition.bind(pool).query(["system"]);
        for field in ["title", "id", "created_at"] {
            query = query
                .order_by(field, SortOrder::Asc)
                .unwrap_or_else(|error| {
                    panic!(
                        "按 {field} 排序被拒：{error}——不可排序的字段得到 \
                     FieldPermissionDenied，在 HTTP 边界上就是 403"
                    )
                });
        }
    }

    #[test]
    fn the_projection_excludes_sensitive_fields() {
        // passphrase_hash 是 secret 字段，不得投影到列表。
        for column in PORTAL_ITEM_COLUMNS {
            assert_ne!(
                *column, "passphrase_hash",
                "passphrase_hash 是 secret 字段，不得投影到列表"
            );
            assert_ne!(
                *column, "passphrase_expires_at",
                "passphrase_expires_at 只在管理侧设置页用，列表不需要"
            );
        }
    }
}
