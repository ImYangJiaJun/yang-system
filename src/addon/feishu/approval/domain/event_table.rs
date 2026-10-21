//! `feishu_approval_event` 表声明——事件落地缓冲。
//!
//! store-then-process 架构：飞书推送事件 → 验签解密 → 落表 → 异步富化处理。
//! `processed_at IS NULL` 即待处理队列；`event_id` 唯一索引天然幂等。

use yang_base::definition::{Int, Key, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 连续失败多少次后放弃（置终态，不再由 worker 自动重试）。
///
/// 放在表声明旁是因为「终态」的判定完全由本表两列决定（`processed_at` 非空 +
/// `retry_count` 达上限），控制台的查询/人工重试 Action 与 worker 必须用同一个
/// 阈值，否则会造出「worker 已丢弃、控制台不认为是终态」的盲区。
pub(crate) const MAX_EVENT_RETRY_COUNT: i64 = 10;

/// 事件是否已进入失败终态（可人工重试回队）。
///
/// 成功事件也是 `processed_at` 非空，但 `retry_count < 上限`——两者必须同时
/// 判定，否则会把成功事件当成可重放对象。
pub(crate) fn is_terminal_event(processed_at: Option<i64>, retry_count: i64) -> bool {
    processed_at.is_some() && retry_count >= MAX_EVENT_RETRY_COUNT
}

/// 声明事件落地表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_event");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批事件")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            // 飞书事件的唯一标识，用于幂等去重。
            event_id => Str::new()
                .title("事件 ID")
                .require(true)
                .unique(true)
                .max_length(128)
                .filterable(true)
                .sortable(true),
            // 事件类型：approval_instance / approval_task
            event_type => Str::new()
                .title("事件类型")
                .require(true)
                .max_length(64)
                .filterable(true),
            // 审批实例编码，用于反查 task 表的 record_id + config_id
            instance_code => Str::new()
                .title("实例编码")
                .require(true)
                .max_length(128)
                .indexed(true)
                .filterable(true)
                .sortable(true),
            // 事件中的状态（事件枚举 8 值，与详情 5 值不同）
            status => Str::new()
                .title("事件状态")
                .max_length(32)
                .filterable(true),
            // 事件发生时间（飞书侧的 operate_time）
            operate_time => Timestamp::new().title("事件时间"),
            // 本服务接收时间
            received_at => Timestamp::new()
                .title("接收时间")
                .require(true)
                .sortable(true),
            // 处理完成时间；NULL = 待处理
            processed_at => Timestamp::new()
                .title("处理时间")
                .filterable(true)
                .sortable(true),
            // 原始 JSON payload，留痕排查用
            raw_payload => Text::new().title("原始报文"),
            // 重试次数；处理失败时递增
            retry_count => Int::new()
                .title("重试次数")
                .require(true)
                .default(0)
                .filterable(true),
            // 最后一次错误信息；处理失败时记录
            last_error => Text::new().title("最后错误"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间"),
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
        assert_eq!(definition.name(), "feishu_approval_event");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn event_id_is_required_and_filterable() {
        let definition = definition();
        let event_id = definition
            .field("event_id")
            .unwrap_or_else(|| panic!("event_id 字段必须存在"));
        assert!(event_id.is_required(), "event_id 必填");
        assert!(event_id.is_filterable(), "event_id 必须可筛选");
        // 唯一性由 `.unique(true)` DSL 保证，与 option 表同构（见 option/table.rs:255）；
        // FieldMetadata 不暴露 is_unique() 内省接口，改动时靠 review。
    }

    #[test]
    fn instance_code_is_indexed_and_filterable() {
        let definition = definition();
        let instance_code = definition
            .field("instance_code")
            .unwrap_or_else(|| panic!("instance_code 字段必须存在"));
        assert!(instance_code.is_required(), "instance_code 必填");
        assert!(
            instance_code.is_filterable(),
            "按实例编码反查 task 行依赖该位"
        );
    }

    #[test]
    fn processed_at_is_nullable_and_filterable() {
        let definition = definition();
        let processed_at = definition
            .field("processed_at")
            .unwrap_or_else(|| panic!("processed_at 字段必须存在"));
        assert!(
            !processed_at.is_required(),
            "processed_at 可空（NULL = 待处理）"
        );
        assert!(processed_at.is_filterable(), "按处理状态筛选依赖该位");
    }

    #[test]
    fn id_is_filterable_and_sortable() {
        // 认领查询按 id 升序取批 + 分页（`claim_events` L259 的 `order_by("id", ..)`
        // 与 `page(1, EVENT_BATCH)`），DSL 的 sortable/filterable 都是 fail-closed，
        // 漏了只在运行期报 FieldPermissionDenied——本轮生产事故复现过的。
        let definition = definition();
        let id = definition
            .field("id")
            .unwrap_or_else(|| panic!("id 字段必须存在"));
        assert!(id.is_filterable(), "id 必须可筛选（分页游标依赖）");
        assert!(id.is_sortable(), "id 必须可排序（认领升序取批依赖）");
    }

    #[test]
    fn terminal_event_requires_both_processed_at_and_retry_limit() {
        assert!(!is_terminal_event(None, 0), "待处理事件不是终态");
        assert!(
            !is_terminal_event(None, MAX_EVENT_RETRY_COUNT),
            "未置处理时间不算终态"
        );
        assert!(
            !is_terminal_event(Some(1_700_000_000), MAX_EVENT_RETRY_COUNT - 1),
            "成功事件（重试未达上限）不是终态，不得允许重放"
        );
        assert!(is_terminal_event(
            Some(1_700_000_000),
            MAX_EVENT_RETRY_COUNT
        ));
        assert!(is_terminal_event(
            Some(1_700_000_000),
            MAX_EVENT_RETRY_COUNT + 1
        ));
    }

    #[test]
    fn retry_count_defaults_to_zero() {
        let definition = definition();
        let retry_count = definition
            .field("retry_count")
            .unwrap_or_else(|| panic!("retry_count 字段必须存在"));
        assert_eq!(retry_count.default_value(), Some(&serde_json::json!(0)));
        // 终态查询（terminal_only）与人工重试的 CAS 条件都按该列过滤，漏了
        // filterable 只会在运行期报 FieldPermissionDenied。
        assert!(retry_count.is_filterable(), "retry_count 必须可筛选");
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
                "字段 {} 的展示名退化成了字段名",
                field.name()
            );
        }
    }
}
