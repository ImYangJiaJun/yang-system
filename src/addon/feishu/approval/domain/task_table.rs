//! `feishu_approval_task` 表声明：审批派发的**认领队列**。
//!
//! 这张表不只是记账表。使用方选择不在多维表格里加「可提交」闸门字段，所以
//! 「这条记录是否需要处理」的唯一事实源就在这里——扫描集合由本表定义，
//! 多维表格的「审批编号」字段只作输出。
//!
//! # 状态机
//!
//! ```text
//! pending ──claim──> creating ──create 成功──> created ──回写成功──> backfilled
//!                      │                          │
//!                      │ create 终态失败           │ 回写失败（可重试）
//!                      ▼                          ▼
//!                   terminal                  回到 created 重试
//! ```
//!
//! `unnotified` 是 `terminal` 的**孪生态**：处理同样结束、同样退出认领集，区别只在
//! 于**通知没能送出去**（接收人链全空，或发送本身失败）。分成两个值是因为
//! 「处理完了但没人被告知」与「处理完了且已反馈」对运维是两件事：前者要人去捞，
//! 后者不用。合并成一个值会让前者在状态列上与后者长得一模一样。
//!
//! `creating` 是**崩溃恢复的支点**：`create` 之前先写它，所以「create 成功但本地
//! 未落库」的窗口从「编号永久丢失」降为「可判定、可续跑」。蓝绿 cutover 用
//! `docker stop`（SIGTERM → 10 秒后 SIGKILL）静默杀掉处理中的批次，没有这个状态
//! 就没有任何痕迹。

use yang_base::definition::{Int, Key, Radio, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 认领队列表。
///
/// # 两个唯一的键
///
/// - `record_id` 唯一：同一行不能有两个进行中的任务，并发点击靠它挡住。
/// - `uuid` 唯一：它是 `60012` 响应丢失时**唯一**的对账键（`serial_number` 无法
///   反查实例，官方 FAQ 明文「暂不支持通过审批单编号获取审批实例详情」）。
///
/// # 不声明外键
///
/// 外键规则被框架硬编码为 `RESTRICT` 且不可改，而 `schema_sync` 只增不删——
/// 外键一旦声明即永久存在。`config_id` 用整数列 + 应用层校验。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_approval_task"))
        .title("飞书审批派发任务")
        .fields(yang_base::fields! {
            // 排序与筛选都要：认领查询按 `id` 升序取批（`page` 分页依赖排序），
            // 按 `id` 定位要走 `where_gt`/`where_eq`。DSL 的 `sortable`/`filterable`
            // 是 **fail-closed**——漏了只在运行期报 FieldPermissionDenied，
            // 启动与单测都发现不了（见 `bootstrap` 首轮日志的那次真实失败）。
            id => Key::new().title("ID").filterable(true).sortable(true),
            config_id => Int::new()
                .title("所属配置")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 多维表格记录 id。**在一个多维表格内**唯一（全局不一定），
            // 所以 uuid 的派生键里必须带上表坐标。
            // filterable 必开：HTTP 单条派发的结果回写按 record_id 定位行
            // （`record_dispatch_task` 的 upsert），fail-closed 漏了只在运行期报错。
            record_id => Str::new()
                .title("多维表格记录 ID")
                .require(true)
                .unique(true)
                .max_length(128)
                .filterable(true),
            // 派生的幂等键。**必须持久化**：响应丢失时 instance_code 根本不存在，
            // 它是唯一还能拿回来的对账键。
            uuid => Str::new()
                .title("幂等键")
                .require(true)
                .unique(true)
                .max_length(64),
            state => Radio::<String>::new()
                .title("状态")
                .require(true)
                .varchar(16)
                .options([
                    ("pending", "待处理"),
                    ("creating", "创建中"),
                    ("created", "已创建"),
                    ("backfilled", "已回填"),
                    ("terminal", "终止"),
                    ("unnotified", "未通知"),
                    ("review", "待人工核查"),
                ])
                .default("pending")
                .filterable(true),
            // 创建成功后**立即**落库，不要等回写成功——那时响应已经丢了。
            // filterable 必开：事件 worker 用 instance_code 反查 task 行
            // （`process_event_inner` 步骤 1），fail-closed 漏了只在运行期报错。
            instance_code => Str::new()
                .title("审批实例 Code")
                .max_length(128)
                .filterable(true),
            serial_number => Str::new().title("审批单编号").max_length(64),
            // 事件订阅回写的实例状态（设计 §八 二期列）。
            instance_status => Str::new()
                .title("实例状态")
                .max_length(32)
                .filterable(true),
            // 事件订阅最近一次同步时间。
            last_synced_at => Timestamp::new().title("最近同步时间"),
            attempts => Int::new().title("已尝试次数").require(true).default(0),
            generation => Int::new().title("执行代次").require(true).default(0),
            effect_phase => Str::new().title("外部写入阶段").require(true).max_length(32).default("ready"),
            dispatch_payload => Text::new().title("受理时配置与身份快照"),
            // 可重试退避的下次可认领时间。显式写入而非自动时间戳——
            // 退避由业务决定，不是「写入时刻」。
            available_at => Timestamp::new().title("下次可认领时间"),
            // 租约。到期后可被重新抢占，因此进程崩溃不会造成永久死锁
            // （与 `authorization_outbox` 同一形态）。
            lease_until => Timestamp::new().title("租约到期时间"),
            worker_id => Str::new().title("持有租约的实例").max_length(128),
            // 落库的失败原因。**不回灌多维表格**（那张表的读者范围远大于运维，
            // 见设计 §9.2），所以系统侧的完整原因留在这里。
            last_error => Text::new().title("最近错误"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_approval_task");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn record_id_and_uuid_are_required() {
        // 两个键都不可空：record_id 是并发去重的依据，uuid 是崩溃恢复的唯一对账键。
        let definition = definition();
        for name in ["record_id", "uuid"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn record_id_is_filterable_for_http_upsert() {
        // HTTP 单条派发的结果回写按 record_id 定位行（`record_dispatch_task`），
        // DSL 的 filterable 是 fail-closed——漏了只在运行期报 FieldPermissionDenied。
        let definition = definition();
        let record_id = definition
            .field("record_id")
            .unwrap_or_else(|| panic!("record_id 字段必须存在"));
        assert!(record_id.is_filterable(), "HTTP 结果回写的 upsert 依赖该位");
    }

    #[test]
    fn state_is_filterable_and_defaults_to_pending() {
        // `filterable` 是认领查询的硬依赖（按 state 取待处理行），而 DSL 的
        // filterable 是 fail-closed——漏了只在运行期报 FieldPermissionDenied。
        let definition = definition();
        let state = definition
            .field("state")
            .unwrap_or_else(|| panic!("state 字段必须存在"));
        assert!(state.is_required());
        assert!(state.is_filterable(), "认领查询按 state 过滤依赖该位");
        assert_eq!(state.default_value(), Some(&serde_json::json!("pending")));
    }

    /// `unnotified` 必须在候选值里：Radio 是**应用层白名单**，写一个不在候选值里的
    /// 状态会在运行期被 `Validator::Custom` 拒掉，而拒掉的是「报告刚发生的事」——
    /// 表现为终态写不进去，行留在 `pending` 里每轮空转。
    #[test]
    fn state_radio_carries_the_unnotified_value_alongside_terminal() {
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let field = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "state")
            .unwrap_or_else(|| panic!("state 字段必须存在"));
        let values: Vec<&str> = field
            .options
            .iter()
            .map(|(value, _)| value.as_str())
            .collect();
        assert_eq!(
            values,
            [
                "pending",
                "creating",
                "created",
                "backfilled",
                "terminal",
                "unnotified",
                "review"
            ]
        );
        // 每个候选值都要有中文标签：控制台按状态扫列表，英文原值读不出来。
        for (value, label) in &field.options {
            assert!(!label.trim().is_empty(), "{value} 缺中文标签");
        }
    }

    #[test]
    fn attempts_defaults_to_zero() {
        let definition = definition();
        let attempts = definition
            .field("attempts")
            .unwrap_or_else(|| panic!("attempts 字段必须存在"));
        assert_eq!(attempts.default_value(), Some(&serde_json::json!(0)));
    }

    #[test]
    fn lease_and_result_columns_are_optional() {
        // 处理中与终态的行没有租约；未创建成功时没有 instance_code / serial_number。
        let definition = definition();
        for name in [
            "instance_code",
            "serial_number",
            "available_at",
            "lease_until",
            "worker_id",
            "last_error",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 可空");
        }
    }

    #[test]
    fn config_id_is_indexed_and_filterable() {
        let definition = definition();
        let config_id = definition
            .field("config_id")
            .unwrap_or_else(|| panic!("config_id 字段必须存在"));
        assert!(config_id.is_required());
        assert!(config_id.is_filterable(), "按配置取任务依赖该位");
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        let definition = definition();
        for field in definition.fields() {
            assert!(
                !field.label().is_empty(),
                "字段 {} 必须有展示名",
                field.name()
            );
            assert_ne!(
                field.label(),
                field.name(),
                "字段 {} 的展示名退化成了字段名（忘了 .title(..)）",
                field.name()
            );
        }
    }
}
