//! 密码重置凭证领域模块。

mod repository;

pub(crate) use repository::{
    consume_in_tx, find_target_user, insert_issued, invalid_reset_token, lock_in_tx,
    IssuedPasswordReset, LockedPasswordReset, PasswordResetReference,
};
