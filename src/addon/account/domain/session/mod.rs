//! 会话持久化（路线图 C-1）：`user_session` 表的仓储边界与展示视图。

pub(crate) mod repository;

#[allow(unused_imports)] // C-1d 会话列表/撤销 Action 使用
pub(crate) use repository::{
    NewSession, SessionRepository, SessionView, SESSION_LAST_SEEN_THROTTLE_SECONDS,
};
