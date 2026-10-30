//! 审批事件处理 worker：从 `feishu_approval_event` 表取未处理事件，富化后回写多维表格。
//!
//! 骨架与 `feishu_approval_worker` 一致：`watch` 通道 + `JoinHandle`，`shutdown()` 发信号后
//! `await` 任务退出；`Drop` 也发一次，避免忘记关闭时任务悬挂。
//!
//! shortcut: 事件领取尚无跨进程互斥，处理多实例旧回写覆盖需要 N3 的串行写者与对账。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use tracing::Instrument;
use yang_base::table::{Record, SortOrder};
use yang_base::tools::Tools;
use yang_runtime::shutdown::OwnedTask;

use crate::addon::feishu::approval::domain::event_table::MAX_EVENT_RETRY_COUNT;
use crate::addon::feishu::domain::approval::{get_instance, subscribe_approval, GetOutcome};
use crate::addon::feishu::domain::approval_convert::FixedOffset;
use crate::addon::feishu::domain::approval_dispatch::{
    fill_department_name, write_extended, BitableBackfill, DispatchInput,
};
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    FailureKind, HttpClientTransport, OutboundFailure, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 每轮最多处理多少条事件。
const EVENT_BATCH: usize = 50;

/// 编译期守护：批量不超框架上限。
const _: () = assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 事件处理 worker。
pub(crate) struct FeishuEventWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl FeishuEventWorker {
    /// 启动 worker。
    ///
    /// 调用方必须先确认 `settings.can_pull()`——凭证还是占位值时不该起。
    pub(crate) fn start(
        tools: Arc<Tools>,
        settings: &FeishuSettings,
        context: Arc<FeishuContext>,
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

        let timezone_offset = FixedOffset::from_iana(settings.approval_base_timezone.trim())
            .map_err(|error| {
                yang_base::BaseError::ConfigError(format!(
                    "feishu.approval_base_timezone 无效：{error}"
                ))
            })?;

        let (shutdown, receiver) = watch::channel(false);
        let runner = RoundRunner {
            tools,
            context,
            tokens,
            transport,
            sleeper,
            timezone_offset,
        };
        let idle_poll = Duration::from_secs(settings.approval_scan_interval_seconds);
        let task = tokio::spawn(run_loop(runner, idle_poll, receiver));

        tracing::info!(
            idle_poll_seconds = settings.approval_scan_interval_seconds,
            "飞书事件处理 Worker 已启动"
        );
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
        })
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
            .map_err(|error| anyhow::anyhow!("等待事件处理 Worker 退出失败: {error}"))?;
        Ok(())
    }
}

impl Drop for FeishuEventWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

fn deployment_namespace(tools: &Tools) -> anyhow::Result<String> {
    Ok(tools
        .extension::<crate::authorization::AuthorizationVersionCache>()?
        .deployment()
        .to_string())
}

/// 跑一轮要用到的共享句柄。
struct RoundRunner {
    #[allow(dead_code)]
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    transport: Arc<dyn OutboundTransport>,
    sleeper: Arc<TokioSleeper>,
    timezone_offset: FixedOffset,
}

impl RoundRunner {
    fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }
}

/// 主循环：启动即订阅一次全部启用配置的审批定义，然后等信号或到点跑一轮。
async fn run_loop(runner: RoundRunner, idle_poll: Duration, mut shutdown: watch::Receiver<bool>) {
    // 按定义订阅是运行时状态，控制台重配、发布新版本都可能被清掉——每次启动
    // 重订一遍，链路自愈（设计 §7.2 第 2 步）。失败只 warn，不挡运行。
    let subscribe = async {
        if let Err(error) = subscribe_enabled_configs(&runner).await {
            tracing::warn!(error = %error, "启动期订阅审批定义失败（下次启动会重试）");
        }
    };
    if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), subscribe).await {
        tracing::info!("飞书事件处理 Worker 收到关闭信号（启动订阅阶段）");
        return;
    }
    let mut ticker = tokio::time::interval(idle_poll);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("飞书事件处理 Worker 收到关闭信号");
                    return;
                }
            }
            _ = ticker.tick() => {
                let round = run_round(&runner, "poll");
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("飞书事件处理 Worker 收到关闭信号，已取消进行中的轮次");
                    return;
                }
            }
        }
    }
}

/// 订阅全部启用配置的审批定义 Code。幂等（`1390007`=已订阅视为成功），
/// 同一 Code 被多个配置共用时只调一次。
async fn subscribe_enabled_configs(runner: &RoundRunner) -> anyhow::Result<()> {
    let rows = runner
        .context
        .approval_configs()
        .query()
        .where_eq("enabled", serde_json::json!(true))?
        .select_fields(&["id", "approval_code"])?
        .all()
        .await?;
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let code = match row.optional::<String>("approval_code")? {
            Some(code) => code.trim().to_string(),
            None => continue,
        };
        if code.is_empty() || !seen.insert(code.clone()) {
            continue;
        }
        match subscribe_approval(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            &code,
        )
        .await
        {
            Ok(()) => tracing::info!(approval_code = %code, "审批定义事件订阅确认"),
            Err(failure) => tracing::warn!(
                error = %failure.message,
                approval_code = %code,
                "审批定义事件订阅失败"
            ),
        }
    }
    Ok(())
}

/// 一条已取出的待处理事件。
struct PendingEvent {
    id: i64,
    instance_code: String,
    retry_count: i64,
}

/// 跑一轮：取未处理事件 -> 逐条富化 -> 回写多维表格 -> 标记已处理。
#[tracing::instrument(name = "event_round", skip_all, fields(trigger))]
async fn run_round(runner: &RoundRunner, trigger: &str) {
    let started = std::time::Instant::now();
    tracing::Span::current().record("trigger", trigger);

    let events = match claim_events(runner).await {
        Ok(events) => events,
        Err(error) => {
            tracing::error!(error = %error, "事件处理认领失败");
            return;
        }
    };

    if events.is_empty() {
        tracing::debug!(
            duration_ms = started.elapsed().as_millis() as u64,
            "事件处理本轮没有待处理事件"
        );
        return;
    }

    tracing::info!(count = events.len(), "事件处理认领到待处理事件");
    let mut processed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut terminal = 0usize;

    for event in &events {
        let outcome = process_event(runner, event).await;
        match outcome {
            EventOutcome::Processed => {
                processed += 1;
                if let Err(error) = mark_processed(runner, event.id).await {
                    tracing::warn!(error = %error, event_id = event.id, "标记事件已处理失败");
                }
            }
            EventOutcome::Skipped(reason) => {
                skipped += 1;
                metrics::counter!("feishu_event_skipped_total").increment(1);
                tracing::info!(
                    event_id = event.id,
                    instance_code = %event.instance_code,
                    reason = %reason,
                    "事件与本系统任务无关，已跳过并标记处理"
                );
                if let Err(error) = mark_processed(runner, event.id).await {
                    tracing::warn!(error = %error, event_id = event.id, "标记已跳过事件失败");
                }
            }
            EventOutcome::Failed(message) => {
                failed += 1;
                let new_count = event.retry_count + 1;
                if new_count >= MAX_EVENT_RETRY_COUNT {
                    terminal += 1;
                    let terminal_message =
                        format!("连续失败超 {MAX_EVENT_RETRY_COUNT} 次: {message}");
                    if let Err(error) =
                        mark_terminal_event(runner, event.id, &terminal_message).await
                    {
                        tracing::warn!(error = %error, event_id = event.id, "置事件终态失败");
                    }
                    // 终态丢弃是刻意的毒事件隔离；但必须可观测，否则「状态列停在旧值」
                    // 只能靠翻库发现。计数器给告警/看板，error 日志留单条证据。
                    metrics::counter!("feishu_event_terminal_total").increment(1);
                    tracing::error!(
                        event_id = event.id,
                        retry_count = new_count,
                        error = %message,
                        "飞书事件连续失败达到上限，已置终态并从队列移除"
                    );
                } else if let Err(error) = mark_retry(runner, event.id, new_count, &message).await {
                    tracing::warn!(error = %error, event_id = event.id, "更新事件重试信息失败");
                }
            }
            EventOutcome::Terminal(message) => {
                terminal += 1;
                if let Err(error) = mark_terminal_event(runner, event.id, &message).await {
                    tracing::warn!(error = %error, event_id = event.id, "置事件终态失败");
                }
                metrics::counter!("feishu_event_terminal_total").increment(1);
                tracing::error!(
                    event_id = event.id,
                    error = %message,
                    "飞书事件被远端明确拒绝（不可重试），已置终态并从队列移除"
                );
            }
        }
    }

    tracing::info!(
        claimed = events.len(),
        processed,
        skipped,
        failed,
        terminal,
        duration_ms = started.elapsed().as_millis() as u64,
        "事件处理轮次结束"
    );
}

/// 取未处理事件（`processed_at IS NULL`），按 id 升序，限制 `EVENT_BATCH` 条。
async fn claim_events(runner: &RoundRunner) -> anyhow::Result<Vec<PendingEvent>> {
    tracing::debug!("开始查询未处理事件（processed_at IS NULL）");
    let query = runner
        .context
        .approval_events()
        .query()
        .where_null("processed_at")?
        .order_by("id", SortOrder::Asc)?
        .page(1, EVENT_BATCH)?
        .select_fields(&["id", "instance_code", "retry_count"])?;
    let rows = query.all().await?;

    tracing::debug!(raw_row_count = rows.len(), "事件认领查询返回原始行数");

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(PendingEvent {
            id: row.require("id")?,
            instance_code: row.require("instance_code")?,
            retry_count: row.optional("retry_count")?.unwrap_or(0i64),
        });
    }
    tracing::debug!(claimed = events.len(), "认领到未处理事件");
    Ok(events)
}

/// 事件处理结果。
enum EventOutcome {
    /// 已处理并回写完成。
    Processed,
    /// 可重试失败（下一轮重新认领）。
    Failed(String),
    /// 确认与本系统无关（无 task 行且实例归属可判定为外部/已删除），直接收口。
    Skipped(String),
    /// 远端明确拒绝（Fatal），不再重试，立即置终态留痕。
    Terminal(String),
}

/// task 行里后续步骤依赖的三个字段。
struct TaskIdentity {
    task_id: i64,
    record_id: String,
    config_id: i64,
}

/// 提取 task 身份；失败时保持既有日志与文案。
fn require_task_identity(row: &Record) -> Result<TaskIdentity, String> {
    let task_id: i64 = match row.require("id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, "task 行缺 id");
            return Err(format!("task 行缺 id: {error}"));
        }
    };
    let record_id: String = match row.require("record_id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, task_id, "task 行缺 record_id");
            return Err(format!("task 行缺 record_id: {error}"));
        }
    };
    let config_id: i64 = match row.require("config_id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, task_id, "task 行缺 config_id");
            return Err(format!("task 行缺 config_id: {error}"));
        }
    };
    Ok(TaskIdentity {
        task_id,
        record_id,
        config_id,
    })
}

/// 从事件原始报文里取**实例 uuid**。
///
/// 只认 `approval_instance` 事件的 `event.uuid`——它是创建实例时传入的幂等键；
/// 顶层 `uuid` 是事件 id，语义不同（见 `receive_approval_event.rs` 的说明）。
/// `approval_task` 事件没有这个字段，调用方改用实例详情回显的 uuid。
fn event_instance_uuid(raw_payload: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(raw_payload).ok()?;
    let event = body.get("event")?;
    if event.get("type")?.as_str()? != "approval_instance" {
        return None;
    }
    let uuid = event.get("uuid")?.as_str()?.trim();
    if uuid.is_empty() {
        None
    } else {
        Some(uuid.to_ascii_lowercase())
    }
}

/// instance_code 查不到 task 行时的归属判定结果。
enum MissingTaskResolution {
    /// 按实例 uuid 反查到了本系统的 task 行（instance_code 尚未落库的竞态窗口）。
    Found(Record),
    /// 可以确定永远不会出现 task 行：外部实例或任务行已被删除。
    Absent,
    /// 暂时无法判定（报文/网络/数据库异常），保持可重试。
    Transient(String),
}

/// 读取事件原始报文并解析实例 uuid。
async fn load_event_uuid(runner: &RoundRunner, event_id: i64) -> Result<Option<String>, String> {
    let row = runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))
        .map_err(|error| format!("构造事件报文查询失败: {error}"))?
        .select_fields(&["raw_payload"])
        .map_err(|error| format!("投影事件报文失败: {error}"))?
        .optional()
        .await
        .map_err(|error| format!("读取事件报文失败: {error}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let raw = row
        .optional::<String>("raw_payload")
        .map_err(|error| format!("解析事件报文失败: {error}"))?;
    Ok(raw.as_deref().and_then(event_instance_uuid))
}

/// 事件在 task 表里查不到时，用实例 uuid 判断归属。
///
/// 判定链：
/// 1. `approval_instance` 事件的 `event.uuid` 直接可用；
/// 2. `approval_task` 事件从实例详情回显取 uuid；
/// 3. 实例详情暂时查不到（1390003）或网络失败 → Transient，绝不据此跳过；
/// 4. uuid 为空（非本系统 API 创建的外部实例）或 task 表无此 uuid → Absent。
async fn resolve_missing_task(runner: &RoundRunner, event: &PendingEvent) -> MissingTaskResolution {
    let uuid = match load_event_uuid(runner, event.id).await {
        Ok(uuid) => uuid,
        Err(message) => return MissingTaskResolution::Transient(message),
    };
    let uuid = match uuid {
        Some(uuid) => Some(uuid),
        None => match get_instance(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            &event.instance_code,
        )
        .await
        {
            Ok(GetOutcome::Found(detail)) => detail
                .uuid
                .as_deref()
                .map(str::trim)
                .filter(|uuid| !uuid.is_empty())
                .map(str::to_ascii_lowercase),
            // 1390003 在并发窗口里也代表「实例还没建好」，不是「外部实例」。
            Ok(GetOutcome::NotFound) => {
                return MissingTaskResolution::Transient(
                    "实例暂不可查询（1390003），下一轮重试".to_string(),
                )
            }
            Err(failure) => {
                return MissingTaskResolution::Transient(format!(
                    "获取实例详情失败: {}",
                    failure.message
                ))
            }
        },
    };
    let Some(uuid) = uuid else {
        // 没有创建时传入的 uuid：不可能是本系统通过 API 创建的实例。
        return MissingTaskResolution::Absent;
    };
    let query = match runner
        .context
        .approval_tasks()
        .query()
        .where_eq("uuid", serde_json::json!(uuid))
    {
        Ok(query) => query,
        Err(error) => {
            return MissingTaskResolution::Transient(format!("构造 uuid 反查失败: {error}"))
        }
    };
    let query = match query.select_fields(&["id", "record_id", "config_id"]) {
        Ok(query) => query,
        Err(error) => {
            return MissingTaskResolution::Transient(format!("投影 uuid 反查失败: {error}"))
        }
    };
    match query.all().await {
        Ok(rows) => rows
            .into_iter()
            .next()
            .map_or(MissingTaskResolution::Absent, MissingTaskResolution::Found),
        Err(error) => {
            MissingTaskResolution::Transient(format!("按 uuid 反查 task 表失败: {error}"))
        }
    }
}

/// 出站失败 → 事件结论：Fatal（远端明确拒绝，重试无意义）立即终态；
/// 其余保持可重试。与审批派发侧的 Fatal→Terminal 映射保持一致。
fn outbound_failure_outcome(prefix: &str, failure: OutboundFailure) -> EventOutcome {
    let message = format!("{prefix}: {}", failure.message);
    match failure.kind {
        FailureKind::Fatal { .. } => EventOutcome::Terminal(message),
        _ => EventOutcome::Failed(message),
    }
}

/// 处理一条事件：反查 task -> 加载 config -> 富化 -> 回写。
async fn process_event(runner: &RoundRunner, event: &PendingEvent) -> EventOutcome {
    let span = tracing::info_span!(
        "event_process",
        event_id = event.id,
        instance_code = %event.instance_code
    );
    async { process_event_inner(runner, event).await }
        .instrument(span)
        .await
}

async fn process_event_inner(runner: &RoundRunner, event: &PendingEvent) -> EventOutcome {
    tracing::debug!(
        event_id = event.id,
        instance_code = %event.instance_code,
        retry_count = event.retry_count,
        "=== 开始处理事件 ==="
    );

    // 1. 用 instance_code 反查 task 表，取 record_id + config_id
    tracing::debug!(instance_code = %event.instance_code, "步骤 1: 反查 task 表");
    let task_query_result = runner
        .context
        .approval_tasks()
        .query()
        .where_eq("instance_code", serde_json::json!(&event.instance_code));
    let task_rows = match task_query_result {
        Ok(query) => match query.select_fields(&["id", "record_id", "config_id"]) {
            Ok(query) => match query.all().await {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::error!(error = %error, "查询 task 表失败");
                    return EventOutcome::Failed(format!("查询 task 表失败: {error}"));
                }
            },
            Err(error) => {
                tracing::error!(error = %error, "投影字段失败");
                return EventOutcome::Failed(format!("投影字段失败: {error}"));
            }
        },
        Err(error) => {
            tracing::error!(error = %error, "构造 task 查询失败");
            return EventOutcome::Failed(format!("构造 task 查询失败: {error}"));
        }
    };

    let (task_id, record_id, config_id) = match task_rows.first() {
        Some(row) => match require_task_identity(row) {
            Ok(identity) => (identity.task_id, identity.record_id, identity.config_id),
            Err(message) => return EventOutcome::Failed(message),
        },
        None => match resolve_missing_task(runner, event).await {
            MissingTaskResolution::Found(row) => {
                // instance_code 尚未落库的竞态窗口：按实例 uuid 找到了本系统任务行。
                metrics::counter!("feishu_event_task_uuid_fallback_total").increment(1);
                tracing::info!(
                    event_id = event.id,
                    instance_code = %event.instance_code,
                    "instance_code 未命中，按实例 uuid 找到 task 行"
                );
                match require_task_identity(&row) {
                    Ok(identity) => (identity.task_id, identity.record_id, identity.config_id),
                    Err(message) => return EventOutcome::Failed(message),
                }
            }
            MissingTaskResolution::Absent => {
                return EventOutcome::Skipped(
                    "task 行不存在且实例归属为非本系统或任务已删除".to_string(),
                );
            }
            MissingTaskResolution::Transient(message) => {
                tracing::debug!(
                    event_id = event.id,
                    error = %message,
                    "task 行缺失且归属暂不可判定，保持重试"
                );
                return EventOutcome::Failed(message);
            }
        },
    };

    tracing::debug!(
        task_id,
        record_id = %record_id,
        config_id,
        "找到对应的派发任务"
    );

    // 事件 UUID 在入站去重；终态之后仍可能撤销或删除，须重新读取最新详情。
    // 2. 加载 config
    tracing::debug!(config_id, "步骤 2: 加载审批配置");
    let config = match runner
        .context
        .approval_configs()
        .query()
        .where_eq("id", serde_json::json!(config_id))
    {
        Ok(query) => match query.optional().await {
            Ok(Some(config)) => config,
            Ok(None) => {
                tracing::error!(config_id, "config 不存在");
                return EventOutcome::Failed(format!("config_id={config_id} 不存在"));
            }
            Err(error) => {
                tracing::error!(error = %error, config_id, "加载 config 失败");
                return EventOutcome::Failed(format!("加载 config 失败: {error}"));
            }
        },
        Err(error) => {
            tracing::error!(error = %error, config_id, "构造 config 查询失败");
            return EventOutcome::Failed(format!("构造 config 查询失败: {error}"));
        }
    };

    let base_token: String = match config.require("base_token") {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(error = %error, config_id, "config 缺 base_token");
            return EventOutcome::Failed(format!("config 缺 base_token: {error}"));
        }
    };
    let table_id: String = match config.require("table_id") {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(error = %error, config_id, "config 缺 table_id");
            return EventOutcome::Failed(format!("config 缺 table_id: {error}"));
        }
    };
    let approval_code: String = match config.require("approval_code") {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(error = %error, config_id, "config 缺 approval_code");
            return EventOutcome::Failed(format!("config 缺 approval_code: {error}"));
        }
    };
    let applicant_field: String = match config.optional::<String>("applicant_field") {
        Ok(Some(v)) => v,
        Ok(None) => String::new(),
        Err(error) => {
            tracing::error!(error = %error, config_id, "config 缺 applicant_field");
            return EventOutcome::Failed(format!("config 缺 applicant_field: {error}"));
        }
    };
    let backfill_field: String = match config.optional::<String>("backfill_field") {
        Ok(Some(v)) => v,
        Ok(None) => String::new(),
        Err(error) => {
            tracing::error!(error = %error, config_id, "config 缺 backfill_field");
            return EventOutcome::Failed(format!("config 缺 backfill_field: {error}"));
        }
    };
    let status_field = optional_nonempty(&config, "status_field");
    let flow_field = optional_nonempty(&config, "flow_field");
    let start_time_field = optional_nonempty(&config, "start_time_field");
    let end_time_field = optional_nonempty(&config, "end_time_field");
    let department_field = optional_nonempty(&config, "department_field");
    let handler_field = optional_nonempty(&config, "handler_field");

    tracing::debug!(
        config_id,
        base_token = %base_token,
        table_id = %table_id,
        approval_code = %approval_code,
        status_field = ?status_field,
        flow_field = ?flow_field,
        start_time_field = ?start_time_field,
        end_time_field = ?end_time_field,
        department_field = ?department_field,
        handler_field = ?handler_field,
        "加载配置完成"
    );

    // 3. 调 get_instance 获取实例详情
    tracing::debug!(instance_code = %event.instance_code, "步骤 3: 调用 get_instance 获取审批详情");
    let mut detail = match get_instance(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        &event.instance_code,
    )
    .await
    {
        Ok(GetOutcome::Found(detail)) => {
            tracing::debug!(
                instance_code = %event.instance_code,
                status = ?detail.status,
                start_time = ?detail.start_time,
                end_time = ?detail.end_time,
                "获取审批实例详情成功"
            );
            *detail
        }
        Ok(GetOutcome::NotFound) => {
            tracing::warn!(
                instance_code = %event.instance_code,
                "审批实例不存在，可能已被删除"
            );
            return EventOutcome::Failed("实例不存在".to_string());
        }
        Err(failure) => {
            tracing::error!(
                error = %failure.message,
                instance_code = %event.instance_code,
                "获取实例详情失败"
            );
            return outbound_failure_outcome("获取实例详情失败", failure);
        }
    };

    // 3.5 部门名解析：配了部门回填列才有这次出站；解析失败退化为写 ID。
    fill_department_name(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        department_field.as_deref(),
        &mut detail,
    )
    .await;

    // 4. 构造 BitableBackfill
    tracing::debug!("步骤 4: 构造 BitableBackfill");
    let coordinates = BitableCoordinates {
        app_token: base_token.clone(),
        table_id: table_id.clone(),
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
        Ok(fields) => {
            tracing::debug!(field_count = fields.len(), "读取表结构成功");
            fields
        }
        Err(failure) => {
            tracing::error!(error = %failure.message, "读取表结构失败");
            return outbound_failure_outcome("读取表结构失败", failure);
        }
    };
    let backfill = BitableBackfill {
        transport: runner.transport(),
        sleeper: runner.sleeper.as_ref(),
        tokens: &runner.tokens,
        coordinates: &coordinates,
        fields: &fields,
    };

    // 5. 构造 DispatchInput
    tracing::debug!("步骤 5: 构造 DispatchInput");
    let backfill_field_name = match bitable::resolve_field_name(&fields, &backfill_field) {
        Ok(name) => name,
        Err(error) => {
            tracing::error!(error = %error, backfill_field = %backfill_field, "回填列不可用");
            return EventOutcome::Failed(format!("回填列不可用: {error}"));
        }
    };

    let input = DispatchInput {
        fields: &fields,
        context: Some(&runner.context),
        execution: None,
        coordinates: &coordinates,
        record_id: &record_id,
        cells: &serde_json::Map::new(),
        applicant_field: &applicant_field,
        applicant: None,
        backfill_field_name: &backfill_field_name,
        status_field: status_field.as_deref(),
        flow_field: flow_field.as_deref(),
        start_time_field: start_time_field.as_deref(),
        end_time_field: end_time_field.as_deref(),
        department_field: department_field.as_deref(),
        handler_field: handler_field.as_deref(),
        approval_code: &approval_code,
        widgets: &[],
        form_snapshot: String::new(),
        timezone_offset: runner.timezone_offset,
    };

    // 6. 调 write_extended 回写多维表格
    tracing::debug!(
        record_id = %record_id,
        "步骤 6: 调用 write_extended 回写多维表格"
    );
    if let Err(failure) = write_extended(&backfill, &input, &detail).await {
        return outbound_failure_outcome("扩展字段回填失败", failure);
    }
    tracing::info!(
        record_id = %record_id,
        "多维表格回写完成"
    );

    // 7. 更新 task 行的 instance_status
    if let Some(status) = detail.status.as_deref() {
        tracing::debug!(task_id, status = %status, "步骤 7: 更新 task 的 instance_status");
        let mut row = Record::new();
        row.insert("instance_status", serde_json::json!(status));
        row.insert(
            "last_synced_at",
            serde_json::json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64),
        );
        let result = runner
            .context
            .approval_tasks()
            .query()
            .where_eq("id", serde_json::json!(task_id));
        match result {
            Ok(query) => {
                if let Err(error) = query.update(row).await {
                    tracing::warn!(error = %error, task_id, "更新 task instance_status 失败");
                    return EventOutcome::Failed(format!("保存审批同步事实失败: {error}"));
                } else {
                    tracing::info!(task_id, status = %status, "task instance_status 更新成功");
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, task_id, "构造 task 更新查询失败");
                return EventOutcome::Failed(format!("构造审批同步事实查询失败: {error}"));
            }
        }
    }

    tracing::debug!(
        event_id = event.id,
        instance_code = %event.instance_code,
        "=== 事件处理完成 ==="
    );
    EventOutcome::Processed
}

/// 从 config 行取可选字段，空串视为 None。
fn optional_nonempty(config: &Record, field: &str) -> Option<String> {
    match config.optional::<String>(field) {
        Ok(Some(v)) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// 标记事件已处理（设 `processed_at`，清 `last_error` 并把重试计数归零）。
///
/// 重试计数归零让 `is_terminal_event` 不会把「成功处理但此前的重试计数恰好
/// 达到上限」的事件误判为终态；单实例部署下也是终态判据的幂等加固。
async fn mark_processed(runner: &RoundRunner, event_id: i64) -> anyhow::Result<()> {
    let mut row = Record::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    row.insert("processed_at", serde_json::json!(now));
    row.insert("retry_count", serde_json::json!(0));
    row.insert("last_error", serde_json::Value::Null);
    runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))?
        .update(row)
        .await?;
    Ok(())
}

/// 更新事件重试信息。
async fn mark_retry(
    runner: &RoundRunner,
    event_id: i64,
    retry_count: i64,
    error_message: &str,
) -> anyhow::Result<()> {
    let mut row = Record::new();
    row.insert("retry_count", serde_json::json!(retry_count));
    row.insert("last_error", serde_json::json!(error_message));
    runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))?
        .update(row)
        .await?;
    Ok(())
}

/// 置事件终态（重试耗尽或远端明确拒绝）。`error_message` 按传入原样落库——
/// 「连续失败超 N 次」的前缀由重试耗尽那个调用点拼接，避免把即时终态写歪。
async fn mark_terminal_event(
    runner: &RoundRunner,
    event_id: i64,
    error_message: &str,
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut row = Record::new();
    row.insert("processed_at", serde_json::json!(now));
    row.insert("retry_count", serde_json::json!(MAX_EVENT_RETRY_COUNT));
    row.insert("last_error", serde_json::json!(error_message));
    runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))?
        .update(row)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EventReplay {
        status: String,
        reverted: bool,
        fail_write: bool,
        writes: std::sync::Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl OutboundTransport for EventReplay {
        async fn send(
            &self,
            request: crate::addon::feishu::domain::outbound::OutboundRequest,
        ) -> Result<crate::addon::feishu::domain::outbound::OutboundResponse, yang_base::BaseError>
        {
            use serde_json::json;
            let body = if request.url.contains("/instances/") {
                json!({"code":0,"data":{"instance_code":"INST-TERMINAL","status":self.status,"reverted":self.reverted,"serial_number":"SN-1"}})
            } else if request.url.ends_with("/fields") {
                json!({"code":0,"data":{"has_more":false,"total":2,"items":[{"field_id":"fldNumber","field_name":"编号","type":1},{"field_id":"fldStatus","field_name":"状态","type":1}]}})
            } else if request.url.ends_with("/records/batch_update") {
                self.writes
                    .lock()
                    .unwrap_or_else(|e| panic!("{e}"))
                    .push(request.json_body.unwrap_or_default());
                if self.fail_write {
                    // 1254607 Data not ready 是官方「稍后重试」：验证可重试失败仍走 Failed。
                    json!({"code":1254607,"msg":"模拟暂时失败"})
                } else {
                    json!({"code":0,"data":{}})
                }
            } else {
                panic!("不得发生其它出站请求: {}", request.url);
            };
            Ok(crate::addon::feishu::domain::outbound::OutboundResponse {
                status: 200,
                body: body.to_string(),
                bytes: None,
                headers: Default::default(),
            })
        }
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库和 Redis DB 15"]
    async fn terminal_task_still_syncs_later_reverted_and_deleted_events() -> anyhow::Result<()> {
        use crate::addon::feishu::domain::tenant_token::TenantTokenCache;
        use serde_json::json;
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
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(
            redis_url.trim_end_matches('/').ends_with("/15"),
            "只允许 Redis DB 15"
        );
        let redis =
            yang_db::RedisClient::connect_with_config(&redis_url, yang_db::RedisConfig::default())
                .await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let cache = Arc::new(RedisTenantTokenCache::new(redis, &namespace));
        cache.put("test-tenant", 7200).await?;
        let config_id = sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,status_field,base_timezone,enabled,created_at,updated_at) VALUES ('事件测试','appTest',?,'DEF-TEST','fldPerson','fldNumber','fldStatus','Asia/Shanghai',true,NOW(),NOW())")
            .bind(format!("tbl{namespace}")).execute(database.pool()).await?.last_insert_id();
        let instance_code = format!("INST-{namespace}");
        let task_id = sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,instance_code,instance_status,created_at,updated_at) VALUES (?,? ,?,'backfilled',?,'APPROVED',NOW(),NOW())")
            .bind(config_id).bind(format!("rec{namespace}")).bind(&namespace).bind(&instance_code).execute(database.pool()).await?.last_insert_id();
        for (status, reverted, expected, fail_write) in [
            ("APPROVED", true, "已通过(已撤销)", false),
            ("DELETED", false, "已删除", false),
            ("APPROVED", false, "已通过", true),
        ] {
            let transport = Arc::new(EventReplay {
                status: status.into(),
                reverted,
                fail_write,
                writes: Default::default(),
            });
            let tokens = Arc::new(TenantTokenProvider::new(
                cache.clone(),
                transport.clone(),
                Arc::new(TokioSleeper),
                FeishuCredentials {
                    app_id: "cli_test".into(),
                    app_secret: "test-secret".into(),
                },
                &namespace,
            )?);
            let runner = RoundRunner {
                tools: Arc::new(yang_base::tools::ToolsBuilder::new().build()?),
                context: context.clone(),
                tokens,
                transport: transport.clone(),
                sleeper: Arc::new(TokioSleeper),
                timezone_offset: FixedOffset::from_iana("Asia/Shanghai")?,
            };
            let event = PendingEvent {
                id: 1,
                instance_code: instance_code.clone(),
                retry_count: 0,
            };
            match process_event_inner(&runner, &event).await {
                EventOutcome::Processed if !fail_write => {}
                EventOutcome::Processed => panic!("回填失败不能把事件确认完成"),
                EventOutcome::Failed(_) if fail_write => {
                    let saved: String = sqlx::query_scalar(
                        "SELECT instance_status FROM feishu_approval_task WHERE id=?",
                    )
                    .bind(task_id)
                    .fetch_one(database.pool())
                    .await?;
                    assert_eq!(saved, "DELETED", "回填失败不得提前更新本地同步事实");
                    continue;
                }
                EventOutcome::Failed(message) => panic!("事件同步失败: {message}"),
                EventOutcome::Skipped(message) => panic!("不应跳过真实任务事件: {message}"),
                EventOutcome::Terminal(message) => panic!("可重试回填失败不应立即终态: {message}"),
            }
            {
                let writes = transport.writes.lock().unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(writes.len(), 1);
                assert_eq!(writes[0]["records"][0]["fields"]["状态"], json!(expected));
            }
            let (saved, synced): (String, Option<i64>) = sqlx::query_as(
                "SELECT instance_status,last_synced_at FROM feishu_approval_task WHERE id=?",
            )
            .bind(task_id)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(saved, status);
            assert!(synced.is_some(), "记录最新同步时间");
        }
        sqlx::query("DELETE FROM feishu_approval_task WHERE id=?")
            .bind(task_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM feishu_approval_config WHERE id=?")
            .bind(config_id)
            .execute(database.pool())
            .await?;
        cache.invalidate().await?;
        Ok(())
    }

    #[test]
    fn event_instance_uuid_reads_only_instance_events() {
        let instance = r#"{"uuid":"event-id","event":{"type":"approval_instance","uuid":"EF21C6FA-5A3F-8753-9BE8-C01690F2E1EA"}}"#;
        assert_eq!(
            event_instance_uuid(instance).as_deref(),
            Some("ef21c6fa-5a3f-8753-9be8-c01690f2e1ea")
        );
        let task = r#"{"uuid":"event-id","event":{"type":"approval_task","uuid":"X"}}"#;
        assert_eq!(
            event_instance_uuid(task),
            None,
            "顶层事件 id / approval_task 的 uuid 不得当实例 uuid"
        );
        assert_eq!(event_instance_uuid("not-json"), None);
        let empty = r#"{"event":{"type":"approval_instance","uuid":"  "}}"#;
        assert_eq!(event_instance_uuid(empty), None);
    }

    #[test]
    fn fatal_outbound_failure_terminalizes_event() {
        use crate::addon::feishu::domain::outbound::{classify, FailureKind};

        let fatal = classify(
            200,
            &Default::default(),
            r#"{"code":1254043,"msg":"record not found"}"#,
        )
        .unwrap_or_else(|| panic!("1254043 必须判失败"));
        assert!(matches!(fatal, FailureKind::Fatal { .. }));
        assert!(matches!(
            outbound_failure_outcome(
                "扩展字段回填失败",
                OutboundFailure {
                    kind: fatal,
                    message: "record not found".to_string(),
                }
            ),
            EventOutcome::Terminal(_)
        ));

        let retry = classify(
            200,
            &Default::default(),
            r#"{"code":1254607,"msg":"Data not ready"}"#,
        )
        .unwrap_or_else(|| panic!("1254607 必须判失败"));
        assert!(matches!(
            outbound_failure_outcome(
                "扩展字段回填失败",
                OutboundFailure {
                    kind: retry,
                    message: "Data not ready".to_string(),
                }
            ),
            EventOutcome::Failed(_)
        ));
    }

    #[test]
    fn event_batch_stays_within_framework_limit() {
        // 常量断言用 const block 让编译器检查（clippy::assertions_on_constants），
        // 与模块顶部的 `const _` 编译期守护互为镜像。
        const { assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE) };
    }

    #[test]
    fn max_retry_count_is_positive() {
        const { assert!(MAX_EVENT_RETRY_COUNT > 0) };
    }
}
