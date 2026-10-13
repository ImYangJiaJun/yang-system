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
    "access_mode",
    "passphrase_hash",
    "passphrase_expires_at",
    "exposed_columns",
    "result_labels",
    "empty_message",
    "rate_limit_per_min",
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

    // 精确匹配：永远只有 `source_key = ? AND label = ?`，不生成 LIKE / BETWEEN / IN。
    let matched = context
        .options()
        .query()
        .select_fields(&["source_key", "label"])?
        .where_eq("source_key", serde_json::json!(&query_source_key))?
        .where_eq("label", serde_json::json!(input.value.trim()))?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, MATCH_LIMIT)?
        .all()
        .await?;

    let rows = project_rows(&matched, &columns)?;
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
        // 精确匹配返回满页只可能是查询值本身不唯一（如同一卡号多行），
        // 这时必须明说被截断了，否则用户会以为看到的就是全部。
        format!("匹配结果超过 {MATCH_LIMIT} 条，仅显示前 {MATCH_LIMIT} 条")
    } else {
        String::new()
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
async fn find_portal(context: &FeishuContext, slug: &str) -> Result<Option<Record>, BaseError> {
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

/// 把匹配到的选项行投影成结果行。
///
/// # 已知天花板
///
/// 一次查询只落在**查询键那一个 `source_key`** 上（设计 §4.2 的精确匹配），
/// 而 `feishu_option` 一行 = 一个字段的一个取值，所以一行只能填上
/// `source_key` 与它相同的那个开放列。
///
/// ponytail: 一个记录里「卡号 → 店铺 / 额度 / 到期日」这种多列结果，要靠
/// `feishu_option.parent_key` 把同一记录的各字段串起来（子字段的绑定
/// `parent_field_id` 指向查询键，拉取时 `derive` 就会把 `parent_key` 写成父选项的
/// `option_id`）。真要做多列，就在拿到命中行的 `option_id` 后按
/// `parent_key IN (...) AND source_key IN (开放列的 source_key)` 再查一轮并合并。
/// 当前按任务书的单查询形态实现。
fn project_rows(
    matched: &[Record],
    columns: &[ExposedColumn],
) -> Result<Vec<serde_json::Value>, BaseError> {
    let mut rows = Vec::with_capacity(matched.len());
    for record in matched {
        let row_source_key: String = record.require("source_key")?;
        let label: String = record.require("label")?;
        let mut values = serde_json::Map::new();
        for column in columns {
            if column.source_key == row_source_key {
                values.insert(column.column.field_id.clone(), serde_json::json!(label));
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

    #[test]
    fn a_row_only_fills_the_columns_whose_source_key_it_belongs_to() {
        // 天花板已记录在 `project_rows` 的文档注释里：一行 = 一个字段的一个取值。
        let columns = vec![
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
        ];
        let mut record = Record::new();
        record.insert("source_key", serde_json::json!("shop"));
        record.insert("label", serde_json::json!("XX 旗舰店"));

        let rows = project_rows(&[record], &columns).unwrap_or_else(|e| panic!("投影应成功: {e}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["values"]["fldShop"], serde_json::json!("XX 旗舰店"));
        assert!(rows[0]["values"].get("fldCard").is_none());
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
}
