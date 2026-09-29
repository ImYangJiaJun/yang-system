//! 文件导入：把上传的一批 xlsx 变成某个表级数据源的外部选项。
//!
//! # 它是拉取路径的兄弟，不是它的复制品
//!
//! 编排骨架与 `pull.rs::pull_table_inner` 逐段对应（校验 → 取数 → 空快照守卫 →
//! 逐绑定派生 → 逐绑定事务落库），**只有取数那一步换成了「读上传的文件」**：
//! 拉取出网问多维表格，导入读本地临时文件。因此这里刻意复用 `pull.rs` 的
//! [`TableBinding`] / [`BoundField`] / `parent_linkage`——祖先链的上溯带着防环守卫，
//! 自己再写一份等于把那条守卫复制两遍，而手工改库造出的 a→b→a 会在一处被挡、
//! 在另一处把请求卡死。
//!
//! # 与拉取路径的两处**刻意不同**
//!
//! 1. **事务用 `ctx.begin_transaction()`**。`pull.rs` 走 `database.transaction()`
//!    是因为它是后台 worker、手里根本没有 `ActionContext`（见 `domain/option_write.rs`
//!    的模块文档）。导入是 Action，有 ctx 就该用 ctx。
//! 2. **多一道逐绑定空快照守卫**（§5.7.1）。拉取侧只有**表级**守卫：快照 0 行且本地有
//!    启用选项就整轮失败。导入侧还有第二种情形——整表有行，但**这一条绑定**派生 0 个
//!    选项（列名对着，列整列为空）。此时若照常跑补集停用，那条绑定的选项会被**全量
//!    清空**；而表级守卫看不见它（表级快照非空）。所以这一条绑定跳过写库、只告警。
//!
//! # 缺列即拒、多列忽略（D9）
//!
//! 表头里没有必需列 → 整份拒绝并点名缺哪列（`xlsx::read_snapshot_with_progress` 负责）；
//! 多出来的列直接忽略——用户在同一份文件里加一列备注是无害的。
//!
//! # 异常行照常入库，但不喂给飞书（§5.7 的 D6）
//!
//! 取数文案超过 `feishu_option.label` 的 255 上限时**截断**并置 `enabled = false`，
//! 原因写进 `extra`。不截断的话会在**插库时**才炸（列是 `max_length(255)`），
//! 而那时的错误不会告诉你是哪一行、什么值。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse, Request, UploadedFile};
use yang_base::definition::{HttpMethod, ModuleSpec, MultipartSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::derive::{
    derive_options, snapshot_digest, DerivedOption, RawValue,
};
use crate::addon::feishu::domain::import_progress::{self, ImportBusy, ImportGuard};
use crate::addon::feishu::domain::linkage::Linkage;
use crate::addon::feishu::domain::option_write::{
    apply_option_rows, count_option_rows, disable_option_rows, find_foreign_option_owner,
};
use crate::addon::feishu::domain::pull::{parent_linkage, BoundField, TableBinding};
use crate::addon::feishu::domain::uploaded_files::one_or_many_files;
use crate::addon::feishu::domain::xlsx;
use crate::infrastructure::audit;

/// 失败码。与 `pull_now` / `health_check` 共用数值域（404/409 段 = 数据源类）。
mod codes {
    /// 数据源不存在。
    pub(super) const SOURCE_NOT_FOUND: i32 = 40401;
    /// 这条数据源的取数方式不是文件导入。
    pub(super) const NOT_IMPORTABLE: i32 = 40906;
    /// 这条数据源已停用。
    pub(super) const DISABLED: i32 = 40907;
}

/// 导入输入。
///
/// `files` 与 `datasource_id` 来自**两个不同的来源**（multipart body 与 URL 路径），
/// 所以 [`ImportInput::decode`] 是手写的——默认实现只做一次单 body 反序列化。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportInput {
    /// URL 路径段里的表级数据源 `id`。
    pub(super) datasource_id: i64,
    /// 上传的 xlsx 文件，按上传顺序拼接为一份快照。上限见 `register`。
    // **不要摘掉 `deserialize_with`**：传输层对单 part 放的是裸对象（不是数组），
    // 直接写 `Vec<UploadedFile>` 会让「只传一个文件」400。见 `one_or_many_files`。
    #[serde(deserialize_with = "one_or_many_files")]
    pub(super) files: Vec<UploadedFile>,
}

impl ParamInput for ImportInput {
    fn params() -> Params {
        Params::new()
    }

    /// 路径参数 + multipart body 各读一次。
    ///
    /// **为什么不合并成一个对象再反序列化一次**（`params!` 的做法）：multipart 的文件
    /// 句柄是**传输层**注进 body 的服务端构造（带受信临时根，用于 `copy_to` 的越界
    /// 校验），没有一条「把它当成普通路径/查询参数」的读取路径。手写 decode 是这条
    /// 约束下唯一诚实的写法：两个来源各自读自己那一份，合起来构造输入。
    fn decode(request: &mut Request) -> Result<Self, BaseError> {
        let datasource_id = request
            .get_path_param("datasource_id")
            .ok_or_else(|| {
                BaseError::ParamInvalid("datasource_id".to_string(), "缺少路径参数".to_string())
            })?
            .parse::<i64>()
            .map_err(|_| {
                BaseError::ParamInvalid("datasource_id".to_string(), "必须是整数".to_string())
            })?;

        /// 只取 `files` 一项；body 里其余键（包括 schema 上的 `datasource_id`）忽略。
        #[derive(Deserialize)]
        struct FilesBody {
            #[serde(deserialize_with = "one_or_many_files")]
            files: Vec<UploadedFile>,
        }

        let body = std::mem::take(&mut request.body);
        let files = serde_json::from_value::<FilesBody>(body)
            .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?
            .files;
        Ok(Self {
            datasource_id,
            files,
        })
    }
}

impl ImportInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.files.is_empty() {
            // 「一份空快照」与「一个文件都没传」是两件事：前者走空快照守卫并由回执
            // 说明，后者是入参错误。不在这里挡住的话会得到一个「导了 0 行」的成功回执。
            return Err(BaseError::ParamInvalid(
                "files".to_string(),
                "至少要上传一个文件".to_string(),
            ));
        }
        Ok(())
    }
}

/// 一条表级数据源的校验视图。
struct DatasourceRow {
    ingest_mode: String,
    status: String,
}

/// 注册文件导入端点。
///
/// 与探表头一样**不出网**，所以不受 `can_pull()` 门控：没有飞书凭证的环境里
/// 文件导入照样要能用。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("import_xlsx"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/datasources/{datasource_id}/import",
        )
        .display_name("导入 xlsx")
        .description("按每条绑定的列名取值、派生选项并落库；整份快照替换该数据源的选项")
        // 与其余配置类端点同权限。这里会**停用补集**，所以是写权限而不是读权限。
        .permissions(["feishu.datasource.write"])
        .multipart(
            // **必须显式设上限**：MultipartSpec 默认 max_total_bytes = 32 MiB，
            // 超过 AxumTransportConfig.max_body_bytes 时**进程拒绝启动**（启动期
            // fail-closed）。16 MiB 与探表头端点、Task 3 抬到的新默认值对齐。
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

pub(super) async fn handle(
    ctx: ActionContext,
    input: ImportInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let started = std::time::Instant::now();

    // 数据源与启用绑定：一次取回。
    let datasource = load_datasource(&context, input.datasource_id).await?;
    let bindings = load_enabled_bindings(&context, input.datasource_id).await?;

    // 步骤 1：校验。三个条件各自以**失败信封**返回（不是 BaseError）——
    // 与 pull_now 的 codes 段同风格，也让前端能区分「参数错」与「这条源现在导不了」。
    for (ok, code, message) in [
        (
            datasource.is_some(),
            codes::SOURCE_NOT_FOUND,
            "数据源不存在",
        ),
        (
            datasource
                .as_ref()
                .is_some_and(|row| row.ingest_mode == "xlsx_import"),
            codes::NOT_IMPORTABLE,
            "这条数据源的取数方式不是文件导入",
        ),
        (
            datasource
                .as_ref()
                .is_some_and(|row| row.status == "active"),
            codes::DISABLED,
            "这条数据源已停用",
        ),
    ] {
        if !ok {
            return Ok(ApiResponse::fail(code, message.to_string()));
        }
    }

    // 拿互斥锁：**在所有校验之后、读文件之前**。
    //
    // 位置是刻意的两端：放在校验之前，一个参数写错的请求（数据源不存在、不是导入源、
    // 已停用）也会占住锁，把人触发的重试从「立刻被拒」变成「看运气」；放在读文件之后，
    // 则已经白白解析了一遍——而两次请求争的正是解析完以后的落库那一段。
    //
    // 为什么必须挡：`pull.rs` 是单 worker 串行跑的，天然不会自我并发；导入是人触发的，
    // 必然会（手抖点两下、两个运维同时操作）。逐绑定事务交错会让两次补集停用互相覆盖，
    // 而补集停用是**全量视图**语义——交错的后果是选项被错误地停用。
    let _guard = ImportGuard::acquire(input.datasource_id).map_err(|ImportBusy| {
        BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "这条数据源正在导入中，请等它跑完再试".to_string(),
        )
    })?;
    // 条目（互斥 + 进度）从这一刻起存在，到 `_guard` 被 drop 为止。
    //
    // **上传阶段的字节进度报不出来**：multipart body 在 transport 层读完才 dispatch，
    // 服务端拿到请求时文件已经在临时目录里了。所以这里先报 `loading`——它离
    // 「正在读文件」最近，前端在没拿到任何计数时就显示「正在上传」。
    let files_total = input.files.len() as u32;
    import_progress::report_loading(input.datasource_id, files_total, 0);

    // 步骤 2：读文件（**替换 pull 的 list_all_records**）。
    // 必需列 = 全部启用绑定的 field_id（xlsx 侧列名 = field_id，Task 8 保证非空）。
    let columns: Vec<String> = bindings.iter().map(|b| b.field_id.clone()).collect();
    let mut loaded = Vec::with_capacity(input.files.len());
    for file in &input.files {
        let name = file.original_filename().to_string();
        let bytes = tokio::fs::read(file.path())
            .await
            .map_err(|error| BaseError::ConfigError(format!("读上传文件 {name} 失败：{error}")))?;
        loaded.push((name, bytes));
    }
    // **文件之间的表头必须一致**（集合相同，顺序可不同——按名取值，不按位置）。
    //
    // 这条**必须由本端点自己守**：`read_snapshot` 只逐文件比对**必需列**，从不做文件之间的
    // 比对——它把这条前置条件留给了调用方。探表头那条路已经守了，但**「重新导入」不经过
    // 探表头**（详情页直接调本端点），漏掉它就意味着：用户传了一组表头互不相同的文件，
    // 会被**静默合并**成一份，而回执里的 `rows_read` 之和看起来完全正常。
    //
    // 代价是每份文件多解析一次表头（`read_header` 只驱动到表头行结束，不扫 sheetData；
    // 贵的那一段是 `Xlsx::new` 读字符串表）。换的是「一条只在探表头存在的规则不会在
    // 重新导入时消失」。
    let mut headers = Vec::with_capacity(loaded.len());
    for (index, (name, bytes)) in loaded.iter().enumerate() {
        let header = xlsx::read_header(bytes).map_err(|error| {
            BaseError::ParamInvalid("files".to_string(), format!("{name}：{error}"))
        })?;
        headers.push((name.clone(), header));
        // `files_done` = **读完并校验过表头**的文件数：这一步就是那道校验，
        // 所以它 +1 的时机在这里，而不是在「把字节读进内存」那一步。
        import_progress::report_loading(input.datasource_id, files_total, (index + 1) as u32);
    }
    xlsx::require_consistent_headers(&headers)
        .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    // 解析挪进 `spawn_blocking`：它是同步 CPU 活，15 万行要跑十几秒，占住一个 tokio
    // worker 就意味着单 vCPU 部署上「进度 GET」与它抢同一个线程——而那正是进度要解决
    // 的场景（页面卡住、什么都看不到）。
    let snapshot = tokio::task::spawn_blocking(move || {
        xlsx::read_snapshot_with_progress(&loaded, &columns, &mut |event| {
            import_progress::report_parsing(
                input.datasource_id,
                event.file_index,
                event.files_total,
                event.rows_done,
                event.rows_total,
            );
        })
    })
    .await
    .map_err(|error| BaseError::ConfigError(format!("解析任务失败：{error}")))?
    .map_err(|error| BaseError::ParamInvalid("files".to_string(), error.to_string()))?;

    // 表级空快照守卫——**与 pull.rs 同款**：解析出 0 行而库里仍有启用选项时，
    // 不停用、且整轮失败。绝不能照常跑补集停用。
    if snapshot.rows.is_empty() {
        let mut suspicious = Vec::new();
        for binding in &bindings {
            if count_active(&context, &binding.source_key).await? > 0 {
                suspicious.push(binding.source_key.clone());
            }
        }
        if !suspicious.is_empty() {
            tracing::warn!(
                datasource_id = input.datasource_id,
                suspicious = %suspicious.join("、"),
                "上传的文件解析出 0 行但本地仍有已启用选项：拒绝停用补集"
            );
            return Err(BaseError::ConfigError(format!(
                "上传的文件解析出 0 行数据，但本地仍有已启用选项（{}）：拒绝停用补集",
                suspicious.join("、")
            )));
        }
    }

    // 步骤 3-5：逐绑定派生与落库。祖先链的构造载体只算一次，与 pull 同形。
    let bound = to_bound_fields(&bindings);
    let mut reports = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        // 报的是**已开始处理**的绑定数：开始第 i 条时已完成 i 条（0-based），所以这个
        // 数到不了 `bindings_total`——差的那一条正是正在写的这条。
        import_progress::report_writing(input.datasource_id, bindings.len() as u32, index as u32);
        let report = import_binding(
            &ctx,
            &context,
            input.datasource_id,
            binding,
            &bindings,
            &bound,
            &snapshot,
        )
        .await?;
        reports.push(report);
    }

    ApiResponse::success(
        serde_json::json!({
            "datasource_id": input.datasource_id,
            "elapsed_ms": started.elapsed().as_millis() as i64,
            "files": snapshot.per_file.iter()
                .map(|file| serde_json::json!({
                    "name": file.name,
                    // **不是「文件总行数」**：统计口径是「在**被请求的列**里至少有一个
                    // 非空」的行——没勾选的列在解析时就被 continue 掉了，整行全空的行
                    // 也被丢掉。它正是逐绑定空快照守卫的判据载体。
                    "rows_read": file.rows_read,
                }))
                .collect::<Vec<_>>(),
            "bindings": reports,
        }),
        "导入完成",
    )
}

/// 从绑定行构造拉取侧的同名类型。
///
/// 直接复用 [`TableBinding`] / [`BoundField`] 而不是各造一套：`parent_linkage` 的签名
/// 认它们，复用了它们也就复用了那条防环守卫。xlsx 没有「解析出的当前名字」这一层
/// （列名就是身份，没有上游可改名），所以 `field_name` 恒等于 `field_id`。
fn to_bound_fields(bindings: &[TableBinding]) -> Vec<BoundField> {
    bindings
        .iter()
        .map(|binding| BoundField {
            field_id: binding.field_id.clone(),
            field_name: binding.field_id.clone(),
            parent_field_id: binding.parent_field_id.clone(),
        })
        .collect()
}

/// 一条绑定的一轮导入：派生 → 比对 → 落库（**自己一个事务**，A12）。
#[allow(clippy::too_many_arguments)]
async fn import_binding(
    ctx: &ActionContext,
    context: &FeishuContext,
    datasource_id: i64,
    binding: &TableBinding,
    all_bindings: &[TableBinding],
    bound: &[BoundField],
    snapshot: &xlsx::ImportSnapshot,
) -> Result<serde_json::Value, BaseError> {
    // 祖先链：**直接复用 pull 的同一条上溯**（它带 visited 防环）。
    let field = bound
        .iter()
        .find(|field| field.field_id == binding.field_id)
        .ok_or_else(|| BaseError::ConfigError("绑定集合里找不到自己".to_string()))?;
    let linkage = parent_linkage(field, all_bindings, bound)?;

    // 投影：本绑定的取数列 + 各级祖先列。祖先列名 = 沿链各级绑定的 `field_id`。
    let ancestor_columns: Vec<String> = linkage
        .as_ref()
        .map(|linkage| {
            linkage
                .ancestors
                .iter()
                .map(|level| level.field_name.clone())
                .collect()
        })
        .unwrap_or_default();

    // 三件套：截断后的文案、各级祖先文案、异常原因（None = 正常）。
    let mut owned: Vec<(String, Vec<String>, Option<String>)> =
        Vec::with_capacity(snapshot.rows.len());
    for row in &snapshot.rows {
        let raw_label = row.get(&binding.field_id).cloned().unwrap_or_default();
        // **设计 §5.7**：取数文案超 `label` 的 255 上限时截断。不截断的话会在**插库时**
        // 才炸（列是 max_length(255)），而那时的错误不会告诉你是哪一行、什么值。
        let (label, anomaly) = truncate_label(&raw_label, LABEL_LIMIT);
        let ancestors: Vec<String> = ancestor_columns
            .iter()
            .map(|column| row.get(column).cloned().unwrap_or_default())
            .collect();
        owned.push((label, ancestors, anomaly));
    }
    // 派生要 `&[RawValue]`，而 `RawValue.ancestors` 是借用切片——先把每行的祖先引用
    // 算好（它活到本语句块末），再借出去。
    let ancestor_refs: Vec<Vec<&str>> = owned
        .iter()
        .map(|(_, ancestors, _)| ancestors.iter().map(String::as_str).collect())
        .collect();
    let raw: Vec<RawValue<'_>> = owned
        .iter()
        .zip(ancestor_refs.iter())
        .map(|((label, _, _), refs)| RawValue {
            ancestors: refs,
            label: label.as_str(),
        })
        .collect();

    // 异常按**截断且 trim 后的 final label** 索引，写库时按同一个键回查。
    // 用 label 而不是 option_id 做键：截断已经发生，`derive_options` 也是按这个 label
    // 算 id 的，所以「文案超长」这个属性天然属于文案本身。
    //
    // **必须与 `derive_options` 的 trim 口径一致**：它按 trim 后的文案写行，所以键也得
    // 是 trim 后的。少了这一步，「超长 + 首尾空白」的值回查不到自己的原因——异常行会
    // 静默保持 `enabled = true` 并被喂给飞书，而回执里那条异常记录还在。
    let anomaly_by_label: std::collections::HashMap<&str, &str> = owned
        .iter()
        .filter_map(|(label, _, reason)| reason.as_deref().map(|reason| (label.trim(), reason)))
        .collect();

    let derived = derive_options(
        &binding.source_key,
        &linkage
            .as_ref()
            .map(Linkage::source_keys)
            .unwrap_or_default(),
        &raw,
    );
    let digest = snapshot_digest(&derived);

    let (existing_rows, active_rows) = count_existing(context, &binding.source_key).await?;
    let disabled_rows = existing_rows.saturating_sub(active_rows);
    // 内容未变**且**本地没有已停用行 → 没有选项要写。
    //
    // 「没有已停用行」这个附加条件不可省：摘要是按**本轮应当是什么**算的、不含
    // `enabled`；只比摘要会让「被补集停用后内容恰好没变」的行永远复活不了。异常行
    // 恒被置为停用，于是有异常的那一轮之后 `disabled_rows` 恒大于 0——这正是让每轮
    // 都真的重写一遍、异常行有机会恢复的机制。
    let unchanged =
        binding.snapshot_digest.as_deref() == Some(digest.as_str()) && disabled_rows == 0;

    // **§5.7.1 逐绑定空快照守卫（pull.rs 没有这条）**：整表有行、但这一条绑定派生
    // 0 个选项，而它库里仍有启用行——跳过写库，**尤其不执行补集停用**。
    // 表级守卫看不见这种情形：`snapshot.rows` 非空，那张网漏了过去。
    if derived.is_empty() && active_rows > 0 {
        tracing::warn!(
            source_key = %binding.source_key,
            "本轮派生出 0 个选项但本地仍有已启用选项：跳过这条绑定，不动它的补集"
        );
        return Ok(binding_report(BindingReport {
            source_key: &binding.source_key,
            fetched: snapshot.rows.len(),
            derived: 0,
            disabled: 0,
            unchanged: false,
            // **发绑定行上已经存着的那个摘要，不是本轮算出来的空摘要**：
            // 跳过 = 这一轮什么都没写，所以「这条绑定现在的内容是什么」仍然是旧值。
            // 发空摘要会被读成「它的内容现在是空的」——与事实正相反，而回执正是
            // 运维判断「这轮到底发生了什么」的唯一依据。
            snapshot_digest: binding.snapshot_digest.as_deref(),
            skipped_reason: Some("本轮派生出 0 个选项，而本地仍有已启用选项——拒绝清空"),
            // 整条绑定都跳过了，补集自然也没跑——但那件事已经由 `skipped_reason`
            // 说清楚了（比「没做补集停用」更重），再报一次只是噪音。
            complement_skipped: None,
            anomalies: anomaly_report(&anomaly_by_label),
            truncated_details: anomaly_by_label.len() > ANOMALY_LIMIT,
        }));
    }

    // 跨源夺取预检与补集扫描都在进事务前做完：前者是可归因的业务失败，后者只读。
    let mut doomed = Vec::new();
    let mut complement_skipped = None;
    if !unchanged {
        let ids: Vec<String> = derived
            .iter()
            .map(|option| option.option_id.clone())
            .collect();
        if let Some((option_id, owner)) =
            find_foreign_option_owner(context.options(), &binding.source_key, &ids).await?
        {
            return Err(BaseError::ConfigError(format!(
                "选项 id {option_id} 已属于数据源 {owner}，拒绝改写其归属"
            )));
        }
        let scan = find_doomed_for(context, &binding.source_key, &derived).await?;
        doomed = scan.doomed;
        complement_skipped = scan.skipped_at;
    }

    // 事务：**用 ctx.begin_transaction()**，不要照抄 pull.rs 的 `database.transaction()`
    // ——pull.rs 没有 `ActionContext`（后台 worker 没有 ctx），导入是 Action。
    let mut transaction = ctx.begin_transaction().await?;
    let result = async {
        let mut disabled = 0u64;
        if !unchanged {
            // **就地构造**（不抽成返回 Record 的 helper）：`schema_anchor` 靠「函数体内
            // 唯一表锚点」定归属，抽出去这 8 个列名就掉进宽松档、只查「三张表之一有没有」。
            let rows: Vec<Record> = derived
                .iter()
                .map(|option| {
                    let mut record = Record::new();
                    record.insert("option_id", serde_json::json!(option.option_id));
                    record.insert("source_key", serde_json::json!(&binding.source_key));
                    record.insert("label", serde_json::json!(option.label));
                    record.insert("sort_order", serde_json::json!(option.sort_order));
                    // 异常行照常入库但**不喂给飞书**（设计 §5.7 的 D6）：出站查询本来
                    // 就 `where_eq("enabled", true)`，置 false 就等于把它藏起来。
                    // `option.label` 已由 `derive_options` trim 过；这里再 trim 一次是让
                    // 「键的口径」在读取处也看得见（写侧同样是 trim 后的）。
                    let anomaly = anomaly_by_label.get(option.label.trim());
                    record.insert("enabled", serde_json::json!(anomaly.is_none()));
                    record.insert("parent_key", serde_json::json!(option.parent_key));
                    record.insert("last_push_at", serde_json::json!(now_seconds()));
                    record.insert(
                        "extra",
                        match anomaly {
                            // `extra` 是 Text 存 JSON 的既有列。
                            Some(reason) => {
                                serde_json::json!(
                                    serde_json::json!({ "anomaly": reason }).to_string()
                                )
                            }
                            // 非异常行**显式写 null**：批量 upsert 是一条语句，所有行必须
                            // 同构列集（异构会被 yang-db fail-closed 拒掉）。安全性来自
                            // 「同一 option_id 的 extra 恒定」——id 由 source_key + 截断后的
                            // label 派生，「是否被截断」只由 label 决定，所以同一个 id 不可能
                            // 这轮异常下轮正常。将来若出现第三条写 extra 的路径，这条前提就破了。
                            None => serde_json::Value::Null,
                        },
                    );
                    record
                })
                .collect();
            apply_option_rows(
                context.options(),
                &mut transaction,
                &binding.source_key,
                rows,
            )
            .await?;
            disabled = disable_option_rows(
                context.options(),
                &mut transaction,
                &binding.source_key,
                &doomed,
            )
            .await?;

            let event = audit::succeeded_event(
                ctx,
                None,
                None,
                audit::entity("feishu_datasource", datasource_id)?,
                None,
                Some(audit::summary([
                    ("outcome_code", serde_json::json!("xlsx_imported")),
                    ("source_key", serde_json::json!(&binding.source_key)),
                    ("option_count", serde_json::json!(derived.len() as i64)),
                    ("disabled_count", serde_json::json!(disabled as i64)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }

        persist_binding_status(
            context,
            &mut transaction,
            binding.id,
            &digest,
            now_seconds(),
        )
        .await?;
        Ok::<u64, BaseError>(disabled)
    }
    .await;
    let disabled = FeishuContext::finish_transaction(transaction, result).await?;

    Ok(binding_report(BindingReport {
        source_key: &binding.source_key,
        fetched: snapshot.rows.len(),
        derived: derived.len(),
        disabled,
        unchanged,
        snapshot_digest: Some(&digest),
        skipped_reason: None,
        // **非 null = 这一轮压根没扫补集**（已启用选项数达单轮上限，见 `find_doomed_for`）。
        // 不带它的话，`disabled: 0` 在两种含义之间没有区别：「扫完了没有要停用的」与
        // 「没扫」——而银行网点那份文件是后者，每一轮都是。
        //
        // `unchanged` 那一轮不走这里：它压根没写库，回执上的「内容没变，本轮没写库」
        // 已经把「什么都没做」说全了。
        complement_skipped,
        anomalies: anomaly_report(&anomaly_by_label),
        truncated_details: anomaly_by_label.len() > ANOMALY_LIMIT,
    }))
}

/// 一条绑定的回执。
///
/// **抽成一个纯函数是为了可单测**：`import_binding` 的其余部分都要真库
/// （`FeishuContext`）才能跑，而这张回执是用户唯一看得见「这一轮到底做了什么」的地方
/// ——不抽出来，「补集被跳过时必须说出来」这条就只能靠集成测试钉。
struct BindingReport<'a> {
    source_key: &'a str,
    fetched: usize,
    derived: usize,
    disabled: u64,
    unchanged: bool,
    snapshot_digest: Option<&'a str>,
    skipped_reason: Option<&'a str>,
    /// 非 `None` = 本轮跳过了补集停用，值是当时已启用的选项数。
    complement_skipped: Option<usize>,
    anomalies: Vec<serde_json::Value>,
    truncated_details: bool,
}

/// 把上面的结构渲染成回执 JSON。
///
/// `skipped_reason` 与 `complement_skipped` **都在这里，缺省是 `null`**：两条分支
/// （跳过整条绑定 / 照常落库）的键集必须一致，不然消费方得按分支猜哪个键在。
fn binding_report(report: BindingReport<'_>) -> serde_json::Value {
    serde_json::json!({
        "source_key": report.source_key,
        "fetched": report.fetched,
        "derived": report.derived,
        "disabled": report.disabled,
        "snapshot_digest": report.snapshot_digest,
        "unchanged": report.unchanged,
        "skipped_reason": report.skipped_reason,
        "complement_skipped": report.complement_skipped,
        // 异常行逐条可归因。**不静默截断**：省略时给 `truncated_details` 与真实总数。
        "anomalies": report.anomalies,
        "truncated_details": report.truncated_details,
    })
}

/// 绑定行的状态回写：本轮摘要 + 最近导入时间。
///
/// 与选项行**同事务**提交：半提交会让「摘要已推进但行没写完」永久错位，而下一轮按
/// 摘要跳过写库，那条绑定的选项就永远停在半份状态。
///
/// 单独一个函数（而不是 `import_binding` 里的一行）是为了让 `schema_anchor` 能把
/// 这条写语句归到绑定表上：归属判据是「函数体内唯一的表锚点」，而 `import_binding`
/// 同时碰选项表。
async fn persist_binding_status(
    context: &FeishuContext,
    transaction: &mut Transaction,
    binding_id: i64,
    digest: &str,
    now: i64,
) -> Result<(), BaseError> {
    let mut binding_update = Record::new();
    binding_update.insert("snapshot_digest", serde_json::json!(digest));
    binding_update.insert("last_push_at", serde_json::json!(now));
    context
        .datasource_fields()
        .query()
        .where_eq("id", serde_json::json!(binding_id))?
        .update_in_tx(transaction, binding_update)
        .await?;
    Ok(())
}

/// `feishu_option.label` 的列上限。
const LABEL_LIMIT: usize = 255;

/// 截断到上限，并按需给出异常原因。返回 `(文案, 原因)`。
///
/// **`option_id` 按截断后的值算**（调用方在截断之后才派生），这是设计 §5.7 明写的
/// 取舍：否则 `id` 与 `label` 对不上，而「改文案 = 新 id」是补集停用语义的地基。
/// 代价认了：两个前 255 字相同的长值会碰撞成同一个 `option_id`，后者被
/// `derive_options` 去重掉——宁可少一个选项并留下 `extra` 里的异常记录，
/// 也不要一个 id 与内容不符的行。
fn truncate_label(value: &str, limit: usize) -> (String, Option<String>) {
    if value.chars().count() <= limit {
        return (value.to_string(), None);
    }
    let truncated: String = value.chars().take(limit).collect();
    (
        truncated,
        Some(format!(
            "取数文案超过 {limit} 字符（原长 {}），已截断",
            value.chars().count()
        )),
    )
}

/// 回执里的异常清单上限。超出时**不静默截断**——带 `truncated_details` 与总数。
const ANOMALY_LIMIT: usize = 100;

fn anomaly_report(by_label: &std::collections::HashMap<&str, &str>) -> Vec<serde_json::Value> {
    let mut entries: Vec<(&str, &str)> = by_label.iter().map(|(k, v)| (*k, *v)).collect();
    // 排序让回执稳定（HashMap 的迭代顺序不定，回执要可比对）。
    entries.sort_unstable();
    entries
        .into_iter()
        .take(ANOMALY_LIMIT)
        .map(|(label, reason)| serde_json::json!({ "label": label, "reason": reason }))
        .collect()
}

/// 读一条表级数据源的校验视图；不存在返回 `None`。
async fn load_datasource(
    context: &FeishuContext,
    datasource_id: i64,
) -> Result<Option<DatasourceRow>, BaseError> {
    let row = context
        .datasources()
        .query()
        .select_fields(&["ingest_mode", "status"])?
        .where_eq("id", serde_json::json!(datasource_id))?
        .optional()
        .await?;
    row.map(|row| {
        Ok(DatasourceRow {
            ingest_mode: row.optional::<String>("ingest_mode")?.unwrap_or_default(),
            status: row.optional::<String>("status")?.unwrap_or_default(),
        })
    })
    .transpose()
}

/// 取一条数据源**启用中**的绑定，按主键升序。
///
/// 升序不是装饰：逐绑定各一个事务，处理顺序就是这个数据源上可观测的提交顺序
/// （「前一条已提交、后一条失败」是逐绑定粒度唯一能给出的保证）。
///
/// 排序在 **Rust 侧**做而不是 `order_by`：`feishu_datasource_field.id` 没有开
/// `sortable`（DSL 的能力位是 fail-closed），走 SQL 排序会在运行期吃
/// `FieldPermissionDenied`。这里不为了排序去放宽一个列的能力位。
async fn load_enabled_bindings(
    context: &FeishuContext,
    datasource_id: i64,
) -> Result<Vec<TableBinding>, BaseError> {
    let rows = context
        .datasource_fields()
        .query()
        .select_fields(&[
            "id",
            "source_key",
            "field_id",
            "field_name",
            "parent_field_id",
            "snapshot_digest",
        ])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        // 只取启用中的：取消勾选 = 停用绑定，停用的列不参与本轮导入（与拉取同一条纪律）。
        .where_eq("enabled", serde_json::json!(true))?
        .all()
        .await?;

    let mut bindings = Vec::with_capacity(rows.len());
    for row in rows {
        bindings.push(TableBinding {
            id: row.require("id")?,
            source_key: row.require("source_key")?,
            field_id: row.require("field_id")?,
            field_name: row.optional("field_name")?,
            parent_field_id: row.optional("parent_field_id")?,
            snapshot_digest: row.optional("snapshot_digest")?,
        });
    }
    bindings.sort_by_key(|binding| binding.id);
    Ok(bindings)
}

/// 统计该数据源**已启用**的选项行数。
async fn count_active(context: &FeishuContext, source_key: &str) -> Result<u64, BaseError> {
    context
        .options()
        .query()
        .select_fields(&["option_id"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .where_eq("enabled", serde_json::json!(true))?
        .count()
        .await
}

/// 统计该数据源的存量行数与已启用行数。
async fn count_existing(
    context: &FeishuContext,
    source_key: &str,
) -> Result<(u64, u64), BaseError> {
    let total = count_option_rows(context.options(), source_key).await?;
    let active = count_active(context, source_key).await?;
    Ok((total, active))
}

/// 补集扫描的每页大小。**必须 ≤ 框架硬上限**：`TableQuery::page` 对超限是**拒绝**
/// 而不是 clamp，越界会让整轮导入在补集那一步必败。
const SCAN_PAGE_SIZE: usize = 100;

/// 编译期守护，同 `pull.rs` 里那条：`MAX_TABLE_QUERY_PAGE_SIZE` 一旦被调低，
/// 这里构建期就炸，而不是等到导入时才发现整轮必败。
const _: () = assert!(SCAN_PAGE_SIZE <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 单轮补集停用的规模上限：超过就**跳过本轮停用**，只告警。
const MAX_COMPLEMENT: usize = 20_000;

/// 补集扫描的结果。
struct DoomedScan {
    /// 本地有、本轮派生结果里没有的选项 id（该被停用）。
    doomed: Vec<String>,
    /// 本轮**跳过了**补集停用时的已启用行数；跑了扫描则是 `None`。
    ///
    /// 这个字段是承重的：`find_doomed_for` 返回空 `Vec` 有两种含义——「扫完了，没有
    /// 该停用的」与「**根本没扫**，因为你的表超过单轮上限」。回执上它们都写成
    /// `disabled: 0`，读起来都是「没什么要停用的」，而后者是**什么都没做**。
    /// 银行网点那份实测文件有 154,362 条选项，每一轮都走的是后者。
    skipped_at: Option<usize>,
}

/// 找出「本地有、本轮派生结果里没有」的选项 id（补集）。
///
/// 与 `pull::find_doomed` 同形，但不依赖 `PullDeps`——导入不出网，用不上 transport。
///
/// 规模保护分两步：**先 `count` 判是否超限，再分页扫**。顺序不能反——先扫再判的话，
/// 大表会把上限那么多行读进来才发现该跳过。
///
/// **跳过的理由要交回调用方**（见 [`DoomedScan::skipped_at`]）。这一段是从 `pull.rs`
/// 逐字抄来的，抄得没错——但在 worker 里没人看日志（那里 `tracing::warn` 就够），
/// 在 Action 里用户盯着回执：回执不说，用户就以为「没什么要停用的」。
async fn find_doomed_for(
    context: &FeishuContext,
    source_key: &str,
    derived: &[DerivedOption],
) -> Result<DoomedScan, BaseError> {
    let live: std::collections::HashSet<&str> = derived
        .iter()
        .map(|option| option.option_id.as_str())
        .collect();

    let enabled_query = || -> Result<yang_base::table::TableQuery, BaseError> {
        context
            .options()
            .query()
            .select_fields(&["option_id"])?
            .where_eq("source_key", serde_json::json!(source_key))?
            .where_eq("enabled", serde_json::json!(true))
    };

    // 第一步：先问总数。超限就没有必要把行读进来。
    let enabled = enabled_query()?.count().await? as usize;
    if enabled >= MAX_COMPLEMENT {
        tracing::warn!(
            source_key,
            enabled,
            limit = MAX_COMPLEMENT,
            "已启用选项达到单轮上限，本轮跳过补集停用（未确保视图完整时不做批量停用）"
        );
        return Ok(DoomedScan {
            doomed: Vec::new(),
            skipped_at: Some(enabled),
        });
    }

    // 第二步：分页扫完。`count` 已保证行数在上限内，所以页数有界。
    let mut rows = Vec::with_capacity(enabled);
    let mut page = 1usize;
    loop {
        let batch = enabled_query()?.page(page, SCAN_PAGE_SIZE)?.all().await?;
        let fetched = batch.len();
        rows.extend(batch);
        if fetched < SCAN_PAGE_SIZE {
            break;
        }
        page += 1;
    }

    let mut doomed = Vec::new();
    for row in &rows {
        let option_id: String = row.require("option_id")?;
        if !live.contains(option_id.as_str()) {
            doomed.push(option_id);
        }
    }
    Ok(DoomedScan {
        doomed,
        skipped_at: None,
    })
}

/// 当前时间（unix 秒）。
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_within_the_limit_is_not_touched() {
        let (label, anomaly) = truncate_label("中国工商银行成都春熙路支行", LABEL_LIMIT);
        assert_eq!(label, "中国工商银行成都春熙路支行");
        assert!(anomaly.is_none(), "没超限就不该背异常标记");
    }

    #[test]
    fn an_overlong_label_is_truncated_to_the_column_limit() {
        // 上限按**字符**而非字节算：`label` 是 varchar(255)，一个汉字在三字节的
        // 存储下仍是**一列**字符。按字节截会把 255 个汉字截成 85 个。
        let (label, anomaly) = truncate_label(&"长".repeat(300), LABEL_LIMIT);
        assert_eq!(label.chars().count(), LABEL_LIMIT);
        let reason = anomaly.unwrap_or_else(|| panic!("超长必须带原因"));
        assert!(reason.contains("300"), "原因要报出原长: {reason}");
    }

    #[test]
    fn the_anomaly_report_is_ordered_and_capped_without_silence() {
        // 排序让回执稳定：HashMap 的迭代顺序不定，回执要能逐轮比对。
        // 键用 ASCII 是为了**不把断言写成对排序规则的误解**：`sort_unstable` 是字节序，
        // 不是拼音序（"乙" U+4E59 排在 "甲" U+7532 前面）。这里要钉的是「有序且稳定」，
        // 不是「按什么序」。
        let mut by_label = std::collections::HashMap::new();
        by_label.insert("b", "原因");
        by_label.insert("a", "原因");
        assert_eq!(
            anomaly_report(&by_label),
            anomaly_report(&by_label),
            "同一次输入两次读取必须给出同一份清单"
        );
        let report = anomaly_report(&by_label);
        assert_eq!(report.len(), 2);
        assert_eq!(report[0]["label"], "a");
        assert_eq!(report[1]["label"], "b");
    }

    #[test]
    fn a_skipped_complement_scan_is_visible_in_the_receipt() {
        // 单轮补集停用的上限是 2 万，而银行网点那份实测文件有 154,362 条选项
        // （设计 §4.6）——**每一轮补集停用都被跳过**：改过名、删掉的旧选项永远
        // 保持 `enabled = true`，继续被出站喂给飞书控件。而那件事在回执上只表现为
        // `disabled: 0`，与「扫完了，没有要停用的」长得一模一样。
        //
        // 上限本身是设计决策（与 `pull.rs` 逐字同款），不动；回执必须自己说清
        // 这一轮**没有**做补集停用。这条用例钉的就是那一个字。
        let skipped = binding_report(BindingReport {
            source_key: "bank_branch",
            fetched: 154_386,
            derived: 154_362,
            disabled: 0,
            unchanged: false,
            snapshot_digest: Some("digest"),
            skipped_reason: None,
            complement_skipped: Some(154_362),
            anomalies: Vec::new(),
            truncated_details: false,
        });
        assert_eq!(
            skipped["complement_skipped"],
            serde_json::json!(154_362),
            "跳过了补集停用就必须在回执里写出来（值 = 当时已启用的选项数），\
             否则 `disabled: 0` 会被读成「没什么要停用的」"
        );

        // 真的扫过一轮时这里是 `null`：两种情形在回执上**必须可分辨**，
        // 而这正是消费方（前端回执）依据的那一个键。
        let scanned = binding_report(BindingReport {
            source_key: "bank_branch",
            fetched: 10,
            derived: 10,
            disabled: 2,
            unchanged: false,
            snapshot_digest: Some("digest"),
            skipped_reason: None,
            complement_skipped: None,
            anomalies: Vec::new(),
            truncated_details: false,
        });
        assert_eq!(scanned["complement_skipped"], serde_json::Value::Null);
        assert_eq!(scanned["disabled"], serde_json::json!(2));

        // 两条分支（整条绑定被跳过 / 照常落库）的键集必须一致：`skipped_reason` 与
        // `complement_skipped` 都在，缺省是 `null`。少一个键，消费方就得按分支猜。
        let keys = |value: &serde_json::Value| -> Vec<String> {
            value
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(keys(&skipped), keys(&scanned), "回执的键集不能随分支变化");
    }

    #[test]
    fn the_complement_scan_cannot_exceed_the_framework_page_limit() {
        // `TableQuery::page` 对超限是**拒绝**而不是 clamp，越界会让整轮导入必败。
        // 编译期断言已经钉住了它；这条测试的意义是让它在测试输出里也可见。
        const _: () = assert!(SCAN_PAGE_SIZE <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);
    }
}
