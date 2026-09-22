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
//! # 写语义：按 `(option_id, source_key)` 定位，0 行则插入
//!
//! 与 `upsert_options` 一致。**必须同时限定 `source_key`**：记录里带着本次调用的
//! `source_key`，只按 `option_id` 定位就会改写归属——原数据源的取选项接口随即少一条，
//! 两边都收不到任何异常信号。跨源夺取由 [`find_foreign_option_owner`] 在事务前预检。
//!
//! 两条路径在**记录内容**上不同，这也是本模块不自己构造记录的原因：
//! 入站推送是**合并**（省略的字段保持原值，所以 `enabled` 是单向的），
//! 出站拉取是**整行替换**（把本轮派生出的全部列一并写入，否则「删→停用→加回」
//! 会让 `enabled` 永久停在 `false`）。

use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

use super::repository::Repository;

/// 待写入的一行。记录由调用方按各自语义构造（合并 vs 整行替换）。
pub(crate) struct OptionWriteItem {
    pub(crate) option_id: String,
    pub(crate) record: Record,
}

/// 本批的写入结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OptionWriteOutcome {
    pub(crate) inserted: u64,
    pub(crate) updated: u64,
}

/// 单批最多写多少行。
///
/// 与 `upsert_options` 的 `MAX_BATCH` 取同一个值，且**不能超过 `where_in` 的
/// 500 元素上限**（`validation.rs` 的 `MAX_IN_LIST_SIZE`）——跨源预检用 `where_in`
/// 一次性查完整批，超限会在进事务前被 `ParamInvalid` 打回。
pub(crate) const MAX_BATCH: usize = 500;

/// 跨源夺取预检：返回第一个「id 属于**别的**数据源」的 `(option_id, owner)`。
///
/// 放在事务之前：既是可归因的业务失败（不烧可用性预算），也不会让一次脏数据把
/// 整批写入带坏。批次**按 500 分片**（`where_in` 的元素上限），命中即提前返回。
///
/// **归属比较交给数据库做。** 表的排序规则是 `utf8mb4_unicode_ci`（大小写不敏感 +
/// PAD SPACE），紧随其后的 UPDATE 用的 `where_eq("source_key", …)` 正是它；若在
/// Rust 侧逐字节比较，调用方拿自己数据源的另一种拼写就会被误判成「另一个数据源」，
/// 三道判据必须用同一套语义。
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

/// 在调用方事务内写入整批选项。
///
/// 逐条「先试更新、0 行再插入」：不需要先读一次，也没有「读到不存在然后被别人插进来」
/// 的窗口。
pub(crate) async fn apply_option_rows(
    options: &Repository,
    transaction: &mut Transaction,
    source_key: &str,
    items: &[OptionWriteItem],
) -> Result<OptionWriteOutcome, BaseError> {
    let mut outcome = OptionWriteOutcome::default();
    for item in items {
        let affected = options
            .query()
            .where_eq("option_id", serde_json::json!(item.option_id))?
            // 必须同时限定 source_key，理由见模块文档。
            .where_eq("source_key", serde_json::json!(source_key))?
            .update_in_tx(transaction, item.record.clone())
            .await?;
        if affected == 0 {
            options
                .query()
                .insert_in_tx(transaction, item.record.clone())
                .await?;
            outcome.inserted += 1;
        } else {
            outcome.updated += affected;
        }
    }
    Ok(outcome)
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
        // 跨源预检与补集停用都用 where_in 一次性传整批；超过 500 会在进事务前被打回。
        // 这个常量必须与 yang-base 的 MAX_IN_LIST_SIZE 保持一致。
        assert_eq!(MAX_BATCH, 500);
    }

    #[test]
    fn outcome_defaults_to_zero() {
        let outcome = OptionWriteOutcome::default();
        assert_eq!(outcome.inserted, 0);
        assert_eq!(outcome.updated, 0);
    }
}
