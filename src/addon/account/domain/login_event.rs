//! 登录事件仓储：`login_event` 表的 append/查询/清理（路线图 C-2）。
//!
//! 登录事件不落审计库（保留期清理会牵连用户可见历史），独立成表；
//! `failure_reason` 只记粗粒度原因（invalid_password/user_not_found/disabled/
//! rate_limited），不记录明文凭据。

use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

pub(crate) const LOGIN_EVENT_USER_ID: &str = "user_id";
pub(crate) const LOGIN_EVENT_OCCURRED_AT: &str = "occurred_at";
pub(crate) const LOGIN_EVENT_IP: &str = "ip";
pub(crate) const LOGIN_EVENT_USER_AGENT: &str = "user_agent";
pub(crate) const LOGIN_EVENT_RESULT: &str = "result";
pub(crate) const LOGIN_EVENT_FAILURE_REASON: &str = "failure_reason";

/// 用户可见的安全事件视图。
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct LoginEventView {
    pub(crate) occurred_at: i64,
    pub(crate) ip: String,
    pub(crate) user_agent: String,
    pub(crate) result: String,
    pub(crate) failure_reason: Option<String>,
}

/// 一条登录事件载荷（避免过长的裸参数列表）。
pub(crate) struct NewLoginEvent {
    pub(crate) user_id: Option<i64>,
    pub(crate) occurred_at: i64,
    pub(crate) ip: String,
    pub(crate) user_agent: String,
    pub(crate) result: &'static str,
    pub(crate) failure_reason: Option<&'static str>,
}

pub(crate) struct LoginEventRepository {
    events: TableDefinition,
}

impl LoginEventRepository {
    pub(crate) fn new(events: TableDefinition) -> Self {
        Self { events }
    }

    fn query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.events.bind(pool).query(["system"]))
    }

    /// 追加一条登录事件（成功或失败）。失败原因只记粗粒度，best-effort 失败不阻塞登录。
    pub(crate) async fn append(
        &self,
        ctx: &ActionContext,
        event: NewLoginEvent,
    ) -> Result<(), BaseError> {
        let mut record = Record::new()
            .set(LOGIN_EVENT_USER_ID, event.user_id.unwrap_or(0))
            .set(LOGIN_EVENT_OCCURRED_AT, event.occurred_at)
            .set(LOGIN_EVENT_IP, event.ip)
            .set(LOGIN_EVENT_USER_AGENT, event.user_agent)
            .set(LOGIN_EVENT_RESULT, event.result);
        if let Some(reason) = event.failure_reason {
            record = record.set(LOGIN_EVENT_FAILURE_REASON, reason);
        }
        self.query(ctx)?.insert(record).await?;
        Ok(())
    }

    /// 按用户查询安全事件，按时间倒序分页。
    pub(crate) async fn list_for_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<LoginEventView>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[
                LOGIN_EVENT_OCCURRED_AT,
                LOGIN_EVENT_IP,
                LOGIN_EVENT_USER_AGENT,
                LOGIN_EVENT_RESULT,
                LOGIN_EVENT_FAILURE_REASON,
            ])?
            .where_eq(
                LOGIN_EVENT_USER_ID,
                serde_json::Value::Number(user_id.into()),
            )?
            .page(page, page_size)?
            .all()
            .await?;
        rows.iter()
            .map(|record| {
                Ok(LoginEventView {
                    occurred_at: record.require(LOGIN_EVENT_OCCURRED_AT)?,
                    ip: record.require(LOGIN_EVENT_IP)?,
                    user_agent: record.require(LOGIN_EVENT_USER_AGENT)?,
                    result: record.require(LOGIN_EVENT_RESULT)?,
                    failure_reason: record.optional(LOGIN_EVENT_FAILURE_REASON)?,
                })
            })
            .collect()
    }
}
