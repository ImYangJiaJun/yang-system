//! 出站拉取的一轮编排：选源 → 拉取 → 派生 → 摘要比对 → 事务内落库与补集停用。
//!
//! 本模块**不依赖 `ActionContext`**（后台 worker 没有可用 ctx，理由见
//! [`super::option_write`]），因此读走 `Repository`、事务走 `Database::transaction()`。
//!
//! # 一轮的成败判据
//!
//! **成功不是 `fetched != 0`**，而是「分页收敛」——`page_token` 耗尽且累计行数与
//! 首屏 `total` 相符（由 [`super::bitable::list_all_records`] 保证）。读成功且确实为空的
//! 表其实应当照常停用补集，否则选项集永久冻结、控制台却显示「刚刚同步」。
//!
//! 但「读成功且为空」有一个官方明示的歧义：多维表格开启高级权限而调用身份不在
//! 授权群内时，**可能出现调用成功但返回数据为空**。此时收敛断言会通过，若照常执行
//! 补集停用，会把该数据源 100% 已启用的选项静默停掉。所以顺序是：
//! 拿到的行数为 0 且库里仍有已启用行 → 判为**可疑空快照**，只记录、不停用。

use std::collections::HashSet;

use serde::Deserialize;
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Database;

use super::bitable::{
    cell_label, list_all_records, BitableCoordinates, CellValue, RecordsSnapshot,
};
use super::context::FeishuContext;
use super::derive::{derive_options, snapshot_digest, DerivedOption, RawValue};
use super::option_write::{
    apply_option_rows, count_option_rows, disable_option_rows, find_foreign_option_owner,
    OptionWriteItem,
};
use super::outbound::{OutboundFailure, OutboundTransport, Sleeper};
use super::tenant_token::TenantTokenProvider;
use crate::infrastructure::audit;

/// 单个数据源一轮拉取的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SourcePullReport {
    /// 拉到的记录行数。
    pub(crate) fetched: usize,
    /// 派生出的选项数。
    pub(crate) derived: usize,
    /// 本轮新增。
    pub(crate) inserted: u64,
    /// 本轮更新。
    pub(crate) updated: u64,
    /// 本轮被补集停用。
    pub(crate) disabled: u64,
    /// 内容摘要未变而跳过了写库。
    pub(crate) skipped: bool,
    /// 判为可疑空快照，已拒绝停用补集。
    pub(crate) suspicious_empty: bool,
}

/// 一轮的汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoundReport {
    pub(crate) sources: usize,
    pub(crate) failures: usize,
    pub(crate) reports: Vec<(String, SourcePullReport)>,
}

/// 拉取所需的共享依赖。借用而不是持有：worker 每轮现取，避免把资源生命周期拉长。
pub(crate) struct PullDeps<'a> {
    pub(crate) context: &'a FeishuContext,
    pub(crate) database: &'a Database,
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    /// 单轮最多翻页数（透传给分页状态机）。
    pub(crate) max_pages: u32,
}

/// 一个待拉取的数据源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullSource {
    pub(crate) source_key: String,
    pub(crate) title: String,
    pub(crate) coordinates: BitableCoordinates,
    /// 取数列的**精确字段名**（接口要名字，不要 field_id）。
    pub(crate) field_name: String,
    /// 子数据源声明的父信息；`None` 表示无级联。
    pub(crate) linkage: Option<Linkage>,
    /// 上一轮落库的内容摘要。
    pub(crate) snapshot_digest: Option<String>,
}

/// 子数据源声明的级联关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Linkage {
    /// 父数据源的 `source_key`；父键列里存的是父的 `option_id`，没有它拼不出来。
    pub(crate) parent_source_key: String,
    /// **本表**里承载父文案的列名（父键由同行共现读出）。
    pub(crate) parent_field: String,
    /// **本表**里承载子文案的列名；应当与数据源的取数列一致。
    pub(crate) cascade_field: String,
}

/// `linkage_mapping` 的一条（键是飞书表单里联动控件的字段代码）。
#[derive(Debug, Deserialize)]
struct LinkageEntry {
    parent_source_key: String,
    parent_field: String,
    cascade_field: String,
}

/// 选出本轮要拉的数据源。
///
/// 只取 `status == active` 且 `ingest_mode == pull` 的行；坐标与取数列名缺一不可
/// （缺了就不是一个可拉取的数据源，静默跳过会让「配了却不生效」无从排查，故在
/// 调用方按条记 warning）。
pub(crate) async fn load_pull_sources(
    context: &FeishuContext,
) -> Result<Vec<PullSource>, BaseError> {
    let rows = context
        .datasources()
        .query()
        .select_fields(&[
            "source_key",
            "title",
            "status",
            "ingest_mode",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
            "bitable_field_name",
            "linkage_mapping",
            "snapshot_digest",
        ])?
        .where_eq("ingest_mode", serde_json::json!("pull"))?
        .where_eq("status", serde_json::json!("active"))?
        .all()
        .await?;

    let mut sources = Vec::new();
    for row in rows {
        let source_key: String = row.require("source_key")?;
        let Some(base_token) = row.optional::<String>("bitable_base_token")? else {
            tracing::warn!(source_key = %source_key, "数据源缺少 bitable_base_token，本轮跳过");
            continue;
        };
        let Some(table_id) = row.optional::<String>("bitable_table_id")? else {
            tracing::warn!(source_key = %source_key, "数据源缺少 bitable_table_id，本轮跳过");
            continue;
        };
        let Some(field_name) = row.optional::<String>("bitable_field_name")? else {
            tracing::warn!(source_key = %source_key, "数据源缺少 bitable_field_name，本轮跳过");
            continue;
        };

        // linkage_mapping 是 JSON 文本；解析失败**不当致命**，按无级联处理并告警——
        // 让整条链路因为一段坏 JSON 全停，比少拉一个级联更糟。
        let linkage = parse_linkage(row.optional::<String>("linkage_mapping")?)?;

        sources.push(PullSource {
            source_key,
            title: row.optional::<String>("title")?.unwrap_or_default(),
            coordinates: BitableCoordinates {
                app_token: base_token.trim().to_string(),
                table_id: table_id.trim().to_string(),
                view_id: row
                    .optional::<String>("bitable_view_id")?
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
            },
            field_name: field_name.trim().to_string(),
            linkage,
            snapshot_digest: row.optional::<String>("snapshot_digest")?,
        });
    }
    Ok(sources)
}

/// 解析 `linkage_mapping`。取第一条能解析出全部三个成员的表项。
fn parse_linkage(raw: Option<String>) -> Result<Option<Linkage>, BaseError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let map: std::collections::BTreeMap<String, LinkageEntry> = match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(error) => {
            tracing::warn!(error = %error, "linkage_mapping 不是合法 JSON，本轮按无级联处理");
            return Ok(None);
        }
    };
    let Some(entry) = map.into_values().next() else {
        return Ok(None);
    };
    if entry.parent_source_key.trim().is_empty()
        || entry.parent_field.trim().is_empty()
        || entry.cascade_field.trim().is_empty()
    {
        // 三者缺一就拼不出父键：`parent_source_key` 缺了不知道父在哪，
        // `parent_field` 缺了读不出共现，`cascade_field` 缺了不知道哪列是子值。
        tracing::warn!("linkage_mapping 三个成员缺一，本轮按无级联处理");
        return Ok(None);
    }
    Ok(Some(Linkage {
        parent_source_key: entry.parent_source_key.trim().to_string(),
        parent_field: entry.parent_field.trim().to_string(),
        cascade_field: entry.cascade_field.trim().to_string(),
    }))
}

/// 一轮拉取：读 → 派生 → 比对 → 落库。
pub(crate) async fn pull_source(
    deps: &PullDeps<'_>,
    source: &PullSource,
) -> Result<SourcePullReport, OutboundFailure> {
    // 1) 拉取。父列与子列一起限定，避免把敏感列读进进程。
    let mut fields = vec![source.field_name.clone()];
    if let Some(linkage) = source.linkage.as_ref() {
        fields.push(linkage.parent_field.clone());
    }
    let snapshot = list_all_records(
        deps.transport,
        deps.sleeper,
        deps.tokens,
        &source.coordinates,
        &fields,
        deps.max_pages.max(1),
    )
    .await?;

    // 2) 抽取取值对，并派生。
    let values = extract_values_owned(&snapshot, &source.field_name, source.linkage.as_ref());
    let parent_source_key = source
        .linkage
        .as_ref()
        .map(|linkage| linkage.parent_source_key.as_str());
    // 派生要 `&[RawValue]`；这里把自有载体借出去，派生完即丢。
    let raw: Vec<RawValue<'_>> = values.iter().map(OwnedRawValue::as_raw).collect();
    let derived = derive_options(&source.source_key, parent_source_key, &raw);
    let digest = snapshot_digest(&derived);

    let mut report = SourcePullReport {
        fetched: snapshot.items.len(),
        derived: derived.len(),
        ..SourcePullReport::default()
    };

    // 3) 空快照歧义守卫（见模块文档）。
    //
    // 只有在「本轮 0 行 **且** 库里还有已启用的行」时才可疑——正常的空数据源
    // （本来就没选项）应当照常走完，否则它会永远停在「疑似异常」上。
    let (existing_rows, active_rows) = count_existing(deps, &source.source_key)
        .await
        .map_err(internal_failure)?;
    if snapshot.items.is_empty() && active_rows > 0 {
        report.suspicious_empty = true;
        tracing::warn!(
            source_key = %source.source_key,
            active_rows,
            "飞书返回空快照但本地仍有已启用选项：疑似文档权限不足（官方明示高级权限下\
             可能「调用成功但返回空」），本轮拒绝停用补集"
        );
        record_failure(deps, &source.source_key, "空快照可疑：拒绝停用补集")
            .await
            .map_err(internal_failure)?;
        return Ok(report);
    }

    // 4) 内容未变且本地没有已停用行 → 跳过写库。
    //
    // 「没有已停用行」这个附加条件不可省：摘要是按**本轮应当是什么**算的，不含
    // `enabled`；若某行被补集停用后内容恰好没变，只比摘要会永远跳过写、那行永远
    // 复活不了。
    let disabled_rows = existing_rows.saturating_sub(active_rows);
    if source.snapshot_digest.as_deref() == Some(digest.as_str()) && disabled_rows == 0 {
        report.skipped = true;
        record_success(deps, &source.source_key, &digest, report).await?;
        return Ok(report);
    }

    // 5) 事务内落库：跨源预检 → 整行替换 → 补集停用 → 审计 → 更新同步状态。
    let options = deps.context.options();
    let ids: Vec<String> = derived
        .iter()
        .map(|option| option.option_id.clone())
        .collect();
    if let Some((option_id, owner)) = find_foreign_option_owner(options, &source.source_key, &ids)
        .await
        .map_err(internal_failure)?
    {
        // 与入站写入同一道预检：不让一个数据源改写另一个数据源的选项归属。
        return Err(internal_failure(BaseError::ConfigError(format!(
            "选项 id {option_id} 已属于数据源 {owner}，拒绝改写其归属"
        ))));
    }

    let doomed = find_doomed(deps, &source.source_key, &derived)
        .await
        .map_err(internal_failure)?;

    let mut transaction = deps
        .database
        .transaction()
        .await
        .map_err(|error| internal_failure(BaseError::from(error)))?;

    let outcome = async {
        let items = derived
            .iter()
            .map(|option| to_write_item(&source.source_key, option))
            .collect::<Vec<_>>();
        let outcome =
            apply_option_rows(options, &mut transaction, &source.source_key, &items).await?;
        let disabled =
            disable_option_rows(options, &mut transaction, &source.source_key, &doomed).await?;

        let event = audit::succeeded_system_event_without_ctx(
            "feishu-pull",
            "feishu.pull_options",
            Some(audit::entity("feishu_datasource", &source.source_key)?),
            audit::entity("feishu_option", &source.source_key)?,
            audit::summary([
                ("outcome_code", serde_json::json!("pulled")),
                ("option_count", serde_json::json!(derived.len() as i64)),
                ("disabled_count", serde_json::json!(disabled as i64)),
            ])?,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        // 同步状态与内容在同事务提交：半提交会让「摘要已推进但行没写完」永久错位。
        let mut update = Record::new();
        update.insert("snapshot_digest", serde_json::json!(digest));
        update.insert("last_pull_at", serde_json::json!(now_seconds()));
        update.insert("last_success_at", serde_json::json!(now_seconds()));
        update.insert("consecutive_failures", serde_json::json!(0));
        update.insert("last_error", serde_json::Value::Null);
        deps.context
            .datasources()
            .query()
            .where_eq("source_key", serde_json::json!(source.source_key))?
            .update_in_tx(&mut transaction, update)
            .await?;

        Ok::<_, BaseError>((outcome, disabled))
    }
    .await;

    let (outcome, disabled) = match outcome {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|error| internal_failure(BaseError::from(error)))?;
            value
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!(error = %rollback_error, "飞书拉取事务回滚失败");
            }
            return Err(internal_failure(error));
        }
    };
    report.inserted = outcome.inserted;
    report.updated = outcome.updated;
    report.disabled = disabled;

    tracing::info!(
        source_key = %source.source_key,
        fetched = report.fetched,
        derived = report.derived,
        inserted = report.inserted,
        updated = report.updated,
        disabled = report.disabled,
        "飞书数据源同步完成"
    );
    Ok(report)
}

/// 一轮：遍历全部拉取源，单个源失败不影响其它源。
pub(crate) async fn run_round(deps: &PullDeps<'_>) -> Result<RoundReport, BaseError> {
    let sources = load_pull_sources(deps.context).await?;
    let mut report = RoundReport {
        sources: sources.len(),
        ..RoundReport::default()
    };

    for source in &sources {
        // 每个源开跑前**重读**它是否仍然存在且 active：管理员可能在本轮进行中把它
        // 删掉或停用，此时必须丢弃本轮对它的全部写入。
        if !source_is_still_active(deps, &source.source_key).await? {
            tracing::info!(source_key = %source.source_key, "数据源已删除或停用，丢弃本轮结果");
            continue;
        }

        match pull_source(deps, source).await {
            Ok(source_report) => report
                .reports
                .push((source.source_key.clone(), source_report)),
            Err(failure) => {
                report.failures += 1;
                tracing::warn!(
                    source_key = %source.source_key,
                    error = %failure,
                    "飞书数据源同步失败"
                );
                // 失败只记状态、不冒泡：一个源坏掉不该拖停其它源。
                if let Err(error) =
                    record_failure(deps, &source.source_key, &failure.to_string()).await
                {
                    tracing::error!(error = %error, source_key = %source.source_key, "记录同步失败状态时出错");
                }
            }
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 抽取取值对。与 [`extract_values`] 的区别是**自有字符串**，避免把借用透传到
/// 派生之外（派生只在本函数返回后调用一次，没有必要为零拷贝引入生命周期参数）。
fn extract_values_owned(
    snapshot: &RecordsSnapshot,
    field_name: &str,
    linkage: Option<&Linkage>,
) -> Vec<OwnedRawValue> {
    let mut values = Vec::new();
    let mut unsupported = 0usize;
    for record in &snapshot.items {
        let Some(value) = record.fields.get(field_name) else {
            continue;
        };
        let label = match cell_label(value) {
            CellValue::Text(label) => label,
            CellValue::Empty => continue,
            CellValue::Unsupported => {
                unsupported += 1;
                continue;
            }
        };
        let parent_label = linkage.and_then(|linkage| {
            record
                .fields
                .get(&linkage.parent_field)
                .and_then(|value| match cell_label(value) {
                    CellValue::Text(label) => Some(label),
                    _ => None,
                })
        });
        values.push(OwnedRawValue {
            parent_label,
            label,
        });
    }
    if unsupported > 0 {
        // 非空告警：静默丢弃会让「取数列选错类型」表现为「选项莫名变少」。
        tracing::warn!(
            unsupported,
            field_name,
            "取数列存在取值不支持的单元格，已跳过（多值列或人员/地理位置等无文本类型）"
        );
    }
    values
}

/// [`RawValue`] 的自有版本。
#[derive(Debug, Clone)]
struct OwnedRawValue {
    parent_label: Option<String>,
    label: String,
}

impl OwnedRawValue {
    fn as_raw(&self) -> RawValue<'_> {
        RawValue {
            parent_label: self.parent_label.as_deref(),
            label: self.label.as_str(),
        }
    }
}

/// 把派生结果转成待写入的行（**整行替换**：把本轮派生的全部列一并写）。
///
/// `enabled` 必须显式写 `true`：出站拉取是整行替换，某行被补集停用后又重新出现时
/// 要能复活。这与入站 upsert 的「合并」语义不同，也是本函数不复用 `to_record` 的原因。
fn to_write_item(source_key: &str, option: &DerivedOption) -> OptionWriteItem {
    let mut record = Record::new();
    record.insert("option_id", serde_json::json!(option.option_id));
    record.insert("source_key", serde_json::json!(source_key));
    record.insert("label", serde_json::json!(option.label));
    record.insert("sort_order", serde_json::json!(option.sort_order));
    record.insert("enabled", serde_json::json!(true));
    record.insert("parent_key", serde_json::json!(option.parent_key));
    record.insert("last_push_at", serde_json::json!(now_seconds()));
    OptionWriteItem {
        option_id: option.option_id.clone(),
        record,
    }
}

/// 找出「本地有、本轮派生结果里没有」的选项 id（补集）。
///
/// 规模保护：一次取回该数据源的全部 `option_id`。**超过 [`MAX_BATCH`] × 上限时
/// 放弃停用并告警**——宁可不收敛，也不要在一次可能被截断的读取上做批量停用。
async fn find_doomed(
    deps: &PullDeps<'_>,
    source_key: &str,
    derived: &[DerivedOption],
) -> Result<Vec<String>, BaseError> {
    /// 单轮补集停用的规模上限：超过就跳过本轮停用，只告警。
    const MAX_COMPLEMENT: usize = 20_000;

    let live: HashSet<&str> = derived
        .iter()
        .map(|option| option.option_id.as_str())
        .collect();
    let rows = deps
        .context
        .options()
        .query()
        .select_fields(&["option_id"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, MAX_COMPLEMENT)?
        .all()
        .await?;

    let mut doomed = Vec::new();
    for row in &rows {
        let option_id: String = row.require("option_id")?;
        if !live.contains(option_id.as_str()) {
            doomed.push(option_id);
        }
    }
    if doomed.len() >= MAX_COMPLEMENT {
        tracing::warn!(
            source_key,
            "补集规模达到上限 {MAX_COMPLEMENT}，本轮跳过补集停用以避免误停"
        );
        return Ok(Vec::new());
    }
    Ok(doomed)
}

/// 统计该数据源的存量行数与已启用行数。
async fn count_existing(deps: &PullDeps<'_>, source_key: &str) -> Result<(u64, u64), BaseError> {
    let total = count_option_rows(deps.context.options(), source_key).await?;
    let active = deps
        .context
        .options()
        .query()
        .select_fields(&["option_id"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .where_eq("enabled", serde_json::json!(true))?
        .count()
        .await?;
    Ok((total, active))
}

/// 数据源是否仍然存在且为 active。
async fn source_is_still_active(deps: &PullDeps<'_>, source_key: &str) -> Result<bool, BaseError> {
    let row = deps
        .context
        .datasources()
        .query()
        .select_fields(&["status"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .optional()
        .await?;
    match row {
        Some(row) => Ok(row.optional::<String>("status")?.as_deref() == Some("active")),
        None => Ok(false),
    }
}

/// 记录一次成功（含「内容未变而跳过」的快路径）。独立事务提交。
async fn record_success(
    deps: &PullDeps<'_>,
    source_key: &str,
    digest: &str,
    report: SourcePullReport,
) -> Result<(), OutboundFailure> {
    let mut transaction = deps
        .database
        .transaction()
        .await
        .map_err(|error| internal_failure(BaseError::from(error)))?;
    let mut update = Record::new();
    update.insert("snapshot_digest", serde_json::json!(digest));
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_success_at", serde_json::json!(now_seconds()));
    update.insert("consecutive_failures", serde_json::json!(0));
    update.insert("last_error", serde_json::Value::Null);
    let result = deps
        .context
        .datasources()
        .query()
        .where_eq("source_key", serde_json::json!(source_key))
        .map_err(internal_failure)?
        .update_in_tx(&mut transaction, update)
        .await;
    match result {
        Ok(_) => {
            transaction
                .commit()
                .await
                .map_err(|error| internal_failure(BaseError::from(error)))?;
            tracing::debug!(source_key, derived = report.derived, "内容未变，跳过写库");
            Ok(())
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(internal_failure(error))
        }
    }
}

/// 记录一次失败：`consecutive_failures` 自增、写 `last_error`。
///
/// **只在整轮成功时清零**，因此这里是自增而不是覆盖为 1——连续失败次数是控制台
/// 与告警判断「这个源还活着吗」的唯一诚实信号。
async fn record_failure(
    deps: &PullDeps<'_>,
    source_key: &str,
    message: &str,
) -> Result<(), BaseError> {
    let current = deps
        .context
        .datasources()
        .query()
        .select_fields(&["consecutive_failures"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .optional()
        .await?;
    let Some(current) = current else {
        return Ok(());
    };
    let failures: i64 = current.optional("consecutive_failures")?.unwrap_or(0);
    // 错误文案可能很长（含飞书原始响应），截断后再落库。
    let message: String = message.chars().take(1000).collect();

    let mut transaction = deps.database.transaction().await?;
    let mut update = Record::new();
    update.insert("consecutive_failures", serde_json::json!(failures + 1));
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_error", serde_json::json!(message));
    let result = deps
        .context
        .datasources()
        .query()
        .where_eq("source_key", serde_json::json!(source_key))?
        .update_in_tx(&mut transaction, update)
        .await;
    match result {
        Ok(_) => transaction.commit().await.map_err(BaseError::from),
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// 把「内部错误」包装成出站失败，让上层的统一日志格式不必分叉。
fn internal_failure(error: BaseError) -> OutboundFailure {
    OutboundFailure {
        kind: super::outbound::FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::bitable::{RecordItem, RecordsSnapshot};
    use serde_json::json;

    fn snapshot(rows: Vec<(&str, &str)>) -> RecordsSnapshot {
        let items = rows
            .into_iter()
            .map(|(child, parent)| {
                let mut fields = std::collections::BTreeMap::new();
                fields.insert("子".to_string(), json!(child));
                fields.insert("父".to_string(), json!(parent));
                RecordItem {
                    record_id: "rec".to_string(),
                    fields,
                }
            })
            .collect::<Vec<_>>();
        RecordsSnapshot {
            total: items.len() as i64,
            items,
        }
    }

    fn linkage() -> Linkage {
        Linkage {
            parent_source_key: "currency".to_string(),
            parent_field: "父".to_string(),
            cascade_field: "子".to_string(),
        }
    }

    #[test]
    fn linkage_mapping_parses_the_declared_triple() {
        let raw = r#"{"widget1":{"parent_source_key":"payment_currency",
            "parent_field":"币种/Currency（单选）","cascade_field":"汇率/Exchange Rate"}}"#;
        let parsed = parse_linkage(Some(raw.to_string()))
            .unwrap_or_else(|error| panic!("应可解析: {error}"))
            .unwrap_or_else(|| panic!("应解析出级联关系"));
        assert_eq!(parsed.parent_source_key, "payment_currency");
        assert_eq!(parsed.parent_field, "币种/Currency（单选）");
        assert_eq!(parsed.cascade_field, "汇率/Exchange Rate");
    }

    #[test]
    fn blank_or_missing_linkage_is_no_cascade() {
        assert!(parse_linkage(None).unwrap_or_default().is_none());
        assert!(parse_linkage(Some("   ".to_string()))
            .unwrap_or_default()
            .is_none());
        assert!(parse_linkage(Some("{}".to_string()))
            .unwrap_or_default()
            .is_none());
    }

    #[test]
    fn malformed_linkage_degrades_instead_of_failing_the_round() {
        // 一段坏 JSON 不该让整条链路停摆
        assert!(parse_linkage(Some("{not json".to_string()))
            .unwrap_or_default()
            .is_none());
    }

    #[test]
    fn incomplete_linkage_is_treated_as_no_cascade() {
        // 三个成员缺一就拼不出父键
        for raw in [
            r#"{"w":{"parent_source_key":"","parent_field":"父","cascade_field":"子"}}"#,
            r#"{"w":{"parent_source_key":"c","parent_field":"","cascade_field":"子"}}"#,
            r#"{"w":{"parent_source_key":"c","parent_field":"父","cascade_field":""}}"#,
        ] {
            assert!(
                parse_linkage(Some(raw.to_string()))
                    .unwrap_or_default()
                    .is_none(),
                "{raw} 应被视为非级联"
            );
        }
    }

    #[test]
    fn values_pair_child_with_the_same_rows_parent() {
        // 父键由**同行共现**读出，这是 A7b 的依据
        let snap = snapshot(vec![("6.9025", "USD 美元"), ("0.8807", "HKD 港币")]);
        let values = extract_values_owned(&snap, "子", Some(&linkage()));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].label, "6.9025");
        assert_eq!(values[0].parent_label.as_deref(), Some("USD 美元"));
        assert_eq!(values[1].parent_label.as_deref(), Some("HKD 港币"));
    }

    #[test]
    fn without_linkage_parents_are_absent() {
        let snap = snapshot(vec![("甲", "父值")]);
        let values = extract_values_owned(&snap, "子", None);
        assert_eq!(values.len(), 1);
        assert!(values[0].parent_label.is_none());
    }

    #[test]
    fn empty_cells_do_not_produce_values() {
        let snap = snapshot(vec![("", "USD"), ("6.9025", "USD")]);
        let values = extract_values_owned(&snap, "子", Some(&linkage()));
        assert_eq!(values.len(), 1, "空单元格不贡献任何值");
        assert_eq!(values[0].label, "6.9025");
    }

    #[test]
    fn missing_child_column_yields_nothing() {
        // 取数列名配错时不是报错，而是没有任何值——上层会看到 derived=0
        let snap = snapshot(vec![("甲", "父值")]);
        assert!(extract_values_owned(&snap, "不存在的列", None).is_empty());
    }

    #[test]
    fn unsupported_cell_shapes_are_skipped() {
        let mut fields = std::collections::BTreeMap::new();
        // 人员列：对象数组，不含 text 键
        fields.insert("子".to_string(), json!([{"id": "ou_x", "name": "张三"}]));
        let snap = RecordsSnapshot {
            total: 1,
            items: vec![RecordItem {
                record_id: "rec".to_string(),
                fields,
            }],
        };
        assert!(extract_values_owned(&snap, "子", None).is_empty());
    }

    #[test]
    fn write_item_replaces_the_whole_row_and_reenables() {
        // 整行替换：`enabled` 显式写 true，否则「停用→重新出现」的行永远复活不了
        let option = DerivedOption {
            option_id: "demo:abc".to_string(),
            label: "北京".to_string(),
            parent_key: String::new(),
            sort_order: 3,
        };
        let item = to_write_item("demo", &option);
        assert_eq!(item.option_id, "demo:abc");
        let record = item.record;
        assert_eq!(record.get("enabled"), Some(&json!(true)));
        assert_eq!(record.get("sort_order"), Some(&json!(3)));
        assert_eq!(record.get("source_key"), Some(&json!("demo")));
        assert_eq!(record.get("parent_key"), Some(&json!("")));
        assert!(
            record.get("last_push_at").is_some(),
            "整行替换必须带上同步信号，否则控制台看到的存活时间会停在旧值"
        );
        // i18n / is_default 不属出站路径拥有的列，不得被写（否则会把入站推来的值抹掉）
        assert!(record.get("i18n").is_none());
        assert!(record.get("is_default").is_none());
    }

    #[test]
    fn owned_raw_value_borrows_correctly() {
        let owned = OwnedRawValue {
            parent_label: Some("USD".to_string()),
            label: "6.9".to_string(),
        };
        let raw = owned.as_raw();
        assert_eq!(raw.label, "6.9");
        assert_eq!(raw.parent_label, Some("USD"));
    }
}
