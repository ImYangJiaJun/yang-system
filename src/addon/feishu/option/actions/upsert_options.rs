//! 多维表格写入 API：批量新增或更新选项（幂等 upsert）。
//!
//! 与取选项端点不同，这条入口返回**框架标准包络** `{code,message,data}`——它的调用方
//! 是飞书多维表格自动化工作流，只需要一个能判断成败的 JSON，不需要字面严格的契约。
//!
//! 幂等语义：按 `option_id` 判定新增还是更新。整批在一个事务内完成，并同事务追加
//! 审计事件（system actor）——不允许「写了一半」。

use std::collections::BTreeMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 单次请求允许的最大选项条数。
///
/// 多维表格一次自动化通常推送一行或少量行；设上限是为了让异常调用（例如把整表塞进
/// 一次请求）在进入事务前就被拒绝，而不是把数据库连接占满。
const MAX_BATCH: usize = 500;

/// 写入接口的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpsertOptionsInput {
    /// 目标数据源。
    pub(super) source_key: String,
    /// 要写入的选项；按 `id` 幂等新增或更新。
    pub(super) options: Vec<OptionUpsertItem>,
}

/// 单个待写入的选项。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OptionUpsertItem {
    /// 飞书契约的选项 id（全局唯一且固定）。
    pub(super) id: String,
    /// 默认语言下的文案。
    pub(super) label: String,
    /// 额外语言下的文案。
    #[serde(default)]
    pub(super) i18n: Option<BTreeMap<String, String>>,
    /// 排序键；省略时保持原值（新增则为 0）。
    #[serde(default)]
    pub(super) sort_order: Option<i64>,
    /// 是否为默认选项；省略时保持原值（新增则为 false）。
    #[serde(default)]
    pub(super) is_default: Option<bool>,
    /// 是否启用；省略时保持原值（新增则为 true）。
    #[serde(default)]
    pub(super) enabled: Option<bool>,
}

impl ParamInput for UpsertOptionsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpsertOptionsInput {
    /// 进入事务前的入参校验。
    fn validate(&self) -> Result<(), BaseError> {
        let invalid =
            |message: &str| BaseError::ParamInvalid("options".to_string(), message.to_string());
        if self.source_key.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识不能为空".to_string(),
            ));
        }
        if self.options.is_empty() {
            return Err(invalid("选项列表不能为空"));
        }
        if self.options.len() > MAX_BATCH {
            return Err(invalid(&format!(
                "单次最多写入 {MAX_BATCH} 条，收到 {}",
                self.options.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for item in &self.options {
            if item.id.trim().is_empty() {
                return Err(invalid("选项 id 不能为空"));
            }
            if item.id.chars().count() > 128 {
                return Err(invalid("选项 id 不能超过 128 个字符"));
            }
            if !seen.insert(item.id.as_str()) {
                // 同一批里重复 id 会让「先 0 行再插入」的判定互相打架，直接拒绝
                return Err(invalid(&format!("同一批次中选项 id 重复: {}", item.id)));
            }
        }
        Ok(())
    }

    /// 把一条输入转成待写入的记录。
    ///
    /// `i18n` 落 Text 列存放 JSON 文本（表声明层面 DSL 没有 Json builder）。
    fn to_record(&self, source_key: &str, item: &OptionUpsertItem) -> Result<Record, BaseError> {
        let mut record = Record::new();
        record.insert("option_id", serde_json::json!(item.id));
        record.insert("source_key", serde_json::json!(source_key));
        record.insert("label", serde_json::json!(item.label));
        if let Some(i18n) = item.i18n.as_ref() {
            let text = serde_json::to_string(i18n)
                .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?;
            record.insert("i18n", serde_json::json!(text));
        }
        if let Some(sort_order) = item.sort_order {
            record.insert("sort_order", serde_json::json!(sort_order));
        }
        if let Some(is_default) = item.is_default {
            record.insert("is_default", serde_json::json!(is_default));
        }
        if let Some(enabled) = item.enabled {
            record.insert("enabled", serde_json::json!(enabled));
        }
        Ok(record)
    }
}

/// 写入结果。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct UpsertOptionsResult {
    /// 新增条数。
    inserted: u64,
    /// 更新条数。
    updated: u64,
    /// 数据源标识。
    source_key: String,
}

/// 注册批量 upsert 端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("upsert_options"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/inbound/options/upsert")
        .display_name("同步飞书选项")
        .description("多维表格工作流批量新增或更新选项")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpsertOptionsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 数据源必须存在：写入一个不存在数据源的选项，等于制造永远取不到的孤儿数据
    let datasource = context
        .datasources()
        .query()
        .select_fields(&["source_key"])?
        .where_eq("source_key", serde_json::json!(input.source_key))?
        .optional()
        .await?;
    if datasource.is_none() {
        return Ok(ApiResponse::fail(40401, "数据源不存在"));
    }

    let mut transaction = ctx.begin_transaction().await?;
    let options = context.options();
    let mut inserted = 0u64;
    let mut updated = 0u64;

    let result: Result<(), BaseError> = async {
        for item in &input.options {
            let record = input.to_record(&input.source_key, item)?;
            // 先试更新：0 行受影响说明该 option_id 还不存在，再插入。这样不需要先读一次，
            // 也不会有「读到不存在然后被别人插进来」的窗口。
            let affected = options
                .query()
                .where_eq("option_id", serde_json::json!(item.id))?
                .update_in_tx(&mut transaction, record.clone())
                .await?;
            if affected == 0 {
                options
                    .query()
                    .insert_in_tx(&mut transaction, record)
                    .await?;
                inserted += 1;
            } else {
                updated += affected;
            }
        }
        // 同事务审计：整批一次事件。actor 是 system——这条入口没有登录身份，
        // 真正的事实是「多维表格推送了一批选项」。
        let event = audit::succeeded_system_event(
            &ctx,
            "feishu-inbound",
            None,
            Some(audit::entity("feishu_datasource", &input.source_key)?),
            audit::entity("feishu_option", &input.source_key)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("upserted")),
                (
                    "option_count",
                    serde_json::json!(input.options.len() as i64),
                ),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;

    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        UpsertOptionsResult {
            inserted,
            updated,
            source_key: input.source_key,
        },
        "同步成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> OptionUpsertItem {
        OptionUpsertItem {
            id: id.to_string(),
            label: format!("选项{id}"),
            i18n: None,
            sort_order: None,
            is_default: None,
            enabled: None,
        }
    }

    fn input(count: usize) -> UpsertOptionsInput {
        UpsertOptionsInput {
            source_key: "demo".to_string(),
            options: (0..count)
                .map(|index| item(&format!("opt_{index}")))
                .collect(),
        }
    }

    #[test]
    fn rejects_empty_option_list() {
        assert!(input(0).validate().is_err(), "空列表应被拒绝");
    }

    #[test]
    fn rejects_over_limit_batch() {
        assert!(input(MAX_BATCH).validate().is_ok(), "上限内应通过");
        assert!(input(MAX_BATCH + 1).validate().is_err(), "超限应被拒绝");
    }

    #[test]
    fn rejects_blank_option_id() {
        let mut payload = input(1);
        payload.options[0].id = "   ".to_string();
        assert!(payload.validate().is_err(), "空白 id 应被拒绝");
    }

    #[test]
    fn rejects_over_long_option_id() {
        let mut payload = input(1);
        payload.options[0].id = "x".repeat(129);
        assert!(payload.validate().is_err(), "超长 id 应被拒绝");
    }

    #[test]
    fn rejects_duplicate_ids_in_one_batch() {
        // 同批重复 id 会让「先更新 0 行再插入」的判定互相打架
        let mut payload = input(2);
        payload.options[1].id = payload.options[0].id.clone();
        assert!(payload.validate().is_err(), "同批重复 id 应被拒绝");
    }

    #[test]
    fn rejects_blank_source_key() {
        let mut payload = input(1);
        payload.source_key = "   ".to_string();
        assert!(payload.validate().is_err(), "空白数据源标识应被拒绝");
    }

    #[test]
    fn record_carries_only_provided_optional_fields() {
        // 省略的可选字段不得写入：写进去会用默认值覆盖已有行
        let payload = input(1);
        let record = payload
            .to_record("demo", &payload.options[0])
            .unwrap_or_else(|error| panic!("应可构造记录: {error}"));
        assert!(
            record.get("sort_order").is_none(),
            "未提供时不得写 sort_order"
        );
        assert!(
            record.get("is_default").is_none(),
            "未提供时不得写 is_default"
        );
        assert!(record.get("enabled").is_none(), "未提供时不得写 enabled");
        assert!(record.get("i18n").is_none(), "未提供时不得写 i18n");
        assert_eq!(
            record.get("source_key").and_then(|v| v.as_str()),
            Some("demo")
        );
    }

    #[test]
    fn record_serializes_i18n_as_json_text() {
        let mut payload = input(1);
        let mut i18n = BTreeMap::new();
        i18n.insert("en_us".to_string(), "Alpha".to_string());
        payload.options[0].i18n = Some(i18n);
        let record = payload
            .to_record("demo", &payload.options[0])
            .unwrap_or_else(|error| panic!("应可构造记录: {error}"));
        let text = record
            .get("i18n")
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("i18n 应序列化为文本"));
        assert!(text.contains(r#""en_us":"Alpha""#), "实际: {text}");
    }

    #[test]
    fn record_writes_explicit_false_and_zero() {
        // 显式传 false / 0 必须写进去，不能被「未提供」的判定吞掉
        let mut payload = input(1);
        payload.options[0].sort_order = Some(0);
        payload.options[0].is_default = Some(false);
        payload.options[0].enabled = Some(false);
        let record = payload
            .to_record("demo", &payload.options[0])
            .unwrap_or_else(|error| panic!("应可构造记录: {error}"));
        assert_eq!(record.get("sort_order"), Some(&serde_json::json!(0)));
        assert_eq!(record.get("is_default"), Some(&serde_json::json!(false)));
        assert_eq!(record.get("enabled"), Some(&serde_json::json!(false)));
    }
}
