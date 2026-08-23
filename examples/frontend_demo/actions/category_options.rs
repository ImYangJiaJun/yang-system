//! 通用关系选择器 options 演示。

use serde_json::json;
use yang_base::action::ActionContext;
use yang_base::table::{RelationOption, RelationOptionsRequest, RelationOptionsResponse};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    input: RelationOptionsRequest,
) -> Result<RelationOptionsResponse, BaseError> {
    let all = [(1_i64, "平台"), (2, "业务"), (3, "实验")];
    let search = input.search.as_deref().unwrap_or_default().trim();
    let mut items = all
        .into_iter()
        .filter(|(value, label)| {
            search.is_empty()
                || label.contains(search)
                || input
                    .selected
                    .iter()
                    .any(|selected| selected == &json!(value))
        })
        .map(|(value, label)| RelationOption {
            value: json!(value),
            label: label.to_string(),
        })
        .collect::<Vec<_>>();
    let total = u64::try_from(items.len())
        .map_err(|_| BaseError::Unknown("分类选项数量超出 u64 范围".to_string()))?;
    items.truncate(input.limit);
    Ok(RelationOptionsResponse {
        items,
        page: input.page,
        limit: input.limit,
        total: Some(total),
    })
}
