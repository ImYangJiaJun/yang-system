//! `feishu.approval` module 的 Action 注册表。
//!
//! 架构门禁要求 `actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。
//!
//! # 当前为空
//!
//! 派发端点（`dispatch`）在后续任务落地；本文件先建立 module 形状，
//! 让三张表能进 schema 并接受启动期的同步校验。
//!
//! # 端点只在集成可用时注册
//!
//! 与 `option` module 的机器入口同一条纪律：派发端点打飞书开放平台，需要
//! `app_id`/`app_secret` 齐备（`FeishuSettings::can_pull()` 判定）。凭证缺失时
//! 注册出来只会返回错误，让人误以为可用；不注册则路由直接 404，语义更准。

#![allow(dead_code)] // 注册表先建立 module 形状；派发端点在后续任务落地。

use yang_base::definition::ModuleSpec;

/// 注册本 module 的全部 Action。
///
/// 暂无 Action —— 派发端点（`dispatch`）在后续任务加入。空注册表让三张表能进
/// schema 并接受启动期的同步校验。
pub(super) fn register_all(module: ModuleSpec) -> ModuleSpec {
    module
}
