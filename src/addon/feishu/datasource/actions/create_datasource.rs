//! 新建数据源（前端控制台）。

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
}

impl ParamInput for CreateDatasourceInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 数据源标识的合法形态。
///
/// 它进 URL 路径段，因此限定为小写字母开头的 `[a-z0-9_]`——避免百分号编码、
/// 大小写歧义与路径穿越。
fn valid_source_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 64
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
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
