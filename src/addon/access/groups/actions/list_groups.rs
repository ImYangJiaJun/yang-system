//! 列出全部权限组：成员数、权限数、内置标记与孤儿条目数。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::repository::{GroupRecord, SYSTEM_ADMIN_GROUP_KEY};
use crate::addon::access::domain::groups::resolution::{catalog_permissions, orphan_items};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyInput {}

impl ParamInput for EmptyInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 列表里的一个组。`orphan_item_count` 只报告不清理（spec §8.4）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GroupSummaryView {
    id: i64,
    group_key: String,
    title: String,
    description: Option<String>,
    member_count: u64,
    item_count: u64,
    is_builtin: bool,
    orphan_item_count: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListGroupsResult {
    groups: Vec<GroupSummaryView>,
}

/// 把一条组事实与它的计数投影成列表项；纯函数，便于单测计数口径。
fn summarize(
    group: &GroupRecord,
    member_count: u64,
    items: &[String],
    catalog: &[String],
) -> GroupSummaryView {
    GroupSummaryView {
        id: group.id,
        group_key: group.group_key.clone(),
        title: group.title.clone(),
        description: group.description.clone(),
        member_count,
        item_count: items.len() as u64,
        is_builtin: group.group_key == SYSTEM_ADMIN_GROUP_KEY,
        orphan_item_count: orphan_items(items, catalog).len() as u64,
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: EmptyInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    // 只读列表：不写任何事实，也不需要扇出失效。
    let mut transaction = ctx.tools().mysql()?.read_only_transaction().await?;
    let result = async {
        let catalog = catalog_permissions(access.permission_catalog())?;
        let mut views = Vec::new();
        for group in access
            .groups()
            .list_groups_in_tx(&ctx, &mut transaction)
            .await?
        {
            let member_count = access
                .groups()
                .count_members_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            let items = access
                .groups()
                .list_items_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            views.push(summarize(&group, member_count, &items, &catalog));
        }
        // 存储层不保证行序，管理列表按组标识稳定排序，避免每次刷新顺序漂移。
        views.sort_by(|left, right| left.group_key.cmp(&right.group_key));
        Ok(views)
    }
    .await;
    let groups = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(ListGroupsResult { groups }, "权限组列表")
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list_groups"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&access))
        })
        .route(HttpMethod::Get, "/api/v1/access/groups")
        .display_name("权限组列表")
        .description("列出全部权限组及其成员数、权限数与孤儿条目数")
        .permissions(["access.groups.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(group_key: &str) -> GroupRecord {
        GroupRecord {
            id: 3,
            group_key: group_key.to_string(),
            title: "运维".to_string(),
            description: None,
            created_by: 1,
        }
    }

    fn catalog() -> Vec<String> {
        vec![
            "access.groups.read".to_string(),
            "access.groups.write".to_string(),
        ]
    }

    #[test]
    fn summary_counts_items_orphans_and_builtin_flag() {
        let items = vec![
            "access.groups.read".to_string(),
            "removed.module.act".to_string(),
        ];
        let view = summarize(&group("ops"), 2, &items, &catalog());
        assert_eq!(view.id, 3);
        assert_eq!(view.group_key, "ops");
        assert_eq!(view.member_count, 2);
        assert_eq!(view.item_count, 2, "条目数是存量口径，孤儿也算在内");
        assert_eq!(view.orphan_item_count, 1, "只报目录外的条目");
        assert!(!view.is_builtin);

        // 内置全权组的条目由目录计算，列表必须把它标出来让前端特判（spec §15 第 3 条）。
        assert!(summarize(&group(SYSTEM_ADMIN_GROUP_KEY), 0, &[], &catalog()).is_builtin);
    }

    #[test]
    fn list_response_names_the_frontend_contract() {
        let payload = ListGroupsResult {
            groups: vec![summarize(&group("ops"), 0, &[], &catalog())],
        };
        let value = serde_json::to_value(&payload).unwrap_or_else(|error| panic!("{error}"));
        let group = value["groups"][0].clone();
        let keys: Vec<&str> = group
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "description",
                "group_key",
                "id",
                "is_builtin",
                "item_count",
                "member_count",
                "orphan_item_count",
                "title"
            ]
        );
    }
}
