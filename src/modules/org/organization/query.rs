//! 企业查询的租户作用域支持。

use yang_base::action::ActionContext;
use yang_base::table::Tables;
use yang_base::BaseError;

pub(super) fn scoped_org_tables(ctx: &ActionContext) -> Result<Tables, BaseError> {
    let tenant = ctx.tenant()?;
    let query = ctx
        .table_query()?
        .where_eq("id", serde_json::json!(tenant.id().get()))?;
    Ok(Tables::new(query))
}
