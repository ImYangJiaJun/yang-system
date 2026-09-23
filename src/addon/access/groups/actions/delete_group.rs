//! 删除一个权限组。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) DeleteGroupInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DeleteGroupResult {
    id: i64,
    group_key: String,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteGroupInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        // 内置全权组是引导流程的产物，删掉它等于把系统锁死在无管理员状态。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组不可删除".to_string(),
            ));
        }
        // 应用层前置检查给出可读错误；数据库外键 RESTRICT 兜底并发窗口（spec §8.3）。
        let member_count = access
            .groups()
            .count_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        if member_count > 0 {
            return Err(group_has_members(Some(member_count)));
        }
        let affected_rows = match access
            .groups()
            .delete_group_in_tx(&ctx, &mut transaction, group.id)
            .await
        {
            Ok(rows) => rows,
            // 兜底路径与前置检查看到的是同一件事（「还有人引用这个组」），必须折算成
            // 同一个可读拒绝，绝不能把 500 泄漏给客户端。
            Err(error) if is_referential_constraint(&error) => {
                return Err(group_has_members(None));
            }
            Err(error) => return Err(error),
        };
        if affected_rows == 0 {
            return Err(BaseError::RecordNotFound("权限组".to_string()));
        }
        // 条目表没有到本表的外键，数据库不会替我们清理，留着就是悬空条目行。
        access
            .groups()
            .delete_items_of_group_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group.id)?,
            Some(audit::summary([
                ("group_key", json!(group.group_key)),
                ("title", json!(group.title)),
            ])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(group.group_key)
    }
    .await;
    let group_key = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        DeleteGroupResult {
            id: input.group_id,
            group_key,
        },
        "权限组已删除",
    )
}

/// 「组内非空」的统一拒绝：前置检查与数据库兜底共用同一条错误，客户端不必区分
/// 自己撞到了哪一层。
///
/// 兜底发生在并发窗口内，刚写入的成员行对当前事务快照还不可见，因此那里不复述
/// 成员数（`None`）。
///
/// 计划此处写的是 `BaseError::Conflict`（409）：`yang_base::BaseError` 并没有该变体，
/// 且设计 §9.3 同时要求「不为个别用例扩展框架错误类型」，因此沿用 `ParamInvalid`
/// ——与 `ensure_member_limit` 对同类「资源状态冲突」的取舍一致。
fn group_has_members(member_count: Option<u64>) -> BaseError {
    let message = match member_count {
        Some(count) => format!("该权限组仍有 {count} 名成员，请先移出成员"),
        None => "该权限组仍有成员，请先移出成员".to_string(),
    };
    BaseError::ParamInvalid("group_id".to_string(), message)
}

/// 该错误是否表示「外键 RESTRICT 拒绝了这次删除」。
///
/// `permission_group` 上只有 `user_group` 的外键能被 DELETE 违反，因此认出
/// 「约束类错误」就足够，不必去解析 MySQL 的报文案（跨库、跨版本都不稳定）。
fn is_referential_constraint(error: &BaseError) -> bool {
    matches!(
        error,
        BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_group"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/delete")
        .display_name("删除权限组")
        .description("删除一个权限组；组内仍有成员时拒绝")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_requires_a_group_id_from_the_body() {
        let injected = serde_json::from_value::<DeleteGroupInput>(serde_json::json!({
            "group_id": 3,
            "force": true
        }));
        assert!(injected.is_err(), "客户端不能注入 force 等额外字段");

        let missing = serde_json::from_value::<DeleteGroupInput>(serde_json::json!({}));
        assert!(missing.is_err(), "缺少 group_id 必须被拒绝");

        let params = <DeleteGroupInput as ParamInput>::params();
        assert_eq!(params.as_slice().len(), 1);
        assert!(params.as_slice()[0].required);
    }

    #[test]
    fn member_conflict_is_a_readable_client_error_with_the_count() {
        let error = group_has_members(Some(3));
        assert_eq!(error.code(), 700005, "必须是既有的 ParamInvalid 错误码");
        let message = error.to_string();
        assert!(message.contains('3'), "必须报出实际成员数，实际 {message}");
        assert!(
            message.contains("移出成员"),
            "必须给出可执行的处置办法，实际 {message}"
        );
        // 并发兜底路径拿不到可信成员数，但拒绝结论与 HTTP 语义必须一致。
        assert_eq!(group_has_members(None).code(), error.code());
    }

    #[test]
    fn only_constraint_failures_are_treated_as_the_foreign_key_backstop() {
        // 外键 RESTRICT 在 yang-db 里是 ConstraintError，到 BaseError 层是
        // DatabaseExecuteFailed——认错变体会把 500 泄漏出去，或把真实故障吞成 400。
        assert!(is_referential_constraint(
            &BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(
                "Cannot delete or update a parent row".to_string()
            ))
        ));
        assert!(!is_referential_constraint(
            &BaseError::DatabaseExecuteFailed(yang_db::DbError::Unknown("连接被重置".to_string()))
        ));
        assert!(!is_referential_constraint(&BaseError::RecordNotFound(
            "权限组".to_string()
        )));
    }
}
