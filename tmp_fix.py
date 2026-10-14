"""Fix permission_groups_integration.rs: remove step-up proof references."""
import sys

filepath = 'D:/code/lib_yang/project/yang-system/tests/permission_groups_integration.rs'

with open(filepath, 'r', encoding='utf-8') as f:
    lines = f.readlines()

# Boundaries (0-indexed): replace lines[2652:3696]
start_idx = 2652
end_idx = 3696

# Verify boundaries
assert 'concurrent_duplicate_adds' in lines[2662], f"Expected fn at 2662, got: {lines[2662].rstrip()[:60]}"
assert 'R5' in lines[3697], f"Expected R5 at 3697, got: {lines[3697].rstrip()[:60]}"

new_content = r'''/// spec §9.2 的幂等契约在并发下同样成立：两个并发加的同一 (user, group)，终态恰好一行。
///
/// 缺陷形态：`insert_member_in_tx` 是「先查后插」，并发时后到者撞唯一键（1062）被
/// `DbError::ConstraintError` 捕获，而它同时覆盖外键（1452）——于是被折算成
/// `RecordNotFound("权限组")`（404），把「已是成员」这个幂等成功谎报成「组已消失」。
///
/// 用真库两条独立连接 + Barrier 让两次插入真正撞在唯一键上（真库上后到者会先阻塞在
/// 索引锁上，等先到者提交后拿到 1062），单连接串行调用压不出这个窗口。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_duplicate_adds_of_the_same_member_stay_idempotent() {
    // 单轮交错仍可能被调度偶然化：两个任务若恰好一前一后跑完，后到者会在「先查」里
    // 读到已提交的成员行而直接幂等返回，压不到唯一键冲突那条路径。重复三轮。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "dup", "并发重复加成员").await;
        let target = harness::register_with_code(&app, "dup_target", "dup_target@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        // Barrier 对齐请求起点，两次插入是否真的撞在唯一键上由真库的独立连接与索引锁决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let app = app.clone();
            let admin = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                // 用 outcome 而非 status：失败时要能读出真正的错误文案，
                // 否则 5xx/404 只留下一串状态码，定位不到是哪条错误被折算出来的。
                harness::member_mutation_outcome(
                    &app,
                    &admin,
                    "add_group_member",
                    group_id,
                    target,
                )
                .await
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        let statuses: Vec<u16> = outcomes.iter().map(|(status, _)| *status).collect();

        // 幂等语义下两次都该是 200（一次 changed=true，一次 changed=false）。这里把它
        // 断言成「两个都是 200」比「不得是 5xx/404」更强：它同时钉住了不得把重复加
        // 当成错误，也钉住了不得泄漏 500。
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮并发重复加同一成员必须两次都幂等成功，实际 {outcomes:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            1,
            "第 {round} 轮并发重复加同一成员必须收敛到恰好一行成员"
        );
    }
}

// ---------------------------------------------------------------------------
// R1（收尾回合）：成员关系读锁定化——成员变更必须串行化，且判据必须在持锁之后读。
// 串行化点与锁序的权威描述见 src/addon/access/domain/groups/admin.rs 的模块注释；
// 收尾回合已把串行化点从「成员行」改为「目标组组行」，此处不复述，避免再次漂移。
// ---------------------------------------------------------------------------

/// 用 Barrier 同时驱动两条组成员写请求。
///
/// 两个任务的操作者**分别给出**：并发用例必须能用两个不同的账号发起请求，否则两次
/// 请求会因为锁批里共有的操作者行而被串行化，从而压不出「两个并发请求各自读到同一份
/// 陈旧快照」这个窗口。
///
/// Barrier 对齐的是**请求起点**——`wait()` 之后还有 JWT 鉴权才进事务，
/// 事务提交之后还有审计 INSERT——不是「开事务 → 读判据 → 写事实」那一段。它只是保证
/// 两次请求被同时投出，临界段的重叠由独立连接与行锁负责。
async fn concurrent_member_mutations(
    app: &BuiltApp,
    group_id: i64,
    left: (harness::Admin, &'static str, i64),
    right: (harness::Admin, &'static str, i64),
) -> Vec<u16> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for (operator, action, user_id) in [left, right] {
        let app = app.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            harness::member_mutation_outcome(&app, &operator, action, group_id, user_id)
                .await
                .0
        }));
    }
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(
            handle
                .await
                .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
        );
    }
    statuses
}

/// spec §8.2 的不可达态在**成员移除**路径上同样不可达：并发移出管理员不得把组清零。
///
/// 缺陷形态：守卫的成员名单来自 `list_members_in_tx` 的**普通一致性读**，而「移出一个
/// 成员」改的是 `user_group` 的成员行、不是 `users.status`——守卫持有的 users 行锁因此
/// 完全串行化不了成员变更。两个并发的移出各自读到同一份陈旧名单 [A,B]、各自数出 2 名
/// 启用管理员，双双放行，`system_admin` 组成员归零（spec §8.2 明称该状态不可达）。
///
/// 真库 + 两条独立连接 + Barrier：Barrier 只对齐两次请求的**起点**，事务临界段的重叠
/// 由两条独立连接与行锁负责。两个场景各跑三轮，避免调度偶然。
///
/// 断言只钉住不安全的那件事——最终必须仍有至少一名启用管理员，且两个请求不得都成功。
/// 败者是被守卫拦下（400/403）还是被授权版本水位线拦下（401）都无关紧要：三者都只是
/// 拒绝，不改变不变量。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_admin_removals_never_leave_zero_active_admins() {
    // 场景一：同一名管理员并发发两条移出——一条移出自己，一条移出另一名管理员。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let first = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &first, "admin2").await;
        let group_id = harness::system_admin_group_id(&app).await;
        assert_eq!(
            harness::active_member_count(&app, group_id).await,
            2,
            "第 {round} 轮场景一必须从两名启用管理员出发，否则并发窗口压不出来"
        );

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (
                first.clone(),
                "remove_group_member",
                second.user_id,
            ),
            (
                first.clone(),
                "remove_group_member",
                first.user_id,
            ),
        )
        .await;

        let active = harness::active_member_count(&app, group_id).await;
        assert!(
            active >= 1,
            "第 {round} 轮场景一：同一管理员并发移出自己与另一名管理员后系统零管理员\
             （响应 {statuses:?}）——spec §8.2 的不可达态被触达"
        );
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮场景一：两条并发移出恰好只能有一个成功（另一个必须被守卫或被\
             授权版本水位线拦下），实际响应 {statuses:?}"
        );
    }

    // 场景二：两名管理员**互相**移出对方（各自的操作者身份不同，锁集仍然相交）。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let first = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &first, "admin2").await;
        let group_id = harness::system_admin_group_id(&app).await;
        assert_eq!(
            harness::active_member_count(&app, group_id).await,
            2,
            "第 {round} 轮场景二必须从两名启用管理员出发"
        );

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, target) in [
            (first.clone(), second.user_id),
            (second.clone(), first.user_id),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "remove_group_member",
                    group_id,
                    target,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }

        let active = harness::active_member_count(&app, group_id).await;
        assert!(
            active >= 1,
            "第 {round} 轮场景二：两名管理员并发互相移出后系统零管理员\
             （响应 {statuses:?}）——spec §8.2 的不可达态被触达"
        );
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮场景二：互相移出恰好只能有一个成功（另一个必须被守卫拦下），\
             实际响应 {statuses:?}"
        );
    }
}

/// 成员上限必须真的封死：并发把两名**不同**成员加进只剩一个空位的组，终态不得越过 200。
///
/// 缺陷形态：`add_group_member` 的上限判定建立在 `list_members_in_tx` 的**非锁定快照**上。
/// 两名**不同操作者**并发加**不同**成员时，两个事务的锁批（操作者 + 目标）互不相交，
/// 请求因此真的并发执行、各自判定 `len + 1 <= 200` 合法，成员数越过 `MAX_GROUP_MEMBERS`
/// （`repository.rs` 里「成员集合被上限约束成有界规模」的注释随之失真）。
///
/// 操作者必须取两个不同账号：同一操作者的两次请求会因为锁批里共有的操作者行而被串行化，
/// 那样压不出这个窗口（实测：同一操作者时本用例在缺陷代码上照样绿）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_adds_cannot_exceed_the_member_limit() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "nearly_full", "只剩一个空位").await;
        let seeded =
            harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS - 1).await;
        assert_eq!(
            seeded,
            harness::MAX_GROUP_MEMBERS - 1,
            "第 {round} 轮夹具必须先把组填到只剩一个空位"
        );

        let left = harness::register_with_code(&app, "limit_left", "limit_left@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let right = harness::register_with_code(&app, "limit_right", "limit_right@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let left_operator =
            harness::grant_only(&app, &admin, "limit_op_left", "access.groups.write").await;
        let right_operator =
            harness::grant_only(&app, &admin, "limit_op_right", "access.groups.write").await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left),
            (right_operator, "add_group_member", right),
        )
        .await;

        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮只剩一个空位时两次并发加成员只能成功一个，实际响应 {statuses:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
            "第 {round} 轮并发加成员后组内必须恰好 {} 人，不得越过上限（响应 {statuses:?}）",
            harness::MAX_GROUP_MEMBERS
        );
    }
}

/// 并发把两名**不同**成员加进同一个**空**组：两次都合法（两名成员，上限 200 之内），
/// 必须双双成功，绝不能死锁。
///
/// 这是收尾回合修掉的死锁形态，且与 `concurrent_adds_cannot_exceed_the_member_limit`
/// 互补：那条用例的组已有 199 名成员，成员行 `FOR UPDATE` 会取到 199 把**记录锁**，
/// 两事务因此天然串行化，压不出缺陷；本条从一个**成员集合为空**的组出发，旧实现里成员行
/// `FOR UPDATE` 在空集上只取到彼此兼容的**间隙锁**，根本没有串行化——两名不同操作者的
/// users 行锁批又不相交（不同操作者、不同目标），于是两事务都读到空名单、都放行、
/// 都去 INSERT，最后在插入意向锁与对方的间隙锁上互相等待，MySQL 牺牲一个返回 40001 → 500。
///
/// 断言直接钉死「响应里不得出现任何非 200」：死锁被折算成 500，一次都不允许。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_adds_of_different_members_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        // 全新空组：成员集合为空，正是间隙锁压不出串行化、旧实现必死锁的初态。
        let group_id = harness::create_group(&app, &admin, "empty_race", "空组并发加").await;
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            0,
            "第 {round} 轮夹具必须从成员集合为空的组出发，否则压不出旧缺陷"
        );

        let left = harness::register_with_code(&app, "race_left", "race_left@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let right = harness::register_with_code(&app, "race_right", "race_right@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        // 操作者必须是两个**不同**账号：同一操作者会让两次请求的 users 行锁批相交、
        // 在锁上被串行化，那样压不出「锁批不相交却都放行」的窗口。
        let left_operator =
            harness::grant_only(&app, &admin, "race_op_left", "access.groups.write").await;
        let right_operator =
            harness::grant_only(&app, &admin, "race_op_right", "access.groups.write").await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left),
            (right_operator, "add_group_member", right),
        )
        .await;

        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮并发加两名不同成员到空组不得死锁（死锁会被折算成 500），\
             实际响应 {statuses:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            2,
            "第 {round} 轮两次都成功时组内必须恰好两名成员"
        );
    }
}

/// 幂等分支不得被上限误拒：重复添加一个**已经在组里**的成员不新增任何人，必须幂等成功。
///
/// 缺陷形态：上限判定 `ensure_member_limit(len + 1)` 排在 `insert_member_in_tx` 的幂等判定
/// **之前**，于是组满 200 人时重复加一个已在组内的成员会被判成「第 201 名」而拒绝
/// （400）——本次请求根本不会新增成员，这是纯粹的误拒。判定口径必须是「本次是否真的会
/// 新增」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_an_existing_member_at_the_limit_stays_idempotent() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "capped", "满员的组").await;
    let target = harness::register_with_code(&app, "cap_target", "cap_target@example.com")
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    // 先填到 199 名占位成员，再经真实 Action 把 target 加成第 200 名（这一名是合法的）。
    let seeded =
        harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS - 1).await;

        harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS - 1).await;
    assert_eq!(seeded, harness::MAX_GROUP_MEMBERS - 1);
    harness::add_member(&app, &admin, group_id, target).await;
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
        "夹具必须先把组填到恰好上限，且 target 已是成员"
    );
    assert!(harness::is_member(&app, group_id, target).await);

    // 重复添加同一名已成为成员的账号：本次不会新增任何人 → 必须幂等成功。
    let (status, message) = harness::add_member_outcome(&app, &admin, group_id, target).await;
    assert_eq!(
        status, 200,
        "重复添加已在组内的成员不得被成员上限误判为超限（本次不会新增任何成员）：{message}"
    );
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
        "幂等分支不得改动成员数"
    );
}

/// spec §13 的扇出锁序测试：**同一组上并发「移出成员」与「加权限」**不得死锁或超时。
///
/// 移出成员会在同一个事务里读全权组成员并逐个锁住成员的 `users` 行（守卫与扇出），
/// 加权限则按升序扇出锁住该组的全部成员——两者必然在同一批用户行上相交。若成员变更
/// 路径在持有成员行锁之后才去取用户行锁、而别的路径以相反顺序取锁，这里就会成环，
/// MySQL 牺牲一个返回 500。本用例钉住「不成环」：两条路径都必须成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_member_removal_and_item_add_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &admin, "admin2").await;
        let third = harness::add_second_admin(&app, &admin, "admin3").await;
        let admin_group = harness::system_admin_group_id(&app).await;
        // 扇出要有真实规模：另两名管理员都在同一个普通组里，给这个组加权限会锁住
        // 「操作者 + 全体成员」三行用户行，正好与移出成员那次事务的用户行锁相交。
        //
        // 刻意**不**把操作者自己写进这个组：入组会递增成员自己的授权版本，操作者的
        // 令牌会随之失效，后续请求会停在 401（与「移出成员」无关的噪声）。操作者
        // 本身是全权组成员，已持有目录里全部权限，因此不加进组也照样能加条目。
        let shared = harness::create_group(&app, &admin, "mixed", "并发锁序组").await;
        harness::add_member(&app, &admin, shared, second.user_id).await;
        harness::add_member(&app, &admin, shared, third.user_id).await;

        let removed = third.user_id;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "remove_group_member",
                    admin_group,
                    removed,
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    shared,
                    "account.users.read",
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮同一组上并发移出成员与加权限不得死锁/超时（死锁会被折算成 500），\
             实际响应 {statuses:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// R3：条目路径与成员路径之间的自提权 TOCTOU（同一账号两个并发请求即可完成）。
// ---------------------------------------------------------------------------

/// spec §8.1 与决策 D2：应用内不存在自提权路径——**并发交错下同样不存在**。
///
/// 缺陷形态：两条路径的判据取自彼此尚未提交的快照，且都是无锁 SELECT：
/// `add_group_item` 的判据是「调用者此刻在该组内」（`list_members_in_tx`），
/// `add_group_member` 的判据是「该组此刻的条目」（`list_items_in_tx`）。
/// 同一账号同时发起这两个请求时：
///
/// - 条目请求读到「我还不在此组」→ 整段 §8.1 子集校验被跳过，权限 X 写进组；
/// - 成员请求读到「该组还没有 X」→ 子集校验通过，把自己写进组。
///
/// 两次提交后，调用者成了「持有一条自己原本没有的权限」的组成员——**不需要同伙**，
/// 一个账号两个并发请求即可完成。串行执行时第二个请求必被 403 拒掉，因此这是纯粹的
/// 原子性缺失，任何单连接串行用例都压不出来。
///
/// 夹具组里有 8 名成员：扇出失效是真的（要逐个锁行并递增授权版本），但刻意**不**做大。
/// 原因是本用例压的是「读快照的时机」而不是「谁跑得慢」：InnoDB 可重复读下两个事务各自
/// 的快照在各自第一条普通读时建立，因此只要两次判据读都早于对方的提交，缺陷就必然出现，
/// 不需要靠人为拖长事务。成员数一旦做得很大，条目请求那批前置行锁本身会把它的快照推到
/// 成员请求提交之后——用例反而会对「操作者行不在锁集内」这种破坏不再敏感（实测：150 名
/// 成员时该破坏仍绿，8 名时变红）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_add_and_self_member_add_cannot_self_escalate() {
    const SEEDED_MEMBERS: usize = 8;
    const ESCALATED: &str = "access.grants.write";

    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        // operator 只被直授 access.groups.write：它一旦拿到 access.grants.write，
        // 就只可能来自这次并发，不可能来自任何既有授权。
        let operator = harness::grant_only(&app, &admin, "operator", "access.groups.write").await;
        let group_id = harness::create_group(&app, &admin, "target", "并发自提权目标组").await;
        let seeded = harness::seed_members_directly(&app, group_id, SEEDED_MEMBERS).await;
        assert_eq!(seeded, SEEDED_MEMBERS, "第 {round} 轮夹具必须先把组填满");
        assert!(
            !harness::is_member(&app, group_id, operator.user_id).await,
            "第 {round} 轮夹具必须从「operator 不在组内」出发，否则条目请求的判据会生效"
        );
        assert!(
            !harness::group_holds_item(&app, group_id, ESCALATED).await,
            "第 {round} 轮夹具必须从「组不持有该权限」出发"
        );

        // Barrier 对齐请求起点，两次请求才会被同时投出，各自的事务也才有机会在「第一条
        // 普通读建立快照」那一刻重叠——这正是缺陷暴露的窗口。重叠是否发生由真库调度决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    ESCALATED,
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    operator.user_id,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }

        assert!(
            statuses.iter().all(|status| *status < 500),
            "第 {round} 轮并发自提权不得泄漏 5xx（含死锁折算），实际 {statuses:?}"
        );
        // 恰好一条成功——两个方向都要钉：
        //   - 两条都 200 ⇒ 两条路径各自读到对方的陈旧快照，完成了自提权（下面那条
        //     `escaped` 断言也会随之变红）；
        //   - 两条都不是 200 ⇒ 两条路径被互相卡死。串行执行时先到者必合法（先加条目
        //     时操作者还不是成员、入组不改变其权限；先入组时组里还没有该条目、入组同样
        //     不改变其权限），后到者才被 §8.1 子集校验拒绝（403），因此 [403,403] 这种
        //     「全都拒掉」的过度串行化同样违反设计意图，`<= 1` 会把它放过去。
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮两条并发路径必须恰好一条成功（另一条被 §8.1 子集校验拒绝），\
             实际 {statuses:?}"
        );
        let escaped = harness::is_member(&app, group_id, operator.user_id).await
            && harness::group_holds_item(&app, group_id, ESCALATED).await;
        assert!(
            !escaped,
            "第 {round} 轮并发让 operator 拿到了它原本没有的权限 {ESCALATED}（响应 {statuses:?}）\
             ——spec §8.1 与决策 D2「应用内不存在自提权路径」被打破"
        );
    }
}

/// spec §13 的扇出锁序测试：**同一组上并发加权限与加成员**不得死锁或超时。
///
/// 加权限会扇出锁住该组的**全部**成员（升序），加成员则锁「操作者 + 目标」两行；
/// 两者都落在 `users` 的行锁上，因此这一次并发真正压的是「不同 Action 的取锁顺序
/// 是否一致」。组里直写 40 名成员让扇出规模真实存在，两名操作者又都是组内成员，
/// 使两组行锁必然相交——不按同一顺序取锁就会在这里成环，MySQL 牺牲一个返回 500。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_and_member_add_on_the_same_group_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "fanout", "并发扇出组").await;
        // 扇出要有真实规模：先直写若干成员，让「加权限」那次事务确实要锁住一串用户行。
        harness::seed_members_directly(&app, group_id, 40).await;

        // 两名成员操作者：都在组内（因此两组行锁必然相交），都持 access.groups.write。
        let item_operator =
            harness::grant_only(&app, &admin, "fanout_a", "access.groups.write").await;
        harness::grant_permission(&app, &admin, item_operator.user_id, "access.grants.read").await;
        let member_operator =
            harness::grant_only(&app, &admin, "fanout_b", "access.groups.write").await;
        harness::add_member(&app, &admin, group_id, item_operator.user_id).await;
        harness::add_member(&app, &admin, group_id, member_operator.user_id).await;
        // 授权与入组都递增了授权版本，令牌必须在两者之后重签。
        let item_operator = harness::relogin(&app, &item_operator).await;
        let member_operator = harness::relogin(&app, &member_operator).await;
        let newcomer = harness::register_with_code(&app, "fanout_new", "fanout_new@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = item_operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                // 操作者已在组内且本来就持有这条权限，§8.1 子集校验放行——本用例测的是锁序，
                // 不是提权判定。
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    "access.grants.read",
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = member_operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    newcomer,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮同组并发加权限与加成员不得死锁/超时（死锁会被折算成 500），\
             实际响应 {statuses:?}"
        );
    }
}

/// spec §13 的锁序测试：**同一组、两名成员操作者并发加权限**不得死锁。
///
/// 操作者行锁若写成「先单独锁操作者自己、再按升序锁受影响成员行」，这里必然成环：
/// 两名操作者各持自己的行，再各自去要对方那一行（A 持 A 等 B，B 持 B 等 A），
/// MySQL 会牺牲一个，客户端收到 500。升序才是唯一不会成环的取锁顺序。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_adds_by_two_member_operators_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "shared", "共同维护的组").await;
        // 两名操作者各持 `access.groups.write`（否则调用不了条目 Action）外加一条
        // 它准备写进组的权限（否则会撞上 §8.1 的子集校验）。两条权限必须互不相同，
        // 否则并发插入会撞上 `(group_id, permission)` 唯一键，把锁序问题掩盖成幂等。
        let first = harness::grant_only(&app, &admin, "member_a", "access.groups.write").await;
        harness::grant_permission(&app, &admin, first.user_id, "access.grants.read").await;
        let second = harness::grant_only(&app, &admin, "member_b", "access.groups.write").await;
        harness::grant_permission(&app, &admin, second.user_id, "account.users.read").await;
        harness::add_member(&app, &admin, group_id, first.user_id).await;
        harness::add_member(&app, &admin, group_id, second.user_id).await;
        // 授权与入组都会递增各自的授权版本，令牌必须在两者之后重签。
        let first = harness::relogin(&app, &first).await;
        let second = harness::relogin(&app, &second).await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, permission) in [
            (first.clone(), "access.grants.read"),
            (second.clone(), "account.users.read"),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    permission,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮两名成员操作者并发加权限不得死锁/超时，实际 {statuses:?}"
        );
    }
}

'''

# Build new file: lines before start_idx + new content + lines from end_idx onward
new_lines = lines[:start_idx] + [new_content] + lines[end_idx:]

with open(filepath, 'w', encoding='utf-8') as f:
    f.writelines(new_lines)

print("Done! Replacement applied.")
print(f"Old line count: {len(lines)}")
new_line_count = start_idx + new_content.count('\n') + (len(lines) - end_idx)
print(f"New approximate line count: {new_line_count}")
