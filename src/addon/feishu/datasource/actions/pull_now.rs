//! 立即拉取：请求后台**马上**跑一轮，不等下一个轮询间隔。
//!
//! # 它为什么只是「发信号」
//!
//! 本 Action **不自己拉**。它把 `datasource_id` 投进 [`FeishuPullHandle`] 的通道，
//! 由 worker 在它自己的 `select!` 里接住，走**与自动轮询完全同一条**路径
//! （`run_once` → `load_pull_tables` → `pull_table`）。这样做换来两件事：
//!
//! - **不需要互斥锁**：worker 循环是单线程的，同一时刻只可能有一轮在跑。
//!   若在这里同步调 `pull_table`，就得自己造一把 per-table 锁，还得让 worker
//!   也认那把锁——为一个按钮引入一个新的失效面。
//! - **不会撞 HTTP 超时**：同步拉一条大表可能超过 `[http].request_timeout_seconds`
//!   （默认 30 秒），届时客户端看到报错而服务端还在跑，两边对不上。
//!
//! 代价是这里**拿不到拉取结果**。控制台靠轮询数据源行的 `last_pull_at` 变化来收口，
//! 这也是为什么下面那道预检必须存在。
//!
//! # 入参是表级 `datasource_id`
//!
//! 配置单位是表，拉取单位也是表（设计 §6.2）——一张表一轮只扫一次。按 `source_key`
//! 点名一条**字段**在这里没有意义：同表 N 个字段共用一份快照，只拉其中一个既省不下
//! 那次全表扫描，又会把表级状态写成「只拉了半张表」的样子。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::pull::check_pullable;
use crate::feishu_pull::FeishuPullHandle;

/// 失败码。与 `approval_options::codes` 共用数值域，新码取 409 段（数据源类）。
mod codes {
    /// 数据源不存在。
    pub(super) const SOURCE_NOT_FOUND: i32 = 40401;
    /// 这条源现在拉不动（模式不对 / 已停用 / 坐标不全）。
    pub(super) const NOT_PULLABLE: i32 = 40903;
}

/// 触发输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PullNowInput {
    /// 要拉取的表级数据源 `id`。
    pub(super) datasource_id: i64,
}

impl ParamInput for PullNowInput {
    fn params() -> Params {
        Params::new()
    }
}

impl PullNowInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册立即拉取端点。
///
/// 与 `pull_probe` 同一取舍：**只在 `can_pull()` 为真时注册**。没有 worker 的时候
/// 这个端点只能返回「Worker 未在运行」，注册出来只会让人以为它可用。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("pull_now"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/pull-now")
        .display_name("立即拉取")
        .description("请求后台立刻跑一轮拉取（可指定单张表），不等轮询间隔")
        // 与 pull_probe 一致：它会出站调飞书、消耗频控配额，属运维动作。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: PullNowInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 1) 读那一行做**触发前预检**。
    //
    // 这一步是本 Action 存在的主要理由：`load_pull_tables` 只取
    // `ingest_mode == pull` + `status == active` + 坐标齐备的行，不满足的会被 worker
    // **静默跳过**（坐标那两条的 `continue` 只覆盖「缺项」，「模式不对」连日志都没有）。
    // 不在这里挡掉，用户点完按钮会看到前端轮询到超时，而真实原因永远浮不出来。
    let row = context
        .datasources()
        .query()
        .select_fields(&[
            "ingest_mode",
            "status",
            "bitable_base_token",
            "bitable_table_id",
        ])?
        .where_eq("id", serde_json::json!(input.datasource_id))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Ok(ApiResponse::fail(codes::SOURCE_NOT_FOUND, "数据源不存在"));
    };

    let ingest_mode = row.optional::<String>("ingest_mode")?.unwrap_or_default();
    let status = row.optional::<String>("status")?.unwrap_or_default();
    let base_token = row.optional::<String>("bitable_base_token")?;
    let table_id = row.optional::<String>("bitable_table_id")?;
    if let Err(reason) = check_pullable(
        &ingest_mode,
        &status,
        base_token.as_deref(),
        table_id.as_deref(),
    ) {
        return Ok(ApiResponse::fail(
            codes::NOT_PULLABLE,
            reason.reason().to_string(),
        ));
    }

    // 2) 发信号。worker 接住后走的是自动轮询那条路径——包括「跑完重排下一次」，
    //    所以手动跑完不会紧接着又来一轮自动的。
    ctx.tools()
        .extension::<FeishuPullHandle>()?
        .request_pull(Some(input.datasource_id))?;

    ApiResponse::success(
        serde_json::json!({ "accepted": true }),
        "已触发，后台正在拉取",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_non_positive_datasource_id() {
        // 0 / 负数都不是一个可定位的表；放行只会得到一次静默的空轮
        assert!(PullNowInput { datasource_id: 0 }.validate().is_err());
        assert!(PullNowInput { datasource_id: -3 }.validate().is_err());
        assert!(PullNowInput { datasource_id: 12 }.validate().is_ok());
    }
}
