//! `feishu_option` 的写机制——**不依赖 `ActionContext`**。
//!
//! # 为什么单独抽出来
//!
//! 两条写入路径要共用同一套语义，但它们的调用方形态完全不同：
//!
//! | 路径 | 调用方 | 凭证 | 审计 |
//! |---|---|---|---|
//! | 入站推送 | HTTP Action | 管理 Token 中间件 | 走 ctx 的 `succeeded_system_event` |
//! | 出站拉取 | 后台 worker | 无（进程内） | 走 ctx-free 的构造器 |
//!
//! worker 手里**没有也不可能有**可用的 ctx：`succeeded_system_event` 要读
//! `ctx.dispatch_target()`，而它唯一的 setter 是 `yang-base` 的 `pub(crate)`——
//! 自造的 ctx 必然硬失败在 `ConfigError`。所以审计事件由调用方注入，本模块只管写。
//!
//! # 写语义：单条多行 ODKU，**没有 WHERE**
//!
//! [`apply_option_rows`] 把整批拼成一条 `INSERT ... ON DUPLICATE KEY UPDATE`，冲突键是
//! 表上的唯一索引 `uk_feishu_option_option_id`。这条语句**没有 WHERE**，于是逐行路径里
//! 「`where_eq(option_id)` + `where_eq(source_key)` 0 行再插入」中由 WHERE 兑现的归属约束，
//! 在批量路径上只剩两道补平：
//!
//! 1. 事务**前**的预检 [`find_foreign_option_owner`]（可归因的业务失败，不烧可用性）；
//! 2. 事务**内、提交前**的写后归属再断言（见 `apply_option_rows` 内的注释）。
//!
//! **两道都要**：预检与写入之间是 TOCTOU 窗口；写后断言才是「这一轮真的覆盖了别人的行」
//! 那个可证伪的判据。预检仍留在两个调用点的事务**之外**（失败时不占事务、错误可归因），
//! 本函数只负责事务内的写与再断言。
//!
//! 这是批量化的**唯一差异面**：跨源夺取从「撞唯一索引响亮失败」变成可能静默，
//! 靠上面两道把它挡回响亮失败。
//!
//! 两条路径在**记录内容**上不同，这也是本模块不自己构造记录的原因：
//! 入站推送是**合并**（省略的字段保持原值，所以 `enabled` 是单向的），
//! 出站拉取是**整行替换**（把本轮派生出的全部列一并写入，否则「删→停用→加回」
//! 会让 `enabled` 永久停在 `false`）。
//!
//! # 计数口径
//!
//! 本模块只报 `rows_affected`：多行 ODKU 的 `rows_affected` ∈ `[N, 2N]`，1=插入、
//! 2=更新且值有变，「命中但值未变」也计 1（sqlx 带 `CLIENT_FOUND_ROWS`），
//! 结构性**分不出 inserted/updated**。推送路径 `upsert_options` 仍是逐行合并语义、
//! 照旧报 `{inserted, updated}`——同一张表两条写入入口的计数口径**从此分叉**，
//! 读日志时别把它们当同一个数。

use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

use super::repository::Repository;

/// 单批最多写多少行——**只服务 `where_in` 分片**（跨源预检、补集停用）。
///
/// 批量写入自己的分片归 yang-db（`INSERT_BATCH_SIZE` / `derive_upsert_batch_size`），
/// 这里不再管。取值仍**不能超过 `where_in` 的 500 元素上限**（`validation.rs` 的
/// `MAX_IN_LIST_SIZE`）——超限会在进事务前被 `ParamInvalid` 打回。
pub(crate) const MAX_BATCH: usize = 500;

/// 跨源夺取预检：返回第一个「id 属于**别的**数据源」的 `(option_id, owner)`。
///
/// 放在事务之前：既是可归因的业务失败（不烧可用性预算），也不会让一次脏数据把
/// 整批写入带坏。批次**按 500 分片**（`where_in` 的元素上限），命中即提前返回。
///
/// **归属比较交给数据库做。** 表的排序规则是 `utf8mb4_unicode_ci`（大小写不敏感 +
/// PAD SPACE）：唯一索引 `uk_feishu_option_option_id` 的冲突判定、本预检、以及
/// `apply_option_rows` 的写后断言，三者必须是同一套语义。若在 Rust 侧逐字节比较
/// `source_key`，调用方拿自己数据源的另一种拼写就会被误判成「另一个数据源」。
pub(crate) async fn find_foreign_option_owner(
    options: &Repository,
    source_key: &str,
    option_ids: &[String],
) -> Result<Option<(String, String)>, BaseError> {
    if option_ids.is_empty() {
        // `where_in` 拒绝空列表；空批没有可夺取的东西。
        return Ok(None);
    }
    // **必须分片**：`where_in` 有 500 元素上限，而派生结果可以远多于 500
    // （拉取侧一条记录出一个选项）。整个列表一次性塞进去，大源每一轮都会在
    // 进事务前被 `ParamInvalid` 打回——而单元测试到不了这条路径（要数据库）。
    // 同类越界在 `find_doomed` 上已经真实发生过一次（`.page(1, 20_000)` 越了
    // `TableQuery::page` 的 100 上限），所以这里不是假想。
    for chunk in option_ids.chunks(MAX_BATCH) {
        let ids: Vec<serde_json::Value> = chunk.iter().map(|id| serde_json::json!(id)).collect();
        let foreign = options
            .query()
            .select_fields(&["option_id", "source_key"])?
            .where_in("option_id", ids)?
            .where_ne("source_key", serde_json::json!(source_key))?
            .all()
            .await?;
        if let Some(record) = foreign.first() {
            return Ok(Some((
                record.require::<String>("option_id")?,
                record.require::<String>("source_key")?,
            )));
        }
    }
    Ok(None)
}

/// 本批 ODKU 的赋值列 = 第一行的键集 − 身份列。
///
/// **身份列绝不可进赋值列**：`option_id` 是冲突键、`source_key` 是归属；ODKU 命中即改
/// 且没有 WHERE，把 `source_key` 放进去就等于把跨源夺取从「撞唯一索引响亮失败」
/// 降级成「静默改写归属」。返回 `Vec<String>`（确定性顺序，SQL 文本可断言）。
///
/// 取第一行的键集：批量写入要求全批同构列集（异构会被 yang-db fail-closed 拒掉），
/// 所以第一行就是全批。
fn upsert_update_columns(row: &Record) -> Vec<String> {
    row.as_map()
        .keys()
        .filter(|name| !matches!(name.as_str(), "option_id" | "source_key"))
        .cloned()
        .collect()
}

/// 在调用方事务内写入整批选项（一条多行 ODKU）。
///
/// `rows` 按值收：批可以到 15 万行，再克隆一份只为传参不值当（写路径内部仍会再序列化
/// 一份，那是 yang-db 的既有代价）。
///
/// 返回 `rows_affected` 之和（口径见模块文档），**不是行数**。
pub(crate) async fn apply_option_rows(
    options: &Repository,
    transaction: &mut Transaction,
    source_key: &str,
    rows: Vec<Record>,
) -> Result<u64, BaseError> {
    // 空批没有第一行可推赋值列，也没有可写的东西。
    if rows.is_empty() {
        return Ok(0);
    }
    let update_columns = upsert_update_columns(&rows[0]);
    // 先在移动 `rows` 之前把 id 抄出来：写后断言要用。
    let ids: Vec<String> = rows
        .iter()
        .map(|row| row.require::<String>("option_id"))
        .collect::<Result<_, _>>()?;
    let affected = options
        .query()
        .upsert_batch_in_tx(
            transaction,
            rows,
            &update_columns
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
        .await?;

    // 写后归属再断言：预检（事务外）与本次写入之间仍有 TOCTOU 窗口，而 ODKU 恰好命中别人
    // 的行时**不会报错**——它只是把那行的值改了。这里读的是已提交状态（`find_foreign_option_owner`
    // 走池连接，本事务未提交的写入对它不可见）：看见别人持有本批某个 id，就说明这一轮真的
    // 命中了别人的行，返回错误让整批回滚。
    //
    // 两个「必须」：**必须在提交前**（提交之后再报错就只剩脏数据），**必须走另一条池连接**
    // ——`options.query()` 绑的是池、不是本事务。前提：连接池 ≥2（生产 20 / 集成测试 16，
    // 已核实），池收到 1 会在这里自锁。
    if let Some((option_id, owner)) = find_foreign_option_owner(options, source_key, &ids).await? {
        return Err(BaseError::ConfigError(format!(
            "选项 id {option_id} 已属于数据源 {owner}，拒绝改写其归属"
        )));
    }
    Ok(affected)
}

/// 停用一批选项（补集停用的落点）。
///
/// **只在整轮快照被证明完整时才可调用**——半份快照上的补集会把仍然有效的选项
/// 批量停用。空集合显式短路：`where_in` 拒绝空列表，且「没有补集」本就无事可做。
///
/// 分片是必需的：`where_in` 有 500 元素上限，而活跃选项可以远多于 500。
/// 撞上上限的表现是整轮在进事务前被 `ParamInvalid` 打回、快照永远写不进去。
pub(crate) async fn disable_option_rows(
    options: &Repository,
    transaction: &mut Transaction,
    source_key: &str,
    doomed: &[String],
) -> Result<u64, BaseError> {
    if doomed.is_empty() {
        return Ok(0);
    }
    let mut disabled = 0u64;
    for chunk in doomed.chunks(MAX_BATCH) {
        let ids: Vec<serde_json::Value> = chunk.iter().map(|id| serde_json::json!(id)).collect();
        let mut record = Record::new();
        record.insert("enabled", serde_json::json!(false));
        disabled += options
            .query()
            .where_eq("source_key", serde_json::json!(source_key))?
            .where_in("option_id", ids)?
            .update_in_tx(transaction, record)
            .await?;
    }
    Ok(disabled)
}

/// 按 `source_key` 统计行数。
///
/// 用于「空快照歧义」的守卫：多维表格开启高级权限而调用身份不在授权群内时，官方明示
/// 可能出现**调用成功但返回空**。此时快照是 `0` 行，若照常执行补集停用，会把该数据源
/// 100% 已启用的选项静默停掉。调用方拿这个数与「本轮拉到 0 行」交叉判断。
pub(crate) async fn count_option_rows(
    options: &Repository,
    source_key: &str,
) -> Result<u64, BaseError> {
    options
        .query()
        .select_fields(&["option_id"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .count()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_limit_matches_the_where_in_cap() {
        // 跨源预检与补集停用都用 where_in 一次性传整批（写入分片已归 yang-db）；
        // 超过 500 会在进事务前被打回。这个常量必须与 yang-base 的 MAX_IN_LIST_SIZE 一致。
        assert_eq!(MAX_BATCH, 500);
    }

    #[test]
    fn update_columns_exclude_the_identity_columns() {
        // 身份列一旦进了赋值列，跨源命中就会**静默改写归属**（ODKU 没有 WHERE）——
        // 这条断言就是那个失效形态的机器守卫。
        let mut row = Record::new();
        row.insert("option_id", serde_json::json!("xlsx_1"));
        row.insert("source_key", serde_json::json!("bank_branch"));
        row.insert("label", serde_json::json!("成都"));
        row.insert("extra", serde_json::Value::Null);

        let columns = upsert_update_columns(&row);
        assert!(
            !columns.iter().any(|name| name == "option_id"),
            "option_id 是冲突键，不能出现在赋值列里：{columns:?}"
        );
        assert!(
            !columns.iter().any(|name| name == "source_key"),
            "source_key 是归属，进赋值列即静默改写归属：{columns:?}"
        );
        assert!(columns.iter().any(|name| name == "label"), "{columns:?}");
        assert!(columns.iter().any(|name| name == "extra"), "{columns:?}");
    }
}
