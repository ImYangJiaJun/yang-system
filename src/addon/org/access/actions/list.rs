//! 当前用户可访问的租户列表。

use super::super::domain::service::{TenantService, TenantSummary};
use crate::addon::org::domain::Page;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::Int;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) TenantListInput {
        #[param(source = query)]
        page: Int::new().title("页码"),
        #[param(source = query)]
        limit: Int::new().title("每页数量"),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TenantListInput,
    service: Arc<TenantService>,
) -> Result<Page<TenantSummary>, BaseError> {
    service.list(&ctx, input.page, input.limit).await
}
