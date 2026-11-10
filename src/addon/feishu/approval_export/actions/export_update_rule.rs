//! 更新审批 → 多维表格回写规则（前端控制台向导）。
//!
//! 全量保存：规则字段与字段映射一起替换，映射先删后插，与规则行同一事务。
//! `effective_from` 沿用旧值——编辑规则不应把「只处理生效之后实例」的窗口重新
//! 放大到编辑时刻之后（否则历史补写会突然被放行）。
//!
//! 校验与创建完全共用 `config::validate_export_rule`，不存在「创建严、编辑松」
//! 的第二套规则。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

use crate::addon::feishu::approval_export::actions::export_create_rule::{
    config_error, fill_rule_record, json_text, map_insert_error, truncate_chars, ExportMapInput,
};
use crate::addon::feishu::domain::approval::subscribe_approval;
use crate::addon::feishu::domain::approval_export::config::{
    validate_export_rule, ExportFieldMapDraft, ExportRuleDraft,
};
use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

fn default_true() -> bool {
    true
}

/// 更新回写规则的输入契约：`rule_id` + 与创建相同的全量字段。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateExportRuleInput {
    pub(super) rule_id: i64,
    pub(super) title: String,
    pub(super) approval_code: String,
    pub(super) trigger_mode: String,
    #[serde(default)]
    pub(super) trigger_node_ids: Vec<String>,
    #[serde(default = "default_true")]
    pub(super) require_final_approval: bool,
    pub(super) detail_widget_id: String,
    pub(super) row_filter: String,
    #[serde(default)]
    pub(super) condition_json: Option<String>,
    pub(super) target_base_token: String,
    pub(super) target_table_id: String,
    #[serde(default)]
    pub(super) status_field: Option<String>,
    #[serde(default)]
    pub(super) status_labels: Option<serde_json::Value>,
    #[serde(default = "default_true")]
    pub(super) mark_enabled: bool,
    #[serde(default = "default_true")]
    pub(super) enabled: bool,
    pub(super) maps: Vec<ExportMapInput>,
}

impl ParamInput for UpdateExportRuleInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpdateExportRuleInput {
    fn validate_id(&self) -> Result<(), BaseError> {
        if self.rule_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "rule_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }

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

/// 注册更新规则端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_update_rule"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/rules/update",
        )
        .display_name("更新审批回写规则")
        .description("全量保存规则与字段映射；生效时间沿用旧值")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateExportRuleInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate_id()?;
    // `into_draft` 按值消费输入；主键先取出来，后面的更新与响应都还用它。
    let rule_id = input.rule_id;

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
    let result: Result<serde_json::Value, BaseError> = async {
        let existing = crate::db::select_for_update::<Record>(
            &mut transaction,
            QueryBuilder::from_pool(
                context.approval_export_rules().pool(),
                table!("feishu_approval_export_rule"),
            )
            .fields(&[
                field!("id"),
                field!("title"),
                field!("approval_code"),
                field!("detail_widget_id"),
                field!("enabled"),
                field!("effective_from"),
            ])
            .where_and(field!("id"), CompareOp::Eq, rule_id)
            .map_err(BaseError::from)?,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::RecordNotFound("审批回写规则不存在".to_string()))?;
        let before_title: String = existing.require("title")?;
        let before_enabled: bool = existing.require("enabled")?;
        let before_approval_code: String = existing.require("approval_code")?;
        // 运行台账按 rule_id 归属；换掉来源审批会让旧实例被新规则继续处理。
        // 换来源请新建规则，让两套台账各自独立。
        if before_approval_code != validated.approval_code {
            return Err(BaseError::ParamInvalid(
                "approval_code".to_string(),
                "来源审批定义不可修改；如需回写另一个审批，请新建规则".to_string(),
            ));
        }
        let before_detail_widget: String = existing.require("detail_widget_id")?;
        // `row:N` 的身份来自明细控件；换控件后旧台账会被当成同一个 N 行。
        if before_detail_widget != validated.detail_widget_id {
            return Err(BaseError::ParamInvalid(
                "detail_widget_id".to_string(),
                "明细控件不可修改（行键按它派生）；如需换明细，请新建规则".to_string(),
            ));
        }
        let effective_from: i64 = existing.optional("effective_from")?.unwrap_or(now);

        let mut rule_row = Record::new();
        fill_rule_record(&mut rule_row, &validated, now, effective_from);
        let updated = context
            .approval_export_rules()
            .query()
            .where_eq("id", serde_json::json!(rule_id))?
            .update_in_tx(&mut transaction, rule_row)
            .await
            .map_err(map_insert_error)?;
        if updated == 0 {
            return Err(BaseError::RecordNotFound("审批回写规则不存在".to_string()));
        }

        context
            .approval_export_field_maps()
            .query()
            .where_eq("rule_id", serde_json::json!(rule_id))?
            .delete_in_tx(&mut transaction)
            .await?;

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

        let unchanged_effective_from = effective_from;
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_export_rule", rule_id)?,
            Some(audit::summary([
                ("title", serde_json::json!(&before_title)),
                ("enabled", serde_json::json!(before_enabled)),
            ])?),
            Some(audit::summary([
                ("title", serde_json::json!(&validated.title)),
                ("enabled", serde_json::json!(validated.enabled)),
                ("approval_code", serde_json::json!(&validated.approval_code)),
                ("maps_count", serde_json::json!(validated.maps.len())),
                (
                    "effective_from",
                    serde_json::json!(unchanged_effective_from),
                ),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({
            "rule_id": rule_id,
            "maps_count": validated.maps.len(),
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

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
            "回写规则已更新，但审批定义事件订阅失败（worker 启动时会重订）"
        );
    }

    tracing::debug!(
        rule_id,
        approval_code = %validated.approval_code,
        maps = validated.maps.len(),
        "审批回写规则已更新"
    );
    ApiResponse::success(value, "规则已更新")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_id_must_be_positive() {
        let mut input = serde_json::from_value::<UpdateExportRuleInput>(serde_json::json!({
            "rule_id": 0,
            "title":"t","approval_code":"c","trigger_mode":"final_approved",
            "detail_widget_id":"d","row_filter":"all_rows",
            "target_base_token":"b","target_table_id":"tbl",
            "maps":[]
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(input.validate_id().is_err());
        input.rule_id = 5;
        assert!(input.validate_id().is_ok());
    }

    #[test]
    fn update_cannot_change_the_rule_identity_by_typo() {
        assert!(
            serde_json::from_value::<UpdateExportRuleInput>(serde_json::json!({
                "rule_id": 1, "title":"t","approval_code":"c","trigger_mode":"final_approved",
                "detail_widget_id":"d","row_filter":"all_rows",
                "target_base_token":"b","target_table_id":"tbl","maps":[],
                "effective_from": 123
            }))
            .is_err(),
            "effective_from 不可由编辑覆盖"
        );
    }
}
