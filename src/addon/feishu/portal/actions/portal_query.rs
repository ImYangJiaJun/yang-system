//! 门户公开查询端点。
//!
//! # 为什么是 `public`
//!
//! 门户的访问控制**不靠框架 JWT**：三种访问模式（公开 / 口令 / 登录）里只有
//! `login` 依赖框架身份，另两种对匿名用户开放。所以端点整体注册为 `public`，
//! 由本 handler 按门户配置自行判权（与 `approval_options` 同一取舍）。
//!
//! # 业务失败返回 `Ok(ApiResponse::fail(..))`，不用 `Err`
//!
//! 匿名端点的 `Err` 会在可观测性里记成 `result="error"` 并消耗全站可用性预算，
//! 而 burn-rate 规则按 `sum by (job)` 聚合、不带 operation 维度——被猜口令的流量
//! 不该把全站告警打起来。`Err` 只留给「限速」（要 429）与真正的内部故障。
//!
//! # 查询值不落明文
//!
//! 日志只写 SHA-256（复用 `domain::token::hash_token`），事后仍可审计
//! 「某个卡号是否被查过」。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::transport::client_ip::client_ip_identity;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::rate_limit;
use crate::addon::feishu::domain::token::hash_token;

/// 门户行的投影列（公开端点只读这些）。
const PORTAL_COLUMNS: &[&str] = &[
    "id",
    "datasource_id",
    "query_field_source_key",
    "query_field_id",
    "access_mode",
    "passphrase_hash",
    "passphrase_expires_at",
    "exposed_columns",
    "result_labels",
    "empty_message",
    "rate_limit_per_min",
    "fuzzy_enabled",
    "fuzzy_min_digits",
    "enabled",
];

/// 单次查询最多返回的行数（设计 §4.2 第 6 条：精确匹配通常 0 或 1 行）。
const MATCH_LIMIT: usize = 100;

/// 查询值长度上限（与 `feishu_option.label` 的 255 列宽一致）。
const VALUE_MAX: usize = 255;

/// `ip_address` / `user_agent` 的列宽。
const IP_MAX: usize = 45;
const UA_MAX: usize = 500;

/// 未找到匹配记录时的默认提示。
const DEFAULT_EMPTY_MESSAGE: &str = "未找到匹配记录";

/// 失败信封的错误码。
mod codes {
    /// 门户不存在 / 已停用（两者同码，不泄露「这个 slug 存在但被停用」）。
    pub(super) const PORTAL_NOT_FOUND: i32 = 40401;
    /// 访问被拒（口令错误 / 口令过期 / 未登录）。与「无权限」统一，不泄露口令是否存在。
    pub(super) const FORBIDDEN: i32 = 40301;
}

/// 公开查询的输入契约（`slug` 走路径参数，从 `ctx.request` 取）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PortalQueryInput {
    /// 查询值（精确匹配）。
    pub(super) value: String,
    /// 门户口令；`access_mode = passphrase` 时必填。
    #[serde(default)]
    pub(super) passphrase: Option<String>,
}

impl ParamInput for PortalQueryInput {
    fn params() -> Params {
        Params::new()
    }
}

impl PortalQueryInput {
    fn validate(&self) -> Result<(), BaseError> {
        // 空值会匹配到「空文案」的选项行，等于一次无意义的全表探测；直接拒掉。
        if self.value.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "value".to_string(),
                "查询值不能为空".to_string(),
            ));
        }
        if self.value.chars().count() > VALUE_MAX {
            return Err(BaseError::ParamInvalid(
                "value".to_string(),
                format!("查询值不得超过 {VALUE_MAX} 字符"),
            ));
        }
        Ok(())
    }
}

/// 一个开放列的展示定义。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct PortalColumn {
    /// 列标识（`field_id`）。
    field_id: String,
    /// 展示名（管理员配置的或字段名）。
    label: String,
    /// 列类型。
    field_type: String,
}

/// 解析后的开放列：展示定义 + 定位数据用的 `source_key`。
struct ExposedColumn {
    column: PortalColumn,
    source_key: String,
}

/// 路由模板里的路径参数名。
const SLUG_PARAM: &str = "slug";

/// 公开查询端点的路由。
const ROUTE: &str = "/api/v1/feishu/portal/{slug}/query";

/// 注册公开查询端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("portal_query"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, ROUTE)
        .display_name("门户查询")
        .description("按精确值查询门户开放的数据；访问控制由门户的访问模式决定")
        // public：三种访问模式里只有 login 依赖框架身份，判权在 handler 内完成。
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: PortalQueryInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let slug = ctx
        .request
        .get_path_param(SLUG_PARAM)
        .unwrap_or_default()
        .trim()
        .to_string();
    if slug.is_empty() {
        return Ok(ApiResponse::fail(codes::PORTAL_NOT_FOUND, "门户不存在"));
    }

    let Some(portal) = find_portal(&context, &slug).await? else {
        return Ok(ApiResponse::fail(codes::PORTAL_NOT_FOUND, "门户不存在"));
    };
    let portal_id: i64 = portal.require("id")?;
    let access_mode: String = portal
        .optional("access_mode")?
        .unwrap_or_else(|| "public".to_string());
    let client_ip = truncate(&client_ip_identity(&ctx), IP_MAX);
    let user_agent = ctx
        .request
        .get_header("user-agent")
        .map(|value| truncate(value, UA_MAX));

    // 停用的门户对公开访问一律「不存在」：不泄露「这个 slug 存在但被停用」。
    if !portal.optional::<bool>("enabled")?.unwrap_or(true) {
        write_log(
            &context,
            portal_id,
            &input.value,
            0,
            &client_ip,
            user_agent.as_deref(),
            &access_mode,
            "forbidden",
        )
        .await;
        return Ok(ApiResponse::fail(codes::PORTAL_NOT_FOUND, "门户不存在"));
    }

    // 限速在**判权之前**：否则猜口令的流量会绕过限速直接压 argon2。
    let limit = portal
        .optional::<i64>("rate_limit_per_min")?
        .unwrap_or(30)
        .clamp(1, i64::from(u32::MAX)) as u32;
    if let Err(error) = rate_limit::check_query(&ctx, &slug, &client_ip, limit).await {
        write_log(
            &context,
            portal_id,
            &input.value,
            0,
            &client_ip,
            user_agent.as_deref(),
            &access_mode,
            "rate_limited",
        )
        .await;
        return Err(error);
    }

    if let Err(reason) = authorize(
        &ctx,
        &context,
        &access_mode,
        input.passphrase.as_deref(),
        portal.optional::<String>("passphrase_hash")?.as_deref(),
        portal.optional::<i64>("passphrase_expires_at")?,
    )
    .await?
    {
        write_log(
            &context,
            portal_id,
            &input.value,
            0,
            &client_ip,
            user_agent.as_deref(),
            &access_mode,
            "forbidden",
        )
        .await;
        return Ok(ApiResponse::fail(codes::FORBIDDEN, reason));
    }

    let datasource_id: i64 = portal.require("datasource_id")?;
    let query_source_key: String = portal.require("query_field_source_key")?;
    let exposed: Vec<String> =
        serde_json::from_str(&portal.require::<String>("exposed_columns")?).unwrap_or_default();
    let labels: HashMap<String, String> = portal
        .optional::<String>("result_labels")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();

    let columns = load_exposed_columns(&context, datasource_id, &exposed, &labels).await?;

    // 第一步：精确匹配命中行（`source_key = ? AND label = ?`，不生成 LIKE / BETWEEN）。
    // 查询键是根级字段（`parent_field_id` 为空），它的 `option_id` 就是同一记录里
    // 各开放列子行的 `parent_key`——这是 `domain::derive` 构造性保证的不变式。
    let matched = context
        .options()
        .query()
        .select_fields(&["option_id", "source_key", "label"])?
        .where_eq("source_key", serde_json::json!(&query_source_key))?
        .where_eq("label", serde_json::json!(input.value.trim()))?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, MATCH_LIMIT)?
        .all()
        .await?;

    // 精确匹配落空时退一步做模糊匹配：用户敲入 ≥min_digits 位连续数字就当成在找卡号。
    // 命中的行要明说是模糊结果，否则用户会把「前几位相同」误当成精确命中。
    let fuzzy_enabled = portal.optional::<bool>("fuzzy_enabled")?.unwrap_or(true);
    let fuzzy_min_digits = portal.optional::<i64>("fuzzy_min_digits")?.unwrap_or(4) as usize;
    let (matched, fuzzy_message) = if matched.is_empty() && fuzzy_enabled {
        let fuzzy =
            fuzzy_match(&context, &query_source_key, &input.value, fuzzy_min_digits).await?;
        if fuzzy.is_empty() {
            (fuzzy, String::new())
        } else {
            let count = fuzzy.len();
            (fuzzy, format!("模糊匹配到 {count} 条结果"))
        }
    } else {
        (matched, String::new())
    };

    // 第二步：按命中行的 `option_id` 反查开放列子行，合并成多列结果。
    let children = load_child_values(&context, &matched, &columns).await?;
    let rows = project_rows(&matched, &children, &columns)?;
    let status = if rows.is_empty() { "miss" } else { "hit" };
    write_log(
        &context,
        portal_id,
        &input.value,
        rows.len() as i64,
        &client_ip,
        user_agent.as_deref(),
        &access_mode,
        status,
    )
    .await;

    let message = if rows.is_empty() {
        portal
            .optional::<String>("empty_message")?
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_EMPTY_MESSAGE.to_string())
    } else if rows.len() == MATCH_LIMIT {
        // 匹配返回满页只可能是查询值本身不唯一（如同一卡号多行）或模糊粗筛被截断，
        // 这时必须明说被截断了，否则用户会以为看到的就是全部。
        format!("匹配结果超过 {MATCH_LIMIT} 条，仅显示前 {MATCH_LIMIT} 条")
    } else {
        fuzzy_message
    };

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "found": !rows.is_empty(),
            "columns": columns.iter().map(|column| &column.column).collect::<Vec<_>>(),
            "rows": rows,
            "message": message,
        }),
        "查询完成",
    ))
}

/// 按 slug 或自定义短链定位门户。
pub(super) async fn find_portal(
    context: &FeishuContext,
    slug: &str,
) -> Result<Option<Record>, BaseError> {
    let by_slug = context
        .portals()
        .query()
        .select_fields(PORTAL_COLUMNS)?
        .where_eq("slug", serde_json::json!(slug))?
        .optional()
        .await?;
    if by_slug.is_some() {
        return Ok(by_slug);
    }
    context
        .portals()
        .query()
        .select_fields(PORTAL_COLUMNS)?
        .where_eq("custom_slug", serde_json::json!(slug))?
        .optional()
        .await
}

/// 判权；`Ok(Err(文案))` 表示业务拒绝（写日志后以失败信封返回），`Err` 是内部故障。
///
/// 返回 `Result<Result<(), String>, BaseError>` 而不是 `Result<(), BaseError>`：
/// 「拒绝」不是故障，把它做成 `Err` 会让每次被拒都记成 `result="error"`。
async fn authorize(
    ctx: &ActionContext,
    context: &FeishuContext,
    access_mode: &str,
    presented: Option<&str>,
    stored_hash: Option<&str>,
    expires_at: Option<i64>,
) -> Result<Result<(), String>, BaseError> {
    match access_mode {
        "public" => Ok(Ok(())),
        "login" => {
            if ctx.authenticated_user().is_some() {
                Ok(Ok(()))
            } else {
                Ok(Err("需要登录后才能查询".to_string()))
            }
        }
        "passphrase" => {
            if let Some(expires_at) = expires_at {
                if now_unix() > expires_at {
                    return Ok(Err("口令已过期".to_string()));
                }
            }
            let Some(stored_hash) = stored_hash else {
                // 口令模式却没存哈希：配置残缺。对调用方仍然只说「口令不正确」。
                return Ok(Err("口令不正确".to_string()));
            };
            let Some(presented) = presented else {
                return Ok(Err("请输入口令".to_string()));
            };
            let matched = context
                .password_engine()
                .verify(presented, stored_hash)
                .await?;
            if matched {
                Ok(Ok(()))
            } else {
                Ok(Err("口令不正确".to_string()))
            }
        }
        // 未知模式 fail-closed：库里出现脏值时拒绝，而不是当成 public 放行。
        _ => Ok(Err("门户访问模式无效".to_string())),
    }
}

/// 把开放列解析成「展示定义 + `source_key`」。
async fn load_exposed_columns(
    context: &FeishuContext,
    datasource_id: i64,
    exposed: &[String],
    labels: &HashMap<String, String>,
) -> Result<Vec<ExposedColumn>, BaseError> {
    if exposed.is_empty() {
        return Ok(Vec::new());
    }
    let values = exposed
        .iter()
        .map(|field_id| serde_json::json!(field_id))
        .collect::<Vec<_>>();
    let rows = context
        .datasource_fields()
        .query()
        .select_fields(&["field_id", "field_name", "source_key"])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .where_in("field_id", values)?
        .all()
        .await?;
    let by_id: HashMap<String, (Option<String>, String)> = rows
        .iter()
        .map(|row| {
            Ok::<_, BaseError>((
                row.require::<String>("field_id")?,
                (row.optional("field_name")?, row.require("source_key")?),
            ))
        })
        .collect::<Result<_, _>>()?;

    // 保持 `exposed_columns` 里的顺序：那是管理员勾选的展示次序。
    Ok(exposed
        .iter()
        .filter_map(|field_id| {
            let (field_name, source_key) = by_id.get(field_id)?;
            let label = labels
                .get(field_id)
                .cloned()
                .or_else(|| field_name.clone())
                .unwrap_or_else(|| field_id.clone());
            Some(ExposedColumn {
                column: PortalColumn {
                    field_id: field_id.clone(),
                    label,
                    // 绑定表没有存字段类型（`feishu_datasource_field` 只有身份与凭据），
                    // 前端只把它当格式化提示用，未知时退化成纯文本最安全。
                    field_type: "text".to_string(),
                },
                source_key: source_key.clone(),
            })
        })
        .collect())
}

/// 一次取回的开放列数据行数上限（= 查询层每页硬上限 `MAX_TABLE_QUERY_PAGE_SIZE`）。
///
/// 开放列数据是「命中行 × 开放列」的展开，精确匹配下命中行通常 0 或 1 行，远够用。
const CHILD_LIMIT: usize = 100;

/// 取命中行在开放列上的数据，按父选项分组。
///
/// 返回 `命中行 option_id -> (开放列 source_key -> 文案)`。`feishu_option` 一行 = 一个
/// 字段的一个取值，同一记录的其他字段靠 `parent_key`（= 父选项的 `option_id`）串起来。
async fn load_child_values(
    context: &FeishuContext,
    matched: &[Record],
    columns: &[ExposedColumn],
) -> Result<HashMap<String, HashMap<String, String>>, BaseError> {
    if matched.is_empty() || columns.is_empty() {
        return Ok(HashMap::new());
    }
    let option_ids = matched
        .iter()
        .map(|record| record.require::<String>("option_id"))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|option_id| serde_json::json!(option_id))
        .collect();
    let source_keys = columns
        .iter()
        .map(|column| serde_json::json!(&column.source_key))
        .collect::<Vec<_>>();

    let rows = context
        .options()
        .query()
        .select_fields(&["parent_key", "source_key", "label"])?
        .where_in("parent_key", option_ids)?
        .where_in("source_key", source_keys)?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, CHILD_LIMIT)?
        .all()
        .await?;

    let mut by_parent: HashMap<String, HashMap<String, String>> = HashMap::new();
    for record in &rows {
        let parent_key = record.optional::<String>("parent_key")?.unwrap_or_default();
        // 根级行的父键是空串，永远不该挂到某个命中行上。
        if parent_key.is_empty() {
            continue;
        }
        by_parent
            .entry(parent_key)
            .or_default()
            .insert(record.require("source_key")?, record.require("label")?);
    }
    Ok(by_parent)
}

/// 把命中行投影成结果行。
///
/// 每行 = 命中行自身的取值（只有查询键也被勾进开放列时才有）+ 它名下所有开放列数据。
/// 子列按 `parent_key` 归并到对应命中行：`feishu_option.parent_key` 存的是父选项的
/// `option_id`，由 `domain::derive` 在拉取时写入。
fn project_rows(
    matched: &[Record],
    children: &HashMap<String, HashMap<String, String>>,
    columns: &[ExposedColumn],
) -> Result<Vec<serde_json::Value>, BaseError> {
    let mut rows = Vec::with_capacity(matched.len());
    for record in matched {
        let option_id: String = record.require("option_id")?;
        let row_source_key: String = record.require("source_key")?;
        let label: String = record.require("label")?;
        let mut values = serde_json::Map::new();
        for column in columns {
            if column.source_key == row_source_key {
                values.insert(column.column.field_id.clone(), serde_json::json!(label));
            }
            if let Some(value) = children
                .get(&option_id)
                .and_then(|by_source| by_source.get(&column.source_key))
            {
                values.insert(column.column.field_id.clone(), serde_json::json!(value));
            }
        }
        rows.push(serde_json::json!({ "values": values }));
    }
    Ok(rows)
}

/// 写一条访问日志。
///
/// # 为什么写失败只告警、不影响查询
///
/// 日志是事后审计事实，不参与判权。让一次日志写失败把已经查到的结果吞掉，
/// 等于把「日志库抖一下」放大成「门户不可用」——同 `dispatch.rs` 对请求记录的取舍。
#[allow(clippy::too_many_arguments)]
async fn write_log(
    context: &FeishuContext,
    portal_id: i64,
    query_value: &str,
    result_count: i64,
    client_ip: &str,
    user_agent: Option<&str>,
    access_mode: &str,
    status: &str,
) {
    let mut record = Record::new();
    record.insert("portal_id", serde_json::json!(portal_id));
    record.insert(
        "query_value_hash",
        serde_json::json!(hash_token(query_value.trim())),
    );
    record.insert("result_count", serde_json::json!(result_count));
    record.insert("ip_address", serde_json::json!(client_ip));
    record.insert(
        "user_agent",
        match user_agent {
            Some(value) => serde_json::json!(value),
            None => serde_json::Value::Null,
        },
    );
    record.insert("access_mode_used", serde_json::json!(access_mode));
    record.insert("status", serde_json::json!(status));

    if let Err(error) = context.portal_logs().query().insert(record).await {
        tracing::warn!(
            error = %error,
            portal_id,
            status,
            "门户访问日志写入失败，查询结果不受影响"
        );
    }
}

/// 按字符截断，避免超列宽（`ip_address` 45 / `user_agent` 500）。
fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// 当前 unix 秒。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 模糊匹配：输入含 ≥`min_digits` 位连续数字时，用前 `min_digits` 位数字做 LIKE 粗筛，
/// 再在 Rust 里精筛。
///
/// 返回命中的行；输入凑不出 ≥`min_digits` 位连续数字（不够像卡号）时返回空，不触发模糊。
async fn fuzzy_match(
    context: &FeishuContext,
    query_source_key: &str,
    value: &str,
    min_digits: usize,
) -> Result<Vec<Record>, BaseError> {
    let Some(like_key) = fuzzy_like_key(value, min_digits) else {
        return Ok(Vec::new());
    };
    let input_digits = extract_digits(value);

    let candidates = context
        .options()
        .query()
        .select_fields(&["option_id", "source_key", "label"])?
        .where_eq("source_key", serde_json::json!(query_source_key))?
        .where_contains("label", &like_key)?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, MATCH_LIMIT)?
        .all()
        .await?;
    Ok(fuzzy_filter_candidates(&candidates, &input_digits))
}

/// 提取字符串里的全部 ASCII 数字，按出现顺序拼接。
pub(super) fn extract_digits(input: &str) -> String {
    input.chars().filter(char::is_ascii_digit).collect()
}

/// 找到输入里第一段长度 ≥ `min_len` 的连续 ASCII 数字。
pub(super) fn first_digit_run(input: &str, min_len: usize) -> Option<String> {
    let mut run = String::new();
    for ch in input.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
        } else {
            if run.len() >= min_len {
                return Some(run);
            }
            run.clear();
        }
    }
    (run.len() >= min_len).then_some(run)
}

/// 模糊匹配的 LIKE 粗筛关键词：输入里第一段 ≥`min_digits` 位的连续数字，只取前 4 位。
///
/// 只取 4 位是因为库里卡号按 4 位分组（`"4514 6175 ..."`）——整段数字在存储里
/// 并不连续，`LIKE '%45146175%'` 反而落空；只有落在单个分组内的 4 位数字能命中。
/// 用户整段粘贴（`"4514617593174990"`）与分段输入（`"6175 9317"`）都因此能粗筛到。
///
/// `min_digits` 只决定**触发门槛**（不够长就不模糊），不改变粗筛取的位数。
///
/// ponytail: 粗筛固定取 4 位、且只取第一页 `MATCH_LIMIT` 条候选——若上百张卡共享
/// 同一 4 位前缀，真命中有可能落在被截断的候选之外。卡号 4 位前缀撞车到这个量级
/// 才会失手；真出现时再按 `option_id` 分页扫全量候选。
pub(super) fn fuzzy_like_key(value: &str, min_digits: usize) -> Option<String> {
    first_digit_run(value, min_digits).map(|run| run.chars().take(4).collect())
}

/// 精筛：候选项去掉非数字后，必须包含输入的全部数字。
///
/// 这样 `"4514 6175 9317 4990"` 与 `"4514617593174990"` 视为同一卡号，输入只给
/// 中间一段（`"61759317"`）也能命中。
pub(super) fn fuzzy_filter_candidates(candidates: &[Record], input_digits: &str) -> Vec<Record> {
    candidates
        .iter()
        .filter(|record| {
            let label: String = record
                .optional::<String>("label")
                .ok()
                .flatten()
                .unwrap_or_default();
            extract_digits(&label).contains(input_digits)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(value: &str) -> PortalQueryInput {
        PortalQueryInput {
            value: value.to_string(),
            passphrase: None,
        }
    }

    #[test]
    fn accepts_a_plain_lookup() {
        assert!(input("6222020200112233").validate().is_ok());
    }

    #[test]
    fn rejects_a_blank_or_over_long_value() {
        assert!(input("   ").validate().is_err());
        assert!(input(&"9".repeat(256)).validate().is_err());
        assert!(input(&"9".repeat(255)).validate().is_ok());
    }

    #[test]
    fn the_input_accepts_the_documented_body_shape() {
        let parsed = serde_json::from_value::<PortalQueryInput>(serde_json::json!({
            "value": "6222020200112233",
            "passphrase": "s3cret"
        }))
        .unwrap_or_else(|error| panic!("公开查询体必须可解析: {error}"));
        assert_eq!(parsed.passphrase.as_deref(), Some("s3cret"));

        // `deny_unknown_fields` 是防线：拼错的键必须响亮失败，而不是被静默忽略成
        // 「没传口令」——那会把一次可诊断的调用错误变成一次莫名的 403。
        assert!(
            serde_json::from_value::<PortalQueryInput>(serde_json::json!({
                "value": "x",
                "passphase": "typo"
            }))
            .is_err()
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // 按字节截断会把多字节字符切成两半，MySQL 写入时报无效编码。
        let long = "字".repeat(600);
        assert_eq!(truncate(&long, UA_MAX).chars().count(), UA_MAX);
        assert_eq!(truncate("短", UA_MAX), "短");
    }

    fn exposed_columns() -> Vec<ExposedColumn> {
        vec![
            ExposedColumn {
                column: PortalColumn {
                    field_id: "fldShop".to_string(),
                    label: "店铺".to_string(),
                    field_type: "text".to_string(),
                },
                source_key: "shop".to_string(),
            },
            ExposedColumn {
                column: PortalColumn {
                    field_id: "fldCard".to_string(),
                    label: "卡号".to_string(),
                    field_type: "text".to_string(),
                },
                source_key: "card".to_string(),
            },
        ]
    }

    fn matched_card(option_id: &str, label: &str) -> Record {
        let mut record = Record::new();
        record.insert("option_id", serde_json::json!(option_id));
        record.insert("source_key", serde_json::json!("card"));
        record.insert("label", serde_json::json!(label));
        record
    }

    #[test]
    fn a_matched_row_merges_its_child_columns() {
        // 链式合并：命中行只带查询键的取值，其他开放列靠 `parent_key` 挂回来。
        let columns = exposed_columns();
        let mut children: HashMap<String, HashMap<String, String>> = HashMap::new();
        children
            .entry("card:abc".to_string())
            .or_default()
            .insert("shop".to_string(), "XX 旗舰店".to_string());

        let rows = project_rows(&[matched_card("card:abc", "6222")], &children, &columns)
            .unwrap_or_else(|e| panic!("投影应成功: {e}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["values"]["fldCard"], serde_json::json!("6222"));
        assert_eq!(rows[0]["values"]["fldShop"], serde_json::json!("XX 旗舰店"));
    }

    #[test]
    fn another_rows_children_do_not_leak_in() {
        // 子行按 `option_id` 归并，别的命中行的数据不得串进来。
        let columns = exposed_columns();
        let mut children: HashMap<String, HashMap<String, String>> = HashMap::new();
        children
            .entry("card:other".to_string())
            .or_default()
            .insert("shop".to_string(), "不该出现".to_string());

        let rows = project_rows(&[matched_card("card:abc", "6222")], &children, &columns)
            .unwrap_or_else(|e| panic!("投影应成功: {e}"));
        assert_eq!(rows[0]["values"]["fldCard"], serde_json::json!("6222"));
        assert!(rows[0]["values"].get("fldShop").is_none());
    }

    #[test]
    fn a_matched_row_without_children_keeps_only_the_query_key() {
        let rows = project_rows(
            &[matched_card("card:abc", "6222")],
            &HashMap::new(),
            &exposed_columns(),
        )
        .unwrap_or_else(|e| panic!("投影应成功: {e}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["values"]["fldCard"], serde_json::json!("6222"));
        assert!(rows[0]["values"].get("fldShop").is_none());
    }

    #[test]
    fn the_route_carries_the_path_parameter_the_handler_reads() {
        // 路由模板里的参数名与 `get_path_param` 的实参必须同一个来源：写错一处
        // `get_path_param` 恒为 None，端点对任何请求都回 404，而这类错误不会在
        // 启动期暴露。
        assert_eq!(SLUG_PARAM, "slug");
        assert!(
            ROUTE.contains(&format!("{{{SLUG_PARAM}}}")),
            "路由必须带 {{{SLUG_PARAM}}} 路径参数，实际是 {ROUTE}"
        );
    }

    #[test]
    fn extract_digits_keeps_only_ascii_digits() {
        assert_eq!(extract_digits("4514 6175-9317"), "451461759317");
        assert_eq!(extract_digits("卡号：4514"), "4514");
        assert_eq!(extract_digits("没有数字"), "");
    }

    #[test]
    fn first_digit_run_finds_the_first_run_of_enough_digits() {
        assert_eq!(
            first_digit_run("abc45146175def", 4).as_deref(),
            Some("45146175")
        );
        // 前面那段不足 4 位，跳过它，取后面那段。
        assert_eq!(first_digit_run("12 34567", 4).as_deref(), Some("34567"));
        // 收尾的连续段也要能取到（循环结束后的兜底分支）。
        assert_eq!(first_digit_run("ab1234", 4).as_deref(), Some("1234"));
        // 没有任何 ≥4 位的连续数字段。
        assert_eq!(first_digit_run("12 34 56", 4), None);
    }

    #[test]
    fn fuzzy_like_key_takes_four_digits_so_spaced_labels_still_match() {
        // 整段 16 位在库里带空格，LIKE 只能取前 4 位，否则 %45146175% 反而落空。
        assert_eq!(
            fuzzy_like_key("4514617593174990", 4).as_deref(),
            Some("4514")
        );
        assert_eq!(fuzzy_like_key("6175 9317", 4).as_deref(), Some("6175"));
        assert_eq!(fuzzy_like_key("abc45146175def", 4).as_deref(), Some("4514"));
        // 不足 4 位连续数字：不触发模糊。
        assert_eq!(fuzzy_like_key("123", 4), None);
        assert_eq!(fuzzy_like_key("12 34", 4), None);
    }

    #[test]
    fn fuzzy_like_key_honours_a_custom_min_digits_threshold() {
        // 门槛只决定触不触发，不改变粗筛取的位数（仍是前 4 位）。
        assert_eq!(fuzzy_like_key("12345", 6), None);
        assert_eq!(fuzzy_like_key("123456", 6).as_deref(), Some("1234"));
    }

    #[test]
    fn fuzzy_filter_ignores_spaces_and_matches_on_digit_substrings() {
        let candidates = vec![
            matched_card("card:a", "4514 6175 9317 4990"),
            matched_card("card:b", "9999 8888 7777 6666"),
        ];

        // 整段粘贴（无空格）应命中带空格的库值。
        let hits = fuzzy_filter_candidates(&candidates, "4514617593174990");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].require::<String>("option_id").unwrap_or_else(|error| panic!("option_id must be present: {error}")), "card:a");

        // 只给中间一段，去掉非数字后包含即可命中。
        assert_eq!(fuzzy_filter_candidates(&candidates, "61759317").len(), 1);
        // 数字对不上：不命中。
        assert!(fuzzy_filter_candidates(&candidates, "1234").is_empty());
    }
}
