//! 基于 `yang-base` 的模块化单体基础系统。

pub mod addon;
pub mod app;
pub mod bootstrap;
pub mod config;
mod infrastructure;

pub use infrastructure::{audit, authorization, schema};
