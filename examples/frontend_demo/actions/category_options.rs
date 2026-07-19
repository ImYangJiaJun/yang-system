use async_trait::async_trait;
use serde_json::json;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::ModuleSpec;
use yang_base::table::{RelationOption, RelationOptionsRequest, RelationOptionsResponse};
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "options",
    display_name = "分类选项",
    description = "通用关系选择器 options",
    method = "POST",
    path = "/api/v1/demo/categories/options",
    public
)]
struct CategoryOptionsAction;

#[async_trait]
impl ActionHandler for CategoryOptionsAction {
    type Input = RelationOptionsRequest;
    type Output = RelationOptionsResponse;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
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
}

pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module.native_action(CategoryOptionsAction)
}
