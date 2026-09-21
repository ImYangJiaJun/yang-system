//! 选项列表（前端控制台，只读）。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 注册选项列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_options"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/options/query")
        .display_name("选项列表")
        .description("分页查询飞书选项")
        .permissions(["feishu.option.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let (page, page_size) = input.normalized();
    let mut query = context
        .options()
        .query()
        .select_fields(&[
            "option_id",
            "source_key",
            "label",
            "i18n",
            "sort_order",
            "is_default",
            "enabled",
        ])?
        .search(input.search.as_deref())?;

    if let Some(source_key) = input.source_key.as_deref() {
        query = query.where_eq("source_key", serde_json::json!(source_key))?;
    }

    let result = query
        .order_by("sort_order", SortOrder::Asc)?
        .order_by("option_id", SortOrder::Asc)?
        .page(page, page_size)?
        .paginate_records()
        .await?;

    // i18n 落 Text 列存 JSON 文本；这里回原样，前端按文本展示即可
    let items = result
        .data
        .iter()
        .map(|record| {
            Ok(serde_json::json!({
                "option_id": record.require::<String>("option_id")?,
                "source_key": record.require::<String>("source_key")?,
                "label": record.require::<String>("label")?,
                "i18n": record.optional::<String>("i18n")?,
                "sort_order": record.require::<i64>("sort_order")?,
                "is_default": record.optional::<bool>("is_default")?.unwrap_or(false),
                "enabled": record.optional::<bool>("enabled")?.unwrap_or(true),
            }))
        })
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "items": items,
            "page": page,
            "page_size": page_size,
            "total": result.total,
        }),
        "查询成功",
    ))
}
