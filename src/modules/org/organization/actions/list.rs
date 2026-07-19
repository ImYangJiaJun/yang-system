//! 企业列表 Action。

use super::support::scoped_org_tables;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, Str};
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
