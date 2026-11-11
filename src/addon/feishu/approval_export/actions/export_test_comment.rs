//! 向指定审批实例发一条测试评论。
//!
//! 用于上线前验证：已完成实例能否评论、附件 URL 能否被接受、作者 open_id
//! 是否有效。**必须显式 confirm=true**，且原样返回飞书 HTTP 状态与业务
//! code/msg——业务失败不折成框架错误，方便对照开放平台文档排错。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::post_instance_comment_raw;
use crate::addon::feishu::domain::approval_comment::{parse_comment_config, CommentFile};
use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 测试评论附件入参。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TestCommentFileInput {
    pub(super) url: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) file_size: i64,
    #[serde(default = "default_file_type")]
    pub(super) file_type: String,
}

fn default_file_type() -> String {
    "file".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TestCommentInput {
    /// 目标审批实例编码。
    pub(super) instance_code: String,
    /// 必须为 true；防止误触在真实单据上发评论。
    #[serde(default)]
    pub(super) confirm: bool,
    /// 评论正文；缺省为一条带时间戳的测试文案。
    #[serde(default)]
    pub(super) text: Option<String>,
    /// 评论作者 open_id；与 rule_id 二选一。
    #[serde(default)]
    pub(super) author_user_id: Option<String>,
    /// 从该规则的评论配置里取作者。
    #[serde(default)]
    pub(super) rule_id: Option<i64>,
    /// 可选附件（验证 files[] 是否被接受）。
    #[serde(default)]
    pub(super) files: Vec<TestCommentFileInput>,
}

impl ParamInput for TestCommentInput {
    fn params() -> Params {
        Params::new()
    }
}

impl TestCommentInput {
    fn validate(&self) -> Result<(), BaseError> {
        if !self.confirm {
            return Err(BaseError::ParamInvalid(
                "confirm".to_string(),
                "必须显式传 confirm=true 才会发出测试评论".to_string(),
            ));
        }
        if self.instance_code.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "instance_code".to_string(),
                "不能为空".to_string(),
            ));
        }
        if self
            .author_user_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
            && self.rule_id.is_none()
        {
            return Err(BaseError::ParamInvalid(
                "author_user_id".to_string(),
                "必须提供 author_user_id 或 rule_id，用于确定评论作者".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册测试评论端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_test_comment"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/comment/test",
        )
        .display_name("发送测试审批评论")
        .description("向指定实例发一条测试评论，原样返回飞书 code/msg（需 confirm=true）")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TestCommentInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let author = resolve_author(&context, &input).await?;
    let text = input
        .text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "【评论测试】这是一条由 yang-system 发送的测试评论（{}）",
                now_unix_secs()
            )
        });
    let files: Vec<CommentFile> = input
        .files
        .iter()
        .map(|file| CommentFile {
            url: file.url.trim().to_string(),
            title: if file.title.trim().is_empty() {
                "测试附件".to_string()
            } else {
                file.title.trim().to_string()
            },
            file_size: file.file_size.max(0),
            file_type: if file.file_type.trim().is_empty() {
                "file".to_string()
            } else {
                file.file_type.trim().to_string()
            },
        })
        .filter(|file| !file.url.is_empty())
        .collect();

    match post_instance_comment_raw(
        outbound.transport(),
        &outbound.tokens,
        input.instance_code.trim(),
        &author,
        &text,
        &files,
    )
    .await
    {
        Ok(response) => ApiResponse::success(
            serde_json::json!({
                "http_status": response.http_status,
                "code": response.code,
                "msg": response.msg,
                "comment_id": response.comment_id,
                "data": response.data,
            }),
            if response.code == 0 {
                "测试评论已发送".to_string()
            } else {
                format!("飞书拒绝了测试评论（code={}）", response.code)
            },
        ),
        Err(failure) => Err(BaseError::ConfigError(format!(
            "发送测试评论失败: {}",
            failure.message
        ))),
    }
}

async fn resolve_author(
    context: &FeishuContext,
    input: &TestCommentInput,
) -> Result<String, BaseError> {
    if let Some(author) = input
        .author_user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(author.to_string());
    }
    let rule_id = input.rule_id.unwrap_or_default();
    let raw: Option<String> = context
        .approval_export_rules()
        .query()
        .select_fields(&["comment_config_json"])?
        .where_eq("id", serde_json::json!(rule_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("审批回写规则不存在".to_string()))?
        .optional("comment_config_json")?;
    let config = parse_comment_config(raw.as_deref())
        .map_err(|error| BaseError::ParamInvalid("rule_id".to_string(), error))?
        .filter(|config| !config.author_user_id.trim().is_empty())
        .ok_or_else(|| {
            BaseError::ParamInvalid(
                "rule_id".to_string(),
                "该规则没有配置评论作者；请传 author_user_id".into(),
            )
        })?;
    Ok(config.author_user_id.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(confirm: bool) -> TestCommentInput {
        TestCommentInput {
            instance_code: "INST-1".into(),
            confirm,
            text: None,
            author_user_id: Some("ou_ops".into()),
            rule_id: None,
            files: Vec::new(),
        }
    }

    #[test]
    fn confirm_is_mandatory() {
        assert!(input(false).validate().is_err());
        assert!(input(true).validate().is_ok());
    }

    #[test]
    fn author_must_be_resolvable() {
        let mut value = input(true);
        value.author_user_id = Some("  ".into());
        assert!(value.validate().is_err());
        value.rule_id = Some(1);
        assert!(value.validate().is_ok());
    }

    #[test]
    fn empty_attachment_urls_are_dropped() {
        let files: Vec<CommentFile> = [
            TestCommentFileInput {
                url: " https://files/1.png ".into(),
                title: "回单".into(),
                file_size: 10,
                file_type: "image".into(),
            },
            TestCommentFileInput {
                url: "  ".into(),
                title: String::new(),
                file_size: 0,
                file_type: String::new(),
            },
        ]
        .iter()
        .map(|file| CommentFile {
            url: file.url.trim().to_string(),
            title: file.title.trim().to_string(),
            file_size: file.file_size,
            file_type: file.file_type.trim().to_string(),
        })
        .filter(|file| !file.url.is_empty())
        .collect();
        assert_eq!(files.len(), 1);
    }
}
