//! 审批事件处理 worker：从 `feishu_approval_event` 表取未处理事件，富化后回写多维表格。
//!
//! 骨架与 `feishu_approval_worker` 一致：`watch` 通道 + `JoinHandle`，`shutdown()` 发信号后
//! `await` 任务退出；`Drop` 也发一次，避免忘记关闭时任务悬挂。
//!
//! # 单实例部署
//!
//! 与审批派发 worker 同一条决策：`deploy/deploy-blue-green.sh` 的 `cutover` 先 `stop_pair`
//! 停线上再起新的，两实例不重叠。所以**不做跨实例互斥**。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::Instrument;
use yang_base::table::{Record, SortOrder};
use yang_base::tools::Tools;

use crate::addon::feishu::domain::approval::{get_instance, subscribe_approval, GetOutcome};
use crate::addon::feishu::domain::approval_convert::FixedOffset;
use crate::addon::feishu::domain::approval_dispatch::{
    fill_department_name, write_extended, BitableBackfill, DispatchInput,
};
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    HttpClientTransport, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 每轮最多处理多少条事件。
const EVENT_BATCH: usize = 50;

/// 连续失败多少次后放弃（置终态，不再重试）。
const MAX_RETRY_COUNT: i64 = 10;

/// 编译期守护：批量不超框架上限。
const _: () = assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 事件处理 worker。
pub(crate) struct FeishuEventWorker {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
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
    transport: Arc<HttpClientTransport>,
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
    if let Err(error) = subscribe_enabled_configs(&runner).await {
        tracing::warn!(error = %error, "启动期订阅审批定义失败（下次启动会重试）");
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
                run_round(&runner, "poll").await;
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
            EventOutcome::Failed(message) => {
                failed += 1;
                let new_count = event.retry_count + 1;
                if new_count >= MAX_RETRY_COUNT {
                    if let Err(error) = mark_terminal_event(runner, event.id, &message).await {
                        tracing::warn!(error = %error, event_id = event.id, "置事件终态失败");
                    }
                    terminal += 1;
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
            EventOutcome::Skipped => {
                if let Err(error) = mark_processed(runner, event.id).await {
                    tracing::warn!(error = %error, event_id = event.id, "标记跳过事件失败");
                }
            }
        }
    }

    tracing::info!(
        claimed = events.len(),
        processed,
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
    Processed,
    Failed(String),
    /// 幂等跳过（同实例已终态）。
    Skipped,
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
        Ok(query) => {
            match query.select_fields(&["id", "record_id", "config_id", "instance_status"]) {
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
            }
        }
        Err(error) => {
            tracing::error!(error = %error, "构造 task 查询失败");
            return EventOutcome::Failed(format!("构造 task 查询失败: {error}"));
        }
    };

    let task_row = match task_rows.first() {
        Some(row) => row,
        None => {
            tracing::warn!(
                instance_code = %event.instance_code,
                "事件对应的审批任务不存在，可能尚未派发"
            );
            return EventOutcome::Failed("task 行不存在".to_string());
        }
    };

    let task_id: i64 = match task_row.require("id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, "task 行缺 id");
            return EventOutcome::Failed(format!("task 行缺 id: {error}"));
        }
    };
    let record_id: String = match task_row.require("record_id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, task_id, "task 行缺 record_id");
            return EventOutcome::Failed(format!("task 行缺 record_id: {error}"));
        }
    };
    let config_id: i64 = match task_row.require("config_id") {
        Ok(id) => id,
        Err(error) => {
            tracing::error!(error = %error, task_id, "task 行缺 config_id");
            return EventOutcome::Failed(format!("task 行缺 config_id: {error}"));
        }
    };

    tracing::debug!(
        task_id,
        record_id = %record_id,
        config_id,
        "找到对应的派发任务"
    );

    // 终态收敛：task 行已是终态，后续事件幂等跳过。
    if let Ok(Some(status)) = task_row.optional::<String>("instance_status") {
        match status.as_str() {
            "APPROVED" | "REJECTED" | "CANCELED" | "DELETED" => {
                tracing::info!(
                    task_id,
                    task_status = %status,
                    "任务已终态，事件幂等跳过"
                );
                return EventOutcome::Skipped;
            }
            _ => {}
        }
    }

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
            return EventOutcome::Failed(format!("获取实例详情失败: {}", failure.message));
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
            return EventOutcome::Failed(format!("读取表结构失败: {}", failure.message));
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
    write_extended(&backfill, &input, &detail).await;
    tracing::info!(
        record_id = %record_id,
        "多维表格回写完成"
    );

    // 7. 更新 task 行的 instance_status
    if let Some(status) = detail.status.as_deref() {
        tracing::debug!(task_id, status = %status, "步骤 7: 更新 task 的 instance_status");
        let mut row = Record::new();
        row.insert("instance_status", serde_json::json!(status));
        let result = runner
            .context
            .approval_tasks()
            .query()
            .where_eq("id", serde_json::json!(task_id));
        match result {
            Ok(query) => {
                if let Err(error) = query.update(row).await {
                    tracing::warn!(error = %error, task_id, "更新 task instance_status 失败");
                } else {
                    tracing::info!(task_id, status = %status, "task instance_status 更新成功");
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, task_id, "构造 task 更新查询失败");
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

/// 标记事件已处理（设 `processed_at`，清 `last_error`）。
async fn mark_processed(runner: &RoundRunner, event_id: i64) -> anyhow::Result<()> {
    let mut row = Record::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    row.insert("processed_at", serde_json::json!(now));
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

/// 置事件终态（连续失败超限）。
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
    row.insert("retry_count", serde_json::json!(MAX_RETRY_COUNT));
    row.insert(
        "last_error",
        serde_json::json!(format!("连续失败超 {MAX_RETRY_COUNT} 次: {error_message}")),
    );
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

    #[test]
    fn event_batch_stays_within_framework_limit() {
        // 常量断言用 const block 让编译器检查（clippy::assertions_on_constants），
        // 与模块顶部的 `const _` 编译期守护互为镜像。
        const { assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE) };
    }

    #[test]
    fn max_retry_count_is_positive() {
        const { assert!(MAX_RETRY_COUNT > 0) };
    }
}
