//! 审批评论回写的领域层：配置、条件判定与正文渲染。
//!
//! 飞书出站调用（评论创建/查询、事件订阅）放在 `domain/approval.rs`；本模块
//! 只保留纯逻辑，供保存期校验、worker 与运维 Action 共用。

pub(crate) mod config;
pub(crate) mod render;

pub(crate) use config::{parse_comment_config, validate_comment_config, CommentConfig};
pub(crate) use render::{
    evaluate_condition, find_marker, payment_fingerprint, render_comment, CommentCondition,
    CommentFile, CommentItem,
};
