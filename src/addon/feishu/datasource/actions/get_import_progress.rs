//! 查一条数据源的 xlsx 导入进度。
//!
//! **不查库**：答的是「本进程上这条源有没有导入在跑、跑到哪了」，数据不存在 / 已删
//! 的源恒答 `idle`。这条端点的用途只有一个——让上传了 15 万行文件的用户看见
//! 「还在解析」，而不是一个转圈的按钮。
//!
//! # 为什么恒 200（没有 404）
//!
//! 「没在跑」是**正常态**，不是错误：查完、没导过、导入跑完了，都是它。用 404 表达
//! 这一态会让前端把正常态读成故障（而蓝绿双实例下轮询还可能落到另一色，见
//! `domain/import_progress.rs` 的第 2 条硬规矩）。所以除了入参本身不合法，
//! 一律成功信封。

use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::import_progress::{self, ImportProgress};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) GetImportProgressInput {
        #[param(source = path)]
        datasource_id: Key::new().title("数据源").require(true),
    }
}

/// 注册本端点。
///
/// 权限与控制台其余数据源端点同一粒（`feishu.datasource.read`）——只读进程内状态，
/// 不写库、不碰飞书，不值得为它新开一个授权位。也因此**不出网**，不受配网凭证门控。
pub(super) fn register(module: ModuleSpec, _context: Arc<FeishuContext>) -> ModuleSpec {
    module
        // 不持有 `FeishuContext`：本端点不碰库也不碰飞书，拿它反而误导读者以为有依赖。
        .action_fn(yang_base::action_name!("get_import_progress"), handle)
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/datasources/{datasource_id}/import-progress",
        )
        .display_name("xlsx 导入进度")
        .description("查询该数据源在本进程内的导入阶段与计数；不写库")
        .permissions(["feishu.datasource.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: GetImportProgressInput,
) -> Result<ApiResponse, BaseError> {
    if input.datasource_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "必须是正整数".to_string(),
        ));
    }
    // 查不到就答 `idle`（`ImportProgress::idle()` 的计数全是 `None`，即「这一次没有
    // 任何数字可报」），**绝不**答 404，也绝不凭空造条目。
    let progress =
        import_progress::snapshot(input.datasource_id).unwrap_or_else(ImportProgress::idle);
    ApiResponse::success(progress, "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::import_progress::ImportGuard;
    use crate::addon::feishu::domain::projection_contract;
    use yang_base::definition::{ParamInput, ParamSource};

    #[test]
    fn the_committed_contract_matches_the_progress_struct() {
        // 键集是前后端各写一份、逐字对账的契约。给任一字段加 `skip_serializing_if`、
        // 或加了字段不改契约，这里当场红。
        projection_contract::assert_keys(
            &ImportProgress::idle(),
            &["get_import_progress", "result", "emitted"],
            "导入进度",
        );
    }

    #[test]
    fn idle_and_running_share_one_key_set() {
        // 上面那条只钉住了 `idle`（所有 `Option` 都是 `None`）。「跑起来之后键集也不能变」
        // 得单独钉：`Option` 为 `Some` 时若序列化成了另一个键集，前端按固定键读就会
        // 部分读空，而契约测试看不见。
        // **id 与其它用例错开**：登记表是进程级静态量，`#[test]` 又是并行跑的。
        let id = 301;
        let _guard = ImportGuard::acquire_for_test(id).unwrap_or_else(|_| panic!("第一次应拿到"));
        import_progress::report_parsing(id, 1, 1, 7, Some(9));

        let running = import_progress::snapshot(id).unwrap_or_else(|| panic!("在跑就必须查得到"));
        projection_contract::assert_keys(
            &running,
            &["get_import_progress", "result", "emitted"],
            "运行中的导入进度",
        );
        let value = serde_json::to_value(&running).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["stage"], serde_json::json!("parsing"));
    }

    /// 懒连接池的 ctx：`handle` 不碰库，但签名要它。
    ///
    /// 照 `infrastructure/authorization/request_validator.rs` 的 `test_ctx`（`connect_lazy`
    /// 不会真连）。
    fn test_ctx() -> ActionContext {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = yang_db::Database::from_pool(pool, yang_db::DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            yang_base::tools::ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        ActionContext::new(
            yang_base::action::Request::new(serde_json::json!({})),
            tools,
        )
    }

    #[tokio::test]
    async fn a_non_positive_datasource_id_is_a_parameter_error() {
        // 入参不合法是**唯一**的非 200 面：其余一切（没在跑、没这条源）都是成功信封。
        for id in [0, -1] {
            let error = handle(test_ctx(), GetImportProgressInput { datasource_id: id })
                .await
                .err()
                .unwrap_or_else(|| panic!("{id} 必须被拒"));
            assert!(error.to_string().contains("必须是正整数"), "实际: {error}");
        }
    }

    #[tokio::test]
    async fn an_id_without_a_run_answers_idle_instead_of_404() {
        // 「没在跑」是正常态：答 404 会让前端把正常态读成故障（蓝绿双实例下轮询还会
        // 落到另一色）。这条同时钉住「查进度不写库」——`handle` 走的 ctx 是懒池，
        // 一旦它真的碰库，这里会连不上而失败。
        let response = handle(
            test_ctx(),
            GetImportProgressInput {
                datasource_id: 9_999,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("查进度不该失败: {error}"));
        let value = serde_json::to_value(&response).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["code"], serde_json::json!(0));
        assert_eq!(value["message"], serde_json::json!("查询成功"));
        assert_eq!(value["data"]["stage"], serde_json::json!("idle"));
        assert_eq!(value["data"]["rows_done"], serde_json::Value::Null);
    }

    #[test]
    fn datasource_id_comes_from_the_path_not_the_body() {
        // 前端按 `source: path` 声明做 URL 替换（`/datasources/{datasource_id}/import-progress`），
        // 声明一旦漂成 query/body，前端拼出来的 URL 就带不上 id。
        let params = <GetImportProgressInput as ParamInput>::params();
        let datasource_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "datasource_id")
            .unwrap_or_else(|| panic!("应声明 datasource_id 参数"));
        assert_eq!(datasource_id.source, ParamSource::Path);
        assert!(datasource_id.required);
    }
}
