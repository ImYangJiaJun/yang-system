//! 停用/启用一条字段绑定。
//!
//! # 停用 = 非破坏性移除
//!
//! 保留绑定行、`source_key`、Token 与历史选项；出站立即返回 `40301 数据源已停用`，
//! 重新启用即可恢复。物理删除只发生在删除整个数据源。
//!
//! # 父级保护（决策 D4）
//!
//! 停用父字段会让子绑定的 `parent_field_id` 指向一个不在启用集合里的字段，
//! 下一轮拉取整棵子树暂停。所以先拦住并点名子字段。
//!
//! # 启用前校验
//!
//! 远端字段必须仍然存在、名字唯一、类型可取值，父字段必须启用。否则一启用就会
//! 把整棵子树重新拖进坏状态。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::field_maintenance::{
    children_of, load_bindings, load_datasource, load_remote_fields, require_pull_coordinates,
    validate_ancestor_fields, validate_enabled_graph, validate_parent_for_change,
    validate_target_field,
};
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::source_key::valid_source_key;
use crate::infrastructure::audit;

/// 停用/启用输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SetFieldEnabledInput {
    pub(super) datasource_id: i64,
    /// 绑定的稳定标识（不是 `field_id`）。
    pub(super) source_key: String,
    pub(super) enabled: bool,
}

impl ParamInput for SetFieldEnabledInput {
    fn params() -> Params {
        Params::new()
    }
}

impl SetFieldEnabledInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if !valid_source_key(self.source_key.trim()) {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册停用/启用端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("set_datasource_field_enabled"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/field/status")
        .display_name("停用/启用字段")
        .description("停用保留绑定行、source_key 与 Token；启用前校验远端字段仍可用")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SetFieldEnabledInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let datasource = load_datasource(&context, input.datasource_id).await?;
    let mut bindings = load_bindings(&context, input.datasource_id).await?;
    let source_key = input.source_key.trim().to_string();
    let index = bindings
        .iter()
        .position(|binding| binding.source_key == source_key)
        .ok_or_else(|| {
            BaseError::ParamInvalid(
                "source_key".to_string(),
                "该数据源下没有这条字段绑定".to_string(),
            )
        })?;

    if bindings[index].enabled == input.enabled {
        return ApiResponse::success(
            serde_json::json!({
                "source_key": source_key,
                "enabled": input.enabled,
                "changed": false,
            }),
            "状态未变化",
        );
    }

    let binding_id = bindings[index].id;
    let field_id = bindings[index].field_id.clone();
    let parent_field_id = bindings[index].parent_field_id.clone();

    if !input.enabled {
        // D4：停用父字段前必须先处理直接子级（含停用子级）。
        let children = children_of(&bindings, &field_id);
        if !children.is_empty() {
            let names = children
                .iter()
                .map(|child| format!("「{}」（{}）", child.display_name(), child.field_id))
                .collect::<Vec<_>>()
                .join("、");
            return Err(BaseError::ParamInvalid(
                "enabled".to_string(),
                format!(
                    "字段「{}」是这些子字段的父级：{names}。\
                     请先把子字段改指到其他父字段或解除父级，再停用。",
                    bindings[index].display_name()
                ),
            ));
        }
    } else {
        // 启用前必须确认远端字段仍可用，否则一启用就把坏状态带回来。
        let coordinates = require_pull_coordinates(&datasource)?;
        let settings = match outbound_setup::require_settings(&context) {
            Ok(settings) => settings,
            Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
        };
        let outbound = outbound_setup::build(&ctx, settings)?;
        let remote_fields = load_remote_fields(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &coordinates,
        )
        .await?;
        validate_target_field(&remote_fields, &field_id, &bindings, Some(&source_key))?;
        validate_parent_for_change(&bindings, &field_id, parent_field_id.as_deref())?;
        validate_ancestor_fields(&remote_fields, &bindings, parent_field_id.as_deref())?;
    }

    bindings[index].enabled = input.enabled;
    validate_enabled_graph(&bindings)?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut update = Record::new();
        update.insert("enabled", serde_json::json!(input.enabled));
        context
            .datasource_fields()
            .query()
            .where_eq("id", serde_json::json!(binding_id))?
            .update_in_tx(&mut transaction, update)
            .await?;

        let before = audit::summary([("enabled", serde_json::json!(!input.enabled))])?;
        let after = audit::summary([
            (
                "outcome_code",
                serde_json::json!(if input.enabled {
                    "field_enabled"
                } else {
                    "field_disabled"
                }),
            ),
            ("enabled", serde_json::json!(input.enabled)),
            ("source_key", serde_json::json!(&source_key)),
            ("field_id", serde_json::json!(&field_id)),
        ])?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", input.datasource_id.to_string())?,
            Some(before),
            Some(after),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "source_key": source_key,
            "enabled": input.enabled,
            "changed": true,
        }),
        if input.enabled {
            "字段已启用"
        } else {
            "字段已停用"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SetFieldEnabledInput {
        SetFieldEnabledInput {
            datasource_id: 7,
            source_key: "currency".to_string(),
            enabled: false,
        }
    }

    #[test]
    fn rejects_bad_ids_and_source_keys() {
        let mut bad = input();
        bad.datasource_id = 0;
        assert!(bad.validate().is_err());

        let mut bad = input();
        bad.source_key = "Bad-Key".to_string();
        assert!(bad.validate().is_err());

        assert!(input().validate().is_ok());
    }
}
