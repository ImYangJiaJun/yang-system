//! 回写试算（dry-run）：按规则 + 实例返回触发判定、条件结果与将写入的行。
//!
//! # 不写库
//!
//! 本端点只调用飞书读取实例详情与目标表字段，然后跑与 worker 相同的领域引擎
//! （触发判定 / 条件求值 / 行映射）。它不会创建记录、不会写运行台账，因此可以
//! 反复「试算保存」。
//!
//! # 表单值不外泄到日志
//!
//! 响应里会带目标行字段值（控制台需要它做预览），但日志只记规则 / 实例 / 行数，
//! 绝不打印值。权限复用了 `feishu.approval.write`：能看到审批表单值的人必须同时
//! 有回写配置权。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::{self, GetOutcome};
use crate::addon::feishu::domain::approval_export::config::load_rule_and_maps;
use crate::addon::feishu::domain::approval_export::{
    build_rows, compute_node_passed, ExportError, InstanceForm, RowFilter, TriggerOutcome,
};
use crate::addon::feishu::domain::bitable::{list_all_fields, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PreviewExportInput {
    /// 已保存的规则 id。
    pub(super) rule_id: i64,
    /// 审批实例 Code。
    pub(super) instance_code: String,
}

impl ParamInput for PreviewExportInput {
    fn params() -> Params {
        Params::new()
    }
}

impl PreviewExportInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.rule_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "rule_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.instance_code.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "instance_code".to_string(),
                "不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册试算端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_preview"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/export/preview")
        .display_name("审批回写试算")
        .description("按规则与实例计算触发、条件与将写入的目标行；不写库")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: PreviewExportInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let Some((rule, maps)) = load_rule_and_maps(&context, input.rule_id).await? else {
        return Err(BaseError::RecordNotFound("审批回写规则不存在".to_string()));
    };

    let detail = match approval::get_instance(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        input.instance_code.trim(),
    )
    .await
    {
        Ok(GetOutcome::Found(detail)) => *detail,
        Ok(GetOutcome::NotFound) => {
            return Err(BaseError::ParamInvalid(
                "instance_code".to_string(),
                "审批实例不存在".to_string(),
            ))
        }
        Err(failure) => return Err(outbound_setup::outbound_error(failure)),
    };

    let form = InstanceForm::from_detail(&detail).map_err(|error| match error {
        ExportError::Retryable(message)
        | ExportError::Fatal(message)
        | ExportError::Review(message) => BaseError::ParamInvalid(
            "instance_code".to_string(),
            format!("实例表单无法解析：{message}"),
        ),
    })?;
    if !form.is_present() {
        return Err(BaseError::ParamInvalid(
            "instance_code".to_string(),
            "审批实例表单为空，无法试算（可能尚未生成或已被清理）".to_string(),
        ));
    }
    let trigger = compute_node_passed(&rule, &detail);
    let condition = match rule.row_filter {
        RowFilter::AllRows => rule.condition.evaluate_instance(&form),
        RowFilter::MatchedRows => rule.condition.evaluate(&form),
    };

    let fields = list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &BitableCoordinates {
            app_token: rule.target_base_token.clone(),
            table_id: rule.target_table_id.clone(),
            view_id: None,
        },
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

    let mut error = None;
    let mut rows = Vec::new();
    let mut reviews = Vec::new();
    let condition_drifts: Vec<String> = condition.drifts.iter().map(ToString::to_string).collect();
    let mut drifts = condition_drifts.clone();
    match build_rows(&rule, &maps, &detail, &form, &fields) {
        Ok(plan) => {
            drifts.extend(plan.drifts.iter().map(ToString::to_string));
            rows.extend(plan.rows.iter().map(|row| {
                serde_json::json!({
                    "row_key": row.row_key,
                    "fields": row.fields,
                    "payload_hash": row.payload_hash,
                })
            }));
            reviews.extend(plan.reviews.iter().map(|review| {
                serde_json::json!({
                    "row_key": review.row_key,
                    "reason": review.reason,
                })
            }));
        }
        Err(export_error) => {
            error = Some(export_error.to_string());
        }
    }

    // 只记坐标与计数，不记任何表单值 / 目标字段值。
    tracing::debug!(
        rule_id = input.rule_id,
        instance_code = %input.instance_code.trim(),
        detail_rows = form.row_count(&rule.detail_widget_id),
        rows = rows.len(),
        reviews = reviews.len(),
        "审批回写试算完成"
    );

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "rule_id": input.rule_id,
            "rule_title": rule.title,
            "instance_code": detail.instance_code,
            "trigger": trigger_json(&trigger),
            "condition": {
                "matched": condition.matched,
                "drifts": condition_drifts,
            },
            "rows": rows,
            "reviews": reviews,
            "drifts": drifts,
            "error": error,
        }),
        if error.is_some() {
            "试算完成（配置存在需要处理的问题）"
        } else {
            "试算完成"
        },
    ))
}

fn trigger_json(outcome: &TriggerOutcome) -> Value {
    match outcome {
        TriggerOutcome::Passed { node_id } => serde_json::json!({
            "state": "passed",
            "passed": true,
            "node_id": node_id,
        }),
        TriggerOutcome::NotReached => serde_json::json!({
            "state": "waiting_node",
            "passed": false,
            "node_id": null,
        }),
        TriggerOutcome::WaitingFinal { node_id } => serde_json::json!({
            "state": "waiting_final",
            "passed": false,
            "node_id": node_id,
        }),
        TriggerOutcome::NotPassed { node_id } => serde_json::json!({
            "state": "not_passed",
            "passed": false,
            "node_id": node_id,
        }),
        TriggerOutcome::RolledBack { node_id } => serde_json::json!({
            "state": "rolled_back",
            "passed": false,
            "node_id": node_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_requires_a_rule_and_an_instance() {
        let valid = PreviewExportInput {
            rule_id: 1,
            instance_code: "INST-1".to_string(),
        };
        assert!(valid.validate().is_ok());

        assert!(PreviewExportInput {
            rule_id: 0,
            instance_code: "INST-1".to_string()
        }
        .validate()
        .is_err());
        assert!(PreviewExportInput {
            rule_id: 1,
            instance_code: "   ".to_string()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn trigger_states_are_stable_strings() {
        assert_eq!(
            trigger_json(&TriggerOutcome::Passed {
                node_id: "n1".to_string()
            })["state"],
            "passed"
        );
        assert_eq!(
            trigger_json(&TriggerOutcome::RolledBack {
                node_id: "n1".to_string()
            })["state"],
            "rolled_back"
        );
        assert_eq!(
            trigger_json(&TriggerOutcome::NotReached)["state"],
            "waiting_node"
        );
    }
}
