//! 事件/周期对账的单条任务同步编排。
//!
//! 输入是事件 worker 已经拿到的实例详情与表结构；本模块负责：
//! 判定退回窗口 → 读配置/映射 → 反向映射 → shadow/实写 → 落同步状态。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use yang_base::table::Record;

use super::super::approval::InstanceDetail;
use super::super::approval_convert::{amount_currency, FixedOffset};
use super::super::approval_dispatch::widget_maps_from_rows;
use super::super::approval_export::form::InstanceForm;
use super::super::approval_notification::{send_feedback, NotificationRecipient, ReceiveIdType};
use super::super::bitable::{self, BitableCoordinates, FieldItem};
use super::super::context::FeishuContext;
use super::super::outbound::{OutboundTransport, Sleeper};
use super::super::tenant_token::TenantTokenProvider;
use super::apply::apply_plan;
use super::attachments::BitableAttachmentUploader;
use super::detect;
use super::error::SyncError;
use super::plan::{build_single, PlanInput, RecordPlan};
use super::resolvers::{DbOptionResolver, FeishuDepartmentResolver};
use super::store::{BitableConnectResolver, BitableStoreImpl};
use super::value::SyncValue;

/// 已退回但尚未看到重提证据时的复扫间隔。重提事件若被飞书省略，周期对账
/// 是唯一触发；1 分钟是「及时」与「不频繁拉详情」之间的折中。
const AWAITING_RESCAN_SECONDS: i64 = 60;
const SYNC_VERIFY_READBACK: bool = true;

/// 重试退避（秒）：30s / 2m / 10m / 30m / 1h / 2h。
///
/// 数组长度同时就是重试预算：第 7 次可重试失败进入 `review`，等人工处理。
const RETRY_BACKOFF_SECONDS: [i64; 6] = [30, 120, 600, 1_800, 3_600, 7_200];

/// 一次可重试失败后的调度决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryDecision {
    /// 继续自动重试：`next_at` 是 Unix 秒。
    Retry { attempts: i64, next_at: i64 },
    /// 预算耗尽，转人工核查。
    Exhausted { attempts: i64 },
}

/// 计算下一次重试（`previous_attempts` 是已经失败的次数）。
///
/// 负数按 0 处理：历史脏数据不应让任务直接跳过整个重试预算。
fn retry_decision(previous_attempts: i64, now: i64) -> RetryDecision {
    let attempts = previous_attempts.max(0) + 1;
    let index = (attempts - 1) as usize;
    match RETRY_BACKOFF_SECONDS.get(index) {
        Some(delay) => RetryDecision::Retry {
            attempts,
            next_at: now.saturating_add(*delay),
        },
        None => RetryDecision::Exhausted { attempts },
    }
}

/// 哪些失败值得自动重试。配置漂移、值形态不对等确定性失败直接 `review`。
fn is_retryable(error: &SyncError) -> bool {
    matches!(
        error,
        SyncError::BitableRead { .. }
            | SyncError::BitableWrite { .. }
            | SyncError::AttachmentUnavailable { .. }
    )
}

/// 是否接受这一轮退回窗口。
///
/// 旧窗口（时间更小）永远不接受；时间相同只有 `awaiting_resubmit` / `retry`
/// 可以续跑。这一点是重提链路的关键：退回事件先把 `form_sync_last_rollback_ms`
/// 写成 R 并进入 `awaiting_resubmit`，重提后时间仍是 R；若把「相等」一律当成
/// 已处理，就会永远停在等待态、一次也不写。
fn accepts_rollback(rollback_ms: i64, stored_rollback_ms: i64, state: &str) -> bool {
    if rollback_ms != stored_rollback_ms {
        return rollback_ms > stored_rollback_ms;
    }
    matches!(state, "awaiting_resubmit" | "retry")
}

/// 聚合组顶层派生值：公式、附件并集、多选并集无法归因到单个成员。
fn derived_top_widget(widget_type: &str) -> bool {
    widget_type == "formula"
        || matches!(widget_type, "attachmentV2" | "image" | "imageV2")
        || matches!(widget_type, "checkbox" | "checkboxV2")
}

/// 顶层派生值在该成员上是否写入。
fn include_derived(derived_mode: &str, index: usize) -> bool {
    match derived_mode.trim() {
        "all_members" => true,
        "skip" => false,
        _ => index == 0, // leader
    }
}

/// 一次同步的结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    Disabled,
    Skipped(&'static str),
    AwaitingResubmit,
    Observed,
    Synced { written: usize, reviews: usize },
    Retry(SyncError),
    Review(SyncError),
}

/// 单条任务同步输入。
pub(crate) struct RuntimeInput<'a> {
    pub(crate) context: &'a FeishuContext,
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) config_id: i64,
    pub(crate) record_id: &'a str,
    pub(crate) task_id: i64,
    pub(crate) form_snapshot: &'a str,
    pub(crate) backfill_field_id: &'a str,
    pub(crate) mode: &'a str,
    pub(crate) allow_clear: bool,
    pub(crate) timezone_offset: FixedOffset,
    pub(crate) detail: &'a InstanceDetail,
    pub(crate) fields: &'a [FieldItem],
    /// 聚合成员任务行不在这里处理；组同步走独立入口。
    pub(crate) group_id: Option<&'a str>,
}

/// 执行一次单条任务的表单同步。
///
/// 返回 `Err` 只表示「同步状态写库失败」——那需要事件重试；业务失败都编码在
/// [`SyncOutcome`] 里，不消耗事件的重试预算。
pub(crate) async fn sync_task(input: RuntimeInput<'_>) -> Result<SyncOutcome, SyncError> {
    if matches!(input.mode.trim(), "" | "off") {
        return Ok(SyncOutcome::Disabled);
    }
    if input.group_id.is_some_and(|group| !group.trim().is_empty()) {
        return Ok(SyncOutcome::Skipped("aggregate_member_not_supported"));
    }

    let task = input
        .context
        .approval_tasks()
        .query()
        .where_eq("id", json!(input.task_id))
        .map_err(|_| SyncError::BitableRead {
            record_id: input.record_id.to_string(),
            reason: "task_row_query_failed",
        })?
        .optional()
        .await
        .map_err(|_| SyncError::BitableRead {
            record_id: input.record_id.to_string(),
            reason: "task_row_query_failed",
        })?;
    let Some(task) = task else {
        return Ok(SyncOutcome::Skipped("task_row_missing"));
    };
    let stored_rollback = task
        .optional::<i64>("form_sync_last_rollback_ms")
        .ok()
        .flatten()
        .unwrap_or_default();
    let stored_state = task
        .optional::<String>("form_sync_state")
        .ok()
        .flatten()
        .unwrap_or_default();
    let stored_attempts = task
        .optional::<i64>("form_sync_attempts")
        .ok()
        .flatten()
        .unwrap_or_default();

    let window = detect::inspect(input.detail);
    if window.last_initiator_rollback_ms.is_none() {
        let reason = if window.non_initiator_rollback {
            "rollback_to_other_node"
        } else {
            "no_initiator_rollback"
        };
        // 周期对账只扫 awaiting/retry。若权威实例快照已经看不到「退回给发起人」，
        // 继续重试只会空转；转 closed 停表。之后的新退回时间更大，会重新打开窗口。
        if matches!(stored_state.as_str(), "awaiting_resubmit" | "retry") {
            save_state(
                input.context,
                input.task_id,
                &[
                    ("form_sync_state", json!("closed")),
                    ("form_sync_next_at", Value::Null),
                    ("form_sync_error", json!("rollback_window_gone")),
                ],
            )
            .await?;
        }
        return Ok(SyncOutcome::Skipped(reason));
    }
    let rollback_ms = window.last_initiator_rollback_ms.unwrap_or_default();
    if !accepts_rollback(rollback_ms, stored_rollback, &stored_state) {
        // 周期对账扫到 retry/awaiting 却拿到更旧的实例快照时，推迟下一次而不是
        // 保持过期的 next_at，避免每 tick 重复拉详情。
        if matches!(stored_state.as_str(), "awaiting_resubmit" | "retry") {
            save_state(
                input.context,
                input.task_id,
                &[(
                    "form_sync_next_at",
                    json!(now_seconds() + AWAITING_RESCAN_SECONDS),
                )],
            )
            .await?;
        }
        return Ok(SyncOutcome::Skipped("rollback_already_handled"));
    }
    if window.awaiting_resubmit() {
        let now = now_seconds();
        save_state(
            input.context,
            input.task_id,
            &[
                ("form_sync_state", json!("awaiting_resubmit")),
                ("form_sync_last_rollback_ms", json!(rollback_ms)),
                ("form_sync_next_at", json!(now + AWAITING_RESCAN_SECONDS)),
                ("form_sync_attempts", json!(0)),
                ("form_sync_error", Value::Null),
            ],
        )
        .await?;
        return Ok(SyncOutcome::AwaitingResubmit);
    }
    if !window.should_sync() {
        return Ok(SyncOutcome::Skipped("no_resubmit_evidence"));
    }

    let Some(form_raw) = input.detail.form.as_deref() else {
        return review(input, rollback_ms, SyncError::FormMissing).await;
    };
    let form = match super::super::approval_export::form::InstanceForm::parse(Some(form_raw)) {
        Ok(form) => form,
        Err(_) => return review(input, rollback_ms, SyncError::FormInvalid).await,
    };

    let maps = input
        .context
        .approval_field_maps()
        .query()
        .where_eq("config_id", json!(input.config_id))
        .map_err(|_| SyncError::BitableRead {
            record_id: input.record_id.to_string(),
            reason: "field_map_query_failed",
        })?
        .all()
        .await
        .map_err(|_| SyncError::BitableRead {
            record_id: input.record_id.to_string(),
            reason: "field_map_query_failed",
        })?;
    let rows: Vec<_> = maps.into_iter().map(Record::into_map).collect();
    let Some(widgets) = widget_maps_from_rows(&rows, |widget_id| {
        amount_currency(input.form_snapshot, widget_id)
    }) else {
        return review(
            input,
            rollback_ms,
            SyncError::Review {
                reason: "field_map_corrupt",
            },
        )
        .await;
    };
    if widgets.is_empty() {
        return review(
            input,
            rollback_ms,
            SyncError::Review {
                reason: "no_field_maps",
            },
        )
        .await;
    }

    let options = DbOptionResolver {
        context: input.context,
    };
    let departments = FeishuDepartmentResolver {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
    };
    let attachments = BitableAttachmentUploader {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        base_token: &input.coordinates.app_token,
        max_bytes: super::attachments::MAX_UPLOAD_BYTES,
    };
    let plan = build_single(PlanInput {
        record_id: input.record_id,
        task_id: Some(input.task_id),
        form: &form,
        widgets: &widgets,
        fields: input.fields,
        timezone_offset_seconds: input.timezone_offset.seconds(),
        allow_clear: input.allow_clear,
        row_index: Some(0),
        options: &options,
        departments: &departments,
        attachments: &attachments,
    })
    .await;
    log_plan(&plan);

    let hash = plan_hash(&plan);
    if matches!(input.mode.trim(), "shadow") {
        let state = if plan.has_reviews() {
            "review"
        } else {
            "observed"
        };
        save_state(
            input.context,
            input.task_id,
            &[
                ("form_sync_state", json!(state)),
                ("form_sync_last_rollback_ms", json!(rollback_ms)),
                ("form_sync_hash", json!(hash)),
                ("form_synced_at", json!(now_seconds())),
                ("form_sync_next_at", Value::Null),
                ("form_sync_attempts", json!(0)),
                ("form_sync_error", plan_error(&plan)),
            ],
        )
        .await?;
        return Ok(if plan.has_reviews() {
            SyncOutcome::Review(
                plan.reviews
                    .first()
                    .map(|review| review.error.clone())
                    .unwrap_or(SyncError::Review {
                        reason: "shadow_review",
                    }),
            )
        } else {
            SyncOutcome::Observed
        });
    }

    let backfill_name = bitable::resolve_field_name(input.fields, input.backfill_field_id)
        .unwrap_or_else(|_| input.backfill_field_id.to_string());
    let store = BitableStoreImpl {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        coordinates: input.coordinates,
        fields: input.fields,
    };
    let connects = BitableConnectResolver {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        coordinates: input.coordinates,
        backfill_field_name: &backfill_name,
        max_pages: 20,
    };
    match apply_plan(
        &store,
        &connects,
        &plan,
        input.allow_clear,
        SYNC_VERIFY_READBACK,
    )
    .await
    {
        Ok(report) => {
            // 附件下载/上传等可重试错误会以 plan-review 形式回来；不能把它们
            // 直接当成人工核查，否则一次网络抖动就把字段钉死在 review。
            if let Some(error) = report
                .reviews
                .iter()
                .find(|error| is_retryable(error))
                .cloned()
            {
                let (state, attempts, next_at, outcome) =
                    match retry_decision(stored_attempts, now_seconds()) {
                        RetryDecision::Retry { attempts, next_at } => (
                            "retry",
                            attempts,
                            json!(next_at),
                            SyncOutcome::Retry(error.clone()),
                        ),
                        RetryDecision::Exhausted { attempts } => (
                            "review",
                            attempts,
                            Value::Null,
                            SyncOutcome::Review(error.clone()),
                        ),
                    };
                save_state(
                    input.context,
                    input.task_id,
                    &[
                        ("form_sync_state", json!(state)),
                        ("form_sync_last_rollback_ms", json!(rollback_ms)),
                        ("form_sync_hash", json!(hash)),
                        ("form_synced_at", json!(now_seconds())),
                        ("form_sync_next_at", next_at),
                        ("form_sync_attempts", json!(attempts)),
                        (
                            "form_sync_error",
                            json!(format!("{}: {error}", error.code())),
                        ),
                    ],
                )
                .await?;
                return Ok(outcome);
            }
            let state = if report.reviews.is_empty() {
                "synced"
            } else {
                "review"
            };
            let alert_errors: Vec<SyncError> = report
                .reviews
                .iter()
                .filter(|error| should_alert(error))
                .cloned()
                .collect();
            let error = report
                .reviews
                .first()
                .map(|error| format!("{}: {error}", error.code()));
            save_state(
                input.context,
                input.task_id,
                &[
                    ("form_sync_state", json!(state)),
                    ("form_sync_last_rollback_ms", json!(rollback_ms)),
                    ("form_sync_hash", json!(hash)),
                    ("form_synced_at", json!(now_seconds())),
                    ("form_sync_next_at", Value::Null),
                    ("form_sync_attempts", json!(0)),
                    (
                        "form_sync_error",
                        error.map(Value::String).unwrap_or(Value::Null),
                    ),
                ],
            )
            .await?;
            if let Some(error) = alert_errors.first() {
                send_sync_alert(
                    input.transport,
                    input.sleeper,
                    input.tokens,
                    SyncAlert {
                        config_id: input.config_id,
                        detail: input.detail,
                        record_label: input.record_id,
                        fields: input.fields,
                        error,
                        count: alert_errors.len(),
                    },
                )
                .await;
            }
            Ok(SyncOutcome::Synced {
                written: report.written.len(),
                reviews: report.reviews.len(),
            })
        }
        Err(error) => {
            let decision = if is_retryable(&error) {
                Some(retry_decision(stored_attempts, now_seconds()))
            } else {
                None
            };
            let (state, attempts, next_at, outcome) = match decision {
                Some(RetryDecision::Retry { attempts, next_at }) => (
                    "retry",
                    attempts,
                    json!(next_at),
                    SyncOutcome::Retry(error.clone()),
                ),
                Some(RetryDecision::Exhausted { attempts }) => (
                    "review",
                    attempts,
                    Value::Null,
                    SyncOutcome::Review(error.clone()),
                ),
                None => (
                    "review",
                    stored_attempts,
                    Value::Null,
                    SyncOutcome::Review(error.clone()),
                ),
            };
            save_state(
                input.context,
                input.task_id,
                &[
                    ("form_sync_state", json!(state)),
                    ("form_sync_last_rollback_ms", json!(rollback_ms)),
                    ("form_sync_next_at", next_at),
                    ("form_sync_attempts", json!(attempts)),
                    (
                        "form_sync_error",
                        json!(format!("{}: {error}", error.code())),
                    ),
                ],
            )
            .await?;
            Ok(outcome)
        }
    }
}

async fn review(
    input: RuntimeInput<'_>,
    rollback_ms: i64,
    error: SyncError,
) -> Result<SyncOutcome, SyncError> {
    save_state(
        input.context,
        input.task_id,
        &[
            ("form_sync_state", json!("review")),
            ("form_sync_last_rollback_ms", json!(rollback_ms)),
            ("form_sync_next_at", Value::Null),
            (
                "form_sync_error",
                json!(format!("{}: {error}", error.code())),
            ),
        ],
    )
    .await?;
    Ok(SyncOutcome::Review(error))
}

async fn save_state(
    context: &FeishuContext,
    task_id: i64,
    fields: &[(&str, Value)],
) -> Result<(), SyncError> {
    let mut row = Record::new();
    for (name, value) in fields {
        row.insert(*name, value.clone());
    }
    context
        .approval_tasks()
        .query()
        .where_eq("id", json!(task_id))
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "sync_state_write_failed",
        })?
        .update(row)
        .await
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "sync_state_write_failed",
        })?;
    Ok(())
}

fn plan_hash(plan: &RecordPlan) -> String {
    let mut patches = plan.patches.clone();
    patches.sort_by(|left, right| left.field_id.cmp(&right.field_id));
    let mut text = String::new();
    for patch in patches {
        text.push_str(&patch.field_id);
        text.push('=');
        text.push_str(&patch.value.hash());
        text.push(';');
    }
    let mut verifications = plan.verifications.clone();
    verifications.sort_by(|left, right| left.field_id.cmp(&right.field_id));
    for verify in verifications {
        text.push_str(&verify.field_id);
        text.push('~');
        text.push_str(&verify.expected.hash());
        text.push(';');
    }
    SyncValue::Text(text).hash()
}

fn log_plan(plan: &RecordPlan) {
    tracing::debug!(
        task_id = ?plan.task_id,
        patches = plan.patches.len(),
        skipped = plan.skipped.len(),
        reviews = plan.reviews.len(),
        verifications = plan.verifications.len(),
        "表单反向映射完成"
    );
    for skipped in &plan.skipped {
        tracing::debug!(
            field_id = %skipped.field_id,
            widget_id = %skipped.widget_id,
            reason = %skipped.reason,
            "表单字段跳过"
        );
    }
    for review in &plan.reviews {
        tracing::debug!(
            field_id = %review.field_id,
            code = %review.error.code(),
            "表单字段待人工核查"
        );
    }
}

fn plan_error(plan: &RecordPlan) -> Value {
    plan.reviews
        .first()
        .map(|review| json!(format!("{}: {}", review.error.code(), review.error)))
        .unwrap_or(Value::Null)
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 需要主动通知发起人的非致命校验问题。
fn should_alert(error: &SyncError) -> bool {
    matches!(
        error,
        SyncError::OptionNotFound { .. } | SyncError::ReadOnlyMismatch { .. }
    )
}

fn alert_message(
    config_id: i64,
    approval_label: &str,
    record_label: &str,
    field_name: &str,
    error: &SyncError,
    count: usize,
) -> String {
    let code = error.code();
    format!(
        "【台账同步校验告警】\n配置：{config_id}\n审批：{approval_label}\n记录：{record_label}\n字段：{field_name}\n结果：{code}: {error}\n数量：{count}\n处理：请核对审批表单与多维表台账；如为公式/查找列，请确认可写字段已更新"
    )
}

struct SyncAlert<'a> {
    config_id: i64,
    detail: &'a InstanceDetail,
    record_label: &'a str,
    fields: &'a [FieldItem],
    error: &'a SyncError,
    count: usize,
}

async fn send_sync_alert(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    alert: SyncAlert<'_>,
) {
    let Some(open_id) = alert
        .detail
        .open_id
        .as_deref()
        .map(str::trim)
        .filter(|open_id| open_id.starts_with("ou_"))
    else {
        return;
    };
    let field_id = match alert.error {
        SyncError::OptionNotFound { field_id } | SyncError::ReadOnlyMismatch { field_id } => {
            field_id.as_str()
        }
        _ => return,
    };
    let field_name = bitable::resolve_field_name(alert.fields, field_id)
        .unwrap_or_else(|_| field_id.to_string());
    let approval_label = alert
        .detail
        .serial_number
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(alert.detail.instance_code.as_str());
    let text = alert_message(
        alert.config_id,
        approval_label,
        alert.record_label,
        &field_name,
        alert.error,
        alert.count,
    );
    let recipient = NotificationRecipient {
        receive_id_type: ReceiveIdType::OpenId,
        receive_id: open_id.to_string(),
    };
    let result = send_feedback(transport, sleeper, tokens, &recipient, &text, &[]).await;
    if result.message_id.is_none() {
        tracing::warn!(
            config_id = alert.config_id,
            count = alert.count,
            "台账同步校验告警未送达"
        );
    }
}

/// 聚合组同步输入。
pub(crate) struct GroupRuntimeInput<'a> {
    pub(crate) context: &'a FeishuContext,
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) config_id: i64,
    pub(crate) group_key: &'a str,
    pub(crate) group: &'a Record,
    /// 组内任务行（按 `group_seq asc, id asc`），每行含 `id` / `record_id`。
    pub(crate) members: &'a [Record],
    pub(crate) form_snapshot: &'a str,
    pub(crate) backfill_field_id: &'a str,
    pub(crate) mode: &'a str,
    pub(crate) allow_clear: bool,
    pub(crate) derived_mode: &'a str,
    /// 聚合明细行键控件 id；配置后按冻结的成员键值匹配明细行，容忍重排。
    pub(crate) row_key_widget: Option<&'a str>,
    pub(crate) timezone_offset: FixedOffset,
    pub(crate) detail: &'a InstanceDetail,
    pub(crate) fields: &'a [FieldItem],
}

/// 聚合组同步：顶层共享标量写全部成员，明细行按成员快照逐行写。
pub(crate) async fn sync_group(input: GroupRuntimeInput<'_>) -> Result<SyncOutcome, SyncError> {
    if matches!(input.mode.trim(), "" | "off") {
        return Ok(SyncOutcome::Disabled);
    }
    let window = detect::inspect(input.detail);
    if window.last_initiator_rollback_ms.is_none() {
        let reason = if window.non_initiator_rollback {
            "rollback_to_other_node"
        } else {
            "no_initiator_rollback"
        };
        let stored_state = input
            .group
            .optional::<String>("form_sync_state")
            .ok()
            .flatten()
            .unwrap_or_default();
        if matches!(stored_state.as_str(), "awaiting_resubmit" | "retry") {
            save_group_state(
                input.context,
                input.config_id,
                input.group_key,
                &[
                    ("form_sync_state", json!("closed")),
                    ("form_sync_next_at", Value::Null),
                    ("form_sync_error", json!("rollback_window_gone")),
                ],
            )
            .await?;
        }
        return Ok(SyncOutcome::Skipped(reason));
    }
    let rollback_ms = window.last_initiator_rollback_ms.unwrap_or_default();
    let stored_rollback = input
        .group
        .optional::<i64>("form_sync_last_rollback_ms")
        .ok()
        .flatten()
        .unwrap_or_default();
    let stored_state = input
        .group
        .optional::<String>("form_sync_state")
        .ok()
        .flatten()
        .unwrap_or_default();
    let stored_attempts = input
        .group
        .optional::<i64>("form_sync_attempts")
        .ok()
        .flatten()
        .unwrap_or_default();
    if !accepts_rollback(rollback_ms, stored_rollback, &stored_state) {
        if matches!(stored_state.as_str(), "awaiting_resubmit" | "retry") {
            save_group_state(
                input.context,
                input.config_id,
                input.group_key,
                &[(
                    "form_sync_next_at",
                    json!(now_seconds() + AWAITING_RESCAN_SECONDS),
                )],
            )
            .await?;
        }
        return Ok(SyncOutcome::Skipped("rollback_already_handled"));
    }
    if window.awaiting_resubmit() {
        save_group_state(
            input.context,
            input.config_id,
            input.group_key,
            &[
                ("form_sync_state", json!("awaiting_resubmit")),
                ("form_sync_last_rollback_ms", json!(rollback_ms)),
                (
                    "form_sync_next_at",
                    json!(now_seconds() + AWAITING_RESCAN_SECONDS),
                ),
                ("form_sync_attempts", json!(0)),
                ("form_sync_error", Value::Null),
            ],
        )
        .await?;
        return Ok(SyncOutcome::AwaitingResubmit);
    }
    if !window.should_sync() {
        return Ok(SyncOutcome::Skipped("no_resubmit_evidence"));
    }

    let Some(form_raw) = input.detail.form.as_deref() else {
        return group_review(input, rollback_ms, SyncError::FormMissing).await;
    };
    let form = match super::super::approval_export::form::InstanceForm::parse(Some(form_raw)) {
        Ok(form) => form,
        Err(_) => return group_review(input, rollback_ms, SyncError::FormInvalid).await,
    };

    // 成员行序：优先用 create 前冻结的快照；缺失时回退当前成员顺序（并记录原因）。
    let snapshot = input
        .group
        .optional::<String>("member_row_keys")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let member_ids: Vec<String> = input
        .members
        .iter()
        .filter_map(|member| member.optional::<String>("record_id").ok().flatten())
        .collect();
    let ordered_ids =
        match resolve_member_order(snapshot.as_ref(), &member_ids, &form, input.row_key_widget) {
            Ok(ordered) => ordered,
            Err(error) => return group_review(input, rollback_ms, error).await,
        };

    // 每个顶层明细的行数必须等于成员数；控件组内的整单共享明细不参与这项校验。
    for detail_id in form.member_detail_ids() {
        let count = form.row_count(&detail_id);
        if count != ordered_ids.len() {
            return group_review(
                input,
                rollback_ms,
                SyncError::RowCountMismatch {
                    expected: ordered_ids.len(),
                    actual: count,
                },
            )
            .await;
        }
    }

    let maps = input
        .context
        .approval_field_maps()
        .query()
        .where_eq("config_id", json!(input.config_id))
        .map_err(|_| SyncError::BitableRead {
            record_id: String::new(),
            reason: "field_map_query_failed",
        })?
        .all()
        .await
        .map_err(|_| SyncError::BitableRead {
            record_id: String::new(),
            reason: "field_map_query_failed",
        })?;
    let rows: Vec<_> = maps.into_iter().map(Record::into_map).collect();
    let Some(widgets) = widget_maps_from_rows(&rows, |widget_id| {
        amount_currency(input.form_snapshot, widget_id)
    }) else {
        return group_review(
            input,
            rollback_ms,
            SyncError::Review {
                reason: "field_map_corrupt",
            },
        )
        .await;
    };
    if widgets.is_empty() {
        return group_review(
            input,
            rollback_ms,
            SyncError::Review {
                reason: "no_field_maps",
            },
        )
        .await;
    }

    let options = DbOptionResolver {
        context: input.context,
    };
    let departments = FeishuDepartmentResolver {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
    };
    let attachments = BitableAttachmentUploader {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        base_token: &input.coordinates.app_token,
        max_bytes: super::attachments::MAX_UPLOAD_BYTES,
    };
    let backfill_name = bitable::resolve_field_name(input.fields, input.backfill_field_id)
        .unwrap_or_else(|_| input.backfill_field_id.to_string());
    let store = BitableStoreImpl {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        coordinates: input.coordinates,
        fields: input.fields,
    };
    let connects = BitableConnectResolver {
        transport: input.transport,
        sleeper: input.sleeper,
        tokens: input.tokens,
        coordinates: input.coordinates,
        backfill_field_name: &backfill_name,
        max_pages: 20,
    };

    let member_by_id: BTreeMap<String, &Record> = input
        .members
        .iter()
        .filter_map(|member| {
            member
                .optional::<String>("record_id")
                .ok()
                .flatten()
                .map(|record_id| (record_id, member))
        })
        .collect();

    let mut written = 0usize;
    let mut review_errors: Vec<SyncError> = Vec::new();
    let mut transient_errors: Vec<SyncError> = Vec::new();
    let mut hashes = Vec::new();
    for (index, record_id) in ordered_ids.iter().enumerate() {
        let Some(member) = member_by_id.get(record_id.as_str()) else {
            review_errors.push(SyncError::Review {
                reason: "group_member_missing",
            });
            continue;
        };
        let task_id = member.optional::<i64>("id").ok().flatten();
        let filtered: Vec<_> = widgets
            .iter()
            .filter(|widget| include_for_member(&form, widget, index, input.derived_mode))
            .cloned()
            .collect();
        let plan = build_single(PlanInput {
            record_id,
            task_id,
            form: &form,
            widgets: &filtered,
            fields: input.fields,
            timezone_offset_seconds: input.timezone_offset.seconds(),
            allow_clear: input.allow_clear,
            row_index: Some(index),
            options: &options,
            departments: &departments,
            attachments: &attachments,
        })
        .await;
        log_plan(&plan);
        hashes.push(plan_hash(&plan));
        if matches!(input.mode.trim(), "shadow") {
            review_errors.extend(plan.reviews.iter().map(|review| review.error.clone()));
            continue;
        }
        match apply_plan(
            &store,
            &connects,
            &plan,
            input.allow_clear,
            SYNC_VERIFY_READBACK,
        )
        .await
        {
            Ok(report) => {
                written += report.written.len();
                // `report.reviews` 已包含 `plan.reviews`，不要重复计数。
                for error in report.reviews {
                    if is_retryable(&error) {
                        transient_errors.push(error);
                    } else {
                        review_errors.push(error);
                    }
                }
            }
            Err(error) if is_retryable(&error) => {
                tracing::warn!(record_id = %record_id, error = %error, "聚合组成员表单同步暂时失败");
                transient_errors.push(error);
            }
            Err(error) => {
                tracing::warn!(record_id = %record_id, error = %error, "聚合组成员表单同步失败");
                review_errors.push(error);
            }
        }
    }

    let alert_errors: Vec<SyncError> = review_errors
        .iter()
        .filter(|error| should_alert(error))
        .cloned()
        .collect();
    let hash = SyncValue::Text(hashes.join(";")).hash();
    let shadow = matches!(input.mode.trim(), "shadow");
    let (state, attempts, next_at, error_value, outcome) = if shadow {
        let state = if review_errors.is_empty() {
            "observed"
        } else {
            "review"
        };
        let error = review_errors.first().cloned();
        (
            state,
            0,
            Value::Null,
            error
                .as_ref()
                .map(|error| json!(format!("{}: {error}", error.code())))
                .unwrap_or(Value::Null),
            match error {
                Some(error) => SyncOutcome::Review(error),
                None => SyncOutcome::Observed,
            },
        )
    } else if let Some(error) = transient_errors.first().cloned() {
        match retry_decision(stored_attempts, now_seconds()) {
            RetryDecision::Retry { attempts, next_at } => (
                "retry",
                attempts,
                json!(next_at),
                json!(format!("{}: {error}", error.code())),
                SyncOutcome::Retry(error),
            ),
            RetryDecision::Exhausted { attempts } => (
                "review",
                attempts,
                Value::Null,
                json!(format!("{}: retry_exhausted", error.code())),
                SyncOutcome::Review(error),
            ),
        }
    } else if let Some(error) = review_errors.first().cloned() {
        (
            "review",
            0,
            Value::Null,
            json!(format!("{}: {error}", error.code())),
            SyncOutcome::Review(error),
        )
    } else {
        (
            "synced",
            0,
            Value::Null,
            Value::Null,
            SyncOutcome::Synced {
                written,
                reviews: 0,
            },
        )
    };
    save_group_state(
        input.context,
        input.config_id,
        input.group_key,
        &[
            ("form_sync_state", json!(state)),
            ("form_sync_last_rollback_ms", json!(rollback_ms)),
            ("form_sync_hash", json!(hash)),
            ("form_synced_at", json!(now_seconds())),
            ("form_sync_next_at", next_at),
            ("form_sync_attempts", json!(attempts)),
            ("form_sync_error", error_value),
        ],
    )
    .await?;
    if !shadow {
        if let Some(error) = alert_errors.first() {
            send_sync_alert(
                input.transport,
                input.sleeper,
                input.tokens,
                SyncAlert {
                    config_id: input.config_id,
                    detail: input.detail,
                    record_label: input.group_key,
                    fields: input.fields,
                    error,
                    count: alert_errors.len(),
                },
            )
            .await;
        }
    }
    Ok(outcome)
}

/// 解析聚合组成员的台账行序。
///
/// 优先用建单前冻结的 `member_row_keys` 快照；快照缺失或没有 `rows` 数组时
/// 回退当前成员顺序。只要行数或成员集合不一致就整组转 `review`、零写入——顺序
/// 错了会把甲的表单值写到乙的台账行。
///
/// 配置了 `form_sync_row_key_widget` 且快照里冻结了成员键值时，再按明细行的
/// 业务键把表单行匹配回成员；要求所有带该控件的明细键序一致（本实现一个成员只带
/// 一个 `row_index`），不一致或键缺失/重复/匹配不上都转 `review`。
///
/// 行键控件应选文本/数字类：日期控件的表单回读是 RFC3339 文案，而台账单元格是
/// 毫秒数，两边规范化结果不同，会稳定落入 `RowKeyUnresolved`。
fn resolve_member_order(
    snapshot: Option<&serde_json::Value>,
    member_ids: &[String],
    form: &InstanceForm,
    row_key_widget: Option<&str>,
) -> Result<Vec<String>, SyncError> {
    let snapshot_rows: Vec<(String, Option<String>)> =
        match snapshot.and_then(|value| value.get("rows")) {
            Some(serde_json::Value::Array(rows)) => rows
                .iter()
                .filter_map(|row| {
                    let record_id = row
                        .get("record_id")
                        .and_then(serde_json::Value::as_str)?
                        .to_string();
                    let row_key = row
                        .get("row_key")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                        .map(str::to_string);
                    Some((record_id, row_key))
                })
                .collect(),
            _ => member_ids
                .iter()
                .cloned()
                .map(|record_id| (record_id, None))
                .collect(),
        };
    let index_order: Vec<String> = snapshot_rows
        .iter()
        .map(|(record_id, _)| record_id.clone())
        .collect();
    validate_member_set(&index_order, member_ids)?;

    let Some(widget_id) = row_key_widget
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(index_order);
    };
    let keys: Vec<Option<&str>> = snapshot_rows
        .iter()
        .map(|(_, key)| key.as_deref())
        .collect();
    if keys.iter().all(Option::is_none) {
        // 组创建时还没配置行键（或快照早于该配置）：退回原下标，行为与旧版一致。
        return Ok(index_order);
    }
    if keys.iter().any(Option::is_none) {
        return Err(SyncError::Review {
            reason: "row_key_snapshot_incomplete",
        });
    }
    let mut by_key: BTreeMap<&str, &str> = BTreeMap::new();
    for (record_id, key) in &snapshot_rows {
        let Some(key) = key.as_deref() else {
            continue;
        };
        if by_key.insert(key, record_id).is_some() {
            return Err(SyncError::Review {
                reason: "row_key_snapshot_duplicate",
            });
        }
    }

    let mut resolved: Option<Vec<String>> = None;
    for detail_id in form.member_detail_ids() {
        let Some(rows) = form.rows(&detail_id) else {
            continue;
        };
        // 行键控件只可能属于一个明细；其它明细不参与键序匹配，否则每个多明细
        // 聚合组都会因为「别的明细里没有这个控件」被误判成 review。
        if !rows.iter().any(|row| {
            row.cells
                .values()
                .any(|cell| cell.find_descendant(widget_id).is_some())
        }) {
            continue;
        }
        let mut sequence = Vec::with_capacity(rows.len());
        for row in rows {
            let key = row
                .cells
                .values()
                .find_map(|cell| cell.find_descendant(widget_id))
                .map(|node| node.display_text())
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .ok_or_else(|| SyncError::RowKeyUnresolved {
                    detail_id: detail_id.clone(),
                })?;
            let record_id =
                by_key
                    .get(key.as_str())
                    .ok_or_else(|| SyncError::RowKeyUnresolved {
                        detail_id: detail_id.clone(),
                    })?;
            sequence.push((*record_id).to_string());
        }
        if sequence.len() != member_ids.len() {
            return Err(SyncError::RowCountMismatch {
                expected: sequence.len(),
                actual: member_ids.len(),
            });
        }
        match &resolved {
            None => resolved = Some(sequence),
            Some(first) if *first != sequence => {
                return Err(SyncError::Review {
                    reason: "row_key_order_differs_by_detail",
                });
            }
            Some(_) => {}
        }
    }
    let Some(resolved) = resolved else {
        // 配置了行键控件，但表单的任何明细行里都找不到它：配置漂移，转人工。
        return Err(SyncError::Review {
            reason: "row_key_widget_not_found",
        });
    };
    validate_member_set(&resolved, member_ids)?;
    Ok(resolved)
}

fn validate_member_set(ordered: &[String], member_ids: &[String]) -> Result<(), SyncError> {
    if ordered.len() != member_ids.len()
        || ordered.iter().collect::<std::collections::BTreeSet<_>>()
            != member_ids.iter().collect::<std::collections::BTreeSet<_>>()
    {
        return Err(SyncError::RowCountMismatch {
            expected: ordered.len(),
            actual: member_ids.len(),
        });
    }
    Ok(())
}

fn include_for_member(
    form: &super::super::approval_export::form::InstanceForm,
    widget: &super::super::approval_convert::WidgetMap,
    index: usize,
    derived_mode: &str,
) -> bool {
    match super::plan::locate(form, &widget.widget_id) {
        super::plan::Location::Top => {
            if !derived_top_widget(&widget.widget_type) {
                return true;
            }
            include_derived(derived_mode, index)
        }
        // 控件组内的明细是整单共享值（正向只取 leader），按派生值的三种模式处理。
        super::plan::Location::Detail { detail_id, .. } if form.is_nested_detail(&detail_id) => {
            include_derived(derived_mode, index)
        }
        // 顶层明细的字段逐成员独立，全部写。
        _ => true,
    }
}

async fn group_review(
    input: GroupRuntimeInput<'_>,
    rollback_ms: i64,
    error: SyncError,
) -> Result<SyncOutcome, SyncError> {
    save_group_state(
        input.context,
        input.config_id,
        input.group_key,
        &[
            ("form_sync_state", json!("review")),
            ("form_sync_last_rollback_ms", json!(rollback_ms)),
            ("form_sync_next_at", Value::Null),
            (
                "form_sync_error",
                json!(format!("{}: {error}", error.code())),
            ),
        ],
    )
    .await?;
    Ok(SyncOutcome::Review(error))
}

async fn save_group_state(
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    fields: &[(&str, Value)],
) -> Result<(), SyncError> {
    let mut row = Record::new();
    for (name, value) in fields {
        row.insert(*name, value.clone());
    }
    context
        .approval_groups()
        .query()
        .where_eq("config_id", json!(config_id))
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "sync_state_write_failed",
        })?
        .where_eq("group_key", json!(group_key))
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "sync_state_write_failed",
        })?
        .update(row)
        .await
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "sync_state_write_failed",
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::approval_convert::{Converter, WidgetMap};
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn widget(widget_id: &str, widget_type: &str) -> WidgetMap {
        WidgetMap {
            widget_id: widget_id.to_string(),
            widget_type: widget_type.to_string(),
            required: false,
            bitable_field: format!("fld_{widget_id}"),
            interval_end_field: None,
            converter: Converter::Direct,
            option_map: BTreeMap::new(),
            option_source_key: None,
            currency: None,
            strip_spaces: false,
        }
    }

    #[test]
    fn accepts_resubmit_in_the_same_rollback_window() {
        // 回归：退回事件先写 awaiting_resubmit，重提后 rollback 时间相同。
        assert!(accepts_rollback(2_000, 2_000, "awaiting_resubmit"));
        assert!(accepts_rollback(2_000, 2_000, "retry"));
        assert!(
            accepts_rollback(2_001, 2_000, "synced"),
            "新退回窗口必须接受"
        );
        assert!(
            !accepts_rollback(2_000, 2_000, "synced"),
            "同一窗口已同步后不得重复写"
        );
        assert!(!accepts_rollback(2_000, 2_000, "observed"));
        assert!(!accepts_rollback(2_000, 2_000, "review"));
        assert!(!accepts_rollback(2_000, 2_000, "disabled"));
        assert!(
            !accepts_rollback(1_999, 2_000, "awaiting_resubmit"),
            "旧快照不得回退已处理的窗口"
        );
    }

    #[test]
    fn retry_backoff_grows_then_exhausts() {
        let now = 1_700_000_000;
        let expected = [30, 120, 600, 1_800, 3_600, 7_200];
        for (previous, delay) in expected.iter().enumerate() {
            let decision = retry_decision(previous as i64, now);
            assert_eq!(
                decision,
                RetryDecision::Retry {
                    attempts: previous as i64 + 1,
                    next_at: now + delay,
                }
            );
        }
        assert_eq!(
            retry_decision(6, now),
            RetryDecision::Exhausted { attempts: 7 },
            "第 7 次失败必须转 review"
        );
        assert_eq!(
            retry_decision(-3, now),
            RetryDecision::Retry {
                attempts: 1,
                next_at: now + 30,
            },
            "负数脏数据按第一次失败处理"
        );
    }

    #[test]
    fn retryable_classification_matches_contract() {
        for error in [
            SyncError::BitableRead {
                record_id: "r".into(),
                reason: "temporary",
            },
            SyncError::BitableWrite {
                record_id: "r".into(),
                reason: "temporary",
            },
            SyncError::AttachmentUnavailable {
                widget_id: "w".into(),
            },
        ] {
            assert!(is_retryable(&error), "{} 应可重试", error.code());
        }
        for error in [
            SyncError::MissingWidget {
                widget_id: "w".into(),
            },
            SyncError::EmptyRequired {
                widget_id: "w".into(),
            },
            SyncError::InvalidValue {
                widget_id: "w".into(),
                reason: "shape",
            },
            SyncError::UnsupportedTarget {
                field_id: "f".into(),
                field_type: Some(19),
            },
            SyncError::OptionNotFound {
                field_id: "f".into(),
            },
            SyncError::ReadOnlyMismatch {
                field_id: "f".into(),
            },
            SyncError::OptionUnresolved {
                widget_id: "w".into(),
            },
            SyncError::FieldNotFound {
                field_id: "f".into(),
            },
            SyncError::FormMissing,
            SyncError::FormInvalid,
            SyncError::Review { reason: "drift" },
        ] {
            assert!(!is_retryable(&error), "{} 不应自动重试", error.code());
        }
    }

    #[test]
    fn aggregate_derived_fields_follow_the_mode() {
        for widget_type in [
            "formula",
            "attachmentV2",
            "image",
            "imageV2",
            "checkbox",
            "checkboxV2",
        ] {
            assert!(derived_top_widget(widget_type), "{widget_type}");
        }
        for widget_type in ["input", "number", "date", "radioV2"] {
            assert!(!derived_top_widget(widget_type), "{widget_type}");
        }

        assert!(include_derived("leader", 0));
        assert!(!include_derived("leader", 1));
        assert!(include_derived("all_members", 0));
        assert!(include_derived("all_members", 3));
        assert!(!include_derived("skip", 0));
        assert!(!include_derived("skip", 2));
        assert!(include_derived(" leader ", 0), "配置值两端空白应容忍");
        assert!(!include_derived("unknown", 1), "未知值按 leader 兜底");
    }

    #[test]
    fn nested_detail_fields_use_derived_mode() {
        let form = form(
            r#"[{"id":"group","type":"workGroup","value":[
                {"id":"inner","type":"fieldList","value":[
                    [{"id":"inner_field","type":"input","value":"x"}]
                ]}
            ]}]"#,
        );
        let inner = widget("inner_field", "input");
        assert!(include_for_member(&form, &inner, 0, "leader"));
        assert!(!include_for_member(&form, &inner, 1, "leader"));
        assert!(include_for_member(&form, &inner, 1, "all_members"));
        assert!(!include_for_member(&form, &inner, 0, "skip"));
    }

    #[test]
    fn top_level_scalars_write_all_members_but_derived_values_follow_mode() {
        let form = form(
            r#"[
                {"id":"title","type":"input","value":"x"},
                {"id":"tags","type":"checkboxV2","value":["A"]}
            ]"#,
        );
        let title = widget("title", "input");
        assert!(
            include_for_member(&form, &title, 5, "skip"),
            "共享标量仍写全部成员"
        );
        let tags = widget("tags", "checkbox");
        assert!(
            !include_for_member(&form, &tags, 5, "skip"),
            "顶层多选并集按模式跳过"
        );
        assert!(!include_for_member(&form, &tags, 1, "leader"));
        assert!(include_for_member(&form, &tags, 1, "all_members"));
    }

    #[test]
    fn member_order_uses_frozen_snapshot_only_when_it_matches() {
        let members = ids(&["recA", "recB"]);
        let empty = form("[]");
        assert_eq!(
            resolve_member_order(None, &members, &empty, None),
            Ok(ids(&["recA", "recB"]))
        );
        assert_eq!(
            resolve_member_order(
                Some(&serde_json::json!({"rows": []})),
                &members,
                &empty,
                None
            ),
            Err(SyncError::RowCountMismatch {
                expected: 0,
                actual: 2,
            }),
            "空快照不得被当成有效行序"
        );

        let snapshot = serde_json::json!({
            "v": 1,
            "rows": [{"record_id": "recB"}, {"record_id": "recA"}]
        });
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &empty, None),
            Ok(ids(&["recB", "recA"]))
        );

        let missing_rows = serde_json::json!({"v": 1});
        assert_eq!(
            resolve_member_order(Some(&missing_rows), &members, &empty, None),
            Ok(ids(&["recA", "recB"])),
            "没有 rows 数组时回退当前成员顺序"
        );

        let wrong_set = serde_json::json!({
            "rows": [{"record_id": "recA"}, {"record_id": "recC"}]
        });
        assert_eq!(
            resolve_member_order(Some(&wrong_set), &members, &empty, None),
            Err(SyncError::RowCountMismatch {
                expected: 2,
                actual: 2,
            }),
            "行数相同但成员集合不同也必须拒绝"
        );

        let duplicate = serde_json::json!({
            "rows": [{"record_id": "recA"}, {"record_id": "recA"}]
        });
        assert_eq!(
            resolve_member_order(Some(&duplicate), &members, &empty, None),
            Err(SyncError::RowCountMismatch {
                expected: 2,
                actual: 2,
            })
        );

        let malformed = serde_json::json!({
            "rows": [{"record_id": "recA"}, {"record_id": 42}]
        });
        assert_eq!(
            resolve_member_order(Some(&malformed), &members, &empty, None),
            Err(SyncError::RowCountMismatch {
                expected: 1,
                actual: 2,
            })
        );
    }

    fn form(raw: &str) -> InstanceForm {
        InstanceForm::parse(Some(raw)).unwrap_or_else(|error| panic!("应可解析: {error}"))
    }

    fn group_snapshot(order: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({
            "v": 1,
            "rows": order
                .iter()
                .map(|(record_id, key)| serde_json::json!({
                    "record_id": record_id,
                    "row_key": key,
                }))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn row_key_matches_reordered_detail_rows_back_to_members() {
        let members = ids(&["recA", "recB"]);
        let snapshot = group_snapshot(&[("recA", "A-001"), ("recB", "B-002")]);
        let reordered = form(
            r#"[
                {"id":"detail","type":"fieldList","value":[
                    [{"id":"row_key","type":"input","value":"B-002"}],
                    [{"id":"row_key","type":"input","value":"A-001"}]
                ]}
            ]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &reordered, Some("row_key")),
            Ok(ids(&["recB", "recA"])),
            "明细行重排后必须按业务键匹配回成员"
        );
    }

    #[test]
    fn row_key_ignores_other_details_without_the_key_widget() {
        let members = ids(&["recA", "recB"]);
        let snapshot = group_snapshot(&[("recA", "A-001"), ("recB", "B-002")]);
        let form = form(
            r#"[
                {"id":"detail","type":"fieldList","value":[
                    [{"id":"row_key","type":"input","value":"B-002"}],
                    [{"id":"row_key","type":"input","value":"A-001"}]
                ]},
                {"id":"other","type":"fieldList","value":[
                    [{"id":"amount","type":"number","value":2}],
                    [{"id":"amount","type":"number","value":1}]
                ]}
            ]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &form, Some("row_key")),
            Ok(ids(&["recB", "recA"])),
            "没有行键控件的明细不参与键序匹配"
        );
    }

    #[test]
    fn row_key_rejects_missing_unknown_duplicate_and_inconsistent_keys() {
        let members = ids(&["recA", "recB"]);
        let snapshot = group_snapshot(&[("recA", "A-001"), ("recB", "B-002")]);

        let missing = form(
            r#"[{"id":"detail","type":"fieldList","value":[
                [{"id":"other","type":"input","value":"A-001"}],
                [{"id":"row_key","type":"input","value":"B-002"}]
            ]}]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &missing, Some("row_key")),
            Err(SyncError::RowKeyUnresolved {
                detail_id: "detail".into()
            })
        );

        let unknown = form(
            r#"[{"id":"detail","type":"fieldList","value":[
                [{"id":"row_key","type":"input","value":"X-999"}],
                [{"id":"row_key","type":"input","value":"B-002"}]
            ]}]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &unknown, Some("row_key")),
            Err(SyncError::RowKeyUnresolved {
                detail_id: "detail".into()
            })
        );

        let duplicate_snapshot = group_snapshot(&[("recA", "DUP"), ("recB", "DUP")]);
        let unchanged = form(
            r#"[{"id":"detail","type":"fieldList","value":[
                [{"id":"row_key","type":"input","value":"DUP"}],
                [{"id":"row_key","type":"input","value":"DUP"}]
            ]}]"#,
        );
        assert_eq!(
            resolve_member_order(
                Some(&duplicate_snapshot),
                &members,
                &unchanged,
                Some("row_key")
            ),
            Err(SyncError::Review {
                reason: "row_key_snapshot_duplicate"
            })
        );

        let differing = form(
            r#"[
                {"id":"d1","type":"fieldList","value":[
                    [{"id":"row_key","type":"input","value":"A-001"}],
                    [{"id":"row_key","type":"input","value":"B-002"}]
                ]},
                {"id":"d2","type":"fieldList","value":[
                    [{"id":"row_key","type":"input","value":"B-002"}],
                    [{"id":"row_key","type":"input","value":"A-001"}]
                ]}
            ]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&snapshot), &members, &differing, Some("row_key")),
            Err(SyncError::Review {
                reason: "row_key_order_differs_by_detail"
            })
        );
    }

    #[test]
    fn row_key_falls_back_to_index_for_legacy_snapshots() {
        let members = ids(&["recA", "recB"]);
        let legacy = group_snapshot(&[("recB", ""), ("recA", "")]);
        let form = form(
            r#"[{"id":"detail","type":"fieldList","value":[
                [{"id":"row_key","type":"input","value":"A-001"}],
                [{"id":"row_key","type":"input","value":"B-002"}]
            ]}]"#,
        );
        assert_eq!(
            resolve_member_order(Some(&legacy), &members, &form, Some("row_key")),
            Ok(ids(&["recB", "recA"])),
            "快照没有冻结行键时保持旧版下标语义"
        );

        let partial = serde_json::json!({
            "rows": [
                {"record_id": "recA", "row_key": "A-001"},
                {"record_id": "recB"}
            ]
        });
        assert_eq!(
            resolve_member_order(Some(&partial), &members, &form, Some("row_key")),
            Err(SyncError::Review {
                reason: "row_key_snapshot_incomplete"
            })
        );
    }

    #[test]
    fn alerts_only_for_mapping_verification_errors() {
        assert!(should_alert(&SyncError::OptionNotFound {
            field_id: "fld_bank".into(),
        }));
        assert!(should_alert(&SyncError::ReadOnlyMismatch {
            field_id: "fld_total".into(),
        }));
        assert!(!should_alert(&SyncError::UnsupportedTarget {
            field_id: "fld".into(),
            field_type: Some(19),
        }));
        assert!(!should_alert(&SyncError::Review { reason: "drift" }));
    }

    #[test]
    fn alert_message_has_location_but_no_field_values() {
        let text = alert_message(
            25,
            "SP-001",
            "rec1",
            "汇率/Exchange Rate",
            &SyncError::ReadOnlyMismatch {
                field_id: "fld_rate".into(),
            },
            2,
        );
        assert!(text.contains("【台账同步校验告警】"), "{text}");
        assert!(text.contains("配置：25"), "{text}");
        assert!(text.contains("审批：SP-001"), "{text}");
        assert!(text.contains("记录：rec1"), "{text}");
        assert!(text.contains("汇率/Exchange Rate"), "{text}");
        assert!(text.contains("readonly_mismatch"), "{text}");
        assert!(text.contains("只读列"), "{text}");
        assert!(text.contains("数量：2"), "{text}");
        assert!(!text.contains("1.5"), "报警正文不能带字段值：{text}");
        assert!(!text.contains("100"), "报警正文不能带字段值：{text}");
    }
}
