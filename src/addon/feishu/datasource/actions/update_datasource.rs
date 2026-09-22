//! 更新数据源（前端控制台）。
//!
//! 除 `source_key` 外全部字段可选：**省略即保持原值**。这条规则很重要——若把省略
//! 当作「清空」，一次只改标题的调用会把 Token 抹掉，而 Token 抹掉后外部选项接口
//! 会立刻开始拒绝该数据源的全部请求。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::token::hash_token;
use crate::infrastructure::audit;

/// 更新数据源的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateDatasourceInput {
    /// 目标数据源。
    pub(super) source_key: String,
    /// 新名称。
    #[serde(default)]
    pub(super) title: Option<String>,
    /// 新 Token；省略表示不轮换。
    #[serde(default)]
    pub(super) token: Option<String>,
    /// 是否加密返回。
    #[serde(default)]
    pub(super) encrypt_enabled: Option<bool>,
    /// 默认语言。
    #[serde(default)]
    pub(super) default_locale: Option<String>,
    /// 状态：`active` / `disabled`。
    #[serde(default)]
    pub(super) status: Option<String>,
    /// 取数方式：`push` / `pull`；省略即保持原值。
    #[serde(default)]
    pub(super) ingest_mode: Option<String>,
    /// 多维表格 app_token。
    #[serde(default)]
    pub(super) bitable_base_token: Option<String>,
    /// 数据表 ID。
    #[serde(default)]
    pub(super) bitable_table_id: Option<String>,
    /// 视图 ID。
    #[serde(default)]
    pub(super) bitable_view_id: Option<String>,
    /// 取数列的**精确字段名**（接口要名字不要 field_id）。
    #[serde(default)]
    pub(super) bitable_field_name: Option<String>,
    /// 级联映射（JSON 文本）。
    #[serde(default)]
    pub(super) linkage_mapping: Option<String>,
}

impl ParamInput for UpdateDatasourceInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 校验坐标列。
///
/// 三个坐标都要拼进 **URL 路径段**，直接采信会让 `../` 之类的输入把请求打到别的
/// 路径上——所以在这里挡，而不是等到拉取时才由 `bitable` 的组装函数挡。
///
/// `pull` 方式还额外要求三者齐备：缺一个就不是一个可拉取的数据源。**不在创建时
/// 挡这一条**，运维就没法先建一个 `push` 源再改成 `pull`（改的那一次会缺字段而失败），
/// 也看不出「配了但拉不起来」的原因。
fn validate_coordinates(
    ingest_mode: Option<&str>,
    base_token: Option<&str>,
    table_id: Option<&str>,
    view_id: Option<&str>,
    field_name: Option<&str>,
) -> Result<(), BaseError> {
    if let Some(mode) = ingest_mode {
        if !matches!(mode, "push" | "pull") {
            return Err(BaseError::ParamInvalid(
                "ingest_mode".to_string(),
                "取数方式只能是 push 或 pull".to_string(),
            ));
        }
    }
    for (name, value) in [
        ("bitable_base_token", base_token),
        ("bitable_table_id", table_id),
        ("bitable_view_id", view_id),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            crate::addon::feishu::domain::bitable::validate_path_segment(name, value)
                .map_err(|error| BaseError::ParamInvalid(name.to_string(), error.to_string()))?;
        }
    }
    if ingest_mode == Some("pull") {
        for (name, value) in [
            ("bitable_base_token", base_token),
            ("bitable_table_id", table_id),
            ("bitable_field_name", field_name),
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

/// 校验级联映射是 JSON 对象（而不是数组或裸值）。
///
/// 只做形状校验，不校验内容：真正的可用性由拉取时的 `parse_linkage` 判定，
/// 那里解析不出来只会降级为「无级联」并告警，不会打挂整条链路。
fn validate_linkage_mapping(raw: Option<&str>) -> Result<(), BaseError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        _ => Err(BaseError::ParamInvalid(
            "linkage_mapping".to_string(),
            "必须是 JSON 对象，形如 {\"控件代码\":{\"parent_source_key\":…}}".to_string(),
        )),
    }
}

impl UpdateDatasourceInput {
    /// 进入事务前的入参校验。
    fn validate(&self) -> Result<(), BaseError> {
        if self.source_key.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识不能为空".to_string(),
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
        if let Some(token) = self.token.as_deref() {
            // 显式传空白 Token 是误用：想清空凭证应该停用数据源，而不是让它带着空摘要
            // 继续对外服务（那会让校验恒失败，表现为「接口一直报错」）
            if token.trim().is_empty() {
                return Err(BaseError::ParamInvalid(
                    "token".to_string(),
                    "Token 不能为空；不轮换请省略该字段".to_string(),
                ));
            }
        }
        if let Some(status) = self.status.as_deref() {
            if !matches!(status, "active" | "disabled") {
                return Err(BaseError::ParamInvalid(
                    "status".to_string(),
                    "status 只能是 active 或 disabled".to_string(),
                ));
            }
        }
        // 坐标的形状校验对已提供与未提供一视同仁；**「pull 必须三件齐备」只在
        // 本次请求把 ingest_mode 改成 pull 时才要求**——否则一次「只改标题」的调用
        // 会因为坐标没带上而被拒，那是荒谬的。
        validate_coordinates(
            self.ingest_mode.as_deref(),
            self.bitable_base_token.as_deref(),
            self.bitable_table_id.as_deref(),
            self.bitable_view_id.as_deref(),
            self.bitable_field_name.as_deref(),
        )?;
        validate_linkage_mapping(self.linkage_mapping.as_deref())?;
        Ok(())
    }
}

/// 注册更新数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("update_datasource"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Put, "/api/v1/feishu/datasources")
        .display_name("更新数据源")
        .description("更新飞书数据源的名称、Token、加密开关或状态")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateDatasourceInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let repository = context.datasources();

    let result: Result<u64, BaseError> = async {
        let mut record = Record::new();
        if let Some(title) = input.title.as_deref() {
            record.insert("title", serde_json::json!(title));
        }
        if let Some(token) = input.token.as_deref() {
            record.insert("token_hash", serde_json::json!(hash_token(token)));
        }
        if let Some(encrypt_enabled) = input.encrypt_enabled {
            record.insert("encrypt_enabled", serde_json::json!(encrypt_enabled));
        }
        if let Some(locale) = input.default_locale.as_deref() {
            record.insert("default_locale", serde_json::json!(locale));
        }
        if let Some(status) = input.status.as_deref() {
            record.insert("status", serde_json::json!(status));
        }

        // 坐标：与既有语义一致——**省略即保持原值**，传空串表示清空。
        //
        // 这里刻意不用「trim 后为空就跳过」：那会让「想清空一个填错的 base_token」
        // 变得做不到。空串写进列里等于清空，而 `load_pull_sources` 会把缺坐标的源
        // 跳过并告警——是响亮的失败，不是静默的错误。
        for (column, value) in [
            ("ingest_mode", input.ingest_mode.as_deref()),
            ("bitable_base_token", input.bitable_base_token.as_deref()),
            ("bitable_table_id", input.bitable_table_id.as_deref()),
            ("bitable_view_id", input.bitable_view_id.as_deref()),
            ("bitable_field_name", input.bitable_field_name.as_deref()),
            ("linkage_mapping", input.linkage_mapping.as_deref()),
        ] {
            if let Some(value) = value {
                record.insert(column, serde_json::json!(value.trim()));
            }
        }
        if record.as_map().is_empty() {
            return Err(BaseError::ParamInvalid(
                "body".to_string(),
                "没有要更新的字段".to_string(),
            ));
        }

        let affected = repository
            .query()
            .where_eq("source_key", serde_json::json!(input.source_key))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("数据源不存在".to_string()));
        }

        // 摘要只记「改了哪几类」不记值；字段名避开 SENSITIVE_FIELD_MARKERS
        // （password / secret / token / nonce / credential / authorization / cookie / hash），
        // 命中即被审计层拒绝
        let mut changed = Vec::new();
        if input.title.is_some() {
            changed.push("title");
        }
        if input.token.is_some() {
            changed.push("token_rotated");
        }
        if input.encrypt_enabled.is_some() {
            changed.push("encrypt_enabled");
        }
        if input.default_locale.is_some() {
            changed.push("default_locale");
        }
        if input.status.is_some() {
            changed.push("status");
        }
        // 新列的变更必须登记：漏了会让审计的 `outcome_code` 变空串——「有人改了取数
        // 方式」这类事实将无法从审计里看出来。
        for (column, present) in [
            ("ingest_mode", input.ingest_mode.is_some()),
            ("bitable_base_token", input.bitable_base_token.is_some()),
            ("bitable_table_id", input.bitable_table_id.is_some()),
            ("bitable_view_id", input.bitable_view_id.is_some()),
            ("bitable_field_name", input.bitable_field_name.is_some()),
            ("linkage_mapping", input.linkage_mapping.is_some()),
        ] {
            if present {
                changed.push(column);
            }
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", &input.source_key)?,
            None,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!(changed.join(",")),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(affected)
    }
    .await;

    let affected = FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(serde_json::json!({ "affected": affected }), "更新成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(source_key: &str) -> UpdateDatasourceInput {
        UpdateDatasourceInput {
            source_key: source_key.to_string(),
            title: None,
            token: None,
            encrypt_enabled: None,
            default_locale: None,
            status: None,
            ingest_mode: None,
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            bitable_field_name: None,
            linkage_mapping: None,
        }
    }

    #[test]
    fn accepts_source_key_only() {
        assert!(input("demo").validate().is_ok(), "只给标识应可校验通过");
    }

    #[test]
    fn rejects_blank_source_key() {
        assert!(input("  ").validate().is_err());
    }

    #[test]
    fn rejects_explicitly_blank_token() {
        // 显式空 Token 会让校验恒失败、表现为「接口一直报错」，属误用
        let mut payload = input("demo");
        payload.token = Some("   ".to_string());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn rejects_unknown_status() {
        let mut payload = input("demo");
        payload.status = Some("archived".to_string());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn accepts_both_documented_statuses() {
        for status in ["active", "disabled"] {
            let mut payload = input("demo");
            payload.status = Some(status.to_string());
            assert!(payload.validate().is_ok(), "{status} 应被接受");
        }
    }

    #[test]
    fn rejects_blank_and_over_long_title() {
        let mut blank = input("demo");
        blank.title = Some("  ".to_string());
        assert!(blank.validate().is_err());

        let mut long = input("demo");
        long.title = Some("标".repeat(101));
        assert!(long.validate().is_err());
    }
}

/// 坐标更新的三条语义：形状校验、`pull` 的齐备性只在本次改成 pull 时要求、
/// 以及「省略即保持原值 / 传空串即清空」。
#[cfg(test)]
mod coordinate_tests {
    use super::*;

    /// 自包含的构造器：本 module 与 `tests` 并列，拿不到那边的同名 helper。
    fn input(source_key: &str) -> UpdateDatasourceInput {
        UpdateDatasourceInput {
            source_key: source_key.to_string(),
            title: None,
            token: None,
            encrypt_enabled: None,
            default_locale: None,
            status: None,
            ingest_mode: None,
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            bitable_field_name: None,
            linkage_mapping: None,
        }
    }

    #[test]
    fn switching_to_pull_requires_every_coordinate_in_the_same_request() {
        // 把 ingest_mode 改成 pull 的这一次必须把坐标一起带上——否则会建出一个
        // 「说是拉取但没有任何坐标」的源，它每轮都会被跳过并告警。
        let mut payload = input("demo");
        payload.ingest_mode = Some("pull".to_string());
        assert!(payload.validate().is_err(), "只改方式不带坐标必须被拒绝");

        payload.bitable_base_token = Some("ZoCWb82JQaCCiAspCqbcUvlsnwg".to_string());
        payload.bitable_table_id = Some("tblauuOafa4acvT3".to_string());
        payload.bitable_field_name = Some("费用大类/Main Exp Cat*".to_string());
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn editing_another_field_does_not_require_coordinates() {
        // 「只改标题」不该因为没带坐标而被拒——那是荒谬的
        let mut payload = input("demo");
        payload.title = Some("新名字".to_string());
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn coordinates_are_still_shape_checked_when_provided_alone() {
        let mut payload = input("demo");
        payload.bitable_base_token = Some("../etc/passwd".to_string());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn blank_coordinate_is_accepted_so_it_can_be_cleared() {
        // 传空串表示**清空**：这里刻意不拦，否则填错的坐标将永远改不掉。
        // 清空后的源会被 load_pull_sources 跳过并告警——响亮的失败，不是静默的错误。
        let mut payload = input("demo");
        payload.bitable_base_token = Some(String::new());
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn rejects_unknown_ingest_mode() {
        let mut payload = input("demo");
        payload.ingest_mode = Some("sync".to_string());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn rejects_non_object_linkage_mapping() {
        let mut payload = input("demo");
        payload.linkage_mapping = Some("[1,2]".to_string());
        assert!(payload.validate().is_err());
    }
}
