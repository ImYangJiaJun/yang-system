//! 企业关系选择 Action。

use super::super::domain::query::scoped_org_tables;
use yang_base::action::ActionContext;
use yang_base::table::{
    RelationOption, RelationOptionsRequest, RelationOptionsResponse, WhereCondition,
};
use yang_base::BaseError;

pub(super) async fn handle(
    ctx: ActionContext,
    input: RelationOptionsRequest,
) -> Result<RelationOptionsResponse, BaseError> {
    input.validate()?;
    let filters = input
        .filter
        .iter()
        .map(|(field, value)| WhereCondition::Eq {
            field: field.clone(),
            value: value.clone(),
        })
        .collect::<Vec<_>>();
    let page = scoped_org_tables(&ctx)?
        .where_from(&filters)?
        .search(input.search.as_deref())?
        .order("name", yang_base::table::SortOrder::Asc)?
        .page(input.page, input.limit)?
        .table_list()
        .await?;
    let mut items = page
        .data
        .iter()
        .map(relation_option)
        .collect::<Result<Vec<_>, _>>()?;

    if !input.selected.is_empty() {
        let selected = scoped_org_tables(&ctx)?
            .where_from(&filters)?
            .where_from(&[WhereCondition::In {
                field: "id".to_string(),
                values: input.selected,
            }])?
            .page(1, 100)?
            .table_select()
            .await?;
        for option in selected.iter().map(relation_option) {
            let option = option?;
            if !items.iter().any(|item| item.value == option.value) {
                items.push(option);
            }
        }
    }

    Ok(RelationOptionsResponse {
        items,
        page: page.page,
        limit: page.page_size,
        total: Some(
            u64::try_from(page.total)
                .map_err(|_| BaseError::Unknown("关系选项总数超出 u64 范围".to_string()))?,
        ),
    })
}

fn relation_option(record: &yang_base::table::Record) -> Result<RelationOption, BaseError> {
    let value = record
        .get("id")
        .cloned()
        .ok_or_else(|| BaseError::FieldRequired("id".to_string()))?;
    let label: String = record.require("name")?;
    Ok(RelationOption { value, label })
}
