//! 批量撤销直授权限（事务原子 + 幂等跳过 + 按用户合并版本递增）。
//!
//! 任一失败整体回滚，成功响应里的 `failed` 明细恒为空；失败时以带条目索引的错误返回。
//! 撤销不做目录成员校验（已从 Catalog 移除的权限也必须能清理），与单条撤销一致。
//!
//! 与单条 revoke_permission 不同，批量撤销不调用 revoke_by_subject 做 Redis 即时收敛——
//! 批量版本变更经 Outbox Worker 异步发布 + Redis 5s TTL 回查 MySQL 生效（与系统内其余
//! 失效路径同款）；已签发 token 在版本缓存过期前仍可用，最坏 5s 窗口。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
use crate::audit;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

/// 批量撤销的最大条目数（防 DoS；与批量授予一致）。
const MAX_BATCH_ITEMS: usize = 100;

/// 批量撤销的单条输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchRevokeItem {
    user_id: i64,
    permission: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchRevokePermissionsInput {
    items: Vec<BatchRevokeItem>,
}

impl ParamInput for BatchRevokePermissionsInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 批量撤销结果。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BatchRevokePermissionsResult {
    /// 实际删除的条目数。
    succeeded: usize,
    /// 幂等跳过的条目数（目标用户本就没有该直授）。
    skipped: usize,
    /// 失败明细。事务原子语义下任一失败整体回滚，因此成功响应的该字段恒为空；
    /// 失败时以带条目索引的错误返回，不产生部分写入。
    failed: Vec<BatchFailedItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BatchFailedItem {
    index: usize,
    reason: String,
}

/// 批量撤销计划：按既有直授行逐条决策 删除/跳过，并合并出需要递增授权版本
/// 的用户集合（同 user_id 多条只计一次，保证版本 +1 而不是 +N）。
///
/// 纯函数，不读库：`existing` 由调用方在行锁之后一次性读入。
fn plan_batch_revokes(
    items: &[BatchRevokeItem],
    existing: &HashSet<(i64, String)>,
) -> (Vec<(i64, String)>, usize, BTreeSet<i64>) {
    let mut deletions = Vec::new();
    let mut skipped = 0;
    let mut changed_users = BTreeSet::new();
    // 本批次内已出现过的 (user_id, permission)：重复条目按幂等跳过，避免对同一
    // 行计两次删除（判据快照是写入前的，不去重会把一条删除重复计入 succeeded）。
    let mut planned = HashSet::new();
    for item in items {
        let key = (item.user_id, item.permission.clone());
        if !planned.insert(key.clone()) {
            skipped += 1;
            continue;
        }
        if existing.contains(&key) {
            deletions.push(key);
            changed_users.insert(item.user_id);
        } else {
            skipped += 1;
        }
    }
    (deletions, skipped, changed_users)
}

/// 逐条前置校验（不读库）：用户 id 正整数、权限键格式合法。
///
/// 权限格式与 `params!` 的 `PERMISSION_PATTERN` 声明同源：点分隔的小写段、
/// 至少两段；撤销不校验目录成员，因此格式检查是唯一的输入卫生闸。
fn validate_revoke_items(items: &[BatchRevokeItem]) -> Result<(), BaseError> {
    for (index, item) in items.iter().enumerate() {
        if item.user_id <= 0 {
            return Err(BaseError::ParamInvalid(
                format!("items[{index}].user_id"),
                "目标用户必须是正整数".to_string(),
            ));
        }
        if !is_valid_permission_format(&item.permission) {
            return Err(BaseError::ParamInvalid(
                format!("items[{index}].permission"),
                format!(
                    "权限键必须是点分隔的小写段（{}），最长 {PERMISSION_MAX_LENGTH} 字符",
                    PERMISSION_PATTERN
                ),
            ));
        }
    }
    Ok(())
}

/// 权限键格式校验（与 `PERMISSION_PATTERN` 等价的段式检查，避免引入正则依赖）。
fn is_valid_permission_format(permission: &str) -> bool {
    if permission.is_empty() || permission.len() > PERMISSION_MAX_LENGTH {
        return false;
    }
    let mut segments = 0;
    for segment in permission.split('.') {
        segments += 1;
        let mut chars = segment.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    segments >= 2
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BatchRevokePermissionsInput,
    access: Arc<Access>,
) -> Result<BatchRevokePermissionsResult, BaseError> {
    if input.items.len() > MAX_BATCH_ITEMS {
        return Err(BaseError::ParamInvalid(
            "items".to_string(),
            format!("批量上限 {MAX_BATCH_ITEMS} 条"),
        ));
    }
    validate_revoke_items(&input.items)?;

    let operator_id = ctx.actor()?.user_id();
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 涉及的 user_id 去重后按升序锁行：全局锁序要求 users 行一律升序整批获取；
        // 锁句柄保留供版本递增（CAS 需要锁时的 authz_version）。
        let user_ids: BTreeSet<i64> = input.items.iter().map(|item| item.user_id).collect();
        let mut locked_by_user = HashMap::new();
        for user_id in &user_ids {
            let locked = access
                .authorization()
                .lock_authorization_version(ctx.tools().mysql()?.pool(), &mut transaction, *user_id)
                .await?;
            locked_by_user.insert(*user_id, locked);
        }

        // 既有直授行在行锁之后一次性读入（判据读晚于行锁）。
        let existing: HashSet<(i64, String)> = if user_ids.is_empty() {
            HashSet::new()
        } else {
            let ids: Vec<i64> = user_ids.iter().copied().collect();
            let mut set = HashSet::new();
            for record in access
                .grants()
                .list_by_users_in_tx(&ctx, &mut transaction, &ids)
                .await?
            {
                set.insert((record.user_id, record.permission));
            }
            set
        };

        let (deletions, skipped, changed_users) = plan_batch_revokes(&input.items, &existing);
        for (user_id, permission) in &deletions {
            access
                .grants()
                .delete_in_tx(&ctx, &mut transaction, *user_id, permission)
                .await?;
        }

        // 每个有变更的用户恰好递增一次授权版本（含授权 Outbox）。
        for user_id in &changed_users {
            let locked = locked_by_user
                .get(user_id)
                .ok_or_else(|| BaseError::ConfigError("批量锁句柄缺失".to_string()))?;
            access
                .authorization()
                .increment_locked_authorization_version(&mut transaction, locked)
                .await?;
        }

        // 审计：每个有变更的用户一条成功事件，摘要列出本次撤销的权限。
        for user_id in &changed_users {
            let permissions: Vec<String> = deletions
                .iter()
                .filter(|(target, _)| *target == *user_id)
                .map(|(_, permission)| permission.clone())
                .collect();
            let event = audit::succeeded_event(
                &ctx,
                None,
                Some(audit::entity("user", operator_id)?),
                audit::entity("user", *user_id)?,
                Some(audit::summary([("permissions", json!(permissions))])?),
                None,
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }

        Ok(BatchRevokePermissionsResult {
            succeeded: deletions.len(),
            skipped,
            failed: Vec::new(),
        })
    }
    .await;
    Access::finish_transaction(transaction, result).await
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("batch_revoke_permissions"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/grants/batch-revoke")
        .display_name("批量撤销权限")
        .description("批量撤销直授权限：事务原子，任一失败整体回滚；本就没有该直授的条目幂等跳过")
        .permissions(["access.grants.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取出校验失败的错误值（不用 `.unwrap_err()`：clippy `unwrap_used` 是 deny，
    /// 且它对 `#[cfg(test)]` 同样生效，仓库既有纪律）。
    fn expected_error(result: Result<(), BaseError>) -> BaseError {
        match result {
            Ok(()) => panic!("期望校验失败，实际通过"),
            Err(error) => error,
        }
    }

    #[test]
    fn input_rejects_unknown_fields_and_missing_items() {
        let injected = serde_json::from_value::<BatchRevokePermissionsInput>(serde_json::json!({
            "items": [{"user_id": 7, "permission": "access.grants.read", "reason": "cleanup"}]
        }));
        assert!(injected.is_err(), "客户端不能注入 reason 等额外字段");

        let missing_items = serde_json::from_value::<BatchRevokePermissionsInput>(
            serde_json::json!({ "user_id": 7 }),
        );
        assert!(missing_items.is_err());

        let valid = serde_json::from_value::<BatchRevokePermissionsInput>(serde_json::json!({
            "items": [
                {"user_id": 7, "permission": "access.grants.read"},
                {"user_id": 8, "permission": "access.grants.write"}
            ]
        }))
        .unwrap_or_else(|error| panic!("合法批量输入应可解析: {error}"));
        assert_eq!(valid.items.len(), 2);
    }

    #[test]
    fn plan_skips_missing_grants_and_merges_changed_users() {
        let items = vec![
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
            },
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.write".to_string(),
            },
            BatchRevokeItem {
                user_id: 8,
                permission: "access.grants.read".to_string(),
            },
        ];
        let mut existing = HashSet::new();
        // user 7 只有 read 一条直授：write 条目幂等跳过；user 8 的 read 存在。
        existing.insert((7, "access.grants.read".to_string()));
        existing.insert((8, "access.grants.read".to_string()));

        let (deletions, skipped, changed_users) = plan_batch_revokes(&items, &existing);

        assert_eq!(skipped, 1, "不存在的直授计入幂等跳过");
        assert_eq!(deletions.len(), 2);
        assert!(deletions.contains(&(7, "access.grants.read".to_string())));
        assert_eq!(changed_users.len(), 2);
        assert!(changed_users.contains(&7));
        assert!(changed_users.contains(&8));
    }

    #[test]
    fn plan_treats_intra_batch_duplicates_as_skipped() {
        // 同一批次内重复的 (user_id, permission)：首次计入删除，后续按幂等跳过，
        // 不会把同一条删除重复计入 succeeded。
        let items = vec![
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
            },
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
            },
        ];
        let mut existing = HashSet::new();
        existing.insert((7, "access.grants.read".to_string()));

        let (deletions, skipped, changed_users) = plan_batch_revokes(&items, &existing);

        assert_eq!(deletions.len(), 1, "重复条目只删一次");
        assert_eq!(skipped, 1, "重复条目计入幂等跳过");
        assert_eq!(changed_users.len(), 1);
    }

    #[test]
    fn plan_merges_same_user_version_increment_to_once() {
        // 同 user_id 多条删除合并为一次版本递增（+1 而不是 +N）。
        let items = vec![
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
            },
            BatchRevokeItem {
                user_id: 7,
                permission: "access.grants.write".to_string(),
            },
        ];
        let mut existing = HashSet::new();
        existing.insert((7, "access.grants.read".to_string()));
        existing.insert((7, "access.grants.write".to_string()));

        let (deletions, skipped, changed_users) = plan_batch_revokes(&items, &existing);

        assert_eq!(skipped, 0);
        assert_eq!(deletions.len(), 2);
        assert_eq!(
            changed_users.len(),
            1,
            "两条删除同属 user 7，版本只递增一次"
        );
        assert!(changed_users.contains(&7));
    }

    #[test]
    fn validation_rejects_negative_user_id_and_malformed_permission() {
        let items = vec![BatchRevokeItem {
            user_id: -1,
            permission: "access.grants.read".to_string(),
        }];
        let error = expected_error(validate_revoke_items(&items));
        assert!(
            matches!(error, BaseError::ParamInvalid(ref field, _) if field == "items[0].user_id"),
            "非正整数 user_id 必须报错: {error}"
        );

        for bad in [
            "UPPER.CASE",
            "no_segments",
            "bad..double",
            "bad-char.seg",
            "",
        ] {
            let items = vec![BatchRevokeItem {
                user_id: 7,
                permission: bad.to_string(),
            }];
            let error = expected_error(validate_revoke_items(&items));
            assert!(
                matches!(error, BaseError::ParamInvalid(ref field, _) if field == "items[0].permission"),
                "非法权限键 {bad:?} 必须报错: {error}"
            );
        }

        let items = vec![BatchRevokeItem {
            user_id: 7,
            permission: "access.grants.read".to_string(),
        }];
        assert!(validate_revoke_items(&items).is_ok(), "合法权限键应通过");
    }
}
