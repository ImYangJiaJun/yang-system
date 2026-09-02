//! 删除一条归属当前用户的便签（跨用户目标视为不存在）。

use crate::addon::demo::domain::context::Demo;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) DeleteNoteInput {
        id: Key::new()
            .title("便签")
            .require(true),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct DeleteNoteResult {
    id: i64,
    deleted: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteNoteInput,
    demo: Arc<Demo>,
) -> Result<ApiResponse, BaseError> {
    let owner_user_id = ctx.actor()?.user_id();
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "便签 ID 必须是正整数".to_string(),
        ));
    }

    // 同一事务：按主键 + 归属人删除便签事实 + 追加审计；
    // 影响 0 行说明便签不存在或不属于当前用户，统一按不存在处理（不泄漏存在性）。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let affected = demo
            .notes()
            .delete_in_tx(&ctx, &mut transaction, input.id, owner_user_id)
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound(format!("便签 {}", input.id)));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", owner_user_id)?),
            audit::entity("demo_note", input.id)?,
            Some(audit::summary([("deleted", json!(true))])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Demo::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        DeleteNoteResult {
            id: input.id,
            deleted: true,
        },
        "便签已删除",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, demo: Arc<Demo>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("delete_note"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&demo))
        })
        .route(HttpMethod::Post, "/api/v1/demo/notes/delete")
        .display_name("删除便签")
        .description("删除一条归属当前用户的便签")
        .permissions(["demo.notes.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_rejects_any_field_beyond_id() {
        let injected = serde_json::from_value::<DeleteNoteInput>(serde_json::json!({
            "id": 7,
            "owner_user_id": 42
        }));
        assert!(injected.is_err(), "客户端不能注入 owner_user_id 等内部字段");

        let missing = serde_json::from_value::<DeleteNoteInput>(serde_json::json!({}));
        assert!(missing.is_err(), "便签 ID 是必填字段");
    }
}
