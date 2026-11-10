//! 批量体检启用的回写规则：逐条按保存期同一套校验重放。
//!
//! 仿 `check_configs`：只读、不落库、不改规则。唯一副作用是出站读审批定义与
//! 目标表字段（消耗应用频控配额），所以归 `feishu.approval.write` 一侧。
//!
//! `status=invalid` 表示配置本身已失效（运行必失败）；`status=error` 表示本轮
//! 没取到飞书数据（网络 / 权限），规则是否有问题未知——两者处置不同。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_export::config::{
    stored_draft, validate_export_rule, ExportConfigError, FIELD_MAP_FIELDS, RULE_FIELDS,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::repository::all_pages;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckExportRulesInput {}

impl ParamInput for CheckExportRulesInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ExportRuleProblem {
    rule_id: i64,
    title: String,
    /// `invalid` = 配置本身失效；`error` = 本轮飞书取数失败，结果未知。
    status: String,
    reasons: Vec<String>,
}

/// 注册批量体检端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_check_rules"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/rules/check",
        )
        .display_name("检查全部启用的回写规则")
        .description("按保存期同一套校验重放启用规则，报告定义控件 / 目标列 / 状态列漂移")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: CheckExportRulesInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    // 启用的规则可能多于默认一页（10 条）；体检必须覆盖全部，不能静默漏检。
    let rows = all_pages(
        context
            .approval_export_rules()
            .query()
            .where_eq("enabled", serde_json::json!(true))?
            .select_fields(RULE_FIELDS)?
            .order_by("id", SortOrder::Asc)?,
        10,
    )
    .await?;

    let ids: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| Ok(serde_json::json!(row.require::<i64>("id")?)))
        .collect::<Result<Vec<_>, BaseError>>()?;
    let mut maps_by_rule: HashMap<i64, Vec<Record>> = HashMap::new();
    if !ids.is_empty() {
        for map_row in all_pages(
            context
                .approval_export_field_maps()
                .query()
                .select_fields(FIELD_MAP_FIELDS)?
                .where_in("rule_id", ids)?,
            20,
        )
        .await?
        {
            maps_by_rule
                .entry(map_row.require("rule_id")?)
                .or_default()
                .push(map_row);
        }
    }

    let total = rows.len();
    let mut problems = Vec::new();
    for row in &rows {
        let rule_id: i64 = row.require("id")?;
        let title: String = row.optional("title")?.unwrap_or_default();
        let map_rows = maps_by_rule.remove(&rule_id).unwrap_or_default();
        let draft = match stored_draft(row, &map_rows) {
            Ok(draft) => draft,
            Err(reason) => {
                problems.push(ExportRuleProblem {
                    rule_id,
                    title,
                    status: "invalid".to_string(),
                    reasons: vec![format!("库内规则无法解析：{reason}")],
                });
                continue;
            }
        };
        match validate_export_rule(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &draft,
        )
        .await
        {
            Ok(_) => {}
            Err(ExportConfigError::Invalid(reasons)) => {
                problems.push(ExportRuleProblem {
                    rule_id,
                    title,
                    status: "invalid".to_string(),
                    reasons,
                });
            }
            Err(ExportConfigError::Fetch(message)) => {
                problems.push(ExportRuleProblem {
                    rule_id,
                    title,
                    status: "error".to_string(),
                    reasons: vec![format!("取飞书数据失败：{message}")],
                });
            }
        }
    }
    problems.sort_by_key(|problem| problem.rule_id);

    let mut by_status: BTreeMap<&str, usize> = BTreeMap::new();
    for problem in &problems {
        *by_status.entry(problem.status.as_str()).or_default() += 1;
    }

    ApiResponse::success(
        serde_json::json!({
            "total": total,
            "ok_count": total - problems.len(),
            "problem_count": problems.len(),
            "invalid_count": by_status.get("invalid").copied().unwrap_or(0),
            "error_count": by_status.get("error").copied().unwrap_or(0),
            "problems": problems,
        }),
        format!(
            "检查完成：共 {total} 条启用规则，{} 条需要处理",
            problems.len()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload_is_the_only_shape() {
        assert!(serde_json::from_value::<CheckExportRulesInput>(serde_json::json!({})).is_ok());
        assert!(
            serde_json::from_value::<CheckExportRulesInput>(serde_json::json!({
                "rule_ids": [1]
            }))
            .is_err(),
            "检查全部启用规则，不接受子集参数"
        );
    }

    #[test]
    fn problem_counts_split_invalid_and_error() {
        let problems = [
            ExportRuleProblem {
                rule_id: 1,
                title: "a".into(),
                status: "invalid".into(),
                reasons: vec!["x".into()],
            },
            ExportRuleProblem {
                rule_id: 2,
                title: "b".into(),
                status: "error".into(),
                reasons: vec!["y".into()],
            },
        ];
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for problem in &problems {
            *counts.entry(problem.status.as_str()).or_default() += 1;
        }
        assert_eq!(counts.get("invalid"), Some(&1));
        assert_eq!(counts.get("error"), Some(&1));
    }
}
