//! 原地改绑一条字段绑定：换 `field_id`，保留绑定行、`source_key` 与 Token。
//!
//! # 为什么不能用 `update_datasource_table`
//!
//! 那个端点按 `field_id` 匹配已有绑定；把绑定改指到另一个字段会被判成
//! 「新增一条绑定 + 停用旧绑定」，`source_key` 与 Token 都会变，飞书审批后台里
//! 已经配好的 URL/Token 立刻失效。本端点按 `source_key` 定位，原地更新同一行。
//!
//! # 父级保护（决策 D4）
//!
//! 如果这条绑定是其他绑定的父字段，改绑会让子绑定的 `parent_field_id` 指向一个
//! 不存在的字段，下一轮拉取整棵子树暂停。所以先拦住并点名子字段，要求先改指或
//! 解除子级。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::field_maintenance::{
    affected_approval_configs, children_of, load_bindings, load_datasource, load_remote_fields,
    require_pull_coordinates, sync_portal_field_references, validate_ancestor_fields,
    validate_enabled_graph, validate_parent_for_change, validate_target_field,
};
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::source_key::valid_source_key;
use crate::infrastructure::audit;

/// 改绑输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RepointFieldInput {
    /// 目标数据源的 `id`。
    pub(super) datasource_id: i64,
    /// 绑定的稳定标识。**不是** `field_id`：后者正是本次要换的东西。
    pub(super) source_key: String,
    /// 新的多维表格字段 ID。
    pub(super) field_id: String,
    /// 新的父字段 `field_id`。
    ///
    /// **省略 = 保持原父指针；显式 `null` = 解除父级；给值 = 改指**。
    /// 三态必须区分：如果省略被当成「无父」，只想换 `field_id` 的调用方会
    /// 静默拆掉整条级联。
    #[serde(default, deserialize_with = "deserialize_some")]
    pub(super) parent_field_id: Option<Option<String>>,
}

/// 把 `null` 反序列化成 `Some(None)`，与「键缺失」的 `None` 区分开。
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl ParamInput for RepointFieldInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RepointFieldInput {
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
        if self.field_id.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                "字段 ID 不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册改绑端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("repoint_datasource_field"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/field/repoint")
        .display_name("改绑字段")
        .description("按 source_key 原地改 field_id；保留绑定行、source_key 与 Token")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RepointFieldInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let datasource = load_datasource(&context, input.datasource_id).await?;
    let coordinates = require_pull_coordinates(&datasource)?;
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

    let binding_id = bindings[index].id;
    let old_field_id = bindings[index].field_id.clone();
    let old_field_name = bindings[index].field_name.clone();
    let old_parent = bindings[index].parent_field_id.clone();
    let new_field_id = input.field_id.trim().to_string();
    let new_parent = match &input.parent_field_id {
        // 省略：保持原父指针。
        None => old_parent.clone(),
        // 显式 null：解除父级。
        Some(None) => None,
        Some(Some(parent)) => Some(parent.trim().to_string()).filter(|parent| !parent.is_empty()),
    };

    // D4：父字段改绑前必须先处理直接子级（含停用子级，避免留下失效父指针）。
    if old_field_id != new_field_id {
        let children = children_of(&bindings, &old_field_id);
        if !children.is_empty() {
            let names = children
                .iter()
                .map(|child| format!("「{}」（{}）", child.display_name(), child.field_id))
                .collect::<Vec<_>>()
                .join("、");
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                format!(
                    "字段「{}」是这些子字段的父级：{names}。\
                     请先把子字段改指到其他父字段或解除父级，再改绑。",
                    bindings[index].display_name()
                ),
            ));
        }
    }

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

    let remote =
        validate_target_field(&remote_fields, &new_field_id, &bindings, Some(&source_key))?;
    let new_field_name = remote.field_name.trim().to_string();
    validate_parent_for_change(&bindings, &new_field_id, new_parent.as_deref())?;
    validate_ancestor_fields(&remote_fields, &bindings, new_parent.as_deref())?;

    bindings[index].field_id = new_field_id.clone();
    bindings[index].field_name = Some(new_field_name.clone());
    bindings[index].parent_field_id = new_parent.clone();
    validate_enabled_graph(&bindings)?;

    let affected = affected_approval_configs(&context, &source_key).await?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<u64, BaseError> = async {
        let mut update = Record::new();
        update.insert("field_id", serde_json::json!(new_field_id));
        update.insert("field_name", serde_json::json!(new_field_name));
        update.insert(
            "parent_field_id",
            match &new_parent {
                Some(parent) => serde_json::json!(parent),
                None => serde_json::Value::Null,
            },
        );
        // 换字段后旧摘要不再可信；清空强制下一轮重新派生，旧选项由补集停用。
        update.insert("snapshot_digest", serde_json::Value::Null);
        context
            .datasource_fields()
            .query()
            .where_eq("id", serde_json::json!(binding_id))?
            .update_in_tx(&mut transaction, update)
            .await?;

        let updated_portals = sync_portal_field_references(
            &context,
            &mut transaction,
            input.datasource_id,
            &old_field_id,
            &new_field_id,
        )
        .await?;

        let before = audit::summary([
            ("source_key", serde_json::json!(&source_key)),
            ("field_id", serde_json::json!(&old_field_id)),
            ("field_name", serde_json::json!(&old_field_name)),
            ("parent_field_id", serde_json::json!(&old_parent)),
        ])?;
        let after = audit::summary([
            ("outcome_code", serde_json::json!("field_repointed")),
            ("source_key", serde_json::json!(&source_key)),
            ("field_id", serde_json::json!(&new_field_id)),
            ("field_name", serde_json::json!(&new_field_name)),
            ("parent_field_id", serde_json::json!(&new_parent)),
            ("updated_portals", serde_json::json!(updated_portals)),
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

        Ok(updated_portals)
    }
    .await;

    let updated_portals = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "source_key": source_key,
            "field_id": new_field_id,
            "field_name": new_field_name,
            "updated_portals": updated_portals,
            "affected_approval_configs": affected,
        }),
        "改绑成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> RepointFieldInput {
        RepointFieldInput {
            datasource_id: 7,
            source_key: "currency".to_string(),
            field_id: "fldA".to_string(),
            parent_field_id: None,
        }
    }

    #[test]
    fn rejects_bad_ids_and_blank_field_ids() {
        let mut bad = input();
        bad.datasource_id = 0;
        assert!(bad.validate().is_err());

        let mut bad = input();
        bad.source_key = "Bad-Key".to_string();
        assert!(bad.validate().is_err());

        let mut bad = input();
        bad.field_id = "  ".to_string();
        assert!(bad.validate().is_err());

        assert!(input().validate().is_ok());
    }

    #[test]
    fn a_missing_parent_keeps_it_while_an_explicit_null_clears_it() {
        let missing: RepointFieldInput = serde_json::from_value(serde_json::json!({
            "datasource_id": 7,
            "source_key": "currency",
            "field_id": "fldA",
        }))
        .unwrap_or_else(|error| panic!("应可反序列化: {error}"));
        assert_eq!(missing.parent_field_id, None, "省略 = 保持原父指针");

        let cleared: RepointFieldInput = serde_json::from_value(serde_json::json!({
            "datasource_id": 7,
            "source_key": "currency",
            "field_id": "fldA",
            "parent_field_id": null,
        }))
        .unwrap_or_else(|error| panic!("应可反序列化: {error}"));
        assert_eq!(cleared.parent_field_id, Some(None), "显式 null = 解除父级");

        let changed: RepointFieldInput = serde_json::from_value(serde_json::json!({
            "datasource_id": 7,
            "source_key": "currency",
            "field_id": "fldA",
            "parent_field_id": "fldB",
        }))
        .unwrap_or_else(|error| panic!("应可反序列化: {error}"));
        assert_eq!(changed.parent_field_id, Some(Some("fldB".to_string())));
    }
}
