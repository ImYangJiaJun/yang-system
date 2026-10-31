//! `feishu_approval_group` 表声明：同批聚合派发的**组实例身份**。
//!
//! 组行是组实例身份的唯一来源：`uuid` / `instance_code` / `serial_number` /
//! `effect_phase` 都只落在这里，leader 任务行只承载执行簿记（租约、认领）。
//! 设计见 `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md` §4.2。

use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// 聚合组表。
///
/// # 身份只落这里
///
/// `uuid` / `instance_code` / `serial_number` / `effect_phase` 是**组实例身份**的
/// 唯一落点；leader 任务行只承载执行簿记（租约、认领），不写组实例码。
/// `effect_phase` 必须持久化：`reopen` 会把任务行的阶段重置为 `ready`，没有组行就
/// 无法表达「曾经尝试过创建」，重试会重发 create 而不是按 uuid 回捞。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_group");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批聚合组")
        .fields(yang_base::fields! {
            // `sortable`/`filterable` 与其余审批表同口径：DSL 是 fail-closed 的。
            id => Key::new().title("ID").filterable(true).sortable(true),
            config_id => Int::new()
                .title("所属配置")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 组键 = hash(config|签名摘要|排序后的成员 record_id)：成员集合变化即换键。
            group_key => Str::new()
                .title("聚合组键")
                .require(true)
                .max_length(64)
                .filterable(true),
            // 组 uuid 是 60012 响应丢失时唯一的对账键。
            uuid => Str::new()
                .title("幂等键")
                .require(true)
                .unique(true)
                .max_length(64)
                .filterable(true),
            signature_hash => Str::new()
                .title("一致性签名摘要")
                .require(true)
                .max_length(64),
            // 持久化的外部副作用标记：`ready → creating → created / lookup`。
            effect_phase => Str::new()
                .title("外部写入阶段")
                .require(true)
                .max_length(32)
                .default("ready"),
            // 事件 worker 按实例编码先查组表，filterable 是硬依赖。
            instance_code => Str::new()
                .title("审批实例 Code")
                .max_length(128)
                .filterable(true),
            serial_number => Str::new().title("审批单编号").max_length(64),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        })
        // 同一配置内组键唯一：批量点击重复播种时靠它挡住第二条组行。
        .unique_named(
            "uk_feishu_approval_group_config_key",
            [
                field_ref(&table_name, "config_id")?,
                field_ref(&table_name, "group_key")?,
            ],
        ))
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
    fn group_table_declares_config_and_group_key_unique_index() {
        // 同一配置内组键唯一：批量点击重复播种时靠它挡住第二条组行。
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let index = spec
            .indexes
            .iter()
            .find(|index| index.name.as_deref() == Some("uk_feishu_approval_group_config_key"))
            .unwrap_or_else(|| panic!("必须声明 (config_id, group_key) 复合唯一索引"));
        assert!(index.unique, "复合索引必须是唯一索引");
        let columns: Vec<&str> = index
            .fields
            .iter()
            .map(|field| field.field().as_str())
            .collect();
        assert_eq!(columns, ["config_id", "group_key"], "索引列顺序必须一致");
    }

    #[test]
    fn group_table_uuid_is_unique_and_effect_phase_defaults_to_ready() {
        // uuid 是 60012 响应丢失时唯一的对账键；effect_phase 是持久化的外部副作用
        // 标记，必须必填且有默认值（存量行与新行同一条口径）。
        let definition = definition();
        let uuid = definition
            .field("uuid")
            .unwrap_or_else(|| panic!("uuid 字段必须存在"));
        assert!(uuid.is_required(), "uuid 必填");
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let uuid_spec = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "uuid")
            .unwrap_or_else(|| panic!("uuid 字段必须存在"));
        assert!(uuid_spec.storage.unique, "uuid 必须唯一");
        let effect_phase = definition
            .field("effect_phase")
            .unwrap_or_else(|| panic!("effect_phase 字段必须存在"));
        assert!(effect_phase.is_required(), "effect_phase 必填");
        assert_eq!(
            effect_phase.default_value(),
            Some(&serde_json::json!("ready"))
        );
    }

    #[test]
    fn group_table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_approval_group");
        assert_eq!(definition.primary_key(), "id");
    }
}
