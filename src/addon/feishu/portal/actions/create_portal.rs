//! 创建查询门户：一次写入门户行 + 审计事件（同一事务）。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::portal::domain::bindings::{
    ensure_datasource_is_active, load_bound_fields, require_all_bound,
};
use crate::infrastructure::audit;

/// 每分钟限速的下限/上限/缺省。
const RATE_LIMIT_MIN: u32 = 1;
const RATE_LIMIT_MAX: u32 = 120;
const RATE_LIMIT_DEFAULT: u32 = 30;

/// 口令最短长度。
const PASSPHRASE_MIN_LEN: usize = 8;

/// 支持的口令哈希最长长度（与 `passphrase_hash` 列宽一致）。
const TITLE_MAX: usize = 100;
const EMPTY_MESSAGE_MAX: usize = 500;
const CUSTOM_SLUG_MAX: usize = 64;

/// 创建门户的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreatePortalInput {
    /// 所属数据源 `id`。
    pub(super) datasource_id: i64,
    /// 门户名称（管理员用）。
    pub(super) title: String,
    /// 查询键：必须是该数据源已绑定的 `field_id`。
    pub(super) query_field_id: String,
    /// 开放列：`field_id` 列表，必须都是该数据源已绑定的字段。
    pub(super) exposed_columns: Vec<String>,
    /// 访问模式：`public` / `passphrase` / `login`。
    pub(super) access_mode: String,
    /// 门户口令；`access_mode = passphrase` 时必填。
    #[serde(default)]
    pub(super) passphrase: Option<String>,
    /// 自定义短链（可选；为空时用系统生成的 UUID slug）。
    #[serde(default)]
    pub(super) custom_slug: Option<String>,
    /// 已废弃的多查询键字段：历史上可写但从未被读取。为兼容旧控制台/脚本，
    /// 仍接受该键但**不校验、不落库、不影响查询**；新客户端不应再发送。
    #[serde(default)]
    pub(super) query_field_ids: Option<Vec<String>>,
    /// 展示名映射（可选）：`field_id` → 展示名。
    #[serde(default)]
    pub(super) result_labels: Option<HashMap<String, String>>,
    /// 查无结果时的提示文案（可选）。
    #[serde(default)]
    pub(super) empty_message: Option<String>,
    /// 每分钟最大查询次数；缺省 30，上限 120。
    #[serde(default)]
    pub(super) rate_limit_per_min: Option<u32>,
    /// 是否启用模糊匹配；缺省 true。
    #[serde(default)]
    pub(super) fuzzy_enabled: Option<bool>,
    /// 触发模糊匹配所需的最少连续数字位数；缺省 4。
    #[serde(default)]
    pub(super) fuzzy_min_digits: Option<u32>,
}

impl ParamInput for CreatePortalInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 访问模式取值。
pub(super) const ACCESS_MODES: [&str; 3] = ["public", "passphrase", "login"];

/// 校验 `custom_slug` 的形状：小写字母/数字/连字符/下划线，1..=64。
pub(super) fn validate_custom_slug(slug: &str) -> Result<(), BaseError> {
    let shaped = !slug.is_empty()
        && slug.chars().count() <= CUSTOM_SLUG_MAX
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !shaped {
        return Err(BaseError::ParamInvalid(
            "custom_slug".to_string(),
            "短链只能用小写字母、数字、连字符与下划线，长度 1..=64".to_string(),
        ));
    }
    Ok(())
}

impl CreatePortalInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.title.trim().is_empty()
            || self.title.chars().count() > TITLE_MAX
            || self.title.chars().any(char::is_control)
        {
            return Err(BaseError::ParamInvalid(
                "title".to_string(),
                format!("名称必须在 1..={TITLE_MAX} 字符"),
            ));
        }
        if self.query_field_id.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "query_field_id".to_string(),
                "查询键不能为空".to_string(),
            ));
        }
        if self.exposed_columns.is_empty() {
            return Err(BaseError::ParamInvalid(
                "exposed_columns".to_string(),
                "至少要开放一列".to_string(),
            ));
        }
        // 查询键本身不展示（设计 §4.1）：卡号这类值只用来查，不该回显在结果里。
        if self
            .exposed_columns
            .iter()
            .any(|column| column == &self.query_field_id)
        {
            return Err(BaseError::ParamInvalid(
                "exposed_columns".to_string(),
                "查询键不能出现在开放列中".to_string(),
            ));
        }
        if !ACCESS_MODES.contains(&self.access_mode.as_str()) {
            return Err(BaseError::ParamInvalid(
                "access_mode".to_string(),
                format!("访问模式只能是 {}", ACCESS_MODES.join("、")),
            ));
        }
        match self.passphrase.as_deref() {
            Some(passphrase) if passphrase.chars().count() < PASSPHRASE_MIN_LEN => {
                return Err(BaseError::ParamInvalid(
                    "passphrase".to_string(),
                    format!("口令至少 {PASSPHRASE_MIN_LEN} 个字符"),
                ));
            }
            // 口令模式没给口令 = 建出一个谁也进不去的门户；不如在这里拒掉。
            None if self.access_mode == "passphrase" => {
                return Err(BaseError::ParamInvalid(
                    "passphrase".to_string(),
                    "口令模式必须设置口令".to_string(),
                ));
            }
            _ => {}
        }
        if let Some(slug) = self.custom_slug.as_deref() {
            validate_custom_slug(slug)?;
        }
        if let Some(message) = self.empty_message.as_deref() {
            if message.chars().count() > EMPTY_MESSAGE_MAX {
                return Err(BaseError::ParamInvalid(
                    "empty_message".to_string(),
                    format!("提示文案不得超过 {EMPTY_MESSAGE_MAX} 字符"),
                ));
            }
        }
        if let Some(rate) = self.rate_limit_per_min {
            if !(RATE_LIMIT_MIN..=RATE_LIMIT_MAX).contains(&rate) {
                return Err(BaseError::ParamInvalid(
                    "rate_limit_per_min".to_string(),
                    format!("限速必须在 {RATE_LIMIT_MIN}..={RATE_LIMIT_MAX} 次/分钟"),
                ));
            }
        }
        if let Some(digits) = self.fuzzy_min_digits {
            if !(2..=16).contains(&digits) {
                return Err(BaseError::ParamInvalid(
                    "fuzzy_min_digits".to_string(),
                    "模糊匹配最低位数必须在 2..=16".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// 生效的限速值。
    fn rate_limit(&self) -> u32 {
        self.rate_limit_per_min.unwrap_or(RATE_LIMIT_DEFAULT)
    }

    /// 进入事务前要校验的 `field_id` 全集（查询键 + 开放列）。
    fn referenced_field_ids(&self) -> Vec<String> {
        let mut ids = vec![self.query_field_id.clone()];
        ids.extend(self.exposed_columns.iter().cloned());
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// `custom_slug` 的 `insert_returning_id_in_tx` 唯一键冲突折算成参数错误。
///
/// 预检（[`ensure_custom_slug_available`]）挡的是先到的那一个；并发创建只能靠
/// 唯一索引兜底，而底层把它包成 `DatabaseExecuteFailed`——不折算就是 500。
fn fold_duplicate_custom_slug(error: BaseError) -> BaseError {
    if let BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(message)) = &error {
        if message.contains("Duplicate entry")
            && message.contains("uk_feishu_datasource_portal_custom_slug")
        {
            return BaseError::ParamInvalid(
                "custom_slug".to_string(),
                "该短链已被占用，请换一个".to_string(),
            );
        }
    }
    error
}

/// 写入前查重，避免一次注定回滚的事务。
async fn ensure_custom_slug_available(
    context: &FeishuContext,
    custom_slug: Option<&str>,
) -> Result<(), BaseError> {
    let Some(custom_slug) = custom_slug else {
        return Ok(());
    };
    let existing = context
        .portals()
        .query()
        .select_fields(&["id"])?
        .where_eq("custom_slug", serde_json::json!(custom_slug))?
        .optional()
        .await?;
    if existing.is_some() {
        return Err(BaseError::ParamInvalid(
            "custom_slug".to_string(),
            "该短链已被占用，请换一个".to_string(),
        ));
    }
    Ok(())
}

/// 注册新建门户端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_portal"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/portals")
        .display_name("新建查询门户")
        .description("把数据源的指定列暴露为一个可对外查询的端点")
        .permissions(["feishu.portal.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreatePortalInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    // 兼容字段：读一次以避免 dead_code；不参与字段校验、不落库、不影响查询。
    let _ = input.query_field_ids.as_ref();
    ensure_datasource_is_active(&context, input.datasource_id).await?;

    let referenced = input.referenced_field_ids();
    let bindings = load_bound_fields(&context, input.datasource_id, &referenced).await?;
    require_all_bound(
        &bindings,
        std::slice::from_ref(&input.query_field_id),
        "query_field_id",
    )?;
    require_all_bound(&bindings, &input.exposed_columns, "exposed_columns")?;

    // 哈希在事务外做：argon2 是**故意**慢的，把它塞进事务会把行锁持有到几百毫秒。
    let passphrase_hash = match input.passphrase.as_deref() {
        Some(passphrase) => Some(context.password_engine().hash(passphrase).await?),
        None => None,
    };

    ensure_custom_slug_available(&context, input.custom_slug.as_deref()).await?;

    let query_field_source_key = bindings
        .get(&input.query_field_id)
        .cloned()
        .ok_or_else(|| {
            BaseError::ParamInvalid(
                "query_field_id".to_string(),
                "查询键不是该数据源的已绑定字段".to_string(),
            )
        })?;
    let slug = uuid::Uuid::new_v4().simple().to_string();

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<i64, BaseError> = async {
        let mut row = Record::new();
        row.insert("datasource_id", serde_json::json!(input.datasource_id));
        row.insert("title", serde_json::json!(input.title.trim()));
        row.insert("slug", serde_json::json!(&slug));
        row.insert(
            "query_field_id",
            serde_json::json!(input.query_field_id.trim()),
        );
        row.insert(
            "query_field_source_key",
            serde_json::json!(&query_field_source_key),
        );
        row.insert("access_mode", serde_json::json!(&input.access_mode));
        row.insert(
            "exposed_columns",
            serde_json::json!(serde_json::to_string(&input.exposed_columns)
                .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?),
        );
        row.insert(
            "rate_limit_per_min",
            serde_json::json!(i64::from(input.rate_limit())),
        );
        row.insert(
            "fuzzy_enabled",
            serde_json::json!(input.fuzzy_enabled.unwrap_or(true)),
        );
        row.insert(
            "fuzzy_min_digits",
            serde_json::json!(i64::from(input.fuzzy_min_digits.unwrap_or(4))),
        );
        row.insert("enabled", serde_json::json!(true));
        for (column, value) in [
            ("passphrase_hash", passphrase_hash.as_deref()),
            ("custom_slug", input.custom_slug.as_deref()),
            ("empty_message", input.empty_message.as_deref()),
        ] {
            if let Some(value) = value {
                row.insert(column, serde_json::json!(value));
            }
        }
        if let Some(labels) = &input.result_labels {
            row.insert(
                "result_labels",
                serde_json::json!(serde_json::to_string(labels)
                    .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?),
            );
        }
        let (_, portal_id) = context
            .portals()
            .query()
            .insert_returning_id_in_tx(&mut transaction, row)
            .await
            .map_err(fold_duplicate_custom_slug)?;

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource_portal", portal_id)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("created_portal")),
                ("datasource_id", serde_json::json!(input.datasource_id)),
                ("access_mode", serde_json::json!(&input.access_mode)),
                (
                    "exposed_columns",
                    serde_json::json!(input.exposed_columns.len()),
                ),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(portal_id as i64)
    }
    .await;

    let portal_id = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "portal_id": portal_id,
            "slug": slug,
        }),
        "创建成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CreatePortalInput {
        CreatePortalInput {
            datasource_id: 7,
            title: "卡号查询".to_string(),
            query_field_id: "fldCard".to_string(),
            exposed_columns: vec!["fldShop".to_string()],
            access_mode: "public".to_string(),
            passphrase: None,
            custom_slug: None,
            query_field_ids: None,
            result_labels: None,
            empty_message: None,
            rate_limit_per_min: None,
            fuzzy_enabled: None,
            fuzzy_min_digits: None,
        }
    }

    #[test]
    fn accepts_a_public_portal() {
        assert!(input().validate().is_ok());
    }

    #[test]
    fn deprecated_query_field_ids_is_accepted_but_ignored() {
        // 旧客户端仍可能发送该键；为兼容不返回 400，但它不参与字段校验/写入。
        let mut input = input();
        input.query_field_ids = Some(vec!["fldCard".to_string(), "fldUnknown".to_string()]);
        assert!(input.validate().is_ok());
        assert_eq!(
            input.referenced_field_ids(),
            vec!["fldCard".to_string(), "fldShop".to_string()],
            "废弃字段不得进入字段绑定校验"
        );
    }

    #[test]
    fn rejects_a_non_positive_datasource_id() {
        let mut input = input();
        input.datasource_id = 0;
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_empty_or_over_long_title() {
        for title in [
            "   ".to_string(),
            "字".repeat(101),
            "卡号\t查询".to_string(),
        ] {
            let mut input = input();
            input.title = title;
            assert!(input.validate().is_err(), "非法名称必须被拒");
        }
    }

    #[test]
    fn rejects_exposing_the_query_key() {
        // 设计 §4.1：查询键本身不展示——卡号只用来查，不该回显。
        let mut input = input();
        input.exposed_columns = vec!["fldCard".to_string()];
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_empty_exposed_column_set() {
        let mut input = input();
        input.exposed_columns.clear();
        assert!(input.validate().is_err());
    }

    #[test]
    fn passphrase_mode_requires_a_long_enough_passphrase() {
        let mut input = input();
        input.access_mode = "passphrase".to_string();
        assert!(input.validate().is_err(), "口令模式必须给口令");

        input.passphrase = Some("short".to_string());
        assert!(input.validate().is_err(), "口令短于 8 位必须被拒");

        input.passphrase = Some("long-enough".to_string());
        assert!(input.validate().is_ok());
    }

    #[test]
    fn rejects_an_unknown_access_mode() {
        let mut input = input();
        input.access_mode = "telepathy".to_string();
        assert!(input.validate().is_err());
    }

    #[test]
    fn rate_limit_defaults_to_thirty_and_is_bounded() {
        assert_eq!(input().rate_limit(), RATE_LIMIT_DEFAULT);

        for rate in [0u32, 121] {
            let mut input = input();
            input.rate_limit_per_min = Some(rate);
            assert!(input.validate().is_err(), "限速 {rate} 应被拒");
        }
        for rate in [RATE_LIMIT_MIN, RATE_LIMIT_MAX] {
            let mut input = input();
            input.rate_limit_per_min = Some(rate);
            assert!(input.validate().is_ok(), "限速 {rate} 应被接受");
        }
    }

    #[test]
    fn rejects_an_out_of_range_fuzzy_min_digits() {
        for digits in [0u32, 1, 17, 100] {
            let mut input = input();
            input.fuzzy_min_digits = Some(digits);
            assert!(input.validate().is_err(), "位数 {digits} 应被拒");
        }
        for digits in [2u32, 4, 16] {
            let mut input = input();
            input.fuzzy_min_digits = Some(digits);
            assert!(input.validate().is_ok(), "位数 {digits} 应被接受");
        }
    }

    #[test]
    fn custom_slug_shape_is_enforced() {
        for slug in ["", "UPPER", "has space", "斜杠/路径", &"a".repeat(65)] {
            assert!(validate_custom_slug(slug).is_err(), "{slug:?} 应被拒");
        }
        for slug in ["card-2026", "shop_1", "a"] {
            assert!(validate_custom_slug(slug).is_ok(), "{slug:?} 应被接受");
        }
    }

    #[test]
    fn rejects_an_over_long_empty_message() {
        let mut input = input();
        input.empty_message = Some("字".repeat(501));
        assert!(input.validate().is_err());
    }

    #[test]
    fn the_slug_generated_by_the_system_is_a_32_char_hex_uuid() {
        // URL 上暴露的是它，不是自增 id；形态钉死，避免哪天换成带连字符的形态
        // 撞上 `slug` 列的 32 字符上限。
        let slug = uuid::Uuid::new_v4().simple().to_string();
        assert_eq!(slug.chars().count(), 32);
        assert!(slug.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn referenced_field_ids_deduplicate_and_include_every_source() {
        let mut input = input();
        // 查询键与开放列都进来，重复的只留一份。
        input.exposed_columns = vec!["fldShop".to_string(), "fldCard".to_string()];
        assert_eq!(
            input.referenced_field_ids(),
            vec!["fldCard".to_string(), "fldShop".to_string()]
        );
    }

    #[test]
    fn the_unique_index_name_matched_by_the_fold_is_the_declared_one() {
        // 折叠函数按索引名识别冲突；索引名是 DSL 自动生成的 `uk_<表>_<列>`。
        // 名字对不上时并发创建会退化成 500，而这条断言是唯一能提前发现它的地方。
        let definition = crate::addon::feishu::portal::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_datasource_portal");
        let spec = crate::addon::feishu::portal::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let custom_slug = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "custom_slug")
            .unwrap_or_else(|| panic!("custom_slug 必须存在"));
        assert!(custom_slug.storage.unique);
    }
}
