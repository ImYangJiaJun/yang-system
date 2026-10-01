//! 更新审批派发配置（前端控制台）。
//!
//! # 只改三件套
//!
//! 仅 `title` / `enabled` / `base_timezone` 三项可更新。坐标（base_token/table_id）
//! 与三件套（approval_code/applicant_field/backfill_field）**不可改**——改了等于删了
//! 重建（见设计 §7：删配置重建有 uuid 幂等 + 60012 回捞兜底）。`deny_unknown_fields`
//! 会在反序列化层把多余键显式拒掉（400），不静默忽略。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 更新配置的输入契约：三个可更新字段全可省、至少给一个。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateConfigInput {
    /// 目标配置的 `id`。
    pub(super) config_id: i64,
    /// 展示名；省略即不改。
    #[serde(default)]
    pub(super) title: Option<String>,
    /// 启用开关；省略即不改。
    #[serde(default)]
    pub(super) enabled: Option<bool>,
    /// Base 时区（IANA 名）；省略即不改。
    #[serde(default)]
    pub(super) base_timezone: Option<String>,
}

impl ParamInput for UpdateConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpdateConfigInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "config_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.title.is_none() && self.enabled.is_none() && self.base_timezone.is_none() {
            return Err(BaseError::ParamInvalid(
                "title/enabled/base_timezone".to_string(),
                "至少提供一个要更新的字段".to_string(),
            ));
        }
        if let Some(title) = self.title.as_deref() {
            if title.trim().is_empty() || title.chars().count() > 100 {
                return Err(BaseError::ParamInvalid(
                    "title".to_string(),
                    "配置名必须在 1..=100 字符".to_string(),
                ));
            }
        }
        if let Some(timezone) = self.base_timezone.as_deref() {
            if timezone.trim().is_empty() || timezone.trim().chars().count() > 64 {
                return Err(BaseError::ParamInvalid(
                    "base_timezone".to_string(),
                    "时区名必须在 1..=64 字符".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// 注册更新配置端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("update_config"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/update")
        .display_name("更新审批派发配置")
        .description("更新配置名、启用开关或 Base 时区；坐标与审批三件套不可改")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 变更前快照与变更同事务提交：审计的 before/after 必须与库里的实际值一一对应。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let before = context
            .approval_configs()
            .query()
            .select_fields(&["title", "enabled", "base_timezone"])?
            .where_eq("id", serde_json::json!(input.config_id))?
            .optional()
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("审批派发配置不存在".to_string()))?;
        let old_title: String = before.require("title")?;
        let old_enabled: bool = before.require("enabled")?;
        let old_timezone: String = before.require("base_timezone")?;

        let mut record = Record::new();
        let mut after_title = old_title.clone();
        let mut after_enabled = old_enabled;
        let mut after_timezone = old_timezone.clone();
        if let Some(title) = input.title.as_deref() {
            let title = title.trim();
            record.insert("title", serde_json::json!(title));
            after_title = title.to_string();
        }
        if let Some(enabled) = input.enabled {
            record.insert("enabled", serde_json::json!(enabled));
            after_enabled = enabled;
        }
        if let Some(timezone) = input.base_timezone.as_deref() {
            let timezone = timezone.trim();
            record.insert("base_timezone", serde_json::json!(timezone));
            after_timezone = timezone.to_string();
        }

        let affected = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(input.config_id))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            // 事务内的 `optional` 与 UPDATE 之间配置被并发删除：明确失败而不是静默成功。
            return Err(BaseError::RecordNotFound("审批派发配置不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", input.config_id.to_string())?,
            Some(audit::summary([
                ("title", serde_json::json!(old_title)),
                ("enabled", serde_json::json!(old_enabled)),
                ("base_timezone", serde_json::json!(old_timezone)),
            ])?),
            Some(audit::summary([
                ("title", serde_json::json!(after_title)),
                ("enabled", serde_json::json!(after_enabled)),
                ("base_timezone", serde_json::json!(after_timezone)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({ "config_id": input.config_id }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(value, "配置已更新")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> UpdateConfigInput {
        UpdateConfigInput {
            config_id: 1,
            title: None,
            enabled: None,
            base_timezone: None,
        }
    }

    #[test]
    fn at_least_one_field_is_required() {
        assert!(input().validate().is_err(), "三件套全不给必须被拒");
    }

    #[test]
    fn any_single_field_suffices() {
        for value in [
            UpdateConfigInput {
                title: Some("改名".to_string()),
                ..input()
            },
            UpdateConfigInput {
                enabled: Some(false),
                ..input()
            },
            UpdateConfigInput {
                base_timezone: Some("UTC".to_string()),
                ..input()
            },
        ] {
            assert!(value.validate().is_ok(), "单个字段也应可更新");
        }
    }

    #[test]
    fn blank_or_overlong_title_is_rejected() {
        let mut value = input();
        value.title = Some("   ".to_string());
        assert!(value.validate().is_err());

        let mut value = input();
        value.title = Some("长".repeat(101));
        assert!(value.validate().is_err());
    }

    /// 坐标与三件套不可改：`deny_unknown_fields` 在反序列化层显式拒掉。
    #[test]
    fn coordinate_keys_are_rejected_not_ignored() {
        for extra in ["base_token", "table_id", "approval_code", "applicant_field"] {
            let mut json = serde_json::json!({ "config_id": 1, "title": "新名" });
            json[extra] = serde_json::json!("不该出现的键");
            let parsed = serde_json::from_value::<UpdateConfigInput>(json);
            assert!(parsed.is_err(), "键 {extra} 必须被拒——改坐标等于删了重建");
        }
    }

    #[test]
    fn zero_config_id_is_rejected() {
        let value = UpdateConfigInput {
            config_id: 0,
            title: Some("新名".to_string()),
            ..input()
        };
        assert!(value.validate().is_err());
    }
}
