//! 读取一个权限组的详情：条目（含孤儿标记）、成员与内置组标记。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::addon::access::domain::groups::resolution::{catalog_permissions, orphan_items};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) GetGroupInput {
        #[param(source = path)]
        group_id: Key::new()
            .title("权限组")
            .require(true),
    }
}

/// 一条组条目：`is_orphan` 表示它已不在权限目录里（spec §8.4，只标记不清理）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GroupItemView {
    permission: String,
    is_orphan: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GetGroupResult {
    id: i64,
    group_key: String,
    title: String,
    description: Option<String>,
    /// 内置全权组：它的权限由权限目录计算，条目表里没有可展示的授权事实，
    /// 前端必须特判（spec §15 第 3 条）。
    effective_all: bool,
    items: Vec<GroupItemView>,
    /// 成员用户 ID，按 `user_id` 升序（与扇出失效的锁序同源）。
    members: Vec<i64>,
}

/// 把条目字符串投影成响应条目：按权限字符串稳定排序，孤儿条目原样保留并打标记。
fn item_views(items: &[String], catalog: &[String]) -> Vec<GroupItemView> {
    let mut sorted: Vec<&String> = items.iter().collect();
    sorted.sort_unstable();
    let orphans: BTreeSet<&str> = orphan_items(items, catalog).into_iter().collect();
    sorted
        .into_iter()
        .map(|permission| GroupItemView {
            permission: permission.clone(),
            is_orphan: orphans.contains(permission.as_str()),
        })
        .collect()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: GetGroupInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let mut transaction = ctx.tools().mysql()?.read_only_transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        let catalog = catalog_permissions(access.permission_catalog())?;
        let items = access
            .groups()
            .list_items_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        Ok(GetGroupResult {
            id: group.id,
            effective_all: group.group_key == SYSTEM_ADMIN_GROUP_KEY,
            group_key: group.group_key,
            title: group.title,
            description: group.description,
            items: item_views(&items, &catalog),
            members,
        })
    }
    .await;
    let view = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(view, "权限组详情")
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("get_group"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&access))
        })
        .route(HttpMethod::Get, "/api/v1/access/groups/{group_id}")
        .display_name("权限组详情")
        .description("读取权限组的条目（含孤儿标记）、成员与内置组标记")
        .permissions(["access.groups.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::{ParamInput, ParamSource};

    fn catalog() -> Vec<String> {
        vec![
            "access.groups.read".to_string(),
            "access.groups.write".to_string(),
        ]
    }

    #[test]
    fn item_views_flag_only_the_permissions_missing_from_the_catalog() {
        let items = vec![
            "removed.module.act".to_string(),
            "access.groups.read".to_string(),
        ];
        let views = item_views(&items, &catalog());
        let projected: Vec<(&str, bool)> = views
            .iter()
            .map(|view| (view.permission.as_str(), view.is_orphan))
            .collect();
        assert_eq!(
            projected,
            [("access.groups.read", false), ("removed.module.act", true)],
            "孤儿标记必须只落在目录里没有的权限上，且输出稳定排序"
        );
        assert_eq!(views.len(), items.len(), "孤儿条目只标记，绝不丢弃");
    }

    #[test]
    fn group_id_comes_from_the_path_not_the_body() {
        let params = <GetGroupInput as ParamInput>::params();
        let group_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "group_id")
            .unwrap_or_else(|| panic!("应声明 group_id 参数"));
        assert_eq!(group_id.source, ParamSource::Path);
        assert!(group_id.required);

        let injected = serde_json::from_value::<GetGroupInput>(serde_json::json!({
            "group_id": 3,
            "title": "运维"
        }));
        assert!(injected.is_err(), "详情接口不接受任何过滤字段");
    }

    #[test]
    fn detail_response_names_the_frontend_contract() {
        let payload = GetGroupResult {
            id: 3,
            group_key: "ops".to_string(),
            title: "运维".to_string(),
            description: None,
            effective_all: false,
            items: item_views(&["access.groups.read".to_string()], &catalog()),
            members: vec![7],
        };
        let value = serde_json::to_value(&payload).unwrap_or_else(|error| panic!("{error}"));
        let keys: Vec<&str> = value
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "description",
                "effective_all",
                "group_key",
                "id",
                "items",
                "members",
                "title"
            ]
        );
        assert_eq!(value["items"][0]["is_orphan"], serde_json::json!(false));
    }
}
