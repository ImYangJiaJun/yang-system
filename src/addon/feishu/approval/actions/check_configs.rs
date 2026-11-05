//! 批量检查审批派发配置：逐条按**派发链路的同一套校验**重放一遍，报告失效配置。
//!
//! # 为什么复用 `build_plan`
//!
//! 派发用的 `WidgetMap` 就是 `build_plan` 产出的那一份。另写一套「定义与库里的映射
//! 是否对得上」的比对逻辑，两套规则迟早漂移——而漂移的表现正是这次线上事故：
//! 审批定义改过之后，库里仍是旧控件 ID，直到用户点提交才被飞书 60022 拒掉。
//! 这里直接把每条配置按建配置/编辑配置的路径重算一次，失败原因原样返回。
//!
//! # 只读
//!
//! 不落库、不改配置、不发审批。唯一副作用是出站读飞书元数据（消耗应用频控配额），
//! 因此与 `list_widgets` 同归 `feishu.approval.write` 一侧。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_match::FieldMapping;
use crate::addon::feishu::domain::approval_provision::{
    build_plan, check_aggregate_batch, ProvisionError, ProvisionInput,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 检查输入：无参数（检查全部配置）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckConfigsInput {}

impl ParamInput for CheckConfigsInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 一条有问题的配置。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ProblemItem {
    config_id: i64,
    title: String,
    enabled: bool,
    /// `invalid` = 配置本身有问题（映射失效等，派发必失败）；
    /// `error` = 本轮没能完成检查（飞书/本库取数失败），配置是否有问题未知。
    status: String,
    reasons: Vec<String>,
}

/// 注册批量检查端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("check_configs"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/check")
        .display_name("检查全部审批派发配置")
        .description("逐条按派发链路的校验重放，报告审批控件 / 多维表格列映射失效的配置")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: CheckConfigsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    // 打飞书元数据：凭证缺了整轮都做不了（照 create_config / list_widgets 的处置）。
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let rows = context
        .approval_configs()
        .query()
        .order_by("id", SortOrder::Asc)?
        .all()
        .await?;
    let map_rows = context
        .approval_field_maps()
        .query()
        .select_fields(&[
            "config_id",
            "widget_id",
            "bitable_field",
            "interval_end_field",
            "option_source_key",
        ])?
        .all()
        .await?;
    let mut maps: HashMap<i64, Vec<FieldMapping>> = HashMap::new();
    for row in map_rows {
        maps.entry(row.require("config_id")?)
            .or_default()
            .push(FieldMapping {
                widget_id: row.require("widget_id")?,
                bitable_field: row.require("bitable_field")?,
                interval_end_field: row.optional("interval_end_field")?,
                option_source_key: row.optional("option_source_key")?,
            });
    }

    let total = rows.len();
    let mut problems = Vec::new();
    for row in rows {
        let config_id: i64 = row.require("id")?;
        let title: String = row.require("title")?;
        let enabled: bool = row.optional("enabled")?.unwrap_or(true);
        let base_token: String = row.require("base_token")?;
        let table_id: String = row.require("table_id")?;
        let approval_code: String = row.require("approval_code")?;
        let applicant_field: String = row.require("applicant_field")?;
        let backfill_field: String = row.require("backfill_field")?;
        let base_timezone: String = row.require("base_timezone")?;
        let status_field: Option<String> = row.optional("status_field")?;
        let flow_field: Option<String> = row.optional("flow_field")?;
        let start_time_field: Option<String> = row.optional("start_time_field")?;
        let end_time_field: Option<String> = row.optional("end_time_field")?;
        let department_field: Option<String> = row.optional("department_field")?;
        let handler_field: Option<String> = row.optional("handler_field")?;
        let batch_scope_field: Option<String> = row.optional("batch_scope_field")?;
        let aggregate_batch: bool = row.optional("aggregate_batch")?.unwrap_or(false);
        let config_maps = maps.remove(&config_id).unwrap_or_default();

        let mut reasons = check_aggregate_batch(aggregate_batch, batch_scope_field.as_deref());
        let mut status = "invalid";
        let outcome = build_plan(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &context,
            &ProvisionInput {
                base_token: &base_token,
                table_id: &table_id,
                approval_code: &approval_code,
                applicant_field: &applicant_field,
                backfill_field: &backfill_field,
                base_timezone: &base_timezone,
                maps: Some(&config_maps),
                status_field: status_field.as_deref(),
                flow_field: flow_field.as_deref(),
                start_time_field: start_time_field.as_deref(),
                end_time_field: end_time_field.as_deref(),
                department_field: department_field.as_deref(),
                handler_field: handler_field.as_deref(),
                batch_scope_field: batch_scope_field.as_deref(),
            },
        )
        .await;
        match outcome {
            Ok(_) => {}
            Err(ProvisionError::Invalid(more)) => reasons.extend(more),
            Err(ProvisionError::Fetch(message)) => {
                status = "error";
                reasons.push(format!("取飞书数据失败：{message}"));
            }
            Err(ProvisionError::Store(message)) => {
                status = "error";
                reasons.push(format!("读本库配置失败：{message}"));
            }
        }
        if reasons.is_empty() {
            continue;
        }
        problems.push(ProblemItem {
            config_id,
            title,
            enabled,
            status: status.to_string(),
            reasons,
        });
    }
    // 已启用的排前面：它们下一次派发就会失败。
    problems.sort_by_key(|problem| (!problem.enabled, problem.config_id));

    let enabled_problem_count = problems.iter().filter(|problem| problem.enabled).count();
    ApiResponse::success(
        serde_json::json!({
            "total": total,
            "ok_count": total - problems.len(),
            "enabled_problem_count": enabled_problem_count,
            "problems": problems,
        }),
        format!(
            "检查完成：共 {total} 条配置，{} 条需要处理（其中 {enabled_problem_count} 条已启用）",
            problems.len()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空请求体是唯一合法形态；`deny_unknown_fields` 挡住手误多传的键。
    #[test]
    fn empty_payload_is_accepted_and_unknown_keys_are_rejected() {
        assert!(serde_json::from_value::<CheckConfigsInput>(serde_json::json!({})).is_ok());
        assert!(
            serde_json::from_value::<CheckConfigsInput>(serde_json::json!({ "config_ids": [1] }))
                .is_err(),
            "检查全部配置，不接受子集参数（子集语义会让人误以为其余配置也被检查了）"
        );
    }

    /// 问题列表把已启用的排前面：它们下一次派发就会撞上 60022。
    #[test]
    fn enabled_problems_sort_first() {
        let mut problems = [
            ProblemItem {
                config_id: 1,
                title: "停用".into(),
                enabled: false,
                status: "invalid".into(),
                reasons: vec!["x".into()],
            },
            ProblemItem {
                config_id: 2,
                title: "启用".into(),
                enabled: true,
                status: "invalid".into(),
                reasons: vec!["y".into()],
            },
        ];
        problems.sort_by_key(|problem| (!problem.enabled, problem.config_id));
        assert_eq!(problems[0].config_id, 2);
    }
}
