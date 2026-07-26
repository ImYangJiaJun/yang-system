//! 基于 `yang-base` 的模块化单体基础系统。

pub mod authorization;

pub mod app;
pub mod audit;
pub mod bootstrap;
pub mod bootstrap_secret;
pub mod config;
mod config_source;
pub mod migrations;
pub mod modules;
mod observability;
mod security;
mod shutdown;
pub mod transport;
