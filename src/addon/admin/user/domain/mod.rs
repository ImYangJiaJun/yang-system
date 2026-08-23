//! 平台账号领域机制。

mod model;
mod repository;
mod service;

pub(super) use model::{AdminAccountPage, AdminAccountView};
pub(super) use repository::AdminRepository;
pub(crate) use repository::AdminSystemOwnerClaimer;
pub(super) use service::{AdminService, PasswordResetCreated};
