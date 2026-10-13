//! 按批次补充展示快照；不读取记录、不派发审批。

use crate::{
    addon::feishu::domain::{
        approval::get_approval_definition,
        approval_provision::display_name,
        bitable::{get_base_name, list_all_fields, list_all_tables, BitableCoordinates},
        context::FeishuContext,
        outbound::{OutboundTransport, Sleeper},
        outbound_setup,
        tenant_token::TenantTokenProvider,
    },
    infrastructure::audit,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use yang_base::{
    action::{ActionContext, ApiResponse},
    definition::{HttpMethod, ModuleSpec, ParamInput, Params},
    table::Record,
    BaseError,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RefreshNamesInput {
    config_ids: Vec<i64>,
}
impl ParamInput for RefreshNamesInput {
    fn params() -> Params {
        Params::new()
    }
}
impl RefreshNamesInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_ids.is_empty()
            || self.config_ids.len() > 10
            || self.config_ids.iter().any(|id| *id <= 0)
        {
            return Err(BaseError::ParamInvalid(
                "config_ids".into(),
                "每批提供1到10个正整数配置编号".into(),
            ));
        }
        Ok(())
    }
}
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("refresh_names"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/configs/refresh-names",
        )
        .display_name("补充审批配置名称")
        .description("按批次读取飞书元数据并保存展示名称，失败时保留已有名称")
        .permissions(["feishu.approval.write"])
        .register()
}
pub(super) async fn handle(
    ctx: ActionContext,
    input: RefreshNamesInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };
    let outbound = outbound_setup::build(&ctx, settings)?;
    refresh(
        &ctx,
        &input.config_ids,
        &context,
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
    )
    .await
}

async fn refresh(
    ctx: &ActionContext,
    config_ids: &[i64],
    context: &FeishuContext,
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
) -> Result<ApiResponse, BaseError> {
    let ids: HashSet<i64> = config_ids.iter().copied().collect();
    let rows = context
        .approval_configs()
        .query()
        .where_in("id", ids.iter().map(|id| serde_json::json!(id)).collect())?
        .all()
        .await?;
    let mut bases = HashMap::new();
    let mut tables = HashMap::new();
    let mut approvals = HashMap::new();
    let mut updates = Vec::new();
    let mut incomplete_ids: Vec<i64> = ids
        .iter()
        .filter(|id| {
            !rows
                .iter()
                .any(|row| row.optional::<i64>("id").ok().flatten() == Some(**id))
        })
        .copied()
        .collect();
    for row in rows {
        let id: i64 = row.require("id")?;
        let base: String = row.require("base_token")?;
        let table: String = row.require("table_id")?;
        let code: String = row.require("approval_code")?;
        if !bases.contains_key(&base) {
            bases.insert(
                base.clone(),
                get_base_name(
                    transport,
                    sleeper,
                    tokens,
                    &base,
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置多维表名称补充失败");
                    None
                }),
            );
            tables.insert(
                base.clone(),
                list_all_tables(
                    transport,
                    sleeper,
                    tokens,
                    &base,
                )
                .await
                .map(Some)
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置数据表名称补充失败");
                    None
                }),
            );
        }
        if !approvals.contains_key(&code) {
            approvals.insert(
                code.clone(),
                get_approval_definition(
                    transport,
                    sleeper,
                    tokens,
                    &code,
                )
                .await
                .map(|definition| Some(definition.approval_name))
                .unwrap_or_else(|error| {
                    tracing::warn!(config_id = id, kind = ?error.kind, "审批配置审批名称补充失败");
                    None
                }),
            );
        }
        let fields = list_all_fields(
            transport,
            sleeper,
            tokens,
            &BitableCoordinates {
                app_token: base.clone(),
                table_id: table.clone(),
                view_id: None,
            },
        )
        .await
        .map(Some)
        .unwrap_or_else(|error| {
            tracing::warn!(config_id = id, kind = ?error.kind, "审批配置列名称补充失败");
            None
        });
        let applicant: String = row.require("applicant_field")?;
        let backfill: String = row.require("backfill_field")?;
        let names = [
            (
                "base_name",
                bases.get(&base).and_then(|name| name.as_deref()),
            ),
            (
                "table_name",
                tables
                    .get(&base)
                    .and_then(|items| items.as_ref())
                    .and_then(|items| items.iter().find(|item| item.table_id == table))
                    .map(|item| item.name.as_str()),
            ),
            (
                "approval_name",
                approvals.get(&code).and_then(|name| name.as_deref()),
            ),
            (
                "applicant_field_name",
                fields
                    .as_ref()
                    .and_then(|items| items.iter().find(|item| item.field_id == applicant))
                    .map(|item| item.field_name.as_str()),
            ),
            (
                "backfill_field_name",
                fields
                    .as_ref()
                    .and_then(|items| items.iter().find(|item| item.field_id == backfill))
                    .map(|item| item.field_name.as_str()),
            ),
        ];
        let mut record = Record::new();
        let mut missing = false;
        for (key, name) in names {
            if let Some(name) = display_name(name) {
                record.insert(key, serde_json::json!(name));
            } else {
                missing = true;
            }
        }
        if missing {
            incomplete_ids.push(id);
        }
        if !record.as_map().is_empty() {
            updates.push((id, record));
        }
    }
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        for (id, record) in updates {
            let affected = context
                .approval_configs()
                .query()
                .where_eq("id", serde_json::json!(id))?
                .update_in_tx(&mut transaction, record)
                .await?;
            if affected == 0 {
                if !incomplete_ids.contains(&id) {
                    incomplete_ids.push(id);
                }
                continue;
            }
            let event = audit::succeeded_event(
                ctx,
                None,
                None,
                audit::entity("feishu_approval_config", id)?,
                None,
                Some(audit::summary([(
                    "display_names_refreshed",
                    serde_json::json!(!incomplete_ids.contains(&id)),
                )])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }
        Ok(())
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await?;
    incomplete_ids.sort_unstable();
    ApiResponse::success(
        serde_json::json!({ "incomplete_ids": incomplete_ids }),
        "名称补充结束；未取得的名称保留原值",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::{
        outbound::{OutboundRequest, OutboundResponse, OutboundTransport, Sleeper},
        tenant_token::{FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider},
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
    use yang_base::{
        action::{Request, TokenAuthMiddleware, User},
        definition::{ActionRef, AddonName, AddonSpec, AppBuilder, ModuleName},
        tools::ToolsBuilder,
    };

    struct MetadataTransport {
        namespace: String,
        partial: AtomicBool,
        requests: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl OutboundTransport for MetadataTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            self.requests
                .lock()
                .map_err(|error| BaseError::ConfigError(error.to_string()))?
                .push(request.url.clone());
            let path = request
                .url
                .strip_prefix("https://open.feishu.cn/open-apis/")
                .unwrap_or_default()
                .replace(&self.namespace, "");
            let partial = self.partial.load(Ordering::SeqCst);
            let prefix = if partial { "更新" } else { "初始" };
            let denied = partial
                && (path == "approval/v4/approvals/CODE-B"
                    || path == "bitable/v1/apps/appOther/tables");
            let body = if path == "auth/v3/tenant_access_token/internal" {
                json!({"code":0,"tenant_access_token":"fixture-token","expire":7200})
            } else if denied {
                json!({"code":99991672,"msg":"permission denied"})
            } else if path.starts_with("approval/v4/approvals/") {
                json!({"code":0,"data":{"approval_name":format!("{prefix}审批")}})
            } else if path.ends_with("/fields") {
                let backfill = if partial && (path.contains("tbl2/") || path.contains("tbl3/")) {
                    " ".to_string()
                } else {
                    format!("{prefix}编号")
                };
                json!({"code":0,"data":{"has_more":false,"total":2,"items":[{"field_id":"fldP","field_name":format!("{prefix}申请人")},{"field_id":"fldB","field_name":backfill}]}})
            } else if path.ends_with("/tables") {
                json!({"code":0,"data":{"has_more":false,"total":3,"items":[{"table_id":"tbl1","name":format!("{prefix}表1")},{"table_id":"tbl2","name":format!("{prefix}表2")},{"table_id":"tbl3","name":format!("{prefix}表3")}]}})
            } else if path.starts_with("bitable/v1/apps/") {
                json!({"code":0,"data":{"app":{"name":if partial && path.ends_with("appOther") { " ".to_string() } else { format!("{prefix}多维表") }}}})
            } else {
                return Err(BaseError::ConfigError(format!("未预期的出站请求：{path}")));
            };
            Ok(OutboundResponse {
                status: if denied { 403 } else { 200 },
                body: body.to_string(),
                bytes: None,
                headers: Default::default(),
            })
        }
    }

    struct NoSleep;
    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _: std::time::Duration) {}
    }

    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库及 Redis DB 15"]
    async fn real_mysql_refresh_names_saves_deduplicates_preserves_and_audits() -> anyhow::Result<()>
    {
        use yang_db::{Database, DatabaseConfig, RedisClient};
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "拒绝在非测试库运行");
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(redis_url.ends_with("/15"), "Redis 必须使用 DB 15");
        crate::schema::sync_with_database(
            database,
            DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let security = Arc::new(crate::config::SecuritySettings::default());
        let database = Database::connect(&url).await?;
        let redis = RedisClient::connect(&redis_url).await?;
        let context =
            crate::addon::feishu::build_context(Arc::new(database.pool().clone()), security, None)?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let mut ids = Vec::new();
        for (base, table, code) in [
            ("appShared", "tbl1", "CODE-A"),
            ("appShared", "tbl2", "CODE-A"),
            ("appOther", "tbl3", "CODE-B"),
        ] {
            let (_, id) = context
                .approval_configs()
                .query()
                .insert_returning_id(
                    Record::new()
                        .set("title", format!("名称刷新测试{suffix}"))
                        .set("base_token", format!("{base}{suffix}"))
                        .set("table_id", table)
                        .set("approval_code", code)
                        .set("applicant_field", "fldP")
                        .set("backfill_field", "fldB"),
                )
                .await?;
            ids.push(id as i64);
        }
        let outcome: anyhow::Result<()> = async {
            let transport = Arc::new(MetadataTransport { namespace: suffix.clone(), partial: AtomicBool::new(false), requests: Mutex::new(Vec::new()) });
            let tokens = Arc::new(TenantTokenProvider::new(
                Arc::new(RedisTenantTokenCache::new(redis.clone(), &suffix)), transport.clone(), Arc::new(NoSleep),
                FeishuCredentials { app_id: "cli_fixture".into(), app_secret: "fixture-secret".into() }, &suffix)?);
            let token_manager = yang_base::token::TokenManager::new_symmetric("refresh-names-fixture-secret-32-bytes", jsonwebtoken::Algorithm::HS256, "fixture".into(), "fixture".into(), 300, 3600)?;
            let access = token_manager.generate_access_token("7", json!({"version":1}))?;
            let tools = Arc::new(ToolsBuilder::new().mysql(Database::from_pool(database.pool().clone(), DatabaseConfig::default())?).cache(redis).token(token_manager).build()?);
            let module = ModuleSpec::new(ModuleName::new("feishu.approval")?)
                .middleware(TokenAuthMiddleware::new(|_| User::new(7, "fixture").with_permissions(["feishu.approval.write"])))
                .action_fn(yang_base::action_name!("refresh_names"), {
                    let context = context.clone();
                    let transport = transport.clone();
                    move |ctx, input: RefreshNamesInput| {
                        let context = context.clone();
                        let transport = transport.clone();
                        let tokens = tokens.clone();
                        async move {
                            input.validate()?;
                            refresh(&ctx, &input.config_ids, &context, transport.as_ref(), &NoSleep, &tokens).await
                        }
                    }
                }).permissions(["feishu.approval.write"]).register();
            let app = AppBuilder::new().addon(AddonSpec::new(AddonName::new("feishu")?).module(module)).build(tools)?;
            let action = app.registry().resolve(&ActionRef::new(ModuleName::new("feishu.approval")?, yang_base::action_name!("refresh_names"))).ok_or_else(|| anyhow::anyhow!("缺少测试 Action"))?;
            let snapshot = |row: &Record| -> anyhow::Result<Vec<String>> {
                ["base_name", "table_name", "approval_name", "applicant_field_name", "backfill_field_name"].iter().map(|key| row.require::<String>(key).map_err(Into::into)).collect()
            };
            let missing = i64::MAX;
            for partial in [false, true] {
                transport.partial.store(partial, Ordering::SeqCst);
                transport.requests.lock().map_err(|error| anyhow::anyhow!("{error}"))?.clear();
                let requested = if partial { vec![missing, ids[2], ids[1], ids[0], ids[1]] } else { ids.clone() };
                let response = app.dispatch(action, Request::new(json!({"config_ids":requested})).header("authorization", format!("Bearer {access}"))).await?;
                assert_eq!(response.code, 0, "{response:?}");
                let expected = if partial { vec![ids[1], ids[2], missing] } else { vec![] };
                assert_eq!(response.data.as_ref().and_then(|data| data.get("incomplete_ids")), Some(&json!(expected)), "请求：{:?}", transport.requests.lock().map_err(|error| anyhow::anyhow!("{error}"))?);
                for (index, id) in ids.iter().enumerate() {
                    let row = context.approval_configs().query().where_eq("id", json!(id))?.one().await?;
                    let mut expected = vec!["初始多维表".into(), format!("初始表{}", index + 1), "初始审批".into(), "初始申请人".into(), "初始编号".into()];
                    if partial {
                        expected[3] = "更新申请人".into();
                        if index < 2 {
                            expected[0] = "更新多维表".into(); expected[1] = format!("更新表{}", index + 1); expected[2] = "更新审批".into();
                        }
                        if index == 0 { expected[4] = "更新编号".into(); }
                    }
                    assert_eq!(snapshot(&row)?, expected, "配置{id}刷新及保旧");
                    let summaries: Vec<Value> = sqlx::query_scalar("SELECT after_summary FROM audit_event WHERE action = 'feishu.approval.refresh_names' AND target_id = ? AND actor_id = '7' AND result = 'succeeded' ORDER BY id")
                        .bind(id.to_string()).fetch_all(database.pool()).await?;
                    assert_eq!(summaries.len(), if partial { 2 } else { 1 });
                    let summary = summaries.last().ok_or_else(|| anyhow::anyhow!("缺少审计"))?;
                    assert_eq!(summary["display_names_refreshed"], !partial || index == 0);
                }
                let requests = transport.requests.lock().map_err(|error| anyhow::anyhow!("{error}"))?;
                for path in ["bitable/v1/apps/appShared", "bitable/v1/apps/appShared/tables", "approval/v4/approvals/CODE-A", "bitable/v1/apps/appOther", "bitable/v1/apps/appOther/tables", "approval/v4/approvals/CODE-B"] {
                    let path = path.replace("appShared", &format!("appShared{suffix}")).replace("appOther", &format!("appOther{suffix}"));
                    let url = format!("https://open.feishu.cn/open-apis/{path}");
                    assert_eq!(requests.iter().filter(|request| request.split('?').next() == Some(url.as_str())).count(), 1, "同一元数据只请求一次：{path}");
                }
            }
            Ok(())
        }.await;
        for id in ids {
            context
                .approval_configs()
                .query()
                .where_eq("id", json!(id))?
                .delete()
                .await?;
        }
        outcome
    }

    #[test]
    fn refresh_batch_is_bounded_and_positive() {
        for ids in [vec![], vec![0], vec![-1], vec![1; 11]] {
            assert!(RefreshNamesInput { config_ids: ids }.validate().is_err());
        }
        assert!(RefreshNamesInput {
            config_ids: (1..=10).collect()
        }
        .validate()
        .is_ok());
    }
}
