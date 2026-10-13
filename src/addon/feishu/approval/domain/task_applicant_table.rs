//! `feishu_approval_task_applicant` 表声明：`feishu_approval_task` 的**属性扩展**。
//!
//! 设计见 `docs/architecture/2026-10-12-feishu-approval-backfill-initiator-tracking-design.md`
//! §8：全表异步受理时拿不到发起人，open_id 要等 `get_instance` 才能解析出来。Action
//! 先把解析结果落这里，worker 再读这里回填。
//!
//! 单独成表而不是给 `feishu_approval_task` 加列——那张表是派发**认领队列**，本表是
//! 解析结果的存放点，写入方与生命周期都不同。
//!
//! 不声明 `created_at`/`updated_at`：本表是任务的属性扩展而非台账，写入即终态，
//! 没有按时间扫描的查询。

use yang_base::definition::{Int, Key, Str, TableSpec};
use yang_base::BaseError;

/// 申请人解析结果表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(
        TableSpec::new(yang_base::table!("feishu_approval_task_applicant"))
            .title("飞书审批任务申请人")
            .fields(yang_base::fields! {
                // 与 `task_table` 同一形态：`sortable`/`filterable` 是 fail-closed 的，
                // 漏了只在运行期报 FieldPermissionDenied，启动与单测都发现不了。
                id => Key::new().title("ID").filterable(true).sortable(true),
                task_id => Int::new()
                    .title("任务 ID")
                    .require(true)
                    .indexed(true)
                    .filterable(true),
                open_id => Str::new()
                    .title("发起人 Open ID")
                    .require(true)
                    .max_length(64),
            }),
    )
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
        assert_eq!(definition.name(), "feishu_approval_task_applicant");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn task_id_and_open_id_are_required() {
        // 两列都不可空：没有 task_id 无法回填，没有 open_id 这张表就没有意义。
        let definition = definition();
        for name in ["task_id", "open_id"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
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
