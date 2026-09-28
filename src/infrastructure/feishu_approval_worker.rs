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

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use yang_base::table::{Record, SortOrder};
use yang_base::tools::Tools;
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_convert::{FixedOffset, WidgetMap};
use crate::addon::feishu::domain::approval_dispatch::{
    backfill_in_chunks, dispatch_one, widget_maps_from_rows, BitableBackfill,
    DispatchInput as OrchestrationInput, DispatchResult,
};
use crate::addon::feishu::domain::approval_rate_limit::acquire_create_slot;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    HttpClientTransport, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 一次认领最多取多少条。
///
/// 与回填子批同量级：认领一批、处理一批、回填一批，批内失败不影响其他批。
/// 取大了会让单轮耗时过长（创建受速率限制），也拉长租约。
const CLAIM_BATCH: usize = 50;

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
    /// 一批里**最后一个**已处理记录的 id；空批不动游标。
    fn advance(&mut self, rows: &[ClaimedTask]) {
        if let Some(last) = rows.last() {
            self.0 = self.0.max(last.id);
        }
    }
}

/// 手动触发的共享句柄。
///
/// 照 `FeishuPullHandle` 的先例：注册进 `Tools` 供 Action 取用。
#[derive(Clone)]
pub(crate) struct ApprovalDispatchHandle {
    trigger: mpsc::UnboundedSender<()>,
}

impl ApprovalDispatchHandle {
    /// 建一对：句柄给 Action，接收端给 worker。
    ///
    /// 无界通道：触发是低频动作，没有一个值得让 HTTP 请求等它的背压。
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<()>) {
        let (trigger, requests) = mpsc::unbounded_channel();
        (Self { trigger }, requests)
    }

    /// 请求立刻跑一轮全表。
    ///
    /// 接收端已消失（worker 没起或正在退出）时返回错误——静默丢弃会让按钮
    /// 显示「已受理」而实际什么都没发生。
    pub(crate) fn request_dispatch(&self) -> Result<(), BaseError> {
        self.trigger.send(()).map_err(|_| {
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
        requests: mpsc::UnboundedReceiver<()>,
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
    mut requests: mpsc::UnboundedReceiver<()>,
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
            // 手动触发：投一个信号即跑一轮，与自动轮询走**完全同一条路径**。
            Some(()) = requests.recv() => {
                run_round(&runner, &mut cursor).await;
            }
            _ = ticker.tick() => {
                run_round(&runner, &mut cursor).await;
            }
        }
    }
}

/// 跑一轮：认领 → 逐条处理 → 写回结果。
///
/// **没有批量标记。** 状态推进只发生在 [`record_outcome`]（逐条、处理结束时）：
/// 这条路经本身就有「崩溃前未处理 → 进 `creating` → 下一轮不认领」的风险，
/// 所以崩溃恢复靠 `claim` 直接认领 `creating`（单实例 + 单线程 ⇒ 此时没有别人
/// 正在处理），下一轮 `record_outcome` 覆盖它。
async fn run_round(runner: &RoundRunner, cursor: &mut ResumeCursor) {
    let started = std::time::Instant::now();
    let claimed = match claim(runner, cursor).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::error!(error = %error, "审批派发认领失败");
            return;
        }
    };
    if claimed.is_empty() {
        return;
    }
    tracing::info!(count = claimed.len(), "审批派发认领到任务");

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
            idled = true;
            break;
        };
        if let Err(failure) =
            acquire_create_slot(redis, &runner.deployment, runner.rate_per_minute).await
        {
            tracing::warn!(error = %failure.message, "取创建名额失败，中断本轮");
            idled = true;
            break;
        }

        let outcome = process_one(runner, task).await;
        record_outcome(runner, task, &outcome).await;
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
}

/// 一条已认领的任务及其全部配置（一次查全，避免处理时再查库）。
struct ClaimedTask {
    id: i64,
    record_id: String,
    base_token: String,
    table_id: String,
    approval_code: String,
    applicant_field: String,
    backfill_field: String,
    timezone_offset: FixedOffset,
    widgets: Vec<WidgetMap>,
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
/// 依赖。崩溃恢复由「重启后重新认领」承担：`claim` 直接认领 `pending`（单实例
/// + 单线程 ⇒ 无并发），`record_outcome` 在处理结束时把它改写成稳定状态。
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
        .select_fields(&["id", "record_id", "config_id"])?
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

    let config = runner
        .context
        .approval_configs()
        .query()
        .where_eq("id", serde_json::json!(config_id))?
        .where_eq("enabled", serde_json::json!(true))?
        .optional()
        .await?;
    let Some(config) = config else {
        return Ok(None);
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
        return Ok(None);
    };
    // 没有映射的任务提不出去，但那多半是「配置还在建」而不是坏掉——
    // 走**等待态**（回 pending）比置终态更合适，等映射配好后自然重扫。
    if widgets.is_empty() {
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
        id: row.require("id")?,
        record_id: row.require("record_id")?,
        base_token: config.require("base_token")?,
        table_id: config.require("table_id")?,
        approval_code: config.require("approval_code")?,
        applicant_field: config.require("applicant_field")?,
        backfill_field: config.require("backfill_field")?,
        timezone_offset,
        widgets,
    }))
}

/// 处理一条记录。
async fn process_one(runner: &RoundRunner, task: &ClaimedTask) -> DispatchResult {
    let coordinates = BitableCoordinates {
        app_token: task.base_token.clone(),
        table_id: task.table_id.clone(),
        view_id: None,
    };

    // 只投影需要的字段：多读列既慢，又可能因为某列形态异常而使整个查询失败。
    let mut field_names: Vec<String> = task
        .widgets
        .iter()
        .map(|widget| widget.bitable_field.clone())
        .collect();
    field_names.push(task.applicant_field.clone());
    field_names.sort();
    field_names.dedup();

    let found = bitable::search_records(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &coordinates,
        bitable::record_by_id_query(&task.record_id, &field_names),
    )
    .await;
    let cells: serde_json::Map<String, serde_json::Value> = match found {
        Ok(data) => match data.items.into_iter().next() {
            Some(record) => record.fields.into_iter().collect(),
            None => {
                return DispatchResult::Terminal {
                    message: "多维表格里已找不到该记录".to_string(),
                }
            }
        },
        Err(failure) => {
            return DispatchResult::Retryable {
                message: format!("读取记录失败：{}", failure.message),
            }
        }
    };

    let backfill = BitableBackfill {
        transport: runner.transport(),
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates: &coordinates,
    };
    dispatch_one(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &backfill,
        &OrchestrationInput {
            coordinates: &coordinates,
            record_id: &task.record_id,
            cells: &cells,
            applicant_field: &task.applicant_field,
            backfill_field: &task.backfill_field,
            approval_code: &task.approval_code,
            widgets: &task.widgets,
            timezone_offset: task.timezone_offset,
        },
    )
    .await
}

/// 把处理结论落库。
async fn record_outcome(runner: &RoundRunner, task: &ClaimedTask, outcome: &DispatchResult) {
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
        DispatchResult::Retryable { message } => ("pending", Some(message.clone())),
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
    rows: &[(String, String)],
) -> anyhow::Result<()> {
    let backfill = BitableBackfill {
        transport: runner.transport(),
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates,
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
