//! `feishu_datasource` 表声明——Schema 的唯一事实来源。

use yang_base::definition::{Int, Key, Radio, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 声明数据源注册表。
///
/// 走 `TableSpec` + `fields!` 是刻意的取舍：这条路径让表进入 Catalog / 前端零代码
/// TableView / 权限目录，代价是拿不到 DSL 的能力（见 `option/table.rs` 的说明）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource"))
        .title("飞书数据源")
        .fields(yang_base::fields! {
            // `filterable` 必须显式打开（DSL 是 fail-closed）：本 addon 有一打以上的
            // 路径按主键定位表级行——`pull.rs` 的表级状态回写、`pull_now` /
            // `pull_probe` / `health_check` / `update_datasource_table` /
            // `delete_datasource_table` 的数据源定位，以及出站读端取 `status`。
            // 漏了它，这些路径会在运行期吃 `FieldPermissionDenied`（不是启动期报错）。
            // 仓库惯例见 `account/user/table.rs:86`。
            //
            // `sortable` **同样必须打开**，这条是**本表对仓库惯例的刻意偏离**：
            // 别处的 `id` 都只有 `filterable`（排序主键对人没有意义），但本表是
            // 「一表一源」的台账，列表要靠**唯一列**收尾才能有确定性全序——
            // `title` 会重名，`id` 是这张表上唯一的唯一列。
            // `list_datasources` 的兜底分支与前端 `withStableOrder` 都恒按 `id` 升序，
            // 而 `validate_order_field` 对不可排序的字段**先于角色权限**直接拒绝
            // （`table_query/validation.rs`），映射到 HTTP 就是 403——列表页不是
            // 「排序不生效」，是整个请求恒失败。
            id => Key::new().title("ID").filterable(true).sortable(true),
            title => Str::new()
                .title("名称")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            status => Radio::<String>::new()
                .title("状态")
                .require(true)
                .varchar(16)
                .options([("active", "启用"), ("disabled", "停用")])
                .filterable(true)
                .default("active"),
            // 取数方式。默认 push：存量数据源靠多维表格工作流推送，语义不变。
            // `filterable` 必开——轮询要按 `ingest_mode = "pull"` 选出待拉取的数据源，
            // 而 DSL 的 filterable 是 fail-closed，漏了会在运行期被
            // FieldPermissionDenied 打回（不是启动期报错，更难查）。
            ingest_mode => Radio::<String>::new()
                .title("取数方式")
                .require(true)
                .varchar(16)
                .options([("push", "手工推送"), ("pull", "定时拉取")])
                .default("push")
                .filterable(true),
            // --- 多维表格坐标：三个路径段 ---
            //
            // 取数列不再在这里：一条数据源现在是一张表，取数是它下面**每条字段绑定**
            // 各自的事（`field_table.rs`）。这里只留表级坐标与表级同步状态。
            bitable_base_token => Str::new()
                .title("Base Token")
                .max_length(128)
                .filterable(true),
            bitable_table_id => Str::new()
                .title("数据表 ID")
                .max_length(128)
                .filterable(true),
            bitable_view_id => Str::new().title("视图 ID").max_length(128),
            // --- 同步状态（控制台台账与告警用） ---
            // 失败与时间戳是**表级**的：一轮拉取以表为单位，失败也是整表一起停。
            last_pull_at => Timestamp::new().title("最近拉取时间"),
            last_success_at => Timestamp::new().title("最近同步成功时间").sortable(true),
            consecutive_failures => Int::new().title("连续失败次数").default(0),
            last_error => Text::new().title("最近错误"),
            // **已废弃**：摘要归属已改为「每条字段绑定一份」，见 `field_table.rs` 的
            // `snapshot_digest`。此列保留只为避免一次破坏性的列删除，不再被读写。
            snapshot_digest => Str::new().title("快照摘要（已废弃）").max_length(64),
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
        assert_eq!(definition.name(), "feishu_datasource");
        // 表必须定义主键，否则 schema-first build 阶段直接失败
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn status_is_required_with_active_default() {
        let definition = definition();
        let status = definition
            .field("status")
            .unwrap_or_else(|| panic!("status 字段必须存在"));
        assert!(status.is_required(), "status 必填");
        assert_eq!(
            status.default_value(),
            Some(&serde_json::json!("active")),
            "status 应默认 active"
        );
    }

    #[test]
    fn timestamps_are_managed_by_the_framework() {
        let definition = definition();
        for name in ["created_at", "updated_at"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 应由框架自动写入");
        }
    }

    #[test]
    fn id_is_filterable_so_every_row_lookup_by_primary_key_works() {
        // DSL 的 filterable 是 fail-closed：未显式打开时 `where_eq("id", ..)` 会在
        // 运行期被 `FieldPermissionDenied` 打回（不是启动期报错）。本 addon 里按主键
        // 定位表级行的路径有一打以上——`pull.rs` 的表级状态回写、`pull_now` / `pull_probe`
        // / `health_check` / `update_datasource_table` / `delete_datasource_table` 的
        // 数据源定位，以及出站读端取 `status`——漏了这一位等于把整条控制面打成恒失败。
        // 仓库惯例见 `account/user/table.rs:86`、`access/grants/table.rs:22`。
        let definition = definition();
        let id = definition
            .field("id")
            .unwrap_or_else(|| panic!("id 字段必须存在"));
        assert!(
            id.is_filterable(),
            "按主键定位必须可用（fail-closed 能力位）"
        );
    }

    #[test]
    fn id_is_sortable_so_the_ledger_can_close_its_ordering_with_it() {
        // 本表刻意偏离仓库惯例（别处的 `id` 只有 `filterable`）：台账要靠唯一列收尾
        // 才有确定性全序，而 `title` 会重名、`id` 是唯一的唯一列。
        // `validate_order_field` 对不可排序的字段先于角色权限直接拒绝，HTTP 边界上
        // 是 **403**——`list_datasources` 的兜底与前端 `withStableOrder` 都恒按 `id`
        // 升序，漏了这一位就是「列表页永远打不开」。
        // 端到端复现见 `actions/list_datasources.rs` 的
        // `the_order_clauses_the_console_always_sends_are_applicable`。
        let definition = definition();
        let id = definition
            .field("id")
            .unwrap_or_else(|| panic!("id 字段必须存在"));
        assert!(
            id.is_sortable(),
            "按主键收尾排序必须可用（fail-closed 能力位）"
        );
    }

    #[test]
    fn title_is_sortable_so_the_ledger_can_order_by_name() {
        // 台账视图在上百条量级下必须能按名称排序。DSL 的 sortable 是 fail-closed：
        // 未显式打开时 TableQuery 会直接拒绝排序请求，所以这是一条能力断言而非优化。
        let definition = definition();
        let title = definition
            .field("title")
            .unwrap_or_else(|| panic!("title 字段必须存在"));
        assert!(title.is_sortable(), "按名称排序必须可用");
        assert!(title.is_searchable(), "名称必须可被关键词检索");
    }

    #[test]
    fn status_is_filterable_so_the_ledger_can_filter_by_state() {
        // 台账工具栏的「全部 / 启用 / 已停用」依赖这一位；同样 fail-closed。
        let definition = definition();
        let status = definition
            .field("status")
            .unwrap_or_else(|| panic!("status 字段必须存在"));
        assert!(status.is_filterable(), "按状态筛选必须可用");
    }

    #[test]
    fn updated_at_is_sortable_for_recency_ordering() {
        let definition = definition();
        let updated_at = definition
            .field("updated_at")
            .unwrap_or_else(|| panic!("updated_at 字段必须存在"));
        assert!(updated_at.is_sortable(), "按更新时间排序必须可用");
    }

    #[test]
    fn ingest_mode_defaults_to_push_and_is_filterable() {
        // 默认 push 让存量数据源的语义不变；filterable 是轮询选源（where_eq
        // ingest_mode = "pull"）的硬依赖——DSL 的 filterable 是 fail-closed，
        // 漏了会在运行期被 FieldPermissionDenied 打回，而不是启动期报错。
        let definition = definition();
        let mode = definition
            .field("ingest_mode")
            .unwrap_or_else(|| panic!("ingest_mode 字段必须存在"));
        assert!(mode.is_required(), "取数方式必须有值");
        assert_eq!(mode.default_value(), Some(&serde_json::json!("push")));
        assert!(mode.is_filterable(), "轮询按取数方式选源，必须可筛");
    }

    #[test]
    fn the_table_row_carries_no_credential_or_routing_columns() {
        // 设计 §5：source_key 与凭据都属于**字段绑定**那一层。
        // 表级行上留着它们，会让「一张表 = 一条数据源」这个语义立刻自相矛盾
        // （一条表级行只能有一个 source_key）。
        let definition = definition();
        for gone in [
            "source_key",
            "token_hash",
            "encrypt_enabled",
            "default_locale",
            "bitable_field_name",
            "linkage_mapping",
        ] {
            assert!(
                definition.field(gone).is_none(),
                "{gone} 属于字段绑定层（datasource/domain/field_table.rs），不该在表级行上"
            );
        }
    }

    #[test]
    fn the_field_level_coordinates_are_gone() {
        // 表级模型下没有「取数列」这一列——取数是每条字段绑定各自的事。
        // 这条把「不要在表级行上加回字段级列」钉住。
        let definition = definition();
        for gone in ["bitable_field_name", "linkage_mapping"] {
            assert!(
                definition.field(gone).is_none(),
                "{gone} 属于字段绑定层（field_table.rs），不该出现在表级行上"
            );
        }
    }

    #[test]
    fn sync_state_columns_are_nullable_or_defaulted() {
        // schema_sync 门禁：已有数据的表上加「必填且无默认值」的列会让启动直接失败。
        let definition = definition();
        for name in [
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
            "last_pull_at",
            "last_success_at",
            "last_error",
            "snapshot_digest",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 必须可空，否则存量行无法加列");
        }
        let failures = definition
            .field("consecutive_failures")
            .unwrap_or_else(|| panic!("consecutive_failures 字段必须存在"));
        assert_eq!(failures.default_value(), Some(&serde_json::json!(0)));
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        // 防线：字段没设 .title() 时展示名会退化成字段名（英文），
        // 前端表格就会满屏 source_key / encrypt_enabled。
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
