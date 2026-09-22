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

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use yang_base::tools::Tools;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{HttpClientTransport, TokioSleeper};
use crate::addon::feishu::domain::pull::{run_round, PullDeps};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 单轮最多翻页数。与 `bitable::MAX_PAGES` 同源；这里显式再声明一次，
/// 是为了让「一轮最多打多少次飞书」这层意思在 worker 里也读得出来。
const MAX_PAGES_PER_ROUND: u32 = 40;

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
            tools, context, tokens, sleeper, interval, receiver,
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

async fn run_loop(
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    sleeper: Arc<TokioSleeper>,
    interval: Duration,
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
            _ = tokio::time::sleep_until(next_run) => {
                if let Err(error) = run_once(&tools, &context, &tokens, sleeper.as_ref()).await {
                    // 整轮失败只记日志：worker 不能因一次失败就退出，
                    // 否则一个瞬时故障会让同步永久停摆。
                    tracing::error!(error = %error, "飞书出站拉取整轮失败");
                }
                next_run = tokio::time::Instant::now() + interval;
            }
        }
    }
    tracing::info!("飞书出站拉取 Worker 已退出");
}

async fn run_once(
    tools: &Arc<Tools>,
    context: &Arc<FeishuContext>,
    tokens: &TenantTokenProvider,
    sleeper: &TokioSleeper,
) -> anyhow::Result<()> {
    // 进程正在关闭时 `Tools` 已进入 Closing/Closed，这里会以可读错误返回而不是 panic。
    let database = tools.mysql()?;
    let transport = HttpClientTransport::new(tools.http()?.clone());

    let deps = PullDeps {
        context,
        database,
        transport: &transport,
        sleeper,
        tokens,
        max_pages: MAX_PAGES_PER_ROUND,
    };
    let report = run_round(&deps).await?;
    if report.sources > 0 {
        tracing::info!(
            sources = report.sources,
            failures = report.failures,
            "飞书出站拉取一轮结束"
        );
    }
    Ok(())
}
