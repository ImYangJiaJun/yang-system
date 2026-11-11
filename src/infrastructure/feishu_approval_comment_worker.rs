//! 审批评论回写的后台 worker。
//!
//! # 两条入口，一本台账
//!
//! - **事件（方案 A）**：每 2 秒认领 `feishu_bitable_event` 未处理行，按
//!   `file_token + table_id` 命中启用规则，把目标记录 upsert 成 `pending`；
//! - **轮询兜底（方案 B）**：每 30 秒对每条启用规则按「审批评论状态列 isEmpty」
//!   扫目标表（每页 500、最多 20 页），把候选记录 upsert 成 `pending`。
//!
//! 两条入口都写同一张 `feishu_approval_comment_run`（唯一键
//! `(rule_id, record_id)`），因此重复事件/两条入口同时命中都是幂等的。
//!
//! # 状态机与「绝不重复发评论」
//!
//! `pending -> commenting -> commented -> done`。评论创建成功、`comment_id`
//! 落库后即进入 `commented`；若随后状态列写入失败，行保持 `commented`，
//! 下一轮**只补状态列**，不会再发评论。发评论之前还会 GET 一次实例评论，
//! 按固定标记 `prefix + record_id` 去重——这是重启/回滚后唯一可信的判据。
//!
//! # 部署形态
//!
//! 与派发/回写 worker 相同：单实例、单线程循环。`commenting` 状态下
//! `next_attempt_at` 兼作租约（+120 秒），进程崩溃后超时可被重新认领；
//! 重新认领仍会先查评论标记，不会双发。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::sync::watch;
use yang_base::table::{Record, SortOrder};
use yang_base::tools::Tools;
use yang_runtime::shutdown::OwnedTask;

use crate::addon::feishu::approval::domain::comment_run_table::{wake_state, MAX_COMMENT_ATTEMPTS};
use crate::addon::feishu::domain::approval::{
    create_instance_comment, list_instance_comments, subscribe_bitable_file,
};
use crate::addon::feishu::domain::approval_comment::{
    evaluate_condition, find_marker, parse_comment_config, payment_fingerprint, render_comment,
    CommentCondition, CommentConfig,
};
use crate::addon::feishu::domain::approval_export::RULE_ROW_KEY;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates, RecordItem};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    FailureKind, HttpClientTransport, OutboundFailure, OutboundTransport, TokioSleeper,
};
use crate::addon::feishu::domain::repository::all_pages;
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};
use crate::config::FeishuSettings;

/// 每轮认领多少条未处理事件。
const EVENT_BATCH: usize = 50;

/// 每轮认领多少条到期台账行。
const RUN_BATCH: usize = 20;

/// 事件入口轮询间隔。
const EVENT_INTERVAL: Duration = Duration::from_secs(2);

/// 扫描兜底间隔。
const SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// 扫描最多翻多少页；到上限必须把丢弃量写进日志。
const SCAN_MAX_PAGES: usize = 20;

/// `commenting` 租约：崩溃后超过这个时间可被重新认领。
const COMMENTING_LEASE_SECONDS: i64 = 120;

/// 可重试失败的基础退避（秒），指数增长到上限。
const RETRY_BASE_SECONDS: i64 = 60;
const RETRY_MAX_SECONDS: i64 = 1800;

/// 规则表读取列。
const RULE_FIELDS: &[&str] = &[
    "id",
    "title",
    "target_base_token",
    "target_table_id",
    "comment_config_json",
];

/// 台账读取列。
const RUN_FIELDS: &[&str] = &[
    "id",
    "rule_id",
    "instance_code",
    "record_id",
    "state",
    "comment_id",
    "attempts",
];

/// 事件读取列。
const EVENT_FIELDS: &[&str] = &[
    "id",
    "event_id",
    "event_type",
    "file_token",
    "table_id",
    "record_id",
    "action",
    "changed_field_ids",
];

const _: () = assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);
const _: () = assert!(RUN_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 空转轮次降为 DEBUG（事件轮 2 秒一次，INFO 会刷出约 4.3 万条/天）；
/// 有实际工作才 INFO。字段列表只写一次，避免两种日志漂移。
macro_rules! round_log {
    ($idle:expr, $($fields:tt)*) => {
        if $idle {
            tracing::debug!($($fields)*);
        } else {
            tracing::info!($($fields)*);
        }
    };
}

/// 审批评论 worker。
pub(crate) struct FeishuApprovalCommentWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl FeishuApprovalCommentWorker {
    /// 启动 worker。调用方必须先确认 `settings.can_pull()`。
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

        let (shutdown, receiver) = watch::channel(false);
        let runner = RoundRunner {
            context,
            tokens,
            transport,
        };
        let task = tokio::spawn(run_loop(runner, receiver));
        tracing::info!("飞书审批评论 Worker 已启动");
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
        })
    }

    /// 主循环是否已经结束（正常退出或 panic）。
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
            .map_err(|error| anyhow::anyhow!("等待审批评论 Worker 退出失败: {error}"))?;
        Ok(())
    }
}

impl Drop for FeishuApprovalCommentWorker {
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

struct RoundRunner {
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    transport: Arc<dyn OutboundTransport>,
}

impl RoundRunner {
    fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }
}

/// 主循环：启动时重订启用规则涉及的多维表格事件，然后事件/扫描双节拍。
async fn run_loop(runner: RoundRunner, mut shutdown: watch::Receiver<bool>) {
    let subscribe = async {
        if let Err(error) = subscribe_enabled(&runner).await {
            tracing::warn!(error = %error, "启动期多维表格事件订阅失败（后续保存规则会再订）");
        }
    };
    if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), subscribe).await {
        tracing::info!("飞书审批评论 Worker 收到关闭信号（启动订阅阶段）");
        return;
    }

    let mut event_ticker = tokio::time::interval(EVENT_INTERVAL);
    let mut scan_ticker = tokio::time::interval(SCAN_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("飞书审批评论 Worker 收到关闭信号");
                    return;
                }
            }
            _ = event_ticker.tick() => {
                let round = run_event_round(&runner);
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("飞书审批评论 Worker 收到关闭信号，已取消事件轮次");
                    return;
                }
            }
            _ = scan_ticker.tick() => {
                let round = run_scan_round(&runner);
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("飞书审批评论 Worker 收到关闭信号，已取消扫描轮次");
                    return;
                }
            }
        }
    }
}

/// 启动期订阅：对所有启用评论的规则订阅目标 Base 的事件。失败只 WARN。
async fn subscribe_enabled(runner: &RoundRunner) -> anyhow::Result<()> {
    let rules = load_enabled_rules(runner).await?;
    let mut seen = HashSet::new();
    for rule in &rules {
        if !seen.insert(rule.base_token.clone()) {
            continue;
        }
        match subscribe_bitable_file(
            runner.transport(),
            &TokioSleeper,
            &runner.tokens,
            &rule.base_token,
        )
        .await
        {
            Ok(()) => tracing::info!(base_token = %rule.base_token, "多维表格事件订阅确认"),
            Err(failure) => tracing::warn!(
                error = %failure.message,
                base_token = %rule.base_token,
                "多维表格事件订阅失败"
            ),
        }
    }
    Ok(())
}

/// 跑一轮事件入口（并顺带处理到期台账）。
#[tracing::instrument(name = "approval_comment_event_round", skip_all)]
async fn run_event_round(runner: &RoundRunner) {
    let started = Instant::now();
    let rules = match load_enabled_rules(runner).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::error!(error = %error, "读取审批评论规则失败");
            return;
        }
    };
    let ingest = ingest_events(runner, &rules).await;
    let process = process_due(runner, &rules).await;
    round_log!(
        ingest.scanned == 0 && ingest.failed == 0 && process.claimed == 0,
        enabled_rules = rules.len(),
        events = ingest.scanned,
        queued = ingest.queued,
        event_failed = ingest.failed,
        claimed = process.claimed,
        done = process.done,
        retry = process.retry,
        failed = process.failed,
        waiting = process.waiting,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批评论事件轮次结束"
    );
}

/// 跑一轮扫描兜底。
#[tracing::instrument(name = "approval_comment_scan_round", skip_all)]
async fn run_scan_round(runner: &RoundRunner) {
    let started = Instant::now();
    let rules = match load_enabled_rules(runner).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::error!(error = %error, "读取审批评论规则失败");
            return;
        }
    };
    let mut scanned = 0usize;
    let mut queued = 0usize;
    let mut failed = 0usize;
    for rule in &rules {
        match scan_rule(runner, rule).await {
            Ok(report) => {
                scanned += report.scanned;
                queued += report.queued;
            }
            Err(error) => {
                failed += 1;
                tracing::warn!(error = %error, rule_id = rule.id, "扫描目标表失败");
            }
        }
    }
    let process = process_due(runner, &rules).await;
    round_log!(
        scanned == 0 && queued == 0 && failed == 0 && process.claimed == 0,
        enabled_rules = rules.len(),
        scanned,
        queued,
        scan_failed = failed,
        claimed = process.claimed,
        done = process.done,
        retry = process.retry,
        failed = process.failed,
        waiting = process.waiting,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批评论扫描轮次结束"
    );
}

// ---------------------------------------------------------------------------
// 规则加载
// ---------------------------------------------------------------------------

/// 一条启用评论的规则。
#[derive(Debug, Clone)]
struct CommentRule {
    id: i64,
    title: String,
    base_token: String,
    table_id: String,
    config: CommentConfig,
}

async fn load_enabled_rules(runner: &RoundRunner) -> anyhow::Result<Vec<CommentRule>> {
    let rows = all_pages(
        runner
            .context
            .approval_export_rules()
            .query()
            .where_eq("enabled", serde_json::json!(true))?
            .select_fields(RULE_FIELDS)?,
        10,
    )
    .await?;
    let mut rules = Vec::new();
    for row in rows {
        let id: i64 = row.require("id")?;
        let raw: Option<String> = row.optional("comment_config_json")?;
        let config = match parse_comment_config(raw.as_deref()) {
            Ok(Some(config)) if config.enabled => config,
            Ok(_) => continue,
            Err(error) => {
                tracing::error!(rule_id = id, error = %error, "审批评论配置无法解析，已跳过");
                continue;
            }
        };
        rules.push(CommentRule {
            id,
            title: row.optional::<String>("title")?.unwrap_or_default(),
            base_token: row.require("target_base_token")?,
            table_id: row.require("target_table_id")?,
            config,
        });
    }
    Ok(rules)
}

// ---------------------------------------------------------------------------
// 事件入口
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct EventIngestReport {
    scanned: usize,
    queued: usize,
    failed: usize,
}

async fn ingest_events(runner: &RoundRunner, rules: &[CommentRule]) -> EventIngestReport {
    let rows = match claim_events(runner).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(error = %error, "多维表格事件认领失败");
            return EventIngestReport {
                failed: 1,
                ..EventIngestReport::default()
            };
        }
    };
    let mut report = EventIngestReport {
        scanned: rows.len(),
        ..EventIngestReport::default()
    };
    for row in rows {
        match ingest_one(runner, rules, &row).await {
            Ok(queued) => {
                report.queued += queued;
                if let Err(error) = mark_event_processed(runner, &row).await {
                    tracing::warn!(error = %error, "标记事件已处理失败，下轮重试");
                }
            }
            Err(error) => {
                report.failed += 1;
                let message = error.to_string();
                if let Err(save_error) = mark_event_failed(runner, &row, &message).await {
                    tracing::warn!(error = %save_error, "记录事件处理失败原因失败");
                }
            }
        }
    }
    report
}

async fn claim_events(runner: &RoundRunner) -> anyhow::Result<Vec<Record>> {
    Ok(runner
        .context
        .bitable_events()
        .query()
        .where_null("processed_at")?
        .order_by("id", SortOrder::Asc)?
        .page(1, EVENT_BATCH)?
        .select_fields(EVENT_FIELDS)?
        .all()
        .await?)
}

async fn ingest_one(
    runner: &RoundRunner,
    rules: &[CommentRule],
    row: &Record,
) -> anyhow::Result<usize> {
    let file_token: String = row.require("file_token")?;
    let table_id: String = row.require("table_id")?;
    let record_id: String = row.require("record_id")?;
    let action: String = row.require("action")?;
    if !matches!(action.as_str(), "record_added" | "record_edited") {
        tracing::debug!(action = %action, "非记录新增/编辑事件，不产生评论候选");
        return Ok(0);
    }
    let changed = parse_changed_fields(row.optional::<String>("changed_field_ids")?);
    let mut queued = 0usize;
    for rule in rules.iter().filter(|rule| {
        rule.base_token == file_token && (table_id.is_empty() || rule.table_id == table_id)
    }) {
        if !changed_field_ids_hit(&rule.config, changed.as_deref()) {
            tracing::debug!(rule_id = rule.id, "变更字段与评论配置无关，按自触发过滤");
            continue;
        }
        match upsert_pending(runner, rule.id, &record_id).await {
            Ok(()) => queued += 1,
            Err(error) => return Err(anyhow::anyhow!("规则 {} 的事件入队失败: {error}", rule.id)),
        }
    }
    Ok(queued)
}

/// 解析变更字段 JSON；`None` = 上游未提供（不能据此过滤）。
fn parse_changed_fields(raw: Option<String>) -> Option<Vec<String>> {
    let raw = raw?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let values: Vec<String> = serde_json::from_str(raw).ok()?;
    Some(
        values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
    )
}

/// 自触发过滤：只认配置字段（含触发列）的变更；状态列自身变化不触发。
fn changed_field_ids_hit(config: &CommentConfig, changed: Option<&[String]>) -> bool {
    let Some(changed) = changed else {
        // 上游没给变更列表：无法区分，放行（commenting/done 台账会兜住重复）。
        return true;
    };
    if changed.is_empty() {
        return true;
    }
    changed
        .iter()
        .any(|field_id| config.is_relevant_field(field_id))
}

async fn mark_event_processed(runner: &RoundRunner, row: &Record) -> anyhow::Result<()> {
    let id: i64 = row.require("id")?;
    let mut record = Record::new();
    record.insert("processed_at", serde_json::json!(now_seconds()));
    record.insert("process_error", serde_json::Value::Null);
    runner
        .context
        .bitable_events()
        .query()
        .where_eq("id", serde_json::json!(id))?
        .update(record)
        .await?;
    Ok(())
}

async fn mark_event_failed(
    runner: &RoundRunner,
    row: &Record,
    message: &str,
) -> anyhow::Result<()> {
    let id: i64 = row.require("id")?;
    let mut record = Record::new();
    record.insert("process_error", serde_json::json!(message));
    runner
        .context
        .bitable_events()
        .query()
        .where_eq("id", serde_json::json!(id))?
        .update(record)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 扫描兜底
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct ScanReport {
    scanned: usize,
    queued: usize,
}

async fn scan_rule(runner: &RoundRunner, rule: &CommentRule) -> anyhow::Result<ScanReport> {
    let coordinates = BitableCoordinates {
        app_token: rule.base_token.clone(),
        table_id: rule.table_id.clone(),
        view_id: None,
    };
    // 列名可改：优先按 field_id 现场解析；取字段失败才退回保存时缓存的名字。
    let status_field_name = match bitable::list_all_fields(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => bitable::resolve_field_name(&fields, &rule.config.status_field_id)
            .unwrap_or_else(|_| rule.config.status_field_name.clone()),
        Err(failure) => {
            tracing::warn!(
                error = %failure.message,
                rule_id = rule.id,
                "解析评论状态列名失败，沿用缓存列名"
            );
            rule.config.status_field_name.clone()
        }
    };
    let status_field_name = status_field_name.trim();
    if status_field_name.is_empty() {
        return Err(anyhow::anyhow!(
            "规则 {} 缺少审批评论状态列名，无法按 isEmpty 扫描",
            rule.id
        ));
    }
    let body = serde_json::json!({
        "filter": {
            "conjunction": "and",
            "conditions": [{
                "field_name": status_field_name,
                "operator": "isEmpty",
                "value": []
            }]
        },
        "automatic_fields": false,
    });

    let mut items: Vec<RecordItem> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut total: Option<i64> = None;
    let mut reached = false;
    for page in 0..SCAN_MAX_PAGES {
        let data = bitable::search_records(
            runner.transport(),
            &TokioSleeper,
            &runner.tokens,
            &coordinates,
            body.clone(),
            cursor.as_deref(),
        )
        .await
        .map_err(|failure| anyhow::anyhow!("查询记录失败: {}", failure.message))?;
        if page == 0 {
            total = data.total;
        }
        items.extend(data.items);
        if !data.has_more {
            reached = true;
            break;
        }
        match data.page_token.filter(|token| !token.trim().is_empty()) {
            Some(token) => cursor = Some(token),
            None => {
                return Err(anyhow::anyhow!(
                    "查询记录返回 has_more=true 却没有 page_token"
                ))
            }
        }
    }
    if !reached {
        // 不静默截断：把「还有多少条没扫」写进日志。
        let dropped = total
            .map(|total| (total - items.len() as i64).max(0))
            .unwrap_or(-1);
        tracing::warn!(
            rule_id = rule.id,
            scanned = items.len(),
            dropped,
            max_pages = SCAN_MAX_PAGES,
            "审批评论扫描达到页数上限，剩余记录本轮未扫描（将由下一轮/事件补上）"
        );
    }

    let mut queued = 0usize;
    for item in &items {
        if item.record_id.trim().is_empty() {
            continue;
        }
        upsert_pending(runner, rule.id, item.record_id.trim()).await?;
        queued += 1;
    }
    tracing::debug!(
        rule_id = rule.id,
        title = %rule.title,
        scanned = items.len(),
        queued,
        "审批评论扫描完成"
    );
    Ok(ScanReport {
        scanned: items.len(),
        queued,
    })
}

// ---------------------------------------------------------------------------
// 台账
// ---------------------------------------------------------------------------

/// 幂等入队：不存在则新建；已有可恢复状态则重置为 pending。
async fn upsert_pending(runner: &RoundRunner, rule_id: i64, record_id: &str) -> anyhow::Result<()> {
    let now = now_seconds();
    let existing = runner
        .context
        .approval_comment_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("record_id", serde_json::json!(record_id))?
        .select_fields(&["id", "state"])?
        .optional()
        .await?;
    if let Some(existing) = existing {
        let state: String = existing.optional("state")?.unwrap_or_default();
        let Some(next) = wake_state(&state) else {
            return Ok(());
        };
        let id: i64 = existing.require("id")?;
        let mut record = Record::new();
        record.insert("state", serde_json::json!(next));
        record.insert("next_attempt_at", serde_json::json!(now));
        record.insert("last_error", serde_json::Value::Null);
        runner
            .context
            .approval_comment_runs()
            .query()
            .where_eq("id", serde_json::json!(id))?
            .update(record)
            .await?;
        return Ok(());
    }

    let mut record = Record::new();
    record.insert("rule_id", serde_json::json!(rule_id));
    record.insert("record_id", serde_json::json!(record_id));
    record.insert("state", serde_json::json!("pending"));
    record.insert("attempts", serde_json::json!(0));
    record.insert("next_attempt_at", serde_json::json!(now));
    if let Err(error) = runner
        .context
        .approval_comment_runs()
        .query()
        .insert(record)
        .await
    {
        // 并发插入撞唯一键：另一条入口已入队，退化为一次唤醒更新。
        if crate::db::is_unique_violation(&error) {
            return Ok(());
        }
        return Err(error.into());
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy)]
struct ProcessReport {
    claimed: usize,
    done: usize,
    retry: usize,
    failed: usize,
    waiting: usize,
}

#[derive(Debug, Clone)]
struct RunRow {
    id: i64,
    rule_id: i64,
    record_id: String,
    instance_code: Option<String>,
    state: String,
    comment_id: Option<String>,
    attempts: i64,
}

async fn process_due(runner: &RoundRunner, rules: &[CommentRule]) -> ProcessReport {
    let rows = match claim_runs(runner, rules).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(error = %error, "审批评论台账认领失败");
            return ProcessReport::default();
        }
    };
    if rows.is_empty() {
        return ProcessReport::default();
    }
    let rules_by_id: BTreeMap<i64, &CommentRule> =
        rules.iter().map(|rule| (rule.id, rule)).collect();
    let mut report = ProcessReport {
        claimed: rows.len(),
        ..ProcessReport::default()
    };
    let mut status_names: HashMap<i64, String> = HashMap::new();
    for row in &rows {
        // `claim_runs` 已按启用规则过滤；这里只是防御（认领后规则掉线）。
        // 不改写台账状态：`failed` 不会被自动唤醒，重新启用后 pending/retry
        // 行会被自然认领。
        let Some(rule) = rules_by_id.get(&row.rule_id) else {
            continue;
        };
        match process_one(runner, rule, row, &mut status_names).await {
            StepOutcome::Done => report.done += 1,
            StepOutcome::Retry => report.retry += 1,
            StepOutcome::Failed => report.failed += 1,
            StepOutcome::Waiting => report.waiting += 1,
        }
    }
    report
}

/// 认领范围 = 启用规则的 id；没有启用规则时返回 `None`（不查库）。
///
/// 过滤必须带规则 id：否则停用规则的台账行仍会被认领，而处理时又把它们置
/// `failed`（终态，重新启用不会自动唤醒）。
fn claim_rule_ids(rules: &[CommentRule]) -> Option<Vec<serde_json::Value>> {
    (!rules.is_empty()).then(|| {
        rules
            .iter()
            .map(|rule| serde_json::json!(rule.id))
            .collect()
    })
}

async fn claim_runs(runner: &RoundRunner, rules: &[CommentRule]) -> anyhow::Result<Vec<RunRow>> {
    let Some(rule_ids) = claim_rule_ids(rules) else {
        return Ok(Vec::new());
    };
    let now = now_seconds();
    let states: Vec<serde_json::Value> = ["pending", "retry", "commented", "commenting"]
        .iter()
        .map(|state| serde_json::json!(state))
        .collect();
    let rows = runner
        .context
        .approval_comment_runs()
        .query()
        .where_in("rule_id", rule_ids)?
        .where_in("state", states)?
        .where_lte("next_attempt_at", serde_json::json!(now))?
        .order_by("next_attempt_at", SortOrder::Asc)?
        .order_by("id", SortOrder::Asc)?
        .page(1, RUN_BATCH)?
        .select_fields(RUN_FIELDS)?
        .all()
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(RunRow {
                id: row.require("id")?,
                rule_id: row.require("rule_id")?,
                record_id: row.require("record_id")?,
                instance_code: row
                    .optional::<String>("instance_code")?
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                state: row
                    .optional::<String>("state")?
                    .unwrap_or_else(|| "pending".to_string()),
                comment_id: row
                    .optional::<String>("comment_id")?
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                attempts: row.optional::<i64>("attempts")?.unwrap_or(0),
            })
        })
        .collect()
}

/// 单条台账的推进结果（只用于计数与日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    Done,
    Retry,
    Failed,
    Waiting,
}

async fn process_one(
    runner: &RoundRunner,
    rule: &CommentRule,
    row: &RunRow,
    status_names: &mut HashMap<i64, String>,
) -> StepOutcome {
    // 1) 已评论（但状态列没写成）的行：只补状态列，绝不再读条件/发评论。
    if row.state == "commented" {
        return write_status(
            runner,
            rule,
            row,
            row.comment_id.as_deref().unwrap_or_default(),
            status_names,
        )
        .await;
    }

    // 2) 先读记录并判定条件。支付字段没到位时**不应**去反查实例映射：导出
    //    worker 刚建记录、台账尚未写成 written 的并发窗口会被误判成 failed。
    let fields = match load_record_fields(runner, rule, &row.record_id).await {
        Ok(fields) => fields,
        Err(failure) => {
            return handle_outbound_failure(runner, rule, row, failure, "读取目标记录").await;
        }
    };
    match evaluate_condition(&rule.config, &fields) {
        CommentCondition::Waiting(reason) => {
            let mut update = Record::new();
            update.insert("state", serde_json::json!("waiting"));
            update.insert("next_attempt_at", serde_json::Value::Null);
            update.insert("last_error", serde_json::json!(reason));
            let _ = update_run(runner, row.id, update).await;
            return StepOutcome::Waiting;
        }
        CommentCondition::Skipped(reason) => {
            let mut update = Record::new();
            update.insert("state", serde_json::json!("skipped"));
            update.insert("next_attempt_at", serde_json::Value::Null);
            update.insert("last_error", serde_json::json!(reason));
            let _ = update_run(runner, row.id, update).await;
            return StepOutcome::Waiting;
        }
        CommentCondition::Ready => {}
    }

    // 3) 条件已满足，才需要实例编码：目标表没有实例列，只能从回写台账反查。
    let instance_code = match &row.instance_code {
        Some(code) => code.clone(),
        None => match find_instance_code(runner, rule.id, &row.record_id).await {
            Ok(Some(code)) => {
                let mut update = Record::new();
                update.insert("instance_code", serde_json::json!(&code));
                if let Err(error) = update_run(runner, row.id, update).await {
                    tracing::warn!(error = %error, run_id = row.id, "回填实例编码失败");
                }
                code
            }
            Ok(None) => {
                // 导出 worker 刚建记录、回写台账尚未写成 `written` 的毫秒级窗口
                // 里会查不到映射：先退避重试，连续达到上限才判失败。
                let message = format!(
                    "找不到目标记录 {} 对应的审批实例：回写台账没有 write_status=written 的行",
                    row.record_id
                );
                if row.attempts + 1 >= MAX_COMMENT_ATTEMPTS {
                    update_run_failed(runner, row, &message).await;
                    mark_status_failed(runner, rule, row, &message).await;
                    return StepOutcome::Failed;
                }
                return schedule_retry(runner, rule, row, &message).await;
            }
            Err(error) => {
                tracing::warn!(error = %error, run_id = row.id, "反查实例映射失败");
                return schedule_retry(runner, rule, row, &format!("反查实例映射失败: {error}"))
                    .await;
            }
        },
    };

    // 4) 去重：评论正文第一行是 `prefix + record_id`。
    let marker = rule.config.marker(&row.record_id);
    let mut comment_id = String::new();
    match list_instance_comments(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &instance_code,
        &rule.config.author_user_id,
    )
    .await
    {
        Ok(comments) => {
            if let Some(existing) = find_marker(&comments, &marker) {
                comment_id = existing.id.clone();
                tracing::info!(
                    run_id = row.id,
                    rule_id = rule.id,
                    comment_id = %comment_id,
                    "评论标记已存在，跳过发送并补写状态列"
                );
            }
        }
        Err(failure) => {
            return handle_outbound_failure(runner, rule, row, failure, "查询实例评论").await;
        }
    }

    // 6) 发评论（未命中标记时）。先把台账置 commenting + 租约，进程崩溃后
    //    超时可被重新认领；重新认领仍会先查标记，不会双发。
    if comment_id.is_empty() {
        let mut lease = Record::new();
        lease.insert("state", serde_json::json!("commenting"));
        lease.insert(
            "next_attempt_at",
            serde_json::json!(now_seconds() + COMMENTING_LEASE_SECONDS),
        );
        if let Err(error) = update_run(runner, row.id, lease).await {
            tracing::warn!(error = %error, run_id = row.id, "写 commenting 租约失败");
        }
        let rendered = render_comment(
            &rule.config,
            &row.record_id,
            &rule.base_token,
            &rule.table_id,
            &fields,
        );
        let fingerprint = payment_fingerprint(&rule.config, &fields);
        match create_instance_comment(
            runner.transport(),
            &runner.tokens,
            &instance_code,
            &rule.config.author_user_id,
            &rendered.text,
            &rendered.files,
        )
        .await
        {
            Ok(created) => {
                comment_id = created.comment_id;
                let mut update = Record::new();
                update.insert("state", serde_json::json!("commented"));
                update.insert("comment_id", serde_json::json!(&comment_id));
                update.insert("instance_code", serde_json::json!(&instance_code));
                update.insert("payment_fingerprint", serde_json::json!(&fingerprint));
                update.insert("attempts", serde_json::json!(row.attempts + 1));
                update.insert("next_attempt_at", serde_json::json!(now_seconds()));
                update.insert("last_error", serde_json::Value::Null);
                if let Err(error) = update_run(runner, row.id, update).await {
                    tracing::warn!(error = %error, run_id = row.id, "评论成功但台账落库失败");
                }
                metrics::counter!("feishu_approval_comment_created_total").increment(1);
            }
            Err(failure) => {
                metrics::counter!("feishu_approval_comment_create_failed_total").increment(1);
                return handle_outbound_failure(runner, rule, row, failure, "创建审批评论").await;
            }
        }
    } else {
        let mut update = Record::new();
        update.insert("state", serde_json::json!("commented"));
        update.insert("comment_id", serde_json::json!(&comment_id));
        update.insert("instance_code", serde_json::json!(&instance_code));
        update.insert("next_attempt_at", serde_json::json!(now_seconds()));
        let _ = update_run(runner, row.id, update).await;
    }

    // 7) 状态列：comment_id 已落库，失败也不会重发评论。
    write_status(runner, rule, row, &comment_id, status_names).await
}

/// 写「审批评论状态」列；失败时保持 commented，仅按退避重试补写。
///
/// 不依赖实例编码：即使回写台账暂时查不到实例，只要评论已发出就能收口状态列。
async fn write_status(
    runner: &RoundRunner,
    rule: &CommentRule,
    row: &RunRow,
    comment_id: &str,
    status_names: &mut HashMap<i64, String>,
) -> StepOutcome {
    let status_field_name = match resolve_status_field_name(runner, rule, status_names).await {
        Ok(name) => name,
        Err(failure) => {
            return handle_outbound_failure(runner, rule, row, failure, "解析评论状态列").await;
        }
    };
    let coordinates = BitableCoordinates {
        app_token: rule.base_token.clone(),
        table_id: rule.table_id.clone(),
        view_id: None,
    };
    let mut fields_to_write = BTreeMap::new();
    fields_to_write.insert(
        status_field_name,
        serde_json::json!(rule.config.status_labels.commented),
    );
    let payload = vec![(row.record_id.clone(), fields_to_write)];
    match bitable::batch_update_records(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &coordinates,
        &payload,
    )
    .await
    {
        Ok(()) => {
            let mut update = Record::new();
            update.insert("state", serde_json::json!("done"));
            update.insert("next_attempt_at", serde_json::Value::Null);
            update.insert("last_error", serde_json::Value::Null);
            let _ = update_run(runner, row.id, update).await;
            metrics::counter!("feishu_approval_comment_done_total").increment(1);
            tracing::info!(
                run_id = row.id,
                rule_id = rule.id,
                record_id = %row.record_id,
                "审批评论回写完成"
            );
            StepOutcome::Done
        }
        Err(failure) => {
            let attempts = row.attempts + 1;
            let mut update = Record::new();
            update.insert("state", serde_json::json!("commented"));
            update.insert("comment_id", serde_json::json!(comment_id));
            update.insert("attempts", serde_json::json!(attempts));
            update.insert(
                "next_attempt_at",
                serde_json::json!(now_seconds() + retry_backoff(attempts)),
            );
            update.insert(
                "last_error",
                serde_json::json!(format!("状态列写入失败: {}", failure.message)),
            );
            let _ = update_run(runner, row.id, update).await;
            tracing::warn!(
                error = %failure.message,
                run_id = row.id,
                "评论已发出，状态列写入失败，下一轮仅补状态"
            );
            StepOutcome::Retry
        }
    }
}

/// 按 `target_record_id` 反查回写台账里的实例编码。
///
/// 同一目标记录可能被多条规则/多代次写过；取最近一条 `written` 的行级记录。
async fn find_instance_code(
    runner: &RoundRunner,
    rule_id: i64,
    record_id: &str,
) -> anyhow::Result<Option<String>> {
    // 只取最近一页：同一目标记录通常只有一条 written 行，按 id 倒序第一页就够；
    // 用 all_pages 会在「规则写过超过 500 行」时误报翻页上限。
    let rows = runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_in("target_record_id", vec![serde_json::json!(record_id)])?
        .where_eq("write_status", serde_json::json!("written"))?
        .where_ne("row_key", serde_json::json!(RULE_ROW_KEY))?
        .select_fields(&["instance_code"])?
        .order_by("id", SortOrder::Desc)?
        .page(1, 10)?
        .all()
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.optional::<String>("instance_code")
                .ok()
                .flatten()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .next())
}

/// 读目标记录并按配置的 field_id 建索引。
///
/// `records/batch_get` 返回的单元格按**当前列名**作键；先按保存时缓存的
/// 列名映射。若配置字段一个都取不到，再拉一次字段清单按 id 重映射——
/// 列改名后无需重存配置即可自愈。
async fn load_record_fields(
    runner: &RoundRunner,
    rule: &CommentRule,
    record_id: &str,
) -> Result<BTreeMap<String, serde_json::Value>, OutboundFailure> {
    let coordinates = BitableCoordinates {
        app_token: rule.base_token.clone(),
        table_id: rule.table_id.clone(),
        view_id: None,
    };
    let records = bitable::get_records_by_ids(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &coordinates,
        &[record_id.to_string()],
    )
    .await?;
    let Some(record) = records.into_iter().next() else {
        return Err(OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: format!("目标记录 {record_id} 不存在（可能已被删除）"),
        });
    };

    let mut cells = map_cells_by_field_id(&rule.config, &record.fields);
    if rule
        .config
        .render_field_ids()
        .iter()
        .any(|field_id| !cells.contains_key(*field_id))
    {
        if let Ok(fields) = bitable::list_all_fields(
            runner.transport(),
            &TokioSleeper,
            &runner.tokens,
            &coordinates,
        )
        .await
        {
            if let Ok(rekeyed) = bitable::rekey_cells_by_field_id(&fields, &record.fields) {
                cells = rekeyed.into_iter().collect();
            }
        }
    }
    Ok(cells)
}

fn map_cells_by_field_id(
    config: &CommentConfig,
    fields: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    let mut cells = BTreeMap::new();
    for field_id in config.render_field_ids() {
        let by_name = config
            .field_names
            .get(field_id)
            .and_then(|name| fields.get(name));
        let value = by_name.cloned().or_else(|| fields.get(field_id).cloned());
        if let Some(value) = value {
            cells.insert(field_id.to_string(), value);
        }
    }
    for field_id in &config.required_field_ids {
        if !cells.contains_key(field_id.as_str()) {
            if let Some(value) = fields.get(field_id) {
                cells.insert(field_id.clone(), value.clone());
            }
        }
    }
    if let Some(value) = config
        .trigger_field_id
        .as_deref()
        .and_then(|field_id| fields.get(field_id))
    {
        if let Some(field_id) = config.trigger_field_id.as_deref() {
            cells.insert(field_id.to_string(), value.clone());
        }
    }
    cells
}

async fn resolve_status_field_name(
    runner: &RoundRunner,
    rule: &CommentRule,
    cache: &mut HashMap<i64, String>,
) -> Result<String, OutboundFailure> {
    if let Some(name) = cache.get(&rule.id) {
        return Ok(name.clone());
    }
    let coordinates = BitableCoordinates {
        app_token: rule.base_token.clone(),
        table_id: rule.table_id.clone(),
        view_id: None,
    };
    let resolved = match bitable::list_all_fields(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => bitable::resolve_field_name(&fields, &rule.config.status_field_id)
            .unwrap_or_else(|_| rule.config.status_field_name.clone()),
        Err(_) => rule.config.status_field_name.clone(),
    };
    if resolved.trim().is_empty() {
        return Err(OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: "审批评论状态列无法解析（配置缺少列名且现场查询失败）".to_string(),
        });
    }
    cache.insert(rule.id, resolved.clone());
    Ok(resolved)
}

/// 失败分流：Fatal/实例不存在 → failed；网络/限流/5xx → retry（上限 5 次）。
async fn handle_outbound_failure(
    runner: &RoundRunner,
    rule: &CommentRule,
    row: &RunRow,
    failure: OutboundFailure,
    stage: &str,
) -> StepOutcome {
    let attempts = row.attempts + 1;
    let fatal = matches!(
        failure.kind,
        FailureKind::Fatal { .. } | FailureKind::InstanceNotFound | FailureKind::UuidConflict
    );
    let message = format!("{stage}失败: {}", failure.message);
    if fatal || attempts >= MAX_COMMENT_ATTEMPTS {
        update_run_failed(runner, row, &message).await;
        mark_status_failed(runner, rule, row, &message).await;
        if fatal {
            tracing::error!(run_id = row.id, rule_id = rule.id, error = %message,
                "审批评论遇到不可重试错误，已置 failed");
        } else {
            tracing::error!(run_id = row.id, rule_id = rule.id, attempts, error = %message,
                "审批评论连续失败达到上限，已置 failed");
        }
        StepOutcome::Failed
    } else {
        schedule_retry(runner, rule, row, &message).await
    }
}

async fn schedule_retry(
    runner: &RoundRunner,
    _rule: &CommentRule,
    row: &RunRow,
    message: &str,
) -> StepOutcome {
    let attempts = row.attempts + 1;
    let mut update = Record::new();
    // 已发评论的行保持 commented（下一轮只补状态）；否则回 retry。
    update.insert(
        "state",
        serde_json::json!(if row.state == "commented" {
            "commented"
        } else {
            "retry"
        }),
    );
    update.insert("attempts", serde_json::json!(attempts));
    update.insert(
        "next_attempt_at",
        serde_json::json!(now_seconds() + retry_backoff(attempts)),
    );
    update.insert("last_error", serde_json::json!(message));
    if let Err(error) = update_run(runner, row.id, update).await {
        tracing::warn!(error = %error, run_id = row.id, "记录重试状态失败");
    }
    StepOutcome::Retry
}

async fn update_run_failed(runner: &RoundRunner, row: &RunRow, message: &str) {
    let mut update = Record::new();
    update.insert("state", serde_json::json!("failed"));
    update.insert("next_attempt_at", serde_json::Value::Null);
    update.insert("attempts", serde_json::json!(row.attempts + 1));
    update.insert("last_error", serde_json::json!(message));
    if let Err(error) = update_run(runner, row.id, update).await {
        tracing::warn!(error = %error, run_id = row.id, "记录 failed 状态失败");
    }
    tracing::error!(run_id = row.id, rule_id = row.rule_id, error = %message,
        "审批评论回写失败");
}

/// 失败补偿：尽力把状态列写成「评论失败」，让该行退出扫描集。
async fn mark_status_failed(runner: &RoundRunner, rule: &CommentRule, row: &RunRow, message: &str) {
    let status_field_name = rule.config.status_field_name.trim();
    if status_field_name.is_empty() {
        return;
    }
    let coordinates = BitableCoordinates {
        app_token: rule.base_token.clone(),
        table_id: rule.table_id.clone(),
        view_id: None,
    };
    let mut fields = BTreeMap::new();
    fields.insert(
        status_field_name.to_string(),
        serde_json::json!(rule.config.status_labels.failed),
    );
    let payload = vec![(row.record_id.clone(), fields)];
    if let Err(failure) = bitable::batch_update_records(
        runner.transport(),
        &TokioSleeper,
        &runner.tokens,
        &coordinates,
        &payload,
    )
    .await
    {
        tracing::warn!(error = %failure.message, run_id = row.id,
            "评论失败状态列写入失败（错误: {message}）");
    }
}

async fn update_run(runner: &RoundRunner, run_id: i64, record: Record) -> anyhow::Result<()> {
    runner
        .context
        .approval_comment_runs()
        .query()
        .where_eq("id", serde_json::json!(run_id))?
        .update(record)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 调度
// ---------------------------------------------------------------------------

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 指数退避：60s 起，封顶 30 分钟。
fn retry_backoff(attempts: i64) -> i64 {
    let exponent = (attempts.max(1) - 1).min(10) as u32;
    RETRY_BASE_SECONDS
        .saturating_mul(1i64 << exponent)
        .min(RETRY_MAX_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CommentConfig {
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "field_ids": ["fldBB53wVj","fldHDHvXIH","fldaeNR1GA"],
            "field_names": {
                "fldBB53wVj":"支付ID",
                "fldHDHvXIH":"支付状态",
                "fldaeNR1GA":"支付凭证"
            },
            "required_field_ids": ["fldBB53wVj","fldaeNR1GA"],
            "trigger_field_id": "fldHDHvXIH",
            "trigger_values": ["支付成功"],
            "status_field_id": "fldYArfJiV",
            "status_field_name": "审批评论状态",
            "status_labels": {"commented":"已评论","failed":"评论失败"},
            "author_user_id": "ou_ops",
            "include_attachments": true,
            "prefix": "【支付回填】",
            "record_url_template": "https://tenant/base/{base_token}?table={table_id}&record={record_id}"
        }))
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn rule(id: i64) -> CommentRule {
        CommentRule {
            id,
            title: format!("规则{id}"),
            base_token: "bascn".to_string(),
            table_id: "tbl".to_string(),
            config: config(),
        }
    }

    #[test]
    fn claim_is_scoped_to_enabled_rules_and_reenabled_rows_wake_again() {
        assert!(
            claim_rule_ids(&[]).is_none(),
            "没有启用规则时不得认领任何台账行（停用规则的行保持原状）"
        );
        assert_eq!(
            claim_rule_ids(&[rule(7), rule(9)]),
            Some(vec![serde_json::json!(7), serde_json::json!(9)]),
            "认领必须按启用规则 id 过滤"
        );
        // 重新启用后，pending/retry（以及 waiting/skipped）行会被 wake_state
        // 重置为 pending 并自然到期认领；failed 是终态，不会被自动重开。
        for state in ["pending", "retry", "waiting", "skipped"] {
            assert_eq!(wake_state(state), Some("pending"), "{state}");
        }
        assert_eq!(wake_state("failed"), None, "failed 只能手工重试");
    }

    #[test]
    fn changed_field_filter_ignores_status_only_edits() {
        let config = config();
        assert!(changed_field_ids_hit(&config, Some(&["fldBB53wVj".into()])));
        assert!(changed_field_ids_hit(&config, Some(&["fldHDHvXIH".into()])));
        assert!(
            changed_field_ids_hit(&config, Some(&["支付ID".into()])),
            "changed_fields 给列名时也要命中"
        );
        assert!(
            !changed_field_ids_hit(&config, Some(&["fldYArfJiV".into()])),
            "写状态列不能触发自己"
        );
        assert!(
            changed_field_ids_hit(&config, None),
            "上游没给变更列表时不能过滤"
        );
        assert!(changed_field_ids_hit(&config, Some(&[])));
    }

    #[test]
    fn changed_fields_are_parsed_as_a_json_string_array() {
        assert_eq!(
            parse_changed_fields(Some(r#"["fldA"," fldB ",""]"#.to_string())),
            Some(vec!["fldA".to_string(), "fldB".to_string()])
        );
        assert_eq!(parse_changed_fields(Some("not json".to_string())), None);
        assert_eq!(parse_changed_fields(None), None);
    }

    #[test]
    fn retry_backoff_grows_then_caps() {
        assert_eq!(retry_backoff(1), RETRY_BASE_SECONDS);
        assert_eq!(retry_backoff(2), RETRY_BASE_SECONDS * 2);
        assert_eq!(retry_backoff(99), RETRY_MAX_SECONDS);
    }

    #[test]
    fn cells_are_mapped_from_cached_column_names_and_raw_ids() {
        let config = config();
        let mut fields = BTreeMap::new();
        fields.insert("支付ID".to_string(), serde_json::json!("PAY-1"));
        fields.insert("fldHDHvXIH".to_string(), serde_json::json!("支付成功"));
        let cells = map_cells_by_field_id(&config, &fields);
        assert_eq!(cells.get("fldBB53wVj"), Some(&serde_json::json!("PAY-1")));
        assert_eq!(
            cells.get("fldHDHvXIH"),
            Some(&serde_json::json!("支付成功"))
        );
    }

    #[test]
    fn every_declared_state_is_known_to_the_table_contract() {
        let states = crate::addon::feishu::approval::domain::comment_run_table::COMMENT_STATES;
        for state in [
            "pending",
            "commenting",
            "commented",
            "done",
            "retry",
            "failed",
            "skipped",
            "waiting",
        ] {
            assert!(states.contains(&state), "{state}");
        }
    }
}
