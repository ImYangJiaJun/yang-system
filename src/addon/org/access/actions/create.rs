//! 创建企业并建立初始成员关系。

use super::super::domain::service::{TenantService, TenantSummary};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::Str;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) TenantCreateInput {
        name: Str::new().title("企业名称").require(true).min_length(1).max_length(100),
        code: Str::new().title("企业编号").require(true).min_length(2).max_length(32),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TenantCreateInput,
    service: Arc<TenantService>,
) -> Result<TenantSummary, BaseError> {
    service.create(&ctx, &input.name, &input.code).await
}
