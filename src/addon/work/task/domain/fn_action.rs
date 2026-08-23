//! 函数式 CRUD 写 Handler 的 `DynAction` 桥接。
//!
//! `ModuleSpec::crud_at_with_mutations` 用表定义为写 Action 生成动态 Schema 与
//! 权限契约，只接受 `DynAction` 实例；yang-base 的 `FnAction` 仅供 `action_fn`
//! 通道内部使用。本桥接把 `pub(super) async fn handle` 形态的函数式 Handler
//! 包装成 `DynAction`，与 derive 通道保持同构语义：请求体只解码一次，输出统一
//! 收口进 `data`。Catalog 契约（动态表驱动 Schema、权限、success_status）仍由
//! `crud_at_with_mutations` 生成，不读取本桥接的占位元信息。

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::any::{Any, TypeId};
use std::future::Future;
use std::marker::PhantomData;
use std::sync::OnceLock;
use yang_base::action::{ActionContext, ActionMeta, ApiResponse, DynAction, PermissionMode};
use yang_base::BaseError;

/// 由普通 async fn 承载的 CRUD 写 Action。
pub(in crate::addon::work::task) struct FnAction<F, I, O> {
    name: &'static str,
    handler: F,
    marker: PhantomData<fn(I) -> O>,
}

impl<F, I, O> FnAction<F, I, O> {
    /// 绑定 Action 名（仅用于 tracing span）与业务函数。
    pub(in crate::addon::work::task) fn new(name: &'static str, handler: F) -> Self {
        Self {
            name,
            handler,
            marker: PhantomData,
        }
    }
}

/// `DynAction::meta` 的占位实现：真实契约由 `crud_at_with_mutations` 写入
/// `ActionSpec` 后 `bind_handler_contract` 会跳过 `meta()`，本值仅为满足
/// trait 签名，无运行期语义。
fn placeholder_meta() -> &'static ActionMeta {
    static INPUT_SCHEMA: OnceLock<schemars::schema::RootSchema> = OnceLock::new();
    static OUTPUT_SCHEMA: OnceLock<schemars::schema::RootSchema> = OnceLock::new();
    static META: OnceLock<ActionMeta> = OnceLock::new();
    META.get_or_init(|| {
        ActionMeta::new(
            "<functional>",
            "<functional>",
            "函数式 CRUD 写 Handler 的占位元信息；真实定义以注册期 ActionSpec 为准",
            &[],
            PermissionMode::All,
            false,
            INPUT_SCHEMA.get_or_init(|| schemars::schema_for!(serde_json::Value)),
            OUTPUT_SCHEMA.get_or_init(|| schemars::schema_for!(serde_json::Value)),
        )
    })
}

#[async_trait]
impl<F, I, O, Fut> DynAction for FnAction<F, I, O>
where
    F: Fn(ActionContext, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, BaseError>> + Send,
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
{
    async fn dispatch(&self, ctx: ActionContext) -> Result<ApiResponse, BaseError> {
        use tracing::Instrument;

        let span = tracing::info_span!("handle", action = self.name);
        async {
            let mut ctx = ctx;
            // 与 derive 通道的 TypedHandler::decode_input 一致：body 只反序列化一次。
            let body = std::mem::take(&mut ctx.request.body);
            let input: I = serde_json::from_value(body)
                .map_err(|error| BaseError::ParamInvalid("body".to_string(), error.to_string()))?;
            let output = (self.handler)(ctx, input).await?;
            // 写 Action 输出均为普通 JSON（InsertResult/AffectedResult），统一进 data。
            ApiResponse::success(output, "成功")
        }
        .instrument(span)
        .await
    }

    fn meta(&self) -> &'static ActionMeta {
        placeholder_meta()
    }

    fn input_type_id(&self) -> TypeId {
        TypeId::of::<I>()
    }

    fn output_type_id(&self) -> TypeId {
        TypeId::of::<O>()
    }

    async fn call_boxed(
        &self,
        ctx: ActionContext,
        input: Box<dyn Any + Send>,
    ) -> Result<Box<dyn Any + Send>, BaseError> {
        let input = input.downcast::<I>().map_err(|_| {
            BaseError::ConfigError("函数式 CRUD 写 Action 的内部调用输入类型不匹配".to_string())
        })?;
        let output = (self.handler)(ctx, *input).await?;
        Ok(Box::new(output))
    }
}
