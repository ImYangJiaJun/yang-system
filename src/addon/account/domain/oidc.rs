//! 外部身份提供方（IdP）端口抽象（路线图 E-3，降级项）。
#![allow(dead_code)] // 端口为未来 SSO 需求预留：无消费者时不触发 dead-code 门禁
//!
//! 只定义端口形态，**不建表、不接 Client 实现**——等第一个真实 SSO 需求出现
//! 再声明 `user_identity` 表（provider × external_sub × user_id）并接 IdP 客户端。
//! 无消费者先建表不符合声明式 Schema 的演进纪律。
//!
//! 本模块当前只提供 trait 定义与文档锚点；任何实现方必须保证：
//! - `external_sub` 是 IdP 侧稳定标识（不随邮箱/用户名变化）；
//! - 同一 provider 下 `external_sub` 唯一映射到一个本系统用户；
//! - 关联读写必须走事务内端口（与 authz_version writer 同事务）。

use async_trait::async_trait;
use yang_base::BaseError;

/// 外部身份提供方标识（如 `google`、`microsoft`、`okta`）。
pub type ProviderId = String;

/// 外部主体在 IdP 侧的稳定标识。
pub type ExternalSubject = String;

/// 外部身份 ↔ 本系统用户关联的读取端口。
///
/// 等第一个真实 SSO 需求出现时，实现方应落在 `user_identity` 表
/// （`provider` × `external_sub` 唯一，指向 `users.id`），并在登录/换绑
/// 用例中按此端口查找本系统用户。
#[async_trait]
pub trait ExternalIdentityProvider: Send + Sync + 'static {
    /// 返回本实现支持的 IdP 标识。
    fn provider(&self) -> ProviderId;

    /// 按外部主体查找已关联的本系统用户；未关联返回 `None`。
    async fn find_local_user(
        &self,
        pool: &sqlx::MySqlPool,
        external_sub: &ExternalSubject,
    ) -> Result<Option<i64>, BaseError>;

    /// 建立外部身份与本系统用户的关联（幂等；冲突返回明确错误）。
    async fn link_local_user(
        &self,
        pool: &sqlx::MySqlPool,
        external_sub: &ExternalSubject,
        user_id: i64,
    ) -> Result<(), BaseError>;
}
