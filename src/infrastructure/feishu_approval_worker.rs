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
//! 批次。状态推进只发生在处理结束时（`record_outcome`），所以崩溃会让一批任务
//! 停在 `pending`（`worker_id` 与租约还在）。
//!
//! 恢复**不靠轮询播种**：轮询对已存在的任务行不做任何事。真正的恢复在
//! `claim_ready` 的过期租约清扫——只读阶段（`effect_phase` 为 `ready`/`created`）
//! 可被直接重新认领；外部写入结果未知（`creating`/`backfilling`）的行被写成
//! `review`，之后靠人工点击 `reopen` 重开。
//!
//! `uuid` 是最后一道防线：即使某条记录被重复处理，飞书也只会回 `60012`，
//! 走回捞路径拿到同一个实例，不会重复建单。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::sync::{mpsc, watch};
use tracing::Instrument;
use yang_base::table::Record;
use yang_base::tools::Tools;
use yang_base::BaseError;
use yang_runtime::shutdown::OwnedTask;

use crate::addon::feishu::approval::domain::task_applicant_table::upsert_task_applicant;
use crate::addon::feishu::domain::approval::instance_applink;
use crate::addon::feishu::domain::approval_convert::{
    amount_currency, format_local, FixedOffset, WidgetMap,
};
use crate::addon::feishu::domain::approval_dispatch::{
    applicant_open_id as extract_applicant_open_id, backfill_in_chunks, dispatch_one,
    widget_maps_from_rows, BitableBackfill, DispatchInput as OrchestrationInput, DispatchResult,
};
use crate::addon::feishu::domain::approval_execution::{self, TaskClaim};
use crate::addon::feishu::domain::approval_feedback;
use crate::addon::feishu::domain::approval_notification::{
    FeedbackReference, NotificationRecipient, ReceiveIdType,
};
use crate::addon::feishu::domain::approval_row_edit;
use crate::addon::feishu::domain::approval_uuid::derive_uuid;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    FailureKind, HttpClientTransport, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::repository::{all_pages, PAGE_SIZE};
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

/// 按 record_id 定向读取任务行时的片大小。
///
/// 与框架 `where_in` 的 IN 列表上限（500）取齐：超了是**拒绝**（`ParamInvalid`），
/// 不是截断。那个常量是 yang-base 的 `pub(crate)`，仓外只能用字面量，
/// 所以「与它同步」只能靠这条注释，而运行期还有下面的页数守卫兜底。
const TASK_LOOKUP_CHUNK: usize = 500;

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

/// 心跳巡检间隔。
const PROGRESS_WATCH_INTERVAL: Duration = Duration::from_secs(30);

/// 超过这个时间没有任何阶段推进，就判定为「停住」并告警。
///
/// 心跳按**阶段**更新（seed / claim / 单条处理 / 落库），正常轮次里两次推进
/// 之间只有秒级；3 分钟没有推进已经不是慢，而是卡在某个 await 上。
const PROGRESS_STALL_AFTER: Duration = Duration::from_secs(180);

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

/// 手动受理信号：`(config_id, 受理发起人 open_id, 单条点击的记录 id)`。
///
/// 第三项 `Some` 表示单条点击，发起人只落到被点的那一行；批量受理为 `None`，
/// 落到本轮播种/重开的所有行（见 [`persist_applicant`]）。
pub type ApprovalDispatchRequest = (i64, Option<String>, Option<String>);

/// 手动触发的共享句柄。
///
/// 照 `FeishuPullHandle` 的先例：注册进 `Tools` 供 Action 取用。
///
/// `pub` 而不是 `pub(crate)`：集成测试（独立 crate）要注入这个句柄才能端到端走
/// dispatch 的「批量受理」出口（`tests/feishu_approval_dispatch_integration.rs`）；
/// 生产装配仍只有 `bootstrap.rs` 一处调用 [`ApprovalDispatchHandle::new`]。
#[derive(Clone)]
pub struct ApprovalDispatchHandle {
    trigger: mpsc::UnboundedSender<ApprovalDispatchRequest>,
}

impl ApprovalDispatchHandle {
    /// 建一对：句柄给 Action，接收端给 worker。
    ///
    /// 无界通道：触发是低频动作，没有一个值得让 HTTP 请求等它的背压。
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ApprovalDispatchRequest>) {
        let (trigger, requests) = mpsc::unbounded_channel();
        (Self { trigger }, requests)
    }

    /// 请求立刻跑一轮全表，并把该配置的受理发起人（已解析成 open_id）先落库。
    ///
    /// `config_id` 标识本次受理命中的配置；`applicant` 为 `None` 时语义与旧行为
    /// 完全一致（只跑一轮，不落发起人）。
    ///
    /// `record_id` 限定发起人落库的作用域：单条点击带**被点的那一条**，发起人只
    /// 写到它自己；批量受理为 `None`，写到本轮播种/重开的所有行。取值越界会把
    /// 一个点击人的身份广播给该配置下所有跨轮待处理的任务（见 [`persist_applicant`]）。
    ///
    /// 接收端已消失（worker 没起或正在退出）时返回错误——静默丢弃会让按钮
    /// 显示「已受理」而实际什么都没发生。
    pub(crate) fn request_dispatch(
        &self,
        config_id: i64,
        applicant: Option<String>,
        record_id: Option<String>,
    ) -> Result<(), BaseError> {
        self.trigger
            .send((config_id, applicant, record_id))
            .map_err(|_| {
                BaseError::ConfigError("审批派发 Worker 未在运行，无法受理全表派发".to_string())
            })
    }
}

/// 审批派发 worker。
pub(crate) struct ApprovalDispatchWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
    /// 心跳巡检：主循环卡住时它仍然在跑，负责把「卡在哪一阶段」写进日志与指标。
    watchdog: OwnedTask<()>,
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
        requests: mpsc::UnboundedReceiver<ApprovalDispatchRequest>,
    ) -> anyhow::Result<Self> {
        let deployment = deployment_namespace(&tools)?;
        let cache = Arc::new(RedisTenantTokenCache::new(
            tools.cache()?.clone(),
            &deployment,
        ));
        let transport = Arc::new(
            HttpClientTransport::new(tools.http()?.clone()).with_approval_create_limit(
                tools.cache()?.clone(),
                settings.app_id.as_deref().unwrap_or_default(),
                settings.approval_create_rate_per_minute,
            ),
        );
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
        let heartbeat = ProgressHeartbeat::default();
        let runner = RoundRunner {
            tools,
            context,
            tokens,
            transport,
            sleeper,
            timezone_offset,
            heartbeat: heartbeat.clone(),
            seed_failures: Arc::default(),
        };
        let idle_poll = Duration::from_secs(settings.approval_scan_interval_seconds);
        let task = tokio::spawn(run_loop(runner, idle_poll, requests, receiver.clone()));
        let watchdog = tokio::spawn(watch_progress(heartbeat, receiver));

        tracing::info!(
            rate_per_minute = settings.approval_create_rate_per_minute,
            idle_poll_seconds = settings.approval_scan_interval_seconds,
            "审批派发 Worker 已启动"
        );
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
            watchdog: OwnedTask::new(watchdog),
        })
    }

    /// 主循环是否已经结束（正常退出或 panic）。用于进程级监督。
    pub(crate) fn has_exited(&self) -> bool {
        self.task.is_finished()
    }

    /// 先广播停止，再由总关闭预算统一等待。
    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// 发信号并等待任务退出。
    pub(crate) async fn shutdown(mut self) -> anyhow::Result<()> {
        self.request_shutdown();
        self.task
            .join()
            .await
            .context("等待审批派发 Worker 退出失败")?;
        self.watchdog
            .join()
            .await
            .context("等待审批派发心跳巡检退出失败")?;
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
    timezone_offset: FixedOffset,
    heartbeat: ProgressHeartbeat,
    /// 播种失败跨轮去重：`config_id → 上次失败的完整文案`。
    ///
    /// 坏表配置每轮都会失败，原实现每 30 秒刷一对 WARN；记住文案后相同失败
    /// 只在首次（或文案变化时）告警，恢复时补一条 INFO（见 [`note_seed_failure`]）。
    seed_failures: Arc<Mutex<HashMap<i64, String>>>,
}

impl RoundRunner {
    fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }
}

/// 最近一次推进的观测点：阶段、任务与时刻。
///
/// 单独抽出来是因为它要在**卡住时**仍然可读：主循环自己无法报告「我卡在
/// 哪个 await」，巡检任务拿这份快照就能说清。`stage` 只接受固定短标签，
/// 避免把任务 id 之外的动态值写进指标基数。
#[derive(Clone, Default)]
struct ProgressHeartbeat {
    state: Arc<Mutex<HeartbeatState>>,
}

#[derive(Default)]
struct HeartbeatState {
    stage: &'static str,
    task_id: Option<i64>,
    at: Option<Instant>,
    /// 本轮停住是否已经告警过，避免每 30 秒刷一条同样的 WARN。
    warned: bool,
}

impl HeartbeatState {
    /// 是否该告警：有观测点、超过阈值、且本轮还没告警过。
    fn is_stalled(&self, threshold: Duration) -> bool {
        !self.warned && self.at.is_some_and(|at| at.elapsed() >= threshold)
    }
}

impl ProgressHeartbeat {
    /// 记录一次推进。恢复推进时补一条 INFO，让「WARN → INFO」在日志里成对。
    fn mark(&self, stage: &'static str, task_id: Option<i64>) {
        let resumed = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let resumed = state.warned;
            state.stage = stage;
            state.task_id = task_id;
            state.at = Some(Instant::now());
            state.warned = false;
            resumed
        };
        metrics::gauge!("feishu_approval_last_progress_timestamp_seconds").set(unix_now_seconds());
        if resumed {
            tracing::info!(stage, task_id, "审批派发 Worker 恢复推进");
        }
    }

    fn mark_warned(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .warned = true;
    }

    fn should_warn(&self, threshold: Duration) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_stalled(threshold)
    }

    /// `(阶段, 任务, 已停滞时长, 是否已告警)`；没有观测点时为 `None`。
    fn snapshot(&self) -> Option<(&'static str, Option<i64>, Duration, bool)> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .at
            .map(|at| (state.stage, state.task_id, at.elapsed(), state.warned))
    }
}

fn unix_now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// 心跳巡检：主循环卡住时它仍然活着，负责把停滞阶段写进日志与指标。
///
/// 这是「进程还活着、worker 不推进」这一类的唯一可观测出口；没有它，readiness
/// 与 `/health/live` 都会保持绿色（D7-3）。
async fn watch_progress(heartbeat: ProgressHeartbeat, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(PROGRESS_WATCH_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
            _ = ticker.tick() => {
                if !heartbeat.should_warn(PROGRESS_STALL_AFTER) {
                    continue;
                }
                let Some((stage, task_id, idle, _)) = heartbeat.snapshot() else {
                    continue;
                };
                heartbeat.mark_warned();
                tracing::warn!(
                    stage,
                    task_id,
                    idle_seconds = idle.as_secs(),
                    "审批派发 Worker 长时间没有推进，卡住阶段已定位"
                );
                metrics::counter!("feishu_approval_worker_stall_total", "stage" => stage).increment(1);
            }
        }
    }
}

/// 本轮是谁触发的。
///
/// 受理请求（用户点按钮）**是唯一的入口**：它带着触发人姓名解析出的 open_id，
/// 由 [`persist_applicant`] 落到任务行，成功时再把 open_id 回写进「发起人」列
/// （`approval_dispatch.rs` 的 D2 传参路径）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoundTrigger {
    /// 受理请求：HTTP Action 收到点击后投的信号。携带**被点击的配置** id——
    /// 「重新点击 = 人工重试」只对该配置生效。
    Manual(i64),
    /// 自动轮询。
    Poll,
}

impl RoundTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual(_) => "manual",
            Self::Poll => "poll",
        }
    }

    /// 能不能为「还没有任务行的记录」建行。
    ///
    /// 只有受理请求能。轮询建行等于「没人点也派发」：那条链路没有触发人可传，
    /// 记录缺申请人时只能报「请填写该人员列」——可那一列本来就是点击时由系统
    /// 回写的，于是用户收到一条点一下就自愈的假警报，而且该行落 `pending`
    /// （`outcome_record` 对 `Waiting` 的映射），每轮轮询都会把它重新处理一遍。
    fn discovers_new_records(self) -> bool {
        matches!(self, Self::Manual(_))
    }

    /// 手动受理指向的配置；轮询为 `None`。
    fn manual_config(self) -> Option<i64> {
        match self {
            Self::Manual(config_id) => Some(config_id),
            Self::Poll => None,
        }
    }
}

/// 本轮是否应对该配置执行「点击即人工重试」的 reopen。
///
/// `seed_tasks` 每轮遍历**所有**启用配置。若按 [`RoundTrigger::discovers_new_records`]
/// 全局重开，点 A 表会把 B 表停在 waiting 的旧任务一并重置——`reopen` 会清空
/// `last_error`，而它是通知去重的判据，于是每次点击都重发 B 表的同一条旧报错
/// （2026-10-28 真机）。人工重试只属于被点击的配置。
fn reopens_on_click(trigger: RoundTrigger, config_id: i64) -> bool {
    trigger.manual_config() == Some(config_id)
}

/// 主循环：等信号或到点，然后跑一轮。
async fn run_loop(
    runner: RoundRunner,
    idle_poll: Duration,
    mut requests: mpsc::UnboundedReceiver<ApprovalDispatchRequest>,
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
            // 第三个字段是单条点击的记录 id（批量为 None），用来限定落库作用域。
            Some((config_id, applicant, record_id)) = requests.recv() => {
                run_round(&runner, &mut cursor, applicant.map(|open_id| (config_id, open_id, record_id)), RoundTrigger::Manual(config_id)).await;
            }
            _ = ticker.tick() => {
                run_round(&runner, &mut cursor, None, RoundTrigger::Poll).await;
            }
        }
    }
}

/// 跑一轮：播种 → （受理发起人落库）→ 认领 → 逐条处理 → 写回结果。
///
/// `applicant` 是手动受理请求携带的 `(config_id, 发起人 open_id, record_id)`；
/// `None` 表示本轮由自动轮询触发，不落发起人。第三项 `record_id` 为 `Some` 时是
/// 单条点击，发起人只落到被点的那一行；为 `None` 时是批量受理，落到本轮播种/重开
/// 的所有行（见 [`persist_applicant`]）。
///
/// # 播种是这条链路的地基
///
/// `claim` 只认领**已存在**的任务行，而任务行从来不是人工建的——所以没有
/// [`seed_tasks`] 的话，队列永远是空的，页面按钮点一次就只受理一个空轮。
///
/// # 崩溃恢复不靠轮询播种
///
/// 状态推进发生在 [`record_outcome`]（逐条、处理结束时）。进程在轮中被
/// `docker stop` 杀掉时，剩下的任务仍是 `pending`（`worker_id` 与租约还在）。
/// 轮询时播种对**已存在**的任务行一律不动（见 [`seed_config`] 的
/// `Some(_) => {}` 分支），人工点击的 `reopen` 白名单是
/// `pending`/`review`/`terminal`/`unnotified`。真正的恢复在 [`claim`] 调用的
/// `claim_ready`：过期租约的只读阶段（`effect_phase` 为 `ready`/`created`）
/// 可直接重新认领；外部写入结果未知（`creating`/`backfilling`）的行被清扫成
/// `review`，之后靠人工点击重开。
#[tracing::instrument(name = "approval_round", skip_all, fields(trigger, cursor_start = cursor.0))]
async fn run_round(
    runner: &RoundRunner,
    cursor: &mut ResumeCursor,
    applicant: Option<(i64, String, Option<String>)>,
    trigger: RoundTrigger,
) {
    let started = std::time::Instant::now();
    tracing::Span::current().record("trigger", trigger.as_str());
    runner.heartbeat.mark("seed", None);
    tracing::debug!(stage = "seed", "审批派发轮次开始，扫描所有启用配置");
    // 单条点击的 record_id：传给播种，让「0 条待处理」通知不发给单条点击
    // （dispatch_single 已经处理了那条记录并给了用户反馈）。
    let clicked_record_id = applicant.as_ref().and_then(|(_, _, rid)| rid.as_deref());
    let report = match seed_tasks(
        runner,
        trigger,
        applicant.as_ref().map(|(_, open_id, _)| open_id.as_str()),
        clicked_record_id,
    )
    .await
    {
        Ok(report) => report,
        // 播种失败不中断整轮：已有的待处理任务仍然值得跑，而播种下一轮还会再来。
        Err(error) => {
            tracing::warn!(error = %error, "审批派发播种失败");
            SeedReport::default()
        }
    };
    if report.seeded > 0 {
        tracing::info!(seeded = report.seeded, "审批派发播种了新任务");
    }
    // 受理请求携带的发起人：**播种之后**落库，本轮新播种/重开的行此刻已是
    // `pending`。放在播种前会漏掉本轮新行——首点会静默回退到表格列（可能是错的
    // 或空的），要再点一次才对。作用域由 `record_id` 与 `report.touched` 双重限定
    // （见 [`persist_applicant`]）。
    if let Some((config_id, open_id, record_id)) = applicant {
        persist_applicant(
            runner,
            config_id,
            &open_id,
            record_id.as_deref(),
            &report.touched,
        )
        .await;
    }
    runner.heartbeat.mark("claim", None);
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
        runner.heartbeat.mark("idle", None);
        tracing::debug!(
            stage = "claim",
            cursor_next = cursor.0,
            duration_ms = started.elapsed().as_millis() as u64,
            "审批派发本轮没有可认领任务，游标已重置"
        );
        return;
    }
    tracing::debug!(count = claimed.len(), "审批派发认领到任务");

    let mut counts = [0usize; 4];
    let mut idled = false;
    let mut row_edits = Vec::new();
    for task in &claimed {
        if idled {
            break;
        }
        // 缺少共享设施时中断；具体名额在每次真实创建出站前领取。
        if runner.tools.cache().is_err() {
            tracing::warn!("Redis 不可用，审批派发中断本轮");
            metrics::counter!(
                "feishu_approval_round_total",
                "result" => "idle",
                "reason" => "redis_unavailable"
            )
            .increment(1);
            idled = true;
            break;
        }

        let (outcome, edit) = async {
            let task_started = std::time::Instant::now();
            runner.heartbeat.mark("process", Some(task.id));
            let processed = process_one(runner, task).await;
            runner.heartbeat.mark("record_outcome", Some(task.id));
            let edit = record_outcome(runner, task, &processed).await;
            tracing::debug!(
                duration_ms = task_started.elapsed().as_millis() as u64,
                "审批任务本轮处理结束"
            );
            (processed.result, edit)
        }
        .instrument(
            tracing::info_span!("approval_task", task_id = task.id, config_id = task.config_id,
            table_id = %task.table_id, record_id = %task.record_id),
        )
        .await;
        if let Some(edit) = edit {
            row_edits.push(edit);
        }
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

    // 行号补写放在**发送之后**：消息已先按无行号出站，这里只扫一次表、逐条编辑
    // 同一条消息。中断（idled）时也要补已收集的消息——它们已经送达。
    if !row_edits.is_empty() {
        runner.heartbeat.mark("row_edit", None);
        approval_row_edit::apply(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            row_edits,
        )
        .await;
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
    runner.heartbeat.mark("idle", None);
    tracing::debug!(
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
    claim: TaskClaim,
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

/// 单条配置的播种结果。
#[derive(Debug, Default)]
struct SeedOutcome {
    seeded: usize,
    reopened: usize,
    skipped: usize,
    /// 本轮**新播种或 reopen** 的 record_id。
    ///
    /// 受理发起人只该落到这些行上：广播到该配置的全部 `pending` 行会把一个点击人
    /// 的身份覆盖到别的记录上（见 [`persist_applicant`]）。
    touched: Vec<String>,
}

/// 一轮播种的汇总结果。
#[derive(Debug, Default)]
struct SeedReport {
    seeded: usize,
    /// 本轮新播种/重开的 `(config_id, record_id)`，跨配置汇总。
    touched: Vec<(i64, String)>,
}

/// 一次播种失败该落哪条日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedFailureNote {
    /// 首次失败或失败文案变了：需要 WARN。
    Warn,
    /// 与上次失败文案相同：已经告过警，本轮降为 DEBUG。
    Repeat,
}

/// 记一次播种失败并判断日志级别（纯判断，不写日志）。
fn note_seed_failure(
    failures: &Mutex<HashMap<i64, String>>,
    config_id: i64,
    message: &str,
) -> SeedFailureNote {
    let mut failures = failures.lock().unwrap_or_else(|error| error.into_inner());
    match failures.get(&config_id) {
        Some(previous) if previous.as_str() == message => SeedFailureNote::Repeat,
        _ => {
            failures.insert(config_id, message.to_owned());
            SeedFailureNote::Warn
        }
    }
}

/// 记一次播种成功：清掉该配置上次的失败文案，返回是否「从失败中恢复」。
fn note_seed_success(failures: &Mutex<HashMap<i64, String>>, config_id: i64) -> bool {
    failures
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&config_id)
        .is_some()
}

/// 播种：把「回填列为空」的多维表格记录变成待处理任务。
///
/// # 判据：回填列为空；配置了批量范围列时还要求该复选框已勾选
///
/// 未配置 `batch_scope_field` 时判据只有一个：回填列是不是空的。
/// 配置后，只有「复选框为 true 且回填列为空」的记录才会被播种——勾选这一位
/// 是用户自己指定的批量提交范围。
///
/// **勾选只在受理/播种时点生效**：已在队列中的任务不因取消勾选而撤回；取消勾选
/// 只影响后续播种（已入队的照旧处理，回填成功后自然退出判据）。
///
/// 这也是使用方的**重试通道**——清空那一格就等于「这条再来一遍」。但轮询**不会**
/// 动已存在的任务行（见 [`seed_config`] 里 `Some(_) => {}` 分支）；只有点击被点
/// 配置时才走 `reopen`，而它的白名单只有 `pending`/`review`/`terminal`/`unnotified`：
///
/// | 库里状态 | 字段为空意味着 | 处置 |
/// |---|---|---|
/// | `pending` | 已在队列里（含崩溃遗留的进行中行） | 轮询不动；点击时重置为 `pending` |
/// | `review` / `unnotified` | 外部写入结果未知 / 结论没送到人 | 点击时拉回 `pending` |
/// | `terminal` | 人工清空要重试 | 点击时拉回 `pending` |
/// | `backfilled` | 编号已落表，重开无意义 | 不重开（`reopen` 白名单不含它） |
///
/// # 为什么要扫多维表格而不是「只看有任务的行」
///
/// 首次运行时库里一条任务都没有——不扫表就永远不会有第一条。
/// （这里说的是**多维表格侧**的扫描；任务行侧已改为按本轮记录定向查询，
/// 见 [`load_tasks_by_record`]。）
async fn seed_tasks(
    runner: &RoundRunner,
    trigger: RoundTrigger,
    applicant: Option<&str>,
    clicked_record_id: Option<&str>,
) -> anyhow::Result<SeedReport> {
    let configs = runner
        .context
        .approval_configs()
        .query()
        .select_fields(&[
            "id",
            "title",
            "base_name",
            "table_name",
            "base_token",
            "table_id",
            "approval_code",
            "backfill_field",
            "batch_scope_field",
        ])?
        .where_eq("enabled", serde_json::json!(true))?;
    let configs = all_pages(configs, SEED_MAX_CONFIG_PAGES).await?;
    tracing::debug!(
        stage = "seed",
        config_count = configs.len(),
        "审批派发读取启用配置完成"
    );

    let mut report = SeedReport::default();
    for config in configs {
        let config_id: i64 = config.require("id").unwrap_or_default();
        match seed_config(runner, &config, trigger, applicant, clicked_record_id).await {
            Ok(outcome) => {
                if note_seed_success(&runner.seed_failures, config_id) {
                    tracing::info!(config_id, "审批派发播种恢复");
                }
                report.seeded += outcome.seeded;
                report.touched.extend(
                    outcome
                        .touched
                        .into_iter()
                        .map(|record_id| (config_id, record_id)),
                );
            }
            // 一条配置坏掉不该拖垮别的配置：记下来，下一轮再来。
            // 坏配置每轮都失败，只在首次/文案变化时告警，其余降 DEBUG，
            // 否则每 30 秒一对 WARN、6 小时 1500+ 条。
            Err(error) => {
                // 用完整错误链（`{:#}`）作去重键：同一根因的文案必须稳定，
                // 换了根因（文案变化）要重新告警。
                let message = format!("{error:#}");
                match note_seed_failure(&runner.seed_failures, config_id, &message) {
                    SeedFailureNote::Warn => {
                        tracing::warn!(error = %message, config_id, "审批派发播种跳过该配置");
                    }
                    SeedFailureNote::Repeat => {
                        tracing::debug!(
                            error = %message,
                            config_id,
                            "审批派发播种仍然失败，文案与上一轮相同（已告警过）"
                        );
                    }
                }
            }
        }
    }
    Ok(report)
}

/// 按 record_id 定向读取某配置下的任务行：`record_id → (task_id, state)`。
///
/// # 为什么不再读全表
///
/// 旧实现用 [`all_pages`] 读该配置的**全部**任务行，页数上界是固定的 20 页
/// （`PAGE_SIZE=100`）：任务行累计到 2000 行后，每一轮播种都在读取这一步失败，
/// 而任务行没有保留期清理，于是该配置的播种**永久**停摆。这里改成只查本轮
/// 待处理记录：行数与本轮记录同量级，与历史任务积累无关。
///
/// # 分片与页数守卫
///
/// `where_in` 的 IN 列表上限是 500（超了是 `ParamInvalid` 拒绝），所以按
/// [`TASK_LOOKUP_CHUNK`] 切片。每片最多命中 `chunk.len()` 行，即最多
/// `chunk.len().div_ceil(PAGE_SIZE)` 个满页；预期再给一页（整页倍数时最后一页
/// 为空）。超了说明 `config_id`/`record_id` 筛选没生效，**报错而不是截断**——
/// 截断会把没读到的行当成「不存在」，进而重复插入、撞唯一索引。
async fn load_tasks_by_record(
    context: &FeishuContext,
    config_id: i64,
    record_ids: &[String],
) -> anyhow::Result<BTreeMap<String, (i64, String)>> {
    // 去重并跳过空串：分片大小按有效 id 算，重复 id 不该占份额。
    let mut unique: Vec<&String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for record_id in record_ids {
        if record_id.is_empty() {
            continue;
        }
        if seen.insert(record_id.as_str()) {
            unique.push(record_id);
        }
    }

    let mut by_record = BTreeMap::new();
    for chunk in unique.chunks(TASK_LOOKUP_CHUNK) {
        let values: Vec<serde_json::Value> = chunk
            .iter()
            .map(|record_id| serde_json::json!(record_id))
            .collect();
        let query = context
            .approval_tasks()
            .query()
            .select_fields(&["id", "record_id", "state"])?
            .where_eq("config_id", serde_json::json!(config_id))?
            .where_in("record_id", values)?;
        let max_pages = chunk.len().div_ceil(PAGE_SIZE) + 1;
        let mut rows: Vec<Record> = Vec::with_capacity(chunk.len());
        for page in 1..=max_pages {
            let batch = query.clone().page(page, PAGE_SIZE)?.all().await?;
            let got = batch.len();
            rows.extend(batch);
            if got < PAGE_SIZE {
                break;
            }
            anyhow::ensure!(
                page < max_pages,
                "按 record_id 读取配置 {config_id} 的任务行超过预期页数：\
                 片大小 {}，{max_pages} 页仍满页",
                chunk.len()
            );
        }
        for row in rows {
            by_record.insert(
                row.require("record_id")?,
                (row.require("id")?, row.require("state")?),
            );
        }
    }
    Ok(by_record)
}

/// 解析批量范围复选框列的当前字段名；未配置返回 `None`。
///
/// 运行期校验与配置期同一纪律：字段必须存在且是复选框（type=7）；类型码缺失时
/// 放行（拿不到元数据就不拒绝本来可用的配置，照 `check_backfill_targets`）。
/// 失败文案必须可行动——点名字段 id 或「应为复选框」。
fn resolve_batch_scope_field_name(
    fields: &[bitable::FieldItem],
    field_id: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let Some(field_id) = field_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let field = fields
        .iter()
        .find(|field| field.field_id == field_id)
        .with_context(|| {
            format!("批量范围复选框列的字段 id {field_id} 在多维表格里不存在（可能已被删除）")
        })?;
    if let Some(field_type) = field.field_type {
        anyhow::ensure!(
            field_type == 7,
            "批量范围列「{}」的类型是 type={field_type}，应为复选框（type=7）",
            field.field_name
        );
    }
    Ok(Some(bitable::resolve_field_name(fields, field_id)?))
}

/// 单条配置的播种。
///
/// 筛选条件是「回填列为空」；配置了批量范围列时再加一条「该复选框已勾选」
/// （见 [`bitable::pending_records_filter`] 与 [`resolve_batch_scope_field_name`]）。
///
/// 任务行的存在性用 [`load_tasks_by_record`] 按**本轮待处理记录**定向查询，
/// 不再读该配置的全量任务历史——行数与本轮待处理记录同量级。
async fn seed_config(
    runner: &RoundRunner,
    config: &Record,
    trigger: RoundTrigger,
    applicant: Option<&str>,
    clicked_record_id: Option<&str>,
) -> anyhow::Result<SeedOutcome> {
    let started = std::time::Instant::now();
    let config_id: i64 = config.require("id")?;
    let base_token: String = config.require("base_token")?;
    let table_id: String = config.require("table_id")?;
    let approval_code: String = config.require("approval_code")?;
    let backfill_field_id: String = config.require("backfill_field")?;
    let batch_scope_field: Option<String> = config.optional("batch_scope_field")?;
    tracing::debug!(stage = "seed", config_id, table_id = %table_id, backfill_field = %backfill_field_id, batch_scope_field = ?batch_scope_field, "审批派发开始扫描回填列为空的记录");

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
    // 批量范围列（可选）：运行期再校验一次存在与类型，配置后字段被删/改类型
    // 会让该配置本轮播种失败并告警，而不是静默扫描全表。
    let batch_scope_name = resolve_batch_scope_field_name(&fields, batch_scope_field.as_deref())?;

    let pending_records = bitable::search_all_records(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
        bitable::pending_records_filter(&backfill_name, batch_scope_name.as_deref()),
        SEED_MAX_PAGES,
    )
    .await
    .map_err(|failure| anyhow::anyhow!("筛选待处理记录失败：{}", failure.message))?;
    tracing::debug!(stage = "seed", config_id, table_id = %table_id, field_count = fields.len(), pending_records = pending_records.len(), "审批派发待处理记录扫描完成");
    if pending_records.is_empty() {
        // 点击受理却命中 0 条：给点击人一条 IM 说明，别让「没勾选就点」完全静默。
        // 但**单条点击**不通知：`dispatch_single` 已经处理了那条记录并给了用户反馈，
        // 这里的全表扫描发现 0 条是因为那条已被处理——再发"批量提交未处理任何记录"
        // 会让用户以为自己的点击走错了路径（2026-10-30 真机）。
        let is_single_click =
            trigger.manual_config() == Some(config_id) && clicked_record_id.is_some();
        if !is_single_click {
            if let Some(open_id) = empty_selection_recipient(trigger, config_id, applicant) {
                notify_empty_selection(
                    runner,
                    config_id,
                    open_id,
                    config,
                    &backfill_name,
                    batch_scope_name.as_deref(),
                )
                .await;
            }
        }
        return Ok(SeedOutcome::default());
    }

    // 已有的任务行：**只查本轮待处理记录**，行数与本轮记录同量级，与历史任务
    // 积累无关（旧实现读全表，任务行到 2000 后整配置播种永久失败）。
    // 按 record_id 建索引，决定「新建」还是（点击时）`reopen`。
    let record_ids: Vec<String> = pending_records
        .iter()
        .map(|record| record.record_id.clone())
        .collect();
    let by_record = load_tasks_by_record(&runner.context, config_id, &record_ids).await?;

    let mut seeded = 0usize;
    let mut reopened = 0usize;
    let mut skipped = 0usize;
    let mut touched: Vec<String> = Vec::new();
    for record in &pending_records {
        if record.record_id.is_empty() {
            continue;
        }
        match by_record.get(&record.record_id) {
            // 受理点击是**被点击配置**的显式人工重试：把不在执行中、停在可重试状态
            // （review / terminal / unnotified / 退避中）的任务重置回 pending，本轮认领
            // 时重跑。uuid 不变，飞书侧按 uuid 幂等，不会重复建单。
            // 只重开本配置：点 A 表不能替 B 表重试，否则 B 的旧报错会随每次点击重发。
            Some((id, _)) if reopens_on_click(trigger, config_id) => {
                if approval_execution::reopen(&runner.context, *id).await? {
                    reopened += 1;
                    touched.push(record.record_id.clone());
                }
            }
            // 轮询不拉起旧行：空单元格不能证明旧请求没有成功，也不能撤销
            // 另一个实例的执行权。
            Some(_) => {}
            // 没有任务行 = 这条记录从没被受理过。轮询不建行（见
            // [`RoundTrigger::discovers_new_records`]）：建了就成「没人点也派发」。
            None if !trigger.discovers_new_records() => skipped += 1,
            None => {
                // 幂等键与派发路径**同源**：同一个 record 在同一配置下只有一条
                // 任务行，重复播种会撞 (config_id, record_id) 复合唯一键而不是
                // 建出第二条。跨配置复用 record_id 是合法情形（复制多维表格会
                // 保留 record_id），所以这里不能用全局唯一键判重。
                let uuid = derive_uuid(&base_token, &table_id, &approval_code, &record.record_id);
                if insert_seeded_task(&runner.context, config_id, &record.record_id, &uuid).await? {
                    seeded += 1;
                    touched.push(record.record_id.clone());
                } else {
                    skipped += 1;
                }
            }
        }
    }
    let outcome = SeedOutcome {
        seeded,
        reopened,
        skipped,
        touched,
    };
    tracing::debug!(
        stage = "seed",
        config_id,
        seeded = outcome.seeded,
        reopened = outcome.reopened,
        // 轮询轮里这些是「没人点过、这轮不建行」的记录——不静默，留在日志里。
        skipped = outcome.skipped,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批配置播种完成"
    );
    Ok(outcome)
}

/// 「点击受理、命中 0 条」反馈的接收人：只有被点击的配置、且拿得到点击人时才发。
///
/// 轮询轮与其它配置的播种都不能发：轮询每 30 秒一轮，「回填列为空」命中 0 条是
/// 常态，发出去就是定时骚扰；其它配置的播种也不是这次点击引发的。
/// 工作流没传 applicant 时没有接收人，宁可不发也不能猜。
fn empty_selection_recipient(
    trigger: RoundTrigger,
    config_id: i64,
    applicant: Option<&str>,
) -> Option<&str> {
    if trigger.manual_config() != Some(config_id) {
        return None;
    }
    applicant
        .map(str::trim)
        .filter(|open_id| !open_id.is_empty())
}

/// 「点击受理、命中 0 条」的 IM 正文。
///
/// 首行必须是【…】：`approval_notification::post_content` 首行作卡片标题，其余
/// 按「标签：内容」渲染成卡片行（「配置」行由调用方拼在标题之后）。
fn empty_selection_feedback(batch_scope_name: Option<&str>, backfill_name: &str) -> String {
    match batch_scope_name {
        Some(scope) => format!(
            "【批量提交未处理任何记录】\n\
             原因：表内没有「{scope}」已勾选且「{backfill_name}」为空的记录，本次点击没有新增提交。\n\
             下一步：请勾选需要提交记录的「{scope}」列后重新点击批量提交。"
        ),
        None => format!(
            "【批量提交未处理任何记录】\n\
             原因：表内没有「{backfill_name}」为空的记录，本次点击没有新增提交。\n\
             下一步：所有记录都已有审批编号；如需重新提交，请清空对应记录的「{backfill_name}」后再点击。"
        ),
    }
}

/// 点击受理命中 0 条时向点击人发一条 IM 说明。
///
/// best-effort，与其它 Worker 通知一致：任何失败只告警，**不返回 Err**，
/// 不中断本轮其余配置的播种。
async fn notify_empty_selection(
    runner: &RoundRunner,
    config_id: i64,
    open_id: &str,
    config: &Record,
    backfill_name: &str,
    batch_scope_name: Option<&str>,
) {
    let title = approval_feedback::display_name(
        config.optional::<String>("title").ok().flatten().as_deref(),
    );
    let base_name = approval_feedback::display_name(
        config
            .optional::<String>("base_name")
            .ok()
            .flatten()
            .as_deref(),
    );
    let table_name = approval_feedback::display_name(
        config
            .optional::<String>("table_name")
            .ok()
            .flatten()
            .as_deref(),
    );

    let body = empty_selection_feedback(batch_scope_name, backfill_name);
    let (header, rest) = body.split_once('\n').unwrap_or((body.as_str(), ""));
    let text = format!("{header}\n配置：{title}\n{rest}");

    // 表链接照 [`send_worker_notification`]：坐标校验失败就不带链接。
    let base_token: String = config
        .optional("base_token")
        .ok()
        .flatten()
        .unwrap_or_default();
    let table_id: String = config
        .optional("table_id")
        .ok()
        .flatten()
        .unwrap_or_default();
    let table_url = (|| -> anyhow::Result<String> {
        bitable::validate_path_segment("base_token", &base_token)?;
        bitable::validate_path_segment("table_id", &table_id)?;
        Ok(format!(
            "https://www.feishu.cn/base/{base_token}?table={table_id}"
        ))
    })()
    .ok();

    let references = vec![FeedbackReference {
        label: "多维表",
        text: format!("{base_name} / {table_name}"),
        href: table_url,
    }];
    let recipient = NotificationRecipient {
        receive_id_type: ReceiveIdType::OpenId,
        receive_id: open_id.to_string(),
    };

    let result = crate::addon::feishu::domain::approval_notification::send_feedback(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &recipient,
        &text,
        &references,
    )
    .await;
    match result.status {
        crate::addon::feishu::domain::approval_notification::NotificationStatus::Sent => {
            tracing::info!(
                config_id,
                open_id,
                message_id = ?result.message_id,
                "审批派发：点击受理命中 0 条，已向点击人发送说明"
            );
        }
        status => {
            tracing::warn!(
                config_id,
                open_id,
                status = ?status,
                message = %result.message,
                "审批派发：点击受理命中 0 条，说明消息未送达"
            );
        }
    }
}

/// 播种一条任务行；插入失败只告警并返回 `false`。
///
/// 单条失败不能中止整轮：否则一条坏记录会让该配置的其余新记录永远不派发。
async fn insert_seeded_task(
    context: &FeishuContext,
    config_id: i64,
    record_id: &str,
    uuid: &str,
) -> anyhow::Result<bool> {
    let mut row = Record::new();
    row.insert("config_id", serde_json::json!(config_id));
    row.insert("record_id", serde_json::json!(record_id));
    row.insert("uuid", serde_json::json!(uuid));
    row.insert("state", serde_json::json!("pending"));
    if let Err(error) = context.approval_tasks().query().insert(row).await {
        tracing::warn!(config_id, record_id, error = %error, "播种任务行失败，跳过该记录");
        return Ok(false);
    }
    Ok(true)
}

/// 把受理请求携带的发起人 open_id 落到**本轮受影响**的任务行（设计 §6.3）。
///
/// # 作用域：只写本轮播种/重开的行
///
/// 旧实现按 `config_id + state='pending'` 取行，会把点击人的身份广播给该配置下
/// **所有**跨轮待处理的任务——以后那些任务会以错误的身份建单、通知发错人、回填
/// 写错申请人。现在作用域由两个条件收窄：
///
/// - `touched`：本轮 [`seed_config`] 新播种或 `reopen` 的 `(config_id, record_id)`；
/// - `record_filter`：单条点击时**被点的那一条**记录 id（批量受理为 `None`）。
///
/// 两者取交集后只对仍为 `state == "pending"` 的行 upsert（覆盖语义不变——同一任务
/// 的发起人以最新一次受理为准）。批量只写本轮播种/重开行，单条只写被点行。
///
/// # 为什么在播种之后
///
/// 按钮受理的语义是「本轮把这些行按受理发起人提单」。新行的任务行由 [`seed_tasks`]
/// 在本轮播种出来，播种前写会**漏掉本轮新行**——首点会静默回退到表格列（可能是错的
/// 或空的），要再点一次才对。所以写在播种之后、认领之前。
///
/// # touched 为空直接返回
///
/// 单条点击若该记录既未播种也未 reopen，admit 已在快照里落了身份，不需要广播；
/// 此时一次查询都不该发。
///
/// # 失败只 warn
///
/// 落库失败不该中断整轮：受理发起人只是**增强**，任务照旧处理（回退表格列），
/// 下一轮按钮或轮询还会再写。
async fn persist_applicant(
    runner: &RoundRunner,
    config_id: i64,
    open_id: &str,
    record_filter: Option<&str>,
    touched: &[(i64, String)],
) {
    let record_ids: Vec<String> = touched
        .iter()
        .filter(|(id, _)| *id == config_id)
        .map(|(_, record_id)| record_id)
        .filter(|record_id| record_filter.map_or(true, |filter| filter == record_id.as_str()))
        .cloned()
        .collect();
    if record_ids.is_empty() {
        return;
    }

    let tasks = match load_tasks_by_record(&runner.context, config_id, &record_ids).await {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::warn!(error = %error, config_id, "读取本轮任务行失败，受理发起人未落库");
            return;
        }
    };
    for (record_id, (task_id, state)) in tasks {
        if state != "pending" {
            continue;
        }
        if let Err(error) = upsert_task_applicant(&runner.context, task_id, open_id).await {
            tracing::warn!(error = %error, task_id, record_id, "受理发起人落库失败");
        }
    }
}

/// 认领待处理任务。
///
/// 判据是 `feishu_approval_task` 的状态，**不是**多维表格的回填字段是否为空
/// ——后者是输出位，用它当扫描键会把「填一半」的行永久钉死（见模块文档）。
///
/// # 只认 `pending`：崩溃遗留由过期租约清扫
///
/// 崩溃留下的行 `state` 仍是 `pending`，但 `worker_id` 与租约还在，所以要靠
/// 租约判断是否还有人持有：`claim_ready` 先把过期租约里外部写入结果未知
/// （`effect_phase` 为 `creating`/`backfilling`）的行写成 `review`（等人工点击
/// 重开），再把过期租约的只读阶段（`ready`/`created`）重新认领。
///
/// **播种不参与这条恢复路径**：它只给没有任务行的记录建行，轮询对已存在的行
/// 不做任何事。
async fn claim(runner: &RoundRunner, cursor: &ResumeCursor) -> anyhow::Result<Vec<ClaimedTask>> {
    let claims =
        approval_execution::claim_ready(&runner.context, cursor.0, None, CLAIM_BATCH).await?;
    let mut tasks = Vec::with_capacity(claims.len());
    for claim in claims {
        let row = runner
            .context
            .approval_tasks()
            .query()
            .where_eq("id", serde_json::json!(claim.id))?
            .optional()
            .await?;
        let Some(row) = row else {
            continue;
        };
        match load_task(runner, &row, claim.clone()).await? {
            Some(task) => tasks.push(task),
            None => {
                let mut row = Record::new();
                row.insert("state", serde_json::json!("unnotified"));
                row.insert(
                    "last_error",
                    serde_json::json!("所属配置不存在、已停用或映射无效"),
                );
                anyhow::ensure!(
                    approval_execution::finish(&runner.context, &claim, &row, false).await?,
                    "认领后失去执行权"
                );
            }
        }
    }
    Ok(tasks)
}

/// 读一条任务的配置与字段映射。
async fn load_task(
    runner: &RoundRunner,
    row: &Record,
    claim: TaskClaim,
) -> anyhow::Result<Option<ClaimedTask>> {
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

    let snapshot = row
        .optional::<String>("dispatch_payload")?
        .map(|payload| serde_json::from_str::<approval_execution::DispatchSnapshot>(&payload))
        .transpose()?;
    // 首次受理快照固定的是**申请人/显式接收人**；配置本身可能在控制台重新保存过
    // （最典型：审批控件改了类型 → 控件 id 也变了）。快照比库里的配置旧时，
    // 定义与映射都必须改用新配置，否则永远拿旧控件 id 撞「映射控件已不在定义中」。
    let snapshot_is_fresh = match snapshot.as_ref() {
        None => true,
        Some(snapshot) => {
            let snapshot_at = Record::from(snapshot.config.clone())
                .optional::<i64>("form_snapshot_at")
                .ok()
                .flatten()
                .unwrap_or_default();
            let live_at = config
                .optional::<i64>("form_snapshot_at")
                .ok()
                .flatten()
                .unwrap_or_default();
            snapshot_at >= live_at
        }
    };
    let config = match snapshot.as_ref().filter(|_| snapshot_is_fresh) {
        Some(snapshot) => Record::from(snapshot.config.clone()),
        None => config,
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
    let applicant_open_id = match snapshot.as_ref() {
        Some(snapshot) => snapshot.applicant.clone(),
        None => match applicant {
            Some(applicant) => applicant.optional::<String>("open_id")?,
            None => None,
        },
    };
    let maps = match snapshot.as_ref().filter(|_| snapshot_is_fresh) {
        Some(snapshot) => snapshot.maps.clone(),
        None => runner
            .context
            .approval_field_maps()
            .query()
            .where_eq("config_id", serde_json::json!(config_id))?
            .all()
            .await?
            .into_iter()
            .map(Record::into_map)
            .collect(),
    };

    // 定义快照提前取：金额控件的币种只能从定义里的 `currencyRange` 派生
    // （字段映射表没有币种列，见 `amount_currency`）。
    let form_snapshot: String = config.optional("form_snapshot")?.unwrap_or_default();
    let Some(widgets) = widget_maps_from_rows(&maps, |widget_id| {
        amount_currency(&form_snapshot, widget_id)
    }) else {
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
        claim,
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
        form_snapshot,
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

/// 只读请求失败的统一处置：`Fatal` 重试修不好，转终态；其余按可重试。
///
/// 记录读取与表结构读取共用这一份映射，避免两处各自演进后漂移。
fn read_failure_result(
    failure: crate::addon::feishu::domain::outbound::OutboundFailure,
    prefix: &str,
) -> DispatchResult {
    let message = format!("{prefix}：{}", failure.message);
    match failure.kind {
        FailureKind::Fatal { .. } => DispatchResult::Terminal { message },
        _ => DispatchResult::Retryable { message },
    }
}

/// 读源记录失败的处置。
///
/// `Fatal`（记录不存在 / 无权限 / 坐标不合法）重试修不好，必须转终态——否则
/// 每轮重发同一条「稍后重试」通知，跑满重试预算后仍是转 review（2026-10-28
/// 真机：记录被删后 5 个任务每 30 秒报一次「取记录被拒或记录不存在」）。
/// 只有 `Retry` / `TokenExpired` 这类瞬态失败值得下一轮再来。
fn record_read_failure(
    failure: crate::addon::feishu::domain::outbound::OutboundFailure,
) -> DispatchResult {
    read_failure_result(failure, "读取记录失败")
}

/// 处理一条记录。
async fn process_one(runner: &RoundRunner, task: &ClaimedTask) -> TaskOutcome {
    let coordinates = BitableCoordinates {
        app_token: task.base_token.clone(),
        table_id: task.table_id.clone(),
        view_id: None,
    };

    tracing::debug!(stage = "fields", applicant_field = %task.applicant_field, backfill_field = %task.backfill_field, widget_count = task.widgets.len(), "审批任务开始读取多维表格结构");

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
        // 与记录读取同一套映射：表不存在/无权限等 `Fatal` 重试修不好，转终态；
        // 凭证/网络抖动等瞬态失败仍按可重试。
        Err(failure) => {
            tracing::warn!(stage = "fields", kind = ?failure.kind, "审批任务读取表结构失败");
            return TaskOutcome::early(read_failure_result(failure, "读取表结构失败"));
        }
    };

    tracing::debug!(
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
                return TaskOutcome::early(record_read_failure(failure));
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

    tracing::debug!(
        stage = "record",
        cell_count = cells.len(),
        "审批源记录字段映射完成"
    );

    let guarded = approval_execution::ExecutionTransport {
        inner: runner.transport(),
        context: &runner.context,
        claim: &task.claim,
    };
    let backfill = BitableBackfill {
        transport: &guarded,
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates: &coordinates,
        fields: &fields,
    };
    let result = dispatch_one(
        &guarded,
        runner.sleeper.as_ref(),
        &runner.tokens,
        &backfill,
        &OrchestrationInput {
            fields: &fields,
            context: Some(&runner.context),
            execution: Some(&task.claim),
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
///
/// `notified` 是「本轮结论**已经送到人**」——由 [`record_outcome`] 在写库**之前**
/// 先发通知得出。它影响两处：终态决定写 `terminal` 还是 `unnotified`；中间态的
/// 失败则给 `last_error` 加「通知未送达」前缀，让 [`should_notify`] 下一轮重试
/// （原样写本轮原因会被当成「上一轮已通知」，失败一次即永久跳过）。
fn outcome_record(outcome: &DispatchResult, notified: bool) -> Record {
    // 结论 → 状态 + 原因：
    //
    // - `backfilled`：编号已写进多维表格，任务完成（**终态**，不再认领）。
    // - `pending`：等待态与可重试。都回 pending——前者要靠用户补齐字段自然重扫，
    //   后者要靠下一轮重试；两者都不该退出认领集。
    // - `terminal`：配置/数据的结构性问题，回填字段已写原因，退出认领集。
    // - `unnotified`：同上，但**结论没能送达到人**。它与 `terminal` 一样退出认领集
    //   （接收人不会凭空出现，留在 `pending` 只是每轮空转重试），但状态本身可筛可数，
    //   是「有人得来看看」的信号——这正是它没有和 `terminal` 合并的理由。
    //
    // 等待态写 `last_error` 只是**可观测**（运维能看到哪些行还缺什么），不影响
    // 认领——它不是状态位。但中间态的通知**没送达**时不能把本轮原因原样写进去：
    // `should_notify` 以「本轮原因 == last_error」判「上一轮已通知」（见 [`should_notify`]），
    // 原样落库会让下一轮永久跳过这条通知（#37：失败一次就再也不重试）。加
    // 「通知未送达：」前缀既是可读的失败标记，又让下一轮比较必然不等 → 重试。
    // 终态不受影响：`unnotified` 仍写原始原因（见下面的测试）。
    let (state, reason) = match outcome {
        DispatchResult::Backfilled { .. } => ("backfilled", None),
        DispatchResult::Waiting { reason } => ("pending", Some(reason.clone())),
        DispatchResult::Retryable { message }
        | DispatchResult::CreatedPendingBackfill { message, .. } => {
            ("pending", Some(message.clone()))
        }
        DispatchResult::Terminal { message } if !notified => ("unnotified", Some(message.clone())),
        DispatchResult::Terminal { message } => ("terminal", Some(message.clone())),
    };
    let notification_failed_interim = state == "pending" && !notified;
    let reason = match (notification_failed_interim, reason) {
        (true, Some(reason)) => Some(format!("通知未送达：{reason}")),
        (_, reason) => reason,
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
///   发送失败的那一轮落的是「通知未送达：原因」前缀（见 [`outcome_record`]），
///   比较必然不等，因此下一轮会重试。
fn should_notify(task: &ClaimedTask, outcome: &DispatchResult) -> bool {
    let interim_reason = match outcome {
        DispatchResult::Waiting { reason } => reason,
        DispatchResult::Retryable { message } => message,
        DispatchResult::CreatedPendingBackfill { message, .. } => message,
        DispatchResult::Backfilled { .. } | DispatchResult::Terminal { .. } => return true,
    };
    task.last_error.as_deref() != Some(interim_reason.as_str())
}

/// 把处理结论落库；返回送达后待补行号的那条消息（没有则为 `None`）。
///
/// # 为什么通知排在写库**之前**
///
/// 终态一旦落库，该行同时退出认领集（`claim` 只认 `pending`）与重扫集
/// （`seed_tasks` 只捞回填列为空的行）——那之后再发现「没人可通知」就再没有补救的
/// 机会：用户永远等不到任何东西，也没人知道。所以先定接收人、先发，再按「到底有没
/// 有送达」决定写 `terminal` 还是 `unnotified`。
///
/// 代价是反过来的那个窗口：发完、还没写库就被杀掉，下一轮会**重发**一次。
/// 重复一条消息远轻于静默丢失一条。
///
/// 补行号的编辑**不在这里做**：发送路径不能同步扫表，收集起来交给
/// `run_round` 在轮末统一处理；本函数只把「已送达且可编辑」的消息带出去。
async fn record_outcome(
    runner: &RoundRunner,
    task: &ClaimedTask,
    processed: &TaskOutcome,
) -> Option<approval_row_edit::RowNumberEdit> {
    let outcome = &processed.result;
    if !approval_execution::is_owned(&runner.context, &task.claim)
        .await
        .unwrap_or(false)
    {
        tracing::warn!(task_id = task.id, "任务执行权已失效，拒绝通知与结果写回");
        return None;
    }

    // 通知：**任何结论都要反馈**（2026-10-15 原则：只要用户发起请求，成功/失败
    // 都要发一条消息，失败附原因）。中间态的重复由 [`should_notify`] 按
    // 「结论是否变化」去重。
    let mut edit = None;
    let notified = if should_notify(task, outcome) {
        // 接收人链：任务表申请人 → 表格申请人列 → **记录创建人**（兜底）。前两者
        // 空着正是 waiting 的主因，而「谁建了这条记录」是那时唯一还能定位到的触发人。
        // 三者都取不到才放弃——那时「发给谁」在事实上无解，但**不能静默**：状态会落成
        // `unnotified`，与 `terminal` 区分得开。
        //
        // 注意「可查」的边界：`list_tasks` 的 `where` 树支持按 `state` 过滤，但控制台
        // 的任务下钻视图目前**只按 config_id 查、没有状态筛选**，所以这些行现在只能
        // 通过接口/SQL 捞，不能指望有人在页面上顺手看见。
        let open_id = task
            .applicant_open_id
            .as_deref()
            .or(processed.applicant_open_id.as_deref())
            .or(processed.creator_open_id.as_deref());
        match open_id {
            Some(open_id) => {
                let notification = send_worker_notification(runner, task, outcome, open_id).await;
                edit = notification.edit;
                notification.notified
            }
            None => {
                metrics::counter!("feishu_approval_notify_total", "result" => "no_recipient")
                    .increment(1);
                tracing::warn!(
                    task_id = task.id,
                    config_id = task.config_id,
                    "Worker 通知：申请人 open_id 与记录创建人均不可用，本轮结论未送达"
                );
                false
            }
        }
    } else {
        // 同一结论上一轮已经通知过（`should_notify` 的去重语义），不算「没送达」。
        // 终态永远走 `should_notify == true` 那一支，所以这里不会把终态误判成已送达。
        true
    };

    let row = outcome_record(outcome, notified);
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
    tracing::debug!(
        stage = "persist",
        outcome = outcome_name,
        state,
        "审批任务处理结论，等待和可重试任务保留 pending；具体原因见阶段日志和任务 last_error"
    );
    let retry = matches!(
        outcome,
        DispatchResult::Retryable { .. } | DispatchResult::CreatedPendingBackfill { .. }
    );
    match approval_execution::finish(&runner.context, &task.claim, &row, retry).await {
        Ok(true) => tracing::debug!(stage = "persist", state, "审批任务状态写回成功"),
        Ok(false) => {
            tracing::warn!(task_id = task.id, "任务执行权已失效，拒绝写回");
            return None;
        }
        Err(error) => {
            tracing::error!(%error, task_id=task.id, "审批任务结果未能持久确认");
            return None;
        }
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

    edit
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

/// 发送结果：是否已送达人，加上「送达后要补行号」的那条消息。
///
/// 补行号单独带出而不是在发送函数里做：发送路径不能同步扫表（点击后的消息要
/// 立即出站），扫描统一由 `run_round` 在一轮末尾按表批量执行。
struct WorkerNotification {
    notified: bool,
    edit: Option<approval_row_edit::RowNumberEdit>,
}

/// Worker 路径的 IM 通知：向触发人发送审批派发结果。
///
/// 返回**本轮结论是否已经送到人**（`notified`）与送达后待补行号的消息
/// （`edit`）——调用方拿 `notified` 决定终态写 `terminal` 还是 `unnotified`
/// （见 [`outcome_record`]）。「HTTP 路径已经发过」算已送达；
/// 查库失败、配置名读不到、发送失败都算未送达（那时确实一条都没出去）。
///
/// 发送失败只 warn，不冒泡。通知是附加价值，不该影响审批结论或重试逻辑。
async fn send_worker_notification(
    runner: &RoundRunner,
    task: &ClaimedTask,
    outcome: &DispatchResult,
    open_id: &str,
) -> WorkerNotification {
    // 去重：只有 HTTP Action **确实发出过通知**才跳过。
    //
    // HTTP 路径通知「显式指定的接收人」（notification_recipient），本 worker
    // 通知「申请人」——两者互补：HTTP 没发时（未指定接收人是常态）由这里兜底，
    // HTTP 发过时才避免重复。
    //
    // 查**全部结论**：是否已通知仍只看 `notification.status == "sent"`
    // （HTTP 层只在终态兜底，`outcome_for` 把 `Terminal` 折成 `failed`；
    // 中间态 HTTP 层按设计不发，由 worker 接手）。同时取最近一条请求记录的
    // `created_at` 作「点击时间」——worker 可能晚几分钟才处理，不能用当前时间。
    let request_log_check = async {
        let query = runner.context.approval_request_logs().query();
        let query = query.where_eq("record_id", serde_json::json!(task.record_id))?;
        let query = query.where_eq("config_id", serde_json::json!(task.config_id))?;
        query.all().await
    };
    let clicked_at = match request_log_check.await {
        Ok(rows) => {
            if rows.iter().any(http_path_notified) {
                tracing::info!(
                    task_id = task.id,
                    config_id = task.config_id,
                    record_id = %task.record_id,
                    "Worker 通知：HTTP Action 已发送过通知，跳过重复通知"
                );
                // 用户**已经收到过**——HTTP 那条就是发给他的。跳过不算未送达。
                return WorkerNotification {
                    notified: true,
                    edit: None,
                };
            }
            rows.iter()
                .filter_map(|row| row.optional::<i64>("created_at").ok().flatten())
                .max()
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                task_id = task.id,
                config_id = task.config_id,
                "Worker 通知：查询请求日志失败，跳过通知"
            );
            return WorkerNotification {
                notified: false,
                edit: None,
            };
        }
    };

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
            return WorkerNotification {
                notified: false,
                edit: None,
            };
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

    // 渲染消息正文（先按「无行号」出站，行号由轮末批量扫描后编辑补上）
    let status = outcome.feedback_status();
    let (accepted, reason, _serial) = outcome.response_parts();
    let _ = accepted; // 通知不关心 HTTP 语义的 accepted
    let clicked_at = clicked_at.map(|seconds| format_local(seconds, task.timezone_offset));
    let message = approval_feedback::render(
        status,
        &reason,
        &names,
        Some(task.record_id.as_str()),
        None,
        clicked_at.as_deref(),
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
            let edit = approval_row_edit::RowNumberEdit::after_send(
                &result,
                status,
                &reason,
                &names,
                &task.record_id,
                clicked_at.as_deref(),
                &task.base_token,
                &task.table_id,
                &references,
            );
            WorkerNotification {
                notified: true,
                edit,
            }
        }
        _ => {
            tracing::warn!(
                task_id = task.id,
                config_id = task.config_id,
                status = ?result.status,
                message = %result.message,
                "Worker 通知：IM 消息发送失败（不影响审批结论）"
            );
            WorkerNotification {
                notified: false,
                edit: None,
            }
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
    fn record_read_failures_are_terminal_only_when_fatal() {
        let failure = |kind| crate::addon::feishu::domain::outbound::OutboundFailure {
            kind,
            message: "记录不存在（可能已被删除）：[\"recGone\"]".into(),
        };
        for fatal in [
            FailureKind::Fatal { code: 0 },
            FailureKind::Fatal { code: 1254302 },
        ] {
            assert!(
                matches!(
                    record_read_failure(failure(fatal)),
                    DispatchResult::Terminal { .. }
                ),
                "{fatal:?} 应转终态，不再按可重试空转"
            );
        }
        for transient in [
            FailureKind::Retry {
                retry_after_seconds: None,
            },
            FailureKind::TokenExpired,
        ] {
            assert!(
                matches!(
                    record_read_failure(failure(transient)),
                    DispatchResult::Retryable { .. }
                ),
                "{transient:?} 应保持可重试"
            );
        }
    }

    /// 播种失败跨轮去重：首次/变化的文案才告警，恢复后重置。
    #[test]
    fn seed_failures_warn_once_until_message_changes_or_recovers() {
        let failures = Mutex::new(HashMap::new());
        let message = "读取表结构失败：字段不存在";
        assert_eq!(
            note_seed_failure(&failures, 7, message),
            SeedFailureNote::Warn,
            "首次失败必须告警"
        );
        assert_eq!(
            note_seed_failure(&failures, 7, message),
            SeedFailureNote::Repeat,
            "同文案每轮重复时不刷 WARN"
        );
        assert_eq!(
            note_seed_failure(&failures, 7, "读取表结构失败：token 失效"),
            SeedFailureNote::Warn,
            "文案变化说明根因变了，必须重新告警"
        );
        assert!(note_seed_success(&failures, 7), "之前失败过应报告恢复");
        assert!(!note_seed_success(&failures, 7), "没有失败记录时不算恢复");
        assert_eq!(
            note_seed_failure(&failures, 7, "读取表结构失败：token 失效"),
            SeedFailureNote::Warn,
            "成功后重置，同一文案再次失败要重新告警"
        );
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库和 Redis DB 15"]
    async fn real_mysql_worker_claims_are_exclusive_and_old_owner_cannot_commit(
    ) -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许测试库");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(
            redis_url.trim_end_matches('/').ends_with("/15"),
            "只允许 Redis DB 15"
        );
        let redis =
            yang_db::RedisClient::connect_with_config(&redis_url, yang_db::RedisConfig::default())
                .await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let mut runners = Vec::new();
        for _ in 0..2 {
            // 两个独立连接池模拟两个应用实例，不能靠进程内锁通过。
            let database = yang_db::Database::connect(&url).await?;
            let context = crate::addon::feishu::build_context(
                Arc::new(database.pool().clone()),
                Arc::new(crate::config::SecuritySettings::default()),
                None,
            )?;
            let transport = Arc::new(HttpClientTransport::new(yang_base::http::HttpClient::new(
                1,
            )?));
            let tokens = Arc::new(TenantTokenProvider::new(
                Arc::new(RedisTenantTokenCache::new(redis.clone(), &namespace)),
                transport.clone(),
                Arc::new(TokioSleeper),
                FeishuCredentials {
                    app_id: "test".into(),
                    app_secret: "test".into(),
                },
                &namespace,
            )?);
            runners.push(RoundRunner {
                tools: Arc::new(yang_base::tools::ToolsBuilder::new().build()?),
                context,
                tokens,
                transport,
                sleeper: Arc::new(TokioSleeper),
                timezone_offset: FixedOffset::from_iana("Asia/Shanghai")?,
                heartbeat: ProgressHeartbeat::default(),
                seed_failures: Arc::default(),
            });
        }
        let pool = runners[0].context.approval_tasks().pool();
        let config_id = sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('认领测试','claimTest',?,'DEF-CLAIM','fldPerson','fldNumber','Asia/Shanghai',true,NOW(),NOW())")
            .bind(format!("tbl{namespace}")).execute(pool).await?.last_insert_id();
        sqlx::query("INSERT INTO feishu_approval_field_map (config_id,widget_id,widget_type,bitable_field,created_at,updated_at) VALUES (?,'widget','input','fldValue',NOW(),NOW())")
            .bind(config_id).execute(pool).await?;
        let task_id = sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,last_error,created_at,updated_at) VALUES (?, ?, ?, 'pending','等待输入',NOW(),NOW())")
            .bind(config_id).bind(format!("rec{namespace}")).bind(&namespace)
            .execute(pool).await?.last_insert_id() as i64;
        let cursor = ResumeCursor(task_id - 1);
        let (one, two) = tokio::join!(claim(&runners[0], &cursor), claim(&runners[1], &cursor));
        let one = one?;
        let two = two?;
        let count = one
            .iter()
            .chain(&two)
            .filter(|task| task.id == task_id)
            .count();
        let old = one
            .iter()
            .chain(&two)
            .find(|task| task.id == task_id)
            .ok_or_else(|| anyhow::anyhow!("应至少认领一次"))?;
        sqlx::query("UPDATE feishu_approval_task SET worker_id='new-owner',last_error='新持有者的结果' WHERE id=?")
            .bind(task_id).execute(pool).await?;
        let _ = record_outcome(
            &runners[0],
            old,
            &TaskOutcome::early(DispatchResult::Waiting {
                reason: "等待输入".into(),
            }),
        )
        .await;
        let saved: String =
            sqlx::query_scalar("SELECT last_error FROM feishu_approval_task WHERE id=?")
                .bind(task_id)
                .fetch_one(pool)
                .await?;
        sqlx::query("DELETE FROM feishu_approval_task WHERE id=?")
            .bind(task_id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_field_map WHERE config_id=?")
            .bind(config_id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_config WHERE id=?")
            .bind(config_id)
            .execute(pool)
            .await?;
        assert_eq!(
            (count, saved.as_str()),
            (1, "新持有者的结果"),
            "同一任务只能由一个实例持有执行权，旧持有者不得覆盖新结果"
        );
        Ok(())
    }

    /// 复制多维表格会保留 record_id：同一 record 在两个配置下必须各能建一条任务
    /// 行，而同一配置内重复播种必须被容错跳过（不能中止整轮）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_seed_task_row_is_unique_per_config_not_globally() -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许测试库");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let database = yang_db::Database::connect(&url).await?;
        let context = crate::addon::feishu::build_context(
            Arc::new(database.pool().clone()),
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        let pool = context.approval_tasks().pool();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let mut configs = Vec::new();
        for suffix in ["a", "b"] {
            let id = sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('播种唯一性测试','appSeed',?,'DEF-SEED','fldPerson','fldNumber','Asia/Shanghai',true,NOW(),NOW())")
                .bind(format!("tbl{namespace}{suffix}"))
                .execute(pool)
                .await?
                .last_insert_id() as i64;
            configs.push(id);
        }
        let record_id = format!("rec{namespace}");
        assert!(
            insert_seeded_task(&context, configs[0], &record_id, &format!("{namespace}a")).await?,
            "首个配置应插入成功"
        );
        assert!(
            insert_seeded_task(&context, configs[1], &record_id, &format!("{namespace}b")).await?,
            "复制表的同一 record_id 在另一配置下必须能建任务"
        );
        assert!(
            !insert_seeded_task(&context, configs[0], &record_id, &format!("{namespace}c")).await?,
            "同一配置重复播种应被容错跳过而不是中止整轮"
        );
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT config_id,record_id FROM feishu_approval_task WHERE record_id=? ORDER BY config_id",
        )
        .bind(&record_id)
        .fetch_all(pool)
        .await?;
        assert_eq!(
            rows,
            vec![
                (configs[0], record_id.clone()),
                (configs[1], record_id.clone())
            ]
        );
        sqlx::query("DELETE FROM feishu_approval_task WHERE record_id=?")
            .bind(&record_id)
            .execute(pool)
            .await?;
        for config_id in configs {
            sqlx::query("DELETE FROM feishu_approval_config WHERE id=?")
                .bind(config_id)
                .execute(pool)
                .await?;
        }
        Ok(())
    }

    #[test]
    fn created_pending_backfill_keeps_instance_and_known_serial_number() {
        for number in [None, Some("SN-001".to_string())] {
            let outcome = DispatchResult::CreatedPendingBackfill {
                message: "编号回填失败".into(),
                instance_code: "INST-001".into(),
                serial_number: number.clone(),
            };
            let row = outcome_record(&outcome, true);
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
        for (outcome, notified, state) in [
            (
                DispatchResult::Backfilled {
                    instance_code: "INST".into(),
                    serial_number: "SN".into(),
                },
                true,
                "backfilled",
            ),
            (
                DispatchResult::Waiting {
                    reason: "补齐数据".into(),
                },
                true,
                "pending",
            ),
            (
                DispatchResult::Retryable {
                    message: "服务不可用".into(),
                },
                true,
                "pending",
            ),
            (
                DispatchResult::Terminal {
                    message: "配置错误".into(),
                },
                true,
                "terminal",
            ),
            // 通知没送出去的终态必须与「已反馈的终态」区分开——这是本次修复的全部
            // 意义所在：两者都退出认领集，但只有前者需要人去捞。
            (
                DispatchResult::Terminal {
                    message: "配置错误".into(),
                },
                false,
                "unnotified",
            ),
        ] {
            assert_eq!(
                outcome_record(&outcome, notified).get("state"),
                Some(&serde_json::json!(state)),
                "notified={notified} 时状态写错"
            );
        }
    }

    /// `unnotified` 与 `terminal` 只有状态值不同：原因、清租约的语义必须一致，
    /// 否则「未通知」的行会因为残留租约被重新捞回来空转。
    #[test]
    fn unnotified_differs_from_terminal_only_in_the_state_value() {
        let outcome = DispatchResult::Terminal {
            message: "映射控件「w1」已不在定义中".into(),
        };
        let notified = outcome_record(&outcome, true);
        let silent = outcome_record(&outcome, false);

        assert_eq!(notified.get("state"), Some(&serde_json::json!("terminal")));
        assert_eq!(silent.get("state"), Some(&serde_json::json!("unnotified")));
        for key in ["last_error", "lease_until"] {
            assert_eq!(
                notified.get(key),
                silent.get(key),
                "{key} 不该随是否送达而变"
            );
        }
        assert_eq!(
            silent.get("last_error"),
            Some(&serde_json::json!("映射控件「w1」已不在定义中")),
            "未通知的行同样要留住原因，否则运维只看到「未通知」不知道为什么"
        );
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
            claim: TaskClaim {
                id: 1,
                owner: "test".into(),
                generation: 1,
                attempts: 0,
            },
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

    /// #37：中间态通知**发送失败**时不能把本轮原因原样写进去重字段，否则下一轮
    /// `should_notify` 会判「上一轮已通知」——失败一次就永久不再重试。
    #[test]
    fn failed_interim_notification_is_retried_on_the_next_round() {
        for waiting in [
            DispatchResult::Waiting {
                reason: "缺少申请人".into(),
            },
            DispatchResult::Retryable {
                message: "服务不可用".into(),
            },
            DispatchResult::CreatedPendingBackfill {
                message: "编号回填失败".into(),
                instance_code: "INST".into(),
                serial_number: None,
            },
        ] {
            let reason = match &waiting {
                DispatchResult::Waiting { reason } => reason.clone(),
                DispatchResult::Retryable { message }
                | DispatchResult::CreatedPendingBackfill { message, .. } => message.clone(),
                DispatchResult::Backfilled { .. } | DispatchResult::Terminal { .. } => {
                    panic!("只测中间态")
                }
            };

            // 首轮发送失败：落库的是失败标记，不是本轮原因本身。
            let failed = outcome_record(&waiting, false);
            let stored = failed
                .get("last_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_ne!(stored, reason, "失败不能伪装成已通知");
            assert!(stored.contains("通知未送达"), "失败标记要可读: {stored}");
            // 下一轮同样的中间态必须重试。
            assert!(
                should_notify(&claimed(Some(stored.as_str())), &waiting),
                "首轮发送失败后，下一轮必须重试: {stored}"
            );

            // 发送成功后落本轮原因 → 下一轮跳过（既有去重语义不回归）。
            let sent = outcome_record(&waiting, true);
            assert_eq!(sent.get("last_error"), Some(&serde_json::json!(reason)));
            assert!(
                !should_notify(&claimed(Some(reason.as_str())), &waiting),
                "发送成功后下一轮不得重发"
            );
        }

        // 终态语义不变：unnotified/terminal 仍只由「是否送达」决定。
        let terminal = DispatchResult::Terminal {
            message: "配置错误".into(),
        };
        assert_eq!(
            outcome_record(&terminal, false).get("state"),
            Some(&serde_json::json!("unnotified"))
        );
        assert_eq!(
            outcome_record(&terminal, true).get("state"),
            Some(&serde_json::json!("terminal"))
        );
        for notified in [true, false] {
            assert_eq!(
                outcome_record(&terminal, notified).get("last_error"),
                Some(&serde_json::json!("配置错误")),
                "终态原因不因是否送达而改写"
            );
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

    /// 造一个字段项；`field_type` 为 `None` 模拟缺失类型码的旧响应。
    fn field_item(field_id: &str, field_name: &str, field_type: Option<i32>) -> bitable::FieldItem {
        bitable::FieldItem {
            is_primary: false,
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type,
            ui_type: None,
            property: None,
        }
    }

    #[test]
    fn batch_scope_field_resolves_only_checkbox_columns() {
        let fields = [
            field_item("fldScope", "纳入批量", Some(7)),
            field_item("fldText", "备注", Some(1)),
        ];
        assert_eq!(
            resolve_batch_scope_field_name(&fields, Some("fldScope"))
                .unwrap_or_else(|error| panic!("复选框应可解析: {error}")),
            Some("纳入批量".to_string())
        );
        // 未配置 / 空串 / 纯空白 → 不启用。
        for absent in [None, Some(""), Some("   ")] {
            assert_eq!(
                resolve_batch_scope_field_name(&fields, absent)
                    .unwrap_or_else(|error| panic!("未配置不该报错: {error}")),
                None,
                "{absent:?} 应视为未配置"
            );
        }
        // 文本列被拒，文案要指出应为复选框。
        let error = resolve_batch_scope_field_name(&fields, Some("fldText"))
            .err()
            .unwrap_or_else(|| panic!("文本列必须被拒"));
        assert!(
            error.to_string().contains("type=7"),
            "要说明应为复选框: {error}"
        );
        // 字段被删 → 点名 id。
        let error = resolve_batch_scope_field_name(&fields, Some("fldGone"))
            .err()
            .unwrap_or_else(|| panic!("缺字段必须被拒"));
        assert!(error.to_string().contains("fldGone"), "实际: {error}");
        // 类型码缺失时放行，只解析名字。
        let unknown = [field_item("fldScope", "纳入批量", None)];
        assert_eq!(
            resolve_batch_scope_field_name(&unknown, Some("fldScope"))
                .unwrap_or_else(|error| panic!("类型码缺失不应拦: {error}")),
            Some("纳入批量".to_string())
        );
    }

    #[test]
    fn empty_selection_recipient_gates_on_click_and_applicant() {
        // 只有「点击受理、点的是本配置」且有发起人才反馈：轮询每轮都可能命中 0 条，
        // 其它配置也不是这次点击引发的，都会变成定时骚扰。
        assert_eq!(
            empty_selection_recipient(RoundTrigger::Manual(3), 3, Some("ou_x")),
            Some("ou_x")
        );
        assert_eq!(
            empty_selection_recipient(RoundTrigger::Poll, 3, Some("ou_x")),
            None
        );
        assert_eq!(
            empty_selection_recipient(RoundTrigger::Manual(3), 4, Some("ou_x")),
            None
        );
        assert_eq!(
            empty_selection_recipient(RoundTrigger::Manual(3), 3, None),
            None
        );
        assert_eq!(
            empty_selection_recipient(RoundTrigger::Manual(3), 3, Some("  ")),
            None
        );
    }

    #[test]
    fn empty_selection_feedback_names_scope_and_backfill() {
        let scoped = empty_selection_feedback(Some("纳入批量"), "审批编号");
        assert!(
            scoped.starts_with("【批量提交未处理任何记录】"),
            "首行是卡片标题: {scoped}"
        );
        assert!(
            scoped.contains("纳入批量") && scoped.contains("勾选"),
            "要说明范围列没勾选: {scoped}"
        );
        assert!(scoped.contains("审批编号"), "要点名回填列: {scoped}");

        let unscoped = empty_selection_feedback(None, "审批编号");
        assert!(unscoped.starts_with("【批量提交未处理任何记录】"));
        assert!(!unscoped.contains("纳入批量"), "未配置范围列不该提它");
        assert!(unscoped.contains("审批编号"));
    }

    #[test]
    fn single_click_does_not_notify_empty_selection() {
        // 单条点击时 dispatch_single 已经处理了那条记录并给了用户反馈。
        // 播种的全表扫描发现 0 条是因为那条已被处理——再发"批量提交未处理任何记录"
        // 会让用户以为自己的点击走错了路径（2026-10-30 真机）。
        let is_single_click =
            |trigger: RoundTrigger, config_id: i64, clicked_record_id: Option<&str>| {
                trigger.manual_config() == Some(config_id) && clicked_record_id.is_some()
            };
        // 单条点击：本配置 + 有 record_id → 不发
        assert!(is_single_click(RoundTrigger::Manual(5), 5, Some("recXXX")));
        // 批量点击：本配置 + record_id = None → 发（原有的空选反馈）
        assert!(!is_single_click(RoundTrigger::Manual(5), 5, None));
        // 其它配置的播种：即使有 record_id 也不发（不属于本次点击）
        assert!(!is_single_click(RoundTrigger::Manual(5), 3, Some("recXXX")));
        // 轮询：不发（原有逻辑）
        assert!(!is_single_click(RoundTrigger::Poll, 5, Some("recXXX")));
    }

    #[test]
    fn only_a_click_discovers_records_that_have_no_task_row_yet() {
        // 自动轮询不能发现新记录。发现了就会在没有触发人的前提下派发：记录缺申请人
        // 时只能报「请填写该人员列」，可那一列本来就是点击时由系统回写的——
        // 用户会收到一条自愈的假警报（2026-10-16 实测：批量前 30 秒的轮询先报了两条
        // 「需要修正数据」，点击后两条「已创建」）。
        assert!(RoundTrigger::Manual(1).discovers_new_records());
        assert!(!RoundTrigger::Poll.discovers_new_records());
    }

    #[test]
    fn manual_retry_reopens_only_the_clicked_config() {
        // 点 A 表的批量按钮会播种所有启用配置，但「人工重试」只属于被点的配置：
        // 否则 B 表停在 waiting 的旧任务会被重置，清空的 last_error 让通知去重失效，
        // 每次点击都重发 B 的旧报错。
        assert!(reopens_on_click(RoundTrigger::Manual(4), 4));
        assert!(!reopens_on_click(RoundTrigger::Manual(4), 2));
        assert!(!reopens_on_click(RoundTrigger::Poll, 4));
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

    #[test]
    fn heartbeat_marks_the_stage_and_flags_a_stall_once() {
        let heartbeat = ProgressHeartbeat::default();
        assert!(heartbeat.snapshot().is_none(), "未推进前没有观测点");

        heartbeat.mark("process", Some(7));
        let (stage, task_id, idle, warned) = heartbeat
            .snapshot()
            .unwrap_or_else(|| panic!("mark 后必须有快照"));
        assert_eq!((stage, task_id), ("process", Some(7)));
        assert!(!warned);
        assert!(idle < PROGRESS_STALL_AFTER);

        // 直接把时刻拨到阈值之外，避免测试真的等 3 分钟。
        {
            let mut state = heartbeat
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.at = Some(Instant::now() - PROGRESS_STALL_AFTER);
        }
        let (_, _, idle, warned) = heartbeat.snapshot().unwrap_or_else(|| panic!("有快照"));
        assert!(!warned);
        assert!(idle >= PROGRESS_STALL_AFTER);
        {
            let state = heartbeat
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            assert!(state.is_stalled(PROGRESS_STALL_AFTER));
        }

        // 同一轮只告警一次；恢复推进时重置标记。
        heartbeat.mark_warned();
        {
            let state = heartbeat
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            assert!(!state.is_stalled(PROGRESS_STALL_AFTER), "同一轮不重复告警");
        }
        heartbeat.mark("idle", None);
        let (stage, task_id, _, warned) = heartbeat.snapshot().unwrap_or_else(|| panic!("有快照"));
        assert_eq!((stage, task_id), ("idle", None));
        assert!(!warned, "恢复推进后重置告警标记");
    }

    // -----------------------------------------------------------------------
    // 定向任务查询 / 受理发起人作用域（真实 MySQL）
    // -----------------------------------------------------------------------

    /// 连接测试库、跑与应用启动相同的 schema 同步，并构造上下文。
    async fn real_mysql_context() -> anyhow::Result<(yang_db::Database, Arc<FeishuContext>)> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许测试库");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let database = yang_db::Database::connect(&url).await?;
        let context = crate::addon::feishu::build_context(
            Arc::new(database.pool().clone()),
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        Ok((database, context))
    }

    /// 批量插入任务行（多行 INSERT，避免两千次往返）。
    async fn insert_task_rows(
        pool: &sqlx::MySqlPool,
        config_id: i64,
        record_ids: &[String],
        uuid_prefix: &str,
    ) -> anyhow::Result<()> {
        for (chunk_index, chunk) in record_ids.chunks(500).enumerate() {
            let placeholders = vec!["(?,?,?,'pending',NOW(),NOW())"; chunk.len()].join(",");
            let sql = format!(
                "INSERT INTO feishu_approval_task \
                 (config_id,record_id,uuid,state,created_at,updated_at) VALUES {placeholders}"
            );
            let mut query = sqlx::query(&sql);
            for (offset, record_id) in chunk.iter().enumerate() {
                query = query
                    .bind(config_id)
                    .bind(record_id)
                    .bind(format!("{uuid_prefix}-{}", chunk_index * 500 + offset));
            }
            query.execute(pool).await?;
        }
        Ok(())
    }

    async fn insert_single_task(
        pool: &sqlx::MySqlPool,
        config_id: i64,
        record_id: &str,
        uuid: &str,
        state: &str,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query(
            "INSERT INTO feishu_approval_task \
             (config_id,record_id,uuid,state,created_at,updated_at) VALUES (?,?,?,?,NOW(),NOW())",
        )
        .bind(config_id)
        .bind(record_id)
        .bind(uuid)
        .bind(state)
        .execute(pool)
        .await?
        .last_insert_id() as i64)
    }

    async fn applicant_of(pool: &sqlx::MySqlPool, task_id: i64) -> anyhow::Result<Option<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT open_id FROM feishu_approval_task_applicant WHERE task_id=?",
        )
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .map_err(anyhow::Error::from)
    }

    async fn insert_lookup_config(
        pool: &sqlx::MySqlPool,
        namespace: &str,
        suffix: &str,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('定向查询测试','appLookup',?,'DEF-LOOKUP','fldPerson','fldNumber','Asia/Shanghai',true,NOW(),NOW())")
            .bind(format!("tbl{namespace}{suffix}"))
            .execute(pool)
            .await?
            .last_insert_id() as i64)
    }

    /// 旧实现按 `config_id` 读该配置的**全部**任务行：跑满 20 页（2000 行）即报错，
    /// 而任务行没有保留期清理 ⇒ 该配置的播种永久停摆。
    ///
    /// 新实现按本轮 record_id 定向查询，行数与历史任务量无关。这里覆盖：
    /// - 1999 / 2000 / 2001 / 2100 行历史 + 1 条目标记录都能命中（2000 是回归关键，
    ///   2100 是「整页倍数无短页」）；
    /// - 分片边界 499 / 500 / 501 / 1000 / 1001（500 是 `where_in` 的 IN 列表上限）；
    /// - 重复 / 空串 record_id 不改变结果；
    /// - 另一配置占用同一 record_id 时本配置结果不受影响。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_seed_existing_lookup_survives_large_task_history() -> anyhow::Result<()> {
        let (_database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_lookup_config(pool, &namespace, "a").await?;
        let other_config_id = insert_lookup_config(pool, &namespace, "b").await?;

        let mut history: Vec<String> = Vec::new();
        let mut total_rows = 0usize;
        for total in [1999usize, 2000, 2001, 2100] {
            // 让该配置的任务行**总数**恰好到 total，其中最后一条是本轮目标记录。
            let need = total - total_rows - 1;
            let start = history.len();
            let new_ids: Vec<String> = (start..start + need)
                .map(|index| format!("{namespace}-history-{index}"))
                .collect();
            insert_task_rows(pool, config_id, &new_ids, &format!("{namespace}-h{total}")).await?;
            history.extend(new_ids);
            total_rows += need;

            let target = format!("{namespace}-target-{total}");
            insert_task_rows(
                pool,
                config_id,
                std::slice::from_ref(&target),
                &format!("{namespace}-t{total}"),
            )
            .await?;
            total_rows += 1;
            assert_eq!(total_rows, total, "历史行数构造错误");

            let found =
                load_tasks_by_record(&context, config_id, std::slice::from_ref(&target)).await?;
            let expected_id: i64 = sqlx::query_scalar(
                "SELECT id FROM feishu_approval_task WHERE config_id=? AND record_id=?",
            )
            .bind(config_id)
            .bind(&target)
            .fetch_one(pool)
            .await?;
            assert_eq!(
                found.get(&target),
                Some(&(expected_id, "pending".to_string())),
                "{total} 行历史后仍应命中本轮记录（旧实现从 2000 行起整配置播种必败）"
            );
        }

        // 2100 行历史下「本轮只新增这一条」仍成立：插入与历史任务量无关。
        let fresh = format!("{namespace}-fresh");
        let before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE config_id=?")
                .bind(config_id)
                .fetch_one(pool)
                .await?;
        assert!(
            insert_seeded_task(&context, config_id, &fresh, &format!("{namespace}-fresh")).await?,
            "2100 行历史后新记录仍应能播种"
        );
        let after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE config_id=?")
                .bind(config_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(after, before + 1, "本轮只应新增这一条任务行");

        // 分片边界：每片 ≤ 500（`where_in` 上限），翻页守卫不得误报。
        for size in [499usize, 500, 501, 1000, 1001] {
            let ids = &history[..size];
            let found = load_tasks_by_record(&context, config_id, ids).await?;
            assert_eq!(found.len(), size, "片大小 {size} 必须全部命中");
            for record_id in ids {
                assert_eq!(
                    found.get(record_id).map(|(_, state)| state.as_str()),
                    Some("pending"),
                    "记录 {record_id} 的状态读错"
                );
            }

            // 重复与空串：去重/跳过之后结果集不变，也不会占掉分片名额。
            let mut noisy = ids.to_vec();
            noisy.push(ids[0].clone());
            noisy.push(String::new());
            let found = load_tasks_by_record(&context, config_id, &noisy).await?;
            assert_eq!(found.len(), size, "重复/空串 id 不该改变结果集");
        }

        // 跨配置复用 record_id 是合法情形（复制 Base 会保留）：本配置只该读到自己的行。
        let shared = format!("{namespace}-shared");
        let own_id = insert_single_task(
            pool,
            config_id,
            &shared,
            &format!("{namespace}-sa"),
            "pending",
        )
        .await?;
        let foreign_id = insert_single_task(
            pool,
            other_config_id,
            &shared,
            &format!("{namespace}-sb"),
            "pending",
        )
        .await?;
        let found =
            load_tasks_by_record(&context, config_id, std::slice::from_ref(&shared)).await?;
        assert_eq!(
            found.get(&shared),
            Some(&(own_id, "pending".to_string())),
            "按 config_id 收窄后不应读到另一配置的同 record_id 行"
        );
        assert!(
            found.values().all(|(id, _)| *id != foreign_id),
            "另一配置的任务行不得串进结果"
        );

        sqlx::query("DELETE FROM feishu_approval_task WHERE config_id IN (?,?)")
            .bind(config_id)
            .bind(other_config_id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_config WHERE id IN (?,?)")
            .bind(config_id)
            .bind(other_config_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// 假 token 缓存：本组用例不碰飞书网络，只需 provider 能构造。
    struct FakeTokenCache;

    #[async_trait::async_trait]
    impl crate::addon::feishu::domain::tenant_token::TenantTokenCache for FakeTokenCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("t-fake".to_string()))
        }
        async fn put(&self, _token: &str, _ttl_seconds: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn invalidate(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn acquire_lock(&self, _owner: &str, _ttl_seconds: i64) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn release_lock(&self, _owner: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// 单条点击的受理发起人只能落到**被点的那一行**；批量只能落到**本轮播种/重开**
    /// 的行。旧实现按 `config_id + state='pending'` 广播，会把一个点击人的身份覆盖
    /// 到该配置下所有跨轮待处理任务上（建单身份、通知接收人、回填申请人全部出错）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_persist_applicant_scopes_to_touched_records() -> anyhow::Result<()> {
        let (_database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_lookup_config(&pool, &namespace, "a").await?;
        let other_config_id = insert_lookup_config(&pool, &namespace, "b").await?;

        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        let r3 = format!("{namespace}-r3");
        let r4 = format!("{namespace}-r4");
        let r5 = format!("{namespace}-r5");
        let r1_id =
            insert_single_task(&pool, config_id, &r1, &format!("{namespace}-r1"), "pending")
                .await?;
        let r2_id =
            insert_single_task(&pool, config_id, &r2, &format!("{namespace}-r2"), "pending")
                .await?;
        let r3_id =
            insert_single_task(&pool, config_id, &r3, &format!("{namespace}-r3"), "pending")
                .await?;
        let r4_id = insert_single_task(
            &pool,
            config_id,
            &r4,
            &format!("{namespace}-r4"),
            "terminal",
        )
        .await?;
        let r5_id = insert_single_task(
            &pool,
            other_config_id,
            &r5,
            &format!("{namespace}-r5"),
            "pending",
        )
        .await?;
        // R2 是跨轮 pending 的旧任务，已有自己的发起人。
        sqlx::query(
            "INSERT INTO feishu_approval_task_applicant (task_id, open_id) VALUES (?, 'ou_old')",
        )
        .bind(r2_id)
        .execute(&pool)
        .await?;

        let transport = Arc::new(HttpClientTransport::new(yang_base::http::HttpClient::new(
            1,
        )?));
        let tokens = Arc::new(TenantTokenProvider::new(
            Arc::new(FakeTokenCache),
            Arc::clone(&transport) as Arc<dyn OutboundTransport>,
            Arc::new(TokioSleeper),
            FeishuCredentials {
                app_id: "test".into(),
                app_secret: "test".into(),
            },
            &namespace,
        )?);
        let runner = RoundRunner {
            tools: Arc::new(yang_base::tools::ToolsBuilder::new().build()?),
            context: Arc::clone(&context),
            tokens,
            transport,
            sleeper: Arc::new(TokioSleeper),
            timezone_offset: FixedOffset::from_iana("Asia/Shanghai")?,
            heartbeat: ProgressHeartbeat::default(),
            seed_failures: Arc::default(),
        };

        // 批量受理（filter=None）：只写本轮 touched（R1/R3）。R2 虽同为 pending、
        // R4 虽然被 touched，都不该被写；另一配置的 R5 也不该被写。
        persist_applicant(
            &runner,
            config_id,
            "ou_new",
            None,
            &[
                (config_id, r1.clone()),
                (config_id, r3.clone()),
                (config_id, r4.clone()),
                (other_config_id, r5.clone()),
            ],
        )
        .await;
        assert_eq!(
            applicant_of(&pool, r1_id).await?,
            Some("ou_new".to_string())
        );
        assert_eq!(
            applicant_of(&pool, r3_id).await?,
            Some("ou_new".to_string())
        );
        assert_eq!(
            applicant_of(&pool, r2_id).await?,
            Some("ou_old".to_string()),
            "跨轮 pending 的旧任务不得被本轮点击人的身份覆盖"
        );
        assert_eq!(
            applicant_of(&pool, r4_id).await?,
            None,
            "非 pending 行不写发起人"
        );
        assert_eq!(
            applicant_of(&pool, r5_id).await?,
            None,
            "另一配置的行不写发起人"
        );

        // 单条点击（filter=Some(R1)）：只写 R1，R3 保持上一轮的值。
        persist_applicant(
            &runner,
            config_id,
            "ou_second",
            Some(&r1),
            &[(config_id, r1.clone()), (config_id, r3.clone())],
        )
        .await;
        assert_eq!(
            applicant_of(&pool, r1_id).await?,
            Some("ou_second".to_string())
        );
        assert_eq!(
            applicant_of(&pool, r3_id).await?,
            Some("ou_new".to_string()),
            "单条点击不得改写其它记录"
        );

        // 单条点击的记录不在 touched 集合：零写入。
        persist_applicant(
            &runner,
            config_id,
            "ou_wrong",
            Some(&r2),
            &[(config_id, r1.clone()), (config_id, r3.clone())],
        )
        .await;
        assert_eq!(
            applicant_of(&pool, r2_id).await?,
            Some("ou_old".to_string()),
            "被点记录不在本轮 touched 时不得广播/写入"
        );
        assert_eq!(
            applicant_of(&pool, r1_id).await?,
            Some("ou_second".to_string())
        );

        // 其它配置的 touched 条目即使带着本配置也存在的 record_id，也不能落库：
        // 查询本身按 config_id 收口，但这条断言钉的是「先按 config 过滤」这层意图，
        // 防止将来把查询的 config 条件挪走后跨配置记录借同 id 波及本配置的任务行。
        persist_applicant(
            &runner,
            config_id,
            "ou_foreign",
            None,
            &[(other_config_id, r1.clone())],
        )
        .await;
        assert_eq!(
            applicant_of(&pool, r1_id).await?,
            Some("ou_second".to_string()),
            "其它配置的 touched 条目不得写本配置的行"
        );

        // touched 为空：直接返回，零查询零写入。
        persist_applicant(&runner, config_id, "ou_empty", None, &[]).await;
        assert_eq!(
            applicant_of(&pool, r2_id).await?,
            Some("ou_old".to_string())
        );
        assert_eq!(
            applicant_of(&pool, r3_id).await?,
            Some("ou_new".to_string())
        );

        // upsert 的覆盖语义保留：同一任务再次被 touch 时以最新受理人为准。
        persist_applicant(
            &runner,
            config_id,
            "ou_batch",
            None,
            &[(config_id, r2.clone())],
        )
        .await;
        assert_eq!(
            applicant_of(&pool, r2_id).await?,
            Some("ou_batch".to_string()),
            "同一任务的最新受理人应覆盖旧值"
        );

        sqlx::query("DELETE FROM feishu_approval_task_applicant WHERE task_id IN (?,?,?,?,?)")
            .bind(r1_id)
            .bind(r2_id)
            .bind(r3_id)
            .bind(r4_id)
            .bind(r5_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_task WHERE config_id IN (?,?)")
            .bind(config_id)
            .bind(other_config_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_config WHERE id IN (?,?)")
            .bind(config_id)
            .bind(other_config_id)
            .execute(&pool)
            .await?;
        Ok(())
    }
}
