//! 创建表级数据源：一次写入表级行 + N 条字段绑定（同一事务）。
//!
//! # 为什么是一个事务
//!
//! 表级行与它的绑定行在语义上是一个整体：只有表级行会得到一张没有字段的空壳，
//! 只有绑定行则指向一个不存在的父。半写状态没有任何一种是有意义的。
//!
//! # 凭据在这里生成
//!
//! `source_key` 由运维给（向导按 `field_id` 派生默认值），Token 由**系统生成**并
//! 在同一事务里封存（摘要 + 密文）。明文只在响应里出现一次——但它**可以再取回**，
//! 见回显端点。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::validate_path_segment;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::{generate_token, seal};
use crate::addon::feishu::domain::source_key::valid_source_key;
use crate::infrastructure::audit;

/// 一条字段绑定的输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct FieldBindingInput {
    /// 多维表格字段 ID。**身份就是它**，不是字段名——改名不能断链。
    pub(super) field_id: String,
    /// 进 URL 路径段的数据源标识；全局唯一、创建后不可改。
    pub(super) source_key: String,
    /// 同表内的父列 `field_id`；无父给 `null` 或省略。
    #[serde(default)]
    pub(super) parent_field_id: Option<String>,
}

/// 创建表级数据源的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTableInput {
    /// 展示名。
    pub(super) title: String,
    /// 取数方式：`push`（默认）/ `pull`。
    #[serde(default)]
    pub(super) ingest_mode: Option<String>,
    /// 多维表格 app_token；`pull` 时必填。
    #[serde(default)]
    pub(super) bitable_base_token: Option<String>,
    /// 数据表 ID；`pull` 时必填。
    #[serde(default)]
    pub(super) bitable_table_id: Option<String>,
    /// 视图 ID；省略表示取全表。**它只决定拉取哪些行**，不影响能勾哪些字段。
    #[serde(default)]
    pub(super) bitable_view_id: Option<String>,
    /// 勾选的字段。至少一条。
    pub(super) fields: Vec<FieldBindingInput>,
}

impl ParamInput for CreateTableInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 父链是否成环。
///
/// 从每个节点沿父指针走，步数上限取节点数：无环时每步都走到一个**没走过的**节点，
/// 因此最多走「边数 ≤ 节点数」步；超过就一定回到了走过的节点。
///
/// 刻意不用递归——深链会爆栈，而链深由用户输入决定。
fn has_parent_cycle(fields: &[FieldBindingInput]) -> bool {
    let parent: HashMap<&str, &str> = fields
        .iter()
        .filter_map(|field| {
            field
                .parent_field_id
                .as_deref()
                .map(str::trim)
                .filter(|parent| !parent.is_empty())
                .map(|parent| (field.field_id.trim(), parent))
        })
        .collect();

    for start in parent.keys().copied() {
        let mut cursor = start;
        let mut steps = 0usize;
        while let Some(next) = parent.get(cursor).copied() {
            cursor = next;
            steps += 1;
            if steps > parent.len() {
                return true;
            }
        }
    }
    false
}

impl CreateTableInput {
    /// 进入事务前的入参校验。
    ///
    /// 顺序有讲究：先查便宜的形状，再查需要建索引的集合关系，最后才做要拼 URL 的
    /// 坐标校验——错误消息要指向**最先出问题**的那个输入。
    fn validate(&self) -> Result<(), BaseError> {
        if self.title.trim().is_empty() || self.title.chars().count() > 100 {
            return Err(BaseError::ParamInvalid(
                "title".to_string(),
                "名称必须在 1..=100 字符".to_string(),
            ));
        }
        if let Some(mode) = self.ingest_mode.as_deref() {
            if !matches!(mode, "push" | "pull") {
                return Err(BaseError::ParamInvalid(
                    "ingest_mode".to_string(),
                    "取数方式只能是 push 或 pull".to_string(),
                ));
            }
        }
        if self.fields.is_empty() {
            return Err(BaseError::ParamInvalid(
                "fields".to_string(),
                "至少要勾选一个字段".to_string(),
            ));
        }

        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut seen_keys: HashSet<&str> = HashSet::new();
        for field in &self.fields {
            let field_id = field.field_id.trim();
            if field_id.is_empty() {
                return Err(BaseError::ParamInvalid(
                    "field_id".to_string(),
                    "字段 ID 不能为空".to_string(),
                ));
            }
            if !seen_ids.insert(field_id) {
                return Err(BaseError::ParamInvalid(
                    "field_id".to_string(),
                    format!("字段 {field_id} 勾选了不止一次"),
                ));
            }
            if !valid_source_key(field.source_key.trim()) {
                return Err(BaseError::ParamInvalid(
                    "source_key".to_string(),
                    "数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]".to_string(),
                ));
            }
            if !seen_keys.insert(field.source_key.trim()) {
                return Err(BaseError::ParamInvalid(
                    "source_key".to_string(),
                    format!("数据源标识 {} 重复", field.source_key.trim()),
                ));
            }
        }

        // 父指针：必须在**勾选集合内**（没勾就没有它的选项可挂，拉取时也读不到父列），
        // 不能自指，且父链无环。
        for field in &self.fields {
            let Some(parent) = field
                .parent_field_id
                .as_deref()
                .map(str::trim)
                .filter(|parent| !parent.is_empty())
            else {
                continue;
            };
            if parent == field.field_id.trim() {
                return Err(BaseError::ParamInvalid(
                    "parent_field_id".to_string(),
                    "不能把自己设为父列".to_string(),
                ));
            }
            if !seen_ids.contains(parent) {
                return Err(BaseError::ParamInvalid(
                    "parent_field_id".to_string(),
                    format!("父列 {parent} 不在勾选集合内"),
                ));
            }
        }
        if has_parent_cycle(&self.fields) {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                "父链成环".to_string(),
            ));
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
        if self.ingest_mode.as_deref() == Some("pull") {
            // view 可空（表示取全表）；base_token 与 table_id 缺一不可。
            for (name, value) in [
                ("bitable_base_token", self.bitable_base_token.as_deref()),
                ("bitable_table_id", self.bitable_table_id.as_deref()),
            ] {
                if value
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    return Err(BaseError::ParamInvalid(
                        name.to_string(),
                        "取数方式为 pull 时必填".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// 注册新建表级数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_datasource_table"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/table")
        .display_name("新建表级数据源")
        .description("一次写入表级行与 N 条字段绑定（含系统生成的凭据），同一事务")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateTableInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 系统生成的凭据要靠这把密钥封存；没有它就做不了「可回显」这件事。
    // 与其静默降级成「只存摘要、永远取不回」，不如在这里说清楚缺什么。
    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：系统生成的凭据需要它来封存，否则无法回显".to_string(),
        )
    })?;

    let mut transaction = ctx.begin_transaction().await?;

    let result: Result<(u64, Vec<serde_json::Value>), BaseError> = async {
        let mut row = Record::new();
        row.insert("title", serde_json::json!(input.title.trim()));
        for (column, value) in [
            ("ingest_mode", input.ingest_mode.as_deref()),
            ("bitable_base_token", input.bitable_base_token.as_deref()),
            ("bitable_table_id", input.bitable_table_id.as_deref()),
            ("bitable_view_id", input.bitable_view_id.as_deref()),
        ] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                row.insert(column, serde_json::json!(value));
            }
        }
        // 绑定行要 `datasource_id`，所以必须拿到自增 id——
        // 普通的 `insert_in_tx` 返回的是「影响行数」（硬编码 1），拿不到 id。
        let (_, datasource_id) = context
            .datasources()
            .query()
            .insert_returning_id_in_tx(&mut transaction, row)
            .await?;

        let mut issued = Vec::with_capacity(input.fields.len());
        for field in &input.fields {
            let source_key = field.source_key.trim().to_string();
            let plaintext = generate_token();
            let (token_hash, token_cipher) = seal(&plaintext, &wrapping_key)?;

            let mut binding = Record::new();
            binding.insert("datasource_id", serde_json::json!(datasource_id));
            binding.insert("field_id", serde_json::json!(field.field_id.trim()));
            binding.insert("source_key", serde_json::json!(&source_key));
            binding.insert("token_hash", serde_json::json!(token_hash));
            binding.insert("token_cipher", serde_json::json!(token_cipher));
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

            issued.push(serde_json::json!({
                "field_id": field.field_id.trim(),
                "source_key": source_key,
                "token": plaintext,
            }));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", &input.title)?,
            None,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("created_table"),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok((datasource_id, issued))
    }
    .await;

    let (datasource_id, credentials) =
        FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "datasource_id": datasource_id,
            "credentials": credentials,
        }),
        "创建成功",
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

    /// 目标台账的两级链：费用大类 → 费用类型。
    fn two_level() -> CreateTableInput {
        CreateTableInput {
            title: "公司往来付款".to_string(),
            ingest_mode: Some("pull".to_string()),
            bitable_base_token: Some("ZoCWb82JQaCCiAspCqbcUvlsnwg".to_string()),
            bitable_table_id: Some("tblauuOafa4acvT3".to_string()),
            bitable_view_id: Some("vewAEKSbvO".to_string()),
            fields: vec![
                field("fldTyg5VBz", "main_exp_cat", None),
                field("fldEblAr7X", "fee_type", Some("fldTyg5VBz")),
            ],
        }
    }

    #[test]
    fn accepts_a_two_level_chain() {
        assert!(two_level().validate().is_ok());
    }

    #[test]
    fn accepts_a_three_level_chain() {
        // 实测台账上真有三级：费用大类 → 费用类型 → 银行流水摘要-编码。
        // 父指针模型下这是三条绑定、两条边，不需要界面支持「三级」这个概念。
        let mut input = two_level();
        input
            .fields
            .push(field("fldM0j5Do3", "summary_code", Some("fldEblAr7X")));
        assert!(input.validate().is_ok());
    }

    #[test]
    fn rejects_an_empty_field_list() {
        let mut input = two_level();
        input.fields.clear();
        assert!(input.validate().is_err(), "一个字段都不勾不允许建源");
    }

    #[test]
    fn rejects_a_duplicate_field_id() {
        let mut input = two_level();
        let first = input.fields[0].clone();
        input.fields.push(first);
        assert!(input.validate().is_err(), "同一列不能勾两次");
    }

    #[test]
    fn rejects_a_duplicate_source_key() {
        let mut input = two_level();
        input.fields[1].source_key = input.fields[0].source_key.clone();
        assert!(input.validate().is_err(), "source_key 必须唯一");
    }

    #[test]
    fn rejects_a_parent_that_is_not_checked() {
        // 父必须是**被勾选的**另一列：没勾就没有它的选项可挂，拉取时读不到父列
        let mut input = two_level();
        input.fields[1].parent_field_id = Some("fldNotChecked".to_string());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_self_parent() {
        let mut input = two_level();
        input.fields[0].parent_field_id = Some("fldTyg5VBz".to_string());
        assert!(input.validate().is_err(), "不能自己是自己的父");
    }

    #[test]
    fn rejects_a_two_node_cycle() {
        let mut input = two_level();
        input.fields[0].parent_field_id = Some("fldEblAr7X".to_string());
        assert!(input.validate().is_err(), "A→B 且 B→A 必须被拒");
    }

    #[test]
    fn rejects_a_longer_cycle() {
        // 甲→乙→丙→甲：步数上限判据必须能抓住比两节点更长的环
        let mut input = two_level();
        input.fields.push(field("fldC", "c", Some("fldA")));
        input.fields = vec![
            field("fldA", "a", Some("fldB")),
            field("fldB", "b", Some("fldC")),
            field("fldC", "c", Some("fldA")),
        ];
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_illegal_source_key_shape() {
        let mut input = two_level();
        input.fields[0].source_key = "Bad-Key".to_string();
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_blank_field_id() {
        let mut input = two_level();
        input.fields[0].field_id = "   ".to_string();
        assert!(input.validate().is_err());
    }

    #[test]
    fn pull_requires_base_token_and_table_id_but_not_view() {
        let mut input = two_level();
        input.bitable_view_id = None;
        assert!(input.validate().is_ok(), "缺 view 表示取全表，合法");

        input.bitable_table_id = None;
        assert!(input.validate().is_err(), "pull 缺 table_id 必须失败");
    }

    #[test]
    fn rejects_a_path_traversal_coordinate() {
        let mut input = two_level();
        input.bitable_base_token = Some("../evil".to_string());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_unknown_ingest_mode() {
        let mut input = two_level();
        input.ingest_mode = Some("telepathy".to_string());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_blank_title() {
        let mut input = two_level();
        input.title = "   ".to_string();
        assert!(input.validate().is_err());
    }

    #[test]
    fn push_mode_does_not_require_coordinates() {
        // 存量语义：没有坐标的源也能建（多维表格工作流推送到 source_key）
        let mut input = two_level();
        input.ingest_mode = Some("push".to_string());
        input.bitable_base_token = None;
        input.bitable_table_id = None;
        input.bitable_view_id = None;
        assert!(input.validate().is_ok());
    }
}
