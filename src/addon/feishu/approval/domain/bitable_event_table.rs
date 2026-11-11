//! `feishu_bitable_event` 表声明：多维表格记录变更事件缓冲。
//!
//! store-then-process：事件端点 3 秒内只做「验签 + 落表」，评论 worker 异步
//! 认领 `processed_at IS NULL` 的行。`event_id` 唯一索引承担幂等。
//!
//! `raw_payload` 里**不得**出现任何 token（`header.token` 在落库前删除）。

use yang_base::definition::{Key, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 声明多维表格事件缓冲表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_bitable_event"))
        .title("飞书多维表格事件")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            event_id => Str::new()
                .title("事件 ID")
                .require(true)
                .unique(true)
                .max_length(128)
                .filterable(true)
                .sortable(true),
            event_type => Str::new()
                .title("事件类型")
                .require(true)
                .max_length(64)
                .filterable(true),
            file_token => Str::new()
                .title("多维表格 Token")
                .require(true)
                .max_length(128)
                .indexed(true)
                .filterable(true),
            table_id => Str::new()
                .title("数据表 ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            record_id => Str::new()
                .title("记录 ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            // record_added / record_edited / record_deleted
            action => Str::new()
                .title("动作")
                .require(true)
                .max_length(32)
                .filterable(true),
            // JSON 数组文本；来源字段可能是 changed_fields / changed_field_ids。
            changed_field_ids => Text::new().title("变更字段 ID"),
            // 已删除 header.token 的报文原文。
            raw_payload => Text::new().title("原始报文"),
            received_at => Timestamp::new()
                .title("接收时间")
                .require(true)
                .sortable(true),
            // NULL = 待处理；非空 = 已处理（含「与规则无关，忽略」）。
            processed_at => Timestamp::new()
                .title("处理时间")
                .filterable(true)
                .sortable(true),
            process_error => Text::new().title("处理错误"),
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
    fn table_has_expected_name_and_identity_columns() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_bitable_event");
        assert_eq!(definition.primary_key(), "id");
        for name in [
            "event_id",
            "event_type",
            "file_token",
            "table_id",
            "record_id",
            "action",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
        assert!(
            definition
                .field("event_id")
                .is_some_and(|field| field.is_filterable()),
            "event_id 是幂等键，必须可筛选"
        );
    }

    #[test]
    fn processed_at_is_nullable_and_filterable() {
        let definition = definition();
        let field = definition
            .field("processed_at")
            .unwrap_or_else(|| panic!("processed_at 字段必须存在"));
        assert!(!field.is_required(), "NULL = 待处理");
        assert!(field.is_filterable(), "认领按该列筛选");
        assert!(field.is_sortable(), "认领按该列排序");
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
