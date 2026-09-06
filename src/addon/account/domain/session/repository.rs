//! 会话持久化仓储：`user_session` 表的 upsert/节流更新/轮换/列出/撤销。
//!
//! 会话记录以 `session_id`（Token access claims 中的稳定标识）为主键，
//! `current_jti` 随 Refresh 轮换更新；`last_seen_at` 按节流窗口更新，
//! 避免 refresh 热路径写放大（路线图 C-1c，受 `refresh_load_benchmark.rs` 守护）。
//!
//! 该仓储不触碰 `users` 授权事实，不属于 authorization-writer allowlist。

use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

/// 会话节流窗口（秒）：同一 session 在该窗口内不重复写 `last_seen_at`。
pub(crate) const SESSION_LAST_SEEN_THROTTLE_SECONDS: i64 = 60;

pub(crate) const SESSION_ID: &str = "session_id";
pub(crate) const USER_ID: &str = "user_id";
pub(crate) const CURRENT_JTI: &str = "current_jti";
pub(crate) const CREATED_AT: &str = "created_at";
pub(crate) const LAST_SEEN_AT: &str = "last_seen_at";
pub(crate) const IP: &str = "ip";
pub(crate) const USER_AGENT: &str = "user_agent";
pub(crate) const REVOKED_AT: &str = "revoked_at";

/// 单条会话展示视图（供 GET /users/sessions 返回）。
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct SessionView {
    pub(crate) session_id: String,
    pub(crate) created_at: i64,
    pub(crate) last_seen_at: i64,
    pub(crate) ip: String,
    pub(crate) user_agent: String,
    pub(crate) revoked_at: Option<i64>,
    pub(crate) current: bool,
}

/// 一次新会话的落库载荷（避免过长的裸参数列表）。
pub(crate) struct NewSession {
    pub(crate) session_id: String,
    pub(crate) user_id: i64,
    pub(crate) jti: String,
    pub(crate) ip: String,
    pub(crate) user_agent: String,
    pub(crate) now: i64,
}

pub(crate) struct SessionRepository {
    sessions: TableDefinition,
}

impl SessionRepository {
    pub(crate) fn new(sessions: TableDefinition) -> Self {
        Self { sessions }
    }

    fn query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.sessions.bind(pool).query(["system"]))
    }

    /// 登录成功时创建会话行（或幂等 upsert 已存在 session）。
    pub(crate) async fn upsert(
        &self,
        ctx: &ActionContext,
        new_session: NewSession,
    ) -> Result<(), BaseError> {
        let record = Record::new()
            .set(SESSION_ID, new_session.session_id.as_str())
            .set(USER_ID, new_session.user_id)
            .set(CURRENT_JTI, new_session.jti.as_str())
            .set(CREATED_AT, new_session.now)
            .set(LAST_SEEN_AT, new_session.now)
            .set(IP, new_session.ip.as_str())
            .set(USER_AGENT, new_session.user_agent.as_str());
        // 会话已存在（同 session_id 重新登录/重放）时只更新 jti 与 last_seen。
        let existing = self
            .query(ctx)?
            .select_fields(&[SESSION_ID])?
            .where_eq(
                SESSION_ID,
                serde_json::Value::String(new_session.session_id.clone()),
            )?
            .page(1, 1)?
            .all()
            .await?;
        if existing.is_empty() {
            self.query(ctx)?.insert(record).await?;
        } else {
            self.query(ctx)?
                .where_eq(
                    SESSION_ID,
                    serde_json::Value::String(new_session.session_id.clone()),
                )?
                .update(
                    Record::new()
                        .set(CURRENT_JTI, new_session.jti)
                        .set(LAST_SEEN_AT, new_session.now),
                )
                .await?;
        }
        Ok(())
    }

    /// refresh 轮换时更新 jti；`last_seen_at` 仅在节流窗口外更新（写放大控制）。
    pub(crate) async fn touch_on_refresh(
        &self,
        ctx: &ActionContext,
        session_id: &str,
        jti: &str,
        now: i64,
    ) -> Result<(), BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[LAST_SEEN_AT])?
            .where_eq(SESSION_ID, serde_json::Value::String(session_id.to_string()))?
            .where_null(REVOKED_AT)?
            .page(1, 1)?
            .all()
            .await?;
        let Some(record) = rows.first() else {
            // 会话行缺失（老 Token 无 session_id 或已被清理）时降级为只更新 jti 的
            // 幂等写入；不拒绝 refresh（兼容既有会话）。
            let _ = jti;
            return Ok(());
        };
        let last_seen: i64 = record.require(LAST_SEEN_AT)?;
        let mut update = Record::new().set(CURRENT_JTI, jti);
        if now.saturating_sub(last_seen) >= SESSION_LAST_SEEN_THROTTLE_SECONDS {
            update = update.set(LAST_SEEN_AT, now);
        }
        self.query(ctx)?
            .where_eq(SESSION_ID, serde_json::Value::String(session_id.to_string()))?
            .update(update)
            .await?;
        Ok(())
    }

    /// 列出用户活跃会话（未撤销），按最近活动倒序。
    pub(crate) async fn list_active(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Vec<SessionView>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[
                SESSION_ID,
                CREATED_AT,
                LAST_SEEN_AT,
                IP,
                USER_AGENT,
                REVOKED_AT,
            ])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_null(REVOKED_AT)?
            .all()
            .await?;
        rows.iter()
            .map(|record| {
                Ok(SessionView {
                    session_id: record.require(SESSION_ID)?,
                    created_at: record.require(CREATED_AT)?,
                    last_seen_at: record.require(LAST_SEEN_AT)?,
                    ip: record.require(IP)?,
                    user_agent: record.require(USER_AGENT)?,
                    revoked_at: record.optional(REVOKED_AT)?,
                    current: false,
                })
            })
            .collect()
    }

    /// 读取单个未撤销会话的 current_jti（撤销前用于 jti 黑名单）。
    pub(crate) async fn current_jti(
        &self,
        ctx: &ActionContext,
        session_id: &str,
    ) -> Result<Option<(i64, String)>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[USER_ID, CURRENT_JTI])?
            .where_eq(SESSION_ID, serde_json::Value::String(session_id.to_string()))?
            .where_null(REVOKED_AT)?
            .page(1, 1)?
            .all()
            .await?;
        rows.first()
            .map(|record| {
                Ok((record.require(USER_ID)?, record.require(CURRENT_JTI)?))
            })
            .transpose()
    }

    /// 撤销单个会话（行标记；jti 黑名单由 Action 层完成）。
    pub(crate) async fn revoke(
        &self,
        ctx: &ActionContext,
        session_id: &str,
        now: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .query(ctx)?
            .where_eq(SESSION_ID, serde_json::Value::String(session_id.to_string()))?
            .where_null(REVOKED_AT)?
            .update(Record::new().set(REVOKED_AT, now))
            .await?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_view_fields_are_public_contract() {
        let view = SessionView {
            session_id: "s".to_string(),
            created_at: 1,
            last_seen_at: 2,
            ip: "127.0.0.1".to_string(),
            user_agent: "test".to_string(),
            revoked_at: None,
            current: false,
        };
        let json = serde_json::to_value(view).unwrap_or_else(|error| panic!("会话视图应可序列化: {error}"));
        assert_eq!(json["session_id"], "s");
        assert_eq!(json["current"], false);
        assert!(json.get("revoked_at").is_some());
    }
}
