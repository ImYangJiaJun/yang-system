//! 上传当前用户头像（需登录，数据库存储，不引入对象存储）。
//!
//! 本人写自己的行：不需要 Step-up、不审计、不触碰 authz/credential 版本。
//! 图片经 base64 提交，解码后做 MIME 白名单 + magic bytes + 宽高校验，
//! etag 取内容 sha256 十六进制前 32 字符，作为前端缓存失效的版本号返回。

use crate::addon::account::domain::avatar::validate_avatar_image;
use crate::addon::account::Account;
use base64::Engine;
use schemars::JsonSchema;
use serde::Serialize;
use sha2::Digest;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) UploadAvatarInput {
        /// 头像图片字节的 base64 编码（解码后 ≤ 40 KiB）。
        content_base64: Str::new()
            .title("头像内容（base64）")
            .require(true)
            .max_length(60000),
        /// 图片 MIME 类型：image/png、image/jpeg、image/webp、image/gif。
        mime: Str::new()
            .title("图片 MIME 类型")
            .require(true)
            .max_length(32),
    }
}

/// 上传成功后的头像版本（内容 etag，前端据此失效缓存）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct UploadAvatarOutput {
    avatar_version: String,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UploadAvatarInput,
    account: Arc<Account>,
) -> Result<UploadAvatarOutput, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    let content_base64 = input.content_base64.trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64)
        .map_err(|_| {
            BaseError::ParamInvalid(
                "content_base64".to_string(),
                "头像内容不是合法的 base64 编码".to_string(),
            )
        })?;
    validate_avatar_image(&input.mime, &bytes)?;
    // etag = sha256 十六进制前 32 字符（前 16 字节）。
    let digest = sha2::Sha256::digest(&bytes);
    let etag = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let now = current_unix_timestamp()?;
    account
        .avatars()
        .upsert(&ctx, user_id, &input.mime, content_base64, &etag, now)
        .await?;
    Ok(UploadAvatarOutput {
        avatar_version: etag,
    })
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("upload_avatar"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/avatar")
        .display_name("上传头像")
        .description("上传当前用户头像（PNG/JPEG/WebP/GIF，解码后 ≤ 40 KiB，宽高 ≤ 1024）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_content_and_mime() {
        let params = <UploadAvatarInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["content_base64", "mime"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn input_rejects_unknown_fields() {
        let injected = serde_json::from_value::<UploadAvatarInput>(serde_json::json!({
            "content_base64": "aGVsbG8=",
            "mime": "image/png",
            "user_id": 99
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn output_serializes_avatar_version() {
        let output = UploadAvatarOutput {
            avatar_version: "0123456789abcdef0123456789abcdef".to_string(),
        };
        let value =
            serde_json::to_value(output).unwrap_or_else(|error| panic!("输出应可序列化: {error}"));
        assert_eq!(
            value.get("avatar_version"),
            Some(&serde_json::json!("0123456789abcdef0123456789abcdef"))
        );
    }
}
