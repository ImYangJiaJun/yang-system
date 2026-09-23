//! 自动拉取的排程：控制台靠它回答「下次什么时候跑」。
//!
//! # 为什么需要这个端点
//!
//! 排程此前只活在 worker 进程里的一个 `Instant` 上——既没落库、也没对外暴露，
//! 于是「下次自动拉取是几点」这个问题在系统里**根本没有答案**。控制台只能显示
//! 「最近一次是什么时候」，而用户真正要判断的是「我还要等多久」。
//!
//! # 它是全局的，不是每条数据源一份
//!
//! 只有一个 worker、一个循环，下一轮的时间对**所有**数据源都相同。所以响应里
//! 没有 `source_key`——控制台在每一条源的详情页看到的都是同一个值。把它做成
//! per-source 会造出「每行各带一个其实永远相等的时间」这种假自由度。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::feishu_pull::FeishuPullHandle;

/// 查询输入：无参数。
///
/// 仍要有具名类型——框架按类型索引入参与 Schema。空结构体 + `deny_unknown_fields`
/// 让「多传了一个键」在反序列化阶段就被拒，而不是被静默忽略。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PullScheduleInput {}

impl ParamInput for PullScheduleInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 排程状态。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PullScheduleResult {
    /// 配置的自动拉取间隔（秒）。
    ///
    /// 这是**两次开跑之间的间隔**，不是承诺的完成周期：真实周期 = 本值 + 单轮耗时，
    /// 因为下一次是在一轮跑完之后才排的。
    interval_seconds: u64,
    /// 下次自动拉取的 unix 秒。
    ///
    /// `None` = 答不出来：正在拉取，或进程起来后还没跑过第一轮。两种情况对
    /// 「下次几点」是同一个答案，所以共用一个值，控制台据此显示「正在拉取」。
    next_run_at: Option<i64>,
}

/// 注册排程查询端点。
///
/// 只在 `can_pull()` 时注册：没有 worker 就没有排程可言，注册一个恒答
/// 「不知道」的端点只会让控制台显示一个假的「正在拉取」。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("pull_schedule"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/pull-schedule")
        .display_name("自动拉取排程")
        .description("查询下一次自动拉取的时间与配置的轮询间隔")
        // 纯读：不碰飞书、不消耗配额，所以与数据源列表同权限即可。
        .permissions(["feishu.datasource.read"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: PullScheduleInput,
    _context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let handle = ctx.tools().extension::<FeishuPullHandle>()?;
    let schedule = handle.schedule();

    ApiResponse::success(
        PullScheduleResult {
            interval_seconds: schedule.interval_seconds(),
            next_run_at: schedule.next_run_at(),
        },
        "查询成功",
    )
}

#[cfg(test)]
mod tests {
    //! 只放一条：排程结果的键集与契约对账（工具在 `domain/projection_contract.rs`）。
    use super::*;

    use crate::addon::feishu::domain::projection_contract;

    #[test]
    fn the_committed_contract_matches_the_schedule_struct() {
        let result = PullScheduleResult {
            interval_seconds: 900,
            next_run_at: Some(1),
        };
        projection_contract::assert_keys(
            &result,
            &["pull_schedule", "result", "emitted"],
            "自动拉取排程",
        );
    }
}
