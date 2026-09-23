# 飞书数据源「以表为单位」配置 — 决策流水（Ruling 记录）

> 这是实施过程中主控与各执行代理**逐步做出的决定的原始记录**，从执行用的 ledger
> 原样复制而来（格式粗糙，但未删改）。共 50+ 条 Ruling + 若干 Note。
>
> **为什么留着它**：其中若干净了会被重推一遍的决定（尤其 Ruling 14 / 43 / 44 / 45）
> 是本次改造最贵的产出，而重推的成本远高于存一份原始文本。
>
> 配套：[设计](feishu-datasource-table-config.md) ｜ [实施计划](feishu-datasource-table-config-tasklist.md)
> ｜ [控制台设计](feishu-datasource-console.md)

---


Spec: docs/architecture/feishu-datasource-table-config.md (read at setup)
Execution: inline (native), on `main` per explicit user consent (2026-09-23)
Ruling: work on `main`, not a worktree/branch — user's explicit instruction — cost if wrong: commits land directly on main; they are per-task and revertible.

## Pre-flight interface scan

| # | Producer → Consumer | What crosses | Finding |
|---|---|---|---|
| 1 | T1 → T3/T5/T9/T11/T12 | `FeishuContext::datasource_fields()`, `feishu_datasource_field` 表 | consistent |
| 2 | T3 → T4 | `tables_url(app_token) -> Result<String, BaseError>` 形状 | consistent |
| 3 | T5 → T13 | T5 把 `valid_source_key` 从 `create_datasource.rs` 提到 `domain/source_key.rs`；T13 随后删除 `create_datasource.rs` | **OK but load-bearing**: extraction must happen in T5, else T13's delete loses the function |
| 4 | T5 → T12 | T5 的 handle 需要写入 `token_hash` + `token_cipher`；但 `seal`/`unseal` 在计划里定义于 T12（更晚） | **CONFLICT** — see Ruling 1 |
| 5 | T8 → T9/T11 | `resolve_field_names(...)`, `resolve_from(remote, wanted)` | consistent |
| 6 | T9 → T13 | `pull_table` 取代 `pull.rs` 的逐源路径 | consistent |
| 7 | T1 → T10 | `consecutive_failures` / `last_error` 落在**表级**行 | consistent（T9 写入的是表级行） |
| 8 | T11/T12 → T15 | 体检与回显/轮换端点 | consistent |

Ruling 1: `crypto::seal` / `unseal` 的**实现随 T5 落地**（含其单测），T12 只新增两个端点并复用它们。
— Reason: T5 的 handle 必须写 `token_cipher`，计划却把 `seal` 排在 T12，T5 在 T12 之前编译不过。
— Cost if wrong: 两个步骤的归属对调，无行为差异。

## Progress


### Task 1: complete (commits aaa72e9..HEAD, tests: cargo test --lib --locked → 464 passed; architecture check passed)

Ruling 2: 绑定表的 schema 落在 `datasource/domain/field_table.rs`、module 构造器并进 `datasource/mod.rs` 的 `build_field_module()`，而不是计划里写的 `datasource/field_table.rs` + `datasource/field_module.rs`。
— Reason: 架构门禁（`check_architecture.py`）两条硬规则：① module 目录根只允许 `mod.rs`/`table.rs`/`actions/`/`domain/`；② addon 根下的目录**只有含 `actions/` 才被认定为 module**。而「无 Action 的表 module」这个形状框架不认——独立目录会被判成「游离的机制目录」。
— Cost if wrong: 目录结构选择，改动局限在一个文件的位置。

Ruling 3: 计划 T1 漏了「表要进 schema 必须由某个 `ModuleSpec::table()` 声明」这一步（只加了 Repository）。补了 `build_field_module()` 并在 `addon/mod.rs` 装配。
— Reason: `ModuleSpec::table()` 是单个 `Option<TableSpec>`，`AddonSpec` 无独立表入口。不加则 `feishu_datasource_field` **永远不会被创建**，而 Repository 仍会构造出来（运行期才炸）。
— Cost if wrong: 无——这是计划的事实性错误，不补则任务目标未达成。

Note: `FeishuContext` 的 `datasource_field` 字段与 getter 暂标 `#[allow(dead_code)]`（消费者在 T5 接入）。仓库既有先例：`domain/bitable.rs:29`、`domain/outbound.rs:30`。

Note: 本机 `python scripts/run_ci.py quick` 在 Frontend typecheck 一步失败，原因是 corepack 与 pnpm 版本协商（`Align the "packageManager" field...`），**非代码问题**——直接 `cd frontend && pnpm typecheck` 通过。Rust 侧与架构门禁均通过。
Task 1: complete (commits aaa72e9..af7903f, tests: cargo test --lib --locked → test result: ok. 464 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.08s)

### Task 2: complete (lib_yang c1b847e; tests: yang-base 651 passed / yang-system 464 passed, clippy -D warnings 干净)

Ruling 4: T2 的实现层次从 yang-db 的 `From<sqlx::Error>` 改到 yang-base 的 `From<DbError> for BaseError`，并把「1062」判定改为**报文判定**。
— Reason: ① sqlx 的 `DatabaseError::code()` 返回的是 **SQLSTATE**，MySQL 重复键是 `23000` 而非 1062，计划里的 `code() == Some("1062")` 恒不成立；② MySQL 的外键/非空/CHECK 与唯一键**共用 SQLSTATE 23000**，光看状态码分不出来；③ `ParamInvalid` 定义在 yang-base，yang-db 层造不出来，硬造会给 yang-db 引入 API 语义。
— Cost if wrong: 报文匹配依赖 MySQL/PG 的错误文本措辞；若官方改措辞，会原样退回 `DatabaseExecuteFailed`（即现状），不会更糟。

Ruling 5: 计划缺陷（第 3 处）——`crates/` 不在 yang-system 仓库里：外层 `D:/code/lib_yang` 是独立仓库（分支 `master`，有 GitHub 远端），cargo workspace `members = ["crates/*"]` 且 `exclude = ["project/yang-system"]`；yang-system 是自己的 git 仓库，经 path 依赖 `../../crates/*`。计划里 `git add crates/yang-db/src/...` 与 yang-system 的改动**无法进同一个提交**。
— 处置：用户明确选择「提交到 lib_yang master」。已提交，**未 push**。
— Cost if wrong: 共享库的错误语义变更（`ConstraintError` 里的唯一键冲突从 200004 服务端错误变为 700005 客户端错误）会影响 lib_yang 的其它消费者；如需回退，改动集中在一个函数与一处 match 分支。
Task 2: complete (commits af7903f..af7903f, tests: cargo test --lib --locked → test result: ok. 464 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.09s)

Clarification: Task 2 的自动完成行写的 `af7903f..af7903f` 是**空区间**——T2 的提交在外层 `lib_yang` 仓库（`c1b847e`），不在 yang-system。以后凡触及 `crates/` 的任务，区间都以 lib_yang 的提交为准。

### Task 3: complete (commits af7903f..HEAD, tests: cargo test --lib --locked → 472 passed; architecture check passed; clippy -D warnings 干净)

Ruling 6: 计划 T3/T4 的 URL 组装漏了 `/open-apis` 段。既有的 `records_url`/`fields_url` 都是 `{FEISHU_OPEN_BASE}/open-apis/bitable/v1/...`，计划写的 `{FEISHU_OPEN_BASE}/bitable/v1/...` 会打到不存在的路径。
— Reason: 以既有实现为准（`FEISHU_OPEN_BASE` 只含 host）。
— Cost if wrong: 无——漏了这一段请求必然 404。

Ruling 7: 计划 T3 的权限写 `feishu.datasource.read`，实际用 **`feishu.datasource.write`**。
— Reason: 它**会出站调飞书并消耗本应用频控配额**，与 `pull_probe` 同类。`pull_probe.rs:150-153` 的注释把这条取舍写得很清楚：「不该和纯读的控制台查询共用同一权限」。
— Cost if wrong: 只读角色的用户看不到表列表，配置向导第一步不可用（可由管理员改权限位）。

Ruling 8: 抽出 `domain/outbound_setup.rs` 收敛出站四件套（settings 校验 + Redis token 缓存 + transport + sleeper + 错误映射）。计划的 T3/T4/T11 会各自复制这 25 行，5 处重复。
— Reason: DRY；且 `pull_probe` 与 `feishu_pull` 已各写一遍，本次再添 3~4 处。
— 未回改 `pull_probe`：它是无测试覆盖的热路径，留给 Task 13（退役字段级入口，本来就动它）。
— Cost if wrong: 一处薄封装，回退即把调用点展开。

Note: 计划 T4 要「新建 `fields_url`」，但它**已经存在**（`bitable.rs:93`）——T4 只剩「列出视图」是新的。
Task 3: complete (commits af7903f..e2e7e8b, tests: cargo test --lib --locked → test result: ok. 472 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.09s)

### Task 4: complete (commits e2e7e8b..HEAD, tests: cargo test --lib --locked → 484 passed; architecture passed; clippy -D warnings 干净)

Ruling 9: `outbound_setup::require_settings` 返回 `Result<&FeishuSettings, (i32, &'static str)>`，而不是 `Result<_, ApiResponse>`。
— Reason: `ApiResponse` 体型大，落进 `Err` 触发 `clippy::result_large_err`（仓库 clippy 带 `-D warnings`）。返回码+文案由调用方成形响应，语义不变。
— Cost if wrong: 三个调用点各多一行 match 臂。

Note: `list_fields_query` 的文档已改（现在服务于所有 `page_size` 上限 100 的列表端点：字段/数据表/视图），函数名保持不变以免动到拉取热路径。

Note: 计划 T4 的 `fields_url_does_not_send_view_id` 测试已落地——它把「实测 view_id 对列出字段不生效」这个结论钉进代码，防止有人照着官方参数字段表加回去。
Task 4: complete (commits e2e7e8b..51a45fd, tests: cargo test --lib --locked → test result: ok. 484 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.08s)

### Task 5: complete (commits 51a45fd..HEAD, tests: cargo test --lib --locked → 513 passed; architecture passed; clippy -D warnings 干净)

Ruling 10: `token_cipher` 的**封装密钥用 `feishu.encryption_key`**；未配置时 `create_datasource_table` 返回可归因的 `ConfigError`，而不是静默降级成「只存摘要、永远取不回」。
— Reason: 设计文档 §10.2 只说「用现有 `domain/crypto.rs` 的设施」，没点名钥匙；已归档的 §10.9 把 `feishu.encryption_key` 确定为封装密钥，这里沿用。静默降级会让「复制按钮」在某些部署上莫名其妙地不可用。
— Cost if wrong: 该配置未设的部署建不了表级数据源（报错文案已指明缺什么）。回退即改成允许为空并只在回显时失败。

Ruling 11: 新增 `crypto::unseal_verified(cipher, expected_hash, key)`，**用同一条记录上的 `token_hash` 当完整性校验和**；回显路径必须用它。
— Reason: CBC 只保密不保证完整性。原计划的测试「换钥匙必须失败」是**概率性**的（错钥匙下 PKCS#7 填充约 1/256 恰好合法 → 测试会偶发红），而且改 IV 只污染第一段明文、填充仍然完好，**根本发现不了篡改**。摘要本来就在同一条记录上，且校验路径不需要解密，是随手可得的正解。
— 效果：篡改密文、拿错钥匙、把 A 行密文贴到 B 行，三者都变成**确定性**失败。
— Cost if wrong: 多一次 sha256 比对（微秒级）。

Note: **`insert_in_tx` 返回的 `u64` 是硬编码的 `1`（影响行数），不是自增 id**（`yang-base/src/table/table_query/write.rs:83`）。取 id 必须用 `insert_returning_id_in_tx(tx, data).await?.1`（同文件 `:124-137`）。计划没提这一步，而绑定行没有它就会指向不存在的父。

Note: `valid_source_key` 已从 `create_datasource.rs` 提到 `domain/source_key.rs`（两个消费者，避免漂移）。**这是 T13 删除 `create_datasource.rs` 的前提**——先提取再删，否则函数会跟着文件一起消失。

Note: `crypto::unseal` / `unseal_verified` / `decrypt_bytes` / `Aes256CbcDec` 暂标 `#[allow(dead_code)]`（消费者是 T12 的回显端点）。
Task 5: complete (commits 51a45fd..0d26486, tests: cargo test --lib --locked → test result: ok. 513 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.09s)

### Task 6: complete (commits 0d26486..HEAD, tests: cargo test --lib --locked → 528 passed; architecture passed; clippy -D warnings 干净)

Ruling 12: `datasource_id` 走 **body**，路由不带路径段（`PUT/DELETE /api/v1/feishu/datasources/table`），偏离计划写的 `/table/{id}`。
— Reason: ① 本模块既有的 `update_datasource` / `delete_datasource` / `pull_now` 全部用 body 传标识；② `params!` 的 `#[param(source = path)]` 只用于**简单标量**，而 update 要收 `Vec<FieldBindingInput>`，混用会逼出两套声明风格。仓库里没有任何「path + 复杂 body」的 `params!` 先例。
— Cost if wrong: 改回路径段只需改 route 与取参处；RESTful 观感略有损失。

Ruling 13: **取消勾选 = 停用绑定（`enabled = false`），不删行、不动选项**；重新勾选走「更新」把同一行启用回来。
— Reason: 删行会让飞书侧已配的控件在下次请求时吃 `SOURCE_NOT_FOUND`（而停用是可归因的 `SOURCE_DISABLED` / 40301），历史审批单引用的 `option_id` 也会失联。删行只发生在「删除整个数据源」。
— 这条是 `diff_bindings` 之所以存在的**主要理由**：若重新勾选走了插入，`source_key` 会漂移、飞书侧控件全断，而接口照样返回成功（静默失效）。
— Cost if wrong: 取消勾选的列在库里留一行停用的绑定；`enabled` 是既有列，不新增负担。

Refactor: `FieldBindingInput` / `validate_fields` / `has_parent_cycle` 提到 `domain/field_binding.rs`——create 与 update **共用同一套规则**（漂移只在运行期暴露：创建挡了环、更新没挡，就能绕过界面造出环）。T5 的 `create_datasource_table` 已改为引用它，本地副本删除。
Task 6: complete (commits 0d26486..afc9d90, tests: cargo test --lib --locked → test result: ok. 528 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.10s)

### Task 7: complete (commits afc9d90..HEAD, tests: cargo test --lib --locked addon::feishu → 344 passed; clippy -D warnings 干净; architecture passed)

Ruling 14: **T1 少删了四列**——设计 §5 的表级行只有 `id/title/ingest_mode/status/三个坐标/同步状态`，而 `source_key` / `token_hash` / `encrypt_enabled` / `default_locale` 都属于**字段绑定**层。T1 只删了 `bitable_field_name` 与 `linkage_mapping`（那是计划 T1 的原文口径），这四列留在表级行上。
— 危害：`source_key` 是 `.require(true)` + `.unique(true)`，会让建表级行必然失败（而 T5 的单测只测 validate，测不到）；且「一条表级行只能有一个 source_key」与「一张表 N 个字段各有 source_key」直接矛盾。
— 处置：按 spec 删掉这四列，并加 `the_table_row_carries_no_credential_or_routing_columns` 把它们钉住。
— Cost if wrong: 无——spec §5 是绑定权威，且数据库上线前清空，无迁移负担。

Ruling 15: 列表的兜底排序从 `source_key` 改为 `title` + `id`。
— Reason: `source_key` 已不在表级行上（Ruling 14）。只按 `title` 时同名行之间没有全序，翻页会漏行或重复（原注释里写的正是这条顾虑），故补 `id`。
— Cost if wrong: 排序观感变化；原测试 `title_is_sortable_so_the_ledger_can_order_by_name` 的意图被保留。

Note: 绑定**一次 `where_in` 取回再分组**（`group_bindings`），不是按行查——一页 100 条就是 100 次往返。`where_in` 拒绝空列表，空页要短路（否则第一页为空直接报错）。

Note: 字段级 Action（`create_datasource` / `update_datasource` / `delete_datasource` / `pull_probe` / `pull_now`）与 `pull.rs` 的逐源路径现在**确定是运行期死的**（它们引用的列已全部消失）。它们在 T13 被删除/改写——**T13 落地前不要部署**。
Task 7: complete (commits afc9d90..86abd42, tests: cargo test --lib --locked addon::feishu → test result: ok. 344 passed; 0 failed; 0 ignored; 0 measured; 189 filtered out; finished in 0.02s)

### Task 8: complete (commits 86abd42..HEAD, tests: cargo test --lib --locked addon::feishu::domain::bitable → 45 passed; clippy -D warnings 干净; architecture passed)

Note: `resolve_field_names` 在单列版 `resolve_field_name` 之外补了三条，都是表级拉取逼出来的：① **批量**（一次快照要解析整表勾选的列，逐列调用会重复扫全表元数据）；② **点名全部缺失**（不是第一个缺失处就停——运维一次看全要修的东西，不必「修一个跑一轮」）；③ **顺序与输入一致**（日志、快照摘要、`field_names` 都按它拼）。**重名校验沿用单列版的口径**（`field_names` 按名字匹配，表内同名会取到不确定的列），这条不能因为是批量版就漏。

Note: `resolve_current_field_names` 把缺失归为 `OutboundFailure{ kind: Fatal }`——列被删是**配置错误**，退避重试不会自愈（与 `1254024` 被归为 Fatal 同一条口径）。
Task 8: complete (commits 86abd42..5f9c156, tests: cargo test --lib --locked addon::feishu → test result: ok. 350 passed; 0 failed; 0 ignored; 0 measured; 189 filtered out; finished in 0.02s)

### Task 9: complete (commits 5f9c156..1f35a88, tests: cargo test --lib --locked addon::feishu::domain::pull → test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 523 filtered out; finished in 0.00s; clippy -D warnings 干净; architecture passed)

Ruling 16: 表级落库**逐字段一个事务**（选项整行替换 + 补集停用 + 该绑定的 `field_name`/`snapshot_digest` 同事务），表级状态（`last_pull_at`/`last_success_at`/`consecutive_failures`/`last_error`）在整轮末尾走一个独立事务。
— Reason: 设计 §6.2 末句就是「**拉取合并、落库拆开**」；§6.3 把摘要放在字段绑定层，而「跳过写库」的判据（摘要未变且本地无已停用行）本身就是逐字段算的。D4 的「全有或全无」是**轮**的语义（失败整表停、状态落表级行），不是「N 个绑定必须同一个原子事务」。
— Cost if wrong: 一个绑定写到一半失败时前面已提交（写是整行替换、幂等，下一轮重试即可）；若要改成整表单事务，只需把 `persist_binding` 提到循环外、`BindingWritePlan::doomed` 改成整表累积。

Ruling 17: `pull_table` 只写**成功**状态，**不写**失败状态；失败状态（自增 `consecutive_failures` + `last_error`）由调用方在失败路径上写。
— Reason: T10 的原文就是「在 `feishu_pull.rs` 的失败路径上：自增 `consecutive_failures` → 写 `last_error` → `should_alert` 为真则发邮件」。两边都写会让连续失败次数**翻倍**，而告警阈值正是按它判的。这也保留了逐源路径的既有分工（`pull_source` 返 Err、`run_round` 记失败）。
— Cost if wrong: 若 T10 改成依赖 `pull_table` 自己记失败，则连续失败永不增长、告警永不触发——症状是「表一直坏着但没邮件」，`last_error` 为空可归因。

Ruling 18: 父列**不在本表启用中的绑定集合**里 → 整表失败并点名（`ConfigError`），不降级成「无父」。
— Reason: 集合只含 `enabled = true` 的绑定（T6：取消勾选 = 停用不删行），父列不在里面就读不到它的文案——进不了 `field_names`、快照里根本没有它。此时若静默按「无父」派生，子选项的 `option_id` 会**整棵子树一起变**（`derive.rs` 把 `parent_key` 哈希进 id），飞书控件上已选中的值全部失联，而且**不报错**（失效形态是「下拉静默变空」）。`validate_fields` 本该挡住这种配置，但那是 API 层校验，手工改库可以绕过。
— Cost if wrong: 手工把父列绑定停用的表会整表停拉（`last_error` 点名是哪条绑定的哪个父列）；反之按无父降级则是不报错地毁掉已配控件——两害相权取前者。

Ruling 19: 出站失败映射成 `BaseError` 时**保留 `FailureKind`**：`Fatal` → `ConfigError`，其余 → `UpstreamUnavailable`；**没有**复用 `outbound_setup::outbound_error`。
— Reason: 折成一个笼统变体会让「列被删了（要改配置）」与「飞书暂时抖动（重试有意义）」在下游日志与 `last_error` 里长得一样。既有那个 `outbound_error` 映射成 `ParamInvalid`，那是 **HTTP 入口**的语义（「哪个参数错了」）；后台 worker 里没有参数这回事，套上去只会误导运维。brief 指定 `pull_table` 返回 `BaseError`（而不是 `OutboundFailure`），所以 Kind 必须在这里落地。
— Cost if wrong: 两处出站错误映射口径不一致；改回一个薄函数即可，不影响任何落库行为。

Ruling 20: 解析出的名字**写回绑定行的 `field_name` 缓存**，且与摘要**同事务**；只有「内容未变 **且** 名字未变」时才真的不碰那一行。
— Reason: T8 的接口说明把「写回缓存」明确派给 T9。缓存为 `None`（首次拉取）也算名字过期——不写回的话控制台永远显示空。跳过判据必须同时看两个量：只比摘要会让「内容没变但列改了名」的缓存永远不更新（而改名不断链正是决策 D2 的全部意义）。
— Cost if wrong: 多写一列（本来每轮就在写的那一行），不新增事务；漏写则控制台字段名长期为空，且体检页拿不到当前名字。

Note: `load_pull_tables` / `pull_table` 暂标 `#[allow(dead_code)]`——消费者是 T13 的 worker 与 T10 的失败路径，而本任务只落地入口、逐源路径仍在跑。沿用既有先例（`domain/bitable.rs:29`、`domain/context.rs:21`）。

Note: **空快照歧义守卫被提到表级**：同一份快照服务整表，所以「拉到 0 行但本地还有已启用选项」只要命中**任一**字段就整表停写，而不是逐字段各自判断（逐源路径是逐源判断）。这是 `pull.rs` 模块文档那段守卫在表级下的正确形态。

Note: 编排层**仍然没有**单测覆盖（要数据库）——`pull_table` / `pull_table_inner` / `persist_binding` / `record_table_success` 都是零覆盖路径。有测试的是它的**全部纯函数部分**：`collect_field_names`、`resolve_targets`、`declared_parent`、`bind_fields`、`parent_linkage`、`name_cache_is_stale`（共 13 条新用例，模块内 16 → 29）。T13 接 worker 时务必手工验一次。
Task 9: complete (commits 5f9c156..1f35a88, tests: cargo test --lib --locked addon::feishu::domain::pull → test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 523 filtered out; finished in 0.00s)

### Task 10: rulings（完成行由 task-done 追加在本节末尾）

Ruling 21: 表级失败状态与告警的**分界**——`record_table_failure`（自增 + `last_error` + `last_pull_at`，返回新计数）落在 `domain/pull.rs`，与 `record_table_success` 成对；`infrastructure/feishu_pull.rs` 只放「落库 → 取发送器 → 按阈值发信」的编排（`record_table_failure_and_alert`）。
— Reason: 告警需要 `Tools` 里的发送器，而 `PullDeps` 里没有 `Tools`；反过来状态写入是 `pull.rs` 已有的职责（逐源路径的 `record_failure` 也在那儿）。brief 说「在 feishu_pull.rs 的失败路径上」，落地形态是「编排在 worker、写库在 pull」。
— Cost if wrong: 两个文件的分界挪一处，无行为差异。

Ruling 22: `impl FeishuAlertSender for SmtpEmailSender` 写在 `feishu/domain/alert.rs`，传输能力经**新增的 `SmtpEmailSender::deliver_text`**（`pub(crate)`）放出来；`deliver` 保持模块私有。
— Reason: 依赖方向。`feishu` 已经依赖 `account`（`user_from_claims`），把 trait 实现塞进 `account` 会让两个 addon 成环；且消息文案属飞书域。`deliver_text` 只放出「发一封纯文本」这一件事，from 地址与超时仍锁在 account 里。
— Cost if wrong: 多一个 10 行的薄转发；回退即把 impl 挪进 `account` 并删掉它。

Ruling 23: 阈值**下限取 2**、默认 3、上限 1000；`alert_recipients` 与阈值都在 `enabled = true` 的段上校验（沿用 `pull_interval_seconds` 的先例）。
— Reason: 阈值 1 会把**单次**失败也发出去，而单次失败与飞书抖动无法区分——那正是阈值机制要挡的邮件风暴。要「彻底静音」有更诚实的表达：清空 `alert_recipients`，而不是把阈值调到天上。校验放在 `enabled`（而不是 `can_pull()`）是因为错在配置里，越早暴露越好，且与既有那一块的判据一致。
— Cost if wrong: 想要「第一次失败就发信」的部署被拒（报错点名 `feishu.alert_failure_threshold`），改一个常量即可。

Ruling 24: 地址校验按「结构 + 危险字符」判（拒空白与控制字符、恰好一个 `@`、域名必须有点），且**不 trim 后再判**。
— Reason: 不 trim 是因为 `" a@b.com "` 在配置里看着配上了，投递时会被 lettre 拒掉——正是 brief 说的「以为配了其实没配」，那种错没有任何症状。要求域名有点是抓 `ops@example` 这种漏 `.com` 的笔误。
— Cost if wrong: 纯内网单段域名（`ops@mail`）配不上，启动报错并点名该项；放宽只需删掉 `domain_is_usable` 的一个条件。

Ruling 25: 失败路径**不加重发冷却**；`record_table_failure` 顺带推进 `last_pull_at`。
— Reason: brief 的 `should_alert(4, 3) == true`（「超过阈值后每轮都应继续告警，直到恢复」）把语义钉死了；加冷却会造出「没收到邮件 = 已恢复」的错误收敛信号。`last_pull_at` 是「最近拉取时间」——失败也是一次尝试，不推进会让一张一直拉不动的表在控制台显示成「从来没拉过」（逐源路径的 `record_failure` 也是这么写的）。
— Cost if wrong: 持续故障时每轮一封（收口靠阈值与恢复，不靠冷却）；`last_pull_at` 表示尝试时间而非成功时间——那是列本身的语义。

Note: 本任务改了 brief 的 Files 清单**之外**的 3 个源文件：`bootstrap.rs`（把 `FeishuAlertSenderHandle` 注册进 Tools 扩展槽——不注册则失败路径取不到发送器，邮件永远发不出去）、`account/domain/email_delivery.rs`（新增 `deliver_text`）、`tests/feishu_approval_options_integration.rs`（`FeishuSettings` 字面量要补两个新字段，否则 `--all-targets` 编译不过）。另有 4 处**配置文档**：`config.show.toml`、`docs/contracts/CONFIGURATION.md`、`deploy/README.md`（两处）、`deploy/config.cloud.example.toml`——后三者**逐一枚举了 `[feishu]` 的合法键**，不补就等于告诉运维「加 `alert_recipients` 会起不来」。

Note: 告警路径此刻是**运行期死的**：`record_table_failure_and_alert` 标了 `#[allow(dead_code)]`，消费者是 T13 的表级轮询（与 T9 的 `load_pull_tables`/`pull_table` 同一处置）。**T13 落地前不要部署**——worker 实际跑的逐源路径引用的列已被 T1 删除，每轮都会在第一次查询就失败。

Note: 有单测覆盖的是 `alert.rs` 的全部纯函数 + `alert_pull_failure` 的分发（用记录型假发送器，含「未达阈值不发」「没配收件人不发」「达阈值每个收件人都发」三条）。`record_table_failure` / `record_table_failure_and_alert` 是零覆盖路径（要数据库），T13 接线时手工验一次。
Task 10: complete (commits 1f35a88..9c6dc2a, tests: cargo test --lib --locked addon::feishu::domain::alert → test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 556 filtered out; finished in 0.00s)

# SDD ledger — 后端链（T11 → T12 → T13）

Spec: docs/architecture/feishu-datasource-table-config.md
Plan: docs/architecture/feishu-datasource-table-config-tasklist.md
上游 ledger: progress.md（T1–T10，含 25 条 ruling；本文件只记本链新产生的。）

## Progress


### Task 11: complete (commits 9c6dc2a..baf8f11, tests: cargo test --lib --locked addon::feishu → test result: ok. 384 passed; 0 failed; 0 ignored; 0 measured; 193 filtered out; finished in 0.03s; clippy -D warnings 干净; architecture passed)

Ruling 26: brief 第三个测试里的 `assert!(!report.table_missing)` 是**笔误**，改成 `assert!(report.table_missing)`，并补 `assert!(report.view_missing)`。
— Reason: `classify(&[], &[…], false, false)` 的入参就是 `view_ok = false` / `table_ok = false`，而测试自己的注释（「表被删了、视图也没了、还缺字段 → 三件事一起报」）与测试名（`…_not_short_circuited`）都要求把表写出来。原断言与它自己的意图直接矛盾。spec §9.3 的表格也把「数据表 / 视图被删除」列为**必须标出**的不可自愈项。
— Cost if wrong: 若原断言才是本意（即 `table_ok=false` 时不报 `table_missing`），则「数据表被删」这一整类问题**永远不会出现在体检报告里**——那正是 D6 要买的修复能力。回退只改一行断言与一行 `HealthReport::table_missing` 的赋值。

Ruling 27: 路由用 `POST /api/v1/feishu/datasources/table/health`，`datasource_id` 走 **body**，偏离 brief 写的 `/table/{id}/health`。
— Reason: 沿用 Ruling 12（`datasource_id` 走 body、路由不带路径段）。`/table` 这个前缀下 `POST` 已被 `create_datasource_table` 占用，所以体检必须带上一段区分——`.../table/health` 是既有命名风格（`pull-now` / `pull-probe` / `bitable-views` 都是「动作段」而非「id 段」）。
— Cost if wrong: 改路由字符串一处；RESTful 观感略有损失。

Ruling 28: 体检报告**加第五个键 `unchecked`**（brief 的 JSON 只有 4 个键），并在 `unchecked` 非空时把 `ok` 压成 `false`。
— Reason: brief 明写「异步壳负责把网络错误映射成「这一项查不了」」，而 bool-only 的 `classify` 表达不了「不知道」。两种压缩方式都更差：报成问题 = 假警报（运维为一次网络抖动白跑），静默略过 = `ok: true` 被读成「一切正常」（运维据此跳过一张坏表）。`unchecked` 让「查不了」既能被看见、又不冒充「问题」。
— 配套：`classify(&[], &[], …)` 这个形态**不能**拿空远端去比对绑定——那会把每一条绑定都报成「字段已被删除」。所以「查不了」那一支传空绑定列表；「有证据表没了」那一支才传真实绑定（`table_missing` 与逐列清单一起报，与 Ruling 26 同一条口径）。
— Cost if wrong: 多一个 JSON 键（前端的体检页要认它）。回退即把 `unchecked` 去掉、恢复 4 键形状；`ok` 的语义随之退回「没发现已知问题」。

Ruling 29: 「表不可达」只在出站失败**带得出证据**时上报：`Fatal { code }` 且 code ∈ {1254003, 1254040, 1254004, 1254009, 1254044} ∪ `CODES_PERMISSION_DENIED`。`Retry`（网络 / 频控 / 5xx）与 `Fatal { code: 0 }`（网关回了非 JSON 错误页）只进 `unchecked`。
— Reason: 与「改名不进报告」是同一条取舍——报一次网络抖动为「表被删了」就是让运维白跑一趟。1254024（字段名不匹配）也**不**算：它是字段层面的失败，不是表没了。
— Cost if wrong: 一次真正的「表被删」若恰好以非 Fatal 形态回来，会落进 `unchecked` 而不是 `table_missing`——问题仍然被报出来（连着原始原因），只是没有结构化的标志位。

Note: 权限写 `feishu.datasource.write`（沿用 Ruling 7：出站端点消耗本应用频控配额，不与纯读控制台查询共用权限位）。brief 没指定，此处照既有口径。

Note: 体检只比对 **`enabled = true`** 的绑定。停用 = 「取消勾选不删行」（Ruling 13），停用列的列被删不是问题。

Note: 端点在 `register_all` 里**无条件注册**（不在 `can_pull()` 分支里），与 T3/T4 的元数据端点一致：它会把 `require_settings` 的失败落成可归因的 `40901` / `40902`，比「路由不存在」更能说明问题。

### Task 12: complete (commits baf8f11..98d19a0, tests: cargo test --lib --locked addon::feishu → test result: ok. 395 passed; 0 failed; 0 ignored; 0 measured; 193 filtered out; finished in 0.03s; clippy -D warnings 干净; architecture passed)

Ruling 30: **不重实现 `seal` / `unseal` / `unseal_verified`**（Ruling 1 已把它们随 T5 落地）。本任务只做三件事：新增 `crypto::issue_token`、两个 Action、去掉那四个符号上的 `#[allow(dead_code)]`。
— Reason: brief 的 Files 里「Modify `crypto.rs`（加凭据加解密封装）」在 Ruling 1 之后已经是既成事实；重复实现会造出两份密文格式。
— Cost if wrong: 无（若 Ruling 1 被推翻，本任务只是一次薄接线）。

Ruling 31: `issue_token(key) -> Result<IssuedToken, BaseError>` 落在 `domain/crypto.rs`，**并把 `create_datasource_table.rs` 的 3 行内联签发改成调用它**（那个文件不在 brief 的 Files 清单里）。
— Reason: 建源与轮换是「签发一份凭据」的两个调用点。各写一遍迟早漂移，而漂移的失效形态是静默的——某一处漏了封存，那一行的「复制」按钮就永远失效，建的时候没人看得出来。沿用本链既有做法（Ruling 8 抽 `outbound_setup`、T6 抽 `field_binding`）。
— Cost if wrong: 一处 3 行改动，语义逐字等价（`generate_token` + `seal`），编译期即可验证；回退即把内联版粘回去。

Ruling 32: 两个端点路由用 `POST /api/v1/feishu/datasources/reveal-token` 与 `.../rotate-token`，`source_key` 走 **body**；偏离 brief 写的 `/datasources/fields/{source_key}/reveal`。
— Reason: 沿用 Ruling 12（本模块的实体标识一律走 body，路由不带路径段）。命名照既有动作段风格（`pull-now` / `pull-probe` / `bitable-views`）。**前端链（T15）需要知道这一点**——见下方 Note。
— Cost if wrong: 改两个路由字符串 + 取参处；前端要多改一处 URL。

Ruling 33: 回显的失败分成两个可归因的码，且**不**落成 `BaseError`：`40907 NOT_REVEALABLE`（这条绑定没有 `token_cipher`，是旧入口手填的，我们从来没有它的明文）与 `40906 CREDENTIAL_UNREADABLE`（有密文但解不开）。两者的文案都点明「轮换一次即可重新签发」。
— Reason: 两者修法不同（前者只能轮换，后者轮换也能修）且都不是内部故障——落成 `BaseError` 会变成 5xx 语义，把运维引向「找服务端日志」而不是「点轮换」。spec §10.2 明说手填的 Token 不可回显，这正是要把它说清楚的地方。
— Cost if wrong: 前端少一个可区分的分支；改成 `ConfigError` 只需两处。

Ruling 34: 审计摘要的键**不能含 `token` / `secret` / `hash` / `credential`**（`AuditSummary` 的 `SENSITIVE_FIELD_MARKERS` 会直接拒绝），所以轮换的摘要用 `rotated_at` 而不是 `token_rotated_at`。
— Reason: 这是框架的硬约束，不是选择。写成 `token_rotated_at` 会让**每一次轮换都失败**在审计构造那一步（而业务行已写好、事务会回滚，症状是「轮换永远报配置错误」）。
— Cost if wrong: 摘要少一个字段名，无行为差异。

Note: **回显只审计成功的那次**。失败的（没有密文 / 解不开）没有明文被出示，不算凭据暴露；`spec` 那句「每次追加审计」在这里落成「每次**成功**回显追加审计」。

Note: **前端链（T15）注意**：两个端点的路径是 `/api/v1/feishu/datasources/reveal-token` 与 `/api/v1/feishu/datasources/rotate-token`，入参是 `{"source_key": "..."}`（body，不是路径段）；响应是 `{"token": "<明文>"}`。回显的权限位是 **`feishu.datasource.secret`**（不是 `feishu.datasource.read`），前端按 operation_id 判权限时要认这一位。

Note: 两个 Action 的 `handle` 是**零单测覆盖**路径（要数据库）。有测试的是它们的全部纯函数：`open_credential`（5 例，含「没有密文」「贴错行」「换钥匙」）、`rotation_columns`、`CredentialError` 的码与文案，加上 `crypto::issue_token` 的 2 例。接线本身（事务、审计、`update_in_tx`）要手工验一次。

### Task 13: complete (commits 98d19a0..54849f0, tests: cargo test --lib --locked addon::feishu → test result: ok. 372 passed; 0 failed; 0 ignored; 0 measured; 193 filtered out; finished in 0.02s；cargo test --lib --locked → test result: ok. 561 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.09s；clippy -D warnings 干净; architecture passed; `--all-targets --no-run` 通过)

Ruling 35: 「路由已退役」这条断言**真跑了一遍装配出来的路由表**，而不是写一条永远为真的字符串比对。
— Reason: brief 给的 `registered_routes_for_test()` 在仓库里不存在（全仓没有路由枚举助手）。造一个假的等于什么都没验。`ModuleSpec::actions()` 是公开的，而 `FeishuContext` 只要求 `Repository`（`Repository::new` 要一个连接池）——用 `MySqlPool::connect_lazy` 拿一个**不建立连接**的池，就能在单测里装配出真正的 module 并列出它的路由。用例因此是 `#[tokio::test]`（`connect_lazy` 要求 Tokio 上下文）。
— 效果：这条用例的失败信息会把**整张路由表**打出来（实测：第一次跑就把 13 条路由原样列出来了），以后谁再加一个字段级入口都会在这里被点名。
— Cost if wrong: 用例多依赖一个惰性池的构造细节（sqlx 换了 API 就要改一行）；回退即删掉这条用例，退役只剩「文件不存在」这一层保障。

Ruling 36: `check_pullable` **删掉 `field_name` 参数**并删掉 `NotPullable::MissingFieldName`。
— Reason: 取数列已上移到字段绑定层，表级行上只有一个坐标三元组（Ruling 14 删掉的那四列里就有它）。留一个恒为 `Some(..)` 的参数只会让下一个读代码的人以为取数列还在表级。一条绑定都没勾的表**不是**「配置不全」——它只是没有要与飞书同步的东西（`pull_table` 直接返回）。
— Cost if wrong: 少一个预检变体；表现是「表级行建好了但一条字段都没勾」时点立即拉取会「成功触发但什么都不做」——那是真实语义，不是缺陷。

Ruling 37: **一轮的循环从 `domain/pull.rs` 搬进 `infrastructure/feishu_pull.rs`**（`run_round` 删除，`run_once` 变成表级循环）。
— Reason: 失败路径要发告警，发送器在 `Tools` 里，而 `PullDeps` 里没有它。Ruling 21 已经把分界定成「写库在 pull、编排在 worker」——落实它就意味着循环本身要待在拿得到 `Tools` 的那一层。反过来把 `Tools` 塞进 `PullDeps` 会把「拉取需要什么」从「凭证 + 传输 + 库」污染成「什么都可能要」。
— 副作用：T10 落地的 `record_table_failure_and_alert` **原样保留**（只是去掉了 `#[allow(dead_code)]`），不再是运行期死的路径。
— Cost if wrong: 一次搬迁，无行为差异；要搬回去就得给 `PullDeps` 加一个发送器句柄。

Ruling 38: `FeishuPullHandle` 的触发通道从 `Option<String>`（`source_key`）改为 **`Option<i64>`（表级 `datasource_id`）**；`pull_now` 入参 `source_key` → `datasource_id`。
— Reason: 拉取单位是表（设计 §6.2）——一张表一轮只扫一次。按 `source_key` 点名一条**字段**既省不下那次全表扫描，又会把表级状态写成「只拉了半张表」的样子。类型跟着语义走，改 `String` 为 `i64` 让「点名的是一张表」在编译期就成立。
— Cost if wrong: 改回字符串键；`bootstrap.rs` 只改一处类型标注。

Ruling 39: `pull_probe` **不做「改成只吃 `datasource_id`」的硬切换**，改为两种模式并存：给了 `datasource_id` 就走库（坐标 + 第一条启用中的绑定），只给坐标就还是直传。
— Reason: 计划写的是「入参从 `source_key` 改为表级 `datasource_id`」，但**探针现实中从来没有过 `source_key`**（它一直吃直传坐标）。硬切会把探针**唯一不可替代的用途**删掉：`pull_probe` 的全部价值就在「先于任何数据源行证明凭证链路可用」（它自己的模块文档第 59 行就是这么写的）；那时候库里一行都没有，没有 `datasource_id` 可给。走库模式则正好服务配置落库之后的排查。
— 附带：按 Ruling 8 的交代改用了 `domain/outbound_setup.rs`（删掉本地那 25 行凭证/传输组装与本地 `outbound_error`）。
— Cost if wrong: 多一个模式；回退即删掉 `datasource_id` 分支与 `resolve_target`。

Note: **删掉的**：`create_datasource.rs` / `update_datasource.rs` / `delete_datasource.rs` 三个文件；`pull.rs` 的 `SourcePullReport` / `RoundReport` / `PullSource` / `load_pull_sources` / `pull_source` / `select_sources` / `run_round` / `record_success` / `record_failure` / `source_is_still_active` / `internal_failure`。库内单测 584 → 561（少 23 条，全是那几条路径自己的用例）。

Note: `source_is_still_active` 改成 `table_is_still_active(deps, table_id)`（按 `id` 而非 `source_key` 查），消费者是 worker 的表级循环——「每轮开跑前重读存活」这条自保不能跟着逐源路径一起丢。

Note: **跨链串档（需要主控知道，本链未做任何破坏性补救）**：三个字段级 Action 的 `git rm` 删除被**前端链的提交 `526a892` 带走了**——它的 stat 里除了 `frontend/*` 还有 `create_datasource.rs` / `update_datasource.rs` / `delete_datasource.rs`（共 -1001 行）。成因：并发的两条链共享同一个 index，本链 `git rm` 之后、`git commit` 之前，前端链在同一个 index 上提交了一次。
— **最终状态是对的**：HEAD（54849f0）里那三个文件已不存在，引用它们的 `mod.rs` / `pull.rs` / `bootstrap.rs` / `feishu_pull.rs` 都已改完，561 条库内单测 + clippy -D warnings + 架构门禁全绿。
— **但 `526a892` 单独不可编译**（文件没了、引用还在），而且那条提交的 message（「对齐 T12 的契约」）与它实际带走的三处后端删除对不上。将来 `git bisect` 走到它会得到一个假结论。
— **本链刻意不做补救**：改写别人的提交是破坏性操作，且 HEAD 已正确。**留档给主控**：如需干净的二分历史，应在收尾时统一 `rebase`/重排，而不是由一条链自行改写另一条链的提交。以后凡本链要删文件，`git rm` 与 `git commit` 之间的窗口要压到最短（本次窗口里正好插进了对面的提交）。

Note: **前端残留（不由本链处理，必须让主控排期）**——`frontend/src/features/feishu/api.ts:69-71` 的 `DATASOURCE_OPERATION_IDS.create/update/remove` 仍指向这三个已退役的 operation_id（`api.ts:137/458/523/538` 在用），`frontend/src/engine/contracts/api-types.ts`（生成物）里也还留着它们。**本链没有动前端**：① 硬约束要求只 `git add` 自己的路径；② 前端链正在并发改这几个文件；③ `pnpm gen:contracts` 在本机受 corepack 阻塞（progress.md 的 T1 Note 已记）。**当前不会红**：`api-types.ts` 与 `api.ts` 互相自洽，`pnpm typecheck` 仍然过；而 `hasOperation(catalog, …)` 读的是**服务端实时目录**，所以旧按钮会自动消失而不是报错。**欠的账**是契约快照与前端常量表的清理，应在两条链都完成后跑一次 `python scripts/dump_openapi.py` 并让前端链对齐。

# SDD ledger — 前端链 — plan: docs/architecture/feishu-datasource-table-config-tasklist.md

Spec: docs/architecture/feishu-datasource-table-config.md（T14/T15 动工前读过 §3.2 / §4.3 / §9.3 / §10.2.1 / §10.3）
Execution: inline (native), on `main`，与后端链并发（只 `git add frontend/`）。

Ruling F1: 向导与清单都走**注入 client**（`TableWizardClient` / `CredentialClient`），而不是让组件自己摸会话与界面目录。
— Reason: brief 的用例写的是 `render(<DatasourceTableWizard />)`（无 props），那要求渲染一个组件先把整棵应用壳（会话控制器 + react-query + 目录）搭起来——测出来的东西有一半是壳的行为。真实装配放在 `api.ts` 的 `useTableWizardClient()` / `useCredentialClient()`，而「调的是目录声明的 Action」由 `api.test.ts` 用目录 + 桩 fetch 单独钉住（`requireAction` 对未知 operation_id 抛错，且断言「一个请求都不发」）。
— Cost if wrong: 组件多一个必填 prop；接线处（详情页）必须用那两粒 hook。回退即把 hook 调用移进组件并把 props 变可选。

Ruling F2: 「视图只决定拉取哪些行」这条说明文案挂在**视图选择器边上**（第 2 步），且 `listBitableFields` **一个 `view_id` 都不发**。
— Reason: 实测该参数对返回的字段集合与顺序零影响（T4 已有 `fields_url_does_not_send_view_id` 挂着后端那一侧）。前端再钉一条，是因为真正会犯错的地方在界面：文案与字段列表隔了两个步骤，容易被人当成两件事。
— Cost if wrong: 无——文案与请求形状都是实测结论。

Ruling F3: 父列下拉复用 `@/shared/ui/select`（照 brief），但断言改成「打开下拉后用 `option` 角色查候选」。
— Reason: brief 里 `within(parentSelect).getByText(...)` 对 Radix 是**恒真**的——候选项渲染在 portal 里，永远不在 trigger 元素内部，`queryByText(...)===null` 与 `getByText(...)` 都测不出过滤与否。改成查 option 列表才有鉴别力（并额外钉住「没勾的字段不在候选里」）。
— Cost if wrong: 无——断言更强，覆盖面变大。

Ruling F4: T15 的「复制不触发写请求」用**两个 spy**（`reveal` / `rotate`）钉，而不是 brief 的 `vi.spyOn(api, "rotateToken")`。
— Reason: 组件按 F1 走注入 client，直接 spy api 模块观察不到（组件不会去摸它）。换成对注入 client 的 spy 后，「点复制时到底调了什么」被钉得**更强**：复制 URL 一个客户端方法都不调，复制 Token 只调回显、绝不调轮换。被断言的性质（决策 D10：误点复制不能有任何后果）一字不差。
— Cost if wrong: 无——性质等价且断言更细。

Ruling F5: 回显改成**按需**（点「复制 Token」时才调 reveal），明文不常驻 DOM/内存。
— Reason: 清单是常驻的一页，而凭据是 `secret` 级；设计 §10.2.1 的「复制是纯读、反复复制同一个值」正好落在这条路径上（reveal 本身就是纯读且每次记审计）。轮换后新值由那次响应带回并留在组件状态里——它就是要粘回去的那一串，必须可见可复制。
— Cost if wrong: 每次复制多一次往返；这是刻意的取舍（换来的是凭据不在页面上停留）。

Ruling F6: `CredentialItem.tokenRotatedAt` 是**三态**：`undefined`=拿不到、`null`=从未轮换、数字=那次时间。
— Reason: 列表端点（T7）的绑定投影里**没有** `token_rotated_at`，所以「拿不到」是常态。把它画成「从未轮换」是一句可查证的假话，而这一格正是给人看「凭据是刚换的还是早就配好的」。界面对 undefined 显示「—」。
— Cost if wrong: 多一个可选态；将来某个端点带上这个时间，改一行即可。

Ruling F7: 体检按 **`datasource_id` 走请求体**（`/api/v1/feishu/datasources/table/health`），不是计划里写的 `/table/{id}/health`。
— Reason: T11 已提交，路由是 `/table/health` + body（与后端既有的「标识走 body」口径一致，见后端 ledger 的 Ruling 12）。前端以**已落地的实现**为准，不然就是一个必然 404 的调用；测试仍用桩，不因为后端后续重构而变。
— Cost if wrong: 改回路径段只需改一处 route 断言与一个请求体。

Ruling F8: T12 落地后**回填**契约：reveal/rotate 的真实路由是 `/datasources/reveal-token`、`/datasources/rotate-token`，`source_key` 走请求体。
— 过程：动工时 T12 未合并，我按计划写的 `{source_key}` 路径段写了一个三形状兼容的封装（声明成 path 参数 / 路由有段但 handler 没声明 / 路由无段）；后端一合并（98d19a0）就取到真实路由，**删掉那层封装**、按真实契约写死，并把两条测试改成断言请求体里的 `source_key`。
— Reason: 计划里的路由是预测，落地代码是事实。留着三种分支的兼容层就是给一个已解决的问题常驻抽象。
— Cost if wrong: 无（已对齐真实实现；若后端再改路由，改动集中在一个函数与两条断言）。

Note: **表级化之后 `DatasourceItem` 才可能没有 `source_key`**（它在每条字段绑定上）。两处跟着改：① parse 不再因为缺 `source_key` 丢掉整行（旧行为会把一条真实存在的数据源显示成**不存在**），改用 `id` 兜底身份；② 详情页按「表级 `source_key` **或** 任一绑定的 `source_key`」定位那一行——表级世界里路由参数的含义就是一个字段的取数标识（选项表就是按它查的）。新增 `id` / `fields` 两个字段是**追加**，存量调用点不受影响。

Task 14: complete (commits 9c6dc2a..ff1b67f, tests: cd frontend && pnpm vitest run tests/features/feishu → Test Files 12 passed (12) / Tests 192 passed (192); pnpm typecheck 干净; eslint --max-warnings 0 干净)

Note: 交付物是 `DatasourceTableWizard.tsx`（四步）+ `FieldPickerTable.tsx`（第三步的全量字段网格）+ `api.ts` 的四个端点封装 + `types.ts` 的 `SOURCE_KEY_PATTERN` / `sourceKeyFromFieldId` / `fieldTypeLabel`。**向导没有挂到任何页面上**——计划里 T14 的 Files 清单不含页面，挂载处（列表页的「添加」入口）属表级化的前端改造，见下面那条缺口。

Task 15: complete (commits ff1b67f..526a892, tests: cd frontend && pnpm vitest run → Test Files 53 passed (53) / Tests 502 passed (502); 其中 tests/features/feishu → Test Files 14 passed (14) / Tests 221 passed (221); pnpm typecheck 干净; eslint --max-warnings 0 干净)

Note: 交付物是 `CredentialChecklist.tsx`（每行两个可复制项 + 独立的轮换按钮与确认框）+ `DatasourceHealthPanel.tsx`（缺失字段按 `field_id` 与 `source_key` **一起**列出；「这次没查成」单列并压住「通过」）+ 详情页的两段 + `types.ts::credentialItems`（纯函数，单测在 `credential-items.test.ts`）。

Note: 详情页的两段都按**目录里有没有对应 Action** 渲染：没有 `health_check` 就写「当前身份没有体检权限」而不是画一个必然失败的按钮；回显与轮换**各看各的权限位**（只给回显的部署，轮换确认框里说「没有轮换权限」）。这保证存量用例（目录里没有这三粒的）不会发出没被覆盖的请求。

Note: **已知缺口（T14/T15 之外，主控需要决定）**：控制台其余部分仍是字段级口径——列表页按 `source_key` 渲染台账/卡片与「添加数据源」入口、`DatasourceFormDialog` 发的还是 `create_datasource`（T13 把它退役后这条路径就是死的）。表级化要端到端可用，还差一条链：列表页改成按表级行渲染（`title` + 绑定数 + 表级同步状态）、「添加」接到 T14 的向导、编辑/删除接到 T6 的 `update/delete_datasource_table`。本条缺口在本链的授权范围之外（brief 只给了 T14/T15 的 Files 清单），故不动手，只记账。

Note: 测试基础设施改了两处，都在 `frontend/tests/`：`harness.ts` 新增 `tableConfig` 开关 + 三个端点桩（体检/回显/轮换），并把该开关编进**目录 revision**（引擎的 `CatalogCache.accept` 在 revision 相同时直接复用上一份目录，不加这一位会出现「同一文件里前一个用例的目录串味」——**这是踩过的坑**）；`api.test.ts` / `detail-page.test.tsx` 的回显/轮换断言按 T12 的真实路由改写。

### 修复链复核（后端半边，2026-09-23，主控独立验证）

commit `5a7d523`（H3/H5/M8）+ `style: 补 rustfmt`。

主控**不看自报、直接查代码**三条：
- **H3 已消失**：`field_table.rs:48` 的 `token_hash` 有了 `.unique(true)`；测试
  `token_hash_is_unique_so_two_bindings_cannot_share_a_credential` 断言 `token_hash.storage.unique`。
- **H5 后端半边已消失**：`list_datasources.rs` 的绑定投影带上了 `token_rotated_at: Option<i64>`
  （`:33` 结构体 / `:55` 读取 / `:187` 投影列），并有测试钉住键名是 snake_case、
  且缺列时给 `null` 而不是缺键（前端契约是 `number | null`）。
- **M8 已消失**：`derive.rs` 的注释重写，明写「**「一子多父」是存在的，不要照抄旧结论**」，
  点明旧结论的来源数据集（银行网点 xlsx）与目标台账的 6 例。

门禁：`cargo test --lib addon::feishu` → 375 passed；clippy `-D warnings` → 0 警告；
架构门禁 passed；**fmt 原本没干净**（`list_datasources.rs:287`，修复链漏跑 `cargo fmt`）——
这正是「逐任务只跑 clippy」抓不到的一类，已补 fmt 并单独提交。


<!-- merged from progress-fix-rust.md -->

# SDD ledger — Rust 修复链（H3 / H5 / M8）

Spec: docs/architecture/feishu-datasource-table-config.md
Plan: docs/architecture/feishu-datasource-table-config-tasklist.md
上游 ledger: progress.md（T1–T15，含 25 条 ruling 与前端链的 F1–F8）。本文件只记本链新产生的完成行与 ruling。
Execution: inline (native)，on `main`，与前端链并发（只 `git add src/…` 自己的三个路径；前端链对 `frontend/*` 的改动一律不碰）。
逐修复验证命令：相关 `cargo test --lib --locked <范围>` + `cargo clippy --all-targets --all-features --locked -- -D warnings`。**未跑** `run_ci.py`、**未跑**任何集成测试或数据库写操作。

## Progress


### H3: complete (commits 54849f0..5a7d523, tests: cargo test --lib --locked addon::feishu::datasource::domain::field_table → test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 559 filtered out; finished in 0.00s；clippy -D warnings 干净)

Ruling 40: **这是一条被计划漏掉的 MUST**，不是「计划写错」。设计 §10.2 第 2 条要求 `token_hash` 有唯一索引；计划 T1 把旧声明（`options/table.rs` 时代的 `table.rs:35-40`）逐字照抄，旧声明里只有 `source_key` 有 `.unique(true)`，于是这条 MUST 一路落空到本次修复，期间没有任何任务认领它。
— **实际后果比原表述窄，不要照抄**：出站是按 `source_key`（URL 路径段）先查到**行**、再比对那一行的摘要，所以「按 Token 反查串源」在当前路径上**不成立**。真实后果收窄为「两个绑定拿到同一个 Token 时，该 Token 对两张表都验得过」——一条脏数据，而不是一条串源通道。而 `update_datasource_table.rs` 会封存**手填** Token，人手给的字符串不保证唯一，所以唯一索引不是纸面要求：它把「两条绑定撞同一个凭据」挡在写入那一步。
— 复现测试读的是 **DSL 层**（`TableSpec.fields[*].storage.unique`，`fields` 是 pub 字段），不是编译后的 `TableDefinition`——后者不暴露索引（`table_definition()` 只给字段元数据，与 `tests/feishu_options_integration.rs:103` 里那句注释一致）。故本修复的单测与那条 `#[ignore]` 集成测试**不重复**：那条连库、验 DDL 真的落成唯一索引；这条在单测里钉住**声明本身**，无需数据库。
— Cost if wrong: 若某个已上线部署里真有两条绑定共用同一个 Token，schema 同步会在建索引时被旧数据挡住（启动期预检只读扫描后报出表与主键并**拒绝全部 DDL**，不会半改）。本仓库数据库上线前清空，无迁移负担。

### H5（后端半边）: complete (commits 54849f0..5a7d523, tests: cargo test --lib --locked addon::feishu::datasource::actions::list_datasources → test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 562 filtered out; finished in 0.00s；clippy -D warnings 干净)

Ruling 41: 对外键名 **`token_rotated_at`**（snake_case，与绑定项其余键一致），类型 **`Option<i64>`**：JSON 里恒是 `null` 或数字，**不缺键**。用 `Option` 而不是 `0` 是因为「手填凭据从未轮换过」与「轮换过但时间读不出来」是两回事，用 `0` 会把它画成 1970 年那次轮换。
— 三处一起改：`FieldBindingItem` 加字段、`group_bindings` 取值、`handle` 的 `select_fields` 列出该列（不列的话投影里永远没有它）。
— **前端链（T15）交接**：绑定项契约现在是 `token_rotated_at: number | null`。前端 ledger 的 Ruling F6 把 `CredentialItem.tokenRotatedAt` 设计成三态（`undefined`=拿不到 / `null`=从未轮换 / 数字），本修复之后 `undefined` 经**这个端点**不再可达——留着无害（旧快照/旧部署仍可能给不出），但「列表端点没有这一列」这个理由已经不成立了。
— Cost if wrong: 响应 schema 多一个键，是**追加**。`frontend/contracts/openapi.json` 是生成物且正被前端链并发改写，本链**未动**它；前端的 `openapi-contract.test.ts` 只断言**输入** schema（requestBody 与 parameters），响应加键不影响那条用例。

### M8: complete (commits 54849f0..5a7d523, tests: cargo test --lib --locked addon::feishu::domain::derive → test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 554 filtered out; finished in 0.00s；clippy -D warnings 干净)

Ruling 42: 这是**注释类修复，无复现测试**，也没有为它编一条假测试（TDD 在这里没有可断言的被测量：改的是文档文本，`derive_options` 的行为一字未动）。
— 改动范围：`src/addon/feishu/domain/derive.rs` 的 `RawValue::parent_label` 文档注释，按设计 §4.7 回写——说清「0 例『一子多父』」的来源数据集是**银行网点 xlsx**、对**目标台账不成立**（`费用类型 → 银行流水摘要-编码` 实测 **6 例**，如 `pay for services-YL-AR` 同时挂在 `推广测评服务费` 与 `预付储值款` 下），并说明设计扛得住这种情形（`option_id` 把 `parent_key` 哈希进输入，同文案不同父派生成两个选项，`derive.rs` 内有测试钉着）。
— **算法不变**；`derive.rs` 的 14 条既有用例全绿即为「只改注释」的回归证据。
— Cost if wrong: 无。若将来要按 label 折叠，这条注释正是拦住它的理由。


<!-- merged from progress-fix-frontend.md -->

# SDD ledger — 前端修复链（H1 / H2 / H4 / H5 / L9 / M6）

Spec: docs/architecture/feishu-datasource-table-config.md
Plan: docs/architecture/feishu-datasource-table-config-tasklist.md
上游 ledger: progress.md（T1–T15 + F1–F8）、progress-fix-rust.md（H3 / H5 后端半边 / M8）。本文件只记本链新产生的完成行与 ruling。
Execution: inline (native)，on `main`，与 Rust 链并发（只 `git add frontend/`；对 `src/**` 的改动一律不碰）。
逐修复验证命令：`cd frontend && pnpm vitest run tests/features/feishu` + `pnpm typecheck` + `npx eslint . --max-warnings 0`。**未跑** `run_ci.py`、**未跑**集成测试或任何数据库写操作。
收口门禁（两条链之后由主控跑）：全量 `pnpm vitest run` → Test Files 53 passed (53) / Tests 495 passed (495)。

## Progress


### H1: complete (commit 00788f5, tests: cd frontend && pnpm vitest run tests/features/feishu/views/datasource-pull.test.tsx → Test Files 1 passed (1) / Tests 6 passed (6))

红：先改测试再跑，得到
`AssertionError: expected { source_key: 'expense_category' } to not have property "source_key"`（`- Expected: undefined` / `+ Received: "expense_category"`）。
断言按 brief 的要求落在「后端会拒」这件事上：`expect(body).not.toHaveProperty("source_key")` + `expect(body).toEqual({ datasource_id: 7 })`——后端 `PullNowInput` 是 `deny_unknown_fields` 且必填 `datasource_id`，旧形状 100% 被拒。
绿：`pullNow(datasourceId: number)` 发 `{ datasource_id }`；`FeishuActions.pullNow` 与详情页调用点跟着改；详情页的 `canTrigger` 追加 `item.id !== null`——拉取按表级主键定位，缺它那个按钮点下去什么也发不出去。

### H2: complete (commit 00788f5, tests: cd frontend && pnpm vitest run tests/features/feishu/api.test.ts → Test Files 1 passed (1) / Tests 45 passed (45))

红（**真跑出来的**，不是推演）：把 `DATASOURCE_OPERATION_IDS.create` 临时改回 `"feishu.datasource.create_datasource"` 再跑，得
`AssertionError: expected false to be true // Object.is equality`（`canWriteDatasources(真实目录)`），以及 `三个写操作都按表级主键定位` 一条红。改回来即绿。
绿：`DATASOURCE_OPERATION_IDS` 的 `create/update/remove` 指向 `create_datasource_table` / `update_datasource_table` / `delete_datasource_table`（逐个对着 `src/addon/feishu/datasource/actions/*.rs` 的 `action_name!(...)` 核过；`list` / `pullNow` / `pullSchedule` 原样），`reveal_token` / `rotate_token` / `health_check` 已在 `TABLE_OPERATION_IDS` 里。
页面级证据：`写侧权限门控` 三条 + 列表页权限用例（目录里只有真端点时「添加数据源」与行内菜单都在）。

Ruling F9: **列表查询的确定性收尾键从 `source_key` 换成 `id`**——这是 brief 六条之外的一处，但**不修则 H2/H4 无从验证**。
— 事实：`feishu_datasource` 表级行上**没有** `source_key` 这一列（`datasource/table.rs` 的 `the_table_row_carries_no_credential_or_routing_columns` 明写它在字段绑定那一层），而 `withStableOrder` 恒把它追加成收尾键、`DEFAULT_ORDER_BY` 也以它为默认。后端 `validate_order_field` 对不存在的字段直接 `FieldNotFound`（`crates/yang-base/src/table/table_query/validation.rs:84`）——所以那不是「排序不生效」，是**列表请求整个打不开**：`canWrite` 修成 true 之后，页面仍会停在错误条上。
— 复现（先红）：`list-query.test.ts` 新增「收尾键是表级行的真唯一键 `id`」等三条 + `api.test.ts` 的方向/收尾断言，改实现前 6 条红、3 条绿。
— 连带改动：`DatasourceSortField` 只剩 `"title"`；台账的「标识」列不再挂排序箭头（表级行上没有单一的 `source_key`，点一下就是一次 FieldNotFound），显示改成「首个绑定的标识 → 落单行标识 → `#主键`」三级兜底（`types.ts::identityLabel`，卡片与台账共用）；行 key 从 `source_key` 换成 `id`（否则整页同名空 key）。
— Cost if wrong: 默认排序从「按标识」变成「按名称」，运维的肌肉记忆要改一次；后端若把 `source_key` 加回表级行，改回来是 `list-query.ts` 一个常量 + 两处断言。

### H4: complete (commit 00788f5, tests: cd frontend && pnpm vitest run tests/features/feishu/views/datasource-list-page.test.tsx → Test Files 1 passed (1) / Tests 25 passed (25))

红（**真跑出来的**）：把列表页的 `<DatasourceTableWizard open={wizardOpen}/>` 临时改成 `open={false}` 再跑，7 条红，首条报 `TestingLibraryElementError: Unable to find role="dialog"`——正是「向导没挂在页面上」时用户看到的样子。恢复即绿。
绿：工具栏与空态两处「添加数据源」都开向导；`onSubmitted(created, submission)` 关向导 → 回读列表 → 拿服务端回的**第一条凭据**跑一次预检（明文只在这一刻出现）。`onSubmitted` 因此多带一个 `submission`（回执里要写用户刚填的名称，调用方从响应里读不到）。
编辑/删除接 T6：`updateDatasourceTable({datasourceId, title, fields})` / `deleteDatasourceTable(datasourceId)`，页面新增 `DatasourceEditDialog`（改名称，绑定集合原样带回）。

Ruling F10: **删掉 `DatasourceFormDialog.tsx` 与列表页的「停用 / 启用」入口**——它们指向的端点已经不存在。
— 事实：T13 删掉了字段级 `create_datasource` / `update_datasource` / `delete_datasource`；`update_datasource_table` 的入参**没有** `status`（只有 `datasource_id` / `title` / `ingest_mode` / 三个坐标 / `fields`），`list_datasources` 也不再投影 `encrypt_enabled` / `default_locale`。那个对话框的每一项（标识、Token、加密返回、默认语言、坐标、级联）现在都没有可落地的入口，把它留在页面上就是留一个点了必然报错的按钮——正是 `SyncPanel` 注释里说的「把『这个部署没开』错报成一次功能故障」。
— 复现：不是新写一条红测试，而是**替换**掉 5 条依赖旧路径的用例（创建回执那 5 条改成走向导，编辑那 3 条改成断言表级 PUT 的请求体），`dialogs.test.tsx` 里 `DatasourceFormDialog` 的四段 20 条用例随组件一起删掉（`ConfirmDialog` 那 6 条保留）。
— Cost if wrong: 「停用 / 启用」是用户可见能力的**减少**。恢复路径：后端在 `update_datasource_table` 加 `status` 入参（+ T6 的 projection 加 `encrypt_enabled` / `default_locale`），前端把菜单条目与对话框加回来。本链不做这个后端决定，故只删不补。

Ruling F11: 编辑对话框**只改名称**，并把这一刻**启用中**的绑定原样带回（`api.ts::enabledBindingInputs`）。
— Reason: 后端 `fields` 是必填且「整份替换」；服务端对集合里出现的已有绑定会写 `enabled = true`（`update_datasource_table.rs` 的 `to_update` 分支），把停用的那条塞回去等于在「只是改个名字」时悄悄把它重新启用。集合里没有的行只会被停用（幂等），省略它们是安全的。
— 因此：一条**启用中绑定为 0** 的数据源改不了（后端 `validate_fields` 要求至少一条），对话框在那种情况下说明原因并禁用保存，而不是发一个必然被拒的请求（`没有启用中的绑定时不放行保存` 一条钉住）。
— Cost if wrong: 若将来把向导做成「编辑模式」，这个对话框就该退场（它只是「名称」那一项的就地入口）。

### H5: complete (commit 00788f5, tests: cd frontend && pnpm vitest run tests/features/feishu/credential-items.test.ts tests/features/feishu/components/credential-checklist.test.tsx → Test Files 2 passed (2) / Tests 20 passed (20))

红：先加两条测试再跑，得
`AssertionError: expected undefined to be 1758000000`（绑定带回了 `tokenRotatedAt` 却到不了清单行）、
`AssertionError: expected undefined to be null`（服务端明确回 `null` 时被折平）。
绿：`api.ts::rotatedAtOf` 按**键在不在**读 `token_rotated_at`（键不在 = `undefined` 拿不到 / `null` = 明确从未轮换 / 数字 = 那次时间），`DatasourceFieldBinding.tokenRotatedAt?: number | null` 承接，`credentialItems` 原样透传——`CredentialChecklist` 那一列本就画着三态，红线是中间的**管道**断了。接口契约与 Rust 链 Ruling 41（`Option<i64>`，JSON 里恒 `null` 或数字、不缺键）一致。

### L9: complete (commit 00788f5, tests: 同上门禁)

红：这是「绿着错」的元凶，red 的证据就是 H1 的**旧**断言曾经是绿的——`toMatchObject({ source_key: SOURCE_KEY })` 把错形状钉住了；而替身目录里声称存在 `create_datasource` / `update_datasource` / `delete_datasource`，于是 H2 的恒 false 在替身世界里看不见。
绿：`views/harness.ts` 的目录换成真实的表级七个端点（`create/update/delete_datasource_table` + 三个 bitable 元数据 + `pull_now`），路由桩按 method 分流 `/datasources/table`；`api.test.ts` 的 `DEPLOYED_ACTIONS` 同改。三个退役 id 在 `frontend/` 里已经一个都不剩（`grep` 只命中说明性注释）。

Ruling F12: 契约快照**用仓库脚本重新生成**，不手改。
— 命令：`python scripts/dump_openapi.py`（内部 `cargo run --locked --example openapi-dump frontend/contracts/openapi.json` + `pnpm exec openapi-typescript`），之后必须对 `src/engine/contracts/api-types.ts` 跑一次 `prettier --write`——`.prettierignore` 里写的是 `src/contracts/api-types.ts`（**路径漂移，少了 `engine/`**），所以生成物其实一直在格式门禁里。
— diff 范围只有飞书那一批路径（`grep` 过 `/api/v1/...` 的新增 path 全是 `feishu.datasource.*`），退役的三个 id 消失、表级七个出现。`pull_now` / `pull_schedule` / `pull_probe` **不在快照里**是正常的：它们只在 `can_pull()` 为真时才注册（与 `api.ts` 的既有注释一致）。
— **跑过两次**：第一次在 `54849f0`（Rust 链的 H3/H5 修复尚未提交），第二次在 `2f77b63`（含 `5a7d523` 的 `token_rotated_at` 投影）。两次产物**逐字节相同**（`git status frontend/` 干净），dump 是确定性的；`token_rotated_at` 不出现在快照里也正常——那些端点的响应体是 `serde_json::json!` 拼的，不是带 `JsonSchema` 的类型，所以响应 schema 本来就是泛化的。
— Cost if wrong: 生成物与后端源码同步，无行为影响。

### M6: complete (commit 00788f5, tests: cd frontend && pnpm vitest run tests/features/feishu/components/table-wizard.test.tsx → Test Files 1 passed (1) / Tests 9 passed (9))

红：先加用例再跑，得 `TestingLibraryElementError: Unable to find an element with the text: /回审批后台/`（第 4 步当时只有「创建后不可修改」）。
绿：向导第 4 步在 `SOURCE_KEY_HELP` 下面补 `SOURCE_KEY_WARNING`——「源标识进的是那个审批控件的外部选项地址。换一个就等于换了地址——必须回审批后台把控件里的外部选项地址一并改掉，否则它会立刻取不到任何选项。」

### 遗留缺口（不在本链六条内，主控需要决定）

- 台账 / 卡片的「加密返回」「默认语言」两列与 `DatasourceItem` 上那两个字段：`list_datasources` 已经不再投影它们，所以恒显示「—」。要真恢复得后端先加回投影（或改用字段绑定那一层的语义）。
- 坐标（Base Token / 数据表 / 视图）在列表页是只读展示：T6 的 `update_datasource_table` 支持改，但本链没做那个表单（H4 只要求「编辑/删除接表级入参」）。
- 状态（`active` / `disabled`）现在**没有任何写入路径**：`update_datasource_table` 没有 `status` 入参，页面因此不再提供入口。
- 详情页按**字段标识**路由（`/feishu/datasources/:sourceKey`）不变：选项端点仍是按 `source_key` 索引的，一条表级行有 N 条绑定，列表页取第一条当入口；一条绑定都没有的行点不进详情页，页面给一句说明而不是静默无响应。

### 主控收口（2026-09-23）

**提交**：`2f77b63` 补 rustfmt（后端修复链漏跑 fmt）；本次再提交过期集成测试的表级改写 +
`source_key` 唯一性断言（`tests/feishu_options_integration.rs`、`field_table.rs`）。

Ruling 43: 集成测试 `feishu_tables_are_created_with_required_unique_indexes` 断言的是**旧模型**
（要求表级行上有 `source_key` 唯一索引），而 Ruling 14 按 spec §5 把它移到了绑定表 —— 判定为
**测试过期**而非回归，按表级口径改写：三张表的存在性、`source_key`/`token_hash` 唯一索引搬到
`feishu_datasource_field`、**新增**「表级行上不再有 `source_key`/`token_hash`」的断言。
— 副作用（正面）：**H3 与 Ruling 14 由此获得对真实 MySQL schema 的验证**，不再只是 spec 声明。
— 执行：`cargo test --test feishu_options_integration --locked -- --ignored --test-threads=1` → 2/2 通过。

Ruling 44（**本次最严重**）: `approval_options.rs:277-289` 至今仍在 `feishu_datasource` 上查
`source_key`/`token_hash`/`encrypt_enabled`/`default_locale`/`linkage_mapping` —— **这五列全已删除**。
集成测试原文：`1054 (42S22): Unknown column 'source_key' in 'field list'`。
— 这是**已在生产跑通的出站链路**，现在是死的；而所有绿色门禁都覆盖不到它
（`approval_options` 单测全是纯函数不碰库，唯一碰库的集成测试是 `#[ignore]` 且从未跑过）。
— 根因是**设计文档 §8 的一处错误**：「读端消费的仍是同一份 `linkage_mapping`，所以级联过滤逻辑
不需要改」—— 列已被删，这句不成立，计划照着它做于是无人接这一段。
— 处置：派专门代理重接（按 `source_key` 查绑定行 → 按 `datasource_id` 查表级行取 status；
级联改由 `parent_field_id` 推出）。**这一条说明「跑集成测试」这个决定的价值**。

Ruling 45: 复核者指出的两处小账一并处理：`tests/feishu_options_integration.rs` 改成表级口径后
**一直未提交**（主控疏漏，已补）；`field_table.rs` 的 `source_key_is_unique_and_filterable`
测试名声称 unique 却只断言 required/filterable/sortable —— 补上 DSL 层的唯一性断言
（`token_hash` 的唯一索引当初就是这样漏掉的）。

### 全部完成（2026-09-23 收口）

**集成测试（用户授权后，`yang_system_test` + Redis 第 15 号逻辑库）：12/12 命令、50 条测试全过。**
`python scripts/run_ci.py integration` → 退出码 0（真实退出码，未用管道/echo 吞掉）。

**全量门禁**：`cargo test --lib` 553 passed / clippy `-D warnings` 0 / fmt 干净 /
架构门禁 passed / `--all-targets --no-run` 0 error / 前端 typecheck exit 0 / vitest 53 files · 495 tests。

Ruling 50: 本 ledger 被 `.superpowers/sdd/.gitignore` 忽略，49 条 Ruling 会随临时区一起消失 ——
已复制到 `docs/architecture/feishu-datasource-table-config-decisions.md` 并入提交。
— 理由：这些决定（尤其 Ruling 14 / 43 / 44 / 45）是本次改造最贵的产出，重推一遍的成本远高于存一份原始文本。
— Cost if wrong: 仓库里多一份格式粗糙的决策流水；可随时删除。

Ruling 51: 集成测试**必须**在表级改造这类「列被搬走」的重构中跑。
— 证据：本轮 12 条里有 1 条过期测试（Ruling 43）与 **2 处真实回归**（Ruling 44 出站查死列、
Ruling 45 `id` 缺 `filterable` 打穿 14 处主键定位），全部**逃过了 553 条单测 + clippy + 架构门禁**。
根因是结构性的：这些路径的单测都是纯函数（不碰库），唯一碰库的集成测试是 `#[ignore]`、默认不跑。
— Cost if wrong: 无（这是一条观测，不是选择）。

Ruling 52: 两处能力删除（前端 Ruling F10）**保留删除状态**，记账不改。
— 事实：`DatasourceFormDialog.tsx` 及其 20 条用例已删；列表页「停用/启用」入口已删，因为
`update_datasource_table` 的入参里没有 `status`，后端已无写入路径。
— 后果：**当前无法停用一个数据源**（只能删）。要恢复得先给表级 update 补 `status` 入参。
— 留给用户定：本次不擅自恢复（后端没有对应能力，恢复入口就是恢复一个必然报错的按钮）。
