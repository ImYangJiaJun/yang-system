//! 更新表级数据源：整份替换它的字段绑定集合。
//!
//! # 「替换」的三条语义（都被测试钉着）
//!
//! | 情形 | 处理 | 为什么 |
//! |---|---|---|
//! | 勾选的列，库里没有 | **新增**，并生成一份新凭据 | 新列需要新 URL 与新 Token |
//! | 勾选的列，库里已有 | **更新**（只改父指针、重新启用） | **保住 `source_key` 与凭据**——否则飞书侧已配的 URL 会失效 |
//! | 没勾的列，库里已有 | **停用**，不删行 | 删掉会让控件吃 `SOURCE_NOT_FOUND`，历史审批单引用的 `option_id` 也会失联 |
//!
//! 第三条是刻意的：停用是**可归因、可逆**的状态（`SOURCE_DISABLED`），
//! 而且重新勾选时会走「更新」把同一行启用回来，`source_key` 不变。

use std::sync::Arc;

use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::validate_path_segment;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::{generate_token, seal};
use crate::addon::feishu::domain::field_binding::{validate_fields, FieldBindingInput};
use crate::infrastructure::audit;

/// 库里已有的一条绑定。
///
/// 只要 `id` 与 `field_id`：更新时**不能碰**它的 `source_key` 与凭据，
/// 所以那两个字段根本不取出来——取出来才有被误写的风险。
pub(super) struct ExistingBinding {
    pub(super) id: i64,
    pub(super) field_id: String,
}

/// 绑定集合的差异。
pub(super) struct BindingDiff<'a> {
    /// 要新增的绑定（需要新凭据）。
    pub(super) to_insert: Vec<&'a FieldBindingInput>,
    /// 要更新的已有绑定：`(绑定行 id, 新值)`。
    pub(super) to_update: Vec<(i64, &'a FieldBindingInput)>,
    /// 要停用的绑定行 id。
    pub(super) to_disable: Vec<i64>,
}

/// 算出「这份勾选集合」相对「库里已有什么」的差异。
///
/// 抽成纯函数是为了可测：这三条分支的失效形态是**静默的**——
/// 该更新的走了插入，会让 `source_key` 漂移、飞书侧控件全断，而接口照样返回成功。
pub(super) fn diff_bindings<'a>(
    incoming: &'a [FieldBindingInput],
    existing: &[ExistingBinding],
) -> BindingDiff<'a> {
    let mut diff = BindingDiff {
        to_insert: Vec::new(),
        to_update: Vec::new(),
        to_disable: Vec::new(),
    };

    for field in incoming {
        let target = field.field_id.trim();
        match existing
            .iter()
            .find(|stored| stored.field_id.trim() == target)
        {
            // 已存在 → 更新。**这是保住 source_key 的唯一机会**。
            Some(stored) => diff.to_update.push((stored.id, field)),
            None => diff.to_insert.push(field),
        }
    }

    for stored in existing {
        if !incoming
            .iter()
            .any(|field| field.field_id.trim() == stored.field_id.trim())
        {
            diff.to_disable.push(stored.id);
        }
    }
    diff
}

/// 更新表级数据源的输入契约。
///
/// `datasource_id` 走 **body** 而不是路径段：本模块所有既有 Action
/// （`update_datasource` / `delete_datasource` / `pull_now`）都用 body 传标识，
/// 且 `fields` 是复杂结构，与 `params!` 的路径参数无法共用一套声明。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateTableInput {
    /// 目标数据源的 `id`。
    pub(super) datasource_id: i64,
    /// 展示名；省略即不改。
    #[serde(default)]
    pub(super) title: Option<String>,
    /// 取数方式；省略即不改。
    #[serde(default)]
    pub(super) ingest_mode: Option<String>,
    /// 多维表格 app_token；省略即不改。**给空串表示清空该坐标。**
    #[serde(default)]
    pub(super) bitable_base_token: Option<String>,
    #[serde(default)]
    pub(super) bitable_table_id: Option<String>,
    #[serde(default)]
    pub(super) bitable_view_id: Option<String>,
    /// 期望的字段绑定集合（整份替换）。
    pub(super) fields: Vec<FieldBindingInput>,
}

impl ParamInput for UpdateTableInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpdateTableInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if let Some(title) = self.title.as_deref() {
            if title.trim().is_empty() || title.chars().count() > 100 {
                return Err(BaseError::ParamInvalid(
                    "title".to_string(),
                    "名称必须在 1..=100 字符".to_string(),
                ));
            }
        }
        if let Some(mode) = self.ingest_mode.as_deref() {
            if !matches!(mode, "push" | "pull") {
                return Err(BaseError::ParamInvalid(
                    "ingest_mode".to_string(),
                    "取数方式只能是 push 或 pull".to_string(),
                ));
            }
        }
        for (name, value) in [
            ("bitable_base_token", self.bitable_base_token.as_deref()),
            ("bitable_table_id", self.bitable_table_id.as_deref()),
            ("bitable_view_id", self.bitable_view_id.as_deref()),
        ] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                validate_path_segment(name, value).map_err(|error| {
                    BaseError::ParamInvalid(name.to_string(), error.to_string())
                })?;
            }
        }
        // 与创建路径同一套规则：漂移只在运行期暴露。
        validate_fields(&self.fields)
    }
}

/// 注册更新表级数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("update_datasource_table"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Put, "/api/v1/feishu/datasources/table")
        .display_name("更新表级数据源")
        .description("整份替换字段绑定集合；已有绑定保留其 source_key 与凭据")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateTableInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 新增的绑定要新凭据，所以这里同样得有封装密钥。
    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：系统生成的凭据需要它来封存，否则无法回显".to_string(),
        )
    })?;

    let mut transaction = ctx.begin_transaction().await?;

    let result: Result<(usize, usize, usize), BaseError> = async {
        // 0) 数据源必须存在。不存在就明确 404，而不是「更新了 0 行」的静默成功。
        let exists = context
            .datasources()
            .query()
            .select_fields(&["id"])?
            .where_eq("id", serde_json::json!(input.datasource_id))?
            .optional()
            .await?;
        if exists.is_none() {
            return Err(BaseError::RecordNotFound("数据源不存在".to_string()));
        }

        // 1) 读现有绑定（只取更新需要的两列）。
        let rows = context
            .datasource_fields()
            .query()
            .select_fields(&["id", "field_id"])?
            .where_eq("datasource_id", serde_json::json!(input.datasource_id))?
            .all()
            .await?;
        let existing: Vec<ExistingBinding> = rows
            .iter()
            .map(|record| {
                Ok(ExistingBinding {
                    id: record.require("id")?,
                    field_id: record.require("field_id")?,
                })
            })
            .collect::<Result<_, BaseError>>()?;

        let diff = diff_bindings(&input.fields, &existing);

        // 2) 表级行：只为**提供了**的字段写值（省略即保持）。
        let mut row = Record::new();
        for (column, value) in [
            ("title", input.title.as_deref()),
            ("ingest_mode", input.ingest_mode.as_deref()),
            ("bitable_base_token", input.bitable_base_token.as_deref()),
            ("bitable_table_id", input.bitable_table_id.as_deref()),
            ("bitable_view_id", input.bitable_view_id.as_deref()),
        ] {
            if let Some(value) = value {
                row.insert(column, serde_json::json!(value.trim()));
            }
        }
        if !row.as_map().is_empty() {
            context
                .datasources()
                .query()
                .where_eq("id", serde_json::json!(input.datasource_id))?
                .update_in_tx(&mut transaction, row)
                .await?;
        }

        // 3) 新增：新列需要新凭据。
        for field in &diff.to_insert {
            let plaintext = generate_token();
            let (token_hash, token_cipher) = seal(&plaintext, &wrapping_key)?;
            let mut binding = Record::new();
            binding.insert("datasource_id", serde_json::json!(input.datasource_id));
            binding.insert("field_id", serde_json::json!(field.field_id.trim()));
            binding.insert("source_key", serde_json::json!(field.source_key.trim()));
            binding.insert("token_hash", serde_json::json!(token_hash));
            binding.insert("token_cipher", serde_json::json!(token_cipher));
            binding.insert("enabled", serde_json::json!(true));
            if let Some(parent) = field
                .parent_field_id
                .as_deref()
                .map(str::trim)
                .filter(|parent| !parent.is_empty())
            {
                binding.insert("parent_field_id", serde_json::json!(parent));
            }
            context
                .datasource_fields()
                .query()
                .insert_in_tx(&mut transaction, binding)
                .await?;
        }

        // 4) 更新：**只动父指针与启用位**。`source_key`、`token_hash`、`token_cipher`
        //    一律不碰——碰了就等于换 URL，飞书侧已配的控件全断。
        for (binding_id, field) in &diff.to_update {
            let mut update = Record::new();
            update.insert("enabled", serde_json::json!(true));
            update.insert(
                "parent_field_id",
                match field
                    .parent_field_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|parent| !parent.is_empty())
                {
                    Some(parent) => serde_json::json!(parent),
                    None => serde_json::Value::Null,
                },
            );
            context
                .datasource_fields()
                .query()
                .where_eq("id", serde_json::json!(binding_id))?
                .update_in_tx(&mut transaction, update)
                .await?;
        }

        // 5) 停用：不删行、不动选项。
        for binding_id in &diff.to_disable {
            let mut update = Record::new();
            update.insert("enabled", serde_json::json!(false));
            context
                .datasource_fields()
                .query()
                .where_eq("id", serde_json::json!(binding_id))?
                .update_in_tx(&mut transaction, update)
                .await?;
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", input.datasource_id.to_string())?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("updated_table")),
                ("inserted", serde_json::json!(diff.to_insert.len())),
                ("disabled", serde_json::json!(diff.to_disable.len())),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok((
            diff.to_insert.len(),
            diff.to_update.len(),
            diff.to_disable.len(),
        ))
    }
    .await;

    let (inserted, updated, disabled) =
        FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "inserted": inserted,
            "updated": updated,
            "disabled": disabled,
        }),
        "更新成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(field_id: &str, source_key: &str, parent: Option<&str>) -> FieldBindingInput {
        FieldBindingInput {
            field_id: field_id.to_string(),
            source_key: source_key.to_string(),
            parent_field_id: parent.map(str::to_string),
        }
    }

    fn input_with(fields: Vec<FieldBindingInput>) -> UpdateTableInput {
        UpdateTableInput {
            datasource_id: 7,
            title: None,
            ingest_mode: None,
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            fields,
        }
    }

    /// 库里已有的某条绑定。
    fn existing(id: i64, field_id: &str) -> ExistingBinding {
        ExistingBinding {
            id,
            field_id: field_id.to_string(),
        }
    }

    #[test]
    fn an_unchanged_set_neither_inserts_nor_disables() {
        let incoming = vec![field("fldA", "a", None), field("fldB", "b", Some("fldA"))];
        let stored = vec![existing(1, "fldA"), existing(2, "fldB")];
        let diff = diff_bindings(&incoming, &stored);
        assert!(diff.to_insert.is_empty());
        assert!(diff.to_disable.is_empty());
        assert_eq!(diff.to_update.len(), 2, "两条都走更新（父指针可能变）");
    }

    #[test]
    fn a_newly_checked_field_is_inserted() {
        let incoming = vec![field("fldA", "a", None), field("fldC", "c", None)];
        let stored = vec![existing(1, "fldA")];
        let diff = diff_bindings(&incoming, &stored);
        assert_eq!(diff.to_insert.len(), 1);
        assert_eq!(diff.to_insert[0].field_id, "fldC");
    }

    #[test]
    fn an_unchecked_field_is_disabled_not_deleted() {
        // 删掉会让飞书侧已配的控件在下次请求时吃 SOURCE_NOT_FOUND，
        // 历史审批单引用的 option_id 也会失联。停用是可归因、可逆的状态。
        let incoming = vec![field("fldA", "a", None)];
        let stored = vec![existing(1, "fldA"), existing(2, "fldB")];
        let diff = diff_bindings(&incoming, &stored);
        assert_eq!(diff.to_disable, vec![2]);
        assert!(diff.to_insert.is_empty(), "停用不是删除");
    }

    #[test]
    fn re_checking_a_previously_removed_field_updates_instead_of_reinserting() {
        // 这是本函数存在的**主要理由**：重新勾选必须走「更新」，
        // 才能保住它的 source_key 与凭据——否则飞书侧已配的 URL 会失效。
        let incoming = vec![field("fldB", "b", None)];
        let stored = vec![existing(7, "fldB")];
        let diff = diff_bindings(&incoming, &stored);
        assert!(diff.to_insert.is_empty(), "不得重新插入");
        assert!(diff.to_disable.is_empty());
        assert_eq!(diff.to_update.len(), 1);
        assert_eq!(diff.to_update[0].0, 7, "更新落在已有那一行上");
    }

    #[test]
    fn update_reuses_create_validation_rules() {
        // 同一套校验函数，不允许两条路径规则不同——漂移只在运行期暴露
        let mut input = input_with(vec![field("fldA", "a", None), field("fldB", "b", None)]);
        input.fields[1].parent_field_id = Some("fldMissing".to_string());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_zero_datasource_id() {
        let mut input = input_with(vec![field("fldA", "a", None)]);
        input.datasource_id = 0;
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_empty_field_list() {
        let input = input_with(vec![]);
        assert!(
            input.validate().is_err(),
            "取消勾选全部要走删除，不是更新成空"
        );
    }
}
