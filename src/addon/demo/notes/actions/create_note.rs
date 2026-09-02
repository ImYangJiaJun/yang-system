//! 创建一条归属当前用户的便签。

use crate::addon::demo::domain::context::Demo;
use crate::addon::demo::notes::table::{CONTENT_MAX_LENGTH, TITLE_MAX_LENGTH};
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) CreateNoteInput {
        title: Str::new()
            .title("标题")
            .require(true)
            .min_length(1)
            .max_length(TITLE_MAX_LENGTH),
        content: Str::new()
            .title("内容")
            .max_length(CONTENT_MAX_LENGTH),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct CreateNoteResult {
    id: i64,
    title: String,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateNoteInput,
    demo: Arc<Demo>,
) -> Result<ApiResponse, BaseError> {
    let owner_user_id = ctx.actor()?.user_id();
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(BaseError::ParamInvalid(
            "title".to_string(),
            "标题不能只包含空白字符".to_string(),
        ));
    }

    // 同一事务：写便签事实（归属人取自已认证操作者）+ 追加审计。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let note_id = demo
            .notes()
            .insert_in_tx(
                &ctx,
                &mut transaction,
                owner_user_id,
                &title,
                input.content.as_deref(),
            )
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", owner_user_id)?),
            audit::entity("demo_note", note_id)?,
            None,
            Some(audit::summary([("title", json!(title))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(note_id)
    }
    .await;
    let note_id = Demo::finish_transaction(transaction, result).await?;

    ApiResponse::success(CreateNoteResult { id: note_id, title }, "便签已创建")
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, demo: Arc<Demo>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("create_note"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&demo))
        })
        .route(HttpMethod::Post, "/api/v1/demo/notes")
        .display_name("创建便签")
        .description("创建一条归属当前用户的便签")
        .permissions(["demo.notes.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_rejects_unknown_fields_and_owner_injection() {
        let injected = serde_json::from_value::<CreateNoteInput>(serde_json::json!({
            "title": "购物清单",
            "owner_user_id": 42
        }));
        assert!(injected.is_err(), "客户端不能注入 owner_user_id 等内部字段");

        let missing = serde_json::from_value::<CreateNoteInput>(serde_json::json!({
            "content": "只有内容没有标题"
        }));
        assert!(missing.is_err(), "标题是必填字段");
    }

    #[test]
    fn params_declare_title_and_content_contract() {
        let params = CreateNoteInput::params();
        let title = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "title")
            .unwrap_or_else(|| panic!("应声明 title 参数"));
        assert!(title.required);
        assert_eq!(title.validation.max_length, Some(TITLE_MAX_LENGTH));
        let content = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "content")
            .unwrap_or_else(|| panic!("应声明 content 参数"));
        assert!(!content.required);
        assert_eq!(content.validation.max_length, Some(CONTENT_MAX_LENGTH));
    }
}
