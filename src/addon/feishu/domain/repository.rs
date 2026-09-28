//! 两张表的唯一持久化边界。
//!
//! 所有读写都经这里，且统一以 [`SYSTEM_ROLE`] 操作。外部 Token 调用没有登录身份，
//! 但表查询本身只要求角色满足字段 Audience（默认 `Everyone`），因此不需要伪造用户；
//! 用受信角色是为了让 `secret` 字段（`token_hash`）对 writer 可用。
//!
//! 本模块刻意只提供**构造与查询入口**，不包装具体读写动作——那些动作由各自的
//! Action 组合，避免这里长成一个什么都做的上帝对象。

use std::sync::Arc;

use sqlx::MySqlPool;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

/// 受信服务角色；字段权限判定的依据。
pub(crate) const SYSTEM_ROLE: &str = "system";

/// 一张表在服务端的读写入口。
#[derive(Clone)]
pub(crate) struct Repository {
    pool: Arc<MySqlPool>,
    definition: TableDefinition,
}

impl Repository {
    /// 绑定表定义与连接池。
    pub(crate) fn new(definition: TableDefinition, pool: Arc<MySqlPool>) -> Self {
        Self { definition, pool }
    }

    /// 以受信角色开启一次查询。
    pub(crate) fn query(&self) -> TableQuery {
        self.definition
            .bind(Arc::clone(&self.pool))
            .query([SYSTEM_ROLE])
    }
}

/// 单页行数：取框架的硬上限**本身**，不写字面量。
///
/// `TableQuery::page` 对超过上限的页大小是**拒绝**（`ParamInvalid`）而不是截断——
/// 写大了不会「只取前 100 条」，而是整条查询在运行期直接失败。这个坑本仓栽过两次：
/// `option_write.rs` 的注释记着 `find_doomed` 的 `.page(1, 20_000)`（110 条单测全绿、
/// 端点全挂），`feishu_approval_worker` 的播种又用 `.page(1, 200)` 与
/// `.page(1, 2000)` 栽了第二次——**播种整轮报错、队列永远建不起来**。
///
/// 用框架常量而不是 `100` 这个字面量，「漂移」在类型层面就不可能发生。
pub(crate) const PAGE_SIZE: usize = yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE;

/// 逐页读全符合条件的行，直到读完；页数超过 `max_pages` 即报错。
///
/// # 为什么要自己翻页
///
/// 上限是硬拒绝，而这几处的行数与业务规模同量级——一张表的任务行数 ≈ 多维表格的记录数，
/// 选项表可达数百上千。「一次读够」既不被允许，也不该指望。
///
/// # 为什么超页数要**报错**而不是截断返回
///
/// 截断是静默的：调用方拿到一批「看起来完整」的行，据此建索引，没读到的行于是被当成
/// 「不存在」——症状是重复插入或漏处理，而日志里一个字都没有。到上限就报错，
/// 让「筛选条件没生效 / 数据量超预期」以显式失败出现。
///
/// # 收口判据
///
/// 不满一页就是最后一页；总数恰是整页倍数时，下一轮拿到 0 条，同样落在这里。
/// 不用 `total`：`TableQuery::select`（唯一带 `total` 的入口）是 yang-base 的
/// `pub(crate)`，仓外只拿得到 `all()`。
pub(crate) async fn all_pages(
    base: TableQuery,
    max_pages: usize,
) -> Result<Vec<Record>, BaseError> {
    let mut rows: Vec<Record> = Vec::new();
    for page in 1..=max_pages {
        let batch = base.clone().page(page, PAGE_SIZE)?.all().await?;
        let got = batch.len();
        rows.extend(batch);
        if got < PAGE_SIZE {
            return Ok(rows);
        }
    }
    Err(BaseError::ParamInvalid(
        "page".to_string(),
        format!(
            "翻页超过上限 {max_pages} 页（已读 {} 行）——筛选条件可能没生效",
            rows.len()
        ),
    ))
}
