//! 附件下载代理：把飞书附件地址转换为同源响应，避免浏览器直连飞书域。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::sso::require_current_user;

yang_base::params! {
    pub(super) AttachmentInput {
        #[param(source = query)]
        url: Str::new().require(true),
        #[param(source = query)]
        name: Str::new().require(true),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能下载飞书附件，地址限制在飞书域
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_attachment"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/batch-approval/attachments")
        .display_name("批量审批附件下载")
        .description("代理下载飞书审批附件，供浏览器预览与下载")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AttachmentInput,
    _context: Arc<FeishuContext>,
) -> Result<ResponseBody, BaseError> {
    let _user_id = require_current_user(&ctx)?;
    validate_attachment_url(&input.url)?;
    let filename = safe_filename(&input.name);
    let extension = Path::new(&filename)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 8
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or("bin");
    let response = ctx
        .tools()
        .http()?
        .get(&input.url)
        .timeout(30)
        .max_response_bytes(64 * 1024 * 1024)
        .send()
        .await?;
    if response.status() != 200 {
        return Err(BaseError::ParamInvalid(
            "url".to_string(),
            format!("附件下载失败（HTTP {}）", response.status()),
        ));
    }
    let bytes = response.bytes().await?;
    if bytes.is_empty() {
        return Err(BaseError::ParamInvalid(
            "url".to_string(),
            "附件内容为空".to_string(),
        ));
    }
    let path = std::env::temp_dir().join(format!(
        "yang-feishu-attachment-{}.{}",
        uuid::Uuid::new_v4(),
        extension
    ));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|error| BaseError::ConfigError(format!("写入附件临时文件失败: {error}")))?;
    let cleanup = path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(600)).await;
        let _ = tokio::fs::remove_file(cleanup).await;
    });
    Ok(ResponseBody::download(path, filename))
}

/// 只允许飞书文件域，避免把这个端点变成任意 URL 的 SSRF 通道。
fn validate_attachment_url(raw: &str) -> Result<(), BaseError> {
    if raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err(BaseError::ParamInvalid(
            "url".to_string(),
            "附件地址非法".to_string(),
        ));
    }
    let Some(rest) = raw.strip_prefix("https://") else {
        return Err(BaseError::ParamInvalid(
            "url".to_string(),
            "附件地址必须使用 HTTPS".to_string(),
        ));
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains(':')
        || !(authority == "feishu.cn"
            || authority.ends_with(".feishu.cn")
            || authority == "larksuite.com"
            || authority.ends_with(".larksuite.com"))
    {
        return Err(BaseError::ParamInvalid(
            "url".to_string(),
            "附件地址不属于飞书文件域".to_string(),
        ));
    }
    Ok(())
}

fn safe_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|character| !character.is_control() && *character != '/' && *character != '\\')
        .take(180)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_url_is_limited_to_feishu_domains() {
        assert!(validate_attachment_url(
            "https://internal-api-drive-stream.feishu.cn/space/api/box/stream/download/authcode/?code=x"
        )
        .is_ok());
        assert!(validate_attachment_url("https://open.feishu.cn/open-apis/a").is_ok());
        assert!(validate_attachment_url("http://open.feishu.cn/a").is_err());
        assert!(validate_attachment_url("https://evil.example.com/a.png").is_err());
        assert!(validate_attachment_url("https://open.feishu.cn.evil.example/a.png").is_err());
        assert!(validate_attachment_url("https://user@open.feishu.cn/a.png").is_err());
    }

    #[test]
    fn filename_never_contains_path_separators() {
        assert_eq!(safe_filename("../a\\b.png"), "..ab.png");
        assert_eq!(safe_filename("  "), "attachment");
    }
}
