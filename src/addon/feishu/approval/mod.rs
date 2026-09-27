//! 飞书审批实例派发：配置、字段映射与认领队列。
//!
//! 本模块让多维表格里的一行业务数据由本服务自动创建对应的飞书原生审批实例，
//! 并把 `serial_number` 回填到该行。
//!
//! # 为什么是三张表
//!
//! - `feishu_approval_config`：一条配置 = 一个多维表格 ↔ 一个审批定义。
//! - `feishu_approval_field_map`：审批控件 ↔ 多维表格字段的映射（一对多）。
//! - `feishu_approval_task`：**认领队列**，不只是记账表。
//!
//! # 为什么「是否已处理」的判据在库里，而不是多维表格的字段上
//!
//! 使用方选择不在多维表格里加「可提交」闸门字段（见设计 §11 限制 1）。若以
//! 「回填字段为空」作扫描键，会有两个静默故障：
//!
//! 1. 用户先填业务字段、后填「申请人」的那一轮被扫到，必填缺失被写成终态错误，
//!    字段从此非空、**该行再也不会被处理**，唯一补救是人工逐行清空；
//! 2. 回写失败的记录无法退出扫描集，每轮被重新捞出、反复撞 `60012`。
//!
//! 所以扫描集合由 `feishu_approval_task` 定义，多维表格的「审批编号」只作输出字段。
//!
//! # 部署形态是单实例
//!
//! 与 `feishu_pull` 同一条决策（那里的 A10）：`deploy/deploy-blue-green.sh` 的
//! `cutover` 先 `stop_pair` 停线上再起新的，两实例不重叠。因此**不做跨实例单飞、
//! 不做表级互斥锁**；后台只有一个单线程循环，手动触发只往它的通道投信号。
//!
//! # 成对省略 `view()` 与 `presentation()`
//!
//! 控制台入口是自建页面（`frontend/src/features/feishu/`），不是引擎投影的通用
//! TableView。只省一个会让表静默出现在前端——理由与 `feishu.datasource` 同
//! （见 `datasource/mod.rs:5-19`）。

pub(crate) mod actions;
pub(crate) mod domain;
pub(crate) mod table;

use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 本 module 的名字。
const MODULE: &str = "feishu.approval";

/// 装配 `feishu.approval` Module。
///
/// 暂不接收 `FeishuContext`：本任务只落地三张表，派发端点与其表访问在后续任务
/// 一并决定归属（扩展现有 `FeishuContext` 还是给审批单开一个）。现在传一个用不到
/// 的 context 只会制造假依赖。
pub(crate) fn build_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(module_name()?).table(table::table_spec()?))
}

/// 字段映射表所在的 Module：只有表，没有 Action。
///
/// 「一张表 = 一个 module」是框架的硬形状（`ModuleSpec::table()` 是单个
/// `Option<TableSpec>`，`AddonSpec` 没有挂独立表的入口），而绑定表没有自己的
/// Action——它的读写由本 module 的 Action 经 `FeishuContext` 跨表完成。
///
/// 构造器与主表并列放在这里而不是自己的目录：架构门禁只把**含 `actions/` 的目录**
/// 认定为 module，独立目录会被判成「游离的机制目录」（`scripts/check_architecture.py:291`）。
pub(crate) fn build_field_map_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(field_map_module_name()?).table(domain::field_map_table::table_spec()?))
}

/// 认领队列表所在的 Module：同上，只有表。
pub(crate) fn build_task_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(task_module_name()?).table(domain::task_table::table_spec()?))
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(config_error)
}

fn field_map_module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new("feishu.approval_field_map").map_err(config_error)
}

fn task_module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new("feishu.approval_task").map_err(config_error)
}

fn config_error(error: impl std::fmt::Display) -> BaseError {
    BaseError::ConfigError(error.to_string())
}
