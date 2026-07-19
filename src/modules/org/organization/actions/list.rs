//! 企业列表 Action。

use super::super::model::OrganizationView;
use super::support::scoped_org_tables;
use crate::modules::org::pagination::{Page, PageRequest};
use async_trait::async_trait;
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
    type Output = Page<OrganizationView>;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let request = PageRequest::parse(input.page, input.limit)?;
        let result = scoped_org_tables(&ctx)?
            .search(input.search.as_deref())?
            .order("created_at", yang_base::table::SortOrder::Desc)?
            .page(request.page, request.limit)?
            .table_list()
            .await?;
        let items = result
            .data
            .iter()
            .map(OrganizationView::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page::new(items, result.total, request))
    }
}
