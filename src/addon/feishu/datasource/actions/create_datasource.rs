//! 新建数据源（前端控制台）。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::source_key::valid_source_key;
use crate::addon::feishu::domain::token::hash_token;
use crate::infrastructure::audit;

/// 新建数据源的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateDatasourceInput {
    /// 数据源标识；进外部选项接口的 URL，必须唯一且稳定。
    pub(super) source_key: String,
    /// 展示名。
    pub(super) title: String,
    /// 与飞书审批后台填写的 Token 一致；**只以摘要入库**。
    pub(super) token: String,
    /// 是否加密返回；需要服务端配置 `feishu.encryption_key`。
    #[serde(default)]
    pub(super) encrypt_enabled: Option<bool>,
    /// 默认语言。
    #[serde(default)]
    pub(super) default_locale: Option<String>,
    /// 取数方式：`push`（多维表格工作流推送，默认）/ `pull`（服务端定时拉取）。
    #[serde(default)]
    pub(super) ingest_mode: Option<String>,
    /// 多维表格 app_token（URL 里 `feishu.cn/base/<这段>`）；`pull` 时必填。
    #[serde(default)]
    pub(super) bitable_base_token: Option<String>,
    /// 数据表 ID；`pull` 时必填。
    #[serde(default)]
    pub(super) bitable_table_id: Option<String>,
    /// 视图 ID；省略表示取全表。
    #[serde(default)]
    pub(super) bitable_view_id: Option<String>,
    /// 取数列的**精确字段名**（接口要名字不要 field_id）；`pull` 时必填。
    #[serde(default)]
    pub(super) bitable_field_name: Option<String>,
    /// 级联映射（JSON 文本）。声明本数据源是某个父数据源的子集时给出。
    #[serde(default)]
    pub(super) linkage_mapping: Option<String>,
}

impl ParamInput for CreateDatasourceInput {
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

impl CreateDatasourceInput {
    /// 进入事务前的入参校验。
    fn validate(&self) -> Result<(), BaseError> {
        if !valid_source_key(&self.source_key) {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]".to_string(),
            ));
        }
        if self.title.trim().is_empty() || self.title.chars().count() > 100 {
            return Err(BaseError::ParamInvalid(
                "title".to_string(),
                "名称必须在 1..=100 字符".to_string(),
            ));
        }
        if self.token.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "token".to_string(),
                "Token 不能为空".to_string(),
            ));
        }
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

/// 注册新建数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_datasource"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources")
        .display_name("新建数据源")
        .description("创建一个飞书外部选项数据源")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateDatasourceInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let repository = context.datasources();

    let result: Result<(), BaseError> = async {
        let mut record = Record::new();
        record.insert("source_key", serde_json::json!(input.source_key));
        record.insert("title", serde_json::json!(input.title));
        // 只存摘要：本服务只需要校验 Token，永远不需要出示它
        record.insert("token_hash", serde_json::json!(hash_token(&input.token)));
        record.insert(
            "encrypt_enabled",
            serde_json::json!(input.encrypt_enabled.unwrap_or(false)),
        );

        // 坐标与取数方式：只为**提供了**的字段写值。`ingest_mode` 未提供时走表默认
        // `push`，与存量数据源的语义一致。
        for (column, value) in [
            ("ingest_mode", input.ingest_mode.as_deref()),
            ("bitable_base_token", input.bitable_base_token.as_deref()),
            ("bitable_table_id", input.bitable_table_id.as_deref()),
            ("bitable_view_id", input.bitable_view_id.as_deref()),
            ("bitable_field_name", input.bitable_field_name.as_deref()),
            ("linkage_mapping", input.linkage_mapping.as_deref()),
        ] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                record.insert(column, serde_json::json!(value));
            }
        }
        if let Some(locale) = input.default_locale.as_deref() {
            record.insert("default_locale", serde_json::json!(locale));
        }
        repository
            .query()
            .insert_in_tx(&mut transaction, record)
            .await?;

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", &input.source_key)?,
            None,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("created"),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;

    FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(
        serde_json::json!({"source_key": input.source_key}),
        "创建成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(source_key: &str, title: &str, token: &str) -> CreateDatasourceInput {
        CreateDatasourceInput {
            source_key: source_key.to_string(),
            title: title.to_string(),
            token: token.to_string(),
            encrypt_enabled: None,
            default_locale: None,
            ingest_mode: None,
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            bitable_field_name: None,
            linkage_mapping: None,
        }
    }

    #[test]
    fn accepts_lowercase_identifier() {
        assert!(input("dept_sales", "部门", "t").validate().is_ok());
        assert!(input("a", "部门", "t").validate().is_ok());
        assert!(input("a1_b2", "部门", "t").validate().is_ok());
    }

    #[test]
    fn rejects_identifier_that_would_break_the_url() {
        // 大写、点号、连字符、路径分隔符、中文一律拒绝——它们进 URL 路径段
        for bad in ["Dept", "dept.sales", "dept-sales", "a/b", "部门", "", "_a"] {
            assert!(
                input(bad, "部门", "t").validate().is_err(),
                "{bad:?} 不是合法的数据源标识"
            );
        }
    }

    #[test]
    fn rejects_over_long_identifier() {
        let long = "a".repeat(65);
        assert!(input(&long, "部门", "t").validate().is_err());
    }

    #[test]
    fn rejects_blank_title_and_token() {
        assert!(input("a", "  ", "t").validate().is_err());
        assert!(input("a", "部门", "  ").validate().is_err());
    }

    #[test]
    fn rejects_over_long_title() {
        let long = "标".repeat(101);
        assert!(input("a", &long, "t").validate().is_err());
    }
}

/// 坐标输入：形状校验与 `pull` 的齐备性要求。
#[cfg(test)]
mod coordinate_tests {
    use super::*;

    /// 自包含的构造器：本 module 与 `tests` 是并列的，拿不到那边的同名 helper。
    fn input(source_key: &str, title: &str, token: &str) -> CreateDatasourceInput {
        CreateDatasourceInput {
            source_key: source_key.to_string(),
            title: title.to_string(),
            token: token.to_string(),
            encrypt_enabled: None,
            default_locale: None,
            ingest_mode: None,
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            bitable_field_name: None,
            linkage_mapping: None,
        }
    }

    fn pull_input() -> CreateDatasourceInput {
        let mut payload = input("demo", "演示", "a-token-value");
        payload.ingest_mode = Some("pull".to_string());
        payload.bitable_base_token = Some("ZoCWb82JQaCCiAspCqbcUvlsnwg".to_string());
        payload.bitable_table_id = Some("tblauuOafa4acvT3".to_string());
        payload.bitable_field_name = Some("费用大类/Main Exp Cat*".to_string());
        payload
    }

    #[test]
    fn pull_requires_all_three_coordinates() {
        for clear in [0usize, 1, 2] {
            let mut payload = pull_input();
            match clear {
                0 => payload.bitable_base_token = None,
                1 => payload.bitable_table_id = None,
                _ => payload.bitable_field_name = None,
            }
            assert!(
                payload.validate().is_err(),
                "pull 缺第 {clear} 个坐标时必须被拒绝"
            );
        }
        assert!(pull_input().validate().is_ok(), "三个齐备时应通过");
    }

    #[test]
    fn push_does_not_require_coordinates() {
        // 存量语义不变：默认 push，不带任何坐标也必须能建
        assert!(input("demo", "演示", "a-token-value").validate().is_ok());
    }

    #[test]
    fn rejects_unknown_ingest_mode() {
        let mut payload = input("demo", "演示", "a-token-value");
        payload.ingest_mode = Some("sync".to_string());
        assert!(payload.validate().is_err());
    }

    #[test]
    fn rejects_path_traversal_in_coordinates() {
        // 坐标要拼进 URL 路径段，必须在入口挡住而不是等拉取时
        for (name, setter) in [("base_token", 0usize), ("table_id", 1), ("view_id", 2)] {
            let mut payload = pull_input();
            let bad = Some("../etc/passwd".to_string());
            match setter {
                0 => payload.bitable_base_token = bad,
                1 => payload.bitable_table_id = bad,
                _ => payload.bitable_view_id = bad,
            }
            assert!(payload.validate().is_err(), "{name} 的路径穿越必须被拒绝");
        }
    }

    #[test]
    fn rejects_non_object_linkage_mapping() {
        for raw in ["[]", "\"text\"", "123", "{not json"] {
            let mut payload = pull_input();
            payload.linkage_mapping = Some(raw.to_string());
            assert!(
                payload.validate().is_err(),
                "{raw} 不是 JSON 对象，必须被拒绝"
            );
        }
    }

    #[test]
    fn accepts_a_well_formed_linkage_mapping() {
        let mut payload = pull_input();
        payload.linkage_mapping = Some(
            r#"{"widget1":{"parent_source_key":"payment_currency","parent_field":"币种","cascade_field":"汇率"}}"#
                .to_string(),
        );
        assert!(payload.validate().is_ok());
    }
}
