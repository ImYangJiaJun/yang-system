//! 飞书用户身份授权的到期提醒：授权满 350 天后**每天**给系统管理员发一封信，
//! 直到重新授权。
//!
//! # 为什么是 350 天
//!
//! 飞书的硬性红线是 365 天：用户授权满 365 天后刷新 `user_access_token` 报
//! `20037`，唯一补救是让管理员重新走一次授权页。提前 15 天开始提醒，留出反应时间
//! （判据是 `user_token::reauth_reminder_due`，与状态页用的是同一个函数）。
//!
//! # 为什么每天都要刷一次凭证
//!
//! 这一轮同时干两件事，顺序不能反：
//!
//! 1. **保活**：`refresh_token` 是一次性的、寿命短（官方文档示例 7 天）。只做
//!    「派发时用到才刷」的话，静默期一长整条链就断了——而断链只能靠重新授权补救。
//!    每天刷一次让凭证链永远年轻（`access_token` 只有 2 小时寿命，所以这一刷
//!    每天都会真的打一次飞书，不是空转）。
//! 2. **提醒**：过线才发，且**每轮都发**（与 `alert.rs` 的「不冷却」同一条哲学：
//!    没收到邮件会被读成「已经好了」）。
//!
//! # 收件人怎么取
//!
//! 「系统第一个账号的邮箱」走**表 DSL**（`users` 表的 `user_table_spec`）而不是原生
//! SQL：`users.email` 标了 `readable_by([SYSTEM_ROLE])`，而 `Repository` 正是以该
//! 受信角色查询的。原生 SQL 在这个仓里是要在 `docs/architecture/raw-sql-boundaries.md`
//! 登记的逃生口，读一张有表声明的表够不上那个门槛。
//!
//! # 单实例
//!
//! 与另外三个 worker 同一条假设（`feishu_pull` 的 A10）：不做跨实例单飞。
//! 重复发送的代价是管理员多收一封信，而重复刷新的代价由 `user_token` 的进程内锁
//! 挡住（`refresh_token` 一次性，并发刷新会互相踩）。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use yang_base::tools::Tools;
use yang_base::BaseError;

use crate::addon::feishu::domain::alert::FeishuAlertSenderHandle;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{HttpClientTransport, TokioSleeper};
use crate::addon::feishu::domain::repository::{Repository, PAGE_SIZE};
use crate::addon::feishu::domain::tenant_token::FeishuCredentials;
use crate::addon::feishu::domain::user_token;
use crate::config::FeishuSettings;
use crate::infrastructure::audit;

/// 提醒与保活的间隔：一天一次。
const REMINDER_INTERVAL_SECONDS: u64 = 86_400;

/// 一天的秒数。
const SECONDS_PER_DAY: i64 = 86_400;

/// 飞书用户授权到期提醒 worker。
pub(crate) struct FeishuOauthReminderWorker {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl FeishuOauthReminderWorker {
    /// 启动 worker。
    ///
    /// 调用方先确认 `settings.can_pull()`——凭证是占位值时连保活都做不了，
    /// 与另外三个 worker 同一条准入。
    pub(crate) fn start(
        tools: Arc<Tools>,
        settings: &FeishuSettings,
        context: Arc<FeishuContext>,
    ) -> anyhow::Result<Self> {
        let transport = Arc::new(HttpClientTransport::new(tools.http()?.clone()));
        let sleeper = Arc::new(TokioSleeper);
        let credentials = FeishuCredentials {
            app_id: settings.app_id.clone().unwrap_or_default(),
            app_secret: settings.app_secret.clone().unwrap_or_default(),
        };
        let users = Repository::new(
            crate::addon::account::user_table_spec()?.table_definition()?,
            Arc::new(tools.mysql()?.pool().clone()),
        );
        let (shutdown, receiver) = watch::channel(false);
        let runner = ReminderRunner {
            tools,
            context,
            transport,
            sleeper,
            credentials,
            users,
        };
        let task = tokio::spawn(run_loop(
            runner,
            Duration::from_secs(REMINDER_INTERVAL_SECONDS),
            receiver,
        ));

        tracing::info!(
            interval_seconds = REMINDER_INTERVAL_SECONDS,
            "飞书用户授权提醒 Worker 已启动"
        );
        Ok(Self {
            shutdown,
            task: Some(task),
        })
    }

    /// 发信号并等待任务退出。
    pub(crate) async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await
            .map_err(|error| anyhow::anyhow!("等待飞书用户授权提醒 Worker 退出失败: {error}"))?;
        Ok(())
    }
}

impl Drop for FeishuOauthReminderWorker {
    fn drop(&mut self) {
        // 忘记显式 shutdown 时也要让任务停下来，避免进程退出时任务悬挂。
        let _ = self.shutdown.send(true);
    }
}

/// 一轮要用到的共享句柄（与 `feishu_pull` 的 `RoundRunner` 同一形态）。
struct ReminderRunner {
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    transport: Arc<HttpClientTransport>,
    sleeper: Arc<TokioSleeper>,
    credentials: FeishuCredentials,
    /// `users` 表（账号 addon 的表），只用来取提醒收件人。
    users: Repository,
}

async fn run_loop(runner: ReminderRunner, interval: Duration, mut shutdown: watch::Receiver<bool>) {
    // 首次立即执行（与另外三个 worker 一致）：部署当天就该把凭证保活一次，
    // 否则「进程重启比间隔还频繁」的部署里保活永远轮不到——那会让凭证链静默断掉。
    // 代价是部署当天可能多发一封提醒，可接受（提醒本身就是每天一封）。
    let mut next_run = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(next_run) => {
                run_once(&runner).await;
                next_run = tokio::time::Instant::now() + interval;
            }
        }
    }
    tracing::info!("飞书用户授权提醒 Worker 已退出");
}

/// 跑一轮：保活 + 到期提醒。
async fn run_once(runner: &ReminderRunner) {
    let now = user_token::now_unix_secs();
    let row = match user_token::load_single(runner.context.user_tokens()).await {
        Ok(Some(row)) => row,
        // 从未授权：没有凭证可保活，也没有到期可言。
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(error = %error, "读取飞书用户身份失败，本轮跳过");
            return;
        }
    };

    // ---- 保活 ----
    //
    // 失败只记日志、不发邮件：邮件是「该重新授权了」的信号，而保活失败在 350 天
    // 之前多半是网络抖动或后台开关问题，靠派发路径的可行动文案暴露更准。
    if let Err(error) = user_token::user_access_token(
        runner.context.user_tokens(),
        runner.transport.as_ref(),
        runner.sleeper.as_ref(),
        &runner.credentials,
    )
    .await
    {
        tracing::warn!(message = %error.message(), "飞书用户凭证保活刷新失败");
    }

    // ---- 到期提醒 ----
    if !user_token::reauth_reminder_due(row.authorized_at, now) {
        return;
    }
    let days_since_authorized = (now - row.authorized_at) / SECONDS_PER_DAY;

    let recipient = match admin_recipient(&runner.users).await {
        Ok(Some(recipient)) => recipient,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(error = %error, "查询系统管理员邮箱失败，本轮不发提醒");
            return;
        }
    };
    let sender = match runner.tools.extension::<FeishuAlertSenderHandle>() {
        Ok(sender) => sender,
        Err(error) => {
            tracing::warn!(error = %error, "飞书告警发送器不可用，本轮不发提醒");
            return;
        }
    };
    let user_name = row.user_name.as_deref().unwrap_or("（未记录姓名）");
    match sender
        .send_oauth_expiry_reminder(&recipient, user_name, days_since_authorized)
        .await
    {
        Ok(()) => {
            tracing::warn!(
                recipient = %recipient,
                days_since_authorized,
                "飞书用户授权已过提醒线，已发送重新授权提醒"
            );
            audit_reminder(runner, &row.open_id, days_since_authorized, now).await;
        }
        Err(error) => {
            tracing::error!(error = %error, recipient = %recipient, "飞书用户授权提醒邮件投递失败");
        }
    }
}

/// 取「系统第一个账号」的邮箱。
///
/// 取一页（框架的页大小上限）再挑 **id 最小且有邮箱**的那一行：
///
/// - `users` 表**没有 sortable 列**（控制台的 `list_users` 同样只能分页取），所以
///   在 DSL 里表达不了「按 id 升序 LIMIT 1」，用「一页里的最小 id」代替——账号总数
///   远小于一页，实践上就是第一个账号；
/// - 「有邮箱」这一条是必须的：首账号自动引导为 system_admin，但注册不强制邮箱，
///   跳过没有邮箱的行比对着一个空值静默不发有用。
async fn admin_recipient(users: &Repository) -> Result<Option<String>, BaseError> {
    let rows = users
        .query()
        .select_fields(&["id", "email"])? // schema-anchor: cross-addon（查 account 的 users 表，非 feishu 表）
        .page(1, PAGE_SIZE)?
        .all()
        .await?;
    let mut best: Option<(i64, String)> = None;
    for row in rows {
        let id: i64 = row.require("id")?;
        let email: Option<String> = row.optional("email")?;
        let Some(email) = email.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let replace = match &best {
            Some((best_id, _)) => id < *best_id,
            None => true,
        };
        if replace {
            best = Some((id, email));
        }
    }
    match best {
        Some((_, email)) => Ok(Some(email)),
        None => {
            tracing::warn!("系统里没有带邮箱的账号，飞书用户授权提醒发不出去");
            Ok(None)
        }
    }
}

/// 记一条审计：提醒已发出（运维要能回答「到底有没有催过」）。
async fn audit_reminder(runner: &ReminderRunner, open_id: &str, days: i64, now: i64) {
    let constructed = (|| {
        audit::succeeded_system_event_without_ctx(
            "feishu-oauth-reminder",
            "feishu.oauth_reauth_reminder",
            None,
            audit::entity("feishu_user_token", open_id)?,
            audit::summary([
                ("days_since_authorized", serde_json::json!(days)),
                ("reminded_at", serde_json::json!(now)),
            ])?,
        )
    })();
    let event = match constructed {
        Ok(event) => event,
        Err(error) => {
            tracing::error!(error = %error, "飞书授权提醒审计事件构造失败");
            return;
        }
    };
    match runner.tools.mysql() {
        Ok(database) => {
            if let Err(error) = audit::append_independent(database.pool(), &event).await {
                tracing::error!(error = %error, "飞书授权提醒审计写入失败");
            }
        }
        Err(error) => tracing::error!(error = %error, "飞书授权提醒审计取库失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interval_is_one_day() {
        assert_eq!(REMINDER_INTERVAL_SECONDS as i64, SECONDS_PER_DAY);
    }
}
