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
use crate::addon::feishu::domain::approval::GetOutcome;
use crate::addon::feishu::domain::approval_aggregation::{
    self, group_uuid, members_all_selected, plan_groups, signature_hash, GroupCandidate,
};
use crate::addon::feishu::domain::approval_attachment::{preflight, source_identities, transfer};
use crate::addon::feishu::domain::approval_convert::{
    amount_currency, build_signature, format_local, FixedOffset, FormInput, WidgetMap,
};
use crate::addon::feishu::domain::approval_dispatch::{
    backfill_in_chunks, dispatch_group, dispatch_one, widget_maps_from_rows, BitableBackfill,
    DispatchInput as OrchestrationInput, DispatchResult, GroupDispatchInput, GroupMemberInput,
};
use crate::addon::feishu::domain::approval_execution::{self, TaskClaim};
use crate::addon::feishu::domain::approval_feedback;
use crate::addon::feishu::domain::approval_notification::{
    FeedbackReference, NotificationRecipient, ReceiveIdType,
};
use crate::addon::feishu::domain::approval_provision::resolve_external_options;
use crate::addon::feishu::domain::approval_row_edit;
use crate::addon::feishu::domain::approval_uuid::derive_uuid;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    dedup_key, FailureKind, HttpClientTransport, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::repository::{all_pages, PAGE_SIZE};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;
use crate::infrastructure::audit;
use yang_db::Transaction;

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
    transport: Arc<dyn OutboundTransport>,
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
                let round = run_round(
                    &runner,
                    &mut cursor,
                    applicant.map(|open_id| (config_id, open_id, record_id)),
                    RoundTrigger::Manual(config_id),
                );
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("审批派发 Worker 收到关闭信号，已取消进行中的轮次");
                    return;
                }
            }
            _ = ticker.tick() => {
                let round = run_round(&runner, &mut cursor, None, RoundTrigger::Poll);
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("审批派发 Worker 收到关闭信号，已取消进行中的轮次");
                    return;
                }
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
/// `Some(_) => {}` 分支），人工点击的 `reopen` 覆盖
/// `pending`/`review`/`terminal`/`unnotified` 与「回填列已被清空」的
/// `backfilled`（重新回填原实例）。真正的恢复在 [`claim`] 调用的
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
    // 点击上下文（配置、点击人、被点记录）在下面被 move 走，这里先留一份给轮末汇总。
    let click = applicant.as_ref().cloned();
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
            // 整轮扫描失败时把原因带进汇总：否则点击人会以为「没有待处理记录」。
            let failed = trigger
                .manual_config()
                .map(|config_id| {
                    vec![(
                        config_id,
                        format!("{error:#}")
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                    )]
                })
                .unwrap_or_default();
            SeedReport {
                failed,
                ..SeedReport::default()
            }
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
            // 认领失败也要把「本轮到此为止」告诉点击人。
            notify_round_summary(runner, trigger, click.as_ref(), &report, &[]).await;
            return;
        }
    };
    // 队列深度：**告警的表达式**。连续为 0 说明没有积压；持续为 `CLAIM_BATCH`
    // 说明一批被处理掉了但还有——积压本身由 `idled` 分支的 gauge 兜底。
    metrics::gauge!("feishu_approval_pending").set(claimed.len() as f64);
    if claimed.is_empty() {
        cursor.advance(&claimed);
        runner.heartbeat.mark("idle", None);
        // 零认领也要发汇总：手动点击的人不能等一条永远不来的消息。
        notify_round_summary(runner, trigger, click.as_ref(), &report, &[]).await;
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
    // 被点击配置的逐条结果，供轮末汇总使用（见 [`notify_round_summary`]）。
    let mut entries: Vec<(String, String)> = Vec::new();
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
            // 组分支自己完成了落库与通知（`persisted`）：通用 `record_outcome`
            // 的阶段白名单收不了 `lookup`/`creating` 的组尾，还会把 `paused`
            // 经 `Waiting→pending` 拉回轮询（设计 §4.6 第 10 步）。
            let edit = if processed.persisted {
                None
            } else {
                runner.heartbeat.mark("record_outcome", Some(task.id));
                record_outcome(runner, task, &processed).await
            };
            tracing::debug!(
                duration_ms = task_started.elapsed().as_millis() as u64,
                "审批任务本轮处理结束"
            );
            (processed, edit)
        }
        .instrument(
            tracing::info_span!("approval_task", task_id = task.id, config_id = task.config_id,
            table_id = %task.table_id, record_id = %task.record_id),
        )
        .await;
        if let Some(edit) = edit {
            row_edits.push(edit);
        }
        let group_size = outcome.group_size;
        let outcome = outcome.result;
        counts[match outcome {
            DispatchResult::Backfilled { .. } => 0,
            DispatchResult::Waiting { .. } => 1,
            DispatchResult::Retryable { .. } | DispatchResult::CreatedPendingBackfill { .. } => 2,
            DispatchResult::Terminal { .. } => 3,
        }] += 1;
        // 逐条结果只收集**被点击配置**的记录：汇总发给点击人，别的配置的
        // 记录与他这次点击无关。
        if let Some((config_id, _, _)) = click.as_ref() {
            if task.config_id == *config_id {
                entries.push((
                    task.record_id.clone(),
                    round_result_label(task.existing_serial.as_deref(), &outcome, group_size),
                ));
            }
        }
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
    // 轮末汇总：手动批量点击跑完一轮后，点击人收到一次「命中多少、成败分布」。
    // 放在指标/日志之前，best-effort，不影响轮次语义。
    notify_round_summary(runner, trigger, click.as_ref(), &report, &entries).await;
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
    /// 任务行已有的审批编号（`serial_number`）。用户清空表格编号后重新点击时，
    /// 本轮结论是 `Backfilled` 但这是一次**人工重发**，通知要说明「已发起过」。
    existing_serial: Option<String>,
    /// 聚合组键（`None` = 普通单条任务）。非空即走 [`process_group`]。
    group_id: Option<String>,
    /// 组内序号：0 = leader（执行权持有者），`>=1` = 成员。
    group_seq: i64,
    /// 任务行的外部写入阶段。组处理用它判断「是否曾尝试创建 / 是否需要回捞」。
    effect_phase: String,
    /// 任务行的实例码。**组 leader 恒为 `None`**——组实例码只落组行（设计 §4.1）；
    /// 这里保留是为了单条任务的通知与「已发起过」判定沿用原语义。
    instance_code: Option<String>,
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
    /// 「命中 0 条」的说明消息是否**确实送达**。
    ///
    /// 轮末汇总靠它避免双发：已经发过「批量提交未处理任何记录」，就不该再发一条
    /// 「本轮无新增待处理记录」。
    empty_notified: bool,
}

/// 一轮播种的汇总结果。
#[derive(Debug, Default)]
struct SeedReport {
    seeded: usize,
    /// 本轮新播种/重开的 `(config_id, record_id)`，跨配置汇总。
    touched: Vec<(i64, String)>,
    /// 每个配置本轮的播种结果（轮末汇总用）。
    per_config: Vec<(i64, SeedOutcome)>,
    /// 本轮播种失败的配置与首行错误文案。
    failed: Vec<(i64, String)>,
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
    // 飞书响应体里的 log_id 每轮都不同；不去掉它，同一根因会被当成新失败每轮刷 WARN。
    let key = dedup_key(message);
    let mut failures = failures.lock().unwrap_or_else(|error| error.into_inner());
    match failures.get(&config_id) {
        Some(previous) if previous == &key => SeedFailureNote::Repeat,
        _ => {
            failures.insert(config_id, key);
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
/// 配置时才走 `reopen`，白名单是 `pending`/`review`/`terminal`/`unnotified`，
/// 外加「回填列已被清空」的 `backfilled`（只重新回填原实例）：
///
/// | 库里状态 | 字段为空意味着 | 处置 |
/// |---|---|---|
/// | `pending` | 已在队列里（含崩溃遗留的进行中行） | 轮询不动；点击时重置为 `pending` |
/// | `review` / `unnotified` | 外部写入结果未知 / 结论没送到人 | 点击时拉回 `pending` |
/// | `terminal` | 人工清空要重试 | 点击时拉回 `pending` |
/// | `backfilled` | 编号已落表 | 单元格仍有编号时不动；**用户清空后点击**才重开（重新回填原实例） |
///
/// # 为什么要扫多维表格而不是「只看有任务的行」
///
/// 首次运行时库里一条任务都没有——不扫表就永远不会有第一条。
/// （这里说的是**多维表格侧**的扫描；任务行侧已改为按本轮记录定向查询，
/// 见 [`load_tasks_by_record`]。）
///
/// # 只处理被点击的配置与记录
///
/// 发起人只能是点击人，而一次点击只代表**那一个配置里被点的那一条**（Token 按配置
/// 签发，申请人列又由系统回写）。所以手动轮只扫被点配置：单条点击只建/重开被点
/// 那一条，批量点击建/重开该配置下全部待处理记录；其它配置一律不动。这顺带收掉了
/// 「用 A 表的 Token 触发 B 表建单」的越权面。
///
/// 轮询轮（`Poll`）照旧扫全部启用配置，但只服务**已有**任务行，不建新行。
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
            "aggregate_batch",
            // 聚合签名需要定义快照（控件结构与金额币种）与 Base 时区（日期控件）。
            "form_snapshot",
            "base_timezone",
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
        // 手动轮只看被点的配置：其它配置既不该因这次点击建行，也不该被 reopen
        // （发起人身份只属于点击人，落到别的配置就是把别人的表当成自己的）。
        if let Some(clicked) = trigger.manual_config() {
            if clicked != config_id {
                continue;
            }
        }
        match seed_config(runner, &config, trigger, applicant, clicked_record_id).await {
            Ok(outcome) => {
                if note_seed_success(&runner.seed_failures, config_id) {
                    tracing::info!(config_id, "审批派发播种恢复");
                }
                report.seeded += outcome.seeded;
                report.touched.extend(
                    outcome
                        .touched
                        .iter()
                        .cloned()
                        .map(|record_id| (config_id, record_id)),
                );
                report.per_config.push((config_id, outcome));
            }
            // 一条配置坏掉不该拖垮别的配置：记下来，下一轮再来。
            // 坏配置每轮都失败，只在首次/文案变化时告警，其余降 DEBUG，
            // 否则每 30 秒一对 WARN、6 小时 1500+ 条。
            Err(error) => {
                // 用完整错误链（`{:#}`）的稳定化文案作去重键：同一根因必须稳定
                //（飞书响应体里的 log_id 每轮都在变，去重时抹掉）；
                // 换了根因（业务码/文案变化）仍要重新告警。
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
                report.failed.push((
                    config_id,
                    message.lines().next().unwrap_or_default().to_string(),
                ));
            }
        }
    }
    Ok(report)
}

/// 按 record_id 定向读取某配置下的任务行：`record_id → 任务行投影`。
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
) -> anyhow::Result<BTreeMap<String, TaskRowState>> {
    let unique = unique_record_ids(record_ids);
    let mut by_record = BTreeMap::new();
    for chunk in unique.chunks(TASK_LOOKUP_CHUNK) {
        let values: Vec<serde_json::Value> = chunk
            .iter()
            .map(|record_id| serde_json::json!(record_id))
            .collect();
        let query = task_lookup_query(context, config_id, values)?;
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
            by_record.insert(row.require("record_id")?, TaskRowState::from_record(&row)?);
        }
    }
    Ok(by_record)
}

/// 事务内的 [`load_tasks_by_record`]：解散/建组之后重算候选必须看得见本事务的写入。
///
/// 用连接池读会读到**事务开始前**的已提交快照，把刚被解散的组成员继续当成
/// 「已有任务行」，于是本次点击永远重建不出这些记录。
async fn load_tasks_by_record_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    record_ids: &[String],
) -> anyhow::Result<BTreeMap<String, TaskRowState>> {
    let unique = unique_record_ids(record_ids);
    let mut by_record = BTreeMap::new();
    for chunk in unique.chunks(TASK_LOOKUP_CHUNK) {
        let values: Vec<serde_json::Value> = chunk
            .iter()
            .map(|record_id| serde_json::json!(record_id))
            .collect();
        let query = task_lookup_query(context, config_id, values)?;
        let max_pages = chunk.len().div_ceil(PAGE_SIZE) + 1;
        let mut rows: Vec<Record> = Vec::with_capacity(chunk.len());
        for page in 1..=max_pages {
            let batch = query
                .clone()
                .page(page, PAGE_SIZE)?
                .all_in_tx(transaction)
                .await?;
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
            by_record.insert(row.require("record_id")?, TaskRowState::from_record(&row)?);
        }
    }
    Ok(by_record)
}

/// 播种侧任务行投影：组维护需要 `group_id`/`group_seq` 判断「是不是组行」，
/// 聚合处理期还需要 `effect_phase`（续跑与回捞）。
#[derive(Debug, Clone)]
struct TaskRowState {
    id: i64,
    state: String,
    /// 非空 = 该行属于某个聚合组（`group_seq=0` 是 leader）。
    group_id: Option<String>,
    /// 组内序号与外部写入阶段：播种侧只需要 `group_id` 判断「是不是组行」，
    /// 这两个字段供后续组处理与测试核对（Task 9 接入），先随投影一起读回。
    #[allow(dead_code)]
    group_seq: i64,
    #[allow(dead_code)]
    effect_phase: String,
}

impl TaskRowState {
    fn from_record(row: &Record) -> anyhow::Result<Self> {
        Ok(Self {
            id: row.require("id")?,
            state: row.require("state")?,
            group_id: row
                .optional::<String>("group_id")?
                .filter(|value| !value.trim().is_empty()),
            group_seq: row.require("group_seq")?,
            effect_phase: row.require("effect_phase")?,
        })
    }
}

/// 去重并跳过空串：分片大小按有效 id 算，重复 id 不该占份额。
fn unique_record_ids(record_ids: &[String]) -> Vec<&String> {
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
    unique
}

fn task_lookup_query(
    context: &FeishuContext,
    config_id: i64,
    values: Vec<serde_json::Value>,
) -> anyhow::Result<yang_base::table::TableQuery> {
    Ok(context
        .approval_tasks()
        .query()
        .select_fields(&[
            "id",
            "record_id",
            "state",
            "group_id",
            "group_seq",
            "effect_phase",
        ])?
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_in("record_id", values)?)
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
    // 单条点击只处理**被点的那一条**：发起人是点击人，他那次点击只针对这一行。
    // （批量点击 `clicked_record_id = None`，仍处理本配置全部待处理记录。）
    // 先筛再判空——单条点击时「表里还有别的待处理行」不代表本次没命中。
    let pending_records: Vec<&bitable::RecordItem> = pending_records
        .iter()
        .filter(|record| clicked_record_id.map_or(true, |id| id == record.record_id))
        .collect();
    if pending_records.is_empty() {
        // 点击受理却命中 0 条：给点击人一条 IM 说明，别让「没勾选就点」完全静默。
        // 但**单条点击**不通知：`dispatch_single` 已经处理了那条记录并给了用户反馈，
        // 这里的全表扫描发现 0 条是因为那条已被处理——再发"批量提交未处理任何记录"
        // 会让用户以为自己的点击走错了路径（2026-10-30 真机）。
        let is_single_click =
            trigger.manual_config() == Some(config_id) && clicked_record_id.is_some();
        let mut empty_notified = false;
        if !is_single_click {
            if let Some(open_id) = empty_selection_recipient(trigger, config_id, applicant) {
                empty_notified = notify_empty_selection(
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
        return Ok(SeedOutcome {
            empty_notified,
            ..SeedOutcome::default()
        });
    }

    // 已有的任务行：**只查本轮待处理记录**，行数与本轮记录同量级，与历史任务
    // 积累无关（旧实现读全表，任务行到 2000 后整配置播种永久失败）。
    // 按 record_id 建索引，决定「新建」还是（点击时）`reopen`。
    let record_ids: Vec<String> = pending_records
        .iter()
        .map(|record| record.record_id.clone())
        .collect();
    let by_record = load_tasks_by_record(&runner.context, config_id, &record_ids).await?;

    // 批量点击（`clicked_record_id=None`）走组维护与聚合播种；单条点击与轮询保持
    // 原语义（单条点击不维护组、不新建组，见设计 §4.4）。
    let aggregate_batch: bool = config.optional("aggregate_batch")?.unwrap_or(false);
    let batch_click = reopens_on_click(trigger, config_id) && clicked_record_id.is_none();
    let outcome = if batch_click {
        seed_batch_click(
            runner,
            config,
            &pending_records,
            &fields,
            &by_record,
            aggregate_batch,
            applicant,
        )
        .await?
    } else {
        let mut seeded = 0usize;
        let mut reopened = 0usize;
        let mut skipped = 0usize;
        let mut touched: Vec<String> = Vec::new();
        for record in pending_records {
            if record.record_id.is_empty() {
                continue;
            }
            match by_record.get(&record.record_id) {
                // 组行不在这里重开：组 leader 的复位只由 `maintain_groups` 在
                // 批量点击分支做，否则会把已建实例/creating 组的 leader 无条件
                // 拉回 pending（设计 §4.4 第 2 步）。
                Some(task) if task.group_id.is_some() => {}
                // 受理点击是**被点击配置**的显式人工重试：把不在执行中、停在可重试状态
                // （review / terminal / unnotified / 退避中）的任务重置回 pending，本轮认领
                // 时重跑。uuid 不变，飞书侧按 uuid 幂等，不会重复建单。
                // 只重开本配置：点 A 表不能替 B 表重试，否则 B 的旧报错会随每次点击重发。
                Some(task) if reopens_on_click(trigger, config_id) => {
                    if approval_execution::reopen(&runner.context, task.id).await? {
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
                    let uuid =
                        derive_uuid(&base_token, &table_id, &approval_code, &record.record_id);
                    if insert_seeded_task(&runner.context, config_id, &record.record_id, &uuid)
                        .await?
                    {
                        seeded += 1;
                        touched.push(record.record_id.clone());
                    } else {
                        skipped += 1;
                    }
                }
            }
        }
        SeedOutcome {
            seeded,
            reopened,
            skipped,
            touched,
            empty_notified: false,
        }
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

/// 批量点击的播种：**锁配置行 → 单条任务 reopen → 组维护 → 按当前勾选建组/建行**。
///
/// # 顺序不能换
///
/// - 先锁配置行（与 `admit` / `delete_config` 同一把 `FOR UPDATE`），否则「播种判定
///   可解散」与「受理插入任务行」会交错；
/// - `reopen` 只碰 `group_id` 为空的行；组 leader 的复位只属于 [`maintain_groups`]；
/// - 组维护之后**在事务内**重算「没有任务行」的记录：解散删掉的行对本事务可见，
///   用连接池读会读到事务开始前的快照，把刚解散的成员继续当成「已有行」。
async fn seed_batch_click(
    runner: &RoundRunner,
    config: &Record,
    pending_records: &[&bitable::RecordItem],
    fields: &[bitable::FieldItem],
    by_record: &BTreeMap<String, TaskRowState>,
    aggregate_batch: bool,
    applicant: Option<&str>,
) -> anyhow::Result<SeedOutcome> {
    let config_id: i64 = config.require("id")?;
    let base_token: String = config.require("base_token")?;
    let table_id: String = config.require("table_id")?;
    let approval_code: String = config.require("approval_code")?;

    let mut transaction = runner.tools.mysql()?.transaction().await?;
    let result: anyhow::Result<SeedOutcome> = async {
        anyhow::ensure!(
            approval_aggregation::repository::lock_config_in_tx(
                &mut transaction,
                &runner.context,
                config_id,
            )
            .await?,
            "审批配置已删除或停用，跳过本轮播种"
        );

        let mut outcome = SeedOutcome::default();
        // 第 2 步：非组任务行按点击语义重开（组行留给 maintain_groups）。
        for record in pending_records {
            let Some(task) = by_record.get(&record.record_id) else {
                continue;
            };
            if task.group_id.is_some() {
                continue;
            }
            if approval_execution::reopen(&runner.context, task.id).await? {
                outcome.reopened += 1;
                outcome.touched.push(record.record_id.clone());
            }
        }

        // 第 3 步：组维护与开关无关——关掉聚合也要让已存在的组收尾。
        outcome.reopened +=
            maintain_groups(&mut transaction, runner, config, pending_records, applicant).await?;

        // 第 4 步：解散之后重算候选（事务内可见本事务的删除）。
        let record_ids: Vec<String> = pending_records
            .iter()
            .map(|record| record.record_id.clone())
            .collect();
        let remaining =
            load_tasks_by_record_in_tx(&mut transaction, &runner.context, config_id, &record_ids)
                .await?;
        let fresh: Vec<&bitable::RecordItem> = pending_records
            .iter()
            .copied()
            .filter(|record| !record.record_id.is_empty())
            .filter(|record| !remaining.contains_key(&record.record_id))
            .collect();
        if aggregate_batch {
            let seeded =
                seed_aggregate_groups(&mut transaction, runner, config, &fresh, fields).await?;
            outcome.seeded += seeded.seeded;
            outcome.touched.extend(seeded.touched);
        } else {
            for record in fresh {
                let uuid = derive_uuid(&base_token, &table_id, &approval_code, &record.record_id);
                let inserted = runner
                    .context
                    .approval_tasks()
                    .query()
                    .insert_in_tx(
                        &mut transaction,
                        Record::new()
                            .set("config_id", config_id)
                            .set("record_id", record.record_id.clone())
                            .set("uuid", uuid)
                            .set("state", "pending"),
                    )
                    .await;
                match inserted {
                    Ok(_) => {
                        outcome.seeded += 1;
                        outcome.touched.push(record.record_id.clone());
                    }
                    // 一条坏记录不能中止整轮：与单条播种同一口径。
                    Err(error) => {
                        tracing::warn!(
                            config_id,
                            record_id = %record.record_id,
                            error = %error,
                            "批量播种任务行失败，跳过该记录"
                        );
                        outcome.skipped += 1;
                    }
                }
            }
        }
        Ok(outcome)
    }
    .await;

    match result {
        Ok(outcome) => {
            transaction.commit().await?;
            Ok(outcome)
        }
        Err(error) => {
            if let Err(rollback) = transaction.rollback().await {
                tracing::error!(error = %rollback, "审批派发播种事务回滚失败");
            }
            Err(error)
        }
    }
}

/// 组维护：已提交组补回填、review/lookup 组复位 leader、可解散组解散。
///
/// 返回本轮**复位过 leader** 的组数（计入轮末汇总的「重试」）。解散不计数——
/// 它删掉的记录会作为新记录重新播种，落在「新提交」里。
///
/// # 解散判据（三者同时成立）
///
/// 组行 `effect_phase='ready'`（从未尝试创建）、leader 无未过期租约、
/// **全组成员都在本次勾选内**。只解散这三个条件同时满足的组：勾选外的组保持
/// 不动，已尝试创建的组永不解散（设计 §4.4、§七.2/3）。
///
/// # 租约判定与写入同事务
///
/// 成员行在每个组判定前先 `FOR UPDATE` 锁住并复读（`lock_group_members_in_tx`），
/// 租约用行快照里的服务端时间判定；复位/摘组/删行都在同一事务、同一批锁内完成。
/// 连接池普通读会留下「读到无租约 → 被认领 → 仍按旧结论写」的窗口（设计 §6.1）。
///
/// # 已提交组的阶段
///
/// 组行有实例码时 leader 复位到 `created`（实例已存在，处理期按实例码恢复）；
/// 组行没有实例码时复位到 `ready`。**组行阶段一律不动**——它是外部副作用的
/// 唯一事实，`reopen` 没有资格改写它。
async fn maintain_groups(
    transaction: &mut Transaction,
    runner: &RoundRunner,
    config: &Record,
    pending_records: &[&bitable::RecordItem],
    applicant: Option<&str>,
) -> anyhow::Result<usize> {
    let config_id: i64 = config.require("id")?;
    let selected: HashSet<String> = pending_records
        .iter()
        .map(|record| record.record_id.clone())
        .filter(|record_id| !record_id.is_empty())
        .collect();
    if selected.is_empty() {
        return Ok(0);
    }

    let groups =
        approval_aggregation::repository::list_groups_for_config(&runner.context, config_id)
            .await?;
    let mut reopened = 0usize;
    for group in groups {
        let group_key: String = group.require("group_key")?;
        let phase: String = group.require("effect_phase")?;
        let instance_code = group
            .optional::<String>("instance_code")?
            .filter(|value| !value.trim().is_empty());
        // 租约判定必须与随后的 UPDATE/DELETE 同源：成员行先在本事务内 `FOR UPDATE`
        // 锁住并复读，快照带服务端时间 `now`。连接池普通读与写之间，`claim_ready`
        // 可以把该行认领走（它不锁配置行），复位会清掉有效租约、删除会删掉在途任务
        // 行——设计 §6.1。
        let members = approval_aggregation::repository::lock_group_members_in_tx(
            transaction,
            &runner.context,
            config_id,
            &group_key,
        )
        .await?;
        if members.is_empty() {
            // 组行与成员行不一致（崩溃或手工清理）：删掉孤儿组行，别让它每轮空转。
            approval_aggregation::repository::delete_group_in_tx(
                transaction,
                &runner.context,
                config_id,
                &group_key,
            )
            .await?;
            continue;
        }
        let Some(first_member) = members.first() else {
            continue;
        };
        let now = approval_aggregation::repository::server_now(first_member)?;
        let member_records: Vec<String> = members
            .iter()
            .map(|member| member.require::<String>("record_id"))
            .collect::<Result<_, _>>()?;
        let leader = members
            .iter()
            .find(|member| member.require::<i64>("group_seq").unwrap_or_default() == 0);
        let leader_free = match leader {
            Some(leader) => !approval_aggregation::repository::has_live_lease(leader, now),
            None => true,
        };

        if instance_code.is_none()
            && phase == "ready"
            && leader_free
            && members_all_selected(&member_records, &selected)
        {
            dissolve_group(transaction, runner, config_id, &group_key, &members).await?;
            continue;
        }

        let Some(leader) = leader else {
            continue;
        };
        let leader_state: String = leader.require("state")?;
        let reopen_needed = match (&instance_code, phase.as_str(), leader_state.as_str()) {
            // 已提交组：任一成员在本轮勾选内 = 它的回填列为空，补回填。
            (Some(_), _, _) => member_records
                .iter()
                .any(|record_id| selected.contains(record_id)),
            // 组行阶段表明尝试过外部写入：按组 uuid 回捞，不重发 create。
            (None, "creating" | "lookup", _) => true,
            // leader 停在人工核查：点击就是重试（被 full 组解散的除外）。
            (None, _, "review") => true,
            _ => false,
        };
        if !reopen_needed
            || !leader_free
            || !approval_aggregation::repository::reopenable_state(&leader_state)
        {
            continue;
        }
        let leader_id: i64 = leader.require("id")?;
        let leader_phase = if instance_code.is_some() {
            "created"
        } else {
            "ready"
        };
        if approval_aggregation::repository::reopen_group_leader_in_tx(
            transaction,
            &runner.context,
            leader_id,
            leader_phase,
        )
        .await?
        {
            reopened += 1;
            // 组 leader 不在 `SeedOutcome.touched` 里（它本来就有任务行），受理
            // 发起人只能在这里落——否则组会以「没有发起人」进等待态。
            if let Some(open_id) = applicant.map(str::trim).filter(|id| !id.is_empty()) {
                if let Err(error) = upsert_task_applicant(&runner.context, leader_id, open_id).await
                {
                    tracing::warn!(error = %error, task_id = leader_id, "聚合组 leader 的受理发起人落库失败");
                }
            }
        }
    }
    Ok(reopened)
}

/// 解散一个未提交的组：删 applicant 行 → 删/摘任务行 → 删组行。
///
/// `members` 必须是 [`approval_aggregation::repository::lock_group_members_in_tx`]
/// 在本事务内锁住的行快照：租约与阶段判定都基于它，到提交前不会被认领方改写。
/// 任一成员仍在执行或已进入外部写入阶段时整组不动（返回 `false`），绝不半解散。
///
/// 组内**已有自身实例码或回填编号**的行摘出组保留为单条任务：那些外部事实属于
/// 该记录自己，不能随组一起删掉（设计 §4.4 第 3 步）。
async fn dissolve_group(
    transaction: &mut Transaction,
    runner: &RoundRunner,
    config_id: i64,
    group_key: &str,
    members: &[Record],
) -> anyhow::Result<bool> {
    let Some(first) = members.first() else {
        return Ok(false);
    };
    let now = approval_aggregation::repository::server_now(first)?;
    let mut detach_ids: Vec<i64> = Vec::new();
    let mut delete_ids: Vec<i64> = Vec::new();
    for member in members {
        let task_id: i64 = member.require("id")?;
        // 解散的前置条件之一是「leader 无未过期租约」；任何成员被认领都说明组还在
        // 被处理，本轮不动（行已锁住，这个判定到提交前有效）。
        if approval_aggregation::repository::has_live_lease(member, now) {
            return Ok(false);
        }
        let own_instance = member
            .optional::<String>("instance_code")?
            .filter(|value| !value.trim().is_empty());
        let own_serial = member
            .optional::<String>("serial_number")?
            .filter(|value| !value.trim().is_empty());
        if own_instance.is_some() || own_serial.is_some() {
            detach_ids.push(task_id);
        } else {
            // 删除只允许从未尝试过外部写入的行（设计 §6.1）：阶段不是 `ready`、
            // 或状态绑定外部事实时，删掉它就失去了对账依据。
            let phase: String = member.require("effect_phase")?;
            let state: String = member.require("state")?;
            if phase != "ready" || !approval_aggregation::repository::dissolvable_state(&state) {
                return Ok(false);
            }
            delete_ids.push(task_id);
        }
    }
    for task_id in detach_ids {
        anyhow::ensure!(
            approval_aggregation::repository::detach_task_from_group_in_tx(
                transaction,
                &runner.context,
                task_id,
            )
            .await?,
            "解散摘组失败：任务行 {task_id} 的执行权已变化"
        );
    }
    let deleted = approval_aggregation::repository::delete_task_rows_in_tx(
        transaction,
        &runner.context,
        &delete_ids,
    )
    .await?;
    anyhow::ensure!(
        deleted == delete_ids.len() as u64,
        "解散删行不完整：期望 {} 行，实际 {deleted} 行",
        delete_ids.len()
    );
    approval_aggregation::repository::delete_group_in_tx(
        transaction,
        &runner.context,
        config_id,
        group_key,
    )
    .await?;
    tracing::debug!(
        config_id,
        group_key,
        detached_or_deleted = members.len(),
        deleted = delete_ids.len(),
        "聚合组已解散"
    );
    Ok(true)
}

/// 聚合播种：对「本次勾选且没有任务行」的记录算一致性签名、分组并建组。
///
/// 组的成员行与组行在同一事务里插入；`touched` 返回**全部**成员的记录 id——
/// 缺一个成员，它的受理发起人就不会落库，处理期会在 missing-initiator 守卫下
/// 永久等待（设计 §4.4 第 4 步）。
///
/// 签名失败（非法值、外部选项解析失败、附件结构损坏等）的记录用
/// `error:{record_id}` 单独成组：处理期照常暂停报错，而不是被静默合并进正常组。
async fn seed_aggregate_groups(
    transaction: &mut Transaction,
    runner: &RoundRunner,
    config: &Record,
    pending_records: &[&bitable::RecordItem],
    fields: &[bitable::FieldItem],
) -> anyhow::Result<SeedOutcome> {
    let mut outcome = SeedOutcome::default();
    if pending_records.is_empty() {
        return Ok(outcome);
    }
    let config_id: i64 = config.require("id")?;
    let base_token: String = config.require("base_token")?;
    let table_id: String = config.require("table_id")?;
    let approval_code: String = config.require("approval_code")?;
    let form_snapshot: String = config.optional("form_snapshot")?.unwrap_or_default();
    let timezone_offset = config
        .optional::<String>("base_timezone")?
        .filter(|name| !name.trim().is_empty())
        .and_then(|name| FixedOffset::from_iana(name.trim()).ok())
        .unwrap_or(runner.timezone_offset);

    let map_rows = runner
        .context
        .approval_field_maps()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .all()
        .await?;
    let maps: Vec<serde_json::Map<String, serde_json::Value>> =
        map_rows.into_iter().map(Record::into_map).collect();
    let base_widgets = widget_maps_from_rows(&maps, |widget_id| {
        amount_currency(&form_snapshot, widget_id)
    })
    .ok_or_else(|| anyhow::anyhow!("字段映射配置损坏（转换器标识非法），无法计算聚合签名"))?;

    let mut candidates: Vec<GroupCandidate> = Vec::with_capacity(pending_records.len());
    for record in pending_records {
        let signature = aggregate_signature(
            runner,
            record,
            fields,
            &base_widgets,
            &form_snapshot,
            timezone_offset,
        )
        .await;
        candidates.push(GroupCandidate::new(record.record_id.clone(), signature));
    }

    for group in plan_groups(config_id, &candidates) {
        let uuid = group_uuid(&base_token, &table_id, &approval_code, &group.group_key);
        let inserted = approval_aggregation::repository::insert_group_in_tx(
            transaction,
            &runner.context,
            config_id,
            &group.group_key,
            &group.signature_hash,
            &uuid,
        )
        .await;
        if let Err(error) = inserted {
            tracing::warn!(
                config_id,
                group_key = %group.group_key,
                error = %error,
                "插入聚合组失败，跳过该组"
            );
            continue;
        }
        for (group_seq, record_id) in group.members.iter().enumerate() {
            let uuid = derive_uuid(&base_token, &table_id, &approval_code, record_id);
            approval_aggregation::repository::insert_group_task_in_tx(
                transaction,
                &runner.context,
                config_id,
                &group.group_key,
                group_seq as i64,
                record_id,
                &uuid,
            )
            .await?;
            outcome.seeded += 1;
            outcome.touched.push(record_id.clone());
        }
    }
    Ok(outcome)
}

/// 一条记录的一致性签名；任何失败都折成 `error:{record_id}`（单独成组）。
///
/// 签名阶段**不做任何网络副作用**：附件只取源文件三元组，不调 `transfer`。
async fn aggregate_signature(
    runner: &RoundRunner,
    record: &bitable::RecordItem,
    fields: &[bitable::FieldItem],
    base_widgets: &[WidgetMap],
    form_snapshot: &str,
    timezone_offset: FixedOffset,
) -> String {
    let record_id = record.record_id.as_str();
    let error_signature = || format!("error:{record_id}");
    let cells = match bitable::rekey_cells_by_field_id(fields, &record.fields) {
        Ok(cells) => cells,
        Err(error) => {
            tracing::warn!(record_id, error = %error, "聚合签名重映射字段失败，该记录单独成组");
            return error_signature();
        }
    };
    let mut widgets: Vec<WidgetMap> = base_widgets.to_vec();
    if let Err(error) =
        resolve_external_options(&runner.context, form_snapshot, &mut widgets, &cells, fields).await
    {
        tracing::warn!(record_id, error = %error, "聚合签名解析外部选项失败，该记录单独成组");
        return error_signature();
    }
    let attachment_codes = match source_identities(&widgets, &cells) {
        Ok(codes) => codes,
        Err(error) => {
            tracing::warn!(record_id, error = %error, "聚合签名读取附件源标识失败，该记录单独成组");
            return error_signature();
        }
    };
    match build_signature(&FormInput {
        widgets: &widgets,
        form_snapshot,
        cells: &cells,
        attachment_codes: &attachment_codes,
        timezone_offset,
    }) {
        Ok(signature) => signature,
        Err(error) => {
            tracing::warn!(record_id, error = %error, "聚合签名转换失败，该记录单独成组");
            error_signature()
        }
    }
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
/// 不中断本轮其余配置的播种。返回「是否确实送达」——轮末汇总据此避免双发。
async fn notify_empty_selection(
    runner: &RoundRunner,
    config_id: i64,
    open_id: &str,
    config: &Record,
    backfill_name: &str,
    batch_scope_name: Option<&str>,
) -> bool {
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
            true
        }
        status => {
            tracing::warn!(
                config_id,
                open_id,
                status = ?status,
                message = %result.message,
                "审批派发：点击受理命中 0 条，说明消息未送达"
            );
            false
        }
    }
}

/// 「本轮结论是不是清空编号后的人工重发」。
///
/// 判据：结论是 `Backfilled`，且任务行**在这次处理之前**已有编号
/// （`existing_serial`）。首次成功时任务行还没有编号，不会命中；所以命中只可能
/// 来自「用户清空表格编号 → 点击 → reopen → 实例恢复 → 重新回填」那条路径。
fn already_initiated(task: &ClaimedTask, outcome: &DispatchResult) -> bool {
    matches!(outcome, DispatchResult::Backfilled { .. }) && task.existing_serial.is_some()
}

/// 轮末汇总里一条记录的结果描述（约 80 字符内）。抽成纯函数以便离线钉住措辞。
///
/// `group_size` 非空（且大于 1）时在结尾补「N 条合 1 单」：一组 N 条只被认领
/// 一次，汇总里也只出现 leader 那一条，不写清楚会让人以为漏处理了成员。
fn round_result_label(
    existing_serial: Option<&str>,
    outcome: &DispatchResult,
    group_size: Option<usize>,
) -> String {
    let text = match outcome {
        DispatchResult::Backfilled { serial_number, .. } => {
            if existing_serial.is_some() {
                format!("已发起过：编号 {serial_number}（已重新回填）")
            } else {
                format!("成功：编号 {serial_number}")
            }
        }
        DispatchResult::Waiting { reason } => format!("待补齐：{reason}"),
        DispatchResult::Retryable { message }
        | DispatchResult::CreatedPendingBackfill { message, .. } => {
            format!("待重试：{message}")
        }
        DispatchResult::Terminal { message } => format!("失败：{message}"),
    };
    let text = match group_size.filter(|size| *size > 1) {
        Some(size) => format!("{text}（{size} 条合 1 单）"),
        None => text,
    };
    approval_feedback::one_line(&text, 80)
}

/// 轮末汇总正文（首行是卡片标题）。
///
/// 逐条结果最多列 8 条，其余折成「等 N 条」；积压与主因来自任务行
/// （`state=pending` 的 `last_error` 众数）。整体按 1800 字符截断。
#[allow(clippy::too_many_arguments)] // 9 个参数：汇总要素逐个平铺，收结构体只是挪位置。
fn round_summary_text(
    title: &str,
    base_token: &str,
    table_id: &str,
    seeded: usize,
    reopened: usize,
    entries: &[(String, String)],
    backlog: Option<u64>,
    backlog_reason: Option<&str>,
    scan_failure: Option<&str>,
) -> String {
    let mut lines = vec!["【批量派发结果】".to_string(), format!("配置：{title}")];
    let hit = seeded + reopened;
    if hit == 0 {
        lines.push("记录：本轮无新增待处理记录".to_string());
    } else {
        lines.push(format!(
            "记录：本轮命中 {hit} 条（新提交 {seeded}，重试 {reopened}）"
        ));
    }
    if let Some(error) = scan_failure {
        lines.push(format!(
            "原因：本轮扫描失败：{}",
            approval_feedback::one_line(error, 200)
        ));
    } else if entries.is_empty() {
        if hit == 0 {
            lines.push("原因：本轮没有需要处理的记录".to_string());
        } else {
            // 命中记录已入队，但本轮可能还没轮到认领（游标/批量上限），
            // 不能说成「没有需要处理的记录」。
            lines.push("原因：命中的记录已入队，将在后续轮次处理".to_string());
        }
    } else {
        // 记录 id 只取尾部，避免整行被一长串 id 占满。
        let tail = |record_id: &str| -> String {
            record_id
                .chars()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        };
        let items: Vec<String> = entries
            .iter()
            .take(8)
            .map(|(record_id, label)| format!("{} {label}", tail(record_id)))
            .collect();
        let mut reason = items.join("；");
        if entries.len() > 8 {
            reason.push_str(&format!("；等 {} 条", entries.len()));
        }
        lines.push(format!("原因：{reason}"));
    }
    if let Some(count) = backlog {
        if count > 0 {
            // `backlog_reason` 是原因文案，不是名称：没有非空原因时不能落
            // `display_name` 的「名称暂不可用」兜底。
            match backlog_reason.filter(|reason| !reason.trim().is_empty()) {
                Some(reason) => lines.push(format!(
                    "原因：积压 {count} 条待处理，多数原因：{}",
                    approval_feedback::one_line(reason, 100)
                )),
                None => lines.push(format!("原因：积压 {count} 条待处理")),
            }
        }
    }
    lines.push("下一步：按原因补齐后重新点击；已成功的记录无需处理。".to_string());
    lines.push(format!(
        "技术定位：Base={}；表={}",
        approval_feedback::one_line(base_token, 100),
        approval_feedback::one_line(table_id, 100)
    ));
    approval_feedback::truncate(&lines.join("\n"), 1800)
}

/// 轮末汇总：向点击人发一条「本轮跑完了」的说明。
///
/// 只在**批量点击**（`record_id = None`）且拿得到点击人时发；该配置本轮已经发过
/// 「批量提交未处理任何记录」时跳过（那条已经说明了情况）。全程 best-effort：
/// 任何查询失败都降级，发送失败只告警。
async fn notify_round_summary(
    runner: &RoundRunner,
    trigger: RoundTrigger,
    click: Option<&(i64, String, Option<String>)>,
    report: &SeedReport,
    entries: &[(String, String)],
) {
    let Some(config_id) = trigger.manual_config() else {
        return;
    };
    let Some((_, open_id, record_id)) = click else {
        return;
    };
    if record_id.is_some() {
        return;
    }
    let open_id = open_id.trim();
    if open_id.is_empty() {
        return;
    }
    let outcome = report
        .per_config
        .iter()
        .find(|(id, _)| *id == config_id)
        .map(|(_, outcome)| outcome);
    if outcome.is_some_and(|outcome| outcome.empty_notified) {
        return;
    }
    let seeded = outcome.map_or(0, |outcome| outcome.seeded);
    let reopened = outcome.map_or(0, |outcome| outcome.reopened);
    let scan_failure = report
        .failed
        .iter()
        .find(|(id, _)| *id == config_id)
        .map(|(_, message)| message.as_str());

    // 配置展示名（2 秒预算，失败用兜底文案）与坐标。
    let config_lookup = async {
        runner
            .context
            .approval_configs()
            .query()
            .where_primary_key_eq(serde_json::json!(config_id))?
            .optional()
            .await
    };
    let config = match tokio::time::timeout(std::time::Duration::from_secs(2), config_lookup).await
    {
        Ok(Ok(row)) => row,
        _ => None,
    };
    let field = |name: &str| {
        config
            .as_ref()
            .and_then(|row| row.optional::<String>(name).ok().flatten())
    };
    let title = approval_feedback::display_name(field("title").as_deref());
    let base_token = field("base_token").unwrap_or_default();
    let table_id = field("table_id").unwrap_or_default();
    let base_name = approval_feedback::display_name(field("base_name").as_deref());
    let table_name = approval_feedback::display_name(field("table_name").as_deref());

    // 积压：该配置 state=pending 的总数与最多 20 行的 last_error（众数即主因）。
    // 查询失败只降级为「不写积压行」，不影响汇总的其余部分。
    let (backlog, backlog_reason) = pending_backlog(runner, config_id).await;

    let filtered: Vec<(String, String)> = entries
        .iter()
        .filter(|(_, label)| !label.is_empty())
        .cloned()
        .collect();
    let text = round_summary_text(
        &title,
        &base_token,
        &table_id,
        seeded,
        reopened,
        &filtered,
        backlog,
        backlog_reason.as_deref(),
        scan_failure,
    );

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
                message_id = ?result.message_id,
                "审批派发：已向点击人发送轮末汇总"
            );
        }
        status => {
            tracing::warn!(
                config_id,
                status = ?status,
                message = %result.message,
                "审批派发：轮末汇总未送达"
            );
        }
    }
}

/// 该配置 `state=pending` 的积压条数与主要原因（`last_error` 众数）。
///
/// 查询失败时返回 `(None, None)`——汇总里少一行，不影响其余内容。
async fn pending_backlog(runner: &RoundRunner, config_id: i64) -> (Option<u64>, Option<String>) {
    let pending = || {
        runner
            .context
            .approval_tasks()
            .query()
            .where_eq("config_id", serde_json::json!(config_id))
            .and_then(|query| query.where_eq("state", serde_json::json!("pending")))
    };
    let Ok(count_query) = pending() else {
        return (None, None);
    };
    let count = count_query.count().await.ok();
    let reasons = match pending().and_then(|query| {
        query
            .select_fields(&["last_error"])
            .and_then(|query| query.page(1, 20))
    }) {
        Ok(query) => query.all().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in reasons {
        let reason = row
            .optional::<String>("last_error")
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(reason) = reason {
            *counts.entry(reason).or_default() += 1;
        }
    }
    let main = counts
        .into_iter()
        .max_by_key(|(reason, count)| (*count, reason.clone()))
        .map(|(reason, _)| reason);
    (count, main)
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
/// 在本轮播种出来，播种前写会**漏掉本轮新行**——首点就缺身份，要再点一次才对。
/// 所以写在播种之后、认领之前。
///
/// # touched 为空直接返回
///
/// 单条点击若该记录既未播种也未 reopen，admit 已在快照里落了身份，不需要广播；
/// 此时一次查询都不该发。
///
/// # 失败只 warn
///
/// 落库失败不该中断整轮：那一行本轮会以「没有发起人」进等待态（零出站），
/// 下次有人点它即可自愈——不会静默地用错身份建单。
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
    for (record_id, task) in tasks {
        if task.state != "pending" {
            continue;
        }
        if let Err(error) = upsert_task_applicant(&runner.context, task.id, open_id).await {
            tracing::warn!(error = %error, task_id = task.id, record_id, "受理发起人落库失败");
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
    // 发起人：首次受理快照优先（`admit` 与任务行同事务写入），缺失时读属性扩展表
    // （全表受理的批量行没有快照，身份由 Action/worker 落这张表）。
    //
    // 两个来源**都只来自点击人**；表格的申请人列不参与——它是系统回写的产物。
    // 快照与表都要看：旧版本建的快照可能是 `None`，而之后的一次点击已经把它补进表里。
    let applicant_open_id = match snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.applicant.clone())
    {
        Some(applicant) => Some(applicant),
        None => runner
            .context
            .approval_task_applicants()
            .query()
            .where_eq("task_id", serde_json::json!(task_id))?
            .optional()
            .await?
            .and_then(|applicant| applicant.optional::<String>("open_id").ok().flatten()),
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
        existing_serial: row
            .optional::<String>("serial_number")?
            .filter(|value| !value.trim().is_empty()),
        group_id: row
            .optional::<String>("group_id")?
            .filter(|value| !value.trim().is_empty()),
        group_seq: row.require("group_seq")?,
        effect_phase: row.require("effect_phase")?,
        instance_code: row
            .optional::<String>("instance_code")?
            .filter(|value| !value.trim().is_empty()),
    }))
}

/// 一条任务的处理产物：结论 + 兜底通知接收人。
struct TaskOutcome {
    result: DispatchResult,
    /// 记录创建人 open_id（`automatic_fields` 取回）——发起人不可用时的兜底
    /// 接收人，保证「用户发起过请求」这件事在任何结论下都有反馈出口。
    ///
    /// 发起人本身不在这里：它是任务的属性（`task.applicant_open_id`），只来自
    /// 点击人，**不再回退读表格申请人列**。
    creator_open_id: Option<String>,
    /// 组分支已经自己完成落库、通知、审计与指标（设计 §4.6 第 10 步）。
    ///
    /// 置位时 [`run_round`] 跳过通用 [`record_outcome`]——那条路径用单条的
    /// `finish`（阶段白名单 `ready/created`）收尾，会把 `lookup`/`creating` 的组
    /// 卡死，并把 `paused` 经 `Waiting→pending` 重新拉回轮询。
    persisted: bool,
    /// 聚合组内条数（`None` = 普通单条任务）。轮末汇总据此写明「N 条合 1 单」，
    /// 否则一组 N 条只显示成 1 条，点击人会以为漏处理。
    group_size: Option<usize>,
}

impl TaskOutcome {
    /// 早退构造：还没读到记录（拿不到创建人），只有结论。
    fn early(result: DispatchResult) -> Self {
        Self {
            result,
            creator_open_id: None,
            persisted: false,
            group_size: None,
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
    // 聚合组必须排在 missing-initiator 早退**之前**（设计 §4.6 第 11 步）：
    // 组缺身份落组级暂停（可被重新点击拉起），而单条 `Waiting` 会把 leader 留成
    // `pending` 让它在轮询里反复空转，成员则永远停在原地。
    if task.group_id.is_some() {
        return process_group(runner, task).await;
    }
    // 没有发起人身份就不该出站：不读表结构、不读记录、不碰飞书。
    // 来源只有两种——上线前的存量任务行，以及受理与落库之间的窄窗口。
    // 落 `Waiting`（`outcome_record` 映射成 `pending`）是为了让「有人再点一次
    // 这条记录」能自愈；不落终态是因为回填列若是链接类型，写原因根本写不进去。
    if task.applicant_open_id.is_none() {
        tracing::info!(
            stage = "initiator",
            task_id = task.id,
            record_id = %task.record_id,
            outcome = "waiting",
            "审批任务没有发起人身份，等待人工重新点击"
        );
        return TaskOutcome::early(DispatchResult::Waiting {
            reason: crate::addon::feishu::domain::approval_dispatch::MISSING_INITIATOR_REASON
                .to_string(),
        });
    }
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

    TaskOutcome {
        result,
        creator_open_id,
        persisted: false,
        group_size: None,
    }
}

/// 组内一条待参与表单的成员记录。
struct GroupMemberCells {
    record_id: String,
    /// 已按 `field_id` 重映射的单元格。
    cells: serde_json::Map<String, serde_json::Value>,
    /// 已搬运好的附件 code（空 = 该成员没有媒体控件）。
    attachment_codes: BTreeMap<String, Vec<String>>,
}

/// 处理一个聚合组：组行是身份唯一来源，leader 只承载执行簿记（设计 §4.6）。
///
/// # 为什么不能复用单条的 `process_one`
///
/// 单条路径的收尾是通用 `record_outcome` + `finish`（阶段白名单 `ready/created`），
/// 而组在 `60012` 之后停在 `lookup`、创建前停在 `creating`——照抄会让组永远收不了
/// 尾；单条把中间态写成 `pending` 也会把暂停组重新拉回轮询。所以组分支自己完成
/// 落库、通知、审计与指标，返回 `persisted=true` 让 `run_round` 跳过通用路径。
///
/// # 恢复顺序（设计 §4.6 第 2-5 步）
///
/// 1. 组行有实例码 → 直接回填，不做签名复核、不重新创建；
/// 2. 组行 `lookup` → 按**组 uuid** 回捞，命中即落实例码；`1390003` 未命中把阶段
///    写回 `ready` 再退避重发；
/// 3. 否则按成员逐条读记录、解析外部选项（并集）、搬运附件，组装多行明细表单；
/// 4. 签名复核**仅当组行 `effect_phase='ready'`**——那时还从没尝试过创建。
async fn process_group(runner: &RoundRunner, task: &ClaimedTask) -> TaskOutcome {
    let Some(group_key) = task.group_id.clone() else {
        return TaskOutcome::early(DispatchResult::Terminal {
            message: "任务声称属于聚合组但缺少组键".into(),
        });
    };
    tracing::debug!(
        stage = "group",
        task_id = task.id,
        group_key,
        group_seq = task.group_seq,
        task_phase = %task.effect_phase,
        "开始处理聚合组"
    );
    // 只有 leader 承载执行副作用：成员行被认领（批量认领已按 `group_seq=0` 过滤，
    // 单条点击走摘组）说明组结构异常，不动它，等下一轮摘组或重新点击。
    if task.group_seq != 0 {
        tracing::warn!(
            task_id = task.id,
            group_key,
            "聚合组成员行被认领，跳过本轮处理"
        );
        return TaskOutcome::early(DispatchResult::Retryable {
            message: "聚合组成员行被认领，等待重新点击".into(),
        });
    }
    let mut outcome = group_once(runner, task, &group_key).await;
    outcome.persisted = true;
    outcome
}

/// [`process_group`] 的实现：拿结论、落库、发通知、写审计与指标。
async fn group_once(runner: &RoundRunner, task: &ClaimedTask, group_key: &str) -> TaskOutcome {
    // 1. 组行：组实例身份的唯一来源。
    let group = match approval_aggregation::repository::load_group(
        &runner.context,
        task.config_id,
        group_key,
    )
    .await
    {
        Ok(Some(group)) => group,
        Ok(None) => {
            // 组行不见了：成员行留着也拼不出任何东西，按暂停报出来等人处理，
            // 不静默丢。
            return settle_group(
                runner,
                task,
                group_key,
                GroupSettlement::paused("聚合组行不存在（可能被手工清理），已暂停"),
                1,
            )
            .await;
        }
        Err(error) => {
            return settle_group(
                runner,
                task,
                group_key,
                GroupSettlement::retryable(format!("读取聚合组失败：{error}")),
                1,
            )
            .await
        }
    };
    let instance_code = group
        .optional::<String>("instance_code")
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let group_phase: String = group.require("effect_phase").unwrap_or_default();
    let group_uuid: String = group.require("uuid").unwrap_or_default();
    let group_serial: Option<String> = group
        .optional::<String>("serial_number")
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty());

    // 2. 成员行（按 `group_seq` 升序）：
    let members = match approval_aggregation::repository::load_members(
        &runner.context,
        task.config_id,
        group_key,
    )
    .await
    {
        Ok(members) => members,
        Err(error) => {
            return settle_group(
                runner,
                task,
                group_key,
                GroupSettlement::retryable(format!("读取聚合组成员失败：{error}")),
                1,
            )
            .await
        }
    };
    let member_count = members.len();
    if members.is_empty() {
        return settle_group(
            runner,
            task,
            group_key,
            GroupSettlement::paused("聚合组没有成员记录，已暂停"),
            1,
        )
        .await;
    }

    // 3. 已建实例：恢复回填，跳过签名复核与创建（设计 §4.6 第 2 步）。
    if let Some(code) = instance_code {
        return run_group_dispatch(
            runner,
            task,
            group_key,
            &group_uuid,
            Some(code.as_str()),
            &members,
            member_count,
            group_serial,
            None,
        )
        .await;
    }

    // 4. 组行停在 `lookup`（上一轮 create 响应丢失，本地没拿到实例码）：
    //    先按**组 uuid** 回捞，命中即落实例码；`1390003` 未命中把组行与 leader
    //    阶段一起写回 `ready`，下一轮才允许重新创建（设计 §4.6 第 3 步、§6.4）。
    if group_phase == "lookup" && group_uuid.trim().is_empty() {
        // 组 uuid 缺失 = 组行损坏：不猜 uuid，直接暂停。
        return settle_group(
            runner,
            task,
            group_key,
            GroupSettlement::paused("聚合组缺少 uuid，无法按 uuid 回捞实例，已暂停"),
            member_count,
        )
        .await;
    }
    if group_phase == "lookup" {
        tracing::info!(stage = "recover", group_key, "按组 uuid 回捞审批实例");
        let found = crate::addon::feishu::domain::approval::get_instance(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            &group_uuid,
        )
        .await;
        match found {
            Ok(GetOutcome::Found(detail)) => {
                let code = detail.instance_code.clone();
                if let Err(error) = approval_execution::confirm_group_effect(
                    &runner.context,
                    &task.claim,
                    group_key,
                    "created",
                    Some(&code),
                )
                .await
                {
                    return settle_group(
                        runner,
                        task,
                        group_key,
                        GroupSettlement::paused(format!(
                            "按 uuid 回捞到实例但组行确认失败：{error}"
                        )),
                        member_count,
                    )
                    .await;
                }
                return run_group_dispatch(
                    runner,
                    task,
                    group_key,
                    &group_uuid,
                    Some(code.as_str()),
                    &members,
                    member_count,
                    group_serial,
                    None,
                )
                .await;
            }
            Ok(GetOutcome::NotFound) => {
                // 实例确实不存在：组行阶段复位为 `ready` 才能重发 create（组行阶段
                // 是唯一事实，重试不会自己复位）。
                if let Err(error) = approval_execution::confirm_group_effect(
                    &runner.context,
                    &task.claim,
                    group_key,
                    "ready",
                    None,
                )
                .await
                {
                    tracing::warn!(group_key, error = %error, "聚合组阶段复位失败");
                }
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::retryable("按组 uuid 查不到实例，已复位待重新创建"),
                    member_count,
                )
                .await;
            }
            Err(failure) => {
                let settlement = match failure.kind {
                    FailureKind::Fatal { .. } => {
                        GroupSettlement::paused(format!("按组 uuid 回捞失败：{}", failure.message))
                    }
                    _ => GroupSettlement::retryable(format!(
                        "按组 uuid 回捞失败：{}",
                        failure.message
                    )),
                };
                return settle_group(runner, task, group_key, settlement, member_count).await;
            }
        }
    }

    // 5. 发起人：组缺身份 = 整组暂停（不落 single `Waiting`，设计 §4.6 第 11 步）。
    if task.applicant_open_id.is_none() {
        tracing::info!(
            stage = "initiator",
            task_id = task.id,
            group_key,
            outcome = "paused",
            "聚合组没有发起人身份，整组暂停"
        );
        return settle_group(
            runner,
            task,
            group_key,
            GroupSettlement::paused(
                crate::addon::feishu::domain::approval_dispatch::MISSING_INITIATOR_REASON,
            ),
            member_count,
        )
        .await;
    }

    // 5. 签名复核只在「从未尝试创建」时做（组行阶段是外部副作用的唯一事实）。
    //    比较用的是处理期本来就要读的那份记录，不额外拉一遍表。
    let signature_check = if group_phase == "ready" {
        group
            .optional::<String>("signature_hash")
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty())
    } else {
        None
    };
    run_group_dispatch(
        runner,
        task,
        group_key,
        &group_uuid,
        None,
        &members,
        member_count,
        group_serial,
        signature_check.as_deref(),
    )
    .await
}

/// 组签名复核：成员的非明细签名是否仍与组行 `signature_hash` 一致。
///
/// 只在组行 `effect_phase='ready'`（从未尝试创建）时调用——那时数据变了还能安全
/// 地整组暂停重来；已经尝试过创建的组必须续跑，不能因为签名漂移丢弃实例。
///
/// 返回 `Some(原因)` 表示不一致。签名用**这条记录自己的**已解析控件集合算，
/// 与播种期同一函数（`build_signature`）同一输入，可比。
fn verify_group_signature(
    widgets: &[WidgetMap],
    form_snapshot: &str,
    cells: &serde_json::Map<String, serde_json::Value>,
    timezone_offset: FixedOffset,
    stored: &str,
    signatures: &mut Vec<String>,
) -> Option<String> {
    let Ok(identities) = source_identities(widgets, cells) else {
        return Some("组内记录的附件结构非法，无法复核一致性。请核对后重新点击。".into());
    };
    let signature = match build_signature(&FormInput {
        widgets,
        form_snapshot,
        cells,
        attachment_codes: &identities,
        timezone_offset,
    }) {
        Ok(signature) => signature,
        Err(error) => {
            return Some(format!(
                "组内记录无法生成一致性签名：{error}。请核对后重新点击。"
            ))
        }
    };
    signatures.push(signature_hash(&signature));
    if signatures.iter().all(|hash| *hash == signatures[0]) {
        if signatures[0] == stored {
            return None;
        }
        return Some(
            "组内记录的非明细字段在排队期间被修改，与分组时不一致。请核对后重新批量点击。".into(),
        );
    }
    Some("组内记录的非明细字段不再一致，无法合成同一个审批实例。请核对后重新批量点击。".into())
}

/// 逐成员读记录 → 解析外部选项（并集）→ 搬运附件 → 交给 `dispatch_group`。
#[allow(clippy::too_many_arguments)] // 组派发的要素逐个平铺，收结构体只是挪位置。
async fn run_group_dispatch(
    runner: &RoundRunner,
    task: &ClaimedTask,
    group_key: &str,
    group_uuid: &str,
    instance_code: Option<&str>,
    members: &[Record],
    member_count: usize,
    group_serial: Option<String>,
    // 复核签名用的组行 signature_hash；`None` = 不复核（已尝试过创建或已恢复
    // 实例的组必须续跑，不能因为签名漂移丢掉真实存在的外部实例）。
    signature_check: Option<&str>,
) -> TaskOutcome {
    let coordinates = BitableCoordinates {
        app_token: task.base_token.clone(),
        table_id: task.table_id.clone(),
        view_id: None,
    };
    let fields = match bitable::list_all_fields(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => fields,
        Err(failure) => {
            let settlement = match failure.kind {
                FailureKind::Fatal { .. } => {
                    GroupSettlement::Paused(format!("读取表结构失败：{}", failure.message))
                }
                _ => GroupSettlement::Retryable(format!("读取表结构失败：{}", failure.message)),
            };
            return settle_group(runner, task, group_key, settlement, member_count).await;
        }
    };
    let backfill_field_name = match bitable::resolve_field_name(&fields, &task.backfill_field) {
        Ok(name) => name,
        Err(error) => {
            return settle_group(
                runner,
                task,
                group_key,
                GroupSettlement::paused(format!("回填列不可用：{error}")),
                member_count,
            )
            .await
        }
    };

    // 成员记录：(record_id, cells)。已建实例时仍要读：扩展字段要按成员写。
    let mut rows: Vec<GroupMemberCells> = Vec::with_capacity(members.len());
    let mut missing: Vec<(i64, String)> = Vec::new();
    // 已建实例的组里，记录不可读的成员被摘出并落终态（设计 §5 Fatal 分支）。
    let mut detached = 0usize;
    let mut pooled = task.widgets.clone();
    let mut signature_hashes: Vec<String> = Vec::with_capacity(members.len());
    for member in members.iter() {
        let task_id: i64 = match member.require("id") {
            Ok(id) => id,
            Err(error) => {
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::paused(format!("聚合组成员行缺少 id：{error}")),
                    member_count,
                )
                .await
            }
        };
        let record_id: String = match member.require("record_id") {
            Ok(record_id) => record_id,
            Err(error) => {
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::paused(format!("聚合组成员行缺少记录 id：{error}")),
                    member_count,
                )
                .await
            }
        };
        let found = bitable::get_records_by_ids(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            &coordinates,
            std::slice::from_ref(&record_id),
        )
        .await;
        let record = match found {
            Ok(items) => items.into_iter().next(),
            Err(failure) => {
                let settlement = match failure.kind {
                    // 记录被删/无权限：组已建实例时该成员退出本组（设计 §5 Fatal 分支），
                    // 否则整组暂停（还没建单，暂停可重试）。
                    //
                    // **leader 不能摘**：`finish_group` 按 leader 定位组，摘掉它就再也
                    // 收不了尾。leader 记录不可读时整组暂停，等人处理。
                    FailureKind::Fatal { .. } if instance_code.is_some() && task_id != task.id => {
                        if let Err(error) =
                            approval_aggregation::repository::detach_member_as_terminal(
                                &runner.context,
                                task_id,
                                &failure.message,
                            )
                            .await
                        {
                            tracing::warn!(error = %error, task_id, "聚合组成员摘出失败");
                        }
                        tracing::warn!(
                            task_id,
                            record_id = %record_id,
                            "聚合组成员记录不可读，已摘出本组并落终态"
                        );
                        detached += 1;
                        continue;
                    }
                    FailureKind::Fatal { .. } => {
                        GroupSettlement::paused(format!("读取记录失败：{}", failure.message))
                    }
                    _ => GroupSettlement::Retryable(format!("读取记录失败：{}", failure.message)),
                };
                return settle_group(runner, task, group_key, settlement, member_count).await;
            }
        };
        let Some(record) = record else {
            missing.push((task_id, record_id));
            continue;
        };
        let cells = match bitable::rekey_cells_by_field_id(&fields, &record.fields) {
            Ok(cells) => cells,
            Err(error) => {
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::paused(format!("解析记录字段失败：{error}")),
                    member_count,
                )
                .await
            }
        };
        // 外部选项映射按**成员并集**：每个成员各自解析一遍，冲突（同 label 映射到
        // 不同 option_id）判暂停，避免把别人的选择写进组实例。
        let mut widgets = task.widgets.clone();
        match resolve_external_options(
            &runner.context,
            &task.form_snapshot,
            &mut widgets,
            &cells,
            &fields,
        )
        .await
        {
            Ok(()) => {
                if let Err(conflict) = merge_option_maps(&mut pooled, &widgets) {
                    return settle_group(
                        runner,
                        task,
                        group_key,
                        GroupSettlement::paused(conflict),
                        member_count,
                    )
                    .await;
                }
            }
            Err(error) => {
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::paused(format!("外部选项解析失败：{error}")),
                    member_count,
                )
                .await
            }
        }
        // 签名复核（设计 §4.6 第 5 步）：用**该成员自己**已解析的控件集合算，
        // 与播种期 `build_signature` 同一输入，所以可比。
        if let Some(stored) = signature_check {
            if let Some(reason) = verify_group_signature(
                &widgets,
                &task.form_snapshot,
                &cells,
                task.timezone_offset,
                stored,
                &mut signature_hashes,
            ) {
                tracing::info!(
                    stage = "signature",
                    group_key,
                    "聚合组签名复核不一致，整组暂停"
                );
                return settle_group(
                    runner,
                    task,
                    group_key,
                    GroupSettlement::paused(reason),
                    member_count,
                )
                .await;
            }
        }
        // 附件：已建实例时不再搬运（只要编号回填，附件早已在实例里）。
        let attachment_codes = if instance_code.is_some() {
            BTreeMap::new()
        } else {
            match preflight(&widgets, &cells) {
                Ok(_) => {
                    match transfer(
                        runner.transport(),
                        runner.sleeper.as_ref(),
                        &runner.tokens,
                        &coordinates,
                        &record_id,
                        &widgets,
                        &cells,
                    )
                    .await
                    {
                        Ok(codes) => codes,
                        Err(failure) => {
                            let settlement = match failure.kind {
                                FailureKind::Fatal { .. } => GroupSettlement::Paused(format!(
                                    "附件搬运失败：{}",
                                    failure.message
                                )),
                                _ => GroupSettlement::Retryable(format!(
                                    "附件搬运失败：{}",
                                    failure.message
                                )),
                            };
                            return settle_group(runner, task, group_key, settlement, member_count)
                                .await;
                        }
                    }
                }
                Err(error) => {
                    return settle_group(
                        runner,
                        task,
                        group_key,
                        GroupSettlement::paused(format!("附件结构非法：{error}")),
                        member_count,
                    )
                    .await
                }
            }
        };
        rows.push(GroupMemberCells {
            record_id,
            cells,
            attachment_codes,
        });
    }

    // 记录被删：已建实例 → 摘出成员并落终态（上面已逐个处理）；未建实例 → 整组暂停。
    if !missing.is_empty() {
        let list = missing
            .iter()
            .map(|(_, record_id)| record_id.as_str())
            .collect::<Vec<_>>()
            .join("、");
        return settle_group(
            runner,
            task,
            group_key,
            GroupSettlement::paused(format!("多维表格里已找不到组成员记录：{list}")),
            member_count,
        )
        .await;
    }
    if rows.is_empty() {
        return settle_group(
            runner,
            task,
            group_key,
            GroupSettlement::paused("聚合组已无可用成员记录，已暂停"),
            member_count,
        )
        .await;
    }
    if detached > 0 {
        tracing::info!(
            task_id = task.id,
            group_key,
            detached,
            "聚合组处理前摘出了不可读的成员"
        );
    }

    let guarded = approval_execution::GroupExecutionTransport {
        inner: runner.transport(),
        context: &runner.context,
        claim: &task.claim,
        group_key,
    };
    let backfill = BitableBackfill {
        transport: &guarded,
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates: &coordinates,
        fields: &fields,
    };
    let member_inputs: Vec<GroupMemberInput<'_>> = rows
        .iter()
        .map(|row| GroupMemberInput {
            record_id: row.record_id.as_str(),
            cells: &row.cells,
            attachment_codes: row.attachment_codes.clone(),
        })
        .collect();
    let result = dispatch_group(
        &guarded,
        runner.sleeper.as_ref(),
        &runner.tokens,
        &backfill,
        &GroupDispatchInput {
            fields: &fields,
            context: Some(&runner.context),
            execution: Some(&task.claim),
            coordinates: &coordinates,
            group_key,
            group_uuid,
            instance_code,
            backfill_field_name: &backfill_field_name,
            status_field: task.status_field.as_deref(),
            flow_field: task.flow_field.as_deref(),
            start_time_field: task.start_time_field.as_deref(),
            end_time_field: task.end_time_field.as_deref(),
            department_field: task.department_field.as_deref(),
            handler_field: task.handler_field.as_deref(),
            approval_code: &task.approval_code,
            widgets: &pooled,
            form_snapshot: &task.form_snapshot,
            timezone_offset: task.timezone_offset,
            applicant_field: &task.applicant_field,
            applicant_open_id: task.applicant_open_id.as_deref(),
            members: &member_inputs,
        },
    )
    .await;
    let settlement = match result {
        DispatchResult::Backfilled { serial_number, .. } => {
            GroupSettlement::Succeeded(serial_number, group_serial.clone())
        }
        DispatchResult::Terminal { message } => GroupSettlement::Paused(message),
        DispatchResult::CreatedPendingBackfill { message, .. } => {
            GroupSettlement::RetryableWithSerial(message, group_serial)
        }
        _ => {
            let message = match &result {
                DispatchResult::Waiting { reason } => reason.clone(),
                DispatchResult::Retryable { message } => message.clone(),
                _ => "聚合组本轮未完成".to_string(),
            };
            GroupSettlement::Retryable(message)
        }
    };
    settle_group(runner, task, group_key, settlement, member_count).await
}

/// 把成员解析出的选项映射并入整组的并集。
///
/// 同一控件同一文案映射到不同 `option_id` 说明两位成员选了**不同**的业务选项，
/// 合成一张单时必须暂停：随便取一个会把别人的选择写进组实例（设计 §4.6 第 4 步）。
fn merge_option_maps(pooled: &mut [WidgetMap], member: &[WidgetMap]) -> Result<(), String> {
    for widget in member {
        let Some(target) = pooled
            .iter_mut()
            .find(|target| target.widget_id == widget.widget_id)
        else {
            continue;
        };
        for (label, option_id) in &widget.option_map {
            match target.option_map.get(label) {
                None => {
                    target.option_map.insert(label.clone(), option_id.clone());
                }
                Some(existing) if existing == option_id => {}
                Some(_) => {
                    return Err(format!(
                        "同一批记录里控件「{}」的选项「{label}」对应了不同的审批选项，无法合成一个审批实例。请核对后重新点击。",
                        widget.widget_id
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 组成员行的状态（组专用收尾的唯一输入）。
enum GroupSettlement {
    /// 编号已回填全部成员。第二项是处理前**组行**已有的编号（`None` = 首次回填），
    /// 用于把「清空编号后重新点击」的回执与首次成功区分开——对应单条的
    /// `already_initiated` 判定（组编号只落组行，leader 永远没有 `existing_serial`）。
    Succeeded(String, Option<String>),
    /// 整组暂停：`paused` 落 leader 与全体成员，不写回填列（设计 §5）。
    Paused(String),
    /// 可重试：leader 保持 `pending`，按既有退避与 attempts 预算。
    Retryable(String),
    /// 可重试但已知编号：用于「实例已确认、回填未完成」，组行仍有编号可写。
    RetryableWithSerial(String, Option<String>),
}

impl GroupSettlement {
    fn paused(message: impl Into<String>) -> Self {
        Self::Paused(message.into())
    }
    fn retryable(message: impl Into<String>) -> Self {
        Self::Retryable(message.into())
    }
}

/// 组收尾：`finish_group` 一个事务写 leader + 成员 + 组行，再发通知、写审计与指标。
///
/// 这里取代通用 `record_outcome`：组通知按组渲染一次（`group_notification`），
/// 请求日志去重对组不适用（组没有按后的 HTTP 记录），去重靠 leader 的
/// `last_error` 变化（`should_notify` 的语义）。
async fn settle_group(
    runner: &RoundRunner,
    task: &ClaimedTask,
    group_key: &str,
    settlement: GroupSettlement,
    member_count: usize,
) -> TaskOutcome {
    if !approval_execution::is_owned(&runner.context, &task.claim)
        .await
        .unwrap_or(false)
    {
        tracing::warn!(task_id = task.id, "聚合组执行权已失效，拒绝通知与结果写回");
        return TaskOutcome {
            result: DispatchResult::Retryable {
                message: "聚合组执行权已失效".into(),
            },
            creator_open_id: None,
            persisted: true,
            group_size: Some(member_count),
        };
    }

    let (state, result, reason, serial, retry) = match &settlement {
        GroupSettlement::Succeeded(serial, _) => (
            "backfilled",
            DispatchResult::Backfilled {
                serial_number: serial.clone(),
                instance_code: task.instance_code.clone().unwrap_or_default(),
            },
            None,
            Some(serial.clone()),
            false,
        ),
        GroupSettlement::Paused(message) => (
            "paused",
            DispatchResult::Terminal {
                message: message.clone(),
            },
            Some(message.clone()),
            None,
            false,
        ),
        GroupSettlement::Retryable(message) => (
            "pending",
            DispatchResult::Retryable {
                message: message.clone(),
            },
            Some(message.clone()),
            None,
            true,
        ),
        GroupSettlement::RetryableWithSerial(message, serial) => (
            "pending",
            DispatchResult::CreatedPendingBackfill {
                message: message.clone(),
                instance_code: task.instance_code.clone().unwrap_or_default(),
                serial_number: serial.clone(),
            },
            Some(message.clone()),
            serial.clone(),
            true,
        ),
    };

    // 通知：结论变化才发（暂停消息含行号与原因，靠 `last_error` 去重）。
    let notified = if should_notify_group(task, &settlement) {
        let open_id = task.applicant_open_id.as_deref();
        match open_id {
            Some(open_id) => {
                group_notification(runner, task, group_key, open_id, &settlement, member_count)
                    .await
            }
            None => {
                metrics::counter!("feishu_approval_notify_total", "result" => "no_recipient")
                    .increment(1);
                false
            }
        }
    } else {
        true
    };
    let reason = if state == "pending" && !notified {
        reason.map(|reason| format!("通知未送达：{reason}"))
    } else {
        reason
    };

    match approval_execution::finish_group(
        &runner.context,
        &task.claim,
        group_key,
        state,
        serial.as_deref(),
        reason.as_deref(),
        retry,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(task_id = task.id, group_key, "聚合组收尾被拒：执行权已失效");
            return TaskOutcome {
                result,
                creator_open_id: None,
                persisted: true,
                group_size: Some(member_count),
            };
        }
        Err(error) => {
            tracing::error!(%error, task_id = task.id, group_key, "聚合组收尾未能持久确认");
            return TaskOutcome {
                result,
                creator_open_id: None,
                persisted: true,
                group_size: Some(member_count),
            };
        }
    }

    metrics::counter!(
        "feishu_approval_dispatch_total",
        "outcome" => match (&settlement, state) {
            (GroupSettlement::Succeeded(_, _), _) => "backfilled",
            (_, "paused") => "terminal",
            (_, _) => "retryable",
        }
    )
    .increment(member_count as u64);

    if let GroupSettlement::Succeeded(serial, _) = &settlement {
        append_group_audit(runner, task, serial).await;
    }

    TaskOutcome {
        result,
        creator_open_id: None,
        persisted: true,
        group_size: Some(member_count),
    }
}

/// 组通知去重：结论相对上一轮是否变化。
///
/// 组没有按 `record_id` 的 HTTP 请求记录，`http_path_notified` 那套去重不适用；
/// 判据与单条一致——本轮原因 == leader 的 `last_error` 即「上一轮已通知」。
/// 成功是终态（组退出认领集），只在「回填完成」这一轮出现一次，一律通知；
/// 清空编号后重新点击时用户也要拿到这次点击的回执。
fn should_notify_group(task: &ClaimedTask, settlement: &GroupSettlement) -> bool {
    let reason = match settlement {
        GroupSettlement::Succeeded(_, _) => return true,
        GroupSettlement::Paused(message) | GroupSettlement::Retryable(message) => message,
        GroupSettlement::RetryableWithSerial(message, _) => message,
    };
    task.last_error.as_deref() != Some(reason.as_str())
}

/// 组通知正文里「成员行号摘要」的条数上限（超出折成「等 N 条」）。
const GROUP_ROW_SUMMARY: usize = 5;

/// 组暂停消息：失败成员行号 + 原因 + 重新点击提醒。
///
/// 行号是**表内行号**（用户能看到的那一列），不是 `record_id`：暂停通知的唯一
/// 作用是「去表里修哪几行」，贴一串 `recXXXX` 用户无从下手。
fn group_pause_message(size: usize, failing: &[(String, String)], reclick: bool) -> String {
    let mut lines = vec![format!("本组共 {size} 条记录，整组未派发。")];
    if failing.is_empty() {
        lines.push("原因：本组数据未通过校验，详见派发记录。".to_string());
    } else {
        for (row, reason) in failing.iter().take(GROUP_ROW_SUMMARY) {
            lines.push(format!(
                "第 {row} 行：{}",
                approval_feedback::one_line(reason, 120)
            ));
        }
        if failing.len() > GROUP_ROW_SUMMARY {
            lines.push(format!(
                "等 {} 处问题（仅列前 {GROUP_ROW_SUMMARY} 条）。",
                failing.len()
            ));
        }
    }
    if reclick {
        lines.push("下一步：修好上述记录后，请重新勾选本批记录并再次点击批量派发。".to_string());
    }
    lines.join("\n")
}

/// 组通知：一次渲染、成员行号预解析后随正文出站（设计 §4.8）。
///
/// 与单条通知的差别：
/// - 跳过 `http_path_notified` 去重（组没有按 `record_id` 的请求日志）；
/// - 行号在**发送前**一次性扫描并写进正文，不用 `RowNumberEdit` 逐条编辑——
///   那套编辑只能表达单个 `record_id`，同条消息多次编辑是覆盖式更新；
/// - 发送成功返回 `true`，失败只告警（不影响组的执行结论）。
async fn group_notification(
    runner: &RoundRunner,
    task: &ClaimedTask,
    group_key: &str,
    open_id: &str,
    settlement: &GroupSettlement,
    member_count: usize,
) -> bool {
    let coordinates = BitableCoordinates {
        app_token: task.base_token.clone(),
        table_id: task.table_id.clone(),
        view_id: None,
    };
    let members =
        approval_aggregation::repository::load_members(&runner.context, task.config_id, group_key)
            .await
            .unwrap_or_default();
    let record_ids: Vec<String> = members
        .iter()
        .filter_map(|member| member.optional::<String>("record_id").ok().flatten())
        .collect();
    let rows = bitable::record_row_numbers(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
        &record_ids,
    )
    .await
    .unwrap_or_default();

    let (status, reason) = match settlement {
        GroupSettlement::Succeeded(serial, prior) => {
            let row_text = group_row_summary(&rows, &record_ids);
            // 组行已有同一编号 = 清空编号后的人工重发，措辞要说明「已发起过」。
            let lead = if prior.as_deref() == Some(serial.as_str()) {
                format!("已发起过：编号 {serial}（已重新回填）")
            } else {
                format!("编号 {serial} 已回填")
            };
            (
                approval_feedback::FeedbackStatus::Succeeded,
                format!("{lead}本组 {member_count} 条记录。{row_text}"),
            )
        }
        GroupSettlement::Paused(message) => {
            // 暂停原因是**整组级**的（校验失败、签名漂移、缺发起人…），落到每个
            // 成员行都是同一句；所以逐行列出成员行号 + 同一原因，用户照着行号去改。
            let failing: Vec<(String, String)> = record_ids
                .iter()
                .map(|record_id| (row_label(&rows, record_id), message.clone()))
                .collect();
            (
                approval_feedback::FeedbackStatus::Failed,
                group_pause_message(member_count, &failing, true),
            )
        }
        GroupSettlement::Retryable(message) => (
            approval_feedback::FeedbackStatus::Retryable,
            format!("本组 {member_count} 条记录本轮未完成：{message}"),
        ),
        GroupSettlement::RetryableWithSerial(message, _) => (
            approval_feedback::FeedbackStatus::CreatedPendingBackfill,
            format!("本组 {member_count} 条记录回填尚未完成：{message}"),
        ),
    };

    let names = notification_names(runner, task).await;
    let text = approval_feedback::render(
        status,
        &reason,
        &names,
        None,
        None,
        None,
        &task.base_token,
        &task.table_id,
    );
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
        &[],
    )
    .await;
    match result.status {
        crate::addon::feishu::domain::approval_notification::NotificationStatus::Sent => {
            tracing::info!(
                task_id = task.id,
                group_key,
                members = member_count,
                "聚合组通知：IM 消息发送成功"
            );
            true
        }
        status => {
            tracing::warn!(
                task_id = task.id,
                group_key,
                status = ?status,
                message = %result.message,
                "聚合组通知：IM 消息发送失败（不影响审批结论）"
            );
            false
        }
    }
}

/// 通知头部的配置/多维表/数据表名称（读失败退化为「名称暂不可用」）。
async fn notification_names(runner: &RoundRunner, task: &ClaimedTask) -> String {
    let lookup = async {
        runner
            .context
            .approval_configs()
            .query()
            .where_primary_key_eq(serde_json::json!(task.config_id))?
            .optional()
            .await
    };
    let config = match tokio::time::timeout(Duration::from_secs(2), lookup).await {
        Ok(Ok(row)) => row,
        _ => None,
    };
    let name = |field: &str| {
        let value = config
            .as_ref()
            .and_then(|row| row.optional::<String>(field).ok().flatten());
        approval_feedback::display_name(value.as_deref())
    };
    format!(
        "配置：{}\n多维表：{}\n数据表：{}",
        name("title"),
        name("base_name"),
        name("table_name")
    )
}

/// 成员行号摘要：前 [`GROUP_ROW_SUMMARY`] 个，超出折成「等 N 条」。
fn group_row_summary(rows: &HashMap<String, usize>, record_ids: &[String]) -> String {
    if record_ids.is_empty() {
        return String::new();
    }
    let labels: Vec<String> = record_ids
        .iter()
        .take(GROUP_ROW_SUMMARY)
        .map(|record_id| row_label(rows, record_id))
        .collect();
    if record_ids.len() > GROUP_ROW_SUMMARY {
        format!(
            "记录：第 {} 行等 {} 条",
            labels.join("、"),
            record_ids.len()
        )
    } else {
        format!("记录：第 {} 行", labels.join("、"))
    }
}

/// 行号文案：扫不到（20 页上限、视图外）时退化为「未知行」而不是猜。
fn row_label(rows: &HashMap<String, usize>, record_id: &str) -> String {
    match rows.get(record_id) {
        Some(row) => row.to_string(),
        None => "未知".to_string(),
    }
}

/// 组成功审计：以系统身份落一条「谁、何时、对哪组、发了哪个实例」。
async fn append_group_audit(runner: &RoundRunner, task: &ClaimedTask, serial: &str) {
    let constructed: Result<_, BaseError> = (|| {
        let config_entity = audit::entity("feishu_approval_config", task.config_id)?;
        let task_entity = audit::entity("feishu_approval_task", task.id)?;
        let summary = audit::summary([
            ("outcome_code", serde_json::json!("created")),
            ("record_id", serde_json::json!(task.record_id.clone())),
            ("serial_number", serde_json::json!(serial.to_string())),
        ])?;
        audit::succeeded_system_event_without_ctx(
            "feishu-approval-worker",
            "feishu.approval_dispatch",
            Some(config_entity),
            task_entity,
            summary,
        )
    })();
    if let Err(error) = &constructed {
        tracing::error!(error = %error, task_id = task.id, "聚合组审计事件构造失败");
    }
    if let Ok(event) = constructed {
        match runner.tools.mysql() {
            Ok(database) => {
                if let Err(error) = audit::append_independent(database.pool(), &event).await {
                    tracing::error!(error = %error, task_id = task.id, "聚合组审计写入失败");
                }
            }
            Err(error) => {
                tracing::error!(error = %error, task_id = task.id, "聚合组审计取库失败");
            }
        }
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
        // 接收人链：任务表申请人 → **记录创建人**（兜底）。发起人只来自点击人，
        // 所以这里不再读表格的申请人列（那是系统回写的产物）；创建人兜底保证
        // 「用户发起过请求」这件事在任何结论下都有反馈出口。两者都取不到才放弃——
        // 那时「发给谁」在事实上无解，但**不能静默**：状态会落成 `unnotified`。
        //
        // 注意「可查」的边界：`list_tasks` 的 `where` 树支持按 `state` 过滤，但控制台
        // 的任务下钻视图目前**只按 config_id 查、没有状态筛选**，所以这些行现在只能
        // 通过接口/SQL 捞，不能指望有人在页面上顺手看见。
        let open_id = task
            .applicant_open_id
            .as_deref()
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
                    "Worker 通知：任务发起人与记录创建人均不可用，本轮结论未送达"
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
    // 清空编号后的人工重发：结论是 `Backfilled` 但任务行早已有编号。
    // 无论哪条路径，点击时间都取**这次点击**的请求记录（见下）。
    let reinitiated = already_initiated(task, outcome);
    let clicked_at = match request_log_check.await {
        Ok(rows) => {
            // 清空编号后的人工重发：历史 HTTP 记录（含 outcome=succeeded 的那条）
            // 不能压掉这条「已发起过、已重新回填」的通知——用户要的是这次点击的回执。
            if rows.iter().any(http_path_notified) && !reinitiated {
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
    let (accepted, reason, serial) = outcome.response_parts();
    // 通知不关心 HTTP 语义的 accepted。
    let _ = accepted;
    // 人工重发不写「点击时间」：这条不是一次新的创建，措辞与时间都会误导。
    let clicked_at = clicked_at
        .filter(|_| !reinitiated)
        .map(|seconds| format_local(seconds, task.timezone_offset));
    let reason = match (&serial, reinitiated) {
        (Some(serial), true) => approval_feedback::already_initiated_message(serial),
        _ => reason,
    };
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

    /// Task 0 的接口断言：字段必须是 trait 对象，真实库测试才能注入回放传输。
    /// 只做编译期检查，无运行时行为。
    #[test]
    fn round_runner_transport_is_injectable_trait_object() {
        let field_type: fn(&RoundRunner) -> &Arc<dyn OutboundTransport> =
            |runner| &runner.transport;
        let _ = field_type;
    }

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

    /// 飞书响应体里的 log_id 每轮不同；去重键必须忽略它，否则同一根因每轮刷 WARN。
    #[test]
    fn seed_failure_dedup_ignores_volatile_log_id() {
        let failures = Mutex::new(HashMap::new());
        let first = r#"读取表结构失败：HTTP 200: {"code":1254041,"msg":"TableIdNotFound","error":{"log_id":"202610302006475EEE222FAB5367081525"}}"#;
        let second = r#"读取表结构失败：HTTP 200: {"code":1254041,"msg":"TableIdNotFound","error":{"log_id":"20261030200711470CD6846424D37AE20A"}}"#;
        assert_eq!(
            note_seed_failure(&failures, 24, first),
            SeedFailureNote::Warn
        );
        assert_eq!(
            note_seed_failure(&failures, 24, second),
            SeedFailureNote::Repeat,
            "仅 log_id 变化不得重复告警"
        );
        let changed = first.replace("TableIdNotFound", "PermissionDenied");
        assert_eq!(
            note_seed_failure(&failures, 24, &changed),
            SeedFailureNote::Warn,
            "根因变化仍要重新告警"
        );
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
            existing_serial: None,
            group_id: None,
            group_seq: 0,
            effect_phase: "ready".to_string(),
            instance_code: None,
        }
    }

    /// 清空编号后的重发：结论是 `Backfilled` 且**处理前**任务行已有编号。
    #[test]
    fn already_initiated_requires_a_backfill_and_a_known_serial() {
        let mut task = claimed(None);
        let success = DispatchResult::Backfilled {
            serial_number: "S".into(),
            instance_code: "I".into(),
        };
        assert!(
            !already_initiated(&task, &success),
            "首次成功时任务行还没有编号，不是重发"
        );
        task.existing_serial = Some("S".into());
        assert!(
            already_initiated(&task, &success),
            "已有编号 + 再次回填 = 重发"
        );
        assert!(
            !already_initiated(
                &task,
                &DispatchResult::Waiting {
                    reason: "缺字段".into()
                }
            ),
            "失败/等待不是重发"
        );
    }

    /// 选项并集：同 label 同 option_id 合并，冲突判暂停（否则会把别人的选择写进组实例）。
    #[test]
    fn option_union_merges_and_rejects_conflicting_labels() {
        let widget = |option_map: &[(&str, &str)]| WidgetMap {
            widget_id: "wSel".to_string(),
            widget_type: "radioV2".to_string(),
            required: false,
            bitable_field: "fldSel".to_string(),
            interval_end_field: None,
            converter: crate::addon::feishu::domain::approval_convert::Converter::Option,
            option_map: option_map
                .iter()
                .map(|(label, id)| ((*label).to_string(), (*id).to_string()))
                .collect(),
            option_source_key: None,
            currency: None,
        };
        let mut pooled = vec![widget(&[("差旅", "opt-1")])];
        assert!(merge_option_maps(&mut pooled, &[widget(&[("差旅", "opt-1")])]).is_ok());
        assert!(
            merge_option_maps(&mut pooled, &[widget(&[("采购", "opt-2")])]).is_ok(),
            "新文案必须并入并集"
        );
        assert_eq!(pooled[0].option_map.len(), 2);
        let conflict = merge_option_maps(&mut pooled, &[widget(&[("差旅", "opt-9")])]);
        assert!(
            matches!(&conflict, Err(reason) if reason.contains("不同的审批选项")),
            "{conflict:?}"
        );
    }

    /// 组签名复核：同一签名的两名成员通过与组行一致；任一成员与播种摘要不同、
    /// 或两名成员彼此不再一致，都判暂停（设计 §4.6 第 5 步）。
    #[test]
    fn group_signature_check_pauses_on_drift_or_mismatch() {
        let widgets = aggregate_widgets();
        let timezone =
            FixedOffset::from_seconds(8 * 3600).unwrap_or_else(|error| panic!("{error}"));
        let cells = |text: &str| {
            serde_json::Map::from_iter([("fldText".to_string(), serde_json::json!(text))])
        };
        let stored = aggregate_signature_hash("报销");

        // 两名成员都是「报销」：与组行摘要一致，不暂停。
        let mut hashes = Vec::new();
        for _ in 0..2 {
            assert!(
                verify_group_signature(
                    &widgets,
                    AGGREGATE_FORM_SNAPSHOT,
                    &cells("报销"),
                    timezone,
                    &stored,
                    &mut hashes,
                )
                .is_none(),
                "同签名成员不得被判暂停"
            );
        }
        assert_eq!(hashes.len(), 2);

        // 与组行摘要不同（排队期间被改）：暂停。
        let mut hashes = Vec::new();
        let drift = verify_group_signature(
            &widgets,
            AGGREGATE_FORM_SNAPSHOT,
            &cells("改了"),
            timezone,
            &stored,
            &mut hashes,
        );
        assert!(
            drift
                .as_deref()
                .is_some_and(|reason| reason.contains("排队期间被修改")),
            "{drift:?}"
        );

        // 两名成员彼此不一致：暂停。
        let mut hashes = Vec::new();
        assert!(verify_group_signature(
            &widgets,
            AGGREGATE_FORM_SNAPSHOT,
            &cells("报销"),
            timezone,
            &stored,
            &mut hashes,
        )
        .is_none());
        let mismatch = verify_group_signature(
            &widgets,
            AGGREGATE_FORM_SNAPSHOT,
            &cells("另一批"),
            timezone,
            &stored,
            &mut hashes,
        );
        assert!(
            mismatch
                .as_deref()
                .is_some_and(|reason| reason.contains("不再一致")),
            "{mismatch:?}"
        );
    }

    /// 组暂停消息必须把「重新点击」这一步说清楚：暂停组不会被轮询自动重试，
    /// 消息不说重试方式的话，用户只能看着失败记录等。
    #[test]
    fn pause_message_mentions_reclick() {
        let message = group_pause_message(3, &[("7".to_string(), "明细为空".to_string())], true);
        assert!(message.contains("3 条"), "{message}");
        assert!(message.contains("第 7 行"), "{message}");
        assert!(message.contains("明细为空"), "{message}");
        assert!(message.contains("重新勾选"), "{message}");
        assert!(message.contains("再次点击批量派发"), "{message}");

        // 无明细原因时也要给出可执行提示，且不写「详见派发记录」以外的废话。
        let generic = group_pause_message(1, &[], true);
        assert!(generic.contains("未通过校验"), "{generic}");
        assert!(generic.contains("重新勾选"), "{generic}");

        // 失败项超过上限时折成「等 N 处问题」，不把正文撑爆。
        let many: Vec<(String, String)> = (1..=9)
            .map(|row| (row.to_string(), "原因".to_string()))
            .collect();
        let capped = group_pause_message(9, &many, false);
        assert_eq!(capped.matches("第 ").count(), GROUP_ROW_SUMMARY, "{capped}");
        assert!(capped.contains("等 9 处问题"), "{capped}");
        assert!(!capped.contains("重新勾选"), "reclick=false 时不写重试提醒");
    }

    #[test]
    fn round_result_labels_cover_every_outcome() {
        let success = DispatchResult::Backfilled {
            serial_number: "S1".into(),
            instance_code: "I".into(),
        };
        assert_eq!(round_result_label(None, &success, None), "成功：编号 S1");
        assert_eq!(
            round_result_label(Some("S1"), &success, None),
            "已发起过：编号 S1（已重新回填）"
        );
        assert!(round_result_label(
            None,
            &DispatchResult::Waiting {
                reason: "缺少必填值".into()
            },
            None
        )
        .starts_with("待补齐："));
        for outcome in [
            DispatchResult::Retryable {
                message: "超时".into(),
            },
            DispatchResult::CreatedPendingBackfill {
                message: "编号未生成".into(),
                instance_code: "I".into(),
                serial_number: None,
            },
        ] {
            assert!(round_result_label(None, &outcome, None).starts_with("待重试："));
        }
        assert!(round_result_label(
            None,
            &DispatchResult::Terminal {
                message: "映射坏了".into()
            },
            None
        )
        .starts_with("失败："));
        // 逐条结果截断到约 80 字符。
        let long = round_result_label(
            None,
            &DispatchResult::Terminal {
                message: "长".repeat(200),
            },
            None,
        );
        assert_eq!(long.chars().count(), 80);
    }

    #[test]
    fn round_summary_reports_hits_zero_and_backlog() {
        let text = round_summary_text(
            "财务审批",
            "base1",
            "tbl1",
            3,
            2,
            &[
                ("recAAAAAAAAAA".into(), "成功：编号 S1".into()),
                ("recBBBBBBBBBB".into(), "失败：映射坏了".into()),
            ],
            Some(7),
            Some("请填写申请人"),
            None,
        );
        assert!(text.starts_with("【批量派发结果】"), "{text}");
        assert!(text.contains("配置：财务审批"), "{text}");
        assert!(text.contains("本轮命中 5 条（新提交 3，重试 2）"), "{text}");
        assert!(text.contains("AAAAAA 成功：编号 S1"), "{text}");
        assert!(
            text.contains("积压 7 条待处理，多数原因：请填写申请人"),
            "{text}"
        );
        assert!(text.contains("技术定位：Base=base1；表=tbl1"), "{text}");

        // 零命中：不能只说「命中 0 条」，要写清「本轮无新增待处理记录」。
        let zero = round_summary_text("财务审批", "b", "t", 0, 0, &[], Some(0), None, None);
        assert!(zero.contains("本轮无新增待处理记录"), "{zero}");
        assert!(!zero.contains("积压"), "零积压不写积压行：{zero}");

        // 扫描失败：原因行写失败原因，而不是「没有需要处理的记录」。
        let failed = round_summary_text(
            "财务审批",
            "b",
            "t",
            0,
            0,
            &[],
            None,
            None,
            Some("读取表结构失败：TableIdNotFound"),
        );
        assert!(
            failed.contains("本轮扫描失败：读取表结构失败：TableIdNotFound"),
            "{failed}"
        );

        // 命中但本轮还没轮到认领（游标/批量上限）：不能自相矛盾地说
        // 「命中 2 条」又「没有需要处理的记录」。
        let queued = round_summary_text("财务审批", "b", "t", 2, 0, &[], None, None, None);
        assert!(queued.contains("本轮命中 2 条"), "{queued}");
        assert!(queued.contains("已入队"), "{queued}");
        assert!(!queued.contains("没有需要处理的记录"), "{queued}");

        // 有积压但 pending 行都没有非空原因：只写条数，不落展示名的
        // 「名称暂不可用」兜底。
        let no_reason = round_summary_text("财务审批", "b", "t", 0, 0, &[], Some(3), None, None);
        assert!(no_reason.contains("积压 3 条待处理"), "{no_reason}");
        assert!(!no_reason.contains("名称暂不可用"), "{no_reason}");
        assert!(!no_reason.contains("多数原因"), "{no_reason}");
    }

    #[test]
    fn round_summary_caps_entries_and_total_length() {
        let entries: Vec<(String, String)> = (0..12)
            .map(|index| (format!("rec{index:012}"), "失败：很长的原因".to_string()))
            .collect();
        let text = round_summary_text("t", "b", "t", 12, 0, &entries, None, None, None);
        assert!(text.contains("等 12 条"), "超出 8 条要写等 N 条：{text}");
        assert_eq!(text.matches("失败：很长的原因").count(), 8);

        let many: Vec<(String, String)> = (0..200)
            .map(|index| {
                (
                    format!("rec{index}"),
                    "失败：".to_string() + &"因".repeat(300),
                )
            })
            .collect();
        let long = round_summary_text("t", "b", "t", 200, 0, &many, Some(999), None, None);
        assert_eq!(long.chars().count(), 1800, "整体截到 1800 字符");
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
            let hit = found
                .get(&target)
                .unwrap_or_else(|| panic!("{total} 行历史后仍应命中本轮记录"));
            assert_eq!(
                (hit.id, hit.state.as_str()),
                (expected_id, "pending"),
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
                    found.get(record_id).map(|task| task.state.as_str()),
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
        let hit = found
            .get(&shared)
            .unwrap_or_else(|| panic!("按 config_id 收窄后应命中本配置的任务行"));
        assert_eq!(
            (hit.id, hit.state.as_str()),
            (own_id, "pending"),
            "按 config_id 收窄后不应读到另一配置的同 record_id 行"
        );
        assert!(
            found.values().all(|task| task.id != foreign_id),
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

    // -----------------------------------------------------------------------
    // 聚合播种：锁配置行 → 单条 reopen → 组维护 → 建组/建行（真实 MySQL）
    // -----------------------------------------------------------------------

    /// 聚合签名的审批定义快照：一个普通文本控件，足以覆盖「同值合并 / 异值拆组」。
    const AGGREGATE_FORM_SNAPSHOT: &str =
        r#"[{"id":"wName","name":"事由","type":"input","required":false}]"#;

    /// 脚本化出站传输：按顺序回放预置响应并记录请求，播种测试不碰真实飞书。
    struct SeedTransport {
        responses: Mutex<
            std::collections::VecDeque<crate::addon::feishu::domain::outbound::OutboundResponse>,
        >,
        requests: Mutex<Vec<crate::addon::feishu::domain::outbound::OutboundRequest>>,
    }

    impl SeedTransport {
        fn new(bodies: Vec<String>) -> Self {
            Self {
                responses: Mutex::new(
                    bodies
                        .into_iter()
                        .map(
                            |body| crate::addon::feishu::domain::outbound::OutboundResponse {
                                status: 200,
                                body,
                                bytes: None,
                                headers: BTreeMap::new(),
                            },
                        )
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests
                .lock()
                .map(|guard| guard.len())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for SeedTransport {
        async fn send(
            &self,
            request: crate::addon::feishu::domain::outbound::OutboundRequest,
        ) -> Result<crate::addon::feishu::domain::outbound::OutboundResponse, BaseError> {
            self.requests
                .lock()
                .map_err(|error| BaseError::ConfigError(error.to_string()))?
                .push(request);
            self.responses
                .lock()
                .map_err(|error| BaseError::ConfigError(error.to_string()))?
                .pop_front()
                .ok_or_else(|| BaseError::ConfigError("播种测试的脚本响应已耗尽".to_string()))
        }
    }

    /// 字段列表响应：回填列、批量范围复选框列、签名来源文本列。
    fn fields_envelope() -> String {
        serde_json::json!({
            "code": 0,
            "msg": "",
            "data": {
                "has_more": false,
                "total": 3,
                "items": [
                    {"field_id": "fldNumber", "field_name": "审批编号", "type": 15},
                    {"field_id": "fldScope", "field_name": "纳入批量", "type": 7},
                    {"field_id": "fldText", "field_name": "事由", "type": 1},
                ]
            }
        })
        .to_string()
    }

    /// 查询记录响应：`(record_id, 事由文本)`，字段按**列名**作键（官方形态）。
    fn records_envelope(items: &[(&str, &str)]) -> String {
        let rows: Vec<serde_json::Value> = items
            .iter()
            .map(|(record_id, text)| {
                serde_json::json!({
                    "record_id": record_id,
                    "fields": {"事由": text},
                })
            })
            .collect();
        serde_json::json!({
            "code": 0,
            "msg": "",
            "data": {"has_more": false, "items": rows}
        })
        .to_string()
    }

    fn insert_aggregate_config_sql() -> &'static str {
        "INSERT INTO feishu_approval_config \
         (title,base_token,table_id,approval_code,applicant_field,backfill_field,\
          batch_scope_field,base_timezone,aggregate_batch,form_snapshot,enabled,\
          created_at,updated_at) \
         VALUES ('聚合播种测试',?,?,'DEF-AGG','fldPerson','fldNumber',\
                 'fldScope','Asia/Shanghai',?,?,true,NOW(),NOW())"
    }

    async fn insert_aggregate_config(
        pool: &sqlx::MySqlPool,
        namespace: &str,
        suffix: &str,
        aggregate: bool,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query(insert_aggregate_config_sql())
            .bind(format!("appAgg{namespace}"))
            .bind(format!("tbl{namespace}{suffix}"))
            .bind(aggregate)
            .bind(AGGREGATE_FORM_SNAPSHOT)
            .execute(pool)
            .await?
            .last_insert_id() as i64)
    }

    async fn insert_aggregate_field_map(
        pool: &sqlx::MySqlPool,
        config_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO feishu_approval_field_map \
             (config_id,widget_id,widget_type,required,bitable_field,bitable_field_name,\
              converter,created_at,updated_at) \
             VALUES (?,'wName','input',false,'fldText','事由','direct',NOW(),NOW())",
        )
        .bind(config_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_group_row(
        pool: &sqlx::MySqlPool,
        config_id: i64,
        group_key: &str,
        phase: &str,
        instance_code: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO feishu_approval_group \
             (config_id,group_key,signature_hash,uuid,effect_phase,instance_code,\
              created_at,updated_at) \
             VALUES (?,?,?,?,?,?,NOW(),NOW())",
        )
        .bind(config_id)
        .bind(group_key)
        .bind(format!("sig-{group_key}"))
        .bind(format!("uuid-{group_key}"))
        .bind(phase)
        .bind(instance_code)
        .execute(pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_group_task_row(
        pool: &sqlx::MySqlPool,
        config_id: i64,
        group_key: &str,
        group_seq: i64,
        record_id: &str,
        state: &str,
        effect_phase: &str,
        serial_number: Option<&str>,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query(
            "INSERT INTO feishu_approval_task \
             (config_id,record_id,uuid,state,group_id,group_seq,effect_phase,\
              serial_number,created_at,updated_at) \
             VALUES (?,?,?,?,?,?,?,?,NOW(),NOW())",
        )
        .bind(config_id)
        .bind(record_id)
        .bind(format!("uuid-{record_id}"))
        .bind(state)
        .bind(group_key)
        .bind(group_seq)
        .bind(effect_phase)
        .bind(serial_number)
        .execute(pool)
        .await?
        .last_insert_id() as i64)
    }

    async fn load_seed_config(context: &FeishuContext, config_id: i64) -> anyhow::Result<Record> {
        context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(config_id))?
            .optional()
            .await?
            .ok_or_else(|| anyhow::anyhow!("测试配置不存在"))
    }

    /// 用真实 MySQL 连接池构造 RoundRunner（事务需要 `tools.mysql()`）。
    async fn build_seed_runner(
        database: yang_db::Database,
        context: Arc<FeishuContext>,
        transport: Arc<SeedTransport>,
    ) -> anyhow::Result<RoundRunner> {
        let transport: Arc<dyn OutboundTransport> = transport;
        let tokens = Arc::new(TenantTokenProvider::new(
            Arc::new(FakeTokenCache),
            Arc::clone(&transport),
            Arc::new(TokioSleeper),
            FeishuCredentials {
                app_id: "test".into(),
                app_secret: "test".into(),
            },
            "aggregate-seed-test",
        )?);
        Ok(RoundRunner {
            tools: Arc::new(
                yang_base::tools::ToolsBuilder::new()
                    .mysql(database)
                    .build()?,
            ),
            context,
            tokens,
            transport,
            sleeper: Arc::new(TokioSleeper),
            timezone_offset: FixedOffset::from_iana("Asia/Shanghai")?,
            heartbeat: ProgressHeartbeat::default(),
            seed_failures: Arc::default(),
        })
    }

    async fn cleanup_aggregate_fixture(
        pool: &sqlx::MySqlPool,
        config_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "DELETE FROM feishu_approval_task_applicant WHERE task_id IN \
             (SELECT id FROM feishu_approval_task WHERE config_id=?)",
        )
        .bind(config_id)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM feishu_approval_task WHERE config_id=?")
            .bind(config_id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM feishu_approval_group WHERE config_id=?")
            .bind(config_id)
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
        Ok(())
    }

    /// 同签名的两条记录合成为一个组：一行组行 + 两条任务行（`group_seq` 0/1）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn aggregate_seed_creates_one_group_with_two_members() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", true).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&r1, "报销"), (&r2, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.seeded, 2, "两条记录都成为组成员");
        assert!(outcome.touched.contains(&r1) && outcome.touched.contains(&r2));
        assert_eq!(transport.request_count(), 2, "播种只应读取字段与记录各一次");

        let groups: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT group_key,uuid,effect_phase FROM feishu_approval_group WHERE config_id=?",
        )
        .bind(config_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(groups.len(), 1, "同签名必须合成一个组");
        let (group_key, group_uuid_value, phase) = &groups[0];
        assert_eq!(phase, "ready");
        assert_eq!(group_uuid_value.len(), 36, "组 uuid 必须是规范 UUID 形态");

        let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
            "SELECT record_id,group_id,group_seq FROM feishu_approval_task \
             WHERE config_id=? ORDER BY group_seq",
        )
        .bind(config_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (r1.clone(), Some(group_key.clone()), 0));
        assert_eq!(rows[1], (r2.clone(), Some(group_key.clone()), 1));

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 单条点击不建组：聚合开关打开时，被点记录仍按单条任务入队。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn aggregate_seed_only_runs_on_batch_click() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", true).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&r1, "报销"), (&r2, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            Some(&r1),
        )
        .await?;

        assert_eq!(outcome.seeded, 1);
        assert_eq!(outcome.touched, vec![r1.clone()], "单条点击只碰被点记录");
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(groups, 0, "单条点击不得建组");
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT record_id,group_id FROM feishu_approval_task WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(row, (r1, None), "单条任务不得带组键");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 开关关闭时逐条建任务行，不建组。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn aggregate_off_keeps_one_instance_per_record() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&r1, "报销"), (&r2, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.seeded, 2);
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(groups, 0, "开关关闭不得建组");
        let rows: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT group_id FROM feishu_approval_task WHERE config_id=? ORDER BY record_id",
        )
        .bind(config_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(rows, vec![None, None], "每条记录各自一条单条任务");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 只解散「全组成员都在本次勾选内、阶段 ready、无租约」的组；勾选外的组不动。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn batch_click_dissolves_only_fully_selected_ready_groups() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_a = format!("ga{namespace}");
        let group_b = format!("gb{namespace}");
        let a1 = format!("{namespace}-a1");
        let a2 = format!("{namespace}-a2");
        let b1 = format!("{namespace}-b1");
        let b2 = format!("{namespace}-b2");
        insert_group_row(&pool, config_id, &group_a, "ready", None).await?;
        insert_group_row(&pool, config_id, &group_b, "ready", None).await?;
        insert_group_task_row(&pool, config_id, &group_a, 0, &a1, "pending", "ready", None).await?;
        insert_group_task_row(&pool, config_id, &group_a, 1, &a2, "pending", "ready", None).await?;
        insert_group_task_row(&pool, config_id, &group_b, 0, &b1, "pending", "ready", None).await?;
        insert_group_task_row(&pool, config_id, &group_b, 1, &b2, "pending", "ready", None).await?;
        // 勾选 a1/a2/b1（b2 未被重新勾选）。
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&a1, "报销"), (&a2, "报销"), (&b1, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        let groups: Vec<String> =
            sqlx::query_scalar("SELECT group_key FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_all(&pool)
                .await?;
        assert_eq!(
            groups,
            vec![group_b.clone()],
            "未全选的组必须保留，全选的组被解散"
        );

        let a1_group: Option<String> =
            sqlx::query_scalar("SELECT group_id FROM feishu_approval_task WHERE record_id=?")
                .bind(&a1)
                .fetch_one(&pool)
                .await?;
        assert_eq!(a1_group, None, "解散后的成员重建为单条任务");
        let b1_group: Option<String> =
            sqlx::query_scalar("SELECT group_id FROM feishu_approval_task WHERE record_id=?")
                .bind(&b1)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            b1_group.as_deref(),
            Some(group_b.as_str()),
            "勾选外的组成员不动"
        );

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// review/lookup 组点击后复位 leader，**组行阶段保留**（外部副作用事实）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn batch_click_reopens_review_group_preserving_group_phase() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_key = format!("gr{namespace}");
        let leader_record = format!("{namespace}-r1");
        let member_record = format!("{namespace}-r2");
        insert_group_row(&pool, config_id, &group_key, "lookup", None).await?;
        let leader_id = insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            0,
            &leader_record,
            "review",
            "ready",
            None,
        )
        .await?;
        insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            1,
            &member_record,
            "review",
            "ready",
            None,
        )
        .await?;
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&leader_record, "报销"), (&member_record, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.reopened, 1, "review leader 应被复位一次");
        let leader: (String, String) =
            sqlx::query_as("SELECT state,effect_phase FROM feishu_approval_task WHERE id=?")
                .bind(leader_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(leader, ("pending".to_string(), "ready".to_string()));
        let phase: String =
            sqlx::query_scalar("SELECT effect_phase FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(phase, "lookup", "组行阶段不得被 reopen 改写");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 已提交组的成员编号被清空：复位 leader 重新回填，组行实例码与阶段保留。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn submitted_group_with_cleared_member_serial_reopens_leader_for_backfill(
    ) -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_key = format!("gs{namespace}");
        let leader_record = format!("{namespace}-r1");
        let member_record = format!("{namespace}-r2");
        insert_group_row(&pool, config_id, &group_key, "created", Some("INST-SEED")).await?;
        let leader_id = insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            0,
            &leader_record,
            "backfilled",
            "ready",
            None,
        )
        .await?;
        insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            1,
            &member_record,
            "backfilled",
            "ready",
            None,
        )
        .await?;
        // 只勾选成员记录：它的回填列被用户清空（leader 的编号还在，不在扫描结果里）。
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&member_record, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.reopened, 1);
        let leader: (String, String) =
            sqlx::query_as("SELECT state,effect_phase FROM feishu_approval_task WHERE id=?")
                .bind(leader_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            leader,
            ("pending".to_string(), "created".to_string()),
            "已提交组复位到 created（实例已存在，处理期按实例码恢复）"
        );
        let group: (String, Option<String>) = sqlx::query_as(
            "SELECT effect_phase,instance_code FROM feishu_approval_group WHERE config_id=?",
        )
        .bind(config_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            group,
            ("created".to_string(), Some("INST-SEED".to_string()))
        );
        // 组 leader 不在 touched 里，受理发起人必须由组维护自己落库。
        assert_eq!(
            applicant_of(&pool, leader_id).await?,
            Some("ou_click".to_string()),
            "复位后的组 leader 必须带上本轮点击人"
        );

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 聚合开启不影响单条任务的人工重试：review 行照常复位。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn aggregate_on_still_reopens_plain_review_task() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", true).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let record_id = format!("{namespace}-r1");
        let task_id = insert_single_task(
            &pool,
            config_id,
            &record_id,
            &format!("{namespace}-plain"),
            "review",
        )
        .await?;
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&record_id, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.reopened, 1);
        let state: String = sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
            .bind(task_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(state, "pending", "单条 review 行照常被点击重开");
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(groups, 0, "已有的单条任务不参与聚合，也不新建组");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 开关关闭时，未提交组被解散后按逐条任务重建（回滚路径）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn aggregate_off_dissolves_unsubmitted_group_into_individual_tasks() -> anyhow::Result<()>
    {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_key = format!("gd{namespace}");
        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        insert_group_row(&pool, config_id, &group_key, "ready", None).await?;
        insert_group_task_row(
            &pool, config_id, &group_key, 0, &r1, "pending", "ready", None,
        )
        .await?;
        insert_group_task_row(
            &pool, config_id, &group_key, 1, &r2, "pending", "ready", None,
        )
        .await?;
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&r1, "报销"), (&r2, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        let outcome = seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        assert_eq!(outcome.seeded, 2, "解散后两条记录各自重建单条任务");
        assert!(outcome.touched.contains(&r1) && outcome.touched.contains(&r2));
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(groups, 0, "未提交组应被解散");
        let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
            "SELECT record_id,group_id,group_seq FROM feishu_approval_task \
             WHERE config_id=? ORDER BY record_id",
        )
        .bind(config_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            rows,
            vec![(r1, None, 0), (r2, None, 0),],
            "解散后的成员必须摘掉组键"
        );

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 解散同时清掉 applicant 行：任务行删了而属性扩展留着，就是孤儿 PII。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn dissolve_removes_member_rows_and_applicant_rows() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_key = format!("ge{namespace}");
        let r1 = format!("{namespace}-r1");
        let r2 = format!("{namespace}-r2");
        insert_group_row(&pool, config_id, &group_key, "ready", None).await?;
        let leader_id = insert_group_task_row(
            &pool, config_id, &group_key, 0, &r1, "pending", "ready", None,
        )
        .await?;
        let member_id = insert_group_task_row(
            &pool, config_id, &group_key, 1, &r2, "pending", "ready", None,
        )
        .await?;
        sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?)")
            .bind(leader_id)
            .bind("ou_old_leader")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?)")
            .bind(member_id)
            .bind("ou_old_member")
            .execute(&pool)
            .await?;
        let transport = Arc::new(SeedTransport::new(vec![
            fields_envelope(),
            records_envelope(&[(&r1, "报销"), (&r2, "报销")]),
        ]));
        let runner =
            build_seed_runner(database, Arc::clone(&context), Arc::clone(&transport)).await?;
        let config = load_seed_config(&context, config_id).await?;

        seed_config(
            &runner,
            &config,
            RoundTrigger::Manual(config_id),
            Some("ou_click"),
            None,
        )
        .await?;

        let old_applicants: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feishu_approval_task_applicant WHERE task_id IN (?,?)",
        )
        .bind(leader_id)
        .bind(member_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(old_applicants, 0, "被删除任务行的 applicant 行必须一并清掉");
        let old_tasks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE id IN (?,?)")
                .bind(leader_id)
                .bind(member_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(old_tasks, 0, "解散删除的是旧任务行");
        let rebuilt: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feishu_approval_task WHERE config_id=? AND group_id IS NULL",
        )
        .bind(config_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(rebuilt, 2, "两条记录重建为单条任务");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }

    /// 组维护的复位/摘组/删行必须在行锁内复检租约、状态与外部写入阶段。
    ///
    /// 模拟「事务外读到无租约，写入前被另一实例认领」的窗口：行里已是有效租约，
    /// 三个写入口都必须拒绝；任何一条守卫被摘掉，这里就会变红（设计 §6.1）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_maintenance_rechecks_lease_state_and_phase() -> anyhow::Result<()> {
        let (database, context) = real_mysql_context().await?;
        let pool = context.approval_tasks().pool().clone();
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id = insert_aggregate_config(&pool, &namespace, "a", false).await?;
        insert_aggregate_field_map(&pool, config_id).await?;
        let group_key = format!("gk{namespace}");
        let leader_record = format!("{namespace}-r1");
        let member_record = format!("{namespace}-r2");
        insert_group_row(&pool, config_id, &group_key, "ready", None).await?;
        let leader_id = insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            0,
            &leader_record,
            "pending",
            "ready",
            None,
        )
        .await?;
        let member_id = insert_group_task_row(
            &pool,
            config_id,
            &group_key,
            1,
            &member_record,
            "pending",
            "ready",
            None,
        )
        .await?;
        // 另一实例已经把两行都认领走（有效租约）。
        sqlx::query(
            "UPDATE feishu_approval_task SET worker_id='other-instance', \
             lease_until=UNIX_TIMESTAMP()+300 WHERE config_id=?",
        )
        .bind(config_id)
        .execute(&pool)
        .await?;
        let transport: Arc<SeedTransport> = Arc::new(SeedTransport::new(vec![]));
        let runner = build_seed_runner(database, Arc::clone(&context), transport).await?;

        // 1. 有效租约：复位、摘组、删行全部拒绝。
        let mut transaction = runner.tools.mysql()?.transaction().await?;
        assert!(
            !approval_aggregation::repository::reopen_group_leader_in_tx(
                &mut transaction,
                &runner.context,
                leader_id,
                "created",
            )
            .await?,
            "有有效租约的 leader 不得被复位"
        );
        assert!(
            !approval_aggregation::repository::detach_task_from_group_in_tx(
                &mut transaction,
                &runner.context,
                member_id,
            )
            .await?,
            "有有效租约的成员不得被摘组"
        );
        assert_eq!(
            approval_aggregation::repository::delete_task_rows_in_tx(
                &mut transaction,
                &runner.context,
                &[leader_id, member_id],
            )
            .await?,
            0,
            "有有效租约的任务行不得被删除"
        );
        transaction.commit().await?;
        let leader: (String, Option<String>, Option<i64>, Option<String>, String) = sqlx::query_as(
            "SELECT state,worker_id,lease_until,group_id,effect_phase \
                 FROM feishu_approval_task WHERE id=?",
        )
        .bind(leader_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(leader.0, "pending", "复位不得清掉在途状态");
        assert_eq!(leader.1.as_deref(), Some("other-instance"));
        assert!(leader.2.unwrap_or_default() > 0, "租约不得被清空");
        assert_eq!(leader.3.as_deref(), Some(group_key.as_str()), "不得摘组");
        assert_eq!(leader.4, "ready", "外部写入阶段不得被改写");

        // 2. 租约过期但已进入外部写入：删行仍必须拒绝。
        sqlx::query(
            "UPDATE feishu_approval_task SET worker_id=NULL, lease_until=NULL WHERE config_id=?",
        )
        .bind(config_id)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE feishu_approval_task SET effect_phase='creating' WHERE id=?")
            .bind(member_id)
            .execute(&pool)
            .await?;
        let mut transaction = runner.tools.mysql()?.transaction().await?;
        assert_eq!(
            approval_aggregation::repository::delete_task_rows_in_tx(
                &mut transaction,
                &runner.context,
                &[leader_id, member_id],
            )
            .await?,
            0,
            "effect_phase 非 ready 的行不得被删除"
        );
        transaction.commit().await?;

        // 3. 状态绑定外部事实（created）：同样不得删除。
        sqlx::query(
            "UPDATE feishu_approval_task SET state='created', effect_phase='ready' WHERE id=?",
        )
        .bind(member_id)
        .execute(&pool)
        .await?;
        let mut transaction = runner.tools.mysql()?.transaction().await?;
        assert_eq!(
            approval_aggregation::repository::delete_task_rows_in_tx(
                &mut transaction,
                &runner.context,
                &[leader_id, member_id],
            )
            .await?,
            0,
            "状态绑定外部事实的行不得被删除"
        );
        transaction.commit().await?;

        // 4. 状态不在复位白名单：即使无租约也不得复位。
        sqlx::query(
            "UPDATE feishu_approval_task SET state='created', worker_id=NULL, \
             lease_until=NULL WHERE id=?",
        )
        .bind(leader_id)
        .execute(&pool)
        .await?;
        let mut transaction = runner.tools.mysql()?.transaction().await?;
        assert!(
            !approval_aggregation::repository::reopen_group_leader_in_tx(
                &mut transaction,
                &runner.context,
                leader_id,
                "created",
            )
            .await?,
            "不在复位白名单的状态不得被复位"
        );
        transaction.commit().await?;
        let leader_state: String =
            sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
                .bind(leader_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(leader_state, "created", "被拒绝的复位不得改状态");
        let groups: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_group WHERE config_id=?")
                .bind(config_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(groups, 1, "守卫拒绝时组行必须保留");

        cleanup_aggregate_fixture(&pool, config_id).await?;
        Ok(())
    }
    // -----------------------------------------------------------------------
    // 组处理：认领、恢复、暂停、回填与组通知（真实 MySQL + 脚本传输）
    // -----------------------------------------------------------------------

    fn ok_body(code: i64) -> String {
        // 回填与写扩展字段都要过 `decode_data`：缺 `data` 会被判成响应信封不合法。
        serde_json::json!({"code": code, "msg": "", "data": {}}).to_string()
    }

    fn field_list_envelope() -> String {
        serde_json::json!({
            "code": 0,
            "msg": "",
            "data": {
                "has_more": false,
                "total": 3,
                "items": [
                    // 编号回填是 `{text, link}` 对象：只有链接（15）与文本（1）收得住，
                    // 配置里 `fldNumber` 的类型在这里决定回填成败。
                    {"field_id": "fldNumber", "field_name": "审批编号", "type": 15},
                    {"field_id": "fldText", "field_name": "事由", "type": 1},
                    // 发起人列：成功回填后 `write_extended` 要逐成员覆盖成点击人。
                    {"field_id": "fldPerson", "field_name": "发起人", "type": 11},
                ]
            }
        })
        .to_string()
    }

    fn batch_get_envelope(items: &[(&str, &str)]) -> String {
        let rows: Vec<serde_json::Value> = items
            .iter()
            .map(|(record_id, text)| {
                serde_json::json!({
                    "record_id": record_id,
                    "fields": {"事由": text},
                })
            })
            .collect();
        serde_json::json!({
            "code": 0,
            "msg": "",
            "data": {"records": rows}
        })
        .to_string()
    }

    fn create_envelope(instance_code: &str) -> String {
        serde_json::json!({
            "code": 0,
            "msg": "ok",
            "data": {"instance_code": instance_code}
        })
        .to_string()
    }

    fn detail_envelope(instance_code: &str, serial: &str) -> String {
        serde_json::json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "instance_code": instance_code,
                "serial_number": serial,
                "status": "PENDING"
            }
        })
        .to_string()
    }

    /// 播种时签名用的控件（与 `insert_aggregate_field_map` 写的映射一一对应）。
    fn aggregate_widgets() -> Vec<WidgetMap> {
        vec![WidgetMap {
            widget_id: "wName".to_string(),
            widget_type: "input".to_string(),
            required: false,
            bitable_field: "fldText".to_string(),
            interval_end_field: None,
            converter: crate::addon::feishu::domain::approval_convert::Converter::Direct,
            option_map: BTreeMap::new(),
            option_source_key: None,
            currency: None,
        }]
    }

    /// 「事由」同为 `text` 的两条记录在播种期会算出的签名摘要。
    fn aggregate_signature_hash(text: &str) -> String {
        let cells = serde_json::Map::from_iter([("fldText".to_string(), serde_json::json!(text))]);
        let signature = build_signature(&FormInput {
            widgets: &aggregate_widgets(),
            form_snapshot: AGGREGATE_FORM_SNAPSHOT,
            cells: &cells,
            attachment_codes: &BTreeMap::new(),
            timezone_offset: FixedOffset::from_seconds(8 * 3600)
                .unwrap_or_else(|error| panic!("{error}")),
        })
        .unwrap_or_else(|error| panic!("签名应可计算: {error}"));
        signature_hash(&signature)
    }

    /// 组处理夹具：一行组行 + leader/member 两行任务行，配置与映射齐备。
    struct GroupProcessingFixture {
        url: String,
        context: Arc<FeishuContext>,
        pool: sqlx::MySqlPool,
        config_id: i64,
        group_key: String,
        leader_id: i64,
        member_id: i64,
    }

    impl GroupProcessingFixture {
        async fn new(phase: &str, instance_code: Option<&str>) -> anyhow::Result<Self> {
            let (_database, context) = real_mysql_context().await?;
            let pool = context.approval_tasks().pool().clone();
            let namespace = uuid::Uuid::new_v4().simple().to_string();
            let config_id = insert_aggregate_config(&pool, &namespace, "g", false).await?;
            insert_aggregate_field_map(&pool, config_id).await?;
            let group_key = format!("gq{namespace}");
            sqlx::query(
                "INSERT INTO feishu_approval_group \
                 (config_id,group_key,signature_hash,uuid,effect_phase,instance_code,created_at,updated_at) \
                 VALUES (?,?,?,?,?,?,NOW(),NOW())",
            )
            .bind(config_id)
            .bind(&group_key)
            .bind(aggregate_signature_hash("报销"))
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(phase)
            .bind(instance_code)
            .execute(&pool)
            .await?;
            let leader_record = format!("{namespace}-leader");
            let member_record = format!("{namespace}-member");
            let leader_id = insert_group_task_row(
                &pool,
                config_id,
                &group_key,
                0,
                &leader_record,
                "pending",
                "ready",
                None,
            )
            .await?;
            let member_id = insert_group_task_row(
                &pool,
                config_id,
                &group_key,
                1,
                &member_record,
                "pending",
                "ready",
                None,
            )
            .await?;
            sqlx::query("UPDATE feishu_approval_task SET last_error='等待发起人' WHERE id=?")
                .bind(leader_id)
                .execute(&pool)
                .await?;
            let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
            Ok(Self {
                url,
                context,
                pool,
                config_id,
                group_key,
                leader_id,
                member_id,
            })
        }

        async fn leader_record(&self) -> anyhow::Result<String> {
            Ok(
                sqlx::query_scalar("SELECT record_id FROM feishu_approval_task WHERE id=?")
                    .bind(self.leader_id)
                    .fetch_one(&self.pool)
                    .await?,
            )
        }

        async fn member_record(&self) -> anyhow::Result<String> {
            Ok(
                sqlx::query_scalar("SELECT record_id FROM feishu_approval_task WHERE id=?")
                    .bind(self.member_id)
                    .fetch_one(&self.pool)
                    .await?,
            )
        }

        /// 认领 leader 并读回完整任务（`process_group` 需要执行权与组身份）。
        async fn claim_leader(&self, runner: &RoundRunner) -> anyhow::Result<ClaimedTask> {
            sqlx::query("UPDATE feishu_approval_task SET available_at=NULL, worker_id=NULL, lease_until=NULL WHERE id=?")
                .bind(self.leader_id)
                .execute(&self.pool)
                .await?;
            let claim = crate::addon::feishu::domain::approval_execution::claim_ready(
                &self.context,
                self.leader_id - 1,
                Some(self.leader_id),
                1,
            )
            .await?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("组 leader 未被认领"))?;
            let row = self
                .context
                .approval_tasks()
                .query()
                .where_eq("id", serde_json::json!(self.leader_id))?
                .optional()
                .await?
                .ok_or_else(|| anyhow::anyhow!("组 leader 任务行不存在"))?;
            let task = load_task(runner, &row, claim)
                .await?
                .ok_or_else(|| anyhow::anyhow!("组 leader 任务无法加载配置"))?;
            Ok(task)
        }

        /// 给 leader 落受理发起人（组缺身份会整组暂停，正常路径必须有）。
        async fn with_applicant(&self, open_id: &str) -> anyhow::Result<&Self> {
            sqlx::query(
                "INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?)                  ON DUPLICATE KEY UPDATE open_id=VALUES(open_id)",
            )
            .bind(self.leader_id)
            .bind(open_id)
            .execute(&self.pool)
            .await?;
            Ok(self)
        }

        async fn states(&self) -> anyhow::Result<(String, String)> {
            let leader: String =
                sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
                    .bind(self.leader_id)
                    .fetch_one(&self.pool)
                    .await?;
            let member: String =
                sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
                    .bind(self.member_id)
                    .fetch_one(&self.pool)
                    .await?;
            Ok((leader, member))
        }

        async fn cleanup(self) -> anyhow::Result<()> {
            cleanup_aggregate_fixture(&self.pool, self.config_id).await
        }
    }

    /// 组委派：`process_one` 必须把组行交给 `process_group`，且组分支排在
    /// missing-initiator 早退之前——否则缺身份的组会落成单条 `Waiting`，
    /// 永远等不到 `paused`（设计 §4.6 第 11 步）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_pause_marks_leader_and_members_paused() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("ready", None).await?;
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            Arc::new(SeedTransport::new(vec![])),
        )
        .await?;
        let mut task = fixture.claim_leader(&runner).await?;
        task.applicant_open_id = None;

        let outcome = process_one(&runner, &task).await;
        assert!(outcome.persisted, "组分支必须自己落库");
        assert!(matches!(outcome.result, DispatchResult::Terminal { .. }));

        let (leader, member) = fixture.states().await?;
        assert_eq!(leader, "paused", "暂停必须落 leader");
        assert_eq!(member, "paused", "暂停必须落全体成员");
        let reason: Option<String> =
            sqlx::query_scalar("SELECT last_error FROM feishu_approval_task WHERE id=?")
                .bind(fixture.leader_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert!(
            reason.unwrap_or_default().contains("发起人"),
            "暂停原因必须写进 leader 的 last_error"
        );

        fixture.cleanup().await?;
        Ok(())
    }

    /// 暂停组不参与轮询认领：`claim_ready` 只认 `state='pending'`。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_pause_is_not_reclaimed_by_poll() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("ready", None).await?;
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            Arc::new(SeedTransport::new(vec![])),
        )
        .await?;
        let mut task = fixture.claim_leader(&runner).await?;
        task.applicant_open_id = None;
        process_one(&runner, &task).await;

        let claims = crate::addon::feishu::domain::approval_execution::claim_ready(
            &fixture.context,
            0,
            None,
            CLAIM_BATCH,
        )
        .await?;
        assert!(
            claims.iter().all(|claim| claim.id != fixture.leader_id),
            "暂停组不得被轮询认领"
        );

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组成功：一次 create 两行明细、两名成员写同一编号、扩展列与发起人列逐成员写。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_success_backfills_same_serial_and_writes_extensions() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("ready", None).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "报销")]),
            batch_get_envelope(&[(&member_record, "报销")]),
            create_envelope("INST-GROUP"),
            detail_envelope("INST-GROUP", "SERIAL-9"),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            // 组通知：行号扫描一次 + 消息发送。
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_group"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            Arc::clone(&transport),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        let sent: Vec<String> = transport
            .requests
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .map(|request| format!("{:?} {}", request.method, request.url))
                    .collect()
            })
            .unwrap_or_default();
        assert!(outcome.persisted);
        assert!(
            matches!(outcome.result, DispatchResult::Backfilled { .. }),
            "组成功必须返回 Backfilled，实际 {:?}；请求序列 {sent:?}",
            outcome.result
        );

        let (leader, member) = fixture.states().await?;
        assert_eq!(
            (leader.as_str(), member.as_str()),
            ("backfilled", "backfilled")
        );
        let leader_serial: Option<String> =
            sqlx::query_scalar("SELECT serial_number FROM feishu_approval_task WHERE id=?")
                .bind(fixture.leader_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert!(leader_serial.is_none(), "组编号只落组行，leader 不持有");
        let group: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT effect_phase,instance_code,serial_number FROM feishu_approval_group \
             WHERE config_id=? AND group_key=?",
        )
        .bind(fixture.config_id)
        .bind(&fixture.group_key)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(
            group,
            (
                "created".to_string(),
                Some("INST-GROUP".to_string()),
                Some("SERIAL-9".to_string())
            ),
            "实例码与编号必须落组行"
        );

        let requests = transport
            .requests
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .clone();
        let serial_writes = requests
            .iter()
            .filter(|request| request.url.ends_with("/records/batch_update"))
            .filter_map(|request| request.json_body.as_ref())
            .any(|body| {
                body["records"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 2)
            });
        assert!(serial_writes, "编号回填必须一次覆盖两名成员");

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组通知只发一次，且正文含成员行号摘要（不逐成员编辑同一条消息）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_notification_is_sent_once_with_member_row_summary() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("created", Some("INST-DONE")).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "报销")]),
            batch_get_envelope(&[(&member_record, "报销")]),
            detail_envelope("INST-DONE", "SERIAL-7"),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            // 行号扫描：两行成员都能定位到行号。
            serde_json::json!({
                "code": 0,
                "data": {
                    "has_more": false,
                    "items": [
                        {"record_id": leader_record, "fields": {}},
                        {"record_id": member_record, "fields": {}}
                    ]
                }
            })
            .to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_once"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            Arc::clone(&transport),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(matches!(outcome.result, DispatchResult::Backfilled { .. }));

        let requests = transport
            .requests
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .clone();
        let sends: Vec<&crate::addon::feishu::domain::outbound::OutboundRequest> = requests
            .iter()
            .filter(|request| request.url.ends_with("/im/v1/messages"))
            .collect();
        assert_eq!(sends.len(), 1, "每组只发一条通知");
        let text = sends[0]
            .json_body
            .as_ref()
            .map(|body| body.to_string())
            .unwrap_or_default();
        assert!(
            text.contains("第 1、2 行"),
            "正文必须含成员行号摘要（一次渲染、不逐条编辑）：{text}"
        );

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组已建实例时成员记录被删：该成员摘出并落终态，其余成员继续回填同一编号。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn absent_member_in_created_group_is_detached_and_rest_backfilled() -> anyhow::Result<()>
    {
        let fixture = GroupProcessingFixture::new("created", Some("INST-PART")).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "报销")]),
            // 成员记录已不存在：`absent_record_ids` 是 Fatal。
            serde_json::json!({
                "code": 0,
                "msg": "",
                "data": {"records": [], "absent_record_ids": [member_record]}
            })
            .to_string(),
            detail_envelope("INST-PART", "SERIAL-3"),
            ok_body(0),
            ok_body(0),
            // 组通知：行号扫描 + 发送。
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_part"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport,
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(
            matches!(outcome.result, DispatchResult::Backfilled { .. }),
            "剩余成员回填成功后应整体成功，实际 {:?}",
            outcome.result
        );

        let member: (String, Option<String>, i64) =
            sqlx::query_as("SELECT state,group_id,group_seq FROM feishu_approval_task WHERE id=?")
                .bind(fixture.member_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(member.0, "terminal", "被删成员落终态");
        assert_eq!(member.1, None, "被删成员必须摘出组");
        assert_eq!(member.2, 0);

        let serial: Option<String> =
            sqlx::query_scalar("SELECT serial_number FROM feishu_approval_group WHERE group_key=?")
                .bind(&fixture.group_key)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(serial.as_deref(), Some("SERIAL-3"), "组行编号照常落实");

        fixture.cleanup().await?;
        Ok(())
    }
    /// 组行 `effect_phase='ready'` 时签名复核生效：记录的非明细值从播种后被改过，
    /// 整组暂停而不是建一张与分组时不一致的单。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_signature_drift_pauses_ready_group() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("ready", None).await?;
        fixture.with_applicant("ou_clicker").await?;
        // 组行存的是「报销」的摘要，而表里两条记录已改成「另一批」。
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "另一批")]),
            batch_get_envelope(&[(&member_record, "另一批")]),
            // 组暂停通知：行号扫描 + 发送。
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_drift"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport.clone(),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(matches!(outcome.result, DispatchResult::Terminal { .. }));
        let (leader, member) = fixture.states().await?;
        assert_eq!((leader.as_str(), member.as_str()), ("paused", "paused"));
        let requests = transport
            .requests
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .clone();
        assert!(
            requests
                .iter()
                .all(|request| !request.url.ends_with("/approval/v4/instances")),
            "签名漂移时绝不创建实例"
        );

        fixture.cleanup().await?;
        Ok(())
    }
    /// 签名复核**只在组行 `ready`**：已尝试过创建的组即使记录值变了也必须续跑
    /// （外部实例已经存在，暂停等于把它丢成孤儿）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn created_group_skips_signature_check_and_continues() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("created", Some("INST-OLD")).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        // 两条记录都改成了「另一批」：与组行摘要不一致，但组已建实例。
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "另一批")]),
            batch_get_envelope(&[(&member_record, "另一批")]),
            detail_envelope("INST-OLD", "SERIAL-OLD"),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_old"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport.clone(),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(
            matches!(outcome.result, DispatchResult::Backfilled { .. }),
            "已建实例的组不得因签名漂移暂停，实际 {:?}",
            outcome.result
        );
        let requests = transport
            .requests
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .clone();
        assert!(
            requests
                .iter()
                .all(|request| !request.url.ends_with("/approval/v4/instances")),
            "续跑不得重新创建实例"
        );

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组行停在 `creating`（创建前已提交阶段、结果未知）时**不**复核签名：
    /// 用同一个组 uuid 重发 create 是幂等的（冲突回 60012），而签名漂移不是
    /// 放弃这次创建的正当理由（设计 §4.6 第 5 步）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn creating_group_skips_signature_check_and_retries_create() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("creating", None).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "另一批")]),
            batch_get_envelope(&[(&member_record, "另一批")]),
            create_envelope("INST-NEW"),
            detail_envelope("INST-NEW", "SERIAL-N"),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_new"}}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport,
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(
            matches!(outcome.result, DispatchResult::Backfilled { .. }),
            "creating 阶段必须续跑创建，实际 {:?}",
            outcome.result
        );

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组行 `lookup` 但按 uuid 查不到实例（`1390003`）：阶段复位为 `ready` 并留
    /// 可重试，否则组会永久停在回捞死路（设计 §4.6 第 3 步、§6.4）。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn lookup_miss_resets_group_phase_and_stays_retryable() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("lookup", None).await?;
        fixture.with_applicant("ou_clicker").await?;
        let transport = Arc::new(SeedTransport::new(vec![
            // 按组 uuid 回捞：实例不存在。
            serde_json::json!({"code": 1390003, "msg": "instance not found"}).to_string(),
        ]));
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport.clone(),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(matches!(outcome.result, DispatchResult::Retryable { .. }));
        let phase: String =
            sqlx::query_scalar("SELECT effect_phase FROM feishu_approval_group WHERE group_key=?")
                .bind(&fixture.group_key)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(phase, "ready", "回捞未命中必须把组行阶段复位为 ready");
        let leader: String =
            sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
                .bind(fixture.leader_id)
                .fetch_one(&fixture.pool)
                .await?;
        assert_eq!(leader, "pending", "回捞未命中保持可重试（pending + 退避）");

        fixture.cleanup().await?;
        Ok(())
    }

    /// 组行 `lookup` 且按组 uuid 回捞命中：落实例码后直接回填，不再重发 create。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn lookup_hit_recovers_instance_code_and_backfills() -> anyhow::Result<()> {
        let fixture = GroupProcessingFixture::new("lookup", None).await?;
        fixture.with_applicant("ou_clicker").await?;
        let leader_record = fixture.leader_record().await?;
        let member_record = fixture.member_record().await?;
        let transport = Arc::new(SeedTransport::new(vec![
            // 按组 uuid 回捞命中（详情响应同时带回编号）。
            detail_envelope("INST-LOOKUP", "SERIAL-L"),
            field_list_envelope(),
            batch_get_envelope(&[(&leader_record, "报销")]),
            batch_get_envelope(&[(&member_record, "报销")]),
            detail_envelope("INST-LOOKUP", "SERIAL-L"),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            ok_body(0),
            serde_json::json!({"code":0,"data":{"has_more":false,"items":[]}}).to_string(),
            serde_json::json!({"code":0,"data":{"message_id":"om_lookup"}}).to_string(),
        ]));
        let group_uuid: String =
            sqlx::query_scalar("SELECT uuid FROM feishu_approval_group WHERE group_key=?")
                .bind(&fixture.group_key)
                .fetch_one(&fixture.pool)
                .await?;
        let runner = build_seed_runner(
            yang_db::Database::connect(&fixture.url).await?,
            Arc::clone(&fixture.context),
            transport.clone(),
        )
        .await?;
        let task = fixture.claim_leader(&runner).await?;

        let outcome = process_one(&runner, &task).await;
        assert!(
            matches!(outcome.result, DispatchResult::Backfilled { .. }),
            "回捞命中必须直接回填，实际 {:?}",
            outcome.result
        );
        let requests = transport
            .requests
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .clone();
        assert!(
            requests
                .iter()
                .any(|request| request.url.ends_with(&format!("/instances/{group_uuid}"))),
            "必须按**组 uuid** 回捞：{requests:?}"
        );
        assert!(
            requests.iter().all(|request| !(request.method
                == crate::addon::feishu::domain::outbound::OutboundMethod::Post
                && request.url.ends_with("/approval/v4/instances"))),
            "回捞命中后不得重发 create"
        );
        let group: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT effect_phase,instance_code,serial_number FROM feishu_approval_group WHERE group_key=?",
        )
        .bind(&fixture.group_key)
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(
            group,
            (
                "created".to_string(),
                Some("INST-LOOKUP".to_string()),
                Some("SERIAL-L".to_string())
            )
        );

        fixture.cleanup().await?;
        Ok(())
    }
}
