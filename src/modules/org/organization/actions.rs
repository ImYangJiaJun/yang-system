//! 企业列表与关系选择 Action。

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, Str};
use yang_base::table::{
    RelationOption, RelationOptionsRequest, RelationOptionsResponse, Tables, WhereCondition,
};
use yang_base::{Action, BaseError};

yang_base::params! {
    /// 企业列表查询参数。
    #[deny_unknown_fields]
    pub(super) OrgListInput {
        #[param(source = query)]
        page: Int::new().title("页码"),
        #[param(source = query)]
        limit: Int::new().title("每页数量"),
        #[param(source = query)]
        search: Str::new().title("搜索词").max_length(100),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct OrgListOutput {
    data: Vec<Value>,
    total: usize,
    page: usize,
    page_size: usize,
    total_pages: usize,
}

#[derive(Action)]
#[action(
    name = "list",
    display_name = "企业列表",
    description = "使用标准 Tables 分页、搜索和排序链查询企业",
    method = "GET",
    path = "/api/v1/orgs",
    permissions("org.org:read")
)]
pub(super) struct OrgListAction;

#[async_trait]
impl ActionHandler for OrgListAction {
    type Input = OrgListInput;
    type Output = OrgListOutput;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let page = positive_usize("page", input.page, 1)?;
        let limit = positive_usize("limit", input.limit, 20)?;
        let result = scoped_org_tables(&ctx)?
            .search(input.search.as_deref())?
            .order("created_at", yang_base::table::SortOrder::Desc)?
            .page(page, limit)?
            .table_list()
            .await?;
        let data = result
            .data
            .into_iter()
            .map(record_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(OrgListOutput {
            data,
            total: result.total,
            page: result.page,
            page_size: result.page_size,
            total_pages: result.total_pages,
        })
    }
}

#[derive(Action)]
#[action(
    name = "select",
    display_name = "企业选择器",
    description = "返回关系字段使用的企业选择项",
    method = "POST",
    path = "/api/v1/orgs/options",
    permissions("org.org:read")
)]
pub(super) struct OrgSelectAction;

#[async_trait]
impl ActionHandler for OrgSelectAction {
    type Input = RelationOptionsRequest;
    type Output = RelationOptionsResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
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
}

fn scoped_org_tables(ctx: &ActionContext) -> Result<Tables, BaseError> {
    let tenant = ctx.tenant()?;
    let mut query = ctx.table_query()?;
    if !tenant.is_system() {
        let org_id = tenant
            .id()
            .ok_or_else(|| BaseError::Unauthorized("普通企业上下文缺少 tenant id".to_string()))?;
        query = query.where_eq("id", serde_json::json!(org_id.get()))?;
    }
    Ok(Tables::new(query))
}

fn relation_option(record: &yang_base::table::Record) -> Result<RelationOption, BaseError> {
    let value = record
        .get("id")
        .cloned()
        .ok_or_else(|| BaseError::FieldRequired("id".to_string()))?;
    let label: String = record.require("name")?;
    Ok(RelationOption { value, label })
}

fn positive_usize(name: &str, value: Option<i64>, default: usize) -> Result<usize, BaseError> {
    match value {
        None => Ok(default),
        Some(value) if value > 0 => usize::try_from(value)
            .map_err(|_| BaseError::ParamInvalid(name.to_string(), "参数超出有效范围".to_string())),
        Some(_) => Err(BaseError::ParamInvalid(
            name.to_string(),
            "参数必须大于 0".to_string(),
        )),
    }
}

fn record_value(record: yang_base::table::Record) -> Result<Value, BaseError> {
    serde_json::to_value(record)
        .map_err(|error| BaseError::Unknown(format!("记录序列化失败: {error}")))
}
