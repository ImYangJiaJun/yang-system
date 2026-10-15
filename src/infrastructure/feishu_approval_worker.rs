//! 审批派发的后台 worker：认领待处理记录，限速创建实例，回填编号。
//!
//! 骨架与 `feishu_pull` 一致：`watch` 通道 + `JoinHandle`，`shutdown()` 发信号后
//! `await` 任务退出；`Drop` 也发一次，避免忘记关闭时任务悬挂。
//!
//! # 部署形态是单实例
//!
//! 与 `feishu_pull` 同一条决策（那里的 A10）：`deploy/deploy-blue-green.sh` 的
//! `cutover` 先 `stop_pair` 停线上再起新的，两实例不重叠。所以**不做跨实例单飞、
//! 不做表级互斥锁**；循环是单线程的，同一时刻只可能有一轮在跑。
//!
//! # 认领而不是「扫表决定要不要处理」
//!
//! 「是否已处理」的判据落在 `feishu_approval_task`，不落在多维表格的字段上。
//! 使用方选择不加「可提交」闸门字段；若以「回填字段为空」作判据，有两个静默故障：
//! 用户填一半的行被写成终态错误后永久不再处理；回写失败的记录每轮被重新捞出、
//! 反复撞 `60012`。
//!
//! # 认领走 DSL，不碰裸 SQL
//!
//! `FOR UPDATE SKIP LOCKED` 只能用原始 SQL，而原始 SQL 要求 `raw-sql-boundary`
//! 门禁登记（见 `docs/architecture/raw-sql-boundaries.md`）。本 worker **不需要它**：
//! 部署是单实例、循环是单线程，同一时刻只可能有一轮在跑，拿行锁与租约都没有收益。
//! 用 DSL 认领因此零门禁成本，也复用了 fail-closed 的字段权限校验。
//!
//! # 崩溃恢复
//!
//! 蓝绿 cutover 用 `docker stop`（SIGTERM → 10 秒后 SIGKILL）会静默杀掉处理中的
//! 批次。本 worker 靠两件事恢复，不需要租约：
//!
//! 1. **`claim` 只认领 `pending`**（单实例 + 单线程 ⇒ 此时没有别人在处理 `creating`）；
//! 2. **状态推进只发生在处理结束时**（`record_outcome`），所以崩溃最多让一批任务
//!    停在 `creating`，而 `feishu_approval_task` 从不因「还停着」而阻塞下一轮
//!    ——`claim` 的条件里没有它。
//!
//! `uuid` 是最后一道防线：即使某条记录被重复处理，飞书也只会回 `60012`，
//! 走回捞路径拿到同一个实例，不会重复建单。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;
use yang_base::table::{Record, SortOrder};
use yang_base::tools::Tools;
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::instance_applink;
use crate::addon::feishu::domain::approval_convert::{FixedOffset, WidgetMap};
use crate::addon::feishu::domain::approval_dispatch::{
    applicant_open_id as extract_applicant_open_id, backfill_in_chunks, dispatch_one,
    widget_maps_from_rows, BitableBackfill, DispatchInput as OrchestrationInput, DispatchResult,
};
use crate::addon::feishu::domain::approval_feedback;
use crate::addon::feishu::domain::approval_notification::{
    FeedbackReference, NotificationRecipient, ReceiveIdType,
};
use crate::addon::feishu::domain::approval_rate_limit::acquire_create_slot;
use crate::addon::feishu::domain::approval_uuid::derive_uuid;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    HttpClientTransport, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::repository::all_pages;
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;
use crate::infrastructure::audit;

/// 一次认领最多取多少条。
///
/// 与回填子批同量级：认领一批、处理一批、回填一批，批内失败不影响其他批。
/// 取大了会让单轮耗时过长（创建受速率限制），也拉长租约。
/// 播种时最多翻多少页（每页 500 条）。
///
/// 防失控而不是分页参数：筛选条件没生效时一页页拉全表会把这一轮无限拖长，
/// 而 worker 是单线程的——它卡住等于整个队列停摆。到上限即失败，
/// 让「条件没生效」以显式报错出现。
const SEED_MAX_PAGES: u32 = 20;

/// 播种时最多翻多少页已有任务行（每页 `PAGE_SIZE` 行，用来判「新建还是拉回待处理」）。
///
/// 行数与多维表格的记录数同量级，所以必须真翻页，不能「一次读够」。这里只是防失控的
/// 上界；超了 `all_pages` 会**报错**而不是截断——截断会让没读到的行被当成「不存在」，
/// 于是重复插入，而重复插入撞的是唯一索引，报错点离真正的原因很远。
const SEED_MAX_TASK_PAGES: usize = 20;

/// 播种时最多翻多少页启用中的配置行。配置是「一张多维表格一条」，量级个位到十位。
const SEED_MAX_CONFIG_PAGES: usize = 10;

/// 认领一批处理多少条。
const CLAIM_BATCH: usize = 50;

/// 编译期守护：`CLAIM_BATCH` 是**手工挑**的数，一旦被调到框架上限之上，`claim`
/// 会在运行期直接失败——不是少取几条。上限是 fail-closed 的拒绝语义，而运行期只留
/// 一行 WARN，很容易被当成噪音（播种就是这么挂掉的）。
const _: () = assert!(CLAIM_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 单轮最坏耗时上界（秒）。
///
/// 50 条 × 每条在限速下的间隔，加上取详情与退避的余量。
///
/// **不再写进 `lease_until`**：单实例方案下没有租约判据（见 `claim` 的说明）。
/// 这个常量现在只作为「一轮该在多长时间内跑完」的**度量基准**，
/// `tests` 里有断言钉住它——超了说明限速或批大小配置有误。
const ROUND_WORST_CASE_SECONDS: i64 = 300;

/// 上一轮处理到的记录游标。
///
/// # 为什么需要它
///
/// `claim` 一次取 `CLAIM_BATCH` 条，如果这一批**全部**是等待态或可重试（例如
/// 「申请人还没填完」是常见情形），本轮结束时它们仍是 `pending`；下一轮再按
/// `id` 升序取同一批，会**原地空转**，永远推进不到后面的记录。
///
/// 游标放在**进程内存**而不是库里：单实例 + 单进程的前提下它就是「这一轮跑到
/// 哪了」，重启后从 0 开始重扫一遍是可接受的（而且那恰好也是恢复该恢复的）。
/// 放库里反而要为它建一张单行表，收益是「重启后不重扫」——那是负收益。
#[derive(Debug, Default, Clone, Copy)]
struct ResumeCursor(i64);

impl ResumeCursor {
    /// 一批里最后一个已处理记录的 id；扫描结束后从头重试 pending 任务。
    fn advance(&mut self, rows: &[ClaimedTask]) {
        if let Some(last) = rows.last() {
            self.0 = self.0.max(last.id);
        } else {
            self.0 = 0;
        }
    }
}

/// 手动触发的共享句柄。
///
/// 照 `FeishuPullHandle` 的先例：注册进 `Tools` 供 Action 取用。
///
/// `pub` 而不是 `pub(crate)`：集成测试（独立 crate）要注入这个句柄才能端到端走
/// dispatch 的「批量受理」出口（`tests/feishu_approval_dispatch_integration.rs`）；
/// 生产装配仍只有 `bootstrap.rs` 一处调用 [`ApprovalDispatchHandle::new`]。
#[derive(Clone)]
pub struct ApprovalDispatchHandle {
    trigger: mpsc::UnboundedSender<(i64, Option<String>)>,
}

impl ApprovalDispatchHandle {
    /// 建一对：句柄给 Action，接收端给 worker。
    ///
    /// 无界通道：触发是低频动作，没有一个值得让 HTTP 请求等它的背压。
    pub fn new() -> (Self, mpsc::UnboundedReceiver<(i64, Option<String>)>) {
        let (trigger, requests) = mpsc::unbounded_channel();
        (Self { trigger }, requests)
    }

    /// 请求立刻跑一轮全表，并把该配置的受理发起人（已解析成 open_id）先落库。
    ///
    /// `config_id` 标识本次受理命中的配置；`applicant` 为 `None` 时语义与旧行为
    /// 完全一致（只跑一轮，不落发起人）。
    ///
    /// 接收端已消失（worker 没起或正在退出）时返回错误——静默丢弃会让按钮
    /// 显示「已受理」而实际什么都没发生。
    pub(crate) fn request_dispatch(
        &self,
        config_id: i64,
        applicant: Option<String>,
    ) -> Result<(), BaseError> {
        self.trigger.send((config_id, applicant)).map_err(|_| {
            BaseError::ConfigError("审批派发 Worker 未在运行，无法受理全表派发".to_string())
        })
    }
}

/// 审批派发 worker。
pub(crate) struct ApprovalDispatchWorker {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ApprovalDispatchWorker {
    /// 启动 worker。
    ///
    /// 调用方必须先确认 `settings.can_pull()`——凭证还是占位值时不该起，
    /// 否则会拿占位凭证反复出网、每轮都失败。
    pub(crate) fn start(
        tools: Arc<Tools>,
        settings: &FeishuSettings,
        context: Arc<FeishuContext>,
        requests: mpsc::UnboundedReceiver<(i64, Option<String>)>,
    ) -> anyhow::Result<Self> {
        let deployment = deployment_namespace(&tools)?;
        let cache = Arc::new(RedisTenantTokenCache::new(
            tools.cache()?.clone(),
            &deployment,
        ));
        let transport = Arc::new(HttpClientTransport::new(tools.http()?.clone()));
        let sleeper = Arc::new(TokioSleeper);
        let tokens = Arc::new(
            TenantTokenProvider::new(
                cache,
                // provider 要一份**独占的 Arc**（它的字段是 `Arc<dyn …>`），
                // 而 RoundRunner 也要自己持一份。这里 clone 一份交给 provider——
                // 而不是让 provider 独占，那会让 runner 拿不到 transport。
                Arc::clone(&transport) as Arc<dyn OutboundTransport>,
                sleeper.clone(),
                FeishuCredentials {
                    app_id: settings.app_id.clone().unwrap_or_default(),
                    app_secret: settings.app_secret.clone().unwrap_or_default(),
                },
                &deployment,
            )
            .context("构建飞书 tenant_access_token provider 失败")?,
        );

        // 时区失败就让**启动**失败，而不是每轮静默跳过——
        // 「点了按钮一直没反应」比「启动报错」难查得多。
        let timezone_offset = FixedOffset::from_iana(settings.approval_base_timezone.trim())
            .map_err(|error| {
                BaseError::ConfigError(format!("feishu.approval_base_timezone 无效：{error}"))
            })?;

        let (shutdown, receiver) = watch::channel(false);
        let runner = RoundRunner {
            tools,
            context,
            tokens,
            transport,
            sleeper,
            deployment,
            rate_per_minute: settings.approval_create_rate_per_minute,
            timezone_offset,
        };
        let idle_poll = Duration::from_secs(settings.approval_scan_interval_seconds);
        let task = tokio::spawn(run_loop(runner, idle_poll, requests, receiver));

        tracing::info!(
            rate_per_minute = settings.approval_create_rate_per_minute,
            idle_poll_seconds = settings.approval_scan_interval_seconds,
            "审批派发 Worker 已启动"
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
        task.await.context("等待审批派发 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for ApprovalDispatchWorker {
    fn drop(&mut self) {
        // 忘记显式 shutdown 时也要让任务停下来，避免进程退出时任务悬挂。
        let _ = self.shutdown.send(true);
    }
}

/// 部署命名空间复用授权缓存那一份：同一个部署在缓存层必须是同一个键空间。
fn deployment_namespace(tools: &Tools) -> anyhow::Result<String> {
    Ok(tools
        .extension::<crate::authorization::AuthorizationVersionCache>()?
        .deployment()
        .to_string())
}

/// 跑一轮要用到的共享句柄。
///
/// transport 单独持有：`TenantTokenProvider` 内部那个是私有的，而 `feishu_pull`
/// 的惯例也是每轮新造一个 transport（`run_once`）——这里收成结构体，避免
/// `run_round` / `process_one` 的参数越堆越多。
struct RoundRunner {
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    transport: Arc<HttpClientTransport>,
    sleeper: Arc<TokioSleeper>,
    deployment: String,
    rate_per_minute: u32,
    timezone_offset: FixedOffset,
}

impl RoundRunner {
    fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }
}

/// 主循环：等信号或到点，然后跑一轮。
async fn run_loop(
    runner: RoundRunner,
    idle_poll: Duration,
    mut requests: mpsc::UnboundedReceiver<(i64, Option<String>)>,
    mut shutdown: watch::Receiver<bool>,
) {
    // 启动后**立即**跑第一轮（`interval` 的首个 tick 是立刻可用的），兼作
    // 「部署后不等一个间隔就有反应」。与 `feishu_pull` 同一条约定。
    let mut ticker = tokio::time::interval(idle_poll);
    // 游标在循环外：手动触发与自动轮询**共享**同一份「跑到哪了」。
    let mut cursor = ResumeCursor::default();
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("审批派发 Worker 收到关闭信号");
                    return;
                }
            }
            // 手动触发：投一个信号即跑一轮，与自动轮询走**完全同一条路径**；
            // 携带的受理发起人（已解析成 open_id）随本轮落到待处理任务行。
            Some((config_id, applicant)) = requests.recv() => {
                run_round(&runner, &mut cursor, applicant.map(|open_id| (config_id, open_id)), "manual").await;
            }
            _ = ticker.tick() => {
                run_round(&runner, &mut cursor, None, "poll").await;
            }
        }
    }
}

/// 跑一轮：播种 → （受理发起人落库）→ 认领 → 逐条处理 → 写回结果。
///
/// `applicant` 是手动受理请求携带的发起人（已解析成 open_id）；`None` 表示本轮
/// 由自动轮询触发，不落发起人。
///
/// # 播种是这条链路的地基
///
/// `claim` 只认领**已存在**的任务行，而任务行从来不是人工建的——所以没有
/// [`seed_tasks`] 的话，队列永远是空的，页面按钮点一次就只受理一个空轮。
///
/// # 崩溃恢复也靠播种
///
/// 状态推进发生在 [`record_outcome`]（逐条、处理结束时）。进程在轮中被
/// `docker stop` 杀掉时，剩下的任务停在 `creating`，而 `claim` 只要 `pending`——
/// 没有播种的话它们**永远**不会被再认领。播种把「回填列为空」当成唯一判据，
/// 于是 `creating`（崩溃遗留）、`terminal`（人工清空字段要重试）、`backfilled`
/// （清空字段要重发）都会被拉回 `pending`。
#[tracing::instrument(name = "approval_round", skip_all, fields(trigger, cursor_start = cursor.0))]
async fn run_round(
    runner: &RoundRunner,
    cursor: &mut ResumeCursor,
    applicant: Option<(i64, String)>,
    trigger: &str,
) {
    let started = std::time::Instant::now();
    tracing::Span::current().record("trigger", trigger);
    tracing::info!(stage = "seed", "审批派发轮次开始，扫描所有启用配置");
    match seed_tasks(runner).await {
        Ok(0) => {}
        Ok(seeded) => tracing::info!(seeded, "审批派发播种了新任务"),
        // 播种失败不中断整轮：已有的待处理任务仍然值得跑，而播种下一轮还会再来。
        Err(error) => tracing::warn!(error = %error, "审批派发播种失败"),
    }
    // 受理请求携带的发起人：**播种之后**落库，本轮新播种的行此刻已是 `pending`，
    // 与既有待处理行一起被写入。放在播种前会漏掉本轮新行——首点会静默回退到表格列
    // （可能是错的或空的），要再点一次才对（见 [`persist_applicant`]）。
    if let Some((config_id, open_id)) = applicant {
        persist_applicant(runner, config_id, &open_id).await;
    }
    let claimed = match claim(runner, cursor).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::error!(error = %error, "审批派发认领失败");
            return;
        }
    };
    // 队列深度：**告警的表达式**。连续为 0 说明没有积压；持续为 `CLAIM_BATCH`
    // 说明一批被处理掉了但还有——积压本身由 `idled` 分支的 gauge 兜底。
    metrics::gauge!("feishu_approval_pending").set(claimed.len() as f64);
    if claimed.is_empty() {
        cursor.advance(&claimed);
        tracing::info!(
            stage = "claim",
            cursor_next = cursor.0,
            duration_ms = started.elapsed().as_millis() as u64,
            "审批派发本轮没有可认领任务，游标已重置"
        );
        return;
    }
    tracing::info!(count = claimed.len(), "审批派发认领到任务");

    let mut counts = [0usize; 4];
    let mut idled = false;
    for task in &claimed {
        if idled {
            break;
        }
        // 取名额失败（Redis 故障）时中断整轮：没有名额继续处理只会把记录标成
        // 失败，而它们其实只是「现在不该打飞书」。已处理完的保持原结论，
        // 未处理的保持 `pending`，下一轮自然重扫。
        let Some(redis) = runner.tools.cache().ok() else {
            tracing::warn!("Redis 不可用，审批派发中断本轮");
            metrics::counter!(
                "feishu_approval_round_total",
                "result" => "idle",
                "reason" => "redis_unavailable"
            )
            .increment(1);
            idled = true;
            break;
        };
        tracing::info!(stage = "rate_limit", task_id = task.id, config_id = task.config_id, record_id = %task.record_id, "审批任务等待创建名额");
        let slot_started = std::time::Instant::now();
        if let Err(failure) =
            acquire_create_slot(redis, &runner.deployment, runner.rate_per_minute).await
        {
            tracing::warn!(error = %failure.message, "取创建名额失败，中断本轮");
            metrics::counter!(
                "feishu_approval_round_total",
                "result" => "idle",
                "reason" => "rate_limit"
            )
            .increment(1);
            idled = true;
            break;
        }

        let outcome = async {
            let task_started = std::time::Instant::now();
            tracing::info!(
                stage = "rate_limit",
                wait_ms = slot_started.elapsed().as_millis() as u64,
                "审批任务取得创建名额，开始处理"
            );
            let processed = process_one(runner, task).await;
            record_outcome(runner, task, &processed).await;
            tracing::info!(
                duration_ms = task_started.elapsed().as_millis() as u64,
                "审批任务本轮处理结束"
            );
            processed.result
        }
        .instrument(
            tracing::info_span!("approval_task", task_id = task.id, config_id = task.config_id,
            table_id = %task.table_id, record_id = %task.record_id),
        )
        .await;
        counts[match outcome {
            DispatchResult::Backfilled { .. } => 0,
            DispatchResult::Waiting { .. } => 1,
            DispatchResult::Retryable { .. } | DispatchResult::CreatedPendingBackfill { .. } => 2,
            DispatchResult::Terminal { .. } => 3,
        }] += 1;
    }
    // 全部走完才推进游标：中途 `idled` 中断时**不推进**，否则会跳过未处理的记录。
    if !idled {
        cursor.advance(&claimed);
    }

    // 超预算告警：这是「认领批 × 限速」这条配置关系是否成立的**唯一**可观测出口。
    // 超了不是错误（只是这轮比预期慢），但放任不管会让租约式的设计假设
    // （单轮跑完再开下一轮）悄悄失效。
    let elapsed = started.elapsed().as_secs() as i64;
    if elapsed > ROUND_WORST_CASE_SECONDS {
        tracing::warn!(
            elapsed_seconds = elapsed,
            budget_seconds = ROUND_WORST_CASE_SECONDS,
            claimed = claimed.len(),
            "审批派发单轮超出预算；认领批或限速配置需要复核"
        );
    }
    tracing::info!(
        claimed = claimed.len(),
        backfilled = counts[0],
        waiting = counts[1],
        retryable = counts[2],
        terminal = counts[3],
        interrupted = idled,
        cursor_next = cursor.0,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批派发轮次结束"
    );
}

/// 一条已认领的任务及其全部配置（一次查全，避免处理时再查库）。
struct ClaimedTask {
    id: i64,
    config_id: i64,
    record_id: String,
    base_token: String,
    table_id: String,
    approval_code: String,
    applicant_field: String,
    /// 发起人 open_id：全表异步受理时 Action 解析后落 `feishu_approval_task_applicant`，
    /// 这里按 task_id 取（设计 §6.3）。`Some` 时替代表格列读取。
    applicant_open_id: Option<String>,
    backfill_field: String,
    /// 六个可选回填目标（配置里的 `field_id`；`None` = 不回填）。
    status_field: Option<String>,
    flow_field: Option<String>,
    start_time_field: Option<String>,
    end_time_field: Option<String>,
    department_field: Option<String>,
    handler_field: Option<String>,
    timezone_offset: FixedOffset,
    widgets: Vec<WidgetMap>,
    form_snapshot: String,
    /// 上一轮处理结论的原因（`feishu_approval_task.last_error`）。
    ///
    /// 通知去重的判据：中间态（waiting / retryable）每轮重扫都会产生**同一个**
    /// 结论，本轮原因与它相同即「上一轮已经通知过」，不再重复发消息
    /// （见 [`should_notify`]）。
    last_error: Option<String>,
}

/// 播种：把「回填列为空」的多维表格记录变成待处理任务。
///
/// # 判据只有一个：回填列是不是空的
///
/// 这也是使用方的**重试通道**——清空那一格就等于「这条再来一遍」。所以已存在的
/// 任务行不是「跳过」，而是**按需拉回 `pending`**：
///
/// | 库里状态 | 字段为空意味着 | 处置 |
/// |---|---|---|
/// | `creating` | 上一轮崩在这条上 | 拉回 `pending` |
/// | `terminal` | 人工清空要重试 | 拉回 `pending` |
/// | `backfilled` | 人工清空要重发 | 拉回 `pending`（uuid 幂等兜底，不会重复建单） |
/// | `pending` | 已在队列里 | 不动 |
///
/// # 为什么要读全表而不是「只看有任务的行」
///
/// 首次运行时库里一条任务都没有——不扫表就永远不会有第一条。
async fn seed_tasks(runner: &RoundRunner) -> anyhow::Result<usize> {
    let configs = runner
        .context
        .approval_configs()
        .query()
        .select_fields(&[
            "id",
            "base_token",
            "table_id",
            "approval_code",
            "backfill_field",
        ])?
        .where_eq("enabled", serde_json::json!(true))?;
    let configs = all_pages(configs, SEED_MAX_CONFIG_PAGES).await?;
    tracing::info!(
        stage = "seed",
        config_count = configs.len(),
        "审批派发读取启用配置完成"
    );

    let mut seeded = 0usize;
    for config in configs {
        match seed_config(runner, &config).await {
            Ok(count) => seeded += count,
            // 一条配置坏掉不该拖垮别的配置：记下来，下一轮再来。
            Err(error) => {
                let config_id: i64 = config.require("id").unwrap_or_default();
                tracing::warn!(error = %error, config_id, "审批派发播种跳过该配置");
            }
        }
    }
    Ok(seeded)
}

/// 单条配置的播种。
async fn seed_config(runner: &RoundRunner, config: &Record) -> anyhow::Result<usize> {
    let started = std::time::Instant::now();
    let config_id: i64 = config.require("id")?;
    let base_token: String = config.require("base_token")?;
    let table_id: String = config.require("table_id")?;
    let approval_code: String = config.require("approval_code")?;
    let backfill_field_id: String = config.require("backfill_field")?;
    tracing::info!(stage = "seed", config_id, table_id = %table_id, backfill_field = %backfill_field_id, "审批派发开始扫描回填列为空的记录");

    let coordinates = BitableCoordinates {
        app_token: base_token.clone(),
        table_id: table_id.clone(),
        view_id: None,
    };

    // 筛选按**列名**（不是 id）：`filter.field_name` 与 `field_names` 同一条口径。
    let fields = bitable::list_all_fields(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
    )
    .await
    .map_err(|failure| anyhow::anyhow!("读取表结构失败：{}", failure.message))?;
    let backfill_name = bitable::resolve_field_name(&fields, &backfill_field_id)?;

    let pending_records = bitable::search_all_records(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
        bitable::empty_field_filter(&backfill_name),
        SEED_MAX_PAGES,
    )
    .await
    .map_err(|failure| anyhow::anyhow!("筛选待处理记录失败：{}", failure.message))?;
    tracing::info!(stage = "seed", config_id, table_id = %table_id, field_count = fields.len(), pending_records = pending_records.len(), "审批派发待处理记录扫描完成");
    if pending_records.is_empty() {
        return Ok(0);
    }

    // 已有的任务行：按 record_id 建索引，决定「新建」还是「拉回 pending」。
    let existing = all_pages(
        runner
            .context
            .approval_tasks()
            .query()
            .select_fields(&["id", "record_id", "state"])?
            .where_eq("config_id", serde_json::json!(config_id))?,
        SEED_MAX_TASK_PAGES,
    )
    .await?;
    let mut by_record: BTreeMap<String, (i64, String)> = BTreeMap::new();
    for row in &existing {
        by_record.insert(
            row.require("record_id")?,
            (row.require("id")?, row.require("state")?),
        );
    }

    let mut seeded = 0usize;
    let mut restored = 0usize;
    for record in &pending_records {
        if record.record_id.is_empty() {
            continue;
        }
        match by_record.get(&record.record_id) {
            Some((_, state)) if state == "pending" => {}
            Some((task_id, state)) => {
                // 拉回 `pending` 并清掉上一轮的结论：字段为空是**唯一**判据，
                // 库里那条旧结论（失败原因或已回填）已经不再成立。
                let mut row = Record::new();
                row.insert("state", serde_json::json!("pending"));
                row.insert("last_error", serde_json::Value::Null);
                runner
                    .context
                    .approval_tasks()
                    .query()
                    .where_primary_key_eq(serde_json::json!(task_id))?
                    .update(row)
                    .await?;
                restored += 1;
                tracing::info!(stage = "seed", config_id, task_id, record_id = %record.record_id, from = %state, "审批任务被拉回待处理");
            }
            None => {
                // 幂等键与派发路径**同源**：同一个 record 只有一条任务行，
                // 重复播种会撞 `record_id` 唯一索引而不是建出第二条。
                let uuid = derive_uuid(&base_token, &table_id, &approval_code, &record.record_id);
                let mut row = Record::new();
                row.insert("config_id", serde_json::json!(config_id));
                row.insert("record_id", serde_json::json!(&record.record_id));
                row.insert("uuid", serde_json::json!(uuid));
                row.insert("state", serde_json::json!("pending"));
                runner.context.approval_tasks().query().insert(row).await?;
                seeded += 1;
            }
        }
    }
    tracing::info!(
        stage = "seed",
        config_id,
        seeded,
        restored,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批配置播种完成"
    );
    Ok(seeded)
}

/// 把受理请求携带的发起人 open_id 落到该配置下的待处理任务行（设计 §6.3）。
///
/// # 为什么在播种之后
///
/// 按钮受理的语义是「本轮把这些行按受理发起人提单」。新行的任务行由 [`seed_tasks`]
/// 在本轮播种出来，播种前写会**漏掉本轮新行**——首点会静默回退到表格列（可能是错的
/// 或空的），要再点一次才对。所以写在播种之后、认领之前。
///
/// # 失败只 warn
///
/// 落库失败不该中断整轮：受理发起人只是**增强**，任务照旧处理（回退表格列），
/// 下一轮按钮或轮询还会再写。
async fn persist_applicant(runner: &RoundRunner, config_id: i64, open_id: &str) {
    let built = runner
        .context
        .approval_tasks()
        .query()
        .select_fields(&["id"])
        .and_then(|query| query.where_eq("config_id", serde_json::json!(config_id)))
        .and_then(|query| query.where_eq("state", serde_json::json!("pending")));
    let rows = match built {
        Ok(query) => match all_pages(query, SEED_MAX_TASK_PAGES).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(error = %error, config_id, "读取待处理任务行失败，受理发起人未落库");
                return;
            }
        },
        Err(error) => {
            tracing::warn!(error = %error, config_id, "构造待处理任务查询失败，受理发起人未落库");
            return;
        }
    };

    for row in rows {
        let task_id: i64 = match row.require("id") {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(error = %error, "任务行缺少 id，跳过受理发起人落库");
                continue;
            }
        };
        if let Err(error) = upsert_applicant(runner, task_id, open_id).await {
            tracing::warn!(error = %error, task_id, "受理发起人落库失败");
        }
    }
}

/// 按 task_id upsert 一行受理发起人：已有则更新 `open_id`，没有则插入。
async fn upsert_applicant(runner: &RoundRunner, task_id: i64, open_id: &str) -> anyhow::Result<()> {
    let existing = runner
        .context
        .approval_task_applicants()
        .query()
        .where_eq("task_id", serde_json::json!(task_id))?
        .optional()
        .await?;
    if existing.is_some() {
        let mut row = Record::new();
        row.insert("open_id", serde_json::json!(open_id));
        runner
            .context
            .approval_task_applicants()
            .query()
            .where_eq("task_id", serde_json::json!(task_id))?
            .update(row)
            .await?;
    } else {
        let mut row = Record::new();
        row.insert("task_id", serde_json::json!(task_id));
        row.insert("open_id", serde_json::json!(open_id));
        runner
            .context
            .approval_task_applicants()
            .query()
            .insert(row)
            .await?;
    }
    Ok(())
}

/// 认领待处理任务。
///
/// 判据是 `feishu_approval_task` 的状态，**不是**多维表格的回填字段是否为空
/// ——后者是输出位，用它当扫描键会把「填一半」的行永久钉死（见模块文档）。
///
/// # 为什么认领条件里没有 `lease_until`
///
/// 原设计照搬 `authorization_outbox` 的 `state='creating' AND lease_until <= now`，
/// 那条是**跨实例**并发的产物。本 worker 是单实例（部署形态决策 A10）+ 单线程
/// 循环，每轮开始时根本没有别的执行者持有 `creating`，再判租约只是多一次时钟
/// 依赖。
///
/// **但「只认 `pending`」这条不能单独成立**：轮中崩溃留下的 `creating` 行永远不会
/// 自己回到 `pending`，`claim` 也就永远不再看它们。补上这一环的是 [`seed_tasks`]
/// ——它以「回填列为空」为唯一判据，把崩溃遗留、人工清空要重试的行一并拉回
/// `pending`。两者的分工是：**播种决定「谁该被处理」，认领决定「这一轮处理哪一批」**。
async fn claim(runner: &RoundRunner, cursor: &ResumeCursor) -> anyhow::Result<Vec<ClaimedTask>> {
    // 游标下界：上一轮已处理到的位置。空游标时不加条件（从头开始）。
    let mut query = runner.context.approval_tasks().query();
    query = query.where_eq("state", serde_json::json!("pending"))?;
    if cursor.0 > 0 {
        // 单独用 `where_gt` 而不与 `where_eq` 合并进一个 `WhereCondition` 组：
        // 顶层条件本就是隐式 AND，这样写更短也少一处嵌套。
        query = query.where_gt("id", serde_json::json!(cursor.0))?;
    }
    let rows = query
        .order_by("id", SortOrder::Asc)?
        // `page`/`page_size` 是 `select` 的缺省（`page_size` 缺省 10！），
        // 不显式限定就会只取 10 行——认领批就是这里的一行代码。
        .page(1, CLAIM_BATCH)?
        // `last_error` 进投影是给通知去重用的（`should_notify` 拿它与本轮原因
        // 比对），多取一列省掉「每条任务再查一次自己」的读。
        .select_fields(&["id", "record_id", "config_id", "last_error"])?
        .all()
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut tasks = Vec::with_capacity(rows.len());
    for row in rows {
        match load_task(runner, &row).await? {
            Some(task) => tasks.push(task),
            None => {
                // 配置缺失/停用/时区无效：置终态，不再反复认领。
                let id = row.require("id")?;
                mark_terminal(runner, id, "所属配置不存在或已停用").await;
            }
        }
    }
    Ok(tasks)
}

/// 置终态（配置缺失等无法继续的情形）。
///
/// 终态的意义是「该行退出认领集，等人工处理」，所以必须清租约——否则它还会被
/// `lease_until <= now` 那条分支捞回来。
async fn mark_terminal(runner: &RoundRunner, id: i64, reason: &str) {
    let mut row = Record::new();
    row.insert("state", serde_json::json!("terminal"));
    row.insert("last_error", serde_json::json!(reason));
    row.insert("lease_until", serde_json::Value::Null);
    // 两步而非 `and_then`：`where_eq` 返回 `Result`，而 `update` 返回 future，
    // 两者不能链在同一个闭包里。本函数**不**返回 `Result`——写不进去时把它记成
    // 日志而不是冒泡，因为终态写失败不该中断「报告刚发生的事」。
    let result = match runner
        .context
        .approval_tasks()
        .query()
        .where_eq("id", serde_json::json!(id))
    {
        Ok(query) => query.update(row).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(error = %error, task_id = id, "置审批任务终态失败");
    }
}

/// 读一条任务的配置与字段映射。
async fn load_task(runner: &RoundRunner, row: &Record) -> anyhow::Result<Option<ClaimedTask>> {
    let config_id: i64 = row.require("config_id")?;
    let task_id: i64 = row.require("id")?;

    let config = runner
        .context
        .approval_configs()
        .query()
        .where_eq("id", serde_json::json!(config_id))?
        .where_eq("enabled", serde_json::json!(true))?
        .optional()
        .await?;
    let Some(config) = config else {
        tracing::warn!(
            stage = "claim",
            task_id,
            config_id,
            reason = "所属配置不存在或已停用",
            "审批任务无法加载配置"
        );
        return Ok(None);
    };

    // 发起人是**后续查询**，不是认领批的列（认领投影只有 id/record_id/config_id/last_error）：
    // 全表异步受理拿不到发起人，Action 解析后落在属性扩展表里（设计 §6.3）。
    let applicant = runner
        .context
        .approval_task_applicants()
        .query()
        .where_eq("task_id", serde_json::json!(task_id))?
        .optional()
        .await?;
    let applicant_open_id = match applicant {
        Some(applicant) => applicant.optional::<String>("open_id")?,
        None => None,
    };

    let map_rows = runner
        .context
        .approval_field_maps()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .all()
        .await?;
    let maps: Vec<serde_json::Map<String, serde_json::Value>> =
        map_rows.iter().map(|row| row.clone().into_map()).collect();

    let Some(widgets) = widget_maps_from_rows(&maps, |_| None) else {
        tracing::warn!(
            stage = "claim",
            task_id,
            config_id,
            reason = "字段映射损坏或转换器非法",
            "审批任务无法加载配置"
        );
        return Ok(None);
    };
    // 没有映射的任务提不出去，但那多半是「配置还在建」而不是坏掉——
    // 走**等待态**（回 pending）比置终态更合适，等映射配好后自然重扫。
    if widgets.is_empty() {
        tracing::warn!(
            stage = "claim",
            task_id,
            config_id,
            reason = "配置没有字段映射",
            "审批任务无法加载配置"
        );
        return Ok(None);
    }

    // 时区优先取**单配置级**；无效时回退到部署默认值，而不是把任务判死——
    // 「一条配置写错时区」不该让它的所有记录永远无法提单。部署默认值在
    // worker 启动时已解析成功（解析不了会直接启动失败），所以这条回退是安全的。
    let base_timezone: String = config.require("base_timezone")?;
    let timezone_offset = match FixedOffset::from_iana(base_timezone.trim()) {
        Ok(offset) => offset,
        Err(error) => {
            tracing::warn!(
                error = %error,
                config_id,
                "配置的 Base 时区无效，回退到部署默认值"
            );
            runner.timezone_offset
        }
    };

    Ok(Some(ClaimedTask {
        id: task_id,
        config_id,
        record_id: row.require("record_id")?,
        base_token: config.require("base_token")?,
        table_id: config.require("table_id")?,
        approval_code: config.require("approval_code")?,
        applicant_field: config.require("applicant_field")?,
        applicant_open_id,
        backfill_field: config.require("backfill_field")?,
        // 空串与 NULL 同义（设计 §5.1）：两条读路径（Action 与 worker）口径一致，
        // 各做一次归一化，不依赖配置写入路径把空串存成 NULL。
        status_field: config
            .optional::<String>("status_field")?
            .filter(|v| !v.trim().is_empty()),
        flow_field: config
            .optional::<String>("flow_field")?
            .filter(|v| !v.trim().is_empty()),
        start_time_field: config
            .optional::<String>("start_time_field")?
            .filter(|v| !v.trim().is_empty()),
        end_time_field: config
            .optional::<String>("end_time_field")?
            .filter(|v| !v.trim().is_empty()),
        department_field: config
            .optional::<String>("department_field")?
            .filter(|v| !v.trim().is_empty()),
        handler_field: config
            .optional::<String>("handler_field")?
            .filter(|v| !v.trim().is_empty()),
        timezone_offset,
        widgets,
        form_snapshot: config.optional("form_snapshot")?.unwrap_or_default(),
        last_error: row.optional("last_error")?,
    }))
}

/// 一条任务的处理产物。
///
/// 三个字段一起返回而不是拆成多次调用：它们都来自**同一次**记录读取，
/// 分开取会让「读记录」发生两遍。
struct TaskOutcome {
    result: DispatchResult,
    /// 申请人 open_id（任务表或表格列）——通知的首选接收人。
    applicant_open_id: Option<String>,
    /// 记录创建人 open_id（`automatic_fields` 取回）——申请人缺失时的兜底
    /// 接收人，保证「用户发起过请求」这件事在任何结论下都有反馈出口。
    creator_open_id: Option<String>,
}

impl TaskOutcome {
    /// 早退构造：还没读到记录（拿不到创建人），只有结论。
    fn early(result: DispatchResult) -> Self {
        Self {
            result,
            applicant_open_id: None,
            creator_open_id: None,
        }
    }
}

/// 处理一条记录。
async fn process_one(runner: &RoundRunner, task: &ClaimedTask) -> TaskOutcome {
    let coordinates = BitableCoordinates {
        app_token: task.base_token.clone(),
        table_id: task.table_id.clone(),
        view_id: None,
    };

    tracing::info!(stage = "fields", applicant_field = %task.applicant_field, backfill_field = %task.backfill_field, widget_count = task.widgets.len(), "审批任务开始读取多维表格结构");

    // 表结构要先拿：官方《批量获取记录》的 `fields` **按字段名**作键，而域里按
    // `field_id` 取值（配置存 id）。不重映射的话按 id 永远查不到，每条记录都判成
    // 「缺少申请人」——不报错，只是永远停在等待态。
    let fields = match bitable::list_all_fields(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => fields,
        // 表结构拿不到多半是凭证/网络抖动，判可重试。
        Err(failure) => {
            tracing::warn!(stage = "fields", kind = ?failure.kind, "审批任务读取表结构失败");
            return TaskOutcome::early(DispatchResult::Retryable {
                message: format!("读取表结构失败：{}", failure.message),
            });
        }
    };

    tracing::info!(
        stage = "record",
        field_count = fields.len(),
        "审批任务开始读取源记录"
    );

    // 不投影：官方 `records/batch_get` 的请求体没有 `field_names` 参数。
    let found = bitable::get_records_by_ids(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
        std::slice::from_ref(&task.record_id),
    )
    .await;
    let (cells, creator_open_id): (serde_json::Map<String, serde_json::Value>, Option<String>) =
        match found {
            Ok(items) => match items.into_iter().next() {
                Some(record) => {
                    // 记录创建人 = 通知接收人的兜底：申请人列空着时（waiting 的
                    // 主因），「谁建了这条记录」是唯一还能定位到的触发人。
                    // `automatic_fields: true` 时随记录**顶层**返回；取不到只是
                    // 少一条兜底，不阻塞派发。
                    let creator_open_id = record
                        .created_by
                        .as_ref()
                        .map(|person| person.id.trim().to_string())
                        .filter(|id| !id.is_empty());
                    match bitable::rekey_cells_by_field_id(&fields, &record.fields) {
                        Ok(cells) => (cells, creator_open_id),
                        Err(error) => {
                            tracing::warn!(
                                stage = "record",
                                "审批任务解析记录字段失败，详情见任务记录"
                            );
                            return TaskOutcome::early(DispatchResult::Terminal {
                                message: format!("解析记录字段失败：{error}"),
                            });
                        }
                    }
                }
                None => {
                    tracing::warn!(stage = "record", "多维表格里已找不到审批源记录");
                    return TaskOutcome::early(DispatchResult::Terminal {
                        message: "多维表格里已找不到该记录".to_string(),
                    });
                }
            },
            Err(failure) => {
                tracing::warn!(stage = "record", kind = ?failure.kind, "审批任务读取源记录失败");
                return TaskOutcome::early(DispatchResult::Retryable {
                    message: format!("读取记录失败：{}", failure.message),
                });
            }
        };

    // 回填列：配置里存的是 `field_id`，而 `batch_update` 的 `records[].fields` 按
    // **列名**作键——与上面那条筛选同一个口径（「按列名，不是 id」），所以要在这里
    // 补一次解析。少了它，回写会带着 `fld…` 去打一个按名匹配的接口。
    let backfill_field_name = match bitable::resolve_field_name(&fields, &task.backfill_field) {
        Ok(name) => name,
        // 列被删了/改名后重名：重试好不了，判终态（回填列本身写不进去，只能留日志）。
        Err(error) => {
            tracing::warn!(stage = "fields", backfill_field = %task.backfill_field, "审批任务回填列不可用，详情见任务记录");
            return TaskOutcome::early(DispatchResult::Terminal {
                message: format!("回填列不可用：{error}"),
            });
        }
    };

    tracing::info!(
        stage = "record",
        cell_count = cells.len(),
        "审批源记录字段映射完成"
    );

    let backfill = BitableBackfill {
        transport: runner.transport(),
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates: &coordinates,
        fields: &fields,
    };
    let result = dispatch_one(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &backfill,
        &OrchestrationInput {
            fields: &fields,
            context: Some(&runner.context),
            coordinates: &coordinates,
            record_id: &task.record_id,
            cells: &cells,
            applicant_field: &task.applicant_field,
            applicant: task.applicant_open_id.clone(),
            status_field: task.status_field.as_deref(),
            flow_field: task.flow_field.as_deref(),
            start_time_field: task.start_time_field.as_deref(),
            end_time_field: task.end_time_field.as_deref(),
            department_field: task.department_field.as_deref(),
            handler_field: task.handler_field.as_deref(),
            backfill_field_name: &backfill_field_name,
            approval_code: &task.approval_code,
            widgets: &task.widgets,
            form_snapshot: task.form_snapshot.clone(),
            timezone_offset: task.timezone_offset,
        },
    )
    .await;

    // 从表格 cells 提取申请人 open_id（poll 触发时 task.applicant_open_id 为空，回退到表格）
    let open_id = task
        .applicant_open_id
        .clone()
        .or_else(|| extract_applicant_open_id(&cells, &task.applicant_field));

    TaskOutcome {
        result,
        applicant_open_id: open_id,
        creator_open_id,
    }
}

/// 组装持久化事实，已确认实例在回填失败时也必须保留。
fn outcome_record(outcome: &DispatchResult) -> Record {
    // 结论 → 状态 + 原因：
    //
    // - `backfilled`：编号已写进多维表格，任务完成（**终态**，不再认领）。
    // - `pending`：等待态与可重试。都回 pending——前者要靠用户补齐字段自然重扫，
    //   后者要靠下一轮重试；两者都不该退出认领集。
    // - `terminal`：配置/数据的结构性问题，回填字段已写原因，退出认领集。
    //
    // 等待态写 `last_error` 只是**可观测**（运维能看到哪些行还缺什么），不影响
    // 认领——它不是状态位。
    let (state, reason) = match outcome {
        DispatchResult::Backfilled { .. } => ("backfilled", None),
        DispatchResult::Waiting { reason } => ("pending", Some(reason.clone())),
        DispatchResult::Retryable { message }
        | DispatchResult::CreatedPendingBackfill { message, .. } => {
            ("pending", Some(message.clone()))
        }
        DispatchResult::Terminal { message } => ("terminal", Some(message.clone())),
    };
    let mut row = Record::new();
    row.insert("state", serde_json::json!(state));
    row.insert(
        "last_error",
        reason
            .clone()
            .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    // 清租约：状态已经是稳定状态，不需要再让租约复活它。
    row.insert("lease_until", serde_json::Value::Null);

    // 创建成功时把实例坐标一并落库：`instance_code` / `serial_number` 两列早已存在
    // 却一直是 NULL（设计 §12 缺口 10），事件订阅与 60012 对账都拿不到实例坐标。
    if let DispatchResult::Backfilled {
        serial_number,
        instance_code,
    } = outcome
    {
        row.insert("instance_code", serde_json::json!(instance_code));
        row.insert("serial_number", serde_json::json!(serial_number));
    }

    if let DispatchResult::CreatedPendingBackfill {
        instance_code,
        serial_number,
        ..
    } = outcome
    {
        row.insert("instance_code", serde_json::json!(instance_code));
        if let Some(number) = serial_number {
            row.insert("serial_number", serde_json::json!(number));
        }
    }
    row
}

/// 本轮是否该发通知。
///
/// 判据是「结论相对上一轮**发生了变化**」：
/// - 终态（`backfilled` / `terminal`）只会出现一次（任务退出认领集），总是通知；
/// - 中间态（`waiting` / `retryable` / `created_pending_backfill`）会随重扫反复
///   出现，本轮原因与任务行 `last_error`（上一轮写入的原因）相同即「上一轮已经
///   通知过」，跳过——否则一条缺申请人的记录会每轮给用户重发一条同样的消息。
fn should_notify(task: &ClaimedTask, outcome: &DispatchResult) -> bool {
    let interim_reason = match outcome {
        DispatchResult::Waiting { reason } => reason,
        DispatchResult::Retryable { message } => message,
        DispatchResult::CreatedPendingBackfill { message, .. } => message,
        DispatchResult::Backfilled { .. } | DispatchResult::Terminal { .. } => return true,
    };
    task.last_error.as_deref() != Some(interim_reason.as_str())
}

/// 把处理结论落库。
async fn record_outcome(runner: &RoundRunner, task: &ClaimedTask, processed: &TaskOutcome) {
    let outcome = &processed.result;
    let row = outcome_record(outcome);
    let state = row
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let outcome_name = match outcome {
        DispatchResult::Backfilled { .. } => "backfilled",
        DispatchResult::Waiting { .. } => "waiting",
        DispatchResult::Retryable { .. } => "retryable",
        DispatchResult::CreatedPendingBackfill { .. } => "created_pending_backfill",
        DispatchResult::Terminal { .. } => "terminal",
    };
    tracing::info!(
        stage = "persist",
        outcome = outcome_name,
        state,
        "审批任务处理结论，等待和可重试任务保留 pending；具体原因见阶段日志和任务 last_error"
    );
    let result = match runner
        .context
        .approval_tasks()
        .query()
        .where_eq("id", serde_json::json!(task.id))
    {
        Ok(query) => query.update(row).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(error = %error, task_id = task.id, "写回审批任务状态失败");
    } else {
        tracing::info!(stage = "persist", state, "审批任务状态写回成功");
    }

    // 指标：按结论分桶。`terminal` 与 `waiting` 分开是关键——前者要人动，
    // 后者只要等，混在一个桶里就分不清「有人卡住了」与「数据还没填完」。
    metrics::counter!(
        "feishu_approval_dispatch_total",
        "outcome" => match outcome {
            DispatchResult::Backfilled { .. } => "backfilled",
            DispatchResult::Waiting { .. } => "waiting",
            DispatchResult::Retryable { .. } | DispatchResult::CreatedPendingBackfill { .. } => "retryable",
            DispatchResult::Terminal { .. } => "terminal",
        }
    )
    .increment(1);

    // 审计：只有**成功创建了审批单**才落事件。
    //
    // 依据是 `docs/contracts/AUDIT.md` 的高权限口径——本动作以系统身份创建了
    // 有外部副作用、可被追责的审批单据，这正是「谁、何时、对哪一行、发了哪个
    // 实例」必须留痕的理由。等待态与失败不落：它们是**没有发生的事**，
    // 落进 append-only 审计只会制造噪音，而且失败原因已在多维表格与 `last_error`
    // 各有出处。
    //
    // 摘要键避开 `token`/`hash` 等敏感词（`validate_summary_key` 会拒收）。
    if let DispatchResult::Backfilled { serial_number, .. } = outcome {
        // 本函数不返回 `Result`（状态写失败只记日志，不该中断「报告刚发生的事」），
        // 所以构造链里的 `?` 全部改成显式 `match` + 日志。
        let constructed: Result<_, BaseError> = (|| {
            let config_entity = audit::entity("feishu_approval_config", task.config_id)?;
            let task_entity = audit::entity("feishu_approval_task", task.id)?;
            let summary = audit::summary([
                ("outcome_code", serde_json::json!("created")),
                ("record_id", serde_json::json!(task.record_id.clone())),
                ("serial_number", serde_json::json!(serial_number.clone())),
            ])?;
            audit::succeeded_system_event_without_ctx(
                "feishu-approval-worker",
                "feishu.approval_dispatch",
                Some(config_entity),
                task_entity,
                summary,
            )
        })();
        // 审计构造失败不该吞掉「审批单已创建」这个事实：记日志后继续，
        // 任务状态那次写回仍然有效。
        if let Err(error) = &constructed {
            tracing::error!(error = %error, task_id = task.id, "审批派发审计事件构造失败");
        }
        if let Ok(event) = constructed {
            match runner.tools.mysql() {
                Ok(database) => {
                    if let Err(error) = audit::append_independent(database.pool(), &event).await {
                        tracing::error!(
                            error = %error,
                            task_id = task.id,
                            "审批派发审计写入失败"
                        );
                    }
                }
                Err(error) => {
                    tracing::error!(error = %error, task_id = task.id, "审批派发审计取库失败");
                }
            }
        }
    }

    // 通知：**任何结论都要反馈**（2026-10-15 原则：只要用户发起请求，成功/失败
    // 都要发一条消息，失败附原因）。中间态的重复由 [`should_notify`] 按
    // 「结论是否变化」去重。
    //
    // 接收人链：任务表申请人 → 表格申请人列 → **记录创建人**（兜底）。前两者
    // 空着正是 waiting 的主因，而「谁建了这条记录」是那时唯一还能定位到的触发人。
    // 三者都取不到才跳过——那时「发给谁」在事实上无解，留 warn 让静默丢失可查。
    if should_notify(task, outcome) {
        let open_id = task
            .applicant_open_id
            .as_deref()
            .or(processed.applicant_open_id.as_deref())
            .or(processed.creator_open_id.as_deref());
        match open_id {
            Some(open_id) => send_worker_notification(runner, task, outcome, open_id).await,
            None => tracing::warn!(
                task_id = task.id,
                config_id = task.config_id,
                "Worker 通知：申请人 open_id 与记录创建人均不可用，跳过通知"
            ),
        }
    }
}

/// 请求日志行是否表示「HTTP 路径**确实发出过**通知」。
///
/// 判据是响应体里 `notification.status == "sent"`，**不是** `outcome=succeeded`
/// ——「HTTP 处理成功但没发通知」是常态：工作流未传 `notification_recipient`
/// 时 HTTP 路径按设计不发（not_requested）。2026-10-14 的「没发消息」事故
/// 就是把「处理过」当成了「通知过」，两层都跳过导致一条都不发。
fn http_path_notified(row: &Record) -> bool {
    row.optional::<String>("response_body")
        .ok()
        .flatten()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|value| {
            value
                .get("notification")
                .and_then(|notification| notification.get("status"))
                .and_then(|status| status.as_str())
                .map(|status| status == "sent")
        })
        .unwrap_or(false)
}

/// Worker 路径的 IM 通知：向触发人发送审批派发结果。
///
/// 失败只 warn，不冒泡。通知是附加价值，不该影响审批结论或重试逻辑。
async fn send_worker_notification(
    runner: &RoundRunner,
    task: &ClaimedTask,
    outcome: &DispatchResult,
    open_id: &str,
) {
    // 去重：只有 HTTP Action **确实发出过通知**才跳过。
    //
    // HTTP 路径通知「显式指定的接收人」（notification_recipient），本 worker
    // 通知「申请人」——两者互补：HTTP 没发时（未指定接收人是常态）由这里兜底，
    // HTTP 发过时才避免重复。
    //
    // 查 `succeeded` **与** `failed` 两档：HTTP 层只在终态兜底（`fallback_recipient_of`
    // 只认 `Succeeded | Failed`），而 `outcome_for` 把 `Terminal` 折成 `failed`——
    // 只查 `succeeded` 会让「HTTP 已发过失败通知」的终态在 worker 再发一遍。
    // 中间态（`waiting`）HTTP 层不发，worker 必须接手，所以不查它。
    let request_log_check = async {
        let query = runner.context.approval_request_logs().query();
        let query = query.where_eq("record_id", serde_json::json!(task.record_id))?;
        let query = query.where_eq("config_id", serde_json::json!(task.config_id))?;
        let query = query.where_in(
            "outcome",
            vec![serde_json::json!("succeeded"), serde_json::json!("failed")],
        )?;
        query.all().await
    };
    match request_log_check.await {
        Ok(rows) => {
            if rows.iter().any(http_path_notified) {
                tracing::info!(
                    task_id = task.id,
                    config_id = task.config_id,
                    record_id = %task.record_id,
                    "Worker 通知：HTTP Action 已发送过通知，跳过重复通知"
                );
                return;
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                task_id = task.id,
                config_id = task.config_id,
                "Worker 通知：查询请求日志失败，跳过通知"
            );
            return;
        }
    }

    // 读配置行取展示名（2s 预算，与 HTTP Action 的 complete_feedback 一致）
    let config_lookup = async {
        runner
            .context
            .approval_configs()
            .query()
            .where_primary_key_eq(serde_json::json!(task.config_id))?
            .optional()
            .await
    };
    let config = match tokio::time::timeout(std::time::Duration::from_secs(2), config_lookup).await
    {
        Ok(Ok(row)) => row,
        _ => {
            tracing::warn!(
                task.config_id,
                task_id = task.id,
                "Worker 通知：配置展示名称读取失败，跳过通知"
            );
            return;
        }
    };

    let name = |field: &str| {
        let value = config
            .as_ref()
            .and_then(|row| row.optional::<String>(field).ok().flatten());
        approval_feedback::display_name(value.as_deref())
    };
    let names = format!(
        "配置：{}\n多维表：{}\n数据表：{}\n审批：{}",
        name("title"),
        name("base_name"),
        name("table_name"),
        name("approval_name")
    );

    // 渲染消息正文
    let status = outcome.feedback_status();
    let (accepted, reason, _serial) = outcome.response_parts();
    let _ = accepted; // 通知不关心 HTTP 语义的 accepted
    let message = approval_feedback::render(
        status,
        &reason,
        &names,
        None, // Worker 路径不取记录摘要，省一次 API 调用
        Some(&task.record_id),
        &task.base_token,
        &task.table_id,
    );

    // 构造 references（审批实例链接 + 多维表格链接）
    let table_url = (|| -> anyhow::Result<String> {
        bitable::validate_path_segment("base_token", &task.base_token)?;
        bitable::validate_path_segment("table_id", &task.table_id)?;
        Ok(format!(
            "https://www.feishu.cn/base/{}?table={}",
            task.base_token, task.table_id
        ))
    })()
    .ok();

    let mut references = vec![FeedbackReference {
        label: "多维表",
        text: format!("{} / {}", name("base_name"), name("table_name")),
        href: table_url,
    }];

    // 成功时加审批实例链接
    if let DispatchResult::Backfilled {
        instance_code,
        serial_number,
    } = outcome
    {
        let approval_text = format!("{} / {}", name("approval_name"), serial_number.as_str());
        let approval_href = instance_applink(instance_code).ok();
        references.push(FeedbackReference {
            label: "审批",
            text: approval_text,
            href: approval_href,
        });
    }

    let recipient = NotificationRecipient {
        receive_id_type: ReceiveIdType::OpenId,
        receive_id: open_id.to_string(),
    };

    tracing::info!(
        task_id = task.id,
        config_id = task.config_id,
        open_id,
        "Worker 通知：开始发送 IM 消息"
    );

    let result = crate::addon::feishu::domain::approval_notification::send_feedback(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &recipient,
        &message,
        &references,
    )
    .await;

    match result.status {
        crate::addon::feishu::domain::approval_notification::NotificationStatus::Sent => {
            tracing::info!(
                task_id = task.id,
                config_id = task.config_id,
                message_id = ?result.message_id,
                "Worker 通知：IM 消息发送成功"
            );
        }
        _ => {
            tracing::warn!(
                task_id = task.id,
                config_id = task.config_id,
                status = ?result.status,
                message = %result.message,
                "Worker 通知：IM 消息发送失败（不影响审批结论）"
            );
        }
    }
}

/// 一轮里完成的回填：由 `run_round` 收集后统一批量写。
///
/// 单条处理已经把 `serial_number` 写进了多维表格（`dispatch_one` 内部），
/// 这里**只在诊断/补救路径用**；保留是为了让「集中批量回填」的路径可被单测。
#[allow(dead_code)]
async fn flush_backfill(
    runner: &RoundRunner,
    coordinates: &BitableCoordinates,
    field_id: &str,
    rows: &[(String, serde_json::Value)],
) -> anyhow::Result<()> {
    let fields = bitable::list_all_fields(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        coordinates,
    )
    .await?;
    let backfill = BitableBackfill {
        transport: runner.transport(),
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates,
        fields: &fields,
    };
    let report = backfill_in_chunks(&backfill, field_id, rows).await;
    if !report.failed.is_empty() {
        tracing::warn!(
            failed = report.failed.len(),
            written = report.written.len(),
            "批量回填存在失败记录"
        );
    }
    Ok(())
}

/// 供测试引用：把认领配置折成编排输入需要的形态。
#[allow(dead_code)]
fn _assert_widget_map_type(_: &WidgetMap) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_pending_backfill_keeps_instance_and_known_serial_number() {
        for number in [None, Some("SN-001".to_string())] {
            let outcome = DispatchResult::CreatedPendingBackfill {
                message: "编号回填失败".into(),
                instance_code: "INST-001".into(),
                serial_number: number.clone(),
            };
            let row = outcome_record(&outcome);
            assert_eq!(row.get("state"), Some(&serde_json::json!("pending")));
            assert_eq!(
                row.get("last_error"),
                Some(&serde_json::json!("编号回填失败"))
            );
            assert_eq!(row.get("lease_until"), Some(&serde_json::Value::Null));
            assert_eq!(
                row.get("instance_code"),
                Some(&serde_json::json!("INST-001"))
            );
            assert_eq!(
                row.get("serial_number"),
                number.as_ref().map(|v| serde_json::json!(v)).as_ref()
            );
        }
        for (outcome, state) in [
            (
                DispatchResult::Backfilled {
                    instance_code: "INST".into(),
                    serial_number: "SN".into(),
                },
                "backfilled",
            ),
            (
                DispatchResult::Waiting {
                    reason: "补齐数据".into(),
                },
                "pending",
            ),
            (
                DispatchResult::Retryable {
                    message: "服务不可用".into(),
                },
                "pending",
            ),
            (
                DispatchResult::Terminal {
                    message: "配置错误".into(),
                },
                "terminal",
            ),
        ] {
            assert_eq!(
                outcome_record(&outcome).get("state"),
                Some(&serde_json::json!(state))
            );
        }
    }

    #[test]
    fn exhausted_cursor_revisits_pending_tasks() {
        let mut cursor = ResumeCursor(42);
        cursor.advance(&[]);
        assert_eq!(cursor.0, 0, "扫描结束后旧 pending 任务必须能重新认领");
    }

    /// 最小 `ClaimedTask`：只给通知判定用得到的字段赋值。
    fn claimed(last_error: Option<&str>) -> ClaimedTask {
        ClaimedTask {
            id: 1,
            config_id: 1,
            record_id: "rec1".to_string(),
            base_token: "base".to_string(),
            table_id: "tbl".to_string(),
            approval_code: "CODE".to_string(),
            applicant_field: "fld_applicant".to_string(),
            applicant_open_id: None,
            backfill_field: "fld_back".to_string(),
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
            timezone_offset: FixedOffset::from_seconds(8 * 3600)
                .unwrap_or_else(|error| panic!("{error}")),
            widgets: Vec::new(),
            form_snapshot: String::new(),
            last_error: last_error.map(str::to_string),
        }
    }

    /// 通知去重：**结论变化才发**。
    ///
    /// 中间态每轮重扫都会产生同一个结论，判据取「本轮原因 == 任务行 last_error
    /// （上一轮写入的原因）」；终态只出现一次（任务退出认领集），总是发。
    /// 这条不变量是「任何结论都要反馈」不变成「每轮重发一条」的唯一闸门。
    #[test]
    fn notification_dedup_fires_only_when_the_conclusion_changes() {
        let waiting = DispatchResult::Waiting {
            reason: "缺少申请人".into(),
        };
        // 首次（任务行还没有 last_error）→ 发。
        assert!(should_notify(&claimed(None), &waiting));
        // 上一轮就是同一个 waiting → 已通知过，不重复。
        assert!(!should_notify(&claimed(Some("缺少申请人")), &waiting));
        // 原因变了（用户补了申请人、又卡在外部选项）→ 是新结论，发。
        assert!(should_notify(
            &claimed(Some("缺少申请人")),
            &DispatchResult::Waiting {
                reason: "外部选项绑定不可用".into(),
            }
        ));

        // retryable 同理。
        let retryable = DispatchResult::Retryable {
            message: "服务不可用".into(),
        };
        assert!(should_notify(&claimed(None), &retryable));
        assert!(!should_notify(&claimed(Some("服务不可用")), &retryable));

        // 终态：无论 last_error 是什么都发（它们只会出现一次）。
        for outcome in [
            DispatchResult::Backfilled {
                serial_number: "SN".into(),
                instance_code: "INST".into(),
            },
            DispatchResult::Terminal {
                message: "配置错误".into(),
            },
        ] {
            assert!(should_notify(&claimed(Some("缺少申请人")), &outcome));
            assert!(should_notify(&claimed(None), &outcome));
        }
    }

    #[test]
    fn http_notification_detection_reads_the_actual_notification_status() {
        // 只有 notification.status == "sent" 才算「HTTP 已通知」——判据必须是
        // 通知结果本身，不是 outcome=succeeded（2026-10-14「没发消息」事故根因）。
        // 查询侧同时覆盖 succeeded 与 failed 两档：outcome_for 把 Terminal 折成
        // failed，而 HTTP 层对 Failed 终态也发通知，只查 succeeded 会漏掉它。
        let mut sent = Record::new();
        sent.insert(
            "response_body",
            serde_json::json!(r#"{"notification":{"status":"sent","message_id":"om_1"}}"#),
        );
        assert!(http_path_notified(&sent));

        let mut not_requested = Record::new();
        not_requested.insert(
            "response_body",
            serde_json::json!(r#"{"notification":{"status":"not_requested"}}"#),
        );
        assert!(
            !http_path_notified(&not_requested),
            "未指定接收人不算已通知"
        );

        let mut failed = Record::new();
        failed.insert(
            "response_body",
            serde_json::json!(r#"{"notification":{"status":"failed","message":"超时"}}"#),
        );
        assert!(!http_path_notified(&failed), "发送失败不算已通知");

        // response_body 为空（失败出口）与不含 notification 的响应体：都不算已通知。
        let mut empty = Record::new();
        empty.insert("response_body", serde_json::Value::Null);
        assert!(!http_path_notified(&empty));
        let mut legacy = Record::new();
        legacy.insert(
            "response_body",
            serde_json::json!(r#"{"serial_number":"202610150001"}"#),
        );
        assert!(!http_path_notified(&legacy));
    }

    #[test]
    fn lease_covers_a_full_claim_batch_at_the_rate_limit() {
        // 租约必须覆盖「认领一批的全部处理时间」，否则处理中的行会被另一轮抢走
        // （uuid 幂等兜得住，但会白烧配额）。
        //
        // CLAIM_BATCH 条 × 每条在默认 90 次/分钟限速下的间隔，×3 给取详情与重试留余量。
        let per_item_ms = crate::addon::feishu::domain::approval_rate_limit::min_interval_ms(
            crate::config::default_approval_create_rate_per_minute(),
        );
        let worst_case_seconds = (CLAIM_BATCH as u64 * per_item_ms / 1_000) * 3;
        assert!(
            ROUND_WORST_CASE_SECONDS as u64 >= worst_case_seconds,
            "租约 {ROUND_WORST_CASE_SECONDS}s 短于最坏处理时间 {worst_case_seconds}s"
        );
    }

    #[test]
    fn claim_batch_stays_within_one_second_window_of_the_rate_limit() {
        // 认领批不该大到「一轮还没跑完下一轮就该接上了」——那会让认领与处理重叠。
        // 90 次/分钟 ≈ 1.5 次/秒，CLAIM_BATCH 条占的秒数应有上界。
        let per_item_ms = crate::addon::feishu::domain::approval_rate_limit::min_interval_ms(90);
        let batch_seconds = CLAIM_BATCH as u64 * per_item_ms / 1_000;
        assert!(
            batch_seconds < 120,
            "认领批 {CLAIM_BATCH} 条在限速下占 {batch_seconds}s，超过 2 分钟"
        );
    }

    #[test]
    fn idle_poll_default_is_reasonable() {
        // 这个循环主要跑异步受理的任务：太长让受理后延迟明显，太短空转烧数据库。
        let interval = crate::config::default_approval_scan_interval_seconds();
        assert!(
            (10..=120).contains(&interval),
            "轮询间隔应在 10~120 秒之间，实际 {interval}"
        );
    }

    #[test]
    fn round_budget_covers_two_full_retry_passes() {
        // 单轮最坏耗时必须给「取详情的重试」留出空间（PULL_RETRY 三次 × 20s 超时），
        // 否则认领的一批还没跑完就进入下一轮，两轮会重叠（单线程下不会真的并行，
        // 但会让一轮的实际时长超过配置的轮询间隔）。
        let policy_seconds = crate::addon::feishu::domain::outbound::PULL_RETRY
            .worst_case_seconds(crate::addon::feishu::domain::outbound::PULL_REQUEST_TIMEOUT_SECS);
        assert!(
            ROUND_WORST_CASE_SECONDS as u64 >= policy_seconds * 2,
            "单轮预算 {ROUND_WORST_CASE_SECONDS}s 覆盖不了两轮完整重试 {policy_seconds}s"
        );
    }
}
