//! 按批次补充展示快照；不读取记录、不派发审批。

use crate::{
    addon::feishu::domain::{
        approval::get_approval_definition,
        approval_provision::display_name,
        bitable::{get_base_name, list_all_fields, list_all_tables, BitableCoordinates},
        context::FeishuContext,
        outbound_setup,
    },
    infrastructure::audit,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use yang_base::{
    action::{ActionContext, ApiResponse},
    definition::{HttpMethod, ModuleSpec, ParamInput, Params},
    table::Record,
    BaseError,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RefreshNamesInput {
    config_ids: Vec<i64>,
}
impl ParamInput for RefreshNamesInput {
    fn params() -> Params {
        Params::new()
    }
}
impl RefreshNamesInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_ids.is_empty()
            || self.config_ids.len() > 10
            || self.config_ids.iter().any(|id| *id <= 0)
        {
            return Err(BaseError::ParamInvalid(
                "config_ids".into(),
                "每批提供1到10个正整数配置编号".into(),
            ));
        }
        Ok(())
    }
}
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("refresh_names"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/configs/refresh-names",
        )
        .display_name("补充审批配置名称")
        .description("按批次读取飞书元数据并保存展示名称，失败时保留已有名称")
        .permissions(["feishu.approval.write"])
        .register()
}
pub(super) async fn handle(
    ctx: ActionContext,
    input: RefreshNamesInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;
    let ids: HashSet<i64> = input.config_ids.into_iter().collect();
    let rows = context
        .approval_configs()
        .query()
        .where_in("id", ids.iter().map(|id| serde_json::json!(id)).collect())?
        .all()
        .await?;
    let mut bases = HashMap::new();
    let mut tables = HashMap::new();
    let mut approvals = HashMap::new();
    let mut updates = Vec::new();
    let mut incomplete_ids: Vec<i64> = ids
        .iter()
        .filter(|id| {
            !rows
                .iter()
                .any(|row| row.optional::<i64>("id").ok().flatten() == Some(**id))
        })
        .copied()
        .collect();
    for row in rows {
        let id: i64 = row.require("id")?;
        let base: String = row.require("base_token")?;
        let table: String = row.require("table_id")?;
        let code: String = row.require("approval_code")?;
        if !bases.contains_key(&base) {
            bases.insert(
                base.clone(),
                get_base_name(
                    outbound.transport(),
                    outbound.sleeper(),
                    &outbound.tokens,
                    &base,
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置多维表名称补充失败");
                    None
                }),
            );
            tables.insert(
                base.clone(),
                list_all_tables(
                    outbound.transport(),
                    outbound.sleeper(),
                    &outbound.tokens,
                    &base,
                )
                .await
                .map(Some)
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置数据表名称补充失败");
                    None
                }),
            );
        }
        if !approvals.contains_key(&code) {
            approvals.insert(
                code.clone(),
                get_approval_definition(
                    outbound.transport(),
                    outbound.sleeper(),
                    &outbound.tokens,
                    &code,
                )
                .await
                .map(|definition| Some(definition.approval_name))
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置审批名称补充失败");
                    None
                }),
            );
        }
        let fields = list_all_fields(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &BitableCoordinates {
                app_token: base.clone(),
                table_id: table.clone(),
                view_id: None,
            },
        )
        .await
        .map(Some)
        .unwrap_or_else(|error| {
            tracing::warn!(config_id = id, kind = ?error.kind, "审批配置列名称补充失败");
            None
        });
        let applicant: String = row.require("applicant_field")?;
        let backfill: String = row.require("backfill_field")?;
        let names = [
            (
                "base_name",
                bases.get(&base).and_then(|name| name.as_deref()),
            ),
            (
                "table_name",
                tables
                    .get(&base)
                    .and_then(|items| items.as_ref())
                    .and_then(|items| items.iter().find(|item| item.table_id == table))
                    .map(|item| item.name.as_str()),
            ),
            (
                "approval_name",
                approvals.get(&code).and_then(|name| name.as_deref()),
            ),
            (
                "applicant_field_name",
                fields
                    .as_ref()
                    .and_then(|items| items.iter().find(|item| item.field_id == applicant))
                    .map(|item| item.field_name.as_str()),
            ),
            (
                "backfill_field_name",
                fields
                    .as_ref()
                    .and_then(|items| items.iter().find(|item| item.field_id == backfill))
                    .map(|item| item.field_name.as_str()),
            ),
        ];
        let mut record = Record::new();
        let mut missing = false;
        for (key, name) in names {
            if let Some(name) = display_name(name) {
                record.insert(key, serde_json::json!(name));
            } else {
                missing = true;
            }
        }
        if missing {
            incomplete_ids.push(id);
        }
        if !record.as_map().is_empty() {
            updates.push((id, record));
        }
    }
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        for (id, record) in updates {
            let affected = context
                .approval_configs()
                .query()
                .where_eq("id", serde_json::json!(id))?
                .update_in_tx(&mut transaction, record)
                .await?;
            if affected == 0 {
                if !incomplete_ids.contains(&id) {
                    incomplete_ids.push(id);
                }
                continue;
            }
            let event = audit::succeeded_event(
                &ctx,
                None,
                None,
                audit::entity("feishu_approval_config", id)?,
                None,
                Some(audit::summary([(
                    "display_names_refreshed",
                    serde_json::json!(!incomplete_ids.contains(&id)),
                )])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;
    incomplete_ids.sort_unstable();
    ApiResponse::success(
        serde_json::json!({ "incomplete_ids": incomplete_ids }),
        "名称补充结束；未取得的名称保留原值",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_batch_is_bounded_and_positive() {
        for ids in [vec![], vec![0], vec![-1], vec![1; 11]] {
            assert!(RefreshNamesInput { config_ids: ids }.validate().is_err());
        }
        assert!(RefreshNamesInput {
            config_ids: (1..=10).collect()
        }
        .validate()
        .is_ok());
    }
}
