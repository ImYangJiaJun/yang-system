//! 企业列表 Action。

use super::super::domain::model::OrganizationView;
use super::super::domain::query::scoped_org_tables;
use crate::addon::org::domain::{Page, PageRequest};
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Str};
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: OrgListInput,
) -> Result<Page<OrganizationView>, BaseError> {
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
