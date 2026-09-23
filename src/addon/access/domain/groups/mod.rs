//! 权限组的领域机制层：事实 writer、有效权限解析、管理编排与引导声明。

// 组事实的受信 writer 与常量在本任务先定型；消费者（有效权限解析在 Task 4、
// 首账号引导在 Task 8、管理 Action 在 Task 10+）晚于本任务接入，
// 与 `tables.rs` / `groups/table.rs` 同例显式豁免未使用门禁。
#![allow(unused_imports)]

pub(crate) mod admin;
pub(crate) mod owner;
pub(crate) mod repository;
pub(crate) mod resolution;
pub(crate) mod tables;

pub(crate) use repository::{
    GroupRecord, GroupRepository, MAX_GROUP_MEMBERS, SYSTEM_ADMIN_GROUP_KEY,
};
