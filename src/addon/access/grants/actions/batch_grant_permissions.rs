//! 批量授予直授权限（事务原子 + 幂等跳过 + 按用户合并版本递增）。
//!
//! 任一失败整体回滚，成功响应里的 `failed` 明细恒为空；失败时以带条目索引的错误返回
//! （错误信息里的 `items[N]` 即失败位置），不产生部分写入。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::ensure_may_grant_permission_in_tx;
use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
use crate::addon::access::domain::repository::{current_unix_timestamp, is_expired};
use crate::audit;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

/// 批量授予的最大条目数（防 DoS；与批量撤销一致）。
const MAX_BATCH_ITEMS: usize = 100;

/// 批量授予的单条输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchGrantItem {
    user_id: i64,
    permission: String,
    /// Unix 秒；缺省 = 永久有效。过期后权限在解析侧失效、行保留做审计。
    #[serde(default)]
    expires_at: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchGrantPermissionsInput {
    items: Vec<BatchGrantItem>,
}

impl ParamInput for BatchGrantPermissionsInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 批量授予结果。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BatchGrantPermissionsResult {
    /// 实际写入（新增 + 续期）的条目数。
    succeeded: usize,
    /// 幂等跳过的条目数（目标用户已持有**有效**直授）。
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

/// 逐条写入决策：`Insert` = 新授权，`Renew` = 过期行续期（UPDATE 保留审计痕迹）。
#[derive(Debug, Clone, PartialEq)]
enum PlanWrite {
    Insert {
        user_id: i64,
        permission: String,
        expires_at: Option<i64>,
    },
    Renew {
        user_id: i64,
        permission: String,
        expires_at: Option<i64>,
    },
}

/// 批量授予计划：按既有直授行逐条决策 插入/续期/跳过，并合并出需要递增授权版本
/// 的用户集合（同 user_id 多条只计一次，保证版本 +1 而不是 +N）。
///
/// 纯函数，不读库：`existing` 由调用方在行锁之后一次性读入。
fn plan_batch_grants(
    items: &[BatchGrantItem],
    existing: &HashMap<(i64, String), Option<i64>>,
    now: i64,
) -> (Vec<PlanWrite>, usize, BTreeSet<i64>) {
    let mut writes = Vec::new();
    let mut skipped = 0;
    let mut changed_users = BTreeSet::new();
    // 本批次内已出现过的 (user_id, permission)：重复条目按幂等跳过，避免对同一
    // 唯一键写第二行（判据快照是写入前的，不去重就会在第二次 INSERT 撞唯一键）。
    let mut planned = std::collections::HashSet::new();
    for item in items {
        let key = (item.user_id, item.permission.clone());
        if !planned.insert(key.clone()) {
            skipped += 1;
            continue;
        }
        match existing.get(&key) {
            None => {
                writes.push(PlanWrite::Insert {
                    user_id: item.user_id,
                    permission: item.permission.clone(),
                    expires_at: item.expires_at,
                });
                changed_users.insert(item.user_id);
            }
            Some(expires_at) if is_expired(*expires_at, now) => {
                // 已过期行按审计保留、唯一键不允许插第二行，只能原地续期。
                writes.push(PlanWrite::Renew {
                    user_id: item.user_id,
                    permission: item.permission.clone(),
                    expires_at: item.expires_at,
                });
                changed_users.insert(item.user_id);
            }
            Some(_) => skipped += 1,
        }
    }
    (writes, skipped, changed_users)
}

/// 逐条前置校验（不读库）：用户 id 正整数、过期时间必须晚于当前时刻、权限已声明。
///
/// 全部通过才进入事务写入阶段——这是「一条非法 → 整体回滚、无部分写入」的第一道闸。
fn validate_grant_items(
    items: &[BatchGrantItem],
    catalog: &PermissionCatalogHandle,
    now: i64,
) -> Result<(), BaseError> {
    for (index, item) in items.iter().enumerate() {
        if item.user_id <= 0 {
            return Err(BaseError::ParamInvalid(
                format!("items[{index}].user_id"),
                "目标用户必须是正整数".to_string(),
            ));
        }
        if item.expires_at.is_some_and(|expires_at| expires_at <= now) {
            return Err(BaseError::ParamInvalid(
                format!("items[{index}].expires_at"),
                "过期时间必须大于当前时间".to_string(),
            ));
        }
        if let Err(BaseError::ParamInvalid(_, message)) = catalog.ensure_declared(&item.permission)
        {
            return Err(BaseError::ParamInvalid(
                format!("items[{index}].permission"),
                message,
            ));
        }
    }
    Ok(())
}

/// 给条目错误补充 `items[N]` 索引前缀，便于调用方定位失败位置。
fn annotate_index(index: usize, error: BaseError) -> BaseError {
    match error {
        BaseError::PermissionDenied(message) => {
            BaseError::PermissionDenied(format!("items[{index}]: {message}"))
        }
        BaseError::Unauthorized(message) => {
            BaseError::Unauthorized(format!("items[{index}]: {message}"))
        }
        other => other,
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BatchGrantPermissionsInput,
    access: Arc<Access>,
) -> Result<BatchGrantPermissionsResult, BaseError> {
    if input.items.len() > MAX_BATCH_ITEMS {
        return Err(BaseError::ParamInvalid(
            "items".to_string(),
            format!("批量上限 {MAX_BATCH_ITEMS} 条"),
        ));
    }
    let now = current_unix_timestamp()?;
    validate_grant_items(&input.items, access.permission_catalog(), now)?;

    let operator_id = ctx.actor()?.user_id();
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 涉及的 user_id 去重后按升序锁行：全局锁序要求 users 行一律升序整批获取
        // （与 `lock_users_ascending_in_tx` 同一纪律），本函数保留锁句柄供
        // 启用状态判读与版本递增（CAS 需要锁时的 authz_version）。
        let user_ids: BTreeSet<i64> = input.items.iter().map(|item| item.user_id).collect();
        let mut locked_by_user = HashMap::new();
        for user_id in &user_ids {
            let locked = access
                .authorization()
                .lock_authorization_version(ctx.tools().mysql()?.pool(), &mut transaction, *user_id)
                .await?;
            locked_by_user.insert(*user_id, locked);
        }

        // 既有直授行在行锁之后一次性读入（「第一条普通读即快照起点」纪律：判据读
        // 晚于行锁；此后同事务内无人能改写这批用户的授权事实）。
        let existing: HashMap<(i64, String), Option<i64>> = if user_ids.is_empty() {
            HashMap::new()
        } else {
            let ids: Vec<i64> = user_ids.iter().copied().collect();
            let mut map = HashMap::new();
            for record in access
                .grants()
                .list_by_users_in_tx(&ctx, &mut transaction, &ids)
                .await?
            {
                map.insert((record.user_id, record.permission), record.expires_at);
            }
            map
        };

        // 逐条判据（G2 闸门 + 启用状态），全部通过才进入写入阶段；任一失败整体回滚。
        for (index, item) in input.items.iter().enumerate() {
            let locked = locked_by_user
                .get(&item.user_id)
                .ok_or_else(|| BaseError::ConfigError("批量锁句柄缺失".to_string()))?;
            if !locked.is_active() {
                return Err(BaseError::Unauthorized(format!(
                    "items[{index}]: 目标用户已停用"
                )));
            }
            ensure_may_grant_permission_in_tx(
                &access,
                &ctx,
                &mut transaction,
                operator_id,
                &item.permission,
            )
            .await
            .map_err(|error| annotate_index(index, error))?;
        }

        // 写入阶段：插入/续期/幂等跳过，按计划执行。
        let (writes, skipped, changed_users) = plan_batch_grants(&input.items, &existing, now);
        for write in &writes {
            match write {
                PlanWrite::Insert {
                    user_id,
                    permission,
                    expires_at,
                } => {
                    access
                        .grants()
                        .insert_in_tx(
                            &ctx,
                            &mut transaction,
                            *user_id,
                            permission,
                            operator_id,
                            *expires_at,
                        )
                        .await?;
                }
                PlanWrite::Renew {
                    user_id,
                    permission,
                    expires_at,
                } => {
                    access
                        .grants()
                        .renew_in_tx(
                            &ctx,
                            &mut transaction,
                            *user_id,
                            permission,
                            operator_id,
                            *expires_at,
                        )
                        .await?;
                }
            }
        }

        // 每个有变更的用户恰好递增一次授权版本（含授权 Outbox）：同 user_id 多条
        // 合并为 +1，而不是逐条 +N；升序遍历与锁序一致。
        for user_id in &changed_users {
            let locked = locked_by_user
                .get(user_id)
                .ok_or_else(|| BaseError::ConfigError("批量锁句柄缺失".to_string()))?;
            access
                .authorization()
                .increment_locked_authorization_version(&mut transaction, locked)
                .await?;
        }

        // 审计：每个有变更的用户一条成功事件，摘要列出本次批量授予/续期的权限。
        for user_id in &changed_users {
            let permissions: Vec<String> = writes
                .iter()
                .filter(|write| write_user_id(write) == *user_id)
                .map(|write| write_permission(write))
                .collect();
            let event = audit::succeeded_event(
                &ctx,
                None,
                Some(audit::entity("user", operator_id)?),
                audit::entity("user", *user_id)?,
                None,
                Some(audit::summary([("permissions", json!(permissions))])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }

        Ok(BatchGrantPermissionsResult {
            succeeded: writes.len(),
            skipped,
            failed: Vec::new(),
        })
    }
    .await;
    Access::finish_transaction(transaction, result).await
}

fn write_user_id(write: &PlanWrite) -> i64 {
    match write {
        PlanWrite::Insert { user_id, .. } | PlanWrite::Renew { user_id, .. } => *user_id,
    }
}

fn write_permission(write: &PlanWrite) -> String {
    match write {
        PlanWrite::Insert { permission, .. } | PlanWrite::Renew { permission, .. } => {
            permission.clone()
        }
    }
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("batch_grant_permissions"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/grants/batch")
        .display_name("批量授予权限")
        .description("批量授予直授权限：事务原子，任一失败整体回滚；已持有有效直授的条目幂等跳过")
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
        let injected = serde_json::from_value::<BatchGrantPermissionsInput>(serde_json::json!({
            "items": [{"user_id": 7, "permission": "access.grants.read", "granted_by": 1}]
        }));
        assert!(injected.is_err(), "客户端不能注入 granted_by 等内部字段");

        let missing_items = serde_json::from_value::<BatchGrantPermissionsInput>(
            serde_json::json!({ "user_id": 7 }),
        );
        assert!(missing_items.is_err());

        let valid = serde_json::from_value::<BatchGrantPermissionsInput>(serde_json::json!({
            "items": [
                {"user_id": 7, "permission": "access.grants.read"},
                {"user_id": 8, "permission": "access.grants.write", "expires_at": 1_800_000_000}
            ]
        }))
        .unwrap_or_else(|error| panic!("合法批量输入应可解析: {error}"));
        assert_eq!(valid.items.len(), 2);
        assert_eq!(valid.items[0].expires_at, None, "缺省=永久");
        assert_eq!(valid.items[1].expires_at, Some(1_800_000_000));
    }

    #[test]
    fn plan_skips_effective_grants_and_merges_changed_users() {
        let now = 1_700_000_000;
        let items = vec![
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
                expires_at: None,
            },
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.write".to_string(),
                expires_at: None,
            },
            BatchGrantItem {
                user_id: 8,
                permission: "access.grants.read".to_string(),
                expires_at: None,
            },
        ];
        // user 7 两条：一条已持有有效直授（跳过）、一条新授予；user 8 的条目新授予。
        let mut existing = HashMap::new();
        existing.insert((7, "access.grants.read".to_string()), Some(now + 3_600));

        let (writes, skipped, changed_users) = plan_batch_grants(&items, &existing, now);

        assert_eq!(skipped, 1, "已持有有效直授的条目计入幂等跳过");
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().any(|write| *write
            == PlanWrite::Insert {
                user_id: 7,
                permission: "access.grants.write".to_string(),
                expires_at: None,
            }));
        assert_eq!(changed_users.len(), 2, "7 与 8 都有实际变更");
        assert!(changed_users.contains(&7));
        assert!(changed_users.contains(&8));
    }

    #[test]
    fn plan_treats_intra_batch_duplicates_as_skipped() {
        // 同一批次内重复的 (user_id, permission)：首次计入写入，后续按幂等跳过，
        // 不会对同一唯一键写第二行（否则第二次 INSERT 撞唯一键、整体回滚）。
        let now = 1_700_000_000;
        let items = vec![
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
                expires_at: None,
            },
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
                expires_at: None,
            },
        ];
        let (writes, skipped, changed_users) = plan_batch_grants(&items, &HashMap::new(), now);

        assert_eq!(writes.len(), 1, "重复条目只写一次");
        assert_eq!(skipped, 1, "重复条目计入幂等跳过");
        assert_eq!(changed_users.len(), 1);
    }

    #[test]
    fn plan_renews_expired_rows_instead_of_skipping() {
        let now = 1_700_000_000;
        let items = vec![BatchGrantItem {
            user_id: 7,
            permission: "access.grants.read".to_string(),
            expires_at: Some(now + 3_600),
        }];
        let mut existing = HashMap::new();
        // 行存在但已过期：按审计保留 + 唯一键约束，必须走续期而不是幂等跳过。
        existing.insert((7, "access.grants.read".to_string()), Some(now - 1));

        let (writes, skipped, changed_users) = plan_batch_grants(&items, &existing, now);

        assert_eq!(skipped, 0, "过期行不算「已持有」");
        assert_eq!(
            writes,
            vec![PlanWrite::Renew {
                user_id: 7,
                permission: "access.grants.read".to_string(),
                expires_at: Some(now + 3_600),
            }]
        );
        assert_eq!(changed_users.len(), 1);
    }

    #[test]
    fn plan_merges_same_user_version_increment_to_once() {
        // 同 user_id 多条写入合并为一次版本递增（+1 而不是 +N）：
        // changed_users 只含该 user 一次。
        let now = 1_700_000_000;
        let items = vec![
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.read".to_string(),
                expires_at: None,
            },
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.write".to_string(),
                expires_at: None,
            },
            BatchGrantItem {
                user_id: 7,
                permission: "access.grants.revoke".to_string(),
                expires_at: None,
            },
        ];
        let (_, _, changed_users) = plan_batch_grants(&items, &HashMap::new(), now);

        assert_eq!(
            changed_users.len(),
            1,
            "三条款目同属 user 7，版本只递增一次"
        );
        assert!(changed_users.contains(&7));
    }

    #[test]
    fn validation_rejects_undeclared_permission_and_past_expiry() {
        let now = 1_700_000_000;
        // 空投影目录：任何权限都未声明，fail-closed 覆盖「未声明权限」分支。
        let catalog = PermissionCatalogHandle::new();
        catalog
            .install(crate::addon::access::domain::permission_catalog::project_permissions(&[]))
            .unwrap_or_else(|error| panic!("测试目录安装应成功: {error}"));

        // 过期时间校验在权限声明之前：先验证过期分支，再验证未声明分支。
        let items = vec![BatchGrantItem {
            user_id: 7,
            permission: "access.grants.read".to_string(),
            expires_at: Some(now),
        }];
        let error = expected_error(validate_grant_items(&items, &catalog, now));
        assert!(
            matches!(error, BaseError::ParamInvalid(ref field, _) if field == "items[0].expires_at"),
            "过期时间不晚于 now 必须以条目索引报错: {error}"
        );

        let items = vec![BatchGrantItem {
            user_id: 7,
            permission: "access.grants.write".to_string(),
            expires_at: None,
        }];
        let error = expected_error(validate_grant_items(&items, &catalog, now));
        assert!(
            matches!(error, BaseError::ParamInvalid(ref field, _) if field == "items[0].permission"),
            "未声明权限必须在写入前以条目索引报错: {error}"
        );
    }
}
