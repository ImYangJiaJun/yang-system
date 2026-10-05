//! 查询持有某权限的全部载体：直授行 + 持有该权限条目的权限组（权限下钻的审计视图）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::repository::GroupRecord;
use crate::addon::access::domain::repository::{current_unix_timestamp, is_expired, GrantRecord};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ListHoldersInput {
        /// 目标权限。
        #[param(source = query)]
        permission: Str::new()
            .title("权限")
            .require(true),
    }
}

/// 一条直授持有事实的对外视图（审计口径：含过期行，带 expired 派生标记）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DirectHolderView {
    user_id: i64,
    granted_by: i64,
    occurred_at: i64,
    /// Unix 秒；`None` = 永久有效。
    expires_at: Option<i64>,
    /// 派生标记：是否已过期（与直授解析侧同一时钟同一判据）。
    expired: bool,
}

/// 一个持有该权限条目的权限组视图。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GroupHolderView {
    id: i64,
    group_key: String,
    title: String,
    /// 成员数（该组有多少人因此获得该权限，下钻展示用）。
    member_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListHoldersResult {
    /// 直接持有者（直授行，含已过期的审计行）。
    direct: Vec<DirectHolderView>,
    /// 经权限组持有者（每个组一条，成员数另计）。
    groups: Vec<GroupHolderView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListHoldersInput,
    access: Arc<Access>,
) -> Result<ListHoldersResult, BaseError> {
    // 未声明的权限不允许下钻（与授予闸门同一判据），fail-closed。
    access
        .permission_catalog()
        .ensure_declared(&input.permission)?;

    let mut transaction = ctx
        .tools()
        .mysql()?
        .read_only_transaction()
        .await
        .map_err(BaseError::from)?;
    let result = async {
        // 审计视图读取全部直授行（含过期行）：与解析侧（list_by_user_in_tx 的 SQL
        // 过滤）相反，这里要连过期行一起展示，expired 派生标记由展示层按同一时钟计算。
        let records = access
            .grants()
            .list_by_permission_in_tx(&ctx, &mut transaction, &input.permission)
            .await?;
        let groups = access
            .groups()
            .list_groups_holding_item_in_tx(&ctx, &mut transaction, &input.permission)
            .await?;
        Ok::<_, BaseError>((records, groups))
    }
    .await;
    let (records, groups) = Access::finish_transaction(transaction, result).await?;

    let now = current_unix_timestamp()?;
    Ok(ListHoldersResult {
        direct: records
            .into_iter()
            .map(|record| direct_view(record, now))
            .collect(),
        groups: groups
            .into_iter()
            .map(|(group, member_count)| group_view(group, member_count))
            .collect(),
    })
}

/// 把一条直授事实投影为持有者视图（含 `expired` 派生标记，前端展示用）。
fn direct_view(record: GrantRecord, now: i64) -> DirectHolderView {
    DirectHolderView {
        user_id: record.user_id,
        granted_by: record.granted_by,
        occurred_at: record.occurred_at,
        expires_at: record.expires_at,
        expired: is_expired(record.expires_at, now),
    }
}

/// 把一组事实与它的成员数投影为持有组视图。
fn group_view(group: GroupRecord, member_count: u64) -> GroupHolderView {
    GroupHolderView {
        id: group.id,
        group_key: group.group_key,
        title: group.title,
        member_count: member_count as i64,
    }
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_holders"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Get, "/api/v1/access/grants/holders")
        .display_name("权限持有者列表")
        .description("查询直接持有与经权限组持有某权限的全部主体")
        .permissions(["access.grants.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::domain::permission_catalog::PermissionEntry;
    use crate::authorization::{
        AuthorizationPort, AuthorizationVersionSource, AuthorizationVersionWriter,
        LockedAuthorization,
    };
    use async_trait::async_trait;
    use sqlx::MySqlPool;
    use yang_base::action::Request;
    use yang_base::definition::ParamInput;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig, Transaction};

    fn table_definition(
        spec: Result<yang_base::definition::TableSpec, BaseError>,
    ) -> yang_base::table::TableDefinition {
        spec.and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("表定义应有效: {error}"))
    }

    fn offline_tools() -> Arc<yang_base::tools::Tools> {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        Arc::new(
            ToolsBuilder::new()
                .mysql(
                    Database::from_pool(pool, DatabaseConfig::default())
                        .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}")),
                )
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        )
    }

    /// 权限下钻只读目录与直授/组事实，不触碰授权版本端口；测试桩永不被调用。
    struct UnusedSource;
    struct UnusedWriter;

    #[async_trait]
    impl AuthorizationVersionSource for UnusedSource {
        async fn find_authorization_version(
            &self,
            _pool: &MySqlPool,
            _user_id: i64,
        ) -> Result<Option<crate::authorization::AuthorizationVersionSnapshot>, BaseError> {
            unreachable!("权限下钻不读取授权版本")
        }
    }

    #[async_trait]
    impl AuthorizationVersionWriter for UnusedWriter {
        async fn lock_authorization_version(
            &self,
            _pool: &MySqlPool,
            _transaction: &mut Transaction,
            _user_id: i64,
        ) -> Result<LockedAuthorization, BaseError> {
            unreachable!("权限下钻不写入授权版本")
        }

        async fn increment_locked_authorization_version(
            &self,
            _transaction: &mut Transaction,
            _locked: &LockedAuthorization,
        ) -> Result<i64, BaseError> {
            unreachable!("权限下钻不写入授权版本")
        }
    }

    /// 已安装目录（含一条声明）的 Access：`ensure_declared` 在触碰数据库之前就能
    /// 判定，未声明权限的下钻在这里被 fail-closed 拒绝。
    fn installed_access() -> Arc<Access> {
        let grants = crate::addon::access::domain::repository::GrantRepository::new(
            table_definition(crate::addon::access::grants::table::grants_table_spec()),
        );
        let groups = crate::addon::access::domain::groups::repository::GroupRepository::new(
            table_definition(crate::addon::access::groups::table::groups_table_spec()),
            table_definition(
                crate::addon::access::domain::groups::tables::group_items_table_spec(),
            ),
            table_definition(crate::addon::access::domain::groups::tables::user_group_table_spec()),
            table_definition(
                crate::addon::access::domain::groups::tables::system_owner_table_spec(),
            ),
        );
        let catalog =
            crate::addon::access::domain::permission_catalog::PermissionCatalogHandle::new();
        catalog
            .install(vec![PermissionEntry {
                permission: "access.grants.read".to_string(),
                declared_by: vec!["access.grants.list_holders".to_string()],
                admin_equivalent: false,
                reason: None,
            }])
            .unwrap_or_else(|error| panic!("测试目录应可安装: {error}"));
        Arc::new(Access::new(
            grants,
            groups,
            catalog,
            AuthorizationPort::new(Arc::new(UnusedSource), Arc::new(UnusedWriter)),
        ))
    }

    #[test]
    fn permission_comes_from_the_query_and_is_required() {
        let params = <ListHoldersInput as ParamInput>::params();
        let permission = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "permission")
            .unwrap_or_else(|| panic!("应声明 permission 参数"));
        assert_eq!(permission.source, yang_base::definition::ParamSource::Query);
        assert!(permission.required);

        // GET 无请求体：客户端向 body 注入字段既不会被解码，也过不了
        // deny_unknown_fields 的结构级校验。
        let injected = serde_json::from_value::<ListHoldersInput>(serde_json::json!({
            "permission": "access.grants.read",
            "granted_by": 9
        }));
        assert!(injected.is_err(), "客户端不能注入请求体字段");
    }

    #[tokio::test]
    async fn undeclared_permission_is_rejected_fail_closed() {
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), offline_tools());
        let error = match handle(
            ctx,
            ListHoldersInput {
                permission: "not.declared.anywhere".to_string(),
            },
            installed_access(),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("未声明权限必须被拒绝"),
        };
        let field = match error {
            BaseError::ParamInvalid(field, _) => field,
            other => panic!("未声明权限必须按参数错误拒绝: {other}"),
        };
        assert_eq!(field, "permission");
    }

    #[test]
    fn views_project_the_exact_holder_shape() {
        let now = 1_700_000_000;
        let direct = direct_view(
            GrantRecord {
                id: 1,
                user_id: 7,
                permission: "access.grants.read".to_string(),
                granted_by: 9,
                occurred_at: 1_690_000_000,
                expires_at: Some(now - 1),
            },
            now,
        );
        assert!(direct.expired, "过期直授行必须带 expired 派生标记");

        let group = group_view(
            GroupRecord {
                id: 3,
                group_key: "ops".to_string(),
                title: "运维".to_string(),
                description: None,
                created_by: 9,
            },
            12,
        );
        assert_eq!(group.member_count, 12);

        // 响应契约：前端按字段名解析，空持有者返回空数组而不是错误。
        let result = ListHoldersResult {
            direct: vec![direct],
            groups: vec![group],
        };
        let value = serde_json::to_value(&result).unwrap_or_else(|error| panic!("{error}"));
        let direct = value["direct"][0].clone();
        let keys: Vec<&str> = direct
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "expired",
                "expires_at",
                "granted_by",
                "occurred_at",
                "user_id"
            ]
        );
        let groups = value["groups"][0].clone();
        let keys: Vec<&str> = groups
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(keys, ["group_key", "id", "member_count", "title"]);

        let empty = ListHoldersResult {
            direct: Vec::new(),
            groups: Vec::new(),
        };
        let value = serde_json::to_value(&empty).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["direct"], serde_json::json!([]));
        assert_eq!(value["groups"], serde_json::json!([]));
    }
}
