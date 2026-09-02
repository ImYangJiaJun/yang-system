//! 更新一条归属当前用户的便签（跨用户目标视为不存在）。

use crate::addon::demo::domain::context::Demo;
use crate::addon::demo::notes::table::{CONTENT_MAX_LENGTH, TITLE_MAX_LENGTH};
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) UpdateNoteInput {
        id: Key::new()
            .title("便签")
            .require(true),
        title: Str::new()
            .title("标题")
            .min_length(1)
            .max_length(TITLE_MAX_LENGTH),
        content: Str::new()
            .title("内容")
            .max_length(CONTENT_MAX_LENGTH),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct UpdateNoteResult {
    id: i64,
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateNoteInput,
    demo: Arc<Demo>,
) -> Result<ApiResponse, BaseError> {
    let owner_user_id = ctx.actor()?.user_id();
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "便签 ID 必须是正整数".to_string(),
        ));
    }
    let title = input.title.as_deref().map(str::trim);
    if title.is_some_and(str::is_empty) {
        return Err(BaseError::ParamInvalid(
            "title".to_string(),
            "标题不能只包含空白字符".to_string(),
        ));
    }
    if title.is_none() && input.content.is_none() {
        return Err(BaseError::ParamInvalid(
            "title".to_string(),
            "至少提供标题或内容中的一个修改项".to_string(),
        ));
    }

    // 同一事务：按主键 + 归属人更新便签事实 + 追加审计；
    // 影响 0 行说明便签不存在或不属于当前用户，统一按不存在处理（不泄漏存在性）。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let affected = demo
            .notes()
            .update_in_tx(
                &ctx,
                &mut transaction,
                input.id,
                owner_user_id,
                title,
                input.content.as_deref(),
            )
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound(format!("便签 {}", input.id)));
        }
        let mut changed_fields = Vec::new();
        if title.is_some() {
            changed_fields.push("title");
        }
        if input.content.is_some() {
            changed_fields.push("content");
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", owner_user_id)?),
            audit::entity("demo_note", input.id)?,
            None,
            Some(audit::summary([("changed_fields", json!(changed_fields))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Demo::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        UpdateNoteResult {
            id: input.id,
            changed: true,
        },
        "便签已更新",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, demo: Arc<Demo>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("update_note"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&demo))
        })
        .route(HttpMethod::Post, "/api/v1/demo/notes/update")
        .display_name("更新便签")
        .description("更新一条归属当前用户的便签")
        .permissions(["demo.notes.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_rejects_unknown_fields_and_owner_injection() {
        let injected = serde_json::from_value::<UpdateNoteInput>(serde_json::json!({
            "id": 7,
            "title": "新标题",
            "owner_user_id": 42
        }));
        assert!(injected.is_err(), "客户端不能注入 owner_user_id 等内部字段");

        let missing = serde_json::from_value::<UpdateNoteInput>(serde_json::json!({
            "title": "没有主键"
        }));
        assert!(missing.is_err(), "便签 ID 是必填字段");
    }

    #[test]
    fn params_allow_partial_updates() {
        let params = UpdateNoteInput::params();
        let id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "id")
            .unwrap_or_else(|| panic!("应声明 id 参数"));
        assert!(id.required);
        for optional in ["title", "content"] {
            let param = params
                .as_slice()
                .iter()
                .find(|param| param.name.as_str() == optional)
                .unwrap_or_else(|| panic!("应声明 {optional} 参数"));
            assert!(!param.required, "{optional} 应允许缺省以支持部分更新");
        }
    }
}
