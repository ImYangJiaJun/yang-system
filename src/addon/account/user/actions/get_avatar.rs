//! 读取指定用户头像（需登录；前端用带 token 的 http 客户端取 JSON，不走 <img> 直链）。
//!
//! 无头像时 `etag` 与 `data_url` 都为 null；有头像时 `data_url` 为
//! `data:{mime};base64,{content_base64}`，可直接赋值给 <img src>。

use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) GetAvatarInput {
        /// 目标用户 ID。
        #[param(source = query)]
        user_id: Int::new()
            .title("用户 ID")
            .require(true),
    }
}

/// 头像读取结果：两个字段始终序列化，无头像时都为 null。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct GetAvatarOutput {
    etag: Option<String>,
    data_url: Option<String>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: GetAvatarInput,
    account: Arc<Account>,
) -> Result<GetAvatarOutput, BaseError> {
    ctx.authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?;
    let record = account.avatars().find_by_user(&ctx, input.user_id).await?;
    Ok(match record {
        Some(record) => GetAvatarOutput {
            etag: Some(record.etag),
            data_url: Some(format!(
                "data:{};base64,{}",
                record.mime, record.content_base64
            )),
        },
        None => GetAvatarOutput {
            etag: None,
            data_url: None,
        },
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("get_avatar"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Get, "/api/v1/users/avatar")
        .display_name("读取头像")
        .description("读取指定用户头像（JSON 内嵌 data URL，无头像时字段为 null）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_user_id() {
        let params = <GetAvatarInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["user_id"]);
        assert!(params.as_slice().iter().all(|param| param.required));
        // GET 无请求体：user_id 必须声明为 query 来源，否则浏览器
        // `?user_id=N` 会因缺少 body 参数被 400 拒绝（回归守护）。
        assert_eq!(
            params.as_slice()[0].source,
            yang_base::definition::ParamSource::Query
        );
        let injected = serde_json::from_value::<GetAvatarInput>(serde_json::json!({
            "user_id": 7,
            "extra": 1
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn output_serializes_null_fields_for_missing_avatar() {
        let empty = GetAvatarOutput {
            etag: None,
            data_url: None,
        };
        let value =
            serde_json::to_value(empty).unwrap_or_else(|error| panic!("输出应可序列化: {error}"));
        assert_eq!(value.get("etag"), Some(&serde_json::Value::Null));
        assert_eq!(value.get("data_url"), Some(&serde_json::Value::Null));

        let present = GetAvatarOutput {
            etag: Some("e".to_string()),
            data_url: Some("data:image/png;base64,AAAA".to_string()),
        };
        let value =
            serde_json::to_value(present).unwrap_or_else(|error| panic!("输出应可序列化: {error}"));
        assert_eq!(
            value.get("data_url"),
            Some(&serde_json::json!("data:image/png;base64,AAAA"))
        );
    }
}
