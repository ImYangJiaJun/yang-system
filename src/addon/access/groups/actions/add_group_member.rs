//! 把一个用户加入权限组（幂等），并守住 spec §8.1 的防自提权不变量与成员上限。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    assert_no_self_escalation, effective_permissions_of_in_tx,
    ensure_may_modify_members_of_group_in_tx, ensure_member_limit, invalidate_users_in_tx,
    lock_users_ascending_in_tx, simulate_after_join,
};
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AddGroupMemberInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
        user_id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AddGroupMemberResult {
    group_id: i64,
    user_id: i64,
    /// 本次是否真的新增了成员关系：重复添加同一成员时为 `false`，既不递增任何人的
    /// 授权版本，也不写审计事件（spec §9.2 的幂等语义）。
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AddGroupMemberInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    // 目标用户必须先存在：外键只能告诉我们「这次插入失败了」，分不出是用户不存在
    // 还是组被并发删掉。先读一次版本快照，剩下的失败原因就只有后者（见下方兜底）。
    if access
        .authorization()
        .find_authorization_version(ctx.tools().mysql()?.pool(), input.user_id)
        .await?
        .is_none()
    {
        return Err(BaseError::UserNotFound(format!("用户 {}", input.user_id)));
    }

    let changed = join_group_once(&ctx, &access, &input, operator_id).await?;

    ApiResponse::success(
        AddGroupMemberResult {
            group_id: input.group_id,
            user_id: input.user_id,
            changed,
        },
        if changed {
            "成员已加入权限组，其刷新会话后生效"
        } else {
            "该用户已在此权限组中"
        },
    )
}

/// 在一条事务里执行一次「加入成员」，返回本次是否真的新增了成员关系。
///
/// 事务体独立成函数，是为了让 [`handle`] 只负责「前置校验 + 组装响应」，而把
/// 「锁 → 读 → 判 → 写」的全部顺序纪律收在一处——这里的顺序本身就是不变量（见下）。
async fn join_group_once(
    ctx: &ActionContext,
    access: &Access,
    input: &AddGroupMemberInput,
    operator_id: i64,
) -> Result<bool, BaseError> {
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 第一把锁：users 行。按 `user_id` 升序一次性锁定本事务会触碰的两类用户行
        // ——**操作者自己**与**目标用户**。
        //
        // 操作者行是防自提权的串行化点：`add_group_item` 与 `add_group_member` 的判据
        // 都是无锁 SELECT，同一账号并发发起两者时会各自读到对方未提交的快照、各自
        // 通过 §8.1 子集校验，进而完成自提权。把操作者行纳入本事务的锁批后，
        // 后到者一定在先到者提交之后才读到判据快照，两条路径不再能互相穿透。
        //
        // 这里把操作者行**显式**写进锁批，而不是依赖「目标恰好就是操作者」这个巧合：
        // §8.1 的判据只在 `input.user_id == operator_id` 时才生效，那一刻两行确实是同一行；
        // 但把它写出来，这条串行化保证才是结构性的，不会因为将来给「修改他人」补一条
        // 判据而静默失效。锁批按 `user_id` 升序整集获取，多锁一行没有额外代价。
        //
        // 这批锁同时消掉两个真实故障（真库死锁报告已确认）：
        //
        // 1. `user_group` 到 `users` 的外键让 INSERT 先取父行的**共享锁**，而插入成功
        //    后 `invalidate_users_in_tx` 又要对同一行取**排他锁**。两个并发入组于是
        //    构成经典死锁环：A 持有 users 的 S 锁并等 uk_user_group 上 B 的 X 锁，
        //    B 持有 uk_user_group 的 X 锁并等 users 上 A 的 S 锁——MySQL 会牺牲一个，
        //    客户端看到 500（40001 Deadlock）。先取 users 的 X 锁，后续 INSERT 的外键
        //    检查由同一事务已持有的更强锁满足，不再取 S 锁，环就无从闭合。
        // 2. 唯一键冲突：并发重复加同一成员原本会撞 `uk_user_group`，而
        //    `DbError::ConstraintError` 把唯一键（1062）与外键（1452）混在一个变体里，
        //    兜底只能折算成 404，把「已是成员」的幂等成功谎报成「组已消失」。串行化
        //    之后，后到者一定会在 `insert_member_in_tx` 的前置读取里看到先到者刚提交
        //    的成员行，直接返回幂等结果，根本走不到 INSERT。
        //
        // 锁序纪律：必须按升序**整集**加锁（理由见 `lock_users_ascending_in_tx`）——写成
        // 「先锁操作者、再锁目标」会让两个互相加对方的账号各持自己的行等对方的行，
        // 直接构成死锁环。
        let mut lock_set = BTreeSet::new();
        lock_set.insert(operator_id);
        lock_set.insert(input.user_id);
        lock_users_ascending_in_tx(access, ctx, &mut transaction, &lock_set).await?;

        // 第二把锁：**目标组的组行**（`FOR UPDATE`）。它是成员变更的串行化点：
        // 同一组的两个成员变更在这里排队，后到者读到的成员名单必然包含先到者的提交，
        // 于是「幂等判定」与「成员上限判定」第一次真正建立在一份不会漂移的名单上。
        //
        // 缺陷形态（真库已证伪）：上限判定原先建立在 `list_members_in_tx` 的**非锁定**
        // 快照上，两名不同操作者并发加不同成员时事务锁批互不相交、互不阻塞，各自读到
        // 「199 名成员」而双双放行，成员数越过 `MAX_GROUP_MEMBERS`。
        //
        // 锁序：必须在 users 行锁**之后**取（全局锁序见 `domain::groups::admin` 的模块
        // 注释）。这不是任意的——`add_group_item` 先锁 users 行、再在插入条目时由外键取
        // 父组行的 S 锁，次序正是「users 行 → 组行」；成员变更若反过来就会与它成环。
        // 也正因为组行是**主键等值命中的记录锁**（不是间隙锁），它对空成员集合的组同样
        // 能真正串行化——旧实现拿成员行 `FOR UPDATE` 当串行化点时，空组上只剩彼此兼容的
        // 间隙锁，两个并发加成员都通过它、随后靠插入意向锁与间隙锁互锁成环（40001）。
        let group = access
            .groups()
            .lock_group_in_tx(ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // 以下普通读是本事务的**第一批一致性读**：Read View 在此建立，必然晚于两把行锁，
        // 因此能看到先于持锁的全部提交；持锁期间同一组的成员变更又被组行锁挡住，
        // 这份成员名单在提交前不会变。
        let members = access
            .groups()
            .list_members_in_tx(ctx, &mut transaction, group.id)
            .await?;

        // spec §8.1 附加规则：只有全权组成员能修改全权组成员。防的是「刚被移出全权组
        // 的账户立刻把自己（或同伙）加回去」——这条防线不依赖自提权判定。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY && !members.contains(&operator_id) {
            return Err(BaseError::PermissionDenied(
                "只有系统管理员可以修改系统管理员组的成员".to_string(),
            ));
        }

        // G2 闸门：目标组持有管理员等价权限时，加成员同样是一种「授予」——组本身没有
        // 变化，变的是成员：新成员会继承组的全部权限（含管理员等价那条）。判据因此看
        // **目标组的条目**，而不是本次请求里带了哪条权限（这条路径没有权限参数）。
        // 放在组行锁与成员/条目读之后，判据所见的条目不会漂移。
        ensure_may_modify_members_of_group_in_tx(
            access,
            ctx,
            &mut transaction,
            operator_id,
            group.id,
        )
        .await?;

        // 成员上限：本接口是唯一能**增加**成员的路径，`ensure_member_limit` 不在这里
        // 调用，组就能经 API 无界增长，`repository.rs` 里「成员集合被 MAX_GROUP_MEMBERS
        // 约束成有界规模」的注释也就与事实不符。判定口径必须是「**本次是否真的会新增**」：
        //
        // - 目标已是成员时本次不会新增任何人，重复调用必须按幂等成功返回，不能被上限
        //   误拒（缺陷形态：判定排在幂等分支之前，组满 200 人时重复加一个已在组内的成员
        //   会被判成「第 201 名」而拒绝）；
        // - 真的会新增时，交给它的必须是「插入后」的成员数，该函数的口径是「超过上限才
        //   拒绝」，恰好 200 名合法。成员数读自组行锁保护下的名单，事务提交前不会漂移。
        if !members.contains(&input.user_id) {
            ensure_member_limit(members.len() as u64 + 1)?;
        }

        // spec §8.1 主不变量：任何操作都不得使调用者自身有效权限增大。
        // 只有「目标就是调用者自己」时才存在提权可能；修改他人是正常的授权管理行为。
        if input.user_id == operator_id {
            let before =
                effective_permissions_of_in_tx(access, ctx, &mut transaction, operator_id).await?;
            let after =
                simulate_after_join(access, ctx, &mut transaction, operator_id, &group).await?;
            assert_no_self_escalation(&before, &after)?;
        }

        let changed = match access
            .groups()
            .insert_member_in_tx(ctx, &mut transaction, input.user_id, group.id, operator_id)
            .await
        {
            Ok(changed) => changed,
            // 外键 RESTRICT 的兜底窗口：组在本事务读到它之后被并发删除。用户存在性
            // 已在上方确认，因此这里只可能是组消失，折算成与前置读取同为 404 的
            // 拒绝，绝不能把 500 泄漏给客户端（与 delete_group 的兜底同例）。
            Err(error) if is_referential_constraint(&error) => {
                return Err(BaseError::RecordNotFound("权限组".to_string()));
            }
            Err(error) => return Err(error),
        };
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写审计。
        }
        // 新成员的有效权限变了，其已签发的 Access Token 必须立即失效（spec §6.3）。
        // 用户行锁在事务开头就已持有，这里只是复核并递增版本。
        let mut affected = BTreeSet::new();
        affected.insert(input.user_id);
        invalidate_users_in_tx(access, ctx, &mut transaction, &affected).await?;
        let event = audit::succeeded_event(
            ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group.id)?,
            None,
            Some(audit::summary([("user_id", json!(input.user_id))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    Access::finish_transaction(transaction, result).await
}

/// 该错误是否表示「外键约束拒绝了这次成员写入」。
///
/// 与 `delete_group.rs` 的同名判定同义：`user_group` 上的外键在 INSERT 时只有
/// 「父行不存在」一种违反方式，而用户存在性已在事务外确认过，因此认出约束类错误
/// 就足够，不必解析 MySQL 的报文案（跨库、跨版本都不稳定）。
///
/// `DbError::ConstraintError` 同时覆盖唯一键（1062）与外键（1452），这一点在本用例里
/// 不再构成歧义：唯一键冲突已被 [`join_group_once`] 开头的用户行锁排除（并发重复加
/// 同一成员会串行化后在幂等分支返回，走不到 INSERT），剩下的只有外键这一种。
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
            yang_base::action_name!("add_group_member"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/members")
        .display_name("加入组成员")
        .description("把一个用户加入权限组（幂等；受防自提权子集校验约束）")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_and_target_user_contract() {
        let injected = serde_json::from_value::<AddGroupMemberInput>(serde_json::json!({
            "group_id": 3,
            "user_id": 7,
            "granted_by": 1
        }));
        assert!(injected.is_err(), "客户端不能注入 granted_by 等内部字段");

        let without_user =
            serde_json::from_value::<AddGroupMemberInput>(serde_json::json!({ "group_id": 3 }));
        assert!(without_user.is_err(), "缺少 user_id 必须被拒绝");

        let params = <AddGroupMemberInput as ParamInput>::params();
        let names: Vec<&str> = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["group_id", "user_id"],
            "参数集必须恰好是「哪个组 + 哪个用户」，多一个就是越权入口"
        );
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn only_constraint_failures_are_treated_as_a_vanished_group() {
        // 认错变体会把真实故障吞成 404，或把并发删组泄漏成 500。
        assert!(is_referential_constraint(
            &BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(
                "Cannot add or update a child row: a foreign key constraint fails".to_string()
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
