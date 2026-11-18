//! 审批表单 → 多维表台账的受控反向同步。
//!
//! # 边界
//!
//! - 只处理「退回 → **同一实例**内修改并提交」（`instance_code` 不变）；
//! - 审批是事实源，台账行已锁定；写回时审批值优先；
//! - 只写配置了映射、目标列可写的字段；不可逆的聚合派生值按
//!   `aggregate_derived_mode` 显式处理；
//! - 领域层不直接读数据库/调飞书：外部依赖由调用方以 trait 注入，便于对抗测试。
//!
//! 设计见 `docs/architecture/2026-11-13-feishu-approval-form-sync-design.md`。

pub(crate) mod apply;
pub(crate) mod attachments;
pub(crate) mod detect;
pub(crate) mod encode;
pub(crate) mod error;
pub(crate) mod plan;
pub(crate) mod reconcile;
pub(crate) mod resolvers;
pub(crate) mod runtime;
pub(crate) mod store;
pub(crate) mod value;
