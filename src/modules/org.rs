//! 企业 Addon：展示原生 Module、关系字段、租户隔离和 Tables 查询链。

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{
    Actions, Addon, AddonName, Fields, Int, Module, ModuleName, Modules, Str, Table, TableName,
    Timestamp,
};
use yang_base::{Action, BaseError};

/// 企业能力 Addon。
pub struct OrgAddon;

impl Addon for OrgAddon {
    fn name(&self) -> AddonName {
        yang_base::addon!("org")
    }

    fn modules(&self) -> Modules {
        yang_base::modules![OrgModule, OrgUserModule]
    }

    fn dependencies(&self) -> Vec<AddonName> {
        vec![yang_base::addon!("account")]
    }
}

struct OrgModule;

impl Module for OrgModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("org.org")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("org_org"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID"),
            name => Str::new()
                .title("企业名称")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            code => Str::new()
                .title("企业编号")
                .require(true)
                .max_length(32)
                .unique(true)
                .searchable(true),
            status => Str::new().title("状态").require(true).max_length(16),
            created_at => Timestamp::new().title("创建时间").created_at(),
        }
    }

    fn actions(&self) -> Actions {
        yang_base::actions![OrgListAction, OrgSelectAction]
    }
}

struct OrgUserModule;

impl Module for OrgUserModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("org.user")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("org_user"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID"),
            org_org => Table::new()
                .title("归属企业")
                .require(true)
                .target(yang_base::field!("org_org.id"))
                .display([yang_base::field!("org_org.name")])
                .select(yang_base::action!("org.org.select"))
                .tenant_key(true),
            user_user => Table::new()
                .title("用户")
                .require(true)
                .target(yang_base::field!("users.id"))
                .display([yang_base::field!("users.username")]),
            created_at => Timestamp::new().title("创建时间").created_at(),
        }
    }
}

yang_base::params! {
    /// 企业列表查询参数。
    #[deny_unknown_fields]
    OrgListInput {
        #[param(source = query)]
        page: Int::new().title("页码"),
        #[param(source = query)]
        limit: Int::new().title("每页数量"),
        #[param(source = query)]
        search: Str::new().title("搜索词").max_length(100),
    }
}

yang_base::params! {
    /// 企业选择器参数。
    #[deny_unknown_fields]
    OrgSelectInput {
        #[param(source = query)]
        search: Str::new().title("搜索词").max_length(100),
        #[param(source = query)]
        limit: Int::new().title("返回数量"),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct OrgListOutput {
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
    public
)]
struct OrgListAction;

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
        let result = ctx
            .tables()?
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
    method = "GET",
    path = "/api/v1/orgs/select",
    public
)]
struct OrgSelectAction;

#[async_trait]
impl ActionHandler for OrgSelectAction {
    type Input = OrgSelectInput;
    type Output = Vec<Value>;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let limit = positive_usize("limit", input.limit, 50)?;
        ctx.tables()?
            .search(input.search.as_deref())?
            .page(1, limit)?
            .table_select()
            .await?
            .into_iter()
            .map(record_value)
            .collect()
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
