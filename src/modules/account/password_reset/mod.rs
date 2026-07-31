//! 密码重置凭证领域模块。

mod repository;

pub(crate) use repository::{
    consume_in_tx, create_in_tx, find_target_user, invalid_reset_token, lock_in_tx,
    GeneratedPasswordReset, PasswordResetReference,
};
