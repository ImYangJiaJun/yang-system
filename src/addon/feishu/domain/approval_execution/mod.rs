//! 审批持久执行边界。

mod repository;
mod transport;
pub(crate) use repository::{
    admit, begin_effect, claim_ready, confirm_effect, facts, finish, is_owned, mark_unknown,
    reopen, TaskClaim,
};
pub(crate) use transport::ExecutionTransport;

use super::approval_notification::NotificationRecipient;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use yang_base::table::Record;

/// 只保存业务配置白名单，不复制访问 Token、摘要或密文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DispatchSnapshot {
    pub(crate) config: Map<String, Value>,
    pub(crate) maps: Vec<Map<String, Value>>,
    pub(crate) applicant: Option<String>,
    pub(crate) notification_recipient: Option<NotificationRecipient>,
}

impl DispatchSnapshot {
    pub(crate) fn new(
        config: &Record,
        maps: Vec<Map<String, Value>>,
        applicant: Option<String>,
        notification_recipient: Option<NotificationRecipient>,
    ) -> Self {
        let config = [
            "id",
            "base_token",
            "table_id",
            "approval_code",
            "applicant_field",
            "backfill_field",
            "status_field",
            "flow_field",
            "start_time_field",
            "end_time_field",
            "department_field",
            "handler_field",
            "base_timezone",
            "form_snapshot",
            "title",
            "base_name",
            "table_name",
            "approval_name",
            "enabled",
        ]
        .into_iter()
        .filter_map(|name| {
            config
                .get(name)
                .map(|value| (name.to_string(), value.clone()))
        })
        .collect();
        Self {
            config,
            maps,
            applicant,
            notification_recipient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::{context::FeishuContext, outbound::*};
    use anyhow::Context;
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use yang_base::{table::Record, BaseError};

    async fn context() -> anyhow::Result<Arc<FeishuContext>> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许独立测试库");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        reconnect().await
    }

    async fn reconnect() -> anyhow::Result<Arc<FeishuContext>> {
        let database =
            yang_db::Database::connect(&std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?).await?;
        Ok(crate::addon::feishu::build_context(
            Arc::new(database.pool().clone()),
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?)
    }

    async fn snapshot(context: &FeishuContext) -> anyhow::Result<DispatchSnapshot> {
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let id = sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('持久执行测试','appExecution',?,'DEF-EXECUTION','fldPerson','fldNumber','Asia/Shanghai',true,UNIX_TIMESTAMP(),UNIX_TIMESTAMP())")
            .bind(format!("tbl{unique}")).execute(context.approval_configs().pool()).await?.last_insert_id() as i64;
        let config = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(id))?
            .optional()
            .await?
            .ok_or_else(|| anyhow::anyhow!("配置未保存"))?;
        Ok(DispatchSnapshot::new(
            &config,
            Vec::new(),
            Some("ou_first".into()),
            None,
        ))
    }

    async fn claimed(
        context: &FeishuContext,
        snapshot: &DispatchSnapshot,
    ) -> anyhow::Result<TaskClaim> {
        let record = format!("rec{}", uuid::Uuid::new_v4().simple());
        let id = admit(context, snapshot, &record).await?;
        claim_ready(context, 0, Some(id), 1)
            .await?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("没有取得任务执行权"))
    }

    fn request() -> OutboundRequest {
        OutboundRequest {
            method: OutboundMethod::Post,
            url: "https://example.invalid/open-apis/approval/v4/instances".into(),
            query: Vec::new(),
            bearer_token: None,
            json_body: Some(serde_json::json!({})),
            raw_body: None,
            binary_limit: None,
            timeout_secs: None,
            idempotent: false,
        }
    }

    struct ControlledTransport {
        calls: AtomicUsize,
        response: Option<(u16, String)>,
        close_pool: Option<sqlx::MySqlPool>,
    }

    impl ControlledTransport {
        fn reply(status: u16, body: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                response: Some((status, body.into())),
                close_pool: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ControlledTransport {
        async fn send(&self, _: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(pool) = &self.close_pool {
                pool.close().await;
            }
            match &self.response {
                Some((status, body)) => Ok(OutboundResponse {
                    status: *status,
                    body: body.clone(),
                    bytes: None,
                    headers: BTreeMap::new(),
                }),
                None => Err(BaseError::ConfigError("模拟远端已收包但响应超时".into())),
            }
        }
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_execution_admission_preserves_first_snapshot_and_identity(
    ) -> anyhow::Result<()> {
        let context = context().await?;
        let mut first = snapshot(&context).await?;
        let record = format!("rec{}", uuid::Uuid::new_v4().simple());
        let mut config: Record = first.config.clone().into();
        for key in ["token", "token_hash", "token_cipher", "access_token"] {
            config.insert(key, serde_json::json!("secret-must-not-persist"));
        }
        first = DispatchSnapshot::new(&config, Vec::new(), Some("ou_first".into()), None);
        let id = admit(&context, &first, &record).await?;
        let mut later = first.clone();
        later.applicant = Some("ou_later".into());
        later
            .config
            .insert("title".into(), serde_json::json!("修改后的配置"));
        assert_eq!(admit(&context, &later, &record).await?, id);
        let (payload, applicant): (String, String) = sqlx::query_as("SELECT t.dispatch_payload,a.open_id FROM feishu_approval_task t JOIN feishu_approval_task_applicant a ON a.task_id=t.id WHERE t.id=?")
            .bind(id).fetch_one(context.approval_tasks().pool()).await?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&payload)?,
            serde_json::to_value(&first)?
        );
        assert_eq!(applicant, "ou_first");
        assert!(!payload.contains("secret-must-not-persist"));
        let mut without_applicant = first.clone();
        without_applicant.applicant = None;
        let no_identity_record = format!("rec{}", uuid::Uuid::new_v4().simple());
        let no_identity_id = admit(&context, &without_applicant, &no_identity_record).await?;
        admit(&context, &later, &no_identity_record).await?;
        let attached: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feishu_approval_task_applicant WHERE task_id=?",
        )
        .bind(no_identity_id)
        .fetch_one(context.approval_tasks().pool())
        .await?;
        assert_eq!(attached, 0, "首次未指定身份也不能由后续请求改写");
        // record_id 只在多维表格内唯一（复制 Base 会原样保留它），同一 record 在
        // 另一个配置下必须能独立建任务：产生第二条任务行，归属各自的配置。
        let other = snapshot(&context).await?;
        let other_id = admit(&context, &other, &record).await?;
        assert_ne!(other_id, id, "跨配置应产生第二条任务行");
        let configs: Vec<i64> = sqlx::query_scalar(
            "SELECT config_id FROM feishu_approval_task WHERE record_id=? ORDER BY id",
        )
        .bind(&record)
        .fetch_all(context.approval_tasks().pool())
        .await?;
        assert_eq!(
            configs,
            vec![
                first.config["id"].as_i64().context("配置 id 应存在")?,
                other.config["id"].as_i64().context("配置 id 应存在")?,
            ],
            "同一 record_id 在两个配置下各有一条任务行"
        );
        sqlx::query("UPDATE feishu_approval_config SET enabled=false WHERE id=?")
            .bind(first.config["id"].as_i64())
            .execute(context.approval_configs().pool())
            .await?;
        let disabled_record = format!("rec{}", uuid::Uuid::new_v4().simple());
        assert!(admit(&context, &first, &disabled_record).await.is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_reopen_restarts_retryable_and_backfilled_tasks_but_never_running_ones(
    ) -> anyhow::Result<()> {
        let context = context().await?;
        let snapshot = snapshot(&context).await?;

        // review / terminal / unnotified / 退避中的 pending 都能被「重新点击」拉起。
        for state in ["review", "terminal", "unnotified", "pending"] {
            let claim = claimed(&context, &snapshot).await?;
            sqlx::query(
                "UPDATE feishu_approval_task SET state=?, effect_phase='creating', worker_id=NULL, lease_until=NULL, attempts=4, available_at=UNIX_TIMESTAMP()+600, last_error='x' WHERE id=?",
            )
            .bind(state)
            .bind(claim.id)
            .execute(context.approval_tasks().pool())
            .await?;
            assert!(reopen(&context, claim.id).await?, "{state} 应可重开");
            let (new_state, phase, attempts, last_error): (String, String, i64, Option<String>) =
                sqlx::query_as(
                    "SELECT state,effect_phase,attempts,last_error FROM feishu_approval_task WHERE id=?",
                )
                .bind(claim.id)
                .fetch_one(context.approval_tasks().pool())
                .await?;
            assert_eq!(
                (new_state.as_str(), phase.as_str(), attempts),
                ("pending", "ready", 0)
            );
            assert_eq!(
                last_error.as_deref(),
                Some("x"),
                "reopen 必须保留 last_error，通知去重靠它"
            );
            // 退避被清掉后立刻可认领（= 点击当场重新发起）。
            assert!(
                !claim_ready(&context, 0, Some(claim.id), 1)
                    .await?
                    .is_empty(),
                "{state} 重开后应能立刻认领"
            );
        }

        // 快照缺席的 review 行（seed 建出来的那种）重新受理时要补写快照，
        // 否则点击路径会因「快照不一致」把任务退回 pending 而不是当场重跑。
        let record = format!("rec{}", uuid::Uuid::new_v4().simple());
        let id = admit(&context, &snapshot, &record).await?;
        sqlx::query("UPDATE feishu_approval_task SET state='review', effect_phase='creating', dispatch_payload=NULL, worker_id=NULL, lease_until=NULL WHERE id=?")
            .bind(id)
            .execute(context.approval_tasks().pool())
            .await?;
        assert_eq!(admit(&context, &snapshot, &record).await?, id);
        let payload: Option<String> =
            sqlx::query_scalar("SELECT dispatch_payload FROM feishu_approval_task WHERE id=?")
                .bind(id)
                .fetch_one(context.approval_tasks().pool())
                .await?;
        assert!(payload.is_some(), "review 行重新受理时应补写缺席的快照");

        // 编号已回填的任务也能重开：用户清空表格里的编号后再点击，就是显式重发。
        // 实例已存在时 `effect_phase` 必须是 `created`（走实例恢复 + 重新回填，
        // 不新建审批），且重开后立即可认领。
        let claim = claimed(&context, &snapshot).await?;
        sqlx::query(
            "UPDATE feishu_approval_task SET state='backfilled', instance_code='INSTANCE-KEPT', worker_id=NULL, lease_until=NULL WHERE id=?",
        )
        .bind(claim.id)
        .execute(context.approval_tasks().pool())
        .await?;
        assert!(
            reopen(&context, claim.id).await?,
            "backfilled 在人工点击且回填列为空时应可重开"
        );
        let (state, phase): (String, String) =
            sqlx::query_as("SELECT state,effect_phase FROM feishu_approval_task WHERE id=?")
                .bind(claim.id)
                .fetch_one(context.approval_tasks().pool())
                .await?;
        assert_eq!(
            (state.as_str(), phase.as_str()),
            ("pending", "created"),
            "实例已存在时重开必须走 recovery 相位"
        );
        assert!(
            !claim_ready(&context, 0, Some(claim.id), 1)
                .await?
                .is_empty(),
            "backfilled 重开后应能立刻认领"
        );

        // 租约有效 = 有 worker 正在执行，不许抢（backfilled 也一样）。
        let claim = claimed(&context, &snapshot).await?;
        sqlx::query("UPDATE feishu_approval_task SET state='backfilled' WHERE id=?")
            .bind(claim.id)
            .execute(context.approval_tasks().pool())
            .await?;
        assert!(
            !reopen(&context, claim.id).await?,
            "backfilled 且执行中的任务不应被重开"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_execution_transport_confirms_success_and_quarantines_unknown(
    ) -> anyhow::Result<()> {
        let context = context().await?;
        let snapshot = snapshot(&context).await?;
        let claim = claimed(&context, &snapshot).await?;
        let inner = ControlledTransport::reply(
            200,
            r#"{"code":0,"data":{"instance_code":"INSTANCE-KNOWN"}}"#,
        );
        ExecutionTransport {
            inner: &inner,
            context: &context,
            claim: &claim,
        }
        .send(request())
        .await?;
        assert_eq!(
            facts(&context, &claim).await?,
            ("created".into(), Some("INSTANCE-KNOWN".into()))
        );

        for response in [
            None,
            Some((200, "not-json")),
            Some((500, r#"{"code":0,"data":{"instance_code":"UNCONFIRMED"}}"#)),
            Some((200, r#"{"code":0,"data":{}}"#)),
        ] {
            let claim = claimed(&context, &snapshot).await?;
            let inner = ControlledTransport {
                calls: AtomicUsize::new(0),
                response: response.map(|(status, body)| (status, body.into())),
                close_pool: None,
            };
            let transport = ExecutionTransport {
                inner: &inner,
                context: &context,
                claim: &claim,
            };
            assert!(transport.send(request()).await.is_err());
            assert!(transport.send(request()).await.is_err());
            assert_eq!(
                inner.calls.load(Ordering::SeqCst),
                1,
                "未知写入不得自动重发"
            );
            let (state, phase): (String, String) =
                sqlx::query_as("SELECT state,effect_phase FROM feishu_approval_task WHERE id=?")
                    .bind(claim.id)
                    .fetch_one(context.approval_tasks().pool())
                    .await?;
            assert_eq!((state.as_str(), phase.as_str()), ("review", "creating"));
        }
        // 4xx + 非零业务码是远端**明确拒绝**（校验挡下、写入未发生）：必须原样
        // 上抛让上层拿到真实错误码，且把阶段恢复到安全值——转 review 会把
        // 「值不合法」伪装成「结果未知」，任务永久卡住（2026-10-26 真机事故）。
        let claim = claimed(&context, &snapshot).await?;
        let inner = ControlledTransport::reply(400, r#"{"code":1390001,"msg":"InvalidArgument"}"#);
        let response = ExecutionTransport {
            inner: &inner,
            context: &context,
            claim: &claim,
        }
        .send(request())
        .await
        .unwrap_or_else(|e| panic!("明确拒绝应按业务错误上抛: {e}"));
        assert_eq!(response.status, 400);
        assert_eq!(facts(&context, &claim).await?, ("ready".into(), None));

        // 同一语义的另一种载体：飞书大量业务错误走 **HTTP 200 + 非零业务码**
        // （见 outbound 模块文档）。只认 4xx 会把它们静默推进 review——任务
        // 永久卡住、真实错误码丢成 900001（2026-10-28 真机：同一配置 5 条记录
        // 转 review，飞书侧零实例）。
        let claim = claimed(&context, &snapshot).await?;
        let inner =
            ControlledTransport::reply(200, r#"{"code":1395006,"msg":"invalid widget value"}"#);
        let response = ExecutionTransport {
            inner: &inner,
            context: &context,
            claim: &claim,
        }
        .send(request())
        .await
        .unwrap_or_else(|e| panic!("200+业务码的明确拒绝应按业务错误上抛: {e}"));
        assert_eq!(response.status, 200);
        assert_eq!(facts(&context, &claim).await?, ("ready".into(), None));
        for code in [
            i64::from(CODE_RATE_LIMITED),
            i64::from(CODES_TOKEN_EXPIRED[0]),
        ] {
            let claim = claimed(&context, &snapshot).await?;
            let inner =
                ControlledTransport::reply(200, &serde_json::json!({"code":code}).to_string());
            ExecutionTransport {
                inner: &inner,
                context: &context,
                claim: &claim,
            }
            .send(request())
            .await?;
            assert_eq!(facts(&context, &claim).await?, ("ready".into(), None));
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_execution_remote_success_local_failure_never_recreates(
    ) -> anyhow::Result<()> {
        let context = context().await?;
        let snapshot = snapshot(&context).await?;
        let claim = claimed(&context, &snapshot).await?;
        let mut inner = ControlledTransport::reply(
            200,
            r#"{"code":0,"data":{"instance_code":"REMOTE-CREATED"}}"#,
        );
        inner.close_pool = Some(context.approval_tasks().pool().clone());
        assert!(ExecutionTransport {
            inner: &inner,
            context: &context,
            claim: &claim
        }
        .send(request())
        .await
        .is_err());
        let recovered = reconnect().await?;
        sqlx::query("UPDATE feishu_approval_task SET lease_until=UNIX_TIMESTAMP()-1 WHERE id=?")
            .bind(claim.id)
            .execute(recovered.approval_tasks().pool())
            .await?;
        assert!(claim_ready(&recovered, 0, Some(claim.id), 1)
            .await?
            .is_empty());
        let state: String = sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE id=?")
            .bind(claim.id)
            .fetch_one(recovered.approval_tasks().pool())
            .await?;
        assert_eq!(state, "review");
        assert!(ExecutionTransport {
            inner: &inner,
            context: &recovered,
            claim: &claim
        }
        .send(request())
        .await
        .is_err());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_execution_rejects_lost_owner_generation_lease_and_phase(
    ) -> anyhow::Result<()> {
        let context = context().await?;
        let snapshot = snapshot(&context).await?;
        for change in [
            "worker_id='other-owner'",
            "generation=generation+1",
            "lease_until=UNIX_TIMESTAMP()-1",
            "effect_phase='creating'",
            "state='review'",
        ] {
            let claim = claimed(&context, &snapshot).await?;
            sqlx::query(&format!(
                "UPDATE feishu_approval_task SET {change} WHERE id=?"
            ))
            .bind(claim.id)
            .execute(context.approval_tasks().pool())
            .await?;
            let inner = ControlledTransport::reply(
                200,
                r#"{"code":0,"data":{"instance_code":"MUST-NOT-CREATE"}}"#,
            );
            assert!(
                ExecutionTransport {
                    inner: &inner,
                    context: &context,
                    claim: &claim
                }
                .send(request())
                .await
                .is_err(),
                "{change}"
            );
            assert_eq!(inner.calls.load(Ordering::SeqCst), 0, "{change}");
            let mut row = Record::new();
            row.insert("state", serde_json::json!("backfilled"));
            assert!(!finish(&context, &claim, &row, false).await?, "{change}");
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_execution_waiting_and_retry_budget_are_durable() -> anyhow::Result<()> {
        let context = context().await?;
        let snapshot = snapshot(&context).await?;
        let mut claim = claimed(&context, &snapshot).await?;
        let mut row = Record::new();
        row.insert("state", serde_json::json!("pending"));
        assert!(finish(&context, &claim, &row, false).await?);
        let (attempts, delay): (i64, i64) = sqlx::query_as(
            "SELECT attempts,available_at-UNIX_TIMESTAMP() FROM feishu_approval_task WHERE id=?",
        )
        .bind(claim.id)
        .fetch_one(context.approval_tasks().pool())
        .await?;
        assert_eq!(attempts, 0);
        assert!((29..=30).contains(&delay));
        assert!(claim_ready(&context, 0, Some(claim.id), 1)
            .await?
            .is_empty());
        for expected in 1..=10 {
            sqlx::query(
                "UPDATE feishu_approval_task SET available_at=UNIX_TIMESTAMP()-1 WHERE id=?",
            )
            .bind(claim.id)
            .execute(context.approval_tasks().pool())
            .await?;
            claim = claim_ready(&context, 0, Some(claim.id), 1)
                .await?
                .pop()
                .ok_or_else(|| anyhow::anyhow!("重试任务未被认领"))?;
            assert!(finish(&context, &claim, &row, true).await?);
            let (state, attempts, delay): (String, i64, i64) = sqlx::query_as("SELECT state,attempts,available_at-UNIX_TIMESTAMP() FROM feishu_approval_task WHERE id=?").bind(claim.id).fetch_one(context.approval_tasks().pool()).await?;
            assert_eq!(attempts, expected);
            assert!((1..=300).contains(&delay));
            assert_eq!(state, if expected == 10 { "review" } else { "pending" });
        }
        assert!(claim_ready(&context, 0, Some(claim.id), 1)
            .await?
            .is_empty());
        Ok(())
    }
}
