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
use yang_base::table::{TableDefinition, TableQuery};

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
