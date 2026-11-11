//! 审批评论配置体检。
//!
//! 逐项检查：规则存在且启用、评论配置可解析、字段存在、required 是 field_ids
//! 子集、状态列是单选且包含两种文案、已绑定飞书运维身份、多维表格事件已订阅、
//! 能按 target_record_id 找到实例映射、附件字段类型可识别。
//!
//! 只读出站接口（列表字段 / get_subscribe / 回写台账），不发评论、不改数据。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::{get_bitable_subscription, list_instance_comments};
use crate::addon::feishu::domain::approval_comment::{parse_comment_config, CommentConfig};
use crate::addon::feishu::domain::approval_convert::labels_equivalent;
use crate::addon::feishu::domain::approval_export::RULE_ROW_KEY;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates, FieldItem};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::user_token;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckCommentInput {
    pub(super) rule_id: i64,
    /// 可选：指定目标记录验证实例映射；缺省取该规则最近一条已回写记录。
    #[serde(default)]
    pub(super) record_id: Option<String>,
}

impl ParamInput for CheckCommentInput {
    fn params() -> Params {
        Params::new()
    }
}

impl CheckCommentInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.rule_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "rule_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct CommentCheckItem {
    /// 稳定的检查项标识。
    id: String,
    name: String,
    passed: bool,
    detail: String,
}

/// 注册评论配置体检端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_check_comment"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/comment/check",
        )
        .display_name("体检审批评论配置")
        .description("逐项检查评论配置的字段、状态列、订阅与实例映射")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CheckCommentInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let rule = context
        .approval_export_rules()
        .query()
        .select_fields(&[
            "id",
            "title",
            "enabled",
            "target_base_token",
            "target_table_id",
            "comment_config_json",
        ])?
        .where_eq("id", serde_json::json!(input.rule_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("审批回写规则不存在".to_string()))?;

    let title: String = rule.optional("title")?.unwrap_or_default();
    let enabled: bool = rule.optional("enabled")?.unwrap_or(false);
    let base_token: String = rule.require("target_base_token")?;
    let table_id: String = rule.require("target_table_id")?;
    let raw: Option<String> = rule.optional("comment_config_json")?;

    let mut items = Vec::new();
    items.push(CommentCheckItem {
        id: "rule_enabled".to_string(),
        name: "规则已启用".to_string(),
        passed: enabled,
        detail: if enabled {
            "回写规则处于启用状态".to_string()
        } else {
            "回写规则已停用：评论 worker 不会加载".to_string()
        },
    });

    let config = match parse_comment_config(raw.as_deref()) {
        Ok(Some(config)) => {
            items.push(CommentCheckItem {
                id: "config_parse".to_string(),
                name: "评论配置可解析".to_string(),
                passed: true,
                detail: "评论配置 JSON 合法".to_string(),
            });
            Some(config)
        }
        Ok(None) => {
            items.push(CommentCheckItem {
                id: "config_parse".to_string(),
                name: "评论配置可解析".to_string(),
                passed: false,
                detail: "规则没有评论配置（comment_config_json 为空）".to_string(),
            });
            None
        }
        Err(error) => {
            items.push(CommentCheckItem {
                id: "config_parse".to_string(),
                name: "评论配置可解析".to_string(),
                passed: false,
                detail: error,
            });
            None
        }
    };

    // 字段清单只取一次，后续检查共用。
    let coordinates = BitableCoordinates {
        app_token: base_token.clone(),
        table_id: table_id.clone(),
        view_id: None,
    };
    let fields = match bitable::list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => fields,
        Err(failure) => {
            items.push(CommentCheckItem {
                id: "target_table".to_string(),
                name: "目标表字段可读".to_string(),
                passed: false,
                detail: format!("读取目标表字段失败: {}", failure.message),
            });
            return Ok(check_response(input.rule_id, title, items));
        }
    };
    items.push(CommentCheckItem {
        id: "target_table".to_string(),
        name: "目标表字段可读".to_string(),
        passed: true,
        detail: format!("已读取 {} 个字段", fields.len()),
    });

    // 评论作者统一取页面顶部绑定的飞书运维身份。
    let binding = user_token::load_single(context.user_tokens()).await?;
    let bound_open_id = binding
        .as_ref()
        .map(|row| row.open_id.trim())
        .filter(|open_id| !open_id.is_empty());
    let bound_name = binding
        .as_ref()
        .and_then(|row| row.user_name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("未记录姓名");
    items.push(CommentCheckItem {
        id: "author".to_string(),
        name: "已绑定飞书运维身份".to_string(),
        passed: bound_open_id.is_some(),
        detail: match bound_open_id {
            Some(open_id) => format!("已绑定：{bound_name} · {open_id}"),
            None => "未绑定：请在审批回写页顶部完成飞书授权".to_string(),
        },
    });

    if let Some(config) = config.as_ref() {
        items.push(CommentCheckItem {
            id: "comment_enabled".to_string(),
            name: "评论功能已启用".to_string(),
            passed: config.enabled,
            detail: if config.enabled {
                "评论 worker 会处理该规则".to_string()
            } else {
                "comment_config.enabled=false：评论 worker 不会处理该规则".to_string()
            },
        });
        check_fields(config, &fields, &mut items);
        check_status_field(config, &fields, &mut items);
        check_attachment_fields(config, &fields, &mut items);
    }

    // 订阅状态。
    match get_bitable_subscription(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &base_token,
    )
    .await
    {
        Ok(subscribed) => items.push(CommentCheckItem {
            id: "subscription".to_string(),
            name: "多维表格事件已订阅".to_string(),
            passed: subscribed,
            detail: if subscribed {
                "get_subscribe 返回已订阅".to_string()
            } else {
                "未订阅：方案 A 收不到记录变更，只靠 30 秒轮询兜底".to_string()
            },
        }),
        Err(failure) => items.push(CommentCheckItem {
            id: "subscription".to_string(),
            name: "多维表格事件已订阅".to_string(),
            passed: false,
            detail: format!("查询订阅失败: {}", failure.message),
        }),
    }

    check_instance_mapping(
        &context,
        input.rule_id,
        input.record_id.as_deref(),
        &mut items,
    )
    .await?;

    // 附加信息：已绑定身份时顺便验证评论查询接口可用（去重依赖它）；
    // 未绑定时跳过该项，author 项已经报失败。
    if let Some(open_id) = bound_open_id {
        match find_latest_instance(&context, input.rule_id).await? {
            Some((_record_id, instance_code)) => {
                match list_instance_comments(
                    outbound.transport(),
                    outbound.sleeper(),
                    &outbound.tokens,
                    &instance_code,
                    open_id,
                )
                .await
                {
                    Ok(comments) => items.push(CommentCheckItem {
                        id: "list_comments".to_string(),
                        name: "评论列表可读".to_string(),
                        passed: true,
                        detail: format!(
                            "实例 {} 的评论列表可读（{} 条）",
                            instance_code,
                            comments.len()
                        ),
                    }),
                    Err(failure) => items.push(CommentCheckItem {
                        id: "list_comments".to_string(),
                        name: "评论列表可读".to_string(),
                        passed: false,
                        detail: format!("读取评论列表失败: {}", failure.message),
                    }),
                }
            }
            None => items.push(CommentCheckItem {
                id: "list_comments".to_string(),
                name: "评论列表可读".to_string(),
                passed: false,
                detail: "没有已回写记录，无法抽查评论列表".to_string(),
            }),
        }
    }

    Ok(check_response(input.rule_id, title, items))
}

fn check_response(rule_id: i64, title: String, items: Vec<CommentCheckItem>) -> ApiResponse {
    let passed = items.iter().all(|item| item.passed);
    let failed = items.iter().filter(|item| !item.passed).count();
    ApiResponse::success_value(
        serde_json::json!({
            "rule_id": rule_id,
            "title": title,
            "passed": passed,
            "failed_count": failed,
            "items": items,
        }),
        if passed {
            "体检通过".to_string()
        } else {
            format!("体检发现 {failed} 项需要处理")
        },
    )
}

fn check_fields(config: &CommentConfig, fields: &[FieldItem], items: &mut Vec<CommentCheckItem>) {
    let missing: Vec<&str> = config
        .field_ids
        .iter()
        .filter(|field_id| {
            !fields
                .iter()
                .any(|field| field.field_id.trim() == field_id.trim())
        })
        .map(String::as_str)
        .collect();
    items.push(CommentCheckItem {
        id: "field_ids".to_string(),
        name: "评论字段都存在".to_string(),
        passed: missing.is_empty(),
        detail: if missing.is_empty() {
            format!("{} 个字段均存在", config.field_ids.len())
        } else {
            format!("目标表缺少字段: {}", missing.join("、"))
        },
    });

    let not_subset: Vec<&str> = config
        .required_field_ids
        .iter()
        .filter(|field_id| !config.field_ids.iter().any(|value| value == *field_id))
        .map(String::as_str)
        .collect();
    items.push(CommentCheckItem {
        id: "required_fields".to_string(),
        name: "必填字段是评论字段子集".to_string(),
        passed: not_subset.is_empty(),
        detail: if not_subset.is_empty() {
            format!("{} 个必填字段", config.required_field_ids.len())
        } else {
            format!("不在 field_ids 内: {}", not_subset.join("、"))
        },
    });

    let trigger_ok = config.trigger_field_id.as_deref().map_or(true, |trigger| {
        config.field_ids.iter().any(|value| value == trigger)
    });
    items.push(CommentCheckItem {
        id: "trigger_field".to_string(),
        name: "触发字段配置有效".to_string(),
        passed: trigger_ok,
        detail: match config.trigger_field_id.as_deref() {
            Some(trigger) if trigger_ok => {
                format!("触发字段 {trigger}，触发值 {:?}", config.trigger_values)
            }
            Some(trigger) => format!("触发字段 {trigger} 不在 field_ids 内"),
            None => "未配置触发字段（只按必填判定）".to_string(),
        },
    });

    items.push(CommentCheckItem {
        id: "record_url".to_string(),
        name: "记录链接模板有效".to_string(),
        passed: ["{base_token}", "{table_id}", "{record_id}"]
            .iter()
            .all(|placeholder| config.record_url_template.contains(placeholder)),
        detail: config.record_url_template.clone(),
    });
}

fn check_status_field(
    config: &CommentConfig,
    fields: &[FieldItem],
    items: &mut Vec<CommentCheckItem>,
) {
    let field = fields
        .iter()
        .find(|field| field.field_id.trim() == config.status_field_id.trim());
    let mut reasons = Vec::new();
    if field.is_none() {
        reasons.push(format!(
            "找不到状态列 field_id「{}」",
            config.status_field_id
        ));
    }
    if let Some(field) = field {
        if field.field_type != Some(3) {
            reasons.push(format!(
                "状态列「{}」不是单选（类型 {:?}）",
                field.field_name, field.field_type
            ));
        }
        let options = field.option_names();
        if !options.is_empty() {
            for label in [
                config.status_labels.commented.as_str(),
                config.status_labels.failed.as_str(),
            ] {
                if !options
                    .iter()
                    .any(|option| labels_equivalent(option, label))
                {
                    reasons.push(format!("缺少单选选项「{label}」"));
                }
            }
        }
    }
    items.push(CommentCheckItem {
        id: "status_field".to_string(),
        name: "评论状态列可写".to_string(),
        passed: reasons.is_empty(),
        detail: if reasons.is_empty() {
            format!(
                "单选列「{}」包含文案 {} / {}",
                config.status_field_name,
                config.status_labels.commented,
                config.status_labels.failed
            )
        } else {
            reasons.join("；")
        },
    });
}

fn check_attachment_fields(
    config: &CommentConfig,
    fields: &[FieldItem],
    items: &mut Vec<CommentCheckItem>,
) {
    if !config.include_attachments {
        return;
    }
    let attachments: Vec<&FieldItem> = fields
        .iter()
        .filter(|field| {
            config
                .field_ids
                .iter()
                .any(|field_id| field_id.trim() == field.field_id.trim())
        })
        .filter(|field| field.field_type == Some(17))
        .collect();
    items.push(CommentCheckItem {
        id: "attachment_fields".to_string(),
        name: "附件字段可识别".to_string(),
        passed: !attachments.is_empty(),
        detail: if attachments.is_empty() {
            "include_attachments=true，但评论字段里没有附件类型（type=17）列".to_string()
        } else {
            attachments
                .iter()
                .map(|field| format!("{}（{}）", field.field_name, field.field_id))
                .collect::<Vec<_>>()
                .join("、")
        },
    });
}

async fn check_instance_mapping(
    context: &FeishuContext,
    rule_id: i64,
    record_id: Option<&str>,
    items: &mut Vec<CommentCheckItem>,
) -> Result<(), BaseError> {
    let record_id = match record_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => Some(value.to_string()),
        None => find_latest_instance(context, rule_id)
            .await?
            .map(|(record_id, _)| record_id),
    };
    let Some(record_id) = record_id else {
        items.push(CommentCheckItem {
            id: "instance_mapping".to_string(),
            name: "实例映射可反查".to_string(),
            passed: false,
            detail: "该规则没有已回写的目标记录，无法验证映射".to_string(),
        });
        return Ok(());
    };
    // 只取最近一页即可；all_pages 在规则写过超过 500 行时会误报翻页上限。
    let rows = context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_in("target_record_id", vec![serde_json::json!(&record_id)])?
        .where_eq("write_status", serde_json::json!("written"))?
        .where_ne("row_key", serde_json::json!(RULE_ROW_KEY))?
        .select_fields(&["instance_code"])?
        .order_by("id", SortOrder::Desc)?
        .page(1, 10)?
        .all()
        .await?;
    let instance = rows.iter().find_map(|row| {
        row.optional::<String>("instance_code")
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    });
    items.push(CommentCheckItem {
        id: "instance_mapping".to_string(),
        name: "实例映射可反查".to_string(),
        passed: instance.is_some(),
        detail: match instance {
            Some(instance) => format!("记录 {record_id} → 实例 {instance}"),
            None => format!("记录 {record_id} 在回写台账中找不到 write_status=written 的实例映射"),
        },
    });
    Ok(())
}

/// 取该规则最近一条已回写记录（目标记录 id + 实例编码）。
async fn find_latest_instance(
    context: &FeishuContext,
    rule_id: i64,
) -> Result<Option<(String, String)>, BaseError> {
    let rows = context
        .approval_export_runs()
        .query()
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("write_status", serde_json::json!("written"))?
        .where_ne("row_key", serde_json::json!(RULE_ROW_KEY))?
        .select_fields(&["target_record_id", "instance_code"])?
        .order_by("id", SortOrder::Desc)?
        .page(1, 20)?
        .all()
        .await?;
    Ok(rows.into_iter().find_map(|row: Record| {
        let record_id = row
            .optional::<String>("target_record_id")
            .ok()
            .flatten()?
            .trim()
            .to_string();
        let instance_code = row
            .optional::<String>("instance_code")
            .ok()
            .flatten()?
            .trim()
            .to_string();
        (!record_id.is_empty() && !instance_code.is_empty()).then_some((record_id, instance_code))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_id_and_unknown_keys_are_validated() {
        assert!(
            serde_json::from_value::<CheckCommentInput>(serde_json::json!({
                "rule_id": 0
            }))
            .is_ok()
        );
        let zero = serde_json::from_value::<CheckCommentInput>(serde_json::json!({"rule_id": 0}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(zero.validate().is_err());
        assert!(
            serde_json::from_value::<CheckCommentInput>(serde_json::json!({
                "rule_id": 1, "extra": true
            }))
            .is_err()
        );
    }

    #[test]
    fn response_is_passed_only_when_every_item_passes() {
        let response = check_response(
            1,
            "t".into(),
            vec![
                CommentCheckItem {
                    id: "a".into(),
                    name: "a".into(),
                    passed: true,
                    detail: String::new(),
                },
                CommentCheckItem {
                    id: "b".into(),
                    name: "b".into(),
                    passed: false,
                    detail: "x".into(),
                },
            ],
        );
        let value = serde_json::to_value(response).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["data"]["passed"], false);
        assert_eq!(value["data"]["failed_count"], 1);
    }
}
