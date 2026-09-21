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
}

impl ParamInput for UpdateDatasourceInput {
    fn params() -> Params {
        Params::new()
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
