//! 切换一条字段绑定的「同名选项共用 ID」。
//!
//! # 它改的是对外身份，不是存储
//!
//! 关闭（默认）：`option_id` 把父链哈希进去，同文案不同父是不同选项。
//! 开启：读端与派发把同一文案折成 `derive::label_option_id`（根级公式），
//! 内部行、`parent_key` 与补集停用都不变，级联照旧。
//!
//! # 只允许叶子绑定
//!
//! 开启后父控件拿到的值是规范 id，而子行存的 `parent_key` 仍是父行原本的
//! option_id；两者会失配。所以有子字段的绑定直接拒绝，并点名子字段。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::field_maintenance::{
    children_of, load_bindings, load_datasource, BindingRow,
};
use crate::addon::feishu::domain::source_key::valid_source_key;
use crate::infrastructure::audit;

/// 开关输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SetOptionMergeInput {
    pub(super) datasource_id: i64,
    /// 绑定的稳定标识（不是 `field_id`）。
    pub(super) source_key: String,
    /// 开启后同一文案跨父级共用一个对外 `option_id`。
    pub(super) merge_same_label_options: bool,
}

impl ParamInput for SetOptionMergeInput {
    fn params() -> Params {
        Params::new()
    }
}

impl SetOptionMergeInput {
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

/// 注册开关端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("set_datasource_field_option_merge"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/datasources/field/option-merge",
        )
        .display_name("字段同名选项共用 ID")
        .description("开启后同一文案跨父级共用一个对外 option_id；仅限没有子字段的叶子绑定")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SetOptionMergeInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 先确认数据源存在：否则下面「没有这条绑定」会把 404 说成参数错。
    load_datasource(&context, input.datasource_id).await?;
    let bindings = load_bindings(&context, input.datasource_id).await?;
    let source_key = input.source_key.trim().to_string();
    let binding = bindings
        .iter()
        .find(|binding| binding.source_key == source_key)
        .ok_or_else(|| {
            BaseError::ParamInvalid(
                "source_key".to_string(),
                "该数据源下没有这条字段绑定".to_string(),
            )
        })?;
    if input.merge_same_label_options {
        ensure_leaf(&bindings, &binding.field_id)?;
    }
    let binding_id = binding.id;
    let field_id = binding.field_id.clone();
    let display_name = binding.display_name().to_string();

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut update = Record::new();
        update.insert(
            "merge_same_label_options",
            serde_json::json!(input.merge_same_label_options),
        );
        context
            .datasource_fields()
            .query()
            .where_eq("id", serde_json::json!(binding_id))?
            .update_in_tx(&mut transaction, update)
            .await?;

        let before = audit::summary([(
            "merge_same_label_options",
            serde_json::json!(!input.merge_same_label_options),
        )])?;
        let after = audit::summary([
            (
                "outcome_code",
                serde_json::json!(if input.merge_same_label_options {
                    "field_option_merge_enabled"
                } else {
                    "field_option_merge_disabled"
                }),
            ),
            (
                "merge_same_label_options",
                serde_json::json!(input.merge_same_label_options),
            ),
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
            "merge_same_label_options": input.merge_same_label_options,
        }),
        if input.merge_same_label_options {
            format!("字段「{display_name}」已开启同名选项共用 ID")
        } else {
            format!("字段「{display_name}」已关闭同名选项共用 ID")
        },
    )
}

/// 开启前确认没有子字段；关闭方向不检查（关掉总是安全的）。
fn ensure_leaf(bindings: &[BindingRow], field_id: &str) -> Result<(), BaseError> {
    let children = children_of(bindings, field_id);
    if children.is_empty() {
        return Ok(());
    }
    let names = children
        .iter()
        .map(|child| format!("「{}」（{}）", child.display_name(), child.field_id))
        .collect::<Vec<_>>()
        .join("、");
    Err(BaseError::ParamInvalid(
        "merge_same_label_options".to_string(),
        format!(
            "字段「{field_id}」是这些子字段的父级：{names}。\
             开启后父值会改用规范 id，子字段的 parent_key 将无法匹配；\
             请先解除子字段的父级关系。"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, field_id: &str, source_key: &str, parent: Option<&str>) -> BindingRow {
        BindingRow {
            id,
            field_id: field_id.to_string(),
            field_name: None,
            source_key: source_key.to_string(),
            parent_field_id: parent.map(str::to_string),
            enabled: true,
        }
    }

    fn input() -> SetOptionMergeInput {
        SetOptionMergeInput {
            datasource_id: 7,
            source_key: "people".to_string(),
            merge_same_label_options: true,
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

    #[test]
    fn ensure_leaf_rejects_a_parent_and_names_the_child() {
        let bindings = [
            row(1, "fldParent", "parent", None),
            row(2, "fldChild", "child", Some("fldParent")),
        ];
        let error = ensure_leaf(&bindings, "fldParent")
            .err()
            .unwrap_or_else(|| panic!("父字段必须被拒绝"));
        let message = error.to_string();
        assert!(message.contains("fldChild"), "{message}");
        assert!(
            ensure_leaf(&bindings, "fldChild").is_ok(),
            "叶子字段可以开启"
        );
    }
}
