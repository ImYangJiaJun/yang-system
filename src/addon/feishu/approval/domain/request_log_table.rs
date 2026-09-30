//! `feishu_approval_request_log` 表声明：派发请求的**逐次落库记录**。
//!
//! **一次入口请求一行**：`POST /api/v1/feishu/approval/dispatch` 的每次通过鉴权的
//! 业务请求都落一行（含失败出口）。与 `feishu_approval_task`（逐条处理状态机）分工
//! 明确：本表记「请求」，task 表记「处理」。
//!
//! # 为什么坐标要冗余存储
//!
//! 配置未建成（40401 / 35600）的失败请求也要能记、能按表筛——此时 `config_id`
//! 为空，只有 `base_token` / `table_id` 还指向请求目标。
//!
//! # 不声明外键
//!
//! 同 `task_table` / `field_map_table`：外键规则被框架硬编码为 `RESTRICT` 且不可改，
//! 而 `schema_sync` 只增不删——外键一旦声明即永久存在。`config_id` 用整数列 +
//! 应用层校验。

use yang_base::definition::{Int, Key, Radio, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 派发请求记录表（第四张审批族表）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(
        TableSpec::new(yang_base::table!("feishu_approval_request_log"))
            .title("飞书审批派发请求记录")
            .fields(yang_base::fields! {
                id => Key::new().title("ID").filterable(true).sortable(true),
                // 请求人：请求体 `requested_by`；未带时落 `feishu-workflow`。
                // filterable 必开：记录页按请求人筛选——DSL 的 filterable 是 fail-closed。
                requested_by => Str::new().title("请求人").filterable(true).max_length(128),
                // 冗余存坐标：配置未建成的失败请求也能按表筛（理由见文件顶部）。
                base_token => Str::new()
                    .title("多维表格 Token")
                    .require(true)
                    .filterable(true)
                    .max_length(128),
                table_id => Str::new()
                    .title("多维表格 ID")
                    .require(true)
                    .filterable(true)
                    .max_length(128),
                // 配置存在时关联；首调失败时为空。无外键（理由见文件顶部）。
                config_id => Int::new().title("所属配置").indexed(true),
                record_id => Str::new().title("多维表格记录 ID").max_length(128),
                // 请求数据：`DispatchInput` 序列化（坐标/三件套/请求人）。
                request_body => Text::new().title("请求数据").require(true),
                // 请求结果四桶：succeeded=单条创建成功；waiting=单条必填缺失、本轮未处理；
                // accepted=批量受理；failed=失败（含参数校验/配置/凭证/业务失败）。
                // 不设默认值——每条记录都必须显式归类，写漏了宁愿报错。
                outcome => Radio::<String>::new()
                    .title("请求结果")
                    .require(true)
                    .varchar(16)
                    .options([
                        ("succeeded", "已成功"),
                        ("waiting", "等待处理"),
                        ("accepted", "已受理"),
                        ("failed", "失败"),
                    ])
                    .filterable(true),
                // 结果说明（复用响应 message；失败时为可行动原因）。
                message => Str::new().title("结果说明").require(true),
                // 单条成功时的审批单编号，独立成列便于筛选。
                serial_number => Str::new().title("审批单编号").max_length(64),
                // 返回结果：实际返回的 `data` JSON 原文。
                response_body => Text::new().title("返回数据"),
                created_at => Timestamp::new().created_at().title("创建时间").sortable(true),
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use yang_base::definition::FieldKind;
    use yang_base::table::FieldType;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_approval_request_log");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn coordinates_and_payload_are_required() {
        // 坐标与核心字段不可空：坐标是筛选请求的唯一落点（配置可能还没建成），
        // 请求数据与结果字段是「一次请求一行」这条语义的本体。
        let definition = definition();
        for name in [
            "base_token",
            "table_id",
            "request_body",
            "outcome",
            "message",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn linking_and_result_columns_are_optional() {
        // 首调失败时 config_id / record_id / serial_number 为空；failed 行没有
        // response_body；未带 requested_by 的旧工作流只写默认值。
        let definition = definition();
        for name in [
            "requested_by",
            "config_id",
            "record_id",
            "serial_number",
            "response_body",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 可空");
        }
    }

    #[test]
    fn outcome_radio_has_exactly_the_four_request_outcomes() {
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let field = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "outcome")
            .unwrap_or_else(|| panic!("outcome 字段必须存在"));
        assert_eq!(field.kind, FieldKind::Radio);
        let values: Vec<&str> = field
            .options
            .iter()
            .map(|(value, _)| value.as_str())
            .collect();
        assert_eq!(values, ["succeeded", "waiting", "accepted", "failed"]);
        // varchar 存储时物理类型是 String + 自定义校验器；枚举值域仍要过一遍编译。
        let compiled = definition();
        let outcome = compiled
            .field("outcome")
            .unwrap_or_else(|| panic!("outcome 字段必须存在"));
        assert_eq!(
            outcome.field_type(),
            &FieldType::String { max_length: 16 },
            "varchar 的 Radio 应落成 String 物理列"
        );
    }

    #[test]
    fn outcome_has_no_implicit_default() {
        // 不设默认值：每条记录都要显式归类（succeeded/waiting/accepted/failed），
        // 写漏了应该在调用点编译期/运行期暴露，而不是悄悄落进某个桶。
        let definition = definition();
        let outcome = definition
            .field("outcome")
            .unwrap_or_else(|| panic!("outcome 字段必须存在"));
        assert_eq!(outcome.default_value(), None, "outcome 不应有默认值");
    }

    #[test]
    fn coordinates_are_filterable_and_created_at_is_sortable() {
        // filterable 是记录页按坐标/结果筛选的硬依赖，sortable 是列表按时间排序的
        // 硬依赖——DSL 的这两位 fail-closed，漏了只在运行期报 FieldPermissionDenied。
        let definition = definition();
        for name in ["requested_by", "base_token", "table_id", "outcome"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_filterable(), "{name} 可筛");
        }
        let created_at = definition
            .field("created_at")
            .unwrap_or_else(|| panic!("created_at 字段必须存在"));
        assert!(created_at.is_sortable(), "created_at 可排序");
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
