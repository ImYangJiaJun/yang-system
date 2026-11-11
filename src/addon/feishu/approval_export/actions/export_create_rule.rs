//! 创建审批 → 多维表格回写规则（前端控制台向导的最后一步）。
//!
//! # 保存期就把话说死
//!
//! 规则一旦落库，最贵的错误是「保存成功、运行时才发现控件/列不存在」。因此本
//! Action 与 `check_rules` 共用 `config::validate_export_rule`：取审批定义快照、
//! 校验明细控件与映射来源、校验条件引用、拉目标列并校验类型，全过才写库。
//! 规则与字段映射同一事务，审计同事务追加（AUDIT.md 契约）。
//!
//! # 生效时间
//!
//! 新建规则把 `effective_from` 置为当前时间：worker 启动会回补最近 7 天的实例，
//! 不设生效时间等于把历史单据整批补写进目标表。规则只处理生效之后创建的实例。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;
#[cfg(test)]
use yang_db::DbError;

use crate::addon::feishu::domain::approval::{subscribe_approval, subscribe_bitable_file};
use crate::addon::feishu::domain::approval_comment::parse_comment_config;
use crate::addon::feishu::domain::approval_export::config::{
    validate_export_rule, ExportConfigError, ExportFieldMapDraft, ExportRuleDraft,
    ValidatedExportRule,
};
use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

/// 同一审批定义下重名时的可行动文案。
const RULE_ALREADY_EXISTS: &str = "该审批定义下已存在同名规则，请改名后重试";

fn default_true() -> bool {
    true
}

/// 一条字段映射的入参。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportMapInput {
    /// `detail` / `top` / `system` / `constant`。
    pub(super) source_kind: String,
    /// detail/top 的控件 id。
    #[serde(default)]
    pub(super) source_widget_id: String,
    /// system 的来源键。
    #[serde(default)]
    pub(super) source_system_key: String,
    #[serde(default)]
    pub(super) constant_value: Option<String>,
    /// 目标列 field_id。
    pub(super) target_field_id: String,
    /// 值转换器；缺省 `text`。
    #[serde(default = "default_converter")]
    pub(super) converter: String,
    /// 选项文案映射（审批文案 → 目标选项名）。
    #[serde(default)]
    pub(super) option_map: Option<serde_json::Value>,
    /// 目标列是否必填。
    #[serde(default)]
    pub(super) required: bool,
}

fn default_converter() -> String {
    "text".to_string()
}

/// 创建回写规则的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateExportRuleInput {
    /// 规则名；同一审批定义下唯一。
    pub(super) title: String,
    /// 来源审批定义 Code。
    pub(super) approval_code: String,
    /// `node_passed`（指定节点通过）/ `final_approved`（最终通过）。
    pub(super) trigger_mode: String,
    /// 触发节点 id 列表（`node_passed` 模式必填）。
    #[serde(default)]
    pub(super) trigger_node_ids: Vec<String>,
    /// 节点通过后是否还要求实例最终通过。
    #[serde(default = "default_true")]
    pub(super) require_final_approval: bool,
    /// 明细控件 id（行级回写来源）。
    pub(super) detail_widget_id: String,
    /// `matched_rows`（按条件筛行）/ `all_rows`（全部明细行）。
    pub(super) row_filter: String,
    /// 条件 DSL JSON；空 = 无条件（恒真）。
    #[serde(default)]
    pub(super) condition_json: Option<String>,
    /// 目标 Base Token。
    pub(super) target_base_token: String,
    /// 目标数据表 id。
    pub(super) target_table_id: String,
    /// 目标状态列 field_id；`mark_enabled=true` 时必填。
    #[serde(default)]
    pub(super) status_field: Option<String>,
    /// 状态文案覆盖（键：valid / rolled_back / rejected / canceled / reverted / deleted）。
    #[serde(default)]
    pub(super) status_labels: Option<serde_json::Value>,
    /// 是否维护目标状态列。
    #[serde(default = "default_true")]
    pub(super) mark_enabled: bool,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub(super) enabled: bool,
    /// 审批评论配置；缺省 = 不启用评论。
    #[serde(default)]
    pub(super) comment_config: Option<serde_json::Value>,
    /// 字段映射（至少一条）。
    pub(super) maps: Vec<ExportMapInput>,
}

impl ParamInput for CreateExportRuleInput {
    fn params() -> Params {
        Params::new()
    }
}

/// JSON 值 → 落库的 JSON 文本（空值/空对象落 `None`）。
pub(super) fn json_text(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let text = serde_json::to_string(value).ok()?;
    if text.trim().is_empty() || text == "{}" {
        None
    } else {
        Some(text)
    }
}

impl CreateExportRuleInput {
    fn into_draft(self) -> ExportRuleDraft {
        ExportRuleDraft {
            title: self.title,
            approval_code: self.approval_code,
            trigger_mode: self.trigger_mode,
            trigger_node_ids: self.trigger_node_ids,
            require_final_approval: self.require_final_approval,
            detail_widget_id: self.detail_widget_id,
            row_filter: self.row_filter,
            condition_json: self.condition_json,
            target_base_token: self.target_base_token,
            target_table_id: self.target_table_id,
            status_field: self.status_field,
            status_labels_json: json_text(self.status_labels.as_ref()),
            mark_enabled: self.mark_enabled,
            enabled: self.enabled,
            comment_config_json: json_text(self.comment_config.as_ref()),
            maps: self
                .maps
                .into_iter()
                .map(|map| ExportFieldMapDraft {
                    source_kind: map.source_kind,
                    source_widget_id: map.source_widget_id,
                    source_system_key: map.source_system_key,
                    constant_value: map.constant_value,
                    target_field_id: map.target_field_id,
                    converter: map.converter,
                    option_map_json: json_text(map.option_map.as_ref()),
                    required: map.required,
                })
                .collect(),
        }
    }
}

/// 按字符截断到表的 `max_length`，避免飞书返回的超长名称让整条保存失败。
pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub(super) fn map_insert_error(error: BaseError) -> BaseError {
    if crate::db::is_unique_violation(&error) {
        BaseError::ParamInvalid("title".to_string(), RULE_ALREADY_EXISTS.to_string())
    } else {
        error
    }
}

/// 注册创建规则端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_create_rule"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/rules/create",
        )
        .display_name("创建审批回写规则")
        .description("校验审批定义与目标列后创建回写规则及字段映射")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateExportRuleInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let validated = validate_export_rule(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &input.into_draft(),
    )
    .await
    .map_err(config_error)?;

    let now = now_unix_secs();
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<i64, BaseError> = async {
        let mut rule_row = Record::new();
        fill_rule_record(&mut rule_row, &validated, now, now);
        context
            .approval_export_rules()
            .query()
            .insert_in_tx(&mut transaction, rule_row)
            .await
            .map_err(map_insert_error)?;

        // `insert_in_tx` 只回受影响行数（MySQL 没有 RETURNING）；按唯一键
        // (approval_code, title) 在同一事务里取回主键。`title` 不是 filterable，
        // 所以先按 approval_code 取本定义下的行，再在内存里按标题定位。
        let candidates = context
            .approval_export_rules()
            .query()
            .select_fields(&["id", "title"])?
            .where_eq("approval_code", serde_json::json!(&validated.approval_code))?
            // 默认只取一页（10 行）且无排序；按 id 倒序保证刚插入的那行在首页。
            .order_by("id", SortOrder::Desc)?
            .page(1, yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE)?
            .all_in_tx(&mut transaction)
            .await?;
        let rule_id = candidates
            .iter()
            .find(|row| {
                row.optional::<String>("title")
                    .ok()
                    .flatten()
                    .as_deref()
                    .map(str::trim)
                    == Some(validated.title.as_str())
            })
            .map(|row| row.require::<i64>("id"))
            .transpose()?
            .ok_or_else(|| BaseError::ConfigError("规则已写入但无法取回主键".to_string()))?;

        for map in &validated.maps {
            let mut map_row = Record::new();
            map_row.insert("rule_id", serde_json::json!(rule_id));
            map_row.insert("source_kind", serde_json::json!(map.source_kind.as_str()));
            map_row.insert("source_widget_id", serde_json::json!(&map.source_widget_id));
            map_row.insert(
                "source_system_key",
                serde_json::json!(&map.source_system_key),
            );
            map_row.insert(
                "constant_value",
                map.constant_value
                    .as_deref()
                    .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
            );
            map_row.insert("target_field_id", serde_json::json!(&map.target_field_id));
            map_row.insert(
                "target_field_name",
                serde_json::json!(truncate_chars(&map.target_field_name, 255)),
            );
            map_row.insert("converter", serde_json::json!(map.converter.as_str()));
            map_row.insert(
                "option_map",
                if map.option_map.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(serde_json::to_string(&map.option_map).unwrap_or_default())
                },
            );
            map_row.insert("required", serde_json::json!(map.required));
            context
                .approval_export_field_maps()
                .query()
                .insert_in_tx(&mut transaction, map_row)
                .await?;
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_export_rule", rule_id)?,
            None,
            Some(audit::summary([
                ("title", serde_json::json!(&validated.title)),
                ("approval_code", serde_json::json!(&validated.approval_code)),
                ("maps_count", serde_json::json!(validated.maps.len())),
                ("enabled", serde_json::json!(validated.enabled)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(rule_id)
    }
    .await;
    let rule_id = FeishuContext::finish_transaction(transaction, result).await?;

    // 建规则即订阅该定义的事件：订阅是运行时状态，worker 启动时还会全量重订。
    if let Err(failure) = subscribe_approval(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &validated.approval_code,
    )
    .await
    {
        tracing::warn!(
            error = %failure.message,
            approval_code = %validated.approval_code,
            "回写规则已创建，但审批定义事件订阅失败（worker 启动时会重订）"
        );
    }

    // 评论依赖多维表格记录变更事件；订阅是运行时状态，失败只 WARN（worker 启动重订）。
    if parse_comment_config(validated.comment_config_json.as_deref())
        .ok()
        .flatten()
        .is_some_and(|config| config.enabled)
    {
        if let Err(failure) = subscribe_bitable_file(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &validated.target_base_token,
        )
        .await
        {
            tracing::warn!(
                error = %failure.message,
                base_token = %validated.target_base_token,
                "回写规则已创建，但多维表格事件订阅失败（worker 启动时会重订）"
            );
        }
    }

    tracing::debug!(
        rule_id,
        approval_code = %validated.approval_code,
        maps = validated.maps.len(),
        "审批回写规则已创建"
    );
    ApiResponse::success(
        serde_json::json!({
            "rule_id": rule_id,
            "approval_name": validated.approval_name,
            "maps_count": validated.maps.len(),
        }),
        "规则创建成功",
    )
}

/// 把校验结果填进规则行（创建与更新共用同一份列口径）。
///
/// `snapshot_at` 每次保存都刷新；`effective_from` 由调用方决定（创建 = 当前
/// 时间；更新沿用旧值，避免一次编辑把历史窗口重新放大）。
pub(super) fn fill_rule_record(
    record: &mut Record,
    validated: &ValidatedExportRule,
    snapshot_at: i64,
    effective_from: i64,
) {
    record.insert("title", serde_json::json!(&validated.title));
    record.insert("approval_code", serde_json::json!(&validated.approval_code));
    record.insert(
        "approval_name",
        if validated.approval_name.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(truncate_chars(&validated.approval_name, 512))
        },
    );
    record.insert(
        "trigger_mode",
        serde_json::json!(validated.trigger_mode.as_str()),
    );
    record.insert(
        "trigger_node_ids",
        serde_json::json!(serde_json::to_string(&validated.trigger_node_ids).unwrap_or_default()),
    );
    record.insert(
        "require_final_approval",
        serde_json::json!(validated.require_final_approval),
    );
    record.insert(
        "detail_widget_id",
        serde_json::json!(&validated.detail_widget_id),
    );
    record.insert(
        "row_filter",
        serde_json::json!(validated.row_filter.as_str()),
    );
    record.insert(
        "condition_json",
        if validated.condition_json.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(&validated.condition_json)
        },
    );
    record.insert(
        "target_base_token",
        serde_json::json!(&validated.target_base_token),
    );
    record.insert(
        "target_table_id",
        serde_json::json!(&validated.target_table_id),
    );
    record.insert(
        "status_field",
        validated
            .status_field
            .as_deref()
            .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    record.insert(
        "status_field_name",
        validated
            .status_field_name
            .as_deref()
            .map_or(serde_json::Value::Null, |value| {
                serde_json::json!(truncate_chars(value, 255))
            }),
    );
    record.insert(
        "status_labels_json",
        validated
            .status_labels_json
            .as_deref()
            .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    record.insert("mark_enabled", serde_json::json!(validated.mark_enabled));
    record.insert("enabled", serde_json::json!(validated.enabled));
    record.insert(
        "comment_config_json",
        validated
            .comment_config_json
            .as_deref()
            .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    record.insert(
        "definition_snapshot",
        serde_json::json!(&validated.form_snapshot),
    );
    record.insert("node_snapshot", serde_json::json!(&validated.node_snapshot));
    record.insert("snapshot_at", serde_json::json!(snapshot_at));
    record.insert(
        "definition_hash",
        serde_json::json!(&validated.definition_hash),
    );
    record.insert("effective_from", serde_json::json!(effective_from));
}

pub(super) fn config_error(error: ExportConfigError) -> BaseError {
    match error {
        ExportConfigError::Invalid(reasons) => {
            BaseError::ParamInvalid("rule".to_string(), reasons.join("；"))
        }
        ExportConfigError::Fetch(message) => BaseError::ParamInvalid("feishu".to_string(), message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_keeps_empty_option_map_as_none() {
        assert_eq!(json_text(None), None);
        assert_eq!(json_text(Some(&serde_json::json!(null))), None);
        assert_eq!(json_text(Some(&serde_json::json!({}))), None);
        assert_eq!(
            json_text(Some(&serde_json::json!({"差旅":"交通费"}))).as_deref(),
            Some(r#"{"差旅":"交通费"}"#)
        );
    }

    #[allow(clippy::bool_assert_comparison)]
    #[test]
    fn input_defaults_match_the_domain_defaults() {
        let parsed = serde_json::from_value::<CreateExportRuleInput>(serde_json::json!({
            "title":"付款回写",
            "approval_code":"CODE-1",
            "trigger_mode":"final_approved",
            "detail_widget_id":"detail",
            "row_filter":"all_rows",
            "target_base_token":"baseToken",
            "target_table_id":"tblToken",
            "maps":[{"source_kind":"detail","source_widget_id":"amount",
                     "target_field_id":"fldAmount","converter":"number"}]
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(parsed.require_final_approval);
        assert!(parsed.mark_enabled);
        assert!(parsed.enabled);
        assert!(!parsed.maps[0].required);
    }

    #[test]
    fn unknown_rule_keys_are_rejected() {
        assert!(
            serde_json::from_value::<CreateExportRuleInput>(serde_json::json!({
                "title":"x","approval_code":"c","trigger_mode":"final_approved",
                "detail_widget_id":"d","row_filter":"all_rows",
                "target_base_token":"b","target_table_id":"t",
                "maps":[],"hardcode":true
            }))
            .is_err()
        );
    }

    #[test]
    fn constraint_errors_become_a_friendly_title_conflict() {
        let mapped = map_insert_error(BaseError::DatabaseExecuteFailed(DbError::ConstraintError(
            "Duplicate entry".to_string(),
        )));
        match mapped {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "title");
                assert_eq!(message, RULE_ALREADY_EXISTS);
            }
            other => panic!("实际 {other:?}"),
        }
    }

    #[test]
    fn structured_unique_violations_become_a_friendly_title_conflict() {
        // 新框架把 sqlx 的 Database 错误映射成 DatabaseError(SqlErrorInfo)，
        // 只认旧 ConstraintError 会让并发重名变成 500。
        let mapped = map_insert_error(BaseError::DatabaseExecuteFailed(DbError::DatabaseError(
            Box::new(
                yang_db::SqlErrorInfo::new("23000", "Duplicate entry")
                    .with_kind_hint(yang_db::SqlErrorKind::UniqueViolation),
            ),
        )));
        match mapped {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "title");
                assert_eq!(message, RULE_ALREADY_EXISTS);
            }
            other => panic!("实际 {other:?}"),
        }
    }
}
