//! 更新审批派发配置（前端控制台）。
//!
//! 可更新 `title` / `enabled` / `base_timezone`、全量字段映射与六个可选回填目标列
//! （设计 §5.1）。坐标（base_token/table_id）与三件套（approval_code/applicant_field/
//! backfill_field）**不可改**——改了等于删了重建（见设计 §7：删配置重建有 uuid 幂等 +
//! 60012 回捞兜底）。`deny_unknown_fields` 会在反序列化层把多余键显式拒掉（400），
//! 不静默忽略。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

use crate::addon::feishu::domain::approval_match::{columns_from_fields, FieldMapping};
use crate::addon::feishu::domain::approval_provision::{
    build_plan, check_backfill_targets, display_name, fill_table_names, insert_maps,
    resolve_optional_id, ProvisionInput,
};
use crate::addon::feishu::domain::bitable::{list_all_fields, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

/// 可更新字段均可省略，至少给一个。
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
    /// 审批定义的数字 ID（只用于控制台「打开审批定义」跳转）；省略即不改，
    /// `Some("")` = 清空。
    #[serde(default)]
    pub(super) approval_id: Option<String>,
    /// 全量替换字段映射；省略即不改。
    #[serde(default)]
    pub(super) maps: Option<Vec<FieldMapping>>,
    /// 六个可选回填目标列（设计 §5.1）。`field_id` **或**列名都接受，落库统一存 id；
    /// `Some("")` = 清空该列。给了就校验目标列存在且类型匹配（拉一次字段）。
    #[serde(default)]
    pub(super) status_field: Option<String>,
    #[serde(default)]
    pub(super) flow_field: Option<String>,
    #[serde(default)]
    pub(super) start_time_field: Option<String>,
    #[serde(default)]
    pub(super) end_time_field: Option<String>,
    #[serde(default)]
    pub(super) department_field: Option<String>,
    #[serde(default)]
    pub(super) handler_field: Option<String>,
}

impl ParamInput for UpdateConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpdateConfigInput {
    /// 六个可选回填列里有没有给（`Some("")` 也算——那是「清空该列」）。
    fn has_backfill_fields(&self) -> bool {
        self.status_field.is_some()
            || self.flow_field.is_some()
            || self.start_time_field.is_some()
            || self.end_time_field.is_some()
            || self.department_field.is_some()
            || self.handler_field.is_some()
    }

    fn validate(&self) -> Result<(), BaseError> {
        if self.config_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "config_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.title.is_none()
            && self.enabled.is_none()
            && self.base_timezone.is_none()
            && self.approval_id.is_none()
            && self.maps.is_none()
            && !self.has_backfill_fields()
        {
            return Err(BaseError::ParamInvalid(
                "title/enabled/base_timezone/approval_id/maps/回填列".to_string(),
                "至少提供一个要更新的字段".to_string(),
            ));
        }
        if let Some(approval_id) = self.approval_id.as_deref() {
            let approval_id = approval_id.trim();
            // 空串是「清空该列」，不是非法值。
            if !approval_id.is_empty()
                && (approval_id.len() > 64
                    || !approval_id.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return Err(BaseError::ParamInvalid(
                    "approval_id".to_string(),
                    "审批定义 ID 必须是 1..=64 位数字".to_string(),
                ));
            }
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
        .description(
            "更新配置名、启用开关、Base 时区、逐项字段映射或六个可选回填列；坐标与审批三件套不可改",
        )
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let backfill_requested = input.has_backfill_fields();

    // 飞书元数据校验（字段映射与六个可选回填列）在事务外完成，避免网络请求占用数据库事务。
    let (plan, columns) = if input.maps.is_some() || backfill_requested {
        let coordinates = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(input.config_id))?
            .optional()
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("审批派发配置不存在".to_string()))?;
        let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
            return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
        };
        let outbound = outbound_setup::build(&ctx, settings)?;
        let plan = if let Some(maps) = input.maps.as_deref() {
            let mut plan = build_plan(
                outbound.transport(),
                outbound.sleeper(),
                &outbound.tokens,
                &context,
                &ProvisionInput {
                    base_token: &coordinates.require::<String>("base_token")?,
                    table_id: &coordinates.require::<String>("table_id")?,
                    approval_code: &coordinates.require::<String>("approval_code")?,
                    applicant_field: &coordinates.require::<String>("applicant_field")?,
                    backfill_field: &coordinates.require::<String>("backfill_field")?,
                    base_timezone: input
                        .base_timezone
                        .as_deref()
                        .unwrap_or(&coordinates.require::<String>("base_timezone")?),
                    maps: Some(maps),
                    // 六个回填列走下面单独的轻校验（只拉一次字段），这里不重复校验。
                    status_field: None,
                    flow_field: None,
                    start_time_field: None,
                    end_time_field: None,
                    department_field: None,
                    handler_field: None,
                },
            )
            .await
            .map_err(|failure| BaseError::ParamInvalid("maps".to_string(), failure.to_string()))?;
            fill_table_names(
                &mut plan,
                outbound.transport(),
                outbound.sleeper(),
                &outbound.tokens,
            )
            .await;
            Some(plan)
        } else {
            None
        };

        // 六个可选回填列：拉一次字段校验存在且类型匹配（设计 §5.1）。
        let columns = if backfill_requested {
            let field_coordinates = BitableCoordinates {
                app_token: coordinates.require::<String>("base_token")?,
                table_id: coordinates.require::<String>("table_id")?,
                view_id: None,
            };
            let fields = list_all_fields(
                outbound.transport(),
                outbound.sleeper(),
                &outbound.tokens,
                &field_coordinates,
            )
            .await
            .map_err(|failure| BaseError::ParamInvalid("回填列".to_string(), failure.message))?;
            let columns = columns_from_fields(&fields);
            let reasons = check_backfill_targets(
                &columns,
                &fields,
                input.status_field.as_deref(),
                input.flow_field.as_deref(),
                input.start_time_field.as_deref(),
                input.end_time_field.as_deref(),
                input.department_field.as_deref(),
                input.handler_field.as_deref(),
            );
            if !reasons.is_empty() {
                return Err(BaseError::ParamInvalid(
                    "回填列".to_string(),
                    reasons.join("；"),
                ));
            }
            Some(columns)
        } else {
            None
        };

        (plan, columns)
    } else {
        (None, None)
    };

    // 变更前快照与变更同事务提交：审计的 before/after 必须与库里的实际值一一对应。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let before = transaction
            .select_for_update::<Record>(
                QueryBuilder::from_pool(
                    ctx.tools().mysql()?.pool(),
                    table!("feishu_approval_config"),
                )
                .fields(&[field!("title"), field!("enabled"), field!("base_timezone")])
                .where_and(field!("id"), CompareOp::Eq, input.config_id)?,
            )
            .await?
            .into_iter()
            .next()
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
        // 审批定义 ID：空串 = 清空（写 NULL），非空写 trim 后的值。
        if let Some(value) = input.approval_id.as_deref() {
            let value = value.trim();
            record.insert(
                "approval_id",
                if value.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(value)
                },
            );
        }
        if let Some(plan) = &plan {
            for (key, name) in [
                ("base_name", plan.base_name.as_deref()),
                ("table_name", plan.table_name.as_deref()),
                ("approval_name", Some(plan.approval_name.as_str())),
                (
                    "applicant_field_name",
                    Some(plan.applicant_field_name.as_str()),
                ),
                (
                    "backfill_field_name",
                    Some(plan.backfill_field_name.as_str()),
                ),
            ] {
                if let Some(name) = display_name(name) {
                    record.insert(key, serde_json::json!(name));
                }
            }
            record.insert("form_snapshot", serde_json::json!(plan.form_snapshot));
            record.insert(
                "form_snapshot_at",
                serde_json::json!(
                    crate::addon::feishu::domain::approval_provision::now_unix_secs()
                ),
            );
        }
        // 六个可选回填列（设计 §5.1）：给了才写（空串 = 清空 → NULL），未给的键不动。
        if let Some(columns) = &columns {
            for (name, value) in [
                ("status_field", input.status_field.as_deref()),
                ("flow_field", input.flow_field.as_deref()),
                ("start_time_field", input.start_time_field.as_deref()),
                ("end_time_field", input.end_time_field.as_deref()),
                ("department_field", input.department_field.as_deref()),
                ("handler_field", input.handler_field.as_deref()),
            ] {
                if value.is_some() {
                    record.insert(name, serde_json::json!(resolve_optional_id(columns, value)));
                }
            }
        }

        let affected = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(input.config_id))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            // 行已锁定；写入仍须命中目标配置。
            return Err(BaseError::RecordNotFound("审批派发配置不存在".to_string()));
        }

        let old_maps_count = if plan.is_some() {
            let maps = context
                .approval_field_maps()
                .query()
                .select_fields(&["widget_id", "bitable_field"])?
                .where_eq("config_id", serde_json::json!(input.config_id))?
                .all_in_tx(&mut transaction)
                .await?;
            context
                .approval_field_maps()
                .query()
                .where_eq("config_id", serde_json::json!(input.config_id))?
                .delete_in_tx(&mut transaction)
                .await?;
            Some(maps.len())
        } else {
            None
        };
        if let Some(plan) = &plan {
            insert_maps(&context, &mut transaction, input.config_id, plan).await?;
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", input.config_id.to_string())?,
            Some(config_audit_summary(
                &old_title,
                old_enabled,
                &old_timezone,
                old_maps_count,
            )?),
            Some(config_audit_summary(
                &after_title,
                after_enabled,
                &after_timezone,
                plan.as_ref().map(|p| p.widgets.len()),
            )?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({ "config_id": input.config_id }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(value, "配置已更新")
}

fn config_audit_summary(
    title: &str,
    enabled: bool,
    base_timezone: &str,
    maps_count: Option<usize>,
) -> Result<audit::AuditSummary, BaseError> {
    // 审计摘要禁止嵌套对象；映射明细仍由配置表保存，审计记录数量即可。
    audit::summary([
        ("title", serde_json::json!(title)),
        ("enabled", serde_json::json!(enabled)),
        ("base_timezone", serde_json::json!(base_timezone)),
        ("maps_count", serde_json::json!(maps_count)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_updates_have_valid_audit_summaries() -> Result<(), BaseError> {
        for count in [None, Some(0), Some(13), Some(1000)] {
            let summary = config_audit_summary("技术测试专用", true, "Asia/Shanghai", count)?;
            assert_eq!(
                summary.as_json(),
                serde_json::json!({
                    "title": "技术测试专用",
                    "enabled": true,
                    "base_timezone": "Asia/Shanghai",
                    "maps_count": count,
                })
            );
        }
        Ok(())
    }

    fn input() -> UpdateConfigInput {
        UpdateConfigInput {
            config_id: 1,
            title: None,
            enabled: None,
            base_timezone: None,
            approval_id: None,
            maps: None,
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
        }
    }

    #[test]
    fn at_least_one_field_is_required() {
        assert!(input().validate().is_err(), "没有更新字段必须被拒");
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
            UpdateConfigInput {
                maps: Some(vec![]),
                ..input()
            },
            UpdateConfigInput {
                status_field: Some("申请状态".to_string()),
                ..input()
            },
        ] {
            assert!(value.validate().is_ok(), "单个字段也应可更新");
        }
    }

    #[test]
    fn approval_id_alone_is_a_valid_update() {
        let value = UpdateConfigInput {
            approval_id: Some("7699783289053989908".to_string()),
            ..input()
        };
        assert!(value.validate().is_ok(), "仅 approval_id 也是合法更新");
    }

    #[test]
    fn non_numeric_approval_id_is_rejected() {
        let value = UpdateConfigInput {
            approval_id: Some("ABC123".to_string()),
            ..input()
        };
        assert!(value.validate().is_err(), "非数字必须被拒");

        let value = UpdateConfigInput {
            approval_id: Some("7".repeat(65)),
            ..input()
        };
        assert!(value.validate().is_err(), "超过 64 位必须被拒");
    }

    #[test]
    fn blank_approval_id_is_allowed_and_means_clear() {
        let value = UpdateConfigInput {
            approval_id: Some(String::new()),
            ..input()
        };
        assert!(value.validate().is_ok(), "空串是清空语义，不是没给");
    }

    /// 六个回填列：`Some("")` 也算「给了」（清空该列），不能因空串被判成「没有更新字段」。
    #[test]
    fn blank_backfill_field_still_counts_as_an_update() {
        let value = UpdateConfigInput {
            handler_field: Some(String::new()),
            ..input()
        };
        assert!(value.has_backfill_fields());
        assert!(value.validate().is_ok(), "空串是清空语义，不是没给");
    }

    #[test]
    fn backfill_fields_are_accepted_and_default_to_absent() {
        let parsed = serde_json::from_value::<UpdateConfigInput>(serde_json::json!({
            "config_id": 1,
            "status_field": "fldStatus",
            "handler_field": "当前处理人"
        }))
        .unwrap_or_else(|error| panic!("回填列应接受: {error}"));
        assert_eq!(parsed.status_field.as_deref(), Some("fldStatus"));
        assert_eq!(parsed.handler_field.as_deref(), Some("当前处理人"));
        assert!(parsed.flow_field.is_none());
        assert!(parsed.validate().is_ok());

        let legacy = serde_json::from_value::<UpdateConfigInput>(serde_json::json!({
            "config_id": 1, "title": "新名"
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(!legacy.has_backfill_fields());
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
