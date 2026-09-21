//! 数据源列表（前端控制台）。

use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 列表项：**刻意不含 `token_hash`**，只在响应里暴露「是否已配置」。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct DatasourceItem {
    /// 数据源标识。
    source_key: String,
    /// 展示名。
    title: String,
    /// 是否启用加密返回。
    encrypt_enabled: bool,
    /// 默认语言。
    default_locale: String,
    /// 状态。
    status: String,
}

/// 注册数据源列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_datasources"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/query")
        .display_name("数据源列表")
        .description("分页查询飞书数据源")
        .permissions(["feishu.datasource.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 必须显式列出可读字段：ensure_readable_projection 是框架 crate 私有的，
    // 自定义列表 Action 不会自动拿到「默认可读投影」。token_hash 是 secret 字段，
    // 即便列进来也会被兜住，但显式排除让它更清楚。
    let mut query = context
        .datasources()
        .query()
        .select_fields(&[
            "source_key",
            "title",
            "encrypt_enabled",
            "default_locale",
            "status",
        ])?
        .search(input.search.as_deref())?;

    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    let items = rows
        .iter()
        .map(|record| {
            Ok(DatasourceItem {
                source_key: record.require("source_key")?,
                title: record.require("title")?,
                encrypt_enabled: record.optional("encrypt_enabled")?.unwrap_or(false),
                default_locale: record.optional("default_locale")?.unwrap_or_default(),
                status: record.require("status")?,
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
