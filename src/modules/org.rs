//! 企业 Addon：展示原生 Module、关系字段、租户隔离和 Tables 查询链。

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use yang_base::action::{
    Action as ActionHandler, ActionContext, TenantContext, TenantId, TenantResolver,
    TenantResolverMiddleware, TokenAuthMiddleware,
};
use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, Actions,
    AddonSpec, Fields, Int, Module, ModuleName, ModuleSpec, Str, Table, TableName, Timestamp,
    ViewName, ViewSpec,
};
use yang_base::table::{
    RelationOption, RelationOptionsRequest, RelationOptionsResponse, TableDefinition, Tables,
    WhereCondition,
};
use yang_base::{Action, BaseError};

/// 构建带认证、可信租户解析和标准 CRUD/View 投影的企业 Addon。
pub fn build_addon() -> Result<AddonSpec, BaseError> {
    let (org, org_user) = org_specs()?;
    let memberships = org_user
        .table
        .as_ref()
        .ok_or(BaseError::TableDefinitionNotSet)?
        .table_definition()?;
    assemble_addon(
        org,
        org_user,
        Arc::new(DatabaseMembershipReader { memberships }),
    )
}

fn org_specs() -> Result<(ModuleSpec, ModuleSpec), BaseError> {
    Ok((
        OrgModule.into_spec(),
        OrgUserModule.into_spec().view(org_user_view()?),
    ))
}

fn assemble_addon(
    org: ModuleSpec,
    org_user: ModuleSpec,
    memberships: Arc<dyn OrgMembershipReader>,
) -> Result<AddonSpec, BaseError> {
    let resolver = OrgTenantResolver::new(memberships);

    let org = org
        .middleware(TokenAuthMiddleware::new(super::user::user_from_claims))
        .middleware(TenantResolverMiddleware::new(resolver.clone()));
    let org_user = org_user
        .middleware(TokenAuthMiddleware::new(super::user::user_from_claims))
        .middleware(TenantResolverMiddleware::new(resolver))
        .crud()?;

    Ok(AddonSpec::new(yang_base::addon!("org"))
        .depends_on(yang_base::addon!("account"))
        .module(org)
        .module(org_user))
}

#[async_trait]
trait OrgMembershipReader: Send + Sync + 'static {
    async fn contains(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<bool, BaseError>;
}

struct DatabaseMembershipReader {
    memberships: TableDefinition,
}

#[async_trait]
impl OrgMembershipReader for DatabaseMembershipReader {
    async fn contains(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<bool, BaseError> {
        let table = self
            .memberships
            .bind(Arc::new(context.tools().mysql()?.pool().clone()));
        let rows = table
            .query(std::iter::empty::<&str>())
            .select_fields(&["id"])?
            .where_eq("org_org", serde_json::json!(org_id.get()))?
            .where_eq("user_user", serde_json::json!(user_id))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }
}

#[derive(Clone)]
struct OrgTenantResolver {
    memberships: Arc<dyn OrgMembershipReader>,
}

impl OrgTenantResolver {
    fn new(memberships: Arc<dyn OrgMembershipReader>) -> Self {
        Self { memberships }
    }

    async fn resolve_authenticated(
        &self,
        context: &ActionContext,
        user: &yang_base::action::User,
        requested: Option<TenantId>,
    ) -> Result<TenantContext, BaseError> {
        if user.has_role("system") {
            return Ok(TenantContext::system());
        }
        let org_id = requested
            .ok_or_else(|| BaseError::Unauthorized("请求缺少企业租户上下文".to_string()))?;
        if !self.memberships.contains(context, user.id, org_id).await? {
            return Err(BaseError::PermissionDenied(format!(
                "用户无权访问企业 {}",
                org_id.get()
            )));
        }
        Ok(TenantContext::new(org_id))
    }
}

#[async_trait]
impl TenantResolver for OrgTenantResolver {
    async fn resolve(
        &self,
        context: &ActionContext,
        requested: Option<TenantId>,
    ) -> Result<TenantContext, BaseError> {
        let user = context
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("企业租户解析需要已认证用户".to_string()))?;
        self.resolve_authenticated(context, user, requested).await
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

fn org_user_view() -> Result<ViewSpec, BaseError> {
    let name = ViewName::new("list").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let confirm_delete = ActionConfirmation::new("确认删除成员", "删除后该用户将失去企业访问权");
    Ok(ViewSpec::new(name)
        .data_action(yang_base::action!("org.user.select"))
        .field(yang_base::field!("org_user.id"))
        .field(yang_base::field!("org_user.org_org"))
        .field(yang_base::field!("org_user.user_user"))
        .field(yang_base::field!("org_user.created_at"))
        .present_action(
            yang_base::action!("org.user.add"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("org.user.put"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("org.user.del"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .confirmation(confirm_delete),
        ))
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
    permissions("org.org:read")
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
struct OrgSelectAction;

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

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::action::Request;
    use yang_base::action::User;
    use yang_base::tools::ToolsBuilder;

    struct FakeMembershipReader;

    #[async_trait]
    impl OrgMembershipReader for FakeMembershipReader {
        async fn contains(
            &self,
            _context: &ActionContext,
            user_id: i64,
            org_id: TenantId,
        ) -> Result<bool, BaseError> {
            Ok(user_id == 7 && org_id == TenantId::new(10))
        }
    }

    fn context() -> ActionContext {
        ActionContext::new(
            Request::new(serde_json::json!({})),
            Arc::new(
                ToolsBuilder::new()
                    .build()
                    .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
            ),
        )
    }

    #[tokio::test]
    async fn tenant_policy_rejects_missing_and_cross_tenant_but_accepts_membership() {
        let resolver = OrgTenantResolver::new(Arc::new(FakeMembershipReader));
        let member = User::new(7, "member");

        assert!(matches!(
            resolver
                .resolve_authenticated(&context(), &member, None)
                .await,
            Err(BaseError::Unauthorized(_))
        ));
        assert!(matches!(
            resolver
                .resolve_authenticated(&context(), &member, Some(TenantId::new(20)))
                .await,
            Err(BaseError::PermissionDenied(_))
        ));
        let tenant = resolver
            .resolve_authenticated(&context(), &member, Some(TenantId::new(10)))
            .await
            .unwrap_or_else(|error| panic!("真实成员应通过租户策略: {error}"));
        assert_eq!(tenant.id(), Some(TenantId::new(10)));
        assert!(!tenant.is_system());
    }

    #[tokio::test]
    async fn system_role_is_the_only_explicit_tenant_bypass() {
        let resolver = OrgTenantResolver::new(Arc::new(FakeMembershipReader));
        let system = User::new(9, "system").with_roles(["system"]);

        let tenant = resolver
            .resolve_authenticated(&context(), &system, None)
            .await
            .unwrap_or_else(|error| panic!("system 角色应显式绕过普通租户选择: {error}"));
        assert!(tenant.is_system());
        assert_eq!(tenant.id(), None);
    }
}
