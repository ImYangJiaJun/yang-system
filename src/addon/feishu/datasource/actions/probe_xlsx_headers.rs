//! 探测上传文件的表头——xlsx 导入配置向导的第一步。
//!
//! 只回信封（sheet 名、表头行号、全部列名 + 1-based 列号、文件名），**不写库**：
//! 向导拿它渲染「勾选要当外部选项来源的列」，用户点了确认才落绑定。
//!
//! 多文件必须表头一致（集合相同，顺序可不同），不一致就整份拒绝——
//! 取交集或只导能对上的那个，会让用户以为导了 10 万行、实际只有 5 万。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse, UploadedFile};
use yang_base::definition::{HttpMethod, ModuleSpec, MultipartSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::uploaded_files::one_or_many_files;
use crate::addon::feishu::domain::xlsx;

/// 纯函数输入：**文件名 + 已读进内存的字节**。
///
/// 刻意不用框架的 `UploadedFile`：临时文件是**请求作用域**的（handler 返回即删），
/// 而这一层要能在不装配 `ActionContext` 的单测里直接喂夹具字节。
#[derive(Debug)]
pub(super) struct ProbeInput {
    /// 上传的 xlsx 文件，按上传顺序。上限见 `register`：最多 32 个、合计 16 MiB。
    pub(super) files: Vec<(String, Vec<u8>)>,
}

impl ProbeInput {
    /// 形状校验：至少要有一个文件。
    ///
    /// 文件之间的一致性校验要读文件，所以在 [`probe`] 里而不是这里。
    fn validate(&self) -> Result<(), BaseError> {
        if self.files.is_empty() {
            // 「一个文件都没有」时「表头一致」是**空真**，必须显式拒绝：
            // 否则前端会拿到一个空列名列表，渲染出一个什么都选不了的向导。
            return Err(BaseError::ParamInvalid(
                "files".to_string(),
                "至少要上传一个文件".to_string(),
            ));
        }
        Ok(())
    }
}

/// 探测一组文件的表头，返回向导消费的信封。
///
/// 抽成纯函数是为了不依赖 `ActionContext` 就能测。
///
/// **代价不是「只读一行」**：`Xlsx::new` 会**急切读完整个 `xl/sharedStrings.xml`**
/// （真实 Excel 导出默认就走 sharedStrings 形态），所以这一层是 O(整份字符串表)，
/// 只是不读 sheetData 的数据行——比全量导入便宜，但不是 O(表头行)。
fn probe(input: &ProbeInput) -> Result<serde_json::Value, BaseError> {
    input.validate()?;

    let mut headers = Vec::with_capacity(input.files.len());
    for (name, bytes) in &input.files {
        let header = xlsx::read_header(bytes).map_err(|error| {
            BaseError::ParamInvalid("files".to_string(), format!("{name}：{error}"))
        })?;
        headers.push((name.clone(), header));
    }
    // 文件之间表头必须完全一致（集合相同，顺序可不同——按名取值，不按位置）。
    xlsx::require_consistent_headers(&headers)
        .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    let first = &headers[0].1;
    Ok(serde_json::json!({
        "sheet_name": first.sheet_name,
        // 全部 sheet 名带出来：多于一张时前端提示「只读了第一张」。
        "sheets": first.sheet_names,
        // Task 13 的向导靠它提示「表头在第 N 行」，也是导入时跳过表头行的依据。
        "header_row": first.header_row,
        // **全部列**，不做任何类型/语义过滤——哪些列适合当外部选项由人判断
        // （与 list_bitable_fields 的口径一致）。
        "columns": first.columns.iter()
            .map(|(name, index)| serde_json::json!({ "name": name, "index": index }))
            .collect::<Vec<_>>(),
        "files": input.files.iter()
            .map(|(name, _)| serde_json::json!({ "name": name }))
            .collect::<Vec<_>>(),
    }))
}

/// Action 输入：框架上传的临时文件句柄。
///
/// 与 [`ProbeInput`] 分开是因为 `UploadedFile` **不可在单测里构造**，
/// 且临时文件必须在这一层读掉——handler 一返回框架就删。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeUploadInput {
    /// 上传的 xlsx 文件，至少一个；同名 part 重复出现即为多文件。
    // **不要摘掉 `deserialize_with`**：传输层对单 part 放的是裸对象（不是数组），
    // 直接写 `Vec<UploadedFile>` 会让「只传一个文件」400。见 `one_or_many_files`。
    #[serde(deserialize_with = "one_or_many_files")]
    pub(super) files: Vec<UploadedFile>,
}

impl ParamInput for ProbeUploadInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 注册探表头端点。
///
/// **不做 `settings.can_pull()` 门控**：那个门控是给**出站**端点用的，
/// 本端点不出网，xlsx 导入在没有飞书凭证的环境里也必须可用。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("probe_xlsx_headers"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/xlsx/probe")
        .display_name("解析 xlsx 表头")
        .description("读上传文件的表头（只看第一张 sheet），供配置向导勾列；不写库")
        // 与其余配置类端点同权限。
        .permissions(["feishu.datasource.write"])
        .multipart(
            // **必须显式设上限**：MultipartSpec 默认 max_total_bytes = 32 MiB，
            // 超过 AxumTransportConfig.max_body_bytes 时**进程拒绝启动**（启动期
            // fail-closed）。16 MiB 与 Task 3 抬到的新默认值对齐。
            MultipartSpec::new([
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ])
            .max_fields(1)
            .max_files(32)
            .max_file_bytes(16 * 1024 * 1024)
            .max_total_bytes(16 * 1024 * 1024),
        )
        .register()
}

/// Handler 只做三件事：把临时文件读进内存 → 调 [`probe`] → 包 `ApiResponse`。
pub(super) async fn handle(
    _ctx: ActionContext,
    input: ProbeUploadInput,
    _context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    // 临时文件是请求作用域的，必须在这里读完（handler 返回后框架就删）。
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(input.files.len());
    for file in &input.files {
        let name = file.original_filename().to_string();
        let bytes = tokio::fs::read(file.path())
            .await
            .map_err(|error| BaseError::ConfigError(format!("读上传文件 {name} 失败：{error}")))?;
        files.push((name, bytes));
    }

    let payload = probe(&ProbeInput { files })?;
    ApiResponse::success(payload, "解析成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/xlsx")
                .join(name),
        )
        .unwrap_or_else(|error| panic!("读夹具 {name} 失败: {error}"))
    }

    /// 传输层会产出的**单个**文件 part 的形态：裸对象，不是数组。
    ///
    /// 取自 `yang-base` 的 `transport/axum.rs::insert_multipart_value`——它在
    /// `Entry::Vacant` 时直接放这个对象，只有第二个同名 part 才升级成数组。
    fn transport_file_part(name: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "field_name": "files",
            "original_filename": name,
            "content_type": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "size": 1962,
            "path": path,
            "temp_root": "C:/tmp/yang-scope-1",
        })
    }

    /// **必须走真正的反序列化路径**：这个 bug 之前漏掉，正是因为只测了 `probe()` 纯函数层，
    /// 它绕过了 `ParamInput::decode` 的 `serde_json::from_value`。
    #[test]
    fn one_uploaded_file_deserializes_from_the_bare_object_the_transport_sends() {
        let bare = serde_json::json!({
            "files": transport_file_part("bank_1.xlsx", "C:/tmp/yang-upload-1"),
        });
        let input: ProbeUploadInput = serde_json::from_value(bare)
            .unwrap_or_else(|error| panic!("只传一个文件必须能反序列化: {error}"));
        assert_eq!(input.files.len(), 1, "单 part 的裸对象应归一成 1 个文件");
        assert_eq!(input.files[0].original_filename(), "bank_1.xlsx");
    }

    /// 多文件时传输层放的是数组；这条同时钉住 helper 没有把数组形态弄坏。
    #[test]
    fn two_uploaded_files_deserialize_from_the_array_the_transport_sends() {
        let array = serde_json::json!({
            "files": [
                transport_file_part("bank_1.xlsx", "C:/tmp/yang-upload-1"),
                transport_file_part("bank_2.xlsx", "C:/tmp/yang-upload-2"),
            ],
        });
        let input: ProbeUploadInput = serde_json::from_value(array)
            .unwrap_or_else(|error| panic!("多文件必须能反序列化: {error}"));
        assert_eq!(input.files.len(), 2);
        assert_eq!(input.files[1].original_filename(), "bank_2.xlsx");
    }

    #[test]
    fn a_request_with_no_files_is_rejected() {
        // 一个文件都没有时「表头一致」是空真——必须显式拒绝，
        // 否则前端会拿到一个空列名列表并渲染出一个什么都选不了的向导。
        let input = ProbeInput { files: Vec::new() };
        assert!(input.validate().is_err());
    }

    #[test]
    fn the_response_carries_every_column_with_its_one_based_index() {
        let input = ProbeInput {
            files: vec![("bank_1.xlsx".to_string(), fixture("bank_1.xlsx"))],
        };
        let payload = probe(&input).unwrap_or_else(|error| panic!("应能探测: {error}"));
        assert_eq!(payload["sheet_name"], "境内银行网点信息管理");
        assert_eq!(payload["header_row"], 1);
        let columns = payload["columns"]
            .as_array()
            .unwrap_or_else(|| panic!("columns 应存在"));
        assert_eq!(
            columns.len(),
            8,
            "**全部列**都要带出来，不做任何类型/语义过滤"
        );
        assert_eq!(columns[1]["name"], "开户行行名");
        assert_eq!(columns[1]["index"], 2, "1-based 物理列号");
        // 向导要靠这两个键提示「读了哪张 sheet」「导了哪几个文件」。
        assert_eq!(
            payload["sheets"],
            serde_json::json!(["境内银行网点信息管理"])
        );
        assert_eq!(
            payload["files"],
            serde_json::json!([{ "name": "bank_1.xlsx" }])
        );
    }

    #[test]
    fn inconsistent_files_are_rejected_as_a_whole() {
        let input = ProbeInput {
            files: vec![
                ("bank_1.xlsx".to_string(), fixture("bank_1.xlsx")),
                (
                    "header_mismatch_two.xlsx".to_string(),
                    fixture("header_mismatch_two.xlsx"),
                ),
            ],
        };
        let error = input.validate().err();
        // 一致性校验要读文件，所以它在 probe() 里而不是 validate() 里；
        // 这条测试直接打 probe()，断言整份拒绝。
        assert!(error.is_none(), "形状校验本身应通过（文件非空）");
        let error = probe(&input)
            .err()
            .unwrap_or_else(|| panic!("表头不一致必须整份拒绝"));
        assert!(error.to_string().contains("header_mismatch_two.xlsx"));
    }
}
