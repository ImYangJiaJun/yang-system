//! 基于 `yang-base` 的模块化单体基础系统。

pub mod authorization;

pub mod app;
pub mod audit;
pub mod bootstrap;
pub mod config;
mod config_source;
pub mod migrations;
pub mod modules;
