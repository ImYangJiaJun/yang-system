//! 审批 → 多维表格回写的后台 worker。
//!
//! 骨架与 [`super::feishu_event_worker::FeishuEventWorker`] 一致：`watch` 通道 +
//! `JoinHandle`，`shutdown()` 发信号后等待任务退出；`Drop` 也发一次，避免忘记
//! 关闭时任务悬挂。
//!
//! # 四个阶段
//!
//! 1. **ingest**：认领 `feishu_approval_event` 中 `projection_processed_at IS NULL`
//!    且 `id > 水位线` 的事件，按报文里的 `approval_code` 命中启用规则，upsert
//!    规则级 run 行；
//! 2. **seed**：启动时回补 7 天；之后每 10 分钟按 `approval_code` 扫最近 30 小时
//!    的实例列表（单窗 ≤ 10 小时）。只处理 `effective_from` 之后的实例，不做
//!    历史补录；
//! 3. **process**：认领到期的规则级 run 行，按实例分组只拉一次详情；用领域引擎
//!    （`approval_export`）判触发/条件/映射，逐明细行调 `create_record`；
//! 4. **mark**：对已写行按最新实例状态维护目标状态列（有效/已退回/已拒绝/
//!    已撤回/已撤销/已删除），文案相同则跳过目标表写入。
//!
//! # 幂等与重试
//!
//! - 目标记录的 `client_token` 由 `(规则, 实例, 行键, 代次)` 确定性派生；同一次
//!   尝试重放不会建重复行。目标行被手工删除后，由 `export_force_recreate` 把台账
//!   推进到 `generation + 1` 并用新 token 重建（worker 只消费该代次）。
//! - 一个规则可以配置多个触发节点：先通过的节点负责建立行级台账；后续节点通过
//!   时复用已有台账（不按 trigger_node_id 重复建行），否则会撞 `client_token`
//!   唯一键并陷入无意义重试。
//! - 失败按 [`ExportError`] / [`FailureKind`] 分流：可重试退避，Fatal 置终态，
//!   Review 等人工核对。单个事件/实例失败不阻塞同轮其他事件。
//! - 部署形态是单实例（与派发 worker 同一条前提）；`lease_until` 只用于防止
//!   误起的第二个副本重复处理，正确性最终由幂等 Token 与状态机保证。
//!
//! # 日志纪律
//!
//! 只记 id / 计数 / 状态，**不记审批表单值**。错误文案来自领域层，可能包含控件
//! id 与配置坐标，但不含用户填写的内容。

use std::collections::{BTreeMap, HashSet};

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::sync::watch;
use yang_base::table::{Record, SortOrder, WhereCondition};
use yang_base::tools::Tools;
use yang_base::BaseError;
use yang_runtime::shutdown::OwnedTask;

use crate::addon::feishu::domain::approval::{self, GetOutcome, InstanceDetail};
use crate::addon::feishu::domain::approval_export::marking::should_apply;
use crate::addon::feishu::domain::approval_export::{
    build_rows, client_token, compute_node_passed, desired_mark, parse_option_map, Condition,
    Converter, ExportError, ExportFieldMap, ExportRule, InstanceForm, MarkOutcome, MarkStatus,
    RowFilter, SourceKind, StatusLabels, TargetRow, TriggerMode, TriggerOutcome, RULE_ROW_KEY,
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

/// 每轮认领多少条待投影事件。
const EVENT_BATCH: usize = 50;

/// 每轮认领多少条到期规则行。
const CLAIM_BATCH: usize = 20;

/// 编译期守护：批量不超过框架分页硬上限（超了是拒绝，不是截断）。
const _: () = assert!(EVENT_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);
const _: () = assert!(CLAIM_BATCH <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 规则行租约（秒）：处理一轮的上界，到期可被重新认领。
const LEASE_SECONDS: i64 = 120;

/// 单条规则行最多尝试多少次后置终态。
const MAX_ATTEMPTS: i64 = 10;

/// 事件投影最多尝试多少次后置终态（不阻塞后续事件）。
const PROJECTION_MAX_ATTEMPTS: i64 = 10;

/// 可重试失败的基础退避（秒），指数增长到上限。
const RETRY_BASE_SECONDS: i64 = 60;
const RETRY_MAX_SECONDS: i64 = 1800;

/// 等待节点/最终通过时的复核间隔（秒）：5 分钟起，翻倍，封顶 30 分钟。
const WAIT_BASE_SECONDS: i64 = 300;
const WAIT_MAX_SECONDS: i64 = 1800;

/// 已写行在实例未到终态时的复核间隔（秒）。
const WRITTEN_RECHECK_SECONDS: i64 = 1800;

/// 启动回补窗口：最近 7 天。
const SEED_BACKFILL_MS: i64 = 7 * 24 * 3600 * 1000;

/// 常驻补扫窗口：最近 30 小时（3 个 ≤10 小时的窗口）。
const SEED_RECENT_MS: i64 = 30 * 3600 * 1000;

/// 单次实例列表查询的时间窗上限：飞书要求 ≤ 10 小时。
const WINDOW_MAX_MS: i64 = 10 * 3600 * 1000;

/// 实例列表补扫周期。
const SEED_INTERVAL: Duration = Duration::from_secs(600);

/// 实例列表每页条数。
const INSTANCE_PAGE_SIZE: u32 = 100;

/// 规则/映射是全量小表，翻页上限只防「筛选没生效」。
const CONFIG_MAX_PAGES: usize = 10;

/// 单个实例待标记的行数上限（20 页 × 100 行）：付款明细通常几十行，
/// 超过这个量级说明配置或数据异常，宁可报错也不要静默漏标。
const MARKABLE_MAX_PAGES: usize = 20;

/// 规则级 run 行读取的列。
const RULE_ROW_FIELDS: &[&str] = &[
    "id",
    "rule_id",
    "approval_code",
    "instance_code",
    "trigger_node_id",
    "row_key",
    "generation",
    "write_status",
    "mark_status",
    "applied_label",
    "client_token",
    "payload_hash",
    "target_record_id",
    "attempts",
    "next_attempt_at",
    "lease_until",
    "last_error",
];

/// 规则表的读取列。
const RULE_FIELDS: &[&str] = &[
    "id",
    "title",
    "approval_code",
    "trigger_mode",
    "trigger_node_ids",
    "require_final_approval",
    "detail_widget_id",
    "row_filter",
    "condition_json",
    "target_base_token",
    "target_table_id",
    "status_field",
    "status_field_name",
    "status_labels_json",
    "mark_enabled",
    "enabled",
    "effective_from",
];

/// 映射表的读取列。
const FIELD_MAP_FIELDS: &[&str] = &[
    "rule_id",
    "source_kind",
    "source_widget_id",
    "source_system_key",
    "constant_value",
    "target_field_id",
    "converter",
    "option_map",
    "required",
];

/// worker 心跳/认领用的处理者标识。
const WORKER_ID: &str = "feishu-approval-export";

/// 审批回写 worker。
pub(crate) struct FeishuApprovalExportWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl FeishuApprovalExportWorker {
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

        let (shutdown, receiver) = watch::channel(false);
        let runner = RoundRunner {
            context,
            tokens,
            transport,
            sleeper,
        };
        let idle_poll = Duration::from_secs(settings.approval_scan_interval_seconds);
        let task = tokio::spawn(run_loop(runner, idle_poll, receiver));

        tracing::info!(
            idle_poll_seconds = settings.approval_scan_interval_seconds,
            "飞书审批回写 Worker 已启动"
        );
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
        })
    }

    /// 主循环是否已经结束（正常退出或 panic）。用于进程级监督：
    /// 没有这一位时，worker panic 只会留下一个已完成的 JoinHandle，
    /// readiness 与进程都继续绿，回写会静默停摆。
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
            .map_err(|error| anyhow::anyhow!("等待审批回写 Worker 退出失败: {error}"))?;
        Ok(())
    }
}

impl Drop for FeishuApprovalExportWorker {
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
    context: Arc<FeishuContext>,
    tokens: Arc<TenantTokenProvider>,
    transport: Arc<dyn OutboundTransport>,
    sleeper: Arc<TokioSleeper>,
}

impl RoundRunner {
    fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }
}

/// 跨轮次的进程内状态：事件游标与补扫节流。
///
/// 单实例 + 单进程的前提下，事件水位线放内存即可：重启后从 0 开始重扫一遍
/// 未投影事件，恰好就是恢复该恢复的。放库里要为它单独建单行表，收益为负。
#[derive(Debug, Default)]
struct RunnerState {
    /// 已扫描到的事件 id；0 = 从头开始。
    watermark: i64,
    /// 上一轮补扫时间；`None` = 还没跑过启动回补。
    last_seed_at: Option<Instant>,
    startup_seed_done: bool,
}

/// 主循环：启动即订阅一次启用规则的审批定义，然后等信号或到点跑一轮。
async fn run_loop(runner: RoundRunner, idle_poll: Duration, mut shutdown: watch::Receiver<bool>) {
    // 订阅是运行时状态，控制台重配、发布新版本都可能把它清掉——启动时重订一遍，
    // 失败只 warn，不挡运行（幂等，1390007 已订阅视为成功）。
    let subscribe = async {
        if let Err(error) = subscribe_enabled(&runner).await {
            tracing::warn!(error = %error, "启动期订阅审批定义失败（下次启动会重试）");
        }
    };
    if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), subscribe).await {
        tracing::info!("飞书审批回写 Worker 收到关闭信号（启动订阅阶段）");
        return;
    }

    let mut state = RunnerState::default();
    let mut ticker = tokio::time::interval(idle_poll);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("飞书审批回写 Worker 收到关闭信号");
                    return;
                }
            }
            _ = ticker.tick() => {
                let round = run_round(&runner, &mut state);
                if !super::worker_shutdown::run_round_or_shutdown(shutdown.clone(), round).await {
                    tracing::info!("飞书审批回写 Worker 收到关闭信号，已取消进行中的轮次");
                    return;
                }
            }
        }
    }
}

/// 订阅启用配置 + 启用回写规则涉及的审批定义 Code。
///
/// 与事件 worker 的订阅是两张独立清单；同一 Code 被两边同时订阅是幂等的。
async fn subscribe_enabled(runner: &RoundRunner) -> anyhow::Result<()> {
    let mut codes: HashSet<String> = HashSet::new();
    for (repository, label) in [
        (runner.context.approval_configs(), "派发配置"),
        (runner.context.approval_export_rules(), "回写规则"),
    ] {
        let rows = crate::addon::feishu::domain::repository::all_pages(
            repository
                .query()
                .where_eq("enabled", serde_json::json!(true))?
                .select_fields(&["id", "approval_code"])?,
            CONFIG_MAX_PAGES,
        )
        .await?;
        for row in rows {
            let code = row.optional::<String>("approval_code")?.unwrap_or_default();
            let code = code.trim().to_string();
            if !code.is_empty() {
                codes.insert(code);
            }
        }
        tracing::debug!(source = label, total = codes.len(), "订阅清单已合并");
    }

    for code in codes {
        match approval::subscribe_approval(
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

/// 跑一轮：ingest → seed → process，并输出一条心跳日志。
#[tracing::instrument(name = "approval_export_round", skip_all)]
async fn run_round(runner: &RoundRunner, state: &mut RunnerState) {
    let started = Instant::now();
    let rules = match load_enabled_rules(runner).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::error!(error = %error, "读取审批回写规则失败");
            return;
        }
    };

    let ingest = ingest_events(runner, &rules, state).await;
    let seed = maybe_seed(runner, &rules, state).await;
    let process = process_due(runner, &rules).await;

    tracing::info!(
        enabled_rules = rules.len(),
        ingested = ingest.ingested,
        ingest_failed = ingest.failed,
        seed_scanned = seed.scans,
        seed_instances = seed.instances,
        seed_failed = seed.failed,
        claimed = process.claimed,
        written = process.written,
        marked = process.marked,
        retry = process.retry,
        review = process.review,
        terminal = process.terminal,
        skipped = process.skipped,
        duration_ms = started.elapsed().as_millis() as u64,
        "审批回写轮次结束"
    );
}

// ---------------------------------------------------------------------------
// 规则加载
// ---------------------------------------------------------------------------

/// 读出全部启用规则；单条规则解析失败只跳过该条并留 ERROR，不拖垮整轮。
async fn load_enabled_rules(runner: &RoundRunner) -> anyhow::Result<Vec<ExportRule>> {
    let rows = crate::addon::feishu::domain::repository::all_pages(
        runner
            .context
            .approval_export_rules()
            .query()
            .where_eq("enabled", serde_json::json!(true))?
            .select_fields(RULE_FIELDS)?,
        CONFIG_MAX_PAGES,
    )
    .await?;

    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        match parse_rule(&row) {
            Ok(rule) => rules.push(rule),
            Err(error) => {
                tracing::error!(error = %error, "审批回写规则无法解析，已跳过");
            }
        }
    }
    // 查询已经按 enabled 过滤；再读一次字段，防止规则在查询与解析之间被停用。
    rules.retain(|rule| rule.enabled);
    Ok(rules)
}

fn parse_rule(row: &Record) -> Result<ExportRule, String> {
    let id: i64 = row.require("id").map_err(|error| format!("id: {error}"))?;
    let title: String = row
        .optional("title")
        .map_err(|error| format!("title: {error}"))?
        .unwrap_or_default();
    let approval_code: String = row
        .require("approval_code")
        .map_err(|error| format!("approval_code: {error}"))?;
    let trigger_mode_raw: String = row
        .optional("trigger_mode")
        .map_err(|error| format!("trigger_mode: {error}"))?
        .unwrap_or_else(|| "node_passed".to_string());
    let trigger_mode = TriggerMode::parse(&trigger_mode_raw).map_err(|error| error.to_string())?;
    let node_ids_raw: Option<String> = row
        .optional("trigger_node_ids")
        .map_err(|error| format!("trigger_node_ids: {error}"))?;
    let trigger_node_ids = parse_node_ids(node_ids_raw.as_deref())?;
    let require_final_approval = row
        .optional::<bool>("require_final_approval")
        .map_err(|error| format!("require_final_approval: {error}"))?
        .unwrap_or(true);
    let detail_widget_id: String = row
        .require("detail_widget_id")
        .map_err(|error| format!("detail_widget_id: {error}"))?;
    let row_filter_raw: String = row
        .optional("row_filter")
        .map_err(|error| format!("row_filter: {error}"))?
        .unwrap_or_else(|| "matched_rows".to_string());
    let row_filter = RowFilter::parse(&row_filter_raw).map_err(|error| error.to_string())?;
    let condition_raw: Option<String> = row
        .optional("condition_json")
        .map_err(|error| format!("condition_json: {error}"))?;
    let condition = Condition::parse(condition_raw.as_deref().unwrap_or(""))
        .map_err(|error| error.to_string())?;
    let target_base_token: String = row
        .require("target_base_token")
        .map_err(|error| format!("target_base_token: {error}"))?;
    let target_table_id: String = row
        .require("target_table_id")
        .map_err(|error| format!("target_table_id: {error}"))?;
    let status_field = optional_nonempty(row, "status_field");
    let status_field_name = optional_nonempty(row, "status_field_name");
    let labels_raw: Option<String> = row
        .optional("status_labels_json")
        .map_err(|error| format!("status_labels_json: {error}"))?;
    let status_labels = StatusLabels::parse(labels_raw.as_deref())
        .map_err(|error| format!("status_labels_json: {error}"))?;
    let mark_enabled = row
        .optional::<bool>("mark_enabled")
        .map_err(|error| format!("mark_enabled: {error}"))?
        .unwrap_or(true);
    let enabled = row
        .optional::<bool>("enabled")
        .map_err(|error| format!("enabled: {error}"))?
        .unwrap_or(true);
    let effective_from = row
        .optional::<i64>("effective_from")
        .map_err(|error| format!("effective_from: {error}"))?;

    Ok(ExportRule {
        id,
        title,
        approval_code,
        trigger_mode,
        trigger_node_ids,
        require_final_approval,
        detail_widget_id,
        row_filter,
        condition,
        target_base_token,
        target_table_id,
        status_field,
        status_field_name,
        status_labels,
        mark_enabled,
        enabled,
        effective_from,
    })
}

fn parse_node_ids(raw: Option<&str>) -> Result<Vec<String>, String> {
    let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(Vec::new());
    };
    let values: Vec<String> = serde_json::from_str(raw)
        .map_err(|error| format!("trigger_node_ids 不是字符串数组: {error}"))?;
    Ok(values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect())
}

fn optional_nonempty(row: &Record, field: &str) -> Option<String> {
    row.optional::<String>(field)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 读出规则的字段映射；配置错误与数据库错误分开分类。
async fn load_field_maps(
    runner: &RoundRunner,
    rule_id: i64,
) -> Result<Vec<ExportFieldMap>, ExportError> {
    let query = runner
        .context
        .approval_export_field_maps()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))
        .map_err(|error| ExportError::Retryable(format!("构造字段映射查询失败: {error}")))?
        .select_fields(FIELD_MAP_FIELDS)
        .map_err(|error| ExportError::Retryable(format!("投影字段映射失败: {error}")))?;
    // `.all()` 默认只取一页（10 行）；映射多于 10 条时会被静默截断。
    let rows = crate::addon::feishu::domain::repository::all_pages(query, CONFIG_MAX_PAGES)
        .await
        .map_err(|error| ExportError::Retryable(format!("读取字段映射失败: {error}")))?;

    let mut maps = Vec::with_capacity(rows.len());
    for row in rows {
        maps.push(parse_field_map(&row)?);
    }
    Ok(maps)
}

fn parse_field_map(row: &Record) -> Result<ExportFieldMap, ExportError> {
    let fatal = |field: &str, error: BaseError| {
        ExportError::Fatal(format!("字段映射 {field} 读取失败: {error}"))
    };
    let source_kind_raw: String = row
        .require("source_kind")
        .map_err(|error| fatal("source_kind", error))?;
    let source_kind = SourceKind::parse(&source_kind_raw)?;
    let source_widget_id = row
        .optional::<String>("source_widget_id")
        .map_err(|error| fatal("source_widget_id", error))?
        .unwrap_or_default();
    let source_system_key = row
        .optional::<String>("source_system_key")
        .map_err(|error| fatal("source_system_key", error))?
        .unwrap_or_default();
    let constant_value = row
        .optional::<String>("constant_value")
        .map_err(|error| fatal("constant_value", error))?;
    let target_field_id: String = row
        .require("target_field_id")
        .map_err(|error| fatal("target_field_id", error))?;
    let converter_raw: String = row
        .optional("converter")
        .map_err(|error| fatal("converter", error))?
        .unwrap_or_else(|| "text".to_string());
    let converter = Converter::parse(&converter_raw)?;
    let option_map_raw = row
        .optional::<String>("option_map")
        .map_err(|error| fatal("option_map", error))?;
    let option_map = parse_option_map(option_map_raw.as_deref())?;
    let required = row
        .optional::<bool>("required")
        .map_err(|error| fatal("required", error))?
        .unwrap_or(false);

    Ok(ExportFieldMap {
        source_kind,
        source_widget_id,
        source_system_key,
        constant_value,
        target_field_id,
        converter,
        option_map,
        required,
    })
}

// ---------------------------------------------------------------------------
// ingest：事件投影
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct IngestReport {
    ingested: usize,
    failed: usize,
}

/// 事件报文里回写需要的身份字段。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EventIdentity {
    approval_code: String,
    instance_code: String,
    /// 事件发生时间（秒）；用于 `effective_from` 的粗筛，详情里还会按实例创建时间复核。
    operate_seconds: Option<i64>,
}

/// 从事件原始报文取 `approval_code` / `instance_code`。
///
/// 业务字段在 `event` 嵌套层里；`operate_time` 官方是毫秒，也接受字符串形态。
/// 缺任一编码就不是回写关心的事件（例如三方审批或半截报文）。
fn parse_event_identity(raw: &str) -> Option<EventIdentity> {
    let body: serde_json::Value = serde_json::from_str(raw).ok()?;
    let event = body.get("event")?;
    let instance_code = event
        .get("instance_code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let approval_code = event
        .get("approval_code")
        .or_else(|| body.get("approval_code"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if instance_code.is_empty() || approval_code.is_empty() {
        return None;
    }
    let operate_ms = event.get("operate_time").and_then(|value| match value {
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        serde_json::Value::Number(number) => number.as_i64(),
        _ => None,
    });
    // 官方是毫秒，但也见过秒级时间戳的旧报文。按量级判定；无条件除以 1000
    // 会把秒级值退回 1970，`effective_from` 就会把该事件整条筛掉。
    let operate_seconds = operate_ms.map(|value| {
        if value >= 100_000_000_000 {
            value / 1000
        } else {
            value
        }
    });
    Some(EventIdentity {
        approval_code,
        instance_code,
        operate_seconds,
    })
}

fn rule_allows_event(rule: &ExportRule, operate_seconds: Option<i64>) -> bool {
    rule.effective_from
        .map_or(true, |floor| operate_seconds.map_or(true, |at| at >= floor))
}

/// 认领未投影事件并在本进程内推进水位线。
async fn ingest_events(
    runner: &RoundRunner,
    rules: &[ExportRule],
    state: &mut RunnerState,
) -> IngestReport {
    let rows = match claim_pending_events(runner, state.watermark).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(error = %error, "事件投影认领失败");
            return IngestReport {
                failed: 1,
                ..IngestReport::default()
            };
        }
    };
    if rows.is_empty() {
        // 一轮扫空后回到起点，避免新事件 id 小于旧水位线时永远看不到。
        state.watermark = 0;
        return IngestReport::default();
    }

    let mut report = IngestReport::default();
    let mut max_id = state.watermark;
    for row in rows {
        let event_id: i64 = match row.require("id") {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(error = %error, "事件投影行缺 id");
                report.failed += 1;
                continue;
            }
        };
        max_id = max_id.max(event_id);
        match project_event(runner, rules, &row).await {
            Ok(()) => report.ingested += 1,
            Err(message) => {
                report.failed += 1;
                let attempts = row
                    .optional::<i64>("projection_attempts")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                if let Err(error) =
                    mark_projection_failure(runner, event_id, attempts + 1, &message).await
                {
                    tracing::warn!(error = %error, event_id, "更新事件投影失败信息失败");
                }
                if attempts + 1 >= PROJECTION_MAX_ATTEMPTS {
                    metrics::counter!("feishu_approval_export_event_terminal_total").increment(1);
                    tracing::error!(
                        event_id,
                        attempts = attempts + 1,
                        error = %message,
                        "事件投影连续失败达到上限，已置终态并从回写队列移除"
                    );
                } else {
                    tracing::warn!(event_id, error = %message, "事件投影失败，下轮重试");
                }
            }
        }
    }
    state.watermark = max_id;
    report
}

async fn claim_pending_events(runner: &RoundRunner, watermark: i64) -> anyhow::Result<Vec<Record>> {
    let mut query = runner
        .context
        .approval_events()
        .query()
        .where_null("projection_processed_at")?
        .order_by("id", SortOrder::Asc)?;
    if watermark > 0 {
        query = query.where_gt("id", serde_json::json!(watermark))?;
    }
    Ok(query
        .page(1, EVENT_BATCH)?
        .select_fields(&["id", "instance_code", "raw_payload", "projection_attempts"])?
        .all()
        .await?)
}

async fn project_event(
    runner: &RoundRunner,
    rules: &[ExportRule],
    row: &Record,
) -> Result<(), String> {
    let raw = row
        .optional::<String>("raw_payload")
        .map_err(|error| format!("读取事件报文失败: {error}"))?
        .unwrap_or_default();
    let identity = parse_event_identity(&raw)
        .ok_or_else(|| "事件报文缺少 approval_code/instance_code".to_string())?;

    for rule in rules
        .iter()
        .filter(|rule| rule.approval_code == identity.approval_code)
        .filter(|rule| rule_allows_event(rule, identity.operate_seconds))
    {
        upsert_rule_run(runner, rule, &identity.instance_code, true)
            .await
            .map_err(|error| format!("写入规则级运行行失败: {error}"))?;
    }

    mark_projection_done(
        runner,
        row.require("id").map_err(|error| error.to_string())?,
    )
    .await
}

async fn mark_projection_done(runner: &RoundRunner, event_id: i64) -> Result<(), String> {
    let mut record = Record::new();
    record.insert("projection_processed_at", serde_json::json!(now_seconds()));
    record.insert("projection_attempts", serde_json::json!(0));
    record.insert("projection_last_error", serde_json::Value::Null);
    runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))
        .map_err(|error| format!("构造事件投影标记失败: {error}"))?
        .update(record)
        .await
        .map_err(|error| format!("标记事件投影完成失败: {error}"))?;
    Ok(())
}

async fn mark_projection_failure(
    runner: &RoundRunner,
    event_id: i64,
    attempts: i64,
    message: &str,
) -> anyhow::Result<()> {
    let mut record = Record::new();
    record.insert("projection_attempts", serde_json::json!(attempts));
    record.insert("projection_last_error", serde_json::json!(message));
    if attempts >= PROJECTION_MAX_ATTEMPTS {
        // 终态事件不再阻塞队列；人工可重置 projection_processed_at 重放。
        record.insert("projection_processed_at", serde_json::json!(now_seconds()));
    }
    runner
        .context
        .approval_events()
        .query()
        .where_eq("id", serde_json::json!(event_id))?
        .update(record)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// seed：实例列表补扫
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct SeedReport {
    scans: usize,
    instances: usize,
    failed: usize,
}

/// 按节流窗口决定是否补扫：启动回补 7 天，之后每 10 分钟扫最近 30 小时。
async fn maybe_seed(
    runner: &RoundRunner,
    rules: &[ExportRule],
    state: &mut RunnerState,
) -> SeedReport {
    let startup = !state.startup_seed_done;
    if !startup {
        if let Some(last) = state.last_seed_at {
            if last.elapsed() < SEED_INTERVAL {
                return SeedReport::default();
            }
        }
    }
    state.startup_seed_done = true;
    state.last_seed_at = Some(Instant::now());
    seed_rules(runner, rules, startup).await
}

async fn seed_rules(runner: &RoundRunner, rules: &[ExportRule], startup: bool) -> SeedReport {
    let now_ms = now_seconds() * 1000;
    let horizon = if startup {
        SEED_BACKFILL_MS
    } else {
        SEED_RECENT_MS
    };

    // 同一 approval_code 只扫一次，再 fan-out 到各规则。
    let mut by_code: BTreeMap<&str, Vec<&ExportRule>> = BTreeMap::new();
    for rule in rules {
        by_code
            .entry(rule.approval_code.as_str())
            .or_default()
            .push(rule);
    }

    let mut report = SeedReport::default();
    for (approval_code, code_rules) in by_code {
        let floor_ms = code_rules
            .iter()
            .filter_map(|rule| rule.effective_from)
            .min()
            .map(|seconds| seconds.saturating_mul(1000));
        let start_ms = (now_ms - horizon).max(floor_ms.unwrap_or(i64::MIN));
        if start_ms >= now_ms {
            continue;
        }

        for (window_start, window_end) in split_windows(start_ms, now_ms) {
            report.scans += 1;
            match approval::list_instance_codes(
                runner.transport(),
                runner.sleeper.as_ref(),
                &runner.tokens,
                approval_code,
                window_start,
                window_end,
                INSTANCE_PAGE_SIZE,
            )
            .await
            {
                Ok(codes) => {
                    report.instances += codes.len();
                    for instance_code in codes {
                        for rule in &code_rules {
                            if let Err(error) =
                                upsert_rule_run(runner, rule, &instance_code, false).await
                            {
                                report.failed += 1;
                                tracing::warn!(
                                    error = %error,
                                    rule_id = rule.id,
                                    instance_code = %instance_code,
                                    "补扫写入规则级运行行失败"
                                );
                            }
                        }
                    }
                }
                Err(failure) => {
                    report.failed += 1;
                    tracing::warn!(
                        error = %failure.message,
                        approval_code = %approval_code,
                        window_start,
                        window_end,
                        "补扫审批实例列表失败"
                    );
                }
            }
        }
    }
    report
}

/// 把 `[start_ms, end_ms)` 切成不超过 10 小时的窗口。
fn split_windows(start_ms: i64, end_ms: i64) -> Vec<(i64, i64)> {
    let mut windows = Vec::new();
    let mut cursor = start_ms;
    while cursor < end_ms {
        let end = (cursor + WINDOW_MAX_MS).min(end_ms);
        windows.push((cursor, end));
        cursor = end;
    }
    windows
}

// ---------------------------------------------------------------------------
// 规则级 run 行
// ---------------------------------------------------------------------------

/// 新建（或唤醒）一条 `(规则, 实例)` 的规则级 run 行。
///
/// `wake_existing`：事件驱动时为 `true`——把可恢复状态行的 `next_attempt_at`
/// 拉到当前时间；补扫时为 `false`，只补缺失行，不打扰已排期的行。
async fn upsert_rule_run(
    runner: &RoundRunner,
    rule: &ExportRule,
    instance_code: &str,
    wake_existing: bool,
) -> anyhow::Result<()> {
    let now = now_seconds();
    let existing = runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule.id))?
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .where_eq("trigger_node_id", serde_json::json!(RULE_ROW_KEY))?
        .where_eq("row_key", serde_json::json!(RULE_ROW_KEY))?
        .optional()
        .await?;

    if let Some(existing) = existing {
        if !wake_existing {
            return Ok(());
        }
        let status = existing
            .optional::<String>("write_status")?
            .unwrap_or_default();
        // review / terminal / skipped 是人工或终态，事件不擅自重开。
        let Some(next_status) = wake_transition(&status) else {
            return Ok(());
        };
        let mut record = Record::new();
        record.insert("next_attempt_at", serde_json::json!(now));
        record.insert("lease_until", serde_json::Value::Null);
        if let Some(next_status) = next_status {
            record.insert("write_status", serde_json::json!(next_status));
        }
        runner
            .context
            .approval_export_runs()
            .query()
            .where_eq("id", serde_json::json!(existing.require::<i64>("id")?))?
            .update(record)
            .await?;
        return Ok(());
    }

    let mut record = Record::new();
    record.insert("rule_id", serde_json::json!(rule.id));
    record.insert("approval_code", serde_json::json!(rule.approval_code));
    record.insert("instance_code", serde_json::json!(instance_code));
    record.insert("trigger_node_id", serde_json::json!(RULE_ROW_KEY));
    record.insert("row_key", serde_json::json!(RULE_ROW_KEY));
    record.insert("generation", serde_json::json!(0));
    record.insert("write_status", serde_json::json!("queued"));
    record.insert("mark_status", serde_json::json!("none"));
    record.insert("attempts", serde_json::json!(0));
    record.insert("next_attempt_at", serde_json::json!(now));
    let inserted = runner
        .context
        .approval_export_runs()
        .query()
        .insert(record)
        .await;
    if let Err(error) = inserted {
        // 并发 upsert 撞唯一键：另一条路径已插入，退化成一次唤醒更新。
        let mut record = Record::new();
        record.insert("next_attempt_at", serde_json::json!(now));
        let affected = runner
            .context
            .approval_export_runs()
            .query()
            .where_eq("rule_id", serde_json::json!(rule.id))?
            .where_eq("instance_code", serde_json::json!(instance_code))?
            .where_eq("trigger_node_id", serde_json::json!(RULE_ROW_KEY))?
            .where_eq("row_key", serde_json::json!(RULE_ROW_KEY))?
            .update(record)
            .await?;
        if affected == 0 {
            return Err(error.into());
        }
    }
    Ok(())
}

/// 事件唤醒时的状态迁移。返回 `Some(None)` 表示状态不变，只拉近复核时间；
/// `Some(Some(status))` 表示需要重置状态；`None` 表示不唤醒。
fn wake_transition(status: &str) -> Option<Option<&'static str>> {
    match status.trim() {
        "queued" | "waiting_node" | "waiting_final" | "retry" | "written" => Some(None),
        "unmatched" => Some(Some("queued")),
        // partial 里可能有可重试的行；重开一次让写流程跳过已写行、重试失败行。
        "partial" => Some(Some("retry")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// process：认领到期规则行
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct ProcessReport {
    claimed: usize,
    written: usize,
    marked: usize,
    retry: usize,
    review: usize,
    terminal: usize,
    skipped: usize,
}

#[derive(Debug, Clone)]
struct RunRow {
    id: i64,
    rule_id: i64,
    instance_code: String,
    write_status: String,
    attempts: i64,
}

async fn process_due(runner: &RoundRunner, rules: &[ExportRule]) -> ProcessReport {
    let rows = match claim_due_rules(runner).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(error = %error, "审批回写到期队列认领失败");
            return ProcessReport::default();
        }
    };
    if rows.is_empty() {
        return ProcessReport::default();
    }

    let rules_by_id: BTreeMap<i64, &ExportRule> =
        rules.iter().map(|rule| (rule.id, rule)).collect();
    let mut report = ProcessReport {
        claimed: rows.len(),
        ..ProcessReport::default()
    };

    // 按实例分组：同一实例的多条规则共用一次详情查询。
    let mut by_instance: BTreeMap<String, Vec<RunRow>> = BTreeMap::new();
    for row in &rows {
        by_instance
            .entry(row.instance_code.clone())
            .or_default()
            .push(row.clone());
    }

    for (instance_code, instance_rows) in by_instance {
        let mut leased = Vec::with_capacity(instance_rows.len());
        for row in &instance_rows {
            if let Err(error) = lease_run_row(runner, row.id).await {
                tracing::warn!(error = %error, run_id = row.id, "规则行租约写入失败");
            }
            leased.push(row.clone());
        }

        let outcome = approval::get_instance(
            runner.transport(),
            runner.sleeper.as_ref(),
            &runner.tokens,
            &instance_code,
        )
        .await;

        let detail = match outcome {
            Ok(GetOutcome::Found(detail)) => *detail,
            Ok(GetOutcome::NotFound) => {
                for row in &leased {
                    schedule_retry(runner, row, "审批实例不存在", &mut report).await;
                }
                continue;
            }
            Err(failure) => {
                let message = format!("读取审批实例详情失败: {}", failure.message);
                if matches!(failure.kind, FailureKind::Fatal { .. }) {
                    for row in &leased {
                        finalize_row(
                            runner,
                            row,
                            "terminal",
                            None,
                            Some(&message),
                            Some(row.attempts + 1),
                        )
                        .await;
                        report.terminal += 1;
                    }
                } else {
                    for row in &leased {
                        schedule_retry(runner, row, &message, &mut report).await;
                    }
                }
                continue;
            }
        };

        for row in &leased {
            let Some(rule) = rules_by_id.get(&row.rule_id) else {
                finalize_row(runner, row, "skipped", None, Some("规则已停用或删除"), None).await;
                report.skipped += 1;
                continue;
            };
            process_run_row(runner, rule, row, &detail, &mut report).await;
        }
    }

    report
}

async fn claim_due_rules(runner: &RoundRunner) -> anyhow::Result<Vec<RunRow>> {
    let now = now_seconds();
    let rows = runner
        .context
        .approval_export_runs()
        .query()
        .where_in(
            "write_status",
            vec![
                serde_json::json!("queued"),
                serde_json::json!("waiting_node"),
                serde_json::json!("waiting_final"),
                serde_json::json!("retry"),
                serde_json::json!("written"),
                // 部分行 review/terminal、部分行已写的规则行仍有复核时间；
                // 漏掉它会让这类实例永远不再标记（例如后来被拒绝）。
                serde_json::json!("partial"),
            ],
        )?
        .where_eq("row_key", serde_json::json!(RULE_ROW_KEY))?
        .where_lte("next_attempt_at", serde_json::json!(now))?
        .where_or(vec![
            WhereCondition::IsNull {
                field: "lease_until".to_string(),
            },
            WhereCondition::Lte {
                field: "lease_until".to_string(),
                value: serde_json::json!(now),
            },
        ])?
        .order_by("next_attempt_at", SortOrder::Asc)?
        .page(1, CLAIM_BATCH)?
        .select_fields(RULE_ROW_FIELDS)?
        .all()
        .await?;

    let mut claimed = Vec::with_capacity(rows.len());
    for row in rows {
        claimed.push(RunRow {
            id: row.require("id")?,
            rule_id: row.require("rule_id")?,
            instance_code: row.require("instance_code")?,
            write_status: row
                .optional::<String>("write_status")?
                .unwrap_or_else(|| "queued".to_string()),
            attempts: row.optional::<i64>("attempts")?.unwrap_or(0),
        });
    }
    Ok(claimed)
}

async fn lease_run_row(runner: &RoundRunner, run_id: i64) -> anyhow::Result<()> {
    let mut record = Record::new();
    record.insert(
        "lease_until",
        serde_json::json!(now_seconds() + LEASE_SECONDS),
    );
    record.insert("worker_id", serde_json::json!(WORKER_ID));
    runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("id", serde_json::json!(run_id))?
        .update(record)
        .await?;
    Ok(())
}

async fn process_run_row(
    runner: &RoundRunner,
    rule: &ExportRule,
    row: &RunRow,
    detail: &InstanceDetail,
    report: &mut ProcessReport,
) {
    // 实例早于规则生效时间：不补历史，直接收口。
    if !instance_after_effective_from(rule, detail) {
        finalize_row(
            runner,
            row,
            "skipped",
            None,
            Some("实例创建时间早于规则生效时间"),
            None,
        )
        .await;
        report.skipped += 1;
        return;
    }

    // 标记先于写入与表单解析：退回/拒绝不能因为写流程还没满足触发条件、
    // 或实例表单缺失/变更导致解析失败，就把已写目标行的状态留在「有效」。
    let mark = mark_written_rows(runner, rule, &row.instance_code, detail).await;
    report.marked += mark.applied;

    if mark.retry || mark.review {
        let message = mark
            .error
            .clone()
            .unwrap_or_else(|| "状态列标记失败".to_string());
        if mark.review {
            finalize_row(
                runner,
                row,
                "review",
                None,
                Some(&message),
                Some(row.attempts + 1),
            )
            .await;
            report.review += 1;
        } else {
            schedule_retry(runner, row, &message, report).await;
        }
        return;
    }

    let form = match InstanceForm::from_detail(detail) {
        Ok(form) => form,
        Err(error) => {
            apply_export_error(runner, row, error, report).await;
            return;
        }
    };

    let mut decision = decide_write(runner, rule, row, detail, &form).await;
    report.written += decision.written;

    // 本轮刚写出的目标行也要在同一轮补写状态列：终态实例的规则行写入后
    // `next_attempt_at` 会被清空（不再复核），等到下一轮就永远不会发生。
    if decision.written > 0 {
        let mark_after = mark_written_rows(runner, rule, &row.instance_code, detail).await;
        report.marked += mark_after.applied;
        if mark_after.retry || mark_after.review {
            let message = mark_after
                .error
                .clone()
                .unwrap_or_else(|| "状态列标记失败".to_string());
            if mark_after.review {
                decision.status = "review";
                decision.next_attempt_at = None;
            } else {
                decision.next_attempt_at = Some(now_seconds() + retry_backoff(decision.attempts));
            }
            decision.error = Some(match decision.error.take() {
                Some(existing) => format!("{existing}；{message}"),
                None => message,
            });
        }
    }

    match decision.status {
        "terminal" => report.terminal += 1,
        "review" => report.review += 1,
        "retry" => report.retry += 1,
        "skipped" => report.skipped += 1,
        _ => {}
    }
    finalize_row(
        runner,
        row,
        decision.status,
        decision.next_attempt_at,
        decision.error.as_deref(),
        Some(decision.attempts),
    )
    .await;
}

/// 一次写流程的结论（不直接落库，统一由调用方收口）。
struct WriteDecision {
    status: &'static str,
    next_attempt_at: Option<i64>,
    error: Option<String>,
    written: usize,
    attempts: i64,
}

async fn decide_write(
    runner: &RoundRunner,
    rule: &ExportRule,
    row: &RunRow,
    detail: &InstanceDetail,
    form: &InstanceForm,
) -> WriteDecision {
    let now = now_seconds();
    let attempts = row.attempts + 1;
    match compute_node_passed(rule, detail) {
        TriggerOutcome::Passed { node_id } => {
            // all_rows 只要求实例条件命中；行级条件仅用于 matched_rows 的逐行筛选。
            let report = match rule.row_filter {
                RowFilter::AllRows => rule.condition.evaluate_instance(form),
                RowFilter::MatchedRows => rule.condition.evaluate(form),
            };
            if !report.matched {
                return WriteDecision {
                    status: "unmatched",
                    next_attempt_at: None,
                    error: Some(join_notes(&report.drifts, "实例/明细条件未命中")),
                    written: 0,
                    attempts,
                };
            }

            let mut notes: Vec<String> = report.drifts.iter().map(ToString::to_string).collect();
            let coordinates = BitableCoordinates {
                app_token: rule.target_base_token.clone(),
                table_id: rule.target_table_id.clone(),
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
                    return outbound_decision(failure, attempts);
                }
            };
            let maps = match load_field_maps(runner, rule.id).await {
                Ok(maps) => maps,
                Err(error) => {
                    return export_error_decision(error, attempts);
                }
            };
            let plan = match build_rows(rule, &maps, detail, form, &fields) {
                Ok(plan) => plan,
                Err(error) => {
                    return export_error_decision(error, attempts);
                }
            };
            notes.extend(plan.drifts.iter().map(ToString::to_string));

            let mut written = 0usize;
            let mut retry = 0usize;
            let mut review = plan.reviews.len();
            let mut terminal = 0usize;

            for review_row in &plan.reviews {
                notes.push(format!("行 {}：{}", review_row.row_key, review_row.reason));
                if let Err(error) =
                    save_row_review(runner, rule, &row.instance_code, &node_id, review_row).await
                {
                    tracing::warn!(
                        error = %error,
                        rule_id = rule.id,
                        instance_code = %row.instance_code,
                        "写入行级 review 台账失败"
                    );
                }
            }

            for target in &plan.rows {
                match write_one_row(
                    runner,
                    rule,
                    &row.instance_code,
                    &node_id,
                    target,
                    &coordinates,
                )
                .await
                {
                    RowWrite::Written => written += 1,
                    RowWrite::Retry => retry += 1,
                    RowWrite::Review => review += 1,
                    RowWrite::Terminal => terminal += 1,
                }
            }

            let status = aggregate_rule_status(written, retry, review, terminal);
            let next_attempt_at = match status {
                "retry" => Some(now + retry_backoff(attempts)),
                "written" | "partial" if !instance_is_terminal(detail) => {
                    Some(now + WRITTEN_RECHECK_SECONDS)
                }
                _ => None,
            };
            if notes.is_empty() && written == 0 && plan.rows.is_empty() {
                notes.push("没有匹配的明细行".to_string());
            }
            let error = if notes.is_empty() {
                None
            } else {
                Some(notes.join("；"))
            };
            WriteDecision {
                status,
                next_attempt_at,
                error,
                written,
                attempts,
            }
        }
        TriggerOutcome::WaitingFinal { .. } => WriteDecision {
            status: "waiting_final",
            next_attempt_at: Some(now + wait_backoff(attempts)),
            error: None,
            written: 0,
            attempts,
        },
        TriggerOutcome::NotReached => WriteDecision {
            status: "waiting_node",
            next_attempt_at: Some(now + wait_backoff(attempts)),
            error: None,
            written: 0,
            attempts,
        },
        TriggerOutcome::NotPassed { .. } | TriggerOutcome::RolledBack { .. } => {
            decide_not_passed(runner, rule, row, detail, attempts).await
        }
    }
}

/// 节点不再通过 / 回退重审时的处置。
///
/// 已写过：保持 `written`，让标记链路继续维护状态列；实例终态则停止复核。
/// 未写过：实例仍 PENDING 就继续等（事件会唤醒），终态则收口。
async fn decide_not_passed(
    runner: &RoundRunner,
    rule: &ExportRule,
    row: &RunRow,
    detail: &InstanceDetail,
    attempts: i64,
) -> WriteDecision {
    let has_written = match has_written_rows(runner, rule.id, &row.instance_code).await {
        Ok(value) => value,
        Err(error) => {
            return export_error_decision(ExportError::Retryable(error.to_string()), attempts);
        }
    };
    if has_written {
        return WriteDecision {
            status: "written",
            next_attempt_at: if instance_is_terminal(detail) {
                None
            } else {
                Some(now_seconds() + WRITTEN_RECHECK_SECONDS)
            },
            error: None,
            written: 0,
            attempts,
        };
    }
    if instance_is_terminal(detail) {
        WriteDecision {
            status: "terminal",
            next_attempt_at: None,
            error: Some("审批实例最终未通过".to_string()),
            written: 0,
            attempts,
        }
    } else {
        WriteDecision {
            status: if rule.trigger_mode == TriggerMode::FinalApproved {
                "waiting_final"
            } else {
                "waiting_node"
            },
            next_attempt_at: Some(now_seconds() + wait_backoff(attempts)),
            error: None,
            written: 0,
            attempts,
        }
    }
}

fn export_error_decision(error: ExportError, attempts: i64) -> WriteDecision {
    let (status, next_attempt_at) = match &error {
        ExportError::Retryable(_) => ("retry", Some(now_seconds() + retry_backoff(attempts))),
        ExportError::Fatal(_) => ("terminal", None),
        ExportError::Review(_) => ("review", None),
    };
    WriteDecision {
        status,
        next_attempt_at,
        error: Some(error.to_string()),
        written: 0,
        attempts,
    }
}

fn outbound_decision(failure: OutboundFailure, attempts: i64) -> WriteDecision {
    let message = failure.message;
    match failure.kind {
        FailureKind::Fatal { .. } => WriteDecision {
            status: "terminal",
            next_attempt_at: None,
            error: Some(message),
            written: 0,
            attempts,
        },
        _ => WriteDecision {
            status: "retry",
            next_attempt_at: Some(now_seconds() + retry_backoff(attempts)),
            error: Some(message),
            written: 0,
            attempts,
        },
    }
}

fn aggregate_rule_status(
    written: usize,
    retry: usize,
    review: usize,
    terminal: usize,
) -> &'static str {
    if retry > 0 {
        "retry"
    } else if written > 0 && (review > 0 || terminal > 0) {
        "partial"
    } else if review > 0 {
        "review"
    } else if terminal > 0 {
        "terminal"
    } else {
        "written"
    }
}

async fn finalize_row(
    runner: &RoundRunner,
    row: &RunRow,
    status: &str,
    next_attempt_at: Option<i64>,
    error: Option<&str>,
    attempts: Option<i64>,
) {
    let mut record = Record::new();
    record.insert("write_status", serde_json::json!(status));
    record.insert(
        "next_attempt_at",
        next_attempt_at.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    record.insert("lease_until", serde_json::Value::Null);
    record.insert("worker_id", serde_json::Value::Null);
    record.insert(
        "last_error",
        error.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    if let Some(attempts) = attempts {
        record.insert("attempts", serde_json::json!(attempts));
    }
    let query = match runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("id", serde_json::json!(row.id))
    {
        Ok(query) => query,
        Err(error) => {
            tracing::warn!(error = %error, run_id = row.id, "构造规则行收口更新失败");
            return;
        }
    };
    if let Err(update_error) = query.update(record).await {
        tracing::warn!(
            error = %update_error,
            run_id = row.id,
            status,
            "规则行收口写入失败"
        );
    }
}

/// 可重试失败：已写过的行保持 `written`（只延后复核），其余进入 `retry`。
async fn schedule_retry(
    runner: &RoundRunner,
    row: &RunRow,
    message: &str,
    report: &mut ProcessReport,
) {
    let attempts = row.attempts + 1;
    let status = if row.write_status == "written" {
        "written"
    } else if attempts >= MAX_ATTEMPTS {
        "terminal"
    } else {
        "retry"
    };
    let next = if attempts >= MAX_ATTEMPTS && status == "terminal" {
        None
    } else {
        Some(now_seconds() + retry_backoff(attempts))
    };
    finalize_row(runner, row, status, next, Some(message), Some(attempts)).await;
    match status {
        "terminal" => {
            report.terminal += 1;
            metrics::counter!("feishu_approval_export_terminal_total").increment(1);
        }
        _ => report.retry += 1,
    }
}

async fn apply_export_error(
    runner: &RoundRunner,
    row: &RunRow,
    error: ExportError,
    report: &mut ProcessReport,
) {
    let decision = export_error_decision(error, row.attempts + 1);
    match decision.status {
        "terminal" => {
            report.terminal += 1;
            metrics::counter!("feishu_approval_export_terminal_total").increment(1);
        }
        "review" => report.review += 1,
        _ => report.retry += 1,
    }
    finalize_row(
        runner,
        row,
        decision.status,
        decision.next_attempt_at,
        decision.error.as_deref(),
        Some(decision.attempts),
    )
    .await;
}

// ---------------------------------------------------------------------------
// 行级写入
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowWrite {
    Written,
    Retry,
    Review,
    Terminal,
}

/// 写一条明细行对应的目标记录。
///
/// 已 `written` 的行不重写：实例退回后重新通过只恢复状态列，不重建目标行。
async fn write_one_row(
    runner: &RoundRunner,
    rule: &ExportRule,
    instance_code: &str,
    trigger_node_id: &str,
    target: &TargetRow,
    coordinates: &BitableCoordinates,
) -> RowWrite {
    // 多触发节点时沿用台账里首次建立的那个节点，避免同一行在不同节点下重复建账。
    let mut trigger_node_id = trigger_node_id.to_string();
    let existing = match find_row_run(runner, rule.id, instance_code, &target.row_key).await {
        Ok(existing) => existing,
        Err(error) => {
            tracing::warn!(error = %error, rule_id = rule.id, "读取行级运行台账失败");
            return RowWrite::Retry;
        }
    };

    let mut attempts = 0i64;
    // 强制重建会把台账推进到下一代；这里必须沿用该行自己的代次派生 Token，
    // 否则会重新用旧 Token 撞回已被手工删除的记录 id（「重放成功」是假象）。
    let mut generation = 0i64;
    if let Some(existing) = &existing {
        let status = existing
            .optional::<String>("write_status")
            .ok()
            .flatten()
            .unwrap_or_default();
        attempts = existing
            .optional::<i64>("attempts")
            .ok()
            .flatten()
            .unwrap_or(0);
        generation = existing
            .optional::<i64>("generation")
            .ok()
            .flatten()
            .unwrap_or(0);
        if let Ok(Some(stored)) = existing.optional::<String>("trigger_node_id") {
            if !stored.trim().is_empty() {
                trigger_node_id = stored;
            }
        }
        match status.as_str() {
            "written" => return RowWrite::Written,
            "review" => return RowWrite::Review,
            "terminal" => return RowWrite::Terminal,
            _ => {}
        }
    }

    let token = client_token(rule.id, instance_code, &target.row_key, generation);
    let payload_hash = target.payload_hash.clone();

    match &existing {
        Some(existing) => {
            let id: i64 = match existing.require("id") {
                Ok(id) => id,
                Err(error) => {
                    tracing::warn!(error = %error, "行级运行台账缺 id");
                    return RowWrite::Retry;
                }
            };
            let mut record = Record::new();
            record.insert("write_status", serde_json::json!("writing"));
            record.insert("client_token", serde_json::json!(token));
            record.insert("payload_hash", serde_json::json!(payload_hash));
            record.insert("last_error", serde_json::Value::Null);
            let query = match runner
                .context
                .approval_export_runs()
                .query()
                .where_eq("id", serde_json::json!(id))
            {
                Ok(query) => query,
                Err(error) => {
                    tracing::warn!(error = %error, "构造行级台账更新失败");
                    return RowWrite::Retry;
                }
            };
            if let Err(error) = query.update(record).await {
                tracing::warn!(error = %error, "标记行级 writing 失败");
                return RowWrite::Retry;
            }
        }
        None => {
            let mut record = Record::new();
            record.insert("rule_id", serde_json::json!(rule.id));
            record.insert("approval_code", serde_json::json!(rule.approval_code));
            record.insert("instance_code", serde_json::json!(instance_code));
            record.insert("trigger_node_id", serde_json::json!(&trigger_node_id));
            record.insert("row_key", serde_json::json!(target.row_key));
            record.insert("generation", serde_json::json!(generation));
            record.insert("write_status", serde_json::json!("writing"));
            record.insert("mark_status", serde_json::json!("none"));
            record.insert("client_token", serde_json::json!(token));
            record.insert("payload_hash", serde_json::json!(payload_hash));
            record.insert("attempts", serde_json::json!(0));
            if let Err(error) = runner
                .context
                .approval_export_runs()
                .query()
                .insert(record)
                .await
            {
                // 并发插入撞唯一键：下一轮按已有行继续，不在这里硬重试。
                tracing::warn!(error = %error, "行级运行台账插入失败");
                return RowWrite::Retry;
            }
        }
    }

    match bitable::create_record(
        runner.transport(),
        runner.sleeper.as_ref(),
        &runner.tokens,
        coordinates,
        target.fields_map(),
        &token,
    )
    .await
    {
        Ok(created) => {
            if let Err(error) = finalize_row_run(
                runner,
                rule.id,
                instance_code,
                &trigger_node_id,
                &target.row_key,
                generation,
                "written",
                Some(&created.record_id),
                None,
                attempts + 1,
            )
            .await
            {
                tracing::warn!(error = %error, "写行级 written 台账失败");
            }
            metrics::counter!("feishu_approval_export_rows_written_total").increment(1);
            RowWrite::Written
        }
        Err(failure) => {
            let attempts = attempts + 1;
            let (status, error) = match failure.kind {
                FailureKind::Fatal { .. } => ("terminal", failure.message),
                _ if attempts >= MAX_ATTEMPTS => ("terminal", failure.message),
                _ => ("retry", failure.message),
            };
            if status == "terminal" {
                metrics::counter!("feishu_approval_export_terminal_total").increment(1);
            }
            if let Err(error) = finalize_row_run(
                runner,
                rule.id,
                instance_code,
                &trigger_node_id,
                &target.row_key,
                generation,
                status,
                None,
                Some(&error),
                attempts,
            )
            .await
            {
                tracing::warn!(error = %error, "写行级失败台账失败");
            }
            if status == "terminal" {
                RowWrite::Terminal
            } else {
                RowWrite::Retry
            }
        }
    }
}

/// 取该 (规则, 实例, 行键) 的**最新代次**台账行。
///
/// 正常写入只有 generation=0；强制重建会把同一行推进到 generation+1，因此
/// 「最新」而不是「第 0 代」才是当前有效的那一行。
///
/// **刻意不按 `trigger_node_id` 过滤**：一个规则可以配置多个触发节点，谁先通过
/// 谁建立台账。若按本轮算出的节点过滤，后通过的节点会误判为「没写过」，
/// 进而撞 `client_token` 唯一键并重试到终态。
async fn find_row_run(
    runner: &RoundRunner,
    rule_id: i64,
    instance_code: &str,
    row_key: &str,
) -> anyhow::Result<Option<Record>> {
    Ok(runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .where_eq("row_key", serde_json::json!(row_key))?
        .order_by("generation", SortOrder::Desc)?
        .page(1, 1)?
        .optional()
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn finalize_row_run(
    runner: &RoundRunner,
    rule_id: i64,
    instance_code: &str,
    trigger_node_id: &str,
    row_key: &str,
    generation: i64,
    status: &str,
    target_record_id: Option<&str>,
    error: Option<&str>,
    attempts: i64,
) -> anyhow::Result<()> {
    let mut record = Record::new();
    record.insert("write_status", serde_json::json!(status));
    record.insert("attempts", serde_json::json!(attempts));
    record.insert(
        "last_error",
        error.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    if let Some(record_id) = target_record_id {
        record.insert("target_record_id", serde_json::json!(record_id));
        record.insert("written_at", serde_json::json!(now_seconds()));
    }
    runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .where_eq("trigger_node_id", serde_json::json!(trigger_node_id))?
        .where_eq("row_key", serde_json::json!(row_key))?
        .where_eq("generation", serde_json::json!(generation))?
        .update(record)
        .await?;
    Ok(())
}

async fn save_row_review(
    runner: &RoundRunner,
    rule: &ExportRule,
    instance_code: &str,
    trigger_node_id: &str,
    review: &crate::addon::feishu::domain::approval_export::RowReview,
) -> anyhow::Result<()> {
    let existing = find_row_run(runner, rule.id, instance_code, &review.row_key).await?;
    if let Some(existing) = &existing {
        let status = existing
            .optional::<String>("write_status")
            .ok()
            .flatten()
            .unwrap_or_default();
        // 已经成功写出的目标记录不能因为后来的映射/配置变更被降级成 review：
        // 它仍需要参与状态列标记；要重建请走 force-recreate。
        if status == "written" {
            tracing::debug!(
                rule_id = rule.id,
                instance_code = %instance_code,
                row_key = %review.row_key,
                "行级 review 命中已写目标行，保持 written 以继续标记"
            );
            return Ok(());
        }
        let generation: i64 = existing.optional("generation").ok().flatten().unwrap_or(0);
        let stored_node = existing
            .optional::<String>("trigger_node_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let node_id = if stored_node.trim().is_empty() {
            trigger_node_id
        } else {
            stored_node.as_str()
        };
        let mut record = Record::new();
        record.insert("write_status", serde_json::json!("review"));
        record.insert("last_error", serde_json::json!(review.reason));
        runner
            .context
            .approval_export_runs()
            .query()
            .where_eq("rule_id", serde_json::json!(rule.id))?
            .where_eq("instance_code", serde_json::json!(instance_code))?
            .where_eq("trigger_node_id", serde_json::json!(node_id))?
            .where_eq("row_key", serde_json::json!(review.row_key))?
            .where_eq("generation", serde_json::json!(generation))?
            .update(record)
            .await?;
        return Ok(());
    }

    let mut record = Record::new();
    record.insert("rule_id", serde_json::json!(rule.id));
    record.insert("approval_code", serde_json::json!(rule.approval_code));
    record.insert("instance_code", serde_json::json!(instance_code));
    record.insert("trigger_node_id", serde_json::json!(trigger_node_id));
    record.insert("row_key", serde_json::json!(review.row_key));
    record.insert("generation", serde_json::json!(0));
    record.insert("write_status", serde_json::json!("review"));
    record.insert("mark_status", serde_json::json!("none"));
    record.insert("attempts", serde_json::json!(0));
    record.insert("last_error", serde_json::json!(review.reason));
    if let Err(error) = runner
        .context
        .approval_export_runs()
        .query()
        .insert(record)
        .await
    {
        // 并发插入撞唯一键 = 另一轮已经写过 review；语义上等价。
        tracing::debug!(error = %error, "行级 review 台账并发插入，按已存在处理");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// mark：目标状态列
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct MarkReport {
    applied: usize,
    retry: bool,
    review: bool,
    error: Option<String>,
}

/// 按最新实例状态维护已写行的目标状态列。
async fn mark_written_rows(
    runner: &RoundRunner,
    rule: &ExportRule,
    instance_code: &str,
    detail: &InstanceDetail,
) -> MarkReport {
    if !rule.mark_enabled {
        return MarkReport::default();
    }
    let Some(status_field) = rule
        .status_field
        .as_deref()
        .map(str::trim)
        .filter(|field| !field.is_empty())
    else {
        return MarkReport::default();
    };

    let rows = match load_markable_rows(runner, rule.id, instance_code).await {
        Ok(rows) => rows,
        Err(error) => {
            return MarkReport {
                retry: true,
                error: Some(format!("读取已写行失败: {error}")),
                ..MarkReport::default()
            };
        }
    };
    if rows.is_empty() {
        return MarkReport::default();
    }

    let desired: MarkOutcome = desired_mark(rule, detail, true);
    if desired.status == MarkStatus::None {
        return MarkReport::default();
    }
    let Some(label) = desired.label.clone() else {
        return MarkReport::default();
    };

    let mut pending: Vec<(i64, String, String)> = Vec::new();
    for row in &rows {
        let id = match row.require::<i64>("id") {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(error = %error, "已写行缺 id");
                continue;
            }
        };
        let Some(record_id) = row
            .optional::<String>("target_record_id")
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let applied = row.optional::<String>("applied_label").ok().flatten();
        let current_mark = row
            .optional::<String>("mark_status")
            .ok()
            .flatten()
            .unwrap_or_default();
        if !should_apply(applied.as_deref(), &desired) {
            // 文案已经一致：只把台账的 mark_status 对齐，不碰目标表。
            if current_mark != desired.status.as_str() {
                if let Err(error) =
                    update_mark_ledger(runner, id, desired.status.as_str(), None, None).await
                {
                    tracing::warn!(error = %error, run_id = id, "对齐 mark_status 失败");
                }
            }
            continue;
        }
        pending.push((id, record_id, applied.unwrap_or_default()));
    }

    if pending.is_empty() {
        return MarkReport::default();
    }

    let status_field_name = match resolve_status_field_name(runner, rule, status_field).await {
        Ok(name) => name,
        Err(message) => {
            return MarkReport {
                review: true,
                error: Some(message),
                ..MarkReport::default()
            };
        }
    };

    let mut fields = std::collections::BTreeMap::new();
    fields.insert(status_field_name, serde_json::json!(label));
    let coordinates = BitableCoordinates {
        app_token: rule.target_base_token.clone(),
        table_id: rule.target_table_id.clone(),
        view_id: None,
    };

    // `batch_update` 是全有全无语义。先按派发链路同一子批大小切块；某块失败时
    // 再二分定位毒记录——目标行被手工删除时只让那一行 mark_failed，同块其它行
    // 仍然能标记成功（否则一条删除会把整块、乃至后续块全部拖住）。
    let mut applied = 0usize;
    let mut failed = 0usize;
    let mut any_fatal = false;
    let mut first_error: Option<String> = None;

    for chunk in pending.chunks(bitable::BACKFILL_CHUNK) {
        let mut queue: Vec<&[(i64, String, String)]> = vec![chunk];
        while let Some(group) = queue.pop() {
            let payload: Vec<bitable::BackfillRow> = group
                .iter()
                .map(|(_, record_id, _)| (record_id.clone(), fields.clone()))
                .collect();
            match bitable::batch_update_records(
                runner.transport(),
                runner.sleeper.as_ref(),
                &runner.tokens,
                &coordinates,
                &payload,
            )
            .await
            {
                Ok(()) => {
                    for (id, _, _) in group {
                        if let Err(error) = update_mark_ledger(
                            runner,
                            *id,
                            desired.status.as_str(),
                            Some(&label),
                            None,
                        )
                        .await
                        {
                            tracing::warn!(error = %error, run_id = id, "写标记台账失败");
                        }
                    }
                    applied += group.len();
                }
                Err(failure) => {
                    let fatal = matches!(failure.kind, FailureKind::Fatal { .. });
                    // 只有业务性（Fatal）错误才值得二分定位毒记录；网络/限流错误
                    // 整批都没成功，二分只会放大请求量。
                    if fatal && group.len() > 1 {
                        let middle = group.len() / 2;
                        queue.push(&group[middle..]);
                        queue.push(&group[..middle]);
                        continue;
                    }
                    let message = failure.message;
                    if fatal {
                        any_fatal = true;
                    }
                    if first_error.is_none() {
                        first_error = Some(format!("目标状态列更新失败: {message}"));
                    }
                    for (id, _, _) in group {
                        failed += 1;
                        if let Err(error) =
                            update_mark_ledger(runner, *id, "mark_failed", None, Some(&message))
                                .await
                        {
                            tracing::warn!(error = %error, run_id = id, "写 mark_failed 台账失败");
                        }
                    }
                    metrics::counter!("feishu_approval_export_mark_failed_total")
                        .increment(group.len() as u64);
                }
            }
        }
    }

    if applied > 0 {
        metrics::counter!("feishu_approval_export_rows_marked_total").increment(applied as u64);
    }
    if failed > 0 {
        return MarkReport {
            applied,
            retry: !any_fatal,
            review: any_fatal,
            error: first_error,
        };
    }
    MarkReport {
        applied,
        ..MarkReport::default()
    }
}

async fn load_markable_rows(
    runner: &RoundRunner,
    rule_id: i64,
    instance_code: &str,
) -> anyhow::Result<Vec<Record>> {
    // 一笔付款明细可能有几十行；`.all()` 默认一页 10 行，直接调用会漏标后面的记录。
    Ok(crate::addon::feishu::domain::repository::all_pages(
        runner
            .context
            .approval_export_runs()
            .query()
            .where_eq("rule_id", serde_json::json!(rule_id))?
            .where_eq("instance_code", serde_json::json!(instance_code))?
            .where_eq("write_status", serde_json::json!("written"))?
            .where_ne("row_key", serde_json::json!(RULE_ROW_KEY))?
            .select_fields(&["id", "target_record_id", "applied_label", "mark_status"])?,
        MARKABLE_MAX_PAGES,
    )
    .await?)
}

async fn resolve_status_field_name(
    runner: &RoundRunner,
    rule: &ExportRule,
    status_field: &str,
) -> Result<String, String> {
    let cached = rule
        .status_field_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let coordinates = BitableCoordinates {
        app_token: rule.target_base_token.clone(),
        table_id: rule.target_table_id.clone(),
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
            // 取字段失败是临时故障；有缓存列名就先按缓存写，下一轮再纠正。
            if let Some(name) = cached {
                tracing::warn!(
                    error = %failure.message,
                    "读取目标表字段失败，沿用缓存的状态列名"
                );
                return Ok(name.to_string());
            }
            return Err(format!("读取目标表字段失败: {}", failure.message));
        }
    };
    match bitable::resolve_field_name(&fields, status_field) {
        Ok(name) => Ok(name),
        Err(error) => {
            // 状态列被改名时，按 field_id 重新解析能拿到新名字；解析不到才退回缓存。
            if let Some(name) = cached {
                tracing::warn!(
                    error = %error,
                    "目标状态列无法按 field_id 解析，沿用缓存列名"
                );
                return Ok(name.to_string());
            }
            Err(format!("目标状态列不可用: {error}"))
        }
    }
}

async fn update_mark_ledger(
    runner: &RoundRunner,
    run_id: i64,
    mark_status: &str,
    applied_label: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let mut record = Record::new();
    record.insert("mark_status", serde_json::json!(mark_status));
    record.insert(
        "applied_label",
        applied_label.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    record.insert(
        "last_error",
        error.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    if applied_label.is_some() {
        record.insert("marked_at", serde_json::json!(now_seconds()));
    }
    runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("id", serde_json::json!(run_id))?
        .update(record)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 纯函数与调度
// ---------------------------------------------------------------------------

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 实例创建时间是否在规则生效时间之后。
///
/// `effective_from` 是 Timestamp（秒），实例详情的 `start_time` 是毫秒字符串。
/// 取不到实例创建时间时放行——宁可多处理一条，也不要因为上游字段缺失而
/// 静默漏掉全部实例。
fn instance_after_effective_from(rule: &ExportRule, detail: &InstanceDetail) -> bool {
    let Some(floor) = rule.effective_from else {
        return true;
    };
    let Some(start_ms) = detail
        .start_time
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
    else {
        return true;
    };
    start_ms >= floor.saturating_mul(1000)
}

fn instance_is_terminal(detail: &InstanceDetail) -> bool {
    detail.reverted == Some(true)
        || matches!(
            detail.status.as_deref(),
            Some("APPROVED" | "REJECTED" | "CANCELED" | "DELETED" | "REVERTED")
        )
}

/// 等待节点/最终通过的复核退避：5、10、20、30 分钟封顶。
fn wait_backoff(attempts: i64) -> i64 {
    let exponent = (attempts.max(1) - 1).min(3) as u32;
    (WAIT_BASE_SECONDS << exponent).min(WAIT_MAX_SECONDS)
}

/// 可重试失败的指数退避，封顶 30 分钟。
fn retry_backoff(attempts: i64) -> i64 {
    let exponent = (attempts.max(1) - 1).min(10) as u32;
    RETRY_BASE_SECONDS
        .saturating_mul(1i64 << exponent)
        .min(RETRY_MAX_SECONDS)
}

fn join_notes(
    drifts: &[crate::addon::feishu::domain::approval_export::Drift],
    fallback: &str,
) -> String {
    if drifts.is_empty() {
        return fallback.to_string();
    }
    drifts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("；")
}

async fn has_written_rows(
    runner: &RoundRunner,
    rule_id: i64,
    instance_code: &str,
) -> anyhow::Result<bool> {
    let count = runner
        .context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .where_eq("write_status", serde_json::json!("written"))?
        // 规则级行也会落 written，但它没有目标记录；这里只关心真正的写出行。
        .where_ne("row_key", serde_json::json!(RULE_ROW_KEY))?
        .count()
        .await?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_windows_never_exceeds_ten_hours_and_covers_the_span() {
        let start = 1_000_000_000_000i64;
        let end = start + SEED_RECENT_MS;
        let windows = split_windows(start, end);
        assert_eq!(windows.len(), 3, "30 小时必须切成 3 个窗口");
        assert_eq!(windows.first().map(|(start, _)| *start), Some(start));
        assert_eq!(windows.last().map(|(_, end)| *end), Some(end));
        for (window_start, window_end) in &windows {
            assert!(window_end > window_start);
            assert!(window_end - window_start <= WINDOW_MAX_MS);
        }
        // 窗口首尾相接，不丢毫秒。
        for pair in windows.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
        assert!(split_windows(end, end).is_empty());
        assert!(split_windows(end, start).is_empty());
    }

    #[test]
    fn event_identity_reads_nested_codes_and_millis() {
        let raw = r#"{"type":"event_callback","event":{
            "type":"approval_instance",
            "approval_code":"code-1",
            "instance_code":"INST-1",
            "operate_time":"1790750409857"
        }}"#;
        let identity = parse_event_identity(raw).unwrap_or_else(|| panic!("应解析出身份"));
        assert_eq!(identity.approval_code, "code-1");
        assert_eq!(identity.instance_code, "INST-1");
        assert_eq!(identity.operate_seconds, Some(1_790_750_409));

        let numeric = r#"{"event":{"approval_code":"c","instance_code":"i",
            "operate_time":1790750409857}}"#;
        assert_eq!(
            parse_event_identity(numeric).and_then(|identity| identity.operate_seconds),
            Some(1_790_750_409)
        );
        let seconds = r#"{"event":{"approval_code":"c","instance_code":"i",
            "operate_time":"1790750409"}}"#;
        assert_eq!(
            parse_event_identity(seconds).and_then(|identity| identity.operate_seconds),
            Some(1_790_750_409),
            "秒级时间戳不能再除以 1000"
        );
        assert!(
            parse_event_identity(r#"{"event":{"instance_code":"i"}}"#).is_none(),
            "缺 approval_code 的报文不是回写事件"
        );
        assert!(parse_event_identity("not json").is_none());
    }

    #[test]
    fn wake_transition_only_reopens_resumable_states() {
        for status in [
            "queued",
            "waiting_node",
            "waiting_final",
            "retry",
            "written",
        ] {
            assert_eq!(wake_transition(status), Some(None), "{status}");
        }
        assert_eq!(wake_transition("unmatched"), Some(Some("queued")));
        assert_eq!(wake_transition("partial"), Some(Some("retry")));
        for closed in ["review", "terminal", "skipped", "unknown"] {
            assert_eq!(wake_transition(closed), None, "{closed}");
        }
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(wait_backoff(1), WAIT_BASE_SECONDS);
        assert_eq!(wait_backoff(2), WAIT_BASE_SECONDS * 2);
        assert_eq!(wait_backoff(3), WAIT_BASE_SECONDS * 4);
        assert_eq!(wait_backoff(4), WAIT_MAX_SECONDS);
        assert_eq!(wait_backoff(99), WAIT_MAX_SECONDS);

        assert_eq!(retry_backoff(1), RETRY_BASE_SECONDS);
        assert_eq!(retry_backoff(2), RETRY_BASE_SECONDS * 2);
        assert_eq!(retry_backoff(99), RETRY_MAX_SECONDS);
    }

    #[test]
    fn aggregate_status_prefers_retry_then_partial() {
        assert_eq!(aggregate_rule_status(0, 1, 0, 0), "retry");
        assert_eq!(aggregate_rule_status(2, 0, 0, 0), "written");
        assert_eq!(aggregate_rule_status(1, 0, 1, 0), "partial");
        assert_eq!(aggregate_rule_status(0, 0, 1, 0), "review");
        assert_eq!(aggregate_rule_status(0, 0, 0, 1), "terminal");
        assert_eq!(
            aggregate_rule_status(1, 1, 1, 1),
            "retry",
            "有可重试行时整条规则保持可重试"
        );
    }

    #[test]
    fn rule_effective_floor_is_seconds_but_instance_time_is_millis() {
        let rule = ExportRule {
            id: 1,
            title: "t".to_string(),
            approval_code: "c".to_string(),
            trigger_mode: TriggerMode::NodePassed,
            trigger_node_ids: vec!["n1".to_string()],
            require_final_approval: false,
            detail_widget_id: "d".to_string(),
            row_filter: RowFilter::AllRows,
            condition: Condition::default(),
            target_base_token: "b".to_string(),
            target_table_id: "t".to_string(),
            status_field: None,
            status_field_name: None,
            status_labels: StatusLabels::default(),
            mark_enabled: true,
            enabled: true,
            effective_from: Some(1_790_000_000),
        };
        let detail = |start_time: Option<&str>| InstanceDetail {
            instance_code: "INST".to_string(),
            approval_code: Some("c".to_string()),
            serial_number: None,
            status: Some("PENDING".to_string()),
            start_time: start_time.map(str::to_string),
            end_time: None,
            open_id: None,
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted: None,
            task_list: None,
            timeline: None,
            form: None,
        };
        assert!(instance_after_effective_from(
            &rule,
            &detail(Some("1790000000500"))
        ));
        assert!(!instance_after_effective_from(
            &rule,
            &detail(Some("1789999999500"))
        ));
        assert!(
            instance_after_effective_from(&rule, &detail(None)),
            "缺创建时间时放行，避免静默漏实例"
        );
    }

    #[test]
    fn instance_terminal_states_include_reverted_approved() {
        let mut detail = InstanceDetail {
            instance_code: "INST".to_string(),
            approval_code: None,
            serial_number: None,
            status: Some("APPROVED".to_string()),
            start_time: None,
            end_time: None,
            open_id: None,
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted: Some(true),
            task_list: None,
            timeline: None,
            form: None,
        };
        assert!(instance_is_terminal(&detail));
        detail.reverted = None;
        assert!(instance_is_terminal(&detail));
        detail.status = Some("PENDING".to_string());
        assert!(!instance_is_terminal(&detail));
    }
}
