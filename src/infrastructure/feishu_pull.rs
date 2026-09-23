//! 飞书出站拉取的后台 worker：按 `feishu.pull_interval_seconds` 周期跑一轮。
//!
//! 骨架与 `super::authorization::worker` 一致：`watch` 通道 + `JoinHandle`，
//! `shutdown()` 发信号后 `await` 任务退出；`Drop` 也发一次，避免忘记关闭时任务悬挂。
//!
//! # 单实例（设计 A10）
//!
//! **不做跨实例单飞、不做快照 CAS 租约。** 部署形态是单实例，跨实例互斥的复杂度
//! 换不来任何收益；真要上多实例，先要解决的是「谁拥有这一轮」而不是加锁。
//!
//! 单实例之外仍有两道廉价的自保（都在 `pull` 模块里）：
//! - 每轮开跑前**重读数据源行**，已删除/停用则丢弃本轮；
//! - 单个源失败只记状态、不中断整轮。
//!
//! # 首次立即执行
//!
//! 启动后**立刻跑第一轮**，不等一个完整间隔——它同时兼作存量播种。否则部署完成后
//! 最长要等一个 interval 才有数据。
//!
//! # 手动触发不是第二条路径
//!
//! 控制台的「立即拉取」只往 [`FeishuPullHandle`] 的通道里投一个信号（`Some(source_key)`
//! = 只跑那一条），由本循环在同一个 `select!` 里接住，再走
//! [`run_round_and_reschedule`]——与自动轮询**完全同一条路径**。
//!
//! 这也是它**不需要互斥锁**的原因：循环是单线程的，同一时刻只可能有一轮在跑。
//! 若改成在 Action 里同步调 `pull_source`，就得自己造一把 per-source 锁，
//! 还得让 worker 也认那把锁——那条路只为一个按钮引入了新的失效面。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use yang_base::tools::Tools;
use yang_base::BaseError;

use crate::addon::feishu::domain::alert::{alert_pull_failure, FeishuAlertSenderHandle};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{HttpClientTransport, TokioSleeper};
use crate::addon::feishu::domain::pull::{record_table_failure, run_round, PullDeps, PullTable};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 单轮最多翻页数。与 `bitable::MAX_PAGES` 同源；这里显式再声明一次，
/// 是为了让「一轮最多打多少次飞书」这层意思在 worker 里也读得出来。
const MAX_PAGES_PER_ROUND: u32 = 40;

/// 自动拉取的排程出口：控制台靠它回答「下次什么时候跑」。
///
/// # 为什么排程挂在 worker 上而不是数据源行上
///
/// 排程是**全局**的——只有一个 worker、一个循环，下一轮的时间对每个数据源都相同。
/// 把它写进数据源表会造出「每行各带一个其实永远相等的时间」这种假自由度。
/// 所以控制台在**每一条**源的详情页看到的都是同一个值。
pub(crate) struct PullSchedule {
    interval_seconds: u64,
    /// 下次自动拉取的 unix 秒。`0` 表示**没有已排定的时间**——正在跑，或还没跑过第一轮。
    next_run_at: AtomicI64,
}

impl PullSchedule {
    pub(crate) fn new(interval_seconds: u64) -> Self {
        Self {
            interval_seconds,
            next_run_at: AtomicI64::new(0),
        }
    }

    pub(crate) fn interval_seconds(&self) -> u64 {
        self.interval_seconds
    }

    /// 下次自动拉取的 unix 秒；`None` = 答不出来（正在跑 / 还没跑过）。
    pub(crate) fn next_run_at(&self) -> Option<i64> {
        match self.next_run_at.load(Ordering::Relaxed) {
            0 => None,
            value => Some(value),
        }
    }

    /// 一轮**开跑**：撤掉上一条排程，让控制台改说「正在拉取」。
    fn mark_running(&self) {
        self.next_run_at.store(0, Ordering::Relaxed);
    }

    /// 一轮**跑完**：把下一次排在一个完整间隔之后。
    ///
    /// 入参是 `now` 而不是内部取时钟——这条规则因此是可以直接单测的纯计算。
    /// **手动触发也走这里**：否则手动跑完一个间隔内自动那轮又来了。
    fn schedule_next(&self, now_unix: i64) {
        let next = now_unix.saturating_add(self.interval_seconds as i64);
        self.next_run_at.store(next, Ordering::Relaxed);
    }
}

/// 手动触发与排程的共享句柄。
///
/// 注册进 `Tools` 供 Action 取用（照 `AuthorizationVersionCache` 的先例：运行期句柄
/// 走 extension 槽，不进 `FeishuContext`——后者描述的是「有哪些表与什么配置」）。
/// worker 持有另一半：触发信号的接收端。
#[derive(Clone)]
pub(crate) struct FeishuPullHandle {
    trigger: mpsc::UnboundedSender<Option<String>>,
    schedule: Arc<PullSchedule>,
}

impl FeishuPullHandle {
    /// 建一对：句柄给 Action，接收端给 worker。
    ///
    /// 无界通道：触发是**运维的低频动作**，没有一个值得让 HTTP 请求等它的背压。
    pub(crate) fn new(interval_seconds: u64) -> (Self, mpsc::UnboundedReceiver<Option<String>>) {
        let (trigger, requests) = mpsc::unbounded_channel();
        let handle = Self {
            trigger,
            schedule: Arc::new(PullSchedule::new(interval_seconds)),
        };
        (handle, requests)
    }

    /// 请求立刻跑一轮；`Some(key)` = 只跑那一条源。
    ///
    /// 接收端已消失（worker 没起或正在退出）时返回错误——静默丢弃会让控制台
    /// 一直轮询一个永远不会发生的结果。
    pub(crate) fn request_pull(&self, source_key: Option<String>) -> Result<(), BaseError> {
        self.trigger.send(source_key).map_err(|_| {
            BaseError::ConfigError("飞书出站拉取 Worker 未在运行，无法手动触发".to_string())
        })
    }

    /// 排程的只读出口，供 `pull_schedule` Action 取用。
    pub(crate) fn schedule(&self) -> &PullSchedule {
        &self.schedule
    }

    /// worker 侧要的那一份所有权。
    fn schedule_handle(&self) -> Arc<PullSchedule> {
        Arc::clone(&self.schedule)
    }
}

/// 当前 unix 秒。取不到（系统时钟早于纪元）时返回 0——`schedule_next` 会把它推成
/// 一个非零值，不会与「未排程」的哨兵值撞上。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 飞书出站拉取 worker。
///
/// 结构体与方法是 `pub(crate)`：`start` 的入参含 crate 私有的 `FeishuContext`，
/// 对外暴露一个构造不了的类型没有意义。
pub(crate) struct FeishuPullWorker {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl FeishuPullWorker {
    /// 启动 worker。
    ///
    /// 调用方必须先确认 `settings.can_pull()`——凭证还是占位值时不该起，
    /// 否则会拿占位凭证按间隔反复出网、每轮都失败并告警。
    pub(crate) fn start(
        tools: Arc<Tools>,
        settings: &FeishuSettings,
        context: Arc<FeishuContext>,
        handle: FeishuPullHandle,
        requests: mpsc::UnboundedReceiver<Option<String>>,
    ) -> anyhow::Result<Self> {
        // 这三样在启动期取一次即可，它们都是可 Clone 的句柄，且不随 Tools 关闭而失效。
        let cache = Arc::new(RedisTenantTokenCache::new(
            tools.cache()?.clone(),
            &deployment_namespace(&tools)?,
        ));
        let transport = Arc::new(HttpClientTransport::new(tools.http()?.clone()));
        let sleeper = Arc::new(TokioSleeper);
        let tokens = Arc::new(
            TenantTokenProvider::new(
                cache,
                transport,
                sleeper.clone(),
                FeishuCredentials {
                    app_id: settings.app_id.clone().unwrap_or_default(),
                    app_secret: settings.app_secret.clone().unwrap_or_default(),
                },
                &deployment_namespace(&tools)?,
            )
            .context("构建飞书 tenant_access_token provider 失败")?,
        );

        // interval 已在 `can_pull()` 里校验过范围（10..=86400），这里只做转换。
        let interval = Duration::from_secs(settings.pull_interval_seconds);
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(
            RoundRunner {
                tools,
                context,
                tokens,
                sleeper,
            },
            interval,
            handle.schedule_handle(),
            requests,
            receiver,
        ));

        tracing::info!(
            interval_seconds = settings.pull_interval_seconds,
            "飞书出站拉取 Worker 已启动"
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
        task.await.context("等待飞书出站拉取 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for FeishuPullWorker {
    fn drop(&mut self) {
        // 忘记显式 shutdown 时也要让任务停下来，避免进程退出时任务悬挂。
        let _ = self.shutdown.send(true);
    }
}

/// 部署命名空间复用授权缓存那一份：同一个部署在缓存层必须是同一个键空间，
/// 在 `[feishu]` 段再配一个迟早会漂移。
fn deployment_namespace(tools: &Tools) -> anyhow::Result<String> {
    Ok(tools
        .extension::<crate::authorization::AuthorizationVersionCache>()?
        .deployment()
        .to_string())
}

/// 跑一轮要用到的共享句柄。启动期取一次，之后每轮只是借用。
///
/// 收成一个结构体而不是一路传四个参数：`run_loop` 本来就已经贴着参数上限，
/// 再加「手动触发」的通道与排程就直接越界了——而且这四样在语义上确实是一组
/// （「怎么跑一轮」的全部依赖），拆开传只是把它们的位置关系抄了三遍。
struct RoundRunner {
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    sleeper: Arc<TokioSleeper>,
}

async fn run_loop(
    runner: RoundRunner,
    interval: Duration,
    schedule: Arc<PullSchedule>,
    mut requests: mpsc::UnboundedReceiver<Option<String>>,
    mut shutdown: watch::Receiver<bool>,
) {
    // 首次立即执行，不等一个完整间隔（兼作存量播种）。
    let mut next_run = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
            // 手动触发。`recv()` 是取消安全的，与 sleep 分支竞争不会丢信号。
            // 一次只处理一条；积压的多条会依次跑完（触发是低频运维动作，
            // 不值得为它加合并逻辑——真要连点，跑两次的结果也是幂等的）。
            request = requests.recv() => {
                // 发送端全部 drop（进程在关闭）：不再有人能触发，收工。
                let Some(only) = request else { break };
                run_round_and_reschedule(&runner, only.as_deref(), &schedule).await;
                // 手动跑完要**重置**自动排程，否则紧接着又会跑一轮。
                next_run = tokio::time::Instant::now() + interval;
            }
            _ = tokio::time::sleep_until(next_run) => {
                run_round_and_reschedule(&runner, None, &schedule).await;
                next_run = tokio::time::Instant::now() + interval;
            }
        }
    }
    tracing::info!("飞书出站拉取 Worker 已退出");
}

/// 跑一轮，然后重排下一次。
///
/// **手动触发与自动轮询共用这一个函数。** 两条路径在「跑完之后怎么排、失败了怎么办」
/// 上必须一致，否则会出现「手动跑完一个间隔内自动那轮又来」这类只在手动路径上
/// 才有的行为。
async fn run_round_and_reschedule(
    runner: &RoundRunner,
    only: Option<&str>,
    schedule: &PullSchedule,
) {
    schedule.mark_running();
    if let Err(error) = run_once(runner, only).await {
        // 整轮失败只记日志：worker 不能因一次失败就退出，
        // 否则一个瞬时故障会让同步永久停摆。
        tracing::error!(error = %error, "飞书出站拉取整轮失败");
    }
    // 失败也要排下一次——停摆比重复失败更糟。
    schedule.schedule_next(now_unix());
}

async fn run_once(runner: &RoundRunner, only: Option<&str>) -> anyhow::Result<()> {
    // 进程正在关闭时 `Tools` 已进入 Closing/Closed，这里会以可读错误返回而不是 panic。
    let database = runner.tools.mysql()?;
    let transport = HttpClientTransport::new(runner.tools.http()?.clone());

    let deps = PullDeps {
        context: &runner.context,
        database,
        transport: &transport,
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        max_pages: MAX_PAGES_PER_ROUND,
    };
    let report = run_round(&deps, only).await?;
    if report.sources > 0 {
        tracing::info!(
            sources = report.sources,
            failures = report.failures,
            "飞书出站拉取一轮结束"
        );
    }
    Ok(())
}

/// 一张表一轮失败后的收尾：连续失败自增 → 写 `last_error` → 达阈值发告警邮件。
///
/// # 三步的顺序不能换
///
/// **先落库、再告警。** 反过来会出现「邮件已经发出、计数还没落」的窗口：下一轮读到
/// 的仍是旧计数，于是同一轮失败被重复告警。告警的判据读的正是刚落库的那个值。
///
/// # 这里只写失败那一组状态
///
/// 成功清零归 `pull::record_table_success`（它在 [`pull_table`] 里收尾）。两边都写
/// 会让 `consecutive_failures` 翻倍，而阈值正是按它判的。
///
/// # 消费者
///
/// T13 把表级轮询接进 [`run_once`] 时调用本函数；本任务只落地这条路径本身——
/// worker 此刻实际跑的仍是逐源路径（`run_round`），它在 T13 一并退役。
///
/// [`pull_table`]: crate::addon::feishu::domain::pull::pull_table
#[allow(dead_code)]
async fn record_table_failure_and_alert(
    runner: &RoundRunner,
    deps: &PullDeps<'_>,
    table: &PullTable,
    error: &BaseError,
) {
    let message = error.to_string();
    let failures = match record_table_failure(deps, table.id, &message).await {
        Ok(failures) => failures,
        Err(record_error) => {
            // 记不上账就不告警：阈值判据失去了可信的输入，硬发只会误导运维。
            tracing::error!(
                error = %record_error,
                table_id = table.id,
                "记录表级同步失败状态时出错"
            );
            return;
        }
    };

    // 告警只是「失败之后的第二件事」：没有 feishu 段、或告警通道没注册（正在关停、
    // 或装配遗漏）时，上面的失败状态已经落库，跳过发信即可。
    let Some(settings) = runner.context.settings() else {
        return;
    };
    let sender = match runner.tools.extension::<FeishuAlertSenderHandle>() {
        Ok(sender) => sender,
        Err(extension_error) => {
            tracing::warn!(error = %extension_error, "飞书告警发送器不可用，本轮不发告警");
            return;
        }
    };
    alert_pull_failure(
        sender,
        &settings.alert_recipients,
        settings.alert_failure_threshold,
        &table.title,
        &message,
        failures,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_schedule_promises_no_time() {
        // 还没跑过任何一轮时**不能编一个时间出来**——控制台宁可显示「正在拉取」，
        // 也不能显示一个下一秒就被推翻的时刻。
        assert_eq!(PullSchedule::new(900).next_run_at(), None);
    }

    #[test]
    fn scheduling_pushes_the_next_run_one_interval_ahead() {
        let schedule = PullSchedule::new(900);
        schedule.schedule_next(1_700_000_000);
        assert_eq!(schedule.next_run_at(), Some(1_700_000_900));
    }

    #[test]
    fn a_running_round_reports_no_next_run() {
        // 「正在跑」与「还没排过程」对「下次几点」这个问题的答案都是「不知道」，
        // 所以两者共用 None。区别只在文案层，不需要两个状态位。
        let schedule = PullSchedule::new(900);
        schedule.schedule_next(1_700_000_000);
        schedule.mark_running();
        assert_eq!(
            schedule.next_run_at(),
            None,
            "跑的时候不能还挂着上一轮排的时间"
        );
    }

    #[test]
    fn the_interval_is_reported_verbatim() {
        assert_eq!(PullSchedule::new(1860).interval_seconds(), 1860);
    }
}
