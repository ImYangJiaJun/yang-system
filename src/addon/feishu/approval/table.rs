//! `feishu_approval_config` 表声明——Schema 的唯一事实来源。
//!
//! 一条配置表达「**这张多维表格**的这批行要创建**哪个审批定义**的实例」。

use yang_base::definition::{
    FieldName, FieldRef, Key, Str, Switch, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

/// 声明审批派发配置表。
///
/// # 为什么是独立表而不是复用 `feishu_datasource`
///
/// 后者的语义是「这个字段的可选值从哪里来」（外部选项的数据源，见
/// `docs/architecture/feishu-option-ingest.md` §1）；本表的语义是「这张表的这批行要
/// 创建什么审批」。两者不共享生命周期，也不共享读写路径——复用会迫使两套完全不同的
/// 语义挤进一张表。
///
/// # JSON 落 `Text` 列
///
/// `fields!` DSL 没有 Json builder（`simple_builder!` 只实例化 9 个 builder），
/// `form_snapshot` 只能存 JSON 文本，由 `domain/` 自己 serde 转换。与
/// `feishu_option.i18n` / `extra` 同一条已知取舍。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = TableName::new("feishu_approval_config")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批派发配置")
        .fields(yang_base::fields! {
            // 配置行要按 id 定位（`dispatch_single` 的 `where_primary_key_eq` 走类型校验
            // 不走这一位），但排序用于控制台列表，一并开着与 datasource 对齐。
            id => Key::new().title("ID").filterable(true).sortable(true),
            title => Str::new()
                .title("配置名")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            // --- 多维表格坐标 ---
            //
            // 两列都必填：扫描与回写都直接打这两个路径段，缺一个就无法定位表格。
            // `filterable` 必开——入站请求按 (base_token, table_id) 反查配置做白名单
            // 校验（设计 §9.1：不校验就能用管理 Token 操作任意表格），而 DSL 的
            // `filterable` 是 fail-closed，漏了只在运行期报 FieldPermissionDenied。
            base_token => Str::new()
                .title("Base Token")
                .require(true)
                .max_length(128)
                .filterable(true),
            table_id => Str::new()
                .title("数据表 ID")
                .require(true)
                .max_length(128)
                .filterable(true),
            // --- 审批定义 ---
            approval_code => Str::new()
                .title("审批定义 Code")
                .require(true)
                .max_length(128)
                .searchable(true),
            // --- 两个关键字段的坐标 ---
            //
            // 存 **field_id** 而不是字段名：用户在多维表格里改列名会静默失配，
            // 而失配表现为「该控件取不到值」，不报错。与 `feishu_datasource_field`
            // 同一条纪律（那里也同时存 id 与 name，id 作 key、name 仅供展示）。
            applicant_field => Str::new()
                .title("申请人员字段 ID")
                .require(true)
                .max_length(128),
            backfill_field => Str::new()
                .title("回填字段 ID")
                .require(true)
                .max_length(128),
            // 多维表格日期的时区。多维表格给毫秒时间戳（不含时区），而审批 date 控件
            // 要带偏移量的 RFC3339，所以必须显式指定，不能猜。
            base_timezone => Str::new()
                .title("Base 时区")
                .require(true)
                .max_length(64)
                .default("Asia/Shanghai"),
            // --- 启用开关 ---
            enabled => Switch::new()
                .title("启用")
                .require(true)
                .default(true)
                .filterable(true),
            // --- 审批定义的控件结构快照 ---
            //
            // 保存配置时调一次「查看指定审批定义」把它存下来，用途有两个：
            // 校验映射是否覆盖全部必填控件；据 `required` 判定数据完整性。
            // 不做实时拉取——那会让每轮扫描都多打一次飞书 API。
            form_snapshot => Text::new().title("控件结构快照"),
            form_snapshot_at => Timestamp::new().title("快照时间"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        })
        // 一张表**至多一条**配置：这是自动创建语义的**唯一性保证**。
        //
        // 派发端点在配置不存在时**自动创建**（无需人工配 CRUD），所以真正的并发
        // 风险是「两个请求同时判定『不存在』」——两条都插进去会让 `approval_configs()`
        // 的查询取到不确定的一行。这条复合唯一索引让第二条以唯一键冲突失败，
        // 调用方随即改读已存在的那条。
        //
        // 用 `unique_named` 而不是两条 `.unique(true)`：后者建的是**单列**唯一索引，
        // 给不了「坐标组合唯一」的语义。
        .unique_named(
            "uk_feishu_approval_config_coords",
            [
                field_ref(&table_name, "base_token")?,
                field_ref(&table_name, "table_id")?,
            ],
        ))
}

/// 把字段名折成 `FieldRef`，供复合唯一索引用。
///
/// 与 `access::grants::table` 的同名 helper 同构——`unique_named` 收的是
/// `FieldRef` 而不是裸字符串。
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
        assert_eq!(definition.name(), "feishu_approval_config");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn table_coordinates_are_required_and_filterable() {
        // `filterable` 必开：入站请求按 (base_token, table_id) 反查配置做白名单校验，
        // 这是防越权的唯一一道。DSL 的 filterable 是 fail-closed，漏了只在运行期报
        // FieldPermissionDenied——不是启动期报错，更难查。
        let definition = definition();
        for name in ["base_token", "table_id"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填：缺一个就无法定位表格");
            assert!(field.is_filterable(), "{name} 必须可筛选，白名单校验依赖它");
        }
    }

    #[test]
    fn definition_and_field_coordinates_are_required() {
        let definition = definition();
        for name in [
            "approval_code",
            "applicant_field",
            "backfill_field",
            "base_timezone",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn enabled_defaults_to_true_and_is_filterable() {
        let definition = definition();
        let enabled = definition
            .field("enabled")
            .unwrap_or_else(|| panic!("enabled 字段必须存在"));
        assert_eq!(enabled.default_value(), Some(&serde_json::json!(true)));
        assert!(
            enabled.is_filterable(),
            "扫描时要按 enabled 过滤掉停用的配置"
        );
    }

    #[test]
    fn timezone_defaults_to_shanghai() {
        // 默认值不是「猜」的结果，是给本部署一份合理的初值；使用方可在配置时改。
        let definition = definition();
        let tz = definition
            .field("base_timezone")
            .unwrap_or_else(|| panic!("base_timezone 字段必须存在"));
        assert_eq!(
            tz.default_value(),
            Some(&serde_json::json!("Asia/Shanghai"))
        );
    }

    #[test]
    fn snapshot_columns_are_optional() {
        // 保存配置时若快照拉取失败，配置本身仍应落库（否则用户看不到失败原因）；
        // 首次扫描前会补拉。所以两列可空。
        let definition = definition();
        for name in ["form_snapshot", "form_snapshot_at"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 可空");
        }
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        // 字段没设 .title() 时展示名会退化成字段名（英文）。
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
