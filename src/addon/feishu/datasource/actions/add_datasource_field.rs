//! 给已有数据源新增一条字段绑定。
//!
//! # 为什么不是「自动发现即绑定」
//!
//! 每条新绑定都意味着一个新的 `source_key` + 新 Token + 审批后台的一个新控件。
//! 系统无法替业务人员把 URL/Token 粘进审批后台，自动创建只会留下一堆没人使用的
//! 凭据。所以这里要求人工勾选，创建后把 Token 明文返回一次，供立即复制。
//!
//! # 存量绑定不动
//!
//! 新增只插入一行；已有绑定的 `source_key`、Token、选项行都不碰。

use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::issue_token;
use crate::addon::feishu::domain::field_maintenance::{
    fold_duplicate_source_key, load_bindings, load_datasource, load_remote_fields,
    require_pull_coordinates, source_key_from_table_and_field, unique_source_key,
    validate_ancestor_fields, validate_enabled_graph, validate_parent_for_change,
    validate_source_key, validate_target_field, BindingRow,
};
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

/// 新增字段绑定输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AddFieldInput {
    pub(super) datasource_id: i64,
    /// 要新增的多维表格字段 ID。
    pub(super) field_id: String,
    /// 数据源标识；省略时按 `table_id_field_id` 派生并自动避让冲突。
    #[serde(default)]
    pub(super) source_key: Option<String>,
    /// 同表内的父字段 `field_id`；省略或 `null` 表示无父。
    #[serde(default)]
    pub(super) parent_field_id: Option<String>,
}

impl ParamInput for AddFieldInput {
    fn params() -> Params {
        Params::new()
    }
}

impl AddFieldInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.field_id.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                "字段 ID 不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册新增字段端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("add_datasource_field"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/field/add")
        .display_name("新增字段绑定")
        .description("为已有数据源新增一条绑定，生成新 source_key 与 Token；存量绑定不动")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AddFieldInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let datasource = load_datasource(&context, input.datasource_id).await?;
    let coordinates = require_pull_coordinates(&datasource)?;
    let bindings = load_bindings(&context, input.datasource_id).await?;
    let field_id = input.field_id.trim().to_string();
    let requested_source_key = input
        .source_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let new_parent = input
        .parent_field_id
        .as_deref()
        .map(str::trim)
        .filter(|parent| !parent.is_empty())
        .map(str::to_string);

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

    let remote = validate_target_field(&remote_fields, &field_id, &bindings, None)?;
    let remote_name = remote.field_name.trim().to_string();
    validate_parent_for_change(&bindings, &field_id, new_parent.as_deref())?;
    validate_ancestor_fields(&remote_fields, &bindings, new_parent.as_deref())?;

    let occupied: HashSet<String> = bindings
        .iter()
        .map(|binding| binding.source_key.clone())
        .collect();
    let source_key = match requested_source_key {
        Some(value) => validate_source_key(value, &occupied)?,
        None => {
            let base = source_key_from_table_and_field(&coordinates.table_id, &field_id);
            unique_source_key(&base, &occupied).ok_or_else(|| {
                BaseError::ParamInvalid(
                    "source_key".to_string(),
                    "无法生成可用的数据源标识，请手动指定一个 source_key".to_string(),
                )
            })?
        }
    };

    // 先把新绑定放进提议图里验证，再开事务：父链成环/父不可用在这里就会被拒。
    let mut proposed = bindings.clone();
    proposed.push(BindingRow {
        id: 0,
        field_id: field_id.clone(),
        field_name: Some(remote_name.clone()),
        source_key: source_key.clone(),
        parent_field_id: new_parent.clone(),
        enabled: true,
    });
    validate_enabled_graph(&proposed)?;

    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：系统生成的凭据需要它来封存，否则无法回显".to_string(),
        )
    })?;
    let credential = issue_token(&wrapping_key)?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut binding = Record::new();
        binding.insert("datasource_id", serde_json::json!(input.datasource_id));
        binding.insert("field_id", serde_json::json!(&field_id));
        binding.insert("field_name", serde_json::json!(&remote_name));
        binding.insert("source_key", serde_json::json!(&source_key));
        binding.insert("token_hash", serde_json::json!(&credential.hash));
        binding.insert("token_cipher", serde_json::json!(&credential.cipher));
        binding.insert("enabled", serde_json::json!(true));
        if let Some(parent) = &new_parent {
            binding.insert("parent_field_id", serde_json::json!(parent));
        }
        context
            .datasource_fields()
            .query()
            .insert_in_tx(&mut transaction, binding)
            .await
            .map_err(fold_duplicate_source_key)?;

        let after = audit::summary([
            ("outcome_code", serde_json::json!("field_added")),
            ("source_key", serde_json::json!(&source_key)),
            ("field_id", serde_json::json!(&field_id)),
            ("field_name", serde_json::json!(&remote_name)),
            ("parent_field_id", serde_json::json!(&new_parent)),
        ])?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", input.datasource_id.to_string())?,
            None,
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
            "field_id": field_id,
            "field_name": remote_name,
            "token": credential.plaintext,
        }),
        "字段已新增",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AddFieldInput {
        AddFieldInput {
            datasource_id: 7,
            field_id: "fldA".to_string(),
            source_key: None,
            parent_field_id: None,
        }
    }

    #[test]
    fn rejects_bad_ids_and_blank_field_ids() {
        let mut bad = input();
        bad.datasource_id = 0;
        assert!(bad.validate().is_err());

        let mut bad = input();
        bad.field_id = "  ".to_string();
        assert!(bad.validate().is_err());

        assert!(input().validate().is_ok());
    }
}
