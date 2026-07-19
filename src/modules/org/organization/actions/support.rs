//! 企业 Action 共享查询支持。

use yang_base::action::ActionContext;
use yang_base::table::Tables;
use yang_base::BaseError;

pub(super) fn scoped_org_tables(ctx: &ActionContext) -> Result<Tables, BaseError> {
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
