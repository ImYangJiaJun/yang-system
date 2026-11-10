//! `feishu_approval_export_run` 表声明：回写运行台账（规则级 + 行级同表）。
//!
//! - `row_key = __rule__`：一个 (规则, 实例) 的触发/条件/重试状态；
//! - `row_key = row:N`：一条明细行对应一条目标记录的写入/标记状态。
//!
//! 唯一键含 `generation`：正常重试复用同一 generation 与 client_token；
//! 目标行被手工删除后的“强制重建”递增 generation，换新 token 新建一行。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

/// 声明回写运行台账表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_export_run");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批回写运行")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            rule_id => Int::new()
                .title("所属规则")
                .require(true)
                .indexed(true)
                .filterable(true),
            approval_code => Str::new()
                .title("审批定义 Code")
                .require(true)
                .max_length(128)
                .filterable(true),
            instance_code => Str::new()
                .title("实例编码")
                .require(true)
                .max_length(128)
                .indexed(true)
                .filterable(true)
                .sortable(true),
            // 规则级行存 __rule__；行级行存实际匹配的 node_id 或 __final__。
            trigger_node_id => Str::new()
                .title("触发节点 ID")
                .require(true)
                .max_length(128)
                .filterable(true),
            // 规则级 __rule__；行级 row:0、row:1 …
            //
            // filterable 是 worker 认领规则级行的必需位（fail-closed）：到期队列
            // 只处理 `row_key=__rule__` 的行，漏了这一位会在运行期报
            // FieldPermissionDenied，整条回写队列停摆。
            row_key => Str::new()
                .title("行键")
                .require(true)
                .max_length(64)
                .filterable(true),
            generation => Int::new()
                .title("重建代次")
                .require(true)
                .default(0)
                .filterable(true)
                .sortable(true),
            // queued / waiting_node / waiting_final / unmatched / pending /
            // writing / written / partial / retry / review / terminal / skipped
            write_status => Str::new()
                .title("写入状态")
                .require(true)
                .max_length(32)
                .default("queued")
                .filterable(true),
            // none / valid / rolled_back / rejected / canceled / reverted /
            // deleted / marking / mark_failed
            mark_status => Str::new()
                .title("标记状态")
                .require(true)
                .max_length(32)
                .default("none")
                .filterable(true),
            applied_label => Str::new().title("已写状态文案").max_length(64),
            // 确定性 UUIDv4；规则级行为 NULL。MySQL 唯一索引允许多个 NULL。
            client_token => Str::new()
                .title("客户端幂等 Token")
                .max_length(64)
                .unique(true),
            payload_hash => Str::new().title("行内容哈希").max_length(64),
            target_record_id => Str::new().title("目标记录 ID").max_length(128),
            attempts => Int::new()
                .title("尝试次数")
                .require(true)
                .default(0),
            next_attempt_at => Timestamp::new()
                .title("下次尝试时间")
                .filterable(true)
                .sortable(true),
            lease_until => Timestamp::new().title("租约到期").filterable(true),
            worker_id => Str::new().title("处理者").max_length(128),
            last_error => Text::new().title("最后错误"),
            written_at => Timestamp::new().title("写入时间"),
            marked_at => Timestamp::new().title("标记时间"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        })
        .unique_named(
            "uk_feishu_approval_export_run",
            [
                field_ref(&table_name, "rule_id")?,
                field_ref(&table_name, "instance_code")?,
                field_ref(&table_name, "trigger_node_id")?,
                field_ref(&table_name, "row_key")?,
                field_ref(&table_name, "generation")?,
            ],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
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
        assert_eq!(definition.name(), "feishu_approval_export_run");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn unique_key_columns_are_required() {
        let definition = definition();
        for name in [
            "rule_id",
            "instance_code",
            "trigger_node_id",
            "row_key",
            "generation",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填（唯一键不能有 NULL）");
        }
    }

    #[test]
    fn worker_query_columns_are_filterable_and_sortable() {
        let definition = definition();
        for name in [
            "write_status",
            "mark_status",
            "next_attempt_at",
            "lease_until",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_filterable(), "{name} 必须可筛选");
        }
        let next_attempt_at = definition
            .field("next_attempt_at")
            .unwrap_or_else(|| panic!("next_attempt_at 字段必须存在"));
        assert!(next_attempt_at.is_sortable(), "到期队列按该列排序");
    }

    #[test]
    fn defaults_match_the_run_state_machine() {
        let definition = definition();
        assert_eq!(
            definition
                .field("write_status")
                .and_then(|field| field.default_value().cloned()),
            Some(serde_json::json!("queued"))
        );
        assert_eq!(
            definition
                .field("mark_status")
                .and_then(|field| field.default_value().cloned()),
            Some(serde_json::json!("none"))
        );
        assert_eq!(
            definition
                .field("generation")
                .and_then(|field| field.default_value().cloned()),
            Some(serde_json::json!(0))
        );
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
