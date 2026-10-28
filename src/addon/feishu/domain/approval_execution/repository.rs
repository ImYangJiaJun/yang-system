//! 审批任务的跨进程执行权。租约只约束本地写者，不构成飞书侧 fencing。

//! raw-sql-boundary: domain-repository feishu-approval-execution
use super::super::context::FeishuContext;
use super::DispatchSnapshot;
use yang_base::table::Record;

const LEASE_SECONDS: i64 = 300;

/// 受理与身份快照同事务。已存在任务的第一份快照胜出，不能覆盖正在执行的申请人。
pub(crate) async fn admit(
    context: &FeishuContext,
    snapshot: &DispatchSnapshot,
    record_id: &str,
) -> anyhow::Result<i64> {
    let config: Record = snapshot.config.clone().into();
    let config_id: i64 = config.require("id")?;
    let base: String = config.require("base_token")?;
    let table: String = config.require("table_id")?;
    let approval: String = config.require("approval_code")?;
    let uuid = super::super::approval_uuid::derive_uuid(&base, &table, &approval, record_id);
    let payload = serde_json::to_string(snapshot)?;
    let pool = context.approval_tasks().pool();
    let mut transaction = pool.begin().await?;
    // tenant-boundary: raw-sql feishu-approval-admit-config
    let enabled = sqlx::query_scalar::<_, bool>(
        "SELECT enabled FROM feishu_approval_config WHERE id=? FOR UPDATE",
    )
    .bind(config_id)
    .fetch_optional(&mut *transaction)
    .await?;
    anyhow::ensure!(enabled == Some(true), "审批配置已删除或停用，未受理");
    // tenant-boundary: raw-sql feishu-approval-admit-task
    sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,dispatch_payload,created_at,updated_at) VALUES (?,?,?,'pending',?,UNIX_TIMESTAMP(),UNIX_TIMESTAMP()) ON DUPLICATE KEY UPDATE id=LAST_INSERT_ID(id)")
        .bind(config_id).bind(record_id).bind(&uuid).bind(&payload).execute(&mut *transaction).await?;
    // tenant-boundary: raw-sql feishu-approval-admit-owner
    let (id, stored_config, stored_uuid): (i64, i64, String) = sqlx::query_as(
        "SELECT id,config_id,uuid FROM feishu_approval_task WHERE record_id=? FOR UPDATE",
    )
    .bind(record_id)
    .fetch_one(&mut *transaction)
    .await?;
    anyhow::ensure!(
        stored_config == config_id && stored_uuid == uuid,
        "该记录归属另一审批配置，拒绝复用任务"
    );
    // 快照只在缺席时补写（`COALESCE` 保证第一份胜出）。不再要求「未认领且
    // ready」：review/终态的任务被重新点击时也要带回受理快照（申请人/接收人），
    // 已有快照绝不会被覆盖。
    // tenant-boundary: raw-sql feishu-approval-admit-snapshot
    sqlx::query(
        "UPDATE feishu_approval_task SET dispatch_payload=COALESCE(dispatch_payload,?) WHERE id=?",
    )
    .bind(&payload)
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    // 后来的请求不能为首次未提供身份的任务补入另一个申请人。
    let stored_payload: Option<String> =
        // tenant-boundary: raw-sql feishu-approval-admit-identity-snapshot
        sqlx::query_scalar("SELECT dispatch_payload FROM feishu_approval_task WHERE id=?")
            .bind(id)
            .fetch_one(&mut *transaction)
            .await?;
    let persisted = stored_payload
        .as_deref()
        .map(serde_json::from_str::<DispatchSnapshot>)
        .transpose()?;
    if let Some(applicant) = persisted
        .as_ref()
        .and_then(|snapshot| snapshot.applicant.as_deref())
    {
        // tenant-boundary: raw-sql feishu-approval-admit-applicant
        sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?) ON DUPLICATE KEY UPDATE id=id")
            .bind(id).bind(applicant).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(id)
}

/// 把「重新点击」变成一次人工重试：任务不在执行中、又停在可重试状态时，
/// 重置回 `pending` 并让调用方立刻重新认领。
///
/// # 为什么安全
///
/// 任务行与 `uuid` 都不动——`uuid` 由 `base/table/approval_code/record_id`
/// 确定性派生，是飞书创建接口的幂等键：若上次其实建出了实例，重新 create
/// 会拿到 `60012` 并走按 uuid 回捞，不会产生第二条审批。
///
/// # 收窄条件
///
/// - 只有 `pending / review / terminal / unnotified` 可重开；`backfilled`
///   编号已落表，重开没有意义。
/// - 有执行者持有有效租约时不动（条件 UPDATE 兜底），避免把正在写入的任务
///   抢回来并发执行。
/// - 顺带清退避（`available_at`）与重试计数：点击的语义就是「现在再来一次」。
/// - **保留 `last_error`**：它是通知去重的判据（`should_notify` 比较「本轮原因 ==
///   上一轮原因」）。重开时清空会让同一结论每点一次都重发一条通知——2026-10-28
///   真机：批量点击反复收到另一张表的旧 waiting 报错。
pub(crate) async fn reopen(context: &FeishuContext, task_id: i64) -> anyhow::Result<bool> {
    // tenant-boundary: raw-sql feishu-approval-reopen
    let result = sqlx::query(
        "UPDATE feishu_approval_task SET state='pending', effect_phase=IF(instance_code IS NULL,'ready','created'), worker_id=NULL, lease_until=NULL, attempts=0, available_at=NULL, updated_at=UNIX_TIMESTAMP() WHERE id=? AND state IN ('pending','review','terminal','unnotified') AND (worker_id IS NULL OR lease_until<=UNIX_TIMESTAMP())",
    )
    .bind(task_id)
    .execute(context.approval_tasks().pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(crate) async fn facts(
    context: &FeishuContext,
    claim: &TaskClaim,
) -> anyhow::Result<(String, Option<String>)> {
    // tenant-boundary: raw-sql feishu-approval-effect-facts
    let row = sqlx::query_as("SELECT effect_phase,instance_code FROM feishu_approval_task WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation).fetch_optional(context.approval_tasks().pool()).await?;
    row.ok_or_else(|| anyhow::anyhow!("审批任务执行权已失效"))
}

pub(crate) async fn begin_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    phase: &str,
) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-begin
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,lease_until=UNIX_TIMESTAMP()+?,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND effect_phase IN ('ready','created')")
        .bind(phase).bind(LEASE_SECONDS).bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "外部写入未开始：执行权失效或上次写入结果未知"
    );
    Ok(())
}

pub(crate) async fn confirm_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    phase: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-confirm
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,instance_code=COALESCE(?,instance_code),updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(phase).bind(instance).bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "远端结果未能持久确认，禁止重放写入"
    );
    Ok(())
}

pub(crate) async fn mark_unknown(context: &FeishuContext, claim: &TaskClaim) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-unknown
    sqlx::query("UPDATE feishu_approval_task SET state='review',last_error='外部写入结果未知，禁止自动重放；请人工核查',updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=?")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct TaskClaim {
    pub(crate) id: i64,
    pub(crate) owner: String,
    pub(crate) generation: i64,
    pub(crate) attempts: i64,
}

/// 原子认领可安全执行的任务；只读阶段租约过期允许接管，未知副作用禁止重放。
pub(crate) async fn claim_ready(
    context: &FeishuContext,
    after: i64,
    only_id: Option<i64>,
    limit: usize,
) -> anyhow::Result<Vec<TaskClaim>> {
    let pool = context.approval_tasks().pool();
    // tenant-boundary: raw-sql feishu-approval-execution-1
    sqlx::query("UPDATE feishu_approval_task SET state='review',last_error='外部写入结果未知，禁止自动重放；请人工核查',updated_at=UNIX_TIMESTAMP() WHERE worker_id IS NOT NULL AND lease_until<=UNIX_TIMESTAMP() AND effect_phase NOT IN ('ready','created') AND state<>'review'")
        .execute(pool).await?;
    let mut transaction = pool.begin().await?;
    // tenant-boundary: raw-sql feishu-approval-execution-2
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as("SELECT id,generation,attempts FROM feishu_approval_task WHERE id>? AND (? IS NULL OR id=?) AND state='pending' AND effect_phase IN ('ready','created') AND (available_at IS NULL OR available_at<=UNIX_TIMESTAMP()) AND (worker_id IS NULL OR lease_until<=UNIX_TIMESTAMP()) ORDER BY id LIMIT ? FOR UPDATE SKIP LOCKED")
        .bind(after).bind(only_id).bind(only_id).bind(limit as u32)
        .fetch_all(&mut *transaction).await?;
    let mut claims = Vec::with_capacity(rows.len());
    for (id, generation, attempts) in rows {
        let owner = uuid::Uuid::new_v4().to_string();
        // tenant-boundary: raw-sql feishu-approval-execution-3
        sqlx::query("UPDATE feishu_approval_task SET worker_id=?,generation=generation+1,lease_until=UNIX_TIMESTAMP()+?,updated_at=UNIX_TIMESTAMP() WHERE id=?")
            .bind(&owner).bind(LEASE_SECONDS).bind(id).execute(&mut *transaction).await?;
        claims.push(TaskClaim {
            id,
            owner,
            generation: generation + 1,
            attempts,
        });
    }
    transaction.commit().await?;
    Ok(claims)
}

/// 副作用开始前与提交前都要检查；失去租约的旧调用只能丢弃本地结果。
pub(crate) async fn is_owned(context: &FeishuContext, claim: &TaskClaim) -> anyhow::Result<bool> {
    // tenant-boundary: raw-sql feishu-approval-execution-4
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation)
        .fetch_one(context.approval_tasks().pool()).await?;
    Ok(count == 1)
}

/// 任务结论只由本代持有者提交；受影响行数是执行权是否仍成立的事实。
pub(crate) async fn finish(
    context: &FeishuContext,
    claim: &TaskClaim,
    row: &Record,
    retry: bool,
) -> anyhow::Result<bool> {
    let state: String = row.require("state")?;
    let reason: Option<String> = row.optional("last_error")?;
    let instance: Option<String> = row.optional("instance_code")?;
    let serial: Option<String> = row.optional("serial_number")?;
    let attempts = if retry {
        claim.attempts + 1
    } else {
        claim.attempts
    };
    let state = if retry && attempts >= 10 {
        "review"
    } else {
        &state
    };
    let delay = if retry {
        (1_i64 << attempts.min(8)).min(300)
    } else {
        30
    };
    // tenant-boundary: raw-sql feishu-approval-execution-5
    let result = sqlx::query("UPDATE feishu_approval_task SET state=?,last_error=?,instance_code=COALESCE(?,instance_code),serial_number=COALESCE(?,serial_number),attempts=?,available_at=UNIX_TIMESTAMP()+?,worker_id=NULL,lease_until=NULL,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND effect_phase IN ('ready','created')")
        .bind(state).bind(reason).bind(instance).bind(serial).bind(attempts).bind(delay)
        .bind(claim.id).bind(&claim.owner).bind(claim.generation)
        .execute(context.approval_tasks().pool()).await?;
    Ok(result.rows_affected() == 1)
}
