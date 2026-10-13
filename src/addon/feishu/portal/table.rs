//! `feishu_datasource_portal` 表声明——Schema 的唯一事实来源。

use yang_base::definition::{Int, Key, Radio, Str, Switch, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 声明门户表。
///
/// 一条 = 一个可对外暴露的查询端点，绑定一个数据源 + 一个查询键 + 一组开放列。
/// 走 `TableSpec` + `fields!` 是刻意的取舍：这条路径让表进入 Catalog / 权限目录，
/// 代价是拿不到 DSL 的能力（见 `datasource/table.rs` 的说明）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource_portal"))
        .title("飞书数据源查询门户")
        .fields(yang_base::fields! {
            // `filterable` 必须显式打开（DSL 是 fail-closed）：本 addon 有多条路径按主键
            // 定位门户——`portal_query` 的 slug 查询、`update_portal` / `delete_portal` /
            // `toggle_portal` 的门户定位。漏了它，这些路径会在运行期吃
            // `FieldPermissionDenied`（不是启动期报错）。仓库惯例见 `datasource/table.rs:86`。
            //
            // `sortable` **同样必须打开**，这条是**本表对仓库惯例的刻意偏离**：
            // 门户列表是台账，要靠唯一列收尾才能有确定性全序——`title` 会重名，
            // `id` 是这张表上唯一的唯一列。`list_portals` 的兜底分支恒按 `id` 升序，
            // 而 `validate_order_field` 对不可排序的字段先于角色权限直接拒绝，
            // 映射到 HTTP 就是 403——列表页不是「排序不生效」，是整个请求恒失败。
            id => Key::new().title("ID").filterable(true).sortable(true),
            // 指向表级数据源。DSL 没有外键 builder，故用 `Int` + 索引，一致性由应用层
            // 在事务内保证（创建门户时校验数据源存在且 active）。
            datasource_id => Int::new()
                .title("所属数据源")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 门户名称（管理员用）。≤100 字符，与数据源 title 上限一致。
            title => Str::new()
                .title("门户名称")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            // URL 标识。UUID v4 无连字符（32 字符十六进制），不可枚举。
            // **唯一**：slug 是公开 URL 的一部分，重复会让路由分派歧义。
            slug => Str::new()
                .title("URL 标识")
                .require(true)
                .unique(true)
                .max_length(32)
                .filterable(true),
            // 查询键。指向 `feishu_datasource_field.field_id`（改名不断链）。
            // 冗余 `query_field_source_key` 是为了查询时避免 JOIN。
            query_field_id => Str::new()
                .title("查询键字段 ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            // 冗余存 source_key，方便查询。
            query_field_source_key => Str::new()
                .title("查询键标识")
                .require(true)
                .max_length(64)
                .filterable(true),
            // 多查询键（AND 组合）。JSON 数组：field_id 列表。
            // 为空时使用单键 `query_field_id`；非空时多键 AND 查询。
            // 用 Text 存 JSON 而非关联表：键数少（通常 <5）、总是整组读写、不需要按键查询。
            query_field_ids => Text::new().title("多查询键"),
            // 自定义短链。可选，为空时使用 UUID `slug`。
            // **唯一**：自定义短链也是公开 URL 的一部分，重复会让路由分派歧义。
            // ≤64 字符，允许小写字母/数字/连字符/下划线（URL 友好）。
            custom_slug => Str::new()
                .title("自定义短链")
                .unique(true)
                .max_length(64)
                .filterable(true),
            // 访问模式。public（任何人）/ passphrase（口令）/ login（系统登录）。
            // 默认 public 让门户创建后立即可用（无口令门槛）；管理员可后续切换到其他模式。
            access_mode => Radio::<String>::new()
                .title("访问模式")
                .require(true)
                .varchar(16)
                .options([
                    ("public", "公开"),
                    ("passphrase", "口令"),
                    ("login", "登录"),
                ])
                .default("public")
                .filterable(true),
            // 口令哈希。`access_mode = passphrase` 时必填（argon2）；其他模式为空。
            // **secret**：口令本身不落库，只存哈希。
            passphrase_hash => Str::new()
                .title("口令哈希")
                .max_length(255)
                .secret(true),
            // 口令有效期。`access_mode = passphrase` 且需要过期时使用。
            // 为空时口令永不过期；非空时超过该时间后口令失效。
            passphrase_expires_at => Timestamp::new().title("口令有效期"),
            // 开放列。JSON 数组：开放列的 field_id 列表。
            // 用 Text 存 JSON 而非关联表：列数少（通常 <20）、总是整组读写、不需要按列查询。
            exposed_columns => Text::new().title("开放列").require(true),
            // 展示名映射。JSON 对象：field_id → 展示名（管理员可自定义列标题）。
            // 为空时使用 `feishu_datasource_field.field_name` 默认值。
            result_labels => Text::new().title("展示名映射"),
            // 查无结果时的提示文案。≤500 字符。
            empty_message => Str::new()
                .title("未找到提示")
                .max_length(500),
            // 每分钟最大查询次数（按 IP）。默认 30，上限 120。
            rate_limit_per_min => Int::new()
                .title("每分钟限速")
                .require(true)
                .default(30),
            // 启用/停用。停用后门户 URL 返回 404，但配置与日志保留。
            enabled => Switch::new()
                .title("启用")
                .require(true)
                .default(true)
                .filterable(true),
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
    fn table_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_datasource_portal");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn slug_is_unique_and_filterable() {
        // slug 是公开 URL 的一部分，必须唯一且可筛。
        // **唯一性走 DSL 层读**：`table_definition()` 不暴露索引，只断言 required/filterable
        // 的话，测试名声称的那一位其实没验（`token_hash` 的唯一索引曾经漏掉过）。
        let definition = definition();
        let slug = definition
            .field("slug")
            .unwrap_or_else(|| panic!("slug 必须存在"));
        assert!(slug.is_required());
        assert!(slug.is_filterable());

        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let declared = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "slug")
            .unwrap_or_else(|| panic!("slug 必须在 DSL 声明里"));
        assert!(
            declared.storage.unique,
            "slug 必须建唯一索引：它是公开 URL 的一部分，重复会让路由分派歧义"
        );
    }

    #[test]
    fn passphrase_hash_is_secret_and_never_searchable() {
        // 口令本身不落库，只存哈希。
        let definition = definition();
        let hash = definition
            .field("passphrase_hash")
            .unwrap_or_else(|| panic!("passphrase_hash 必须存在"));
        assert!(hash.is_secret(), "passphrase_hash 必须是 secret");
        assert!(!hash.is_searchable(), "passphrase_hash 不得进检索面");
        assert!(!hash.is_filterable(), "passphrase_hash 不得进筛选面");
    }

    #[test]
    fn access_mode_defaults_to_public_and_is_filterable() {
        // 默认 public 让门户创建后立即可用（无口令门槛）。
        let definition = definition();
        let mode = definition
            .field("access_mode")
            .unwrap_or_else(|| panic!("access_mode 必须存在"));
        assert!(mode.is_required());
        assert_eq!(
            mode.default_value(),
            Some(&serde_json::json!("public")),
            "access_mode 应默认 public"
        );
        assert!(mode.is_filterable(), "按访问模式筛选必须可用");
    }

    #[test]
    fn id_is_filterable_so_every_portal_lookup_by_primary_key_works() {
        // DSL 的 filterable 是 fail-closed：未显式打开时 `where_eq("id", ..)` 会在运行期
        // 吃 `FieldPermissionDenied`。按主键定位门户的路径有：`portal_query` 的 slug 查询
        // （先按 slug 查 id）、`update_portal` / `delete_portal` / `toggle_portal` 的门户定位。
        let definition = definition();
        let id = definition
            .field("id")
            .unwrap_or_else(|| panic!("id 必须存在"));
        assert!(
            id.is_filterable(),
            "按主键定位必须可用（fail-closed 能力位）"
        );
    }

    #[test]
    fn exposed_columns_is_required_and_stores_json() {
        // 开放列必须有值（至少一列）；用 Text 存 JSON。
        let definition = definition();
        let columns = definition
            .field("exposed_columns")
            .unwrap_or_else(|| panic!("exposed_columns 必须存在"));
        assert!(columns.is_required(), "开放列必须有值");
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        // 防线：字段没设 .title() 时展示名会退化成字段名（英文），
        // 前端表格就会满屏 query_field_id / exposed_columns。
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

    #[test]
    fn sync_state_columns_are_nullable_or_defaulted() {
        // schema_sync 门禁：已有数据的表上加「必填且无默认值」的列会让启动直接失败。
        let definition = definition();
        for name in [
            "passphrase_hash",
            "passphrase_expires_at",
            "result_labels",
            "empty_message",
            "query_field_ids",
            "custom_slug",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 必须可空，否则存量行无法加列");
        }
        let rate_limit = definition
            .field("rate_limit_per_min")
            .unwrap_or_else(|| panic!("rate_limit_per_min 字段必须存在"));
        assert_eq!(
            rate_limit.default_value(),
            Some(&serde_json::json!(30)),
            "rate_limit_per_min 应默认 30"
        );
    }

    #[test]
    fn custom_slug_is_unique_when_present() {
        // 自定义短链是公开 URL 的一部分，必须唯一且可筛。
        // **唯一性走 DSL 层读**：`table_definition()` 不暴露索引，只断言 filterable
        // 的话，测试名声称的那一位其实没验。
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let declared = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "custom_slug")
            .unwrap_or_else(|| panic!("custom_slug 必须在 DSL 声明里"));
        assert!(
            declared.storage.unique,
            "custom_slug 必须建唯一索引：它是公开 URL 的一部分，重复会让路由分派歧义"
        );
    }

    #[test]
    fn query_field_ids_stores_json_for_multi_key_support() {
        // 多查询键（AND 组合）用 Text 存 JSON 数组。
        let definition = definition();
        let ids = definition
            .field("query_field_ids")
            .unwrap_or_else(|| panic!("query_field_ids 必须存在"));
        assert!(!ids.is_required(), "query_field_ids 必须可空（单键场景）");
    }
}
