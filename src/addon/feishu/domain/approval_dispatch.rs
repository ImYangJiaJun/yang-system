//! 审批实例派发的单条编排。
//!
//! 把「一条多维表格记录」变成「一个审批实例 + 回填的编号」的全过程。

#![allow(dead_code)]
// 编排先落地并自带测试；消费者（dispatch Action 与 worker）在后续任务接入。
//!
//! # 落库时序（不可省，见设计 §5.2）
//!
//! ```text
//! 1. 落 state=creating + uuid      ← 在调创建接口**之前**
//! 2. 本地必填校验                   ← 不完整 → Waiting（不写任何表格字段）
//! 3. 组装 form
//! 4. create                         ← 成功后**立即**落 instance_code
//! 5. 取 serial_number              ← 缺失判可重试，绝不写终态
//! 6. 回写多维表格 → state=backfilled
//! ```
//!
//! 第 1 步与第 4 步的时机是这套设计的核心：蓝绿 cutover 用 `docker stop`
//! （SIGTERM → 10 秒后 SIGKILL）静默杀掉处理中的批次，没有这两步的时序，
//! 「创建成功但本地未落库」就没有任何痕迹可续跑。
//!
//! # `60012` 的回捞
//!
//! uuid 冲突的响应体**不含** `instance_code`，而这是「响应丢失、实例其实已建成功」
//! 的信号。处置是用 uuid 反查实例详情一次取回 `instance_code` 与 `serial_number`。
//! 这条路径必须存在——否则那条记录会被写成终态错误，而回填字段写进去就不可逆
//! （该行再也不会被扫到），彻底丢失一个真实存在的审批单。

use std::collections::BTreeMap;

use serde_json::Value;

use super::approval::{
    create_instance, get_approval_definition, get_instance, CreateOutcome, GetOutcome,
    InstanceDetail,
};
use super::approval_attachment;
use super::approval_convert::{build_form, Converter, FixedOffset, FormInput, WidgetMap};
use super::approval_uuid::derive_uuid;
use super::bitable::{BitableCoordinates, FieldItem};
use super::outbound::{FailureKind, OutboundFailure, OutboundTransport, Sleeper};
use super::tenant_token::TenantTokenProvider;

/// 单条处理的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchResult {
    /// 创建并回填成功。
    Backfilled { serial_number: String },
    /// 数据不完整，本轮跳过。
    ///
    /// **不写任何多维表格字段**——用户先填业务字段、后填「申请人」的那一轮若被
    /// 写成终态错误，该行会因字段非空而永久不再被扫到。详见设计 §6.3。
    Waiting { reason: String },
    /// 终态失败；文本列写原因，链接列仅由任务/请求日志保存。
    Terminal { message: String },
    /// 可重试失败，未写任何字段，等下一轮或退避后再来。
    Retryable { message: String },
}

impl DispatchResult {
    /// 折算成给工作流的响应。
    ///
    /// # 语义映射（不要随手改）
    ///
    /// - `Backfilled` → 成功。工作流可据此发消息或写日志。
    /// - `Waiting` → **也报成功**。它的含义是「本轮数据还没填完，没做任何事」——
    ///   报失败会让工作流把它当异常（甚至触发重试），而这是使用者的正常中间状态。
    ///   真正的信号在 message 里。
    /// - `Terminal` → 失败。原因由日志保存，文本列同时回填原因。
    /// - `Retryable` → 失败，但**可重试**：没写任何字段，再点一次按钮就会重来。
    pub(crate) fn response_parts(&self) -> (bool, String, Option<String>) {
        match self {
            Self::Backfilled { serial_number } => (
                true,
                format!("已创建审批实例，编号 {serial_number}"),
                Some(serial_number.clone()),
            ),
            Self::Waiting { reason } => (true, format!("本轮未处理：{reason}"), None),
            Self::Terminal { message } => (false, message.clone(), None),
            Self::Retryable { message } => (false, format!("暂时失败，可重试：{message}"), None),
        }
    }
}

/// 一条记录的处理输入。
pub(crate) struct DispatchInput<'a> {
    pub(crate) context: Option<&'a super::context::FeishuContext>,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) record_id: &'a str,
    /// 该记录的原始单元格（键是字段 id）。
    pub(crate) cells: &'a serde_json::Map<String, Value>,
    /// 申请人的人员字段 id。
    pub(crate) applicant_field: &'a str,
    /// 回填字段 id。
    /// 回填列的**当前列名**（不是 `field_id`）。
    ///
    /// `batch_update` 的 `records[].fields` 是**按列名**作键的 map（《数据结构概述》：
    /// 「key 是多维表格数据表中的字段名称」，官方每个写示例也都是列名），而配置里
    /// 存的是 `field_id`（刻意如此——改列名不该让配置失效）。两条轴的差由**调用方**
    /// 在拿到表结构后现场补上，与读侧的 `rekey_cells_by_field_id` 是同一件事的两个
    /// 方向。这里收列名而不是 id，正是为了让「写到哪一列」在调用处一眼可见。
    pub(crate) backfill_field_name: &'a str,
    pub(crate) approval_code: &'a str,
    pub(crate) widgets: &'a [WidgetMap],
    /// 定义快照；旧配置留空时，在创建实例前补取定义。
    pub(crate) form_snapshot: String,
    pub(crate) timezone_offset: FixedOffset,
}

/// 回写多维表格里的一个单元格。
///
/// 抽成 trait 是为了让编排逻辑（含 60012 回捞）能在不碰真实飞书的前提下测——
/// 编排的正确性全在这些分支的**顺序与归属**上，而那正是最该被测的部分。
#[async_trait::async_trait]
pub(crate) trait Backfill: Send + Sync {
    /// 成功传 text/link 对象，错误传文本；真实写入层按目标字段类型处理。
    async fn write(
        &self,
        record_id: &str,
        field_id: &str,
        value: &Value,
    ) -> Result<(), OutboundFailure>;

    /// 一次写多条（同一字段）。**全有全无**：任一记录失败即整批零条落库。
    ///
    /// 分批与毒记录隔离由 [`backfill_in_chunks`] 负责，这里只管发一批。
    async fn write_many(
        &self,
        field_id: &str,
        rows: &[(String, Value)],
    ) -> Result<(), OutboundFailure>;
}

/// 真实实现：经 `bitable::batch_update_records` 写单个单元格。
pub(crate) struct BitableBackfill<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) fields: &'a [FieldItem],
}

#[async_trait::async_trait]
impl Backfill for BitableBackfill<'_> {
    async fn write(
        &self,
        record_id: &str,
        field_id: &str,
        value: &Value,
    ) -> Result<(), OutboundFailure> {
        let rows = vec![(record_id.to_string(), value.clone())];
        self.write_many(field_id, &rows).await
    }

    async fn write_many(
        &self,
        field_id: &str,
        rows: &[(String, Value)],
    ) -> Result<(), OutboundFailure> {
        let field = self
            .fields
            .iter()
            .find(|field| field.field_id == field_id || field.field_name == field_id)
            .ok_or_else(|| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: format!("找不到回填列：{field_id}"),
            })?;
        let field_name =
            super::bitable::resolve_field_name(self.fields, &field.field_id).map_err(|error| {
                OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: error.to_string(),
                }
            })?;
        let mut records = Vec::with_capacity(rows.len());
        for (record_id, value) in rows {
            let cell = match field.field_type {
                Some(15) if value.is_object() => value.clone(),
                // 错误信息没有审批实例；链接列保持空白，原因由任务/请求日志保存。
                Some(15) => continue,
                Some(1) | None => value.get("text").unwrap_or(value).clone(),
                _ => {
                    return Err(OutboundFailure {
                        kind: FailureKind::Fatal { code: 0 },
                        message: "回填列必须是文本或链接字段".to_string(),
                    })
                }
            };
            records.push((
                record_id.clone(),
                BTreeMap::from([(field_name.clone(), cell)]),
            ));
        }
        if records.is_empty() {
            return Ok(());
        }
        super::bitable::batch_update_records(
            self.transport,
            self.sleeper,
            self.tokens,
            self.coordinates,
            &records,
        )
        .await
    }
}

/// 处理一条记录。
///
/// 本函数**不做落库**（任务行的写入由调用方在 claim 阶段完成，因为那需要事务，
/// 而事务的生命周期属于 Action/worker）。它只负责「创建 + 取编号 + 回写」这一段。
pub(crate) async fn dispatch_one(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    backfill: &dyn Backfill,
    input: &DispatchInput<'_>,
) -> DispatchResult {
    // ---- 1. uuid ----
    let uuid = derive_uuid(
        &input.coordinates.app_token,
        &input.coordinates.table_id,
        input.approval_code,
        input.record_id,
    );

    // 先恢复已创建实例；源附件或关联审批权限变化也不能阻断编号回填。
    if input
        .widgets
        .iter()
        .any(|w| matches!(w.widget_type.as_str(), "attachmentV2" | "connect"))
    {
        match get_instance(transport, sleeper, tokens, &uuid).await {
            Ok(GetOutcome::Found(detail)) => {
                return finish_backfill(backfill, input, &uuid, detail).await;
            }
            Ok(GetOutcome::NotFound) => {}
            Err(failure) => {
                return DispatchResult::Retryable {
                    message: failure.to_string(),
                }
            }
        }
    }

    // ---- 2. 申请人 ----
    let open_id = match applicant_open_id(input.cells, input.applicant_field) {
        Some(open_id) => open_id,
        None => {
            return DispatchResult::Waiting {
                reason: "缺少「申请人」".to_string(),
            }
        }
    };

    // ---- 3. 组装 form ----
    let form_snapshot = if input.form_snapshot.trim().is_empty() {
        match get_approval_definition(transport, sleeper, tokens, input.approval_code).await {
            Ok(definition) => match definition.form {
                Some(Value::String(form)) => form,
                form => form.unwrap_or(Value::Null).to_string(),
            },
            Err(failure) => return classify_failure(failure, input, backfill, uuid.as_str()).await,
        }
    } else {
        input.form_snapshot.clone()
    };
    let mut widgets = input.widgets.to_vec();
    if let Some(context) = input.context {
        if let Err(error) = super::approval_provision::resolve_external_options(
            context,
            &form_snapshot,
            &mut widgets,
            input.cells,
        )
        .await
        {
            return match error {
                super::approval_provision::ProvisionError::Invalid(reasons) => {
                    DispatchResult::Waiting {
                        reason: reasons.join("；"),
                    }
                }
                error => DispatchResult::Retryable {
                    message: error.to_string(),
                },
            };
        }
    }
    let preflight = approval_attachment::preflight(&widgets, input.cells);
    let form = match preflight.and_then(|codes| {
        build_form(&FormInput {
            widgets: &widgets,
            form_snapshot: &form_snapshot,
            cells: input.cells,
            attachment_codes: &codes,
            timezone_offset: input.timezone_offset,
        })
    }) {
        Ok(form) => form,
        // 组装失败分两类：**数据不完整**（必填缺失、值形态不对）是等待态，
        // **配置不对**（不支持的控件、转换器不匹配、选项没配映射）是终态——
        // 后者不补齐字段就能过，反复重扫永远好不了。
        Err(error) => {
            let message = error.to_string();
            return if is_data_problem(&error) {
                DispatchResult::Waiting { reason: message }
            } else {
                // 配置问题（不支持的控件、转换器不匹配、选项没配映射）**必须写字段**：
                // 它是终态，而终态的意义就是「该行退出扫描集，等人工处理」。不写的话
                // 该行每轮都被重新捞出、每轮都失败一次，用户却看不到任何线索。
                let message = sanitize_terminal_message(&message, 0);
                let _ = backfill
                    .write(
                        input.record_id,
                        input.backfill_field_name,
                        &Value::String(message.clone()),
                    )
                    .await;
                DispatchResult::Terminal { message }
            };
        }
    };

    if let Err(result) =
        validate_connections(transport, sleeper, tokens, &form_snapshot, &form).await
    {
        return result;
    }

    let form = if input
        .widgets
        .iter()
        .any(|w| w.widget_type == "attachmentV2")
    {
        let codes = match approval_attachment::transfer(
            transport,
            sleeper,
            tokens,
            input.coordinates,
            input.record_id,
            &widgets,
            input.cells,
        )
        .await
        {
            Ok(codes) => codes,
            Err(failure) => {
                return DispatchResult::Retryable {
                    message: failure.to_string(),
                }
            }
        };
        match build_form(&FormInput {
            widgets: &widgets,
            form_snapshot: &form_snapshot,
            cells: input.cells,
            attachment_codes: &codes,
            timezone_offset: input.timezone_offset,
        }) {
            Ok(form) => form,
            Err(error) => {
                return DispatchResult::Retryable {
                    message: error.to_string(),
                }
            }
        }
    } else {
        form
    };

    // ---- 4. 创建 ----
    //
    // 回捞成功时，那次 `get_instance` 已经同时拿回了 `instance_code` 与
    // `serial_number`（官方文档的响应体同时含两者），所以**不再多打一次详情接口**
    // ——每条记录省一次 1000 次/分钟的配额消耗。
    let reclaimed: Option<InstanceDetail>;
    let instance_code = match create_instance(
        transport,
        sleeper,
        tokens,
        input.approval_code,
        &form,
        &open_id,
        &uuid,
    )
    .await
    {
        Ok(CreateOutcome::Created { instance_code }) => {
            reclaimed = None;
            instance_code
        }
        Ok(CreateOutcome::UuidConflict) => {
            // ---- 回捞：uuid 冲突 = 实例此前已建成功，响应丢了 ----
            match get_instance(transport, sleeper, tokens, &uuid).await {
                Ok(GetOutcome::Found(detail)) => {
                    let code = detail.instance_code.clone();
                    reclaimed = Some(detail);
                    code
                }
                // 实例不存在：并发窗口（冲突成立但实例还没可见）或前一次其实没建成。
                // **不落终态**——那会把可能存在的实例判死。
                Ok(GetOutcome::NotFound) => {
                    return DispatchResult::Retryable {
                        message: format!("uuid 冲突但按 uuid 查不到实例（uuid={uuid}），稍后重试"),
                    }
                }
                Err(failure) => {
                    return classify_failure(failure, input, backfill, uuid.as_str()).await
                }
            }
        }
        Err(failure) => return classify_failure(failure, input, backfill, uuid.as_str()).await,
    };

    // ---- 5. 取 serial_number ----
    //
    // 回捞路径已经拿到了详情，只有正常创建路径需要再查一次。
    let detail = match reclaimed {
        Some(detail) => detail,
        None => match get_instance(transport, sleeper, tokens, &instance_code).await {
            Ok(GetOutcome::Found(detail)) => detail,
            // 刚创建的实例查不到：判**可重试**，绝不写终态——否则刚建成的实例会被
            // 写成「找不到」，反而制造第二条不可恢复路径。
            Ok(GetOutcome::NotFound) => {
                return DispatchResult::Retryable {
                    message: format!("实例 {instance_code} 刚创建但查不到详情，稍后重试"),
                }
            }
            Err(failure) => return classify_failure(failure, input, backfill, uuid.as_str()).await,
        },
    };

    finish_backfill(backfill, input, &uuid, detail).await
}

/// 在上传附件和创建审批之前校验关联实例；同一实例在一单中只查询一次。
async fn validate_connections(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    snapshot: &str,
    form: &str,
) -> Result<(), DispatchResult> {
    fn collect<'a>(nodes: &'a [Value], out: &mut Vec<&'a Value>) {
        for node in nodes {
            if node["type"] == "connect" {
                out.push(node);
            } else if node["type"] == "fieldList" {
                for row in node["value"].as_array().into_iter().flatten() {
                    if let Some(children) = row.as_array() {
                        collect(children, out);
                    }
                }
            }
        }
    }
    let definition: Vec<super::approval_match::FormWidget> = serde_json::from_str(snapshot)
        .map_err(|_| DispatchResult::Retryable {
            message: "关联审批定义快照解析失败".into(),
        })?;
    let form: Vec<Value> = serde_json::from_str(form).map_err(|_| DispatchResult::Retryable {
        message: "关联审批表单解析失败".into(),
    })?;
    let mut connections = Vec::new();
    collect(&form, &mut connections);
    let nodes: Vec<_> = definition.iter().flat_map(|node| node.walk()).collect();
    let mut checked = BTreeMap::new();
    for connection in connections {
        let node = nodes
            .iter()
            .find(|node| connection["id"] == node.id)
            .ok_or_else(|| DispatchResult::Retryable {
                message: "关联控件不在定义快照中".into(),
            })?;
        let allowed =
            node.connect_definition_codes()
                .map_err(|reason| DispatchResult::Retryable {
                    message: format!(
                        "关联审批控件「{}」（{}）配置无效：{reason}",
                        node.name, node.id
                    ),
                })?;
        for code in connection["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !checked.contains_key(code) {
                let detail = match get_instance(transport, sleeper, tokens, code).await {
                    Ok(GetOutcome::Found(detail)) => detail,
                    Ok(GetOutcome::NotFound) => {
                        return Err(DispatchResult::Waiting {
                            reason: format!(
                                "关联审批控件「{}」（{}）：实例 {code} 不存在",
                                node.name, node.id
                            ),
                        })
                    }
                    Err(failure) => {
                        return Err(DispatchResult::Retryable {
                            message: format!(
                                "关联审批控件「{}」（{}）查询失败：{}",
                                node.name, node.id, failure
                            ),
                        })
                    }
                };
                if !detail.instance_code.eq_ignore_ascii_case(code)
                    || detail
                        .approval_code
                        .as_deref()
                        .map_or(true, |code| code.trim().is_empty())
                {
                    return Err(DispatchResult::Retryable {
                        message: "关联审批详情缺少定义 Code 或实例 Code 不一致".into(),
                    });
                }
                checked.insert(code.to_string(), detail.approval_code.unwrap_or_default());
            }
            let approval_code = &checked[code];
            if allowed.as_ref().is_some_and(|allowed| {
                !allowed
                    .iter()
                    .any(|code| code.eq_ignore_ascii_case(approval_code))
            }) {
                return Err(DispatchResult::Waiting {
                    reason: format!(
                        "关联审批控件「{}」（{}）：实例 {code} 不属于允许关联的审批定义",
                        node.name, node.id
                    ),
                });
            }
        }
    }
    Ok(())
}

async fn finish_backfill(
    backfill: &dyn Backfill,
    input: &DispatchInput<'_>,
    uuid: &str,
    detail: InstanceDetail,
) -> DispatchResult {
    let Some(serial_number) = serial_number_of(&detail) else {
        // 实例存在但编号还没生成：同样是可重试，不是失败。
        return DispatchResult::Retryable {
            message: format!(
                "实例 {} 已创建但编号尚未生成，稍后重试",
                detail.instance_code
            ),
        };
    };

    let link = match super::approval::instance_applink(&detail.instance_code) {
        Ok(link) => link,
        Err(error) => {
            return DispatchResult::Retryable {
                message: error.to_string(),
            }
        }
    };
    let value = serde_json::json!({"text": serial_number, "link": link});
    // ---- 6. 回写 ----
    match backfill
        .write(input.record_id, input.backfill_field_name, &value)
        .await
    {
        Ok(()) => DispatchResult::Backfilled { serial_number },
        Err(failure) => DispatchResult::Retryable {
            message: format!(
                "实例 {} 已创建，回填失败：{}（uuid={uuid}）",
                detail.instance_code, failure.message
            ),
        },
    }
}

/// 从实例详情里取编号。空串与缺失同义。
fn serial_number_of(detail: &InstanceDetail) -> Option<String> {
    detail
        .serial_number
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 把出站失败折成处置结论。
///
/// **写字段的只有终态**：可重试与幂等命中都不碰多维表格——写进去就不可逆
/// （该行不再被扫到），会把一个本可恢复的状态固化成失败。
async fn classify_failure(
    failure: OutboundFailure,
    input: &DispatchInput<'_>,
    backfill: &dyn Backfill,
    uuid: &str,
) -> DispatchResult {
    match failure.kind {
        // 可重试：不写字段。
        FailureKind::Retry { .. } | FailureKind::TokenExpired => DispatchResult::Retryable {
            message: failure.message,
        },
        // uuid 冲突出现在这里说明它来自取值阶段（创建之后），此时按 uuid 反查
        // 才是正解；但调用点已经在创建分支处理过冲突，走到这里只能是别处冒出来的
        // 冲突——一律判可重试，把决定权交给下一轮的创建分支。
        FailureKind::UuidConflict => DispatchResult::Retryable {
            message: format!("uuid 冲突需反查实例（uuid={uuid}）：{}", failure.message),
        },
        FailureKind::InstanceNotFound => DispatchResult::Retryable {
            message: failure.message,
        },
        // 终态：写回填字段。文案要**裁剪**——多维表格的读者范围远大于运维，
        // 原样回灌可能含内部 request_id、租户信息。
        FailureKind::Fatal { code } => {
            let message = sanitize_terminal_message(&failure.message, code);
            // 回写失败也不改变结论：终态的判据是「这个记录本身有问题」，
            // 而错误文案写不进去只是让用户少了一个排查线索。
            let _ = backfill
                .write(
                    input.record_id,
                    input.backfill_field_name,
                    &Value::String(message.clone()),
                )
                .await;
            DispatchResult::Terminal { message }
        }
    }
}

/// 判「组装失败」是数据问题还是配置问题。
///
/// - 数据问题（等待态）：必填缺失、值形态不对——补齐数据就能过。
/// - 配置问题（终态）：不支持的控件、转换器不匹配、选项没配映射——不补数据的话
///   反复重扫永远好不了，必须让用户看见并改配置。
fn is_data_problem(error: &super::approval_convert::ConvertError) -> bool {
    use super::approval_convert::ConvertError;
    matches!(
        error,
        ConvertError::MissingRequired { .. } | ConvertError::BadValue { .. }
    )
}

/// 终态错误写进多维表格前的裁剪。
///
/// 只保留**可行动的部分**：错误码 + 一句话说明。去掉响应体原文（可能含
/// request_id、内部路径、租户信息）。多维表格的读者是业务人员，范围远大于运维。
fn sanitize_terminal_message(raw: &str, code: i32) -> String {
    let hint = super::outbound::fatal_hint(code);
    let head = match hint {
        Some(hint) => hint.to_string(),
        // 没有登记文案的码：保留码本身与原文（原文已由 outbound 的 summarize
        // 截断过），至少给出可搜索的定位信息。
        None => raw.chars().take(200).collect(),
    };
    if code == 0 {
        head
    } else {
        format!("[飞书 {code}] {head}")
    }
}

/// 从单元格里取申请人的 `open_id`。
///
/// 多维表格的人员字段是对象数组（`[{"id": "ou_…"}]`）。取第一个——审批实例只能
/// 有一个发起人，而人员字段若配成多选，多出来的人没有对应的语义。
fn applicant_open_id(cells: &serde_json::Map<String, Value>, field_id: &str) -> Option<String> {
    let cell = cells.get(field_id)?;
    let first = match cell {
        Value::Array(items) => items.first()?,
        other => other,
    };
    let id = match first {
        Value::Object(map) => map.get("id").and_then(Value::as_str),
        Value::String(text) => Some(text.as_str()),
        _ => None,
    }?;
    let trimmed = id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 由配置行构造映射快照。转换器标识未知时返回 `None`——那是配置损坏，
/// 调用方应报错而不是猜一个默认转换器（静默退化成 `direct` 会把日期或选项值
/// 原样送出去，表现为飞书侧的格式错误，排查方向完全跑偏）。
pub(crate) fn widget_maps_from_rows(
    rows: &[serde_json::Map<String, Value>],
    currency_of: impl Fn(&str) -> Option<String>,
) -> Option<Vec<WidgetMap>> {
    let mut widgets = Vec::with_capacity(rows.len());
    for row in rows {
        let widget_id = row.get("widget_id")?.as_str()?.to_string();
        let widget_type = row.get("widget_type")?.as_str()?.to_string();
        let bitable_field = row.get("bitable_field")?.as_str()?.to_string();
        let converter = Converter::parse(row.get("converter")?.as_str()?).ok()?;
        let required = row
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let option_map = row
            .get("option_map")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<BTreeMap<String, String>>(text).ok())
            .unwrap_or_default();
        widgets.push(WidgetMap {
            currency: currency_of(&widget_id),
            widget_id,
            widget_type,
            required,
            bitable_field,
            converter,
            option_map,
        });
    }
    Some(widgets)
}

// ---------------------------------------------------------------------------
// 分批回写与毒记录隔离
// ---------------------------------------------------------------------------

/// 一批回写的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BackfillReport {
    /// 成功写进多维表格的记录 id。
    pub(crate) written: Vec<String>,
    /// 回写失败的记录：记录 id + 原因。这些记录的审批实例**已经创建**，
    /// 只是编号没写进去——所以它们必须落成「可重试」而不是「终止」。
    pub(crate) failed: Vec<(String, String)>,
}

/// 把一批「记录 id → 回填文本」写进多维表格。
///
/// # 为什么必须分批
///
/// 多维表格的批量写是**全有全无**语义（官方：「响应状态是全部成功或者失败，
/// 不存在部分成功或失败的结果」），且响应体**没有 per-record 失败列表**可读。
/// 所以按搜索的一整页（500 条）一次提交时，一条毒记录会让整页零条落库——而
/// 审批实例创建是**已经发生的外部副作用**，等于批量制造孤儿实例。
///
/// # 毒记录隔离
///
/// 子批失败就**对半拆**，直到定位到具体记录。这是「没有 per-record 失败列表」
/// 逼出来的唯一可行做法。定位到之后只把该条记为失败，其余照常落库。
///
/// 拆批的安全性来自全有全无语义本身：失败即整批零条落库，所以对半拆不会
/// 写重复。
pub(crate) async fn backfill_in_chunks(
    backfill: &dyn Backfill,
    field_id: &str,
    rows: &[(String, Value)],
) -> BackfillReport {
    let mut report = BackfillReport::default();
    for chunk in rows.chunks(super::bitable::BACKFILL_CHUNK) {
        write_chunk(backfill, field_id, chunk, &mut report).await;
    }
    report
}

/// 写一个子批；失败则对半拆，直到定位到单条。
async fn write_chunk(
    backfill: &dyn Backfill,
    field_id: &str,
    chunk: &[(String, Value)],
    report: &mut BackfillReport,
) {
    match backfill.write_many(field_id, chunk).await {
        Ok(()) => report
            .written
            .extend(chunk.iter().map(|(record_id, _)| record_id.clone())),
        Err(failure) => {
            if chunk.len() == 1 {
                // 收敛到单条仍失败：这条就是毒记录。
                let (record_id, _) = &chunk[0];
                report
                    .failed
                    .push((record_id.clone(), failure.message.clone()));
                return;
            }
            // 对半拆（用 `div_ceil` 让奇数批的前半多一条，两半都非空）。
            let middle = chunk.len().div_ceil(2);
            let (head, tail) = chunk.split_at(middle);
            // 递归深度是对数级（100 → 50 → 25 → … → 1），不会爆栈。
            // 用 `Box::pin` 是因为 async 递归需要显式装箱。
            Box::pin(write_chunk(backfill, field_id, head, report)).await;
            Box::pin(write_chunk(backfill, field_id, tail, report)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::outbound::OutboundResponse;
    use crate::addon::feishu::domain::tenant_token::{
        FeishuCredentials, TenantTokenCache, TenantTokenProvider,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn coordinates() -> BitableCoordinates {
        BitableCoordinates {
            app_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            view_id: None,
        }
    }

    fn widget(id: &str, kind: &str) -> WidgetMap {
        WidgetMap {
            widget_id: id.to_string(),
            widget_type: kind.to_string(),
            required: false,
            bitable_field: "fld_title".to_string(),
            converter: Converter::Direct,
            option_map: BTreeMap::new(),
            currency: None,
        }
    }

    fn cells(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    /// 记录每个请求的脚本化传输 + 记录回写的假 backfill。
    struct ScriptedTransport {
        responses: Mutex<Vec<OutboundResponse>>,
        requests: Mutex<Vec<String>>,
        bodies: Mutex<Vec<Option<Value>>>,
        full_requests: Mutex<Vec<super::super::outbound::OutboundRequest>>,
    }

    impl ScriptedTransport {
        fn new(bodies: Vec<(u16, &str)>) -> Self {
            Self {
                responses: Mutex::new(
                    bodies
                        .into_iter()
                        .map(|(status, body)| OutboundResponse {
                            status,
                            body: body.to_string(),
                            bytes: None,
                            headers: BTreeMap::new(),
                        })
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
                bodies: Mutex::new(Vec::new()),
                full_requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(
            &self,
            request: super::super::outbound::OutboundRequest,
        ) -> Result<OutboundResponse, yang_base::BaseError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(format!("{:?} {}", request.method, request.url));
            }
            if let Ok(mut bodies) = self.bodies.lock() {
                bodies.push(request.json_body.clone());
            }
            if let Ok(mut requests) = self.full_requests.lock() {
                requests.push(request.clone());
            }
            let mut responses = self
                .responses
                .lock()
                .map_err(|error| yang_base::BaseError::ConfigError(error.to_string()))?;
            if responses.is_empty() {
                return Err(yang_base::BaseError::ConfigError("脚本已耗尽".to_string()));
            }
            Ok(responses.remove(0))
        }
    }

    struct NoSleep;
    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration: std::time::Duration) {}
    }

    /// 假 token 缓存：这些用例不碰真实网络，预置一个常量凭证。
    ///
    /// 用 `TenantTokenProvider` 的真实构造而不是伪造它——后者不是 trait，
    /// 而是带缓存/锁逻辑的**结构体**，伪造它会绕开「token 失效补救」那段真实逻辑，
    /// 而那段逻辑正是本模块依赖的。
    struct FakeCache;

    #[async_trait::async_trait]
    impl TenantTokenCache for FakeCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("t-fake".to_string()))
        }
        async fn put(&self, _token: &str, _ttl_seconds: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn invalidate(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn acquire_lock(&self, _owner: &str, _ttl_seconds: i64) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn release_lock(&self, _owner: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn fake_tokens() -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(FakeCache),
            Arc::new(NullTransport),
            Arc::new(NoSleep),
            FeishuCredentials {
                app_id: "cli_fake".to_string(),
                app_secret: "secret".to_string(),
            },
            "test",
        )
        .unwrap_or_else(|error| panic!("应可构造 token provider: {error}"))
    }

    #[test]
    fn approval_link_encodes_both_paths_and_rejects_query_injection() {
        let link =
            super::super::approval::instance_applink("INST-A").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(link, "https://applink.feishu.cn/client/mini_program/open?appId=cli_9cb844403dbb9108&mode=appCenter&path_pc=pc%2Fpages%2Fin-process%2Findex%3FinstanceId%3DINST-A%26source%3Dbitable&path=pages%2Fdetail%2Findex%3FinstanceId%3DINST-A%26source%3Dbitable");
        for invalid in ["", "x&injected=y", "bad/id", "a%26b", "中文"] {
            assert!(super::super::approval::instance_applink(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn approval_link_native_cells_and_text_compatibility() {
        for kind in [Some(15), Some(1), None, Some(2)] {
            for coord in ["fldResult", "改名后的申请编号"] {
                let transport = ScriptedTransport::new(vec![(200, r#"{"code":0,"data":{}}"#)]);
                let fields = [FieldItem {
                    field_id: "fldResult".into(),
                    field_name: "改名后的申请编号".into(),
                    field_type: kind,
                    ui_type: None,
                    property: None,
                }];
                let coords = coordinates();
                let tokens = fake_tokens();
                let writer = BitableBackfill {
                    transport: &transport,
                    sleeper: &NoSleep,
                    tokens: &tokens,
                    coordinates: &coords,
                    fields: &fields,
                };
                let value = json!({"text":"SERIAL","link":"https://example.com"});
                let result = writer
                    .write_many(
                        coord,
                        &[
                            ("recOne".into(), value.clone()),
                            ("recTwo".into(), value.clone()),
                        ],
                    )
                    .await;
                assert_eq!(result.is_ok(), kind != Some(2));
                if kind == Some(2) {
                    assert!(transport.requests().is_empty());
                    continue;
                }
                let expected = if kind == Some(15) {
                    value
                } else {
                    json!("SERIAL")
                };
                let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(
                    bodies[0].as_ref().map(|v| v["records"].clone()),
                    Some(
                        json!([{"record_id":"recOne","fields":{"改名后的申请编号":expected}},{"record_id":"recTwo","fields":{"改名后的申请编号":expected}}])
                    )
                );
            }
        }
    }

    #[tokio::test]
    async fn approval_link_errors_keep_the_cell_empty_and_missing_columns_fail() {
        let transport = ScriptedTransport::new(vec![]);
        let fields = [FieldItem {
            field_id: "fldResult".into(),
            field_name: "申请编号".into(),
            field_type: Some(15),
            ui_type: None,
            property: None,
        }];
        let coords = coordinates();
        let tokens = fake_tokens();
        let writer = BitableBackfill {
            transport: &transport,
            sleeper: &NoSleep,
            tokens: &tokens,
            coordinates: &coords,
            fields: &fields,
        };
        assert!(writer
            .write("recOne", "fldResult", &json!("错误信息"))
            .await
            .is_ok());
        assert!(writer
            .write("recOne", "missing", &json!("x"))
            .await
            .is_err());
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn approval_link_finish_preserves_instance_code_and_retries_failed_write() {
        let fields = [FieldItem {
            field_id: "fldResult".into(),
            field_name: "审批编号".into(),
            field_type: Some(15),
            ui_type: None,
            property: None,
        }];
        let cells = cells(&[]);
        let input = input(&cells, &[]);
        let tokens = fake_tokens();
        for (code, serial, response, write, success) in [
            (
                "INST-A",
                Some(" SERIAL "),
                (200, r#"{"code":0,"data":{}}"#),
                true,
                true,
            ),
            (
                "INST-A",
                Some("SERIAL"),
                (200, r#"{"code":1254068,"msg":"bad url"}"#),
                true,
                false,
            ),
            ("bad&id=x", Some("SERIAL"), (200, ""), false, false),
            ("INST-A", None, (200, ""), false, false),
        ] {
            let transport = ScriptedTransport::new(vec![response]);
            let writer = BitableBackfill {
                transport: &transport,
                sleeper: &NoSleep,
                tokens: &tokens,
                coordinates: input.coordinates,
                fields: &fields,
            };
            let result = finish_backfill(
                &writer,
                &input,
                "source-uuid",
                InstanceDetail {
                    instance_code: code.into(),
                    approval_code: None,
                    serial_number: serial.map(str::to_string),
                    status: None,
                },
            )
            .await;
            assert_eq!(matches!(result, DispatchResult::Backfilled { .. }), success);
            if !success {
                assert!(matches!(result, DispatchResult::Retryable { .. }));
            }
            assert_eq!(!transport.requests().is_empty(), write);
            if write {
                let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
                let value = &bodies[0].as_ref().unwrap_or_else(|| panic!("body missing"))
                    ["records"][0]["fields"]["审批编号"];
                assert_eq!(value["text"], "SERIAL");
                assert_eq!(
                    value["link"],
                    super::super::approval::instance_applink(code)
                        .unwrap_or_else(|e| panic!("{e}"))
                );
            }
        }
    }

    #[tokio::test]
    async fn batch_get_records_uses_the_official_endpoint() {
        let transport = ScriptedTransport::new(vec![(200, r#"{"code":0,"data":{"records":[]}}"#)]);
        super::super::bitable::get_records_by_ids(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &BitableCoordinates {
                app_token: "appTest".to_string(),
                table_id: "tblTest".to_string(),
                view_id: None,
            },
            &["recTest".to_string()],
        )
        .await
        .unwrap_or_else(|error| panic!("应可读取记录: {error}"));
        assert_eq!(transport.requests(), vec!["Post https://open.feishu.cn/open-apis/bitable/v1/apps/appTest/tables/tblTest/records/batch_get"]);
    }

    /// token provider 只会在缓存未命中时用它，而 `FakeCache` 恒命中，
    /// 所以这个传输永远不会被调用。
    struct NullTransport;

    #[async_trait::async_trait]
    impl OutboundTransport for NullTransport {
        async fn send(
            &self,
            _request: super::super::outbound::OutboundRequest,
        ) -> Result<OutboundResponse, yang_base::BaseError> {
            Err(yang_base::BaseError::ConfigError(
                "FakeCache 恒命中，不应走到换取 token".to_string(),
            ))
        }
    }

    #[derive(Default)]
    struct RecordingBackfill {
        writes: Mutex<Vec<(String, String, String)>>,
        fail: bool,
        /// 毒记录名单：这批里出现任一即让整批失败（复刻全有全无语义）。
        poison: Mutex<Vec<String>>,
    }

    impl RecordingBackfill {
        fn failing() -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
                fail: true,
                poison: Mutex::new(Vec::new()),
            }
        }

        fn with_poison(record_id: &str) -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
                fail: false,
                poison: Mutex::new(vec![record_id.to_string()]),
            }
        }

        fn writes(&self) -> Vec<(String, String, String)> {
            self.writes
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl Backfill for RecordingBackfill {
        async fn write(
            &self,
            record_id: &str,
            field_id: &str,
            value: &Value,
        ) -> Result<(), OutboundFailure> {
            self.write_many(field_id, &[(record_id.to_string(), value.clone())])
                .await
        }

        async fn write_many(
            &self,
            field_id: &str,
            rows: &[(String, Value)],
        ) -> Result<(), OutboundFailure> {
            if let Ok(mut writes) = self.writes.lock() {
                for (record_id, value) in rows {
                    writes.push((
                        record_id.clone(),
                        field_id.to_string(),
                        value
                            .get("text")
                            .unwrap_or(value)
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    ));
                }
            }
            if self.fail {
                return Err(OutboundFailure {
                    kind: FailureKind::Retry {
                        retry_after_seconds: None,
                    },
                    message: "回写失败".to_string(),
                });
            }
            // 毒记录模拟：全有全无——批里出现任一毒记录即整批失败。
            if let Ok(poison) = self.poison.lock() {
                if rows.iter().any(|(record_id, _)| poison.contains(record_id)) {
                    return Err(OutboundFailure {
                        kind: FailureKind::Fatal { code: 1254060 },
                        message: "字段转换失败（毒记录在批内）".to_string(),
                    });
                }
            }
            Ok(())
        }
    }

    fn input<'a>(
        cells: &'a serde_json::Map<String, Value>,
        widgets: &'a [WidgetMap],
    ) -> DispatchInput<'a> {
        DispatchInput {
            context: None,
            coordinates: &COORDINATES,
            record_id: "rec001",
            cells,
            applicant_field: "fld_applicant",
            backfill_field_name: "审批编号",
            approval_code: "4202AD96-9EC1",
            widgets,
            form_snapshot: serde_json::to_string(
                &widgets
                    .iter()
                    .map(|w| {
                        json!({
                            "id": w.widget_id, "type": w.widget_type, "required": w.required
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|e| panic!("{e}")),
            timezone_offset: FixedOffset::from_seconds(8 * 3600)
                .unwrap_or_else(|error| panic!("{error}")),
        }
    }

    // `input()` 需要一个活得够久的坐标，用 `Box::leak` 在测试里换静态生命周期。
    static COORDINATES: std::sync::LazyLock<BitableCoordinates> =
        std::sync::LazyLock::new(coordinates);

    fn ok_create(instance_code: &str) -> String {
        format!(r#"{{"code":0,"msg":"ok","data":{{"instance_code":"{instance_code}"}}}}"#)
    }

    fn ok_detail(instance_code: &str, serial: &str) -> String {
        format!(
            r#"{{"code":0,"msg":"ok","data":{{"instance_code":"{instance_code}","serial_number":"{serial}","status":"PENDING"}}}}"#
        )
    }

    fn uuid_conflict() -> String {
        r#"{"code":60012,"msg":"uuid conflict"}"#.to_string()
    }

    #[tokio::test]
    async fn connect_dispatch_validates_then_submits_official_array_including_details() {
        let code = "19EAC829-F1CB-527F-BE2A-1330422E60C0";
        for detail in [false, true] {
            let node = json!({"id":"w1","name":"关联审批","type":"connect","value":["DEF-A"]});
            let snapshot = if detail {
                json!([{"id":"detail","type":"fieldList","children":[node]}])
            } else {
                json!([node])
            }
            .to_string();
            let linked =
                json!({"code":0,"data":{"instance_code":code,"approval_code":"def-a"}}).to_string();
            let transport = ScriptedTransport::new(vec![
                (400, r#"{"code":1390003}"#),
                (200, &linked),
                (200, &ok_create("CREATED")),
                (200, &ok_detail("CREATED", "SERIAL")),
            ]);
            let widgets = [widget("w1", "connect")];
            let cell_map = cells(&[
                ("fld_applicant", json!([{"id":"ou_test"}])),
                (
                    "fld_title",
                    json!({"link":super::super::approval::instance_applink(code).unwrap_or_else(|e| panic!("{e}")),"text":"申请编号"}),
                ),
            ]);
            let mut request = input(&cell_map, &widgets);
            request.form_snapshot = snapshot;
            let backfill = RecordingBackfill::default();
            assert!(matches!(
                dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await,
                DispatchResult::Backfilled { .. }
            ));
            let requests = transport
                .full_requests
                .lock()
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(requests.len(), 4);
            assert!(requests[1].url.ends_with(code));
            let body = requests[2]
                .json_body
                .as_ref()
                .unwrap_or_else(|| panic!("缺创建请求"));
            let form: Value = serde_json::from_str(body["form"].as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
            let connection = if detail {
                &form[0]["value"][0][0]
            } else {
                &form[0]
            };
            assert_eq!(
                connection,
                &json!({"id":"w1","type":"connect","value":[code]})
            );
            assert_eq!(backfill.writes().len(), 1);
        }
    }

    #[tokio::test]
    async fn connect_invalid_instances_never_create_or_write_and_existing_uuid_recovers() {
        let code = "19EAC829-F1CB-527F-BE2A-1330422E60C0";
        for (status, response, retryable) in [
            (400, json!({"code":1390003}), false),
            (
                403,
                json!({"code":99991672,"msg":"permission denied"}),
                true,
            ),
            (
                200,
                json!({"code":0,"data":{"instance_code":code,"approval_code":"DEF-B"}}),
                false,
            ),
            (
                200,
                json!({"code":0,"data":{"instance_code":"OTHER","approval_code":"DEF-A"}}),
                true,
            ),
            (200, json!({"code":0,"data":{"instance_code":code}}), true),
        ] {
            let response = response.to_string();
            let transport =
                ScriptedTransport::new(vec![(400, r#"{"code":1390003}"#), (status, &response)]);
            let widgets = [widget("w1", "connect")];
            let cell_map = cells(&[
                ("fld_applicant", json!([{"id":"ou_test"}])),
                ("fld_title", json!(code)),
            ]);
            let mut request = input(&cell_map, &widgets);
            request.form_snapshot = r#"[{"id":"w1","name":"关联审批","type":"connect","option":{"definitionIds":["DEF-A"],"notLimitScope":false}}]"#.to_string();
            let backfill = RecordingBackfill::default();
            let result =
                dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;
            assert!(
                if retryable {
                    matches!(result, DispatchResult::Retryable { .. })
                } else {
                    matches!(result, DispatchResult::Waiting { .. })
                },
                "{result:?}"
            );
            assert_eq!(transport.requests().len(), 2);
            assert!(backfill.writes().is_empty());
        }
        let transport = ScriptedTransport::new(vec![(200, &ok_detail("EXISTING", "SERIAL"))]);
        let widgets = [widget("w1", "connect")];
        let backfill = RecordingBackfill::default();
        assert!(matches!(
            dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &backfill,
                &input(&cells(&[]), &widgets)
            )
            .await,
            DispatchResult::Backfilled { .. }
        ));
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(backfill.writes().len(), 1);
    }

    #[tokio::test]
    async fn connect_unlimited_scope_and_dedup_still_enforce_each_detail_widget_scope() {
        let code = "19EAC829-F1CB-527F-BE2A-1330422E60C0";
        let response =
            json!({"code":0,"data":{"instance_code":code,"approval_code":"DEF-A"}}).to_string();
        for allowed in ["DEF-A", "DEF-B"] {
            let snapshot = json!([
                {"id":"top","type":"connect","option":{"notLimitScope":true}},
                {"id":"detail","type":"fieldList","children":[{"id":"child","type":"connect","value":[allowed]}]}
            ]).to_string();
            let form = json!([
                {"id":"top","type":"connect","value":[code]},
                {"id":"detail","type":"fieldList","value":[[{"id":"child","type":"connect","value":[code]}]]}
            ]).to_string();
            let transport = ScriptedTransport::new(vec![(200, &response)]);
            let result =
                validate_connections(&transport, &NoSleep, &fake_tokens(), &snapshot, &form).await;
            if allowed == "DEF-A" {
                assert_eq!(result, Ok(()));
            } else {
                assert!(matches!(result, Err(DispatchResult::Waiting { .. })));
            }
            assert_eq!(transport.requests().len(), 1, "同一实例只查询一次");
        }
    }

    fn attachment_cells() -> serde_json::Map<String, Value> {
        cells(&[
            ("fld_applicant", json!([{"id":"ou_test"}])),
            (
                "fld_src",
                json!([
                    {"file_token":"TokenA", "name":"凭证.png", "size":3, "url":"https://untrusted.invalid"},
                    {"file_token":"TokenB", "name":"凭证2.pdf", "size":3}
                ]),
            ),
        ])
    }

    fn attachment_widget() -> WidgetMap {
        WidgetMap {
            bitable_field: "fld_src".to_string(),
            required: true,
            ..widget("w1", "attachmentV2")
        }
    }

    fn attachment_transport() -> ScriptedTransport {
        let transport = ScriptedTransport::new(vec![
            (400, r#"{"code":1390003}"#),
            (200, ""),
            (200, r#"{"code":0,"data":{"code":"CODE-A"}}"#),
            (200, ""),
            (200, r#"{"code":0,"data":{"code":"CODE-B"}}"#),
            (200, &ok_create("INST-A")),
            (200, &ok_detail("INST-A", "SERIAL-A")),
        ]);
        let mut responses = transport.responses.lock().unwrap_or_else(|e| panic!("{e}"));
        responses[1].bytes = Some(vec![0, 255, 3]);
        responses[3].bytes = Some(vec![0, 255, 3]);
        drop(responses);
        transport
    }

    #[tokio::test]
    async fn attachment_transfer_reaches_plain_and_detail_create_requests() {
        for detail in [false, true] {
            let transport = attachment_transport();
            let backfill = RecordingBackfill::default();
            let widgets = [attachment_widget()];
            let cells = attachment_cells();
            let mut input = input(&cells, &widgets);
            if detail {
                input.form_snapshot = r#"[{"id":"detail","type":"fieldList","children":[{"id":"w1","type":"attachmentV2","required":true}]}]"#.to_string();
            }
            assert_eq!(
                dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &input).await,
                DispatchResult::Backfilled {
                    serial_number: "SERIAL-A".to_string()
                }
            );
            let requests = transport
                .full_requests
                .lock()
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(requests.len(), 7);
            assert!(requests[1].url.ends_with("/medias/TokenA/download"));
            let extra: Value =
                serde_json::from_str(&requests[1].query[0].1).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                extra,
                json!({"bitablePerm":{"tableId":COORDINATES.table_id,"attachments":{"fld_src":{"rec001":["TokenA"]}}}})
            );
            for i in [2, 4] {
                assert_eq!(
                    requests[i].url,
                    "https://www.feishu.cn/approval/openapi/v2/file/upload"
                );
                assert!(!requests[i].idempotent);
                assert_eq!(requests[i].bearer_token.as_deref(), Some("t-fake"));
                let (content_type, body) = requests[i]
                    .raw_body
                    .as_ref()
                    .unwrap_or_else(|| panic!("缺 multipart"));
                let boundary = content_type
                    .strip_prefix("multipart/form-data; boundary=")
                    .unwrap_or_else(|| panic!("缺 boundary"));
                assert!(body.starts_with(format!("--{boundary}\r\n").as_bytes()));
                assert!(body.windows(3).any(|b| b == [0, 255, 3]));
                let text = String::from_utf8_lossy(body);
                assert!(text.contains("name=\"type\"\r\n\r\nattachment"));
                assert!(text.contains("name=\"content\"; filename=\"凭证"));
            }
            let body = requests[5]
                .json_body
                .as_ref()
                .unwrap_or_else(|| panic!("缺创建请求"));
            let form: Value = serde_json::from_str(
                body["form"]
                    .as_str()
                    .unwrap_or_else(|| panic!("form 必须是字符串")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let attachment = if detail {
                &form[0]["value"][0][0]
            } else {
                &form[0]
            };
            assert_eq!(
                attachment,
                &json!({"id":"w1","type":"attachmentV2","value":["CODE-A","CODE-B"]})
            );
            assert!(!body["form"].as_str().unwrap_or_default().contains("TokenA"));
            assert_eq!(backfill.writes().len(), 1);
        }
    }

    #[tokio::test]
    async fn attachment_existing_uuid_backfills_even_after_source_was_removed() {
        let transport = ScriptedTransport::new(vec![(200, &ok_detail("EXISTING", "SERIAL"))]);
        let backfill = RecordingBackfill::default();
        let widgets = [attachment_widget()];
        assert_eq!(
            dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &backfill,
                &input(&cells(&[]), &widgets)
            )
            .await,
            DispatchResult::Backfilled {
                serial_number: "SERIAL".to_string()
            }
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(backfill.writes().len(), 1);
    }

    #[tokio::test]
    async fn attachment_download_rate_limit_retries_then_creates_once() {
        let transport = attachment_transport();
        transport
            .responses
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .insert(
                1,
                OutboundResponse {
                    status: 429,
                    body: String::new(),
                    bytes: None,
                    headers: BTreeMap::from([(
                        "x-ogw-ratelimit-reset".to_string(),
                        "1".to_string(),
                    )]),
                },
            );
        let widgets = [attachment_widget()];
        assert!(matches!(
            dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &RecordingBackfill::default(),
                &input(&attachment_cells(), &widgets)
            )
            .await,
            DispatchResult::Backfilled { .. }
        ));
        let requests = transport.requests();
        assert_eq!(requests.len(), 8);
        assert_eq!(requests[1], requests[2]);
    }

    #[tokio::test]
    async fn attachment_expired_token_refreshes_once_and_uses_returned_token() {
        let refresh = Arc::new(ScriptedTransport::new(vec![(
            200,
            r#"{"code":0,"tenant_access_token":"t-fresh","expire":7200}"#,
        )]));
        let tokens = TenantTokenProvider::new(
            Arc::new(FakeCache),
            refresh.clone(),
            Arc::new(NoSleep),
            FeishuCredentials {
                app_id: "cli_fake".to_string(),
                app_secret: "secret".to_string(),
            },
            "test",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for repeated in [false, true] {
            let transport = attachment_transport();
            {
                let mut responses = transport.responses.lock().unwrap_or_else(|e| panic!("{e}"));
                responses.insert(
                    1,
                    OutboundResponse {
                        status: 400,
                        body: r#"{"code":99991663}"#.to_string(),
                        bytes: None,
                        headers: BTreeMap::new(),
                    },
                );
                if repeated {
                    responses.insert(
                        2,
                        OutboundResponse {
                            status: 400,
                            body: r#"{"code":99991663}"#.to_string(),
                            bytes: None,
                            headers: BTreeMap::new(),
                        },
                    );
                }
            }
            // FakeCache 保留旧 token，证明重发直接使用 refresh 的返回值。
            if repeated {
                refresh
                    .responses
                    .lock()
                    .unwrap_or_else(|e| panic!("{e}"))
                    .push(OutboundResponse {
                        status: 200,
                        body: r#"{"code":0,"tenant_access_token":"t-fresh","expire":7200}"#
                            .to_string(),
                        bytes: None,
                        headers: BTreeMap::new(),
                    });
            }
            let widgets = [attachment_widget()];
            let backfill = RecordingBackfill::default();
            let result = dispatch_one(
                &transport,
                &NoSleep,
                &tokens,
                &backfill,
                &input(&attachment_cells(), &widgets),
            )
            .await;
            assert!(if repeated {
                matches!(result, DispatchResult::Retryable { .. })
            } else {
                matches!(result, DispatchResult::Backfilled { .. })
            });
            assert_eq!(
                transport
                    .full_requests
                    .lock()
                    .unwrap_or_else(|e| panic!("{e}"))[2]
                    .bearer_token
                    .as_deref(),
                Some("t-fresh")
            );
        }
        assert_eq!(refresh.requests().len(), 2);
    }

    #[tokio::test]
    async fn attachment_failure_never_creates_or_writes_terminal_marker() {
        for (index, status, body, bytes) in [
            (0, 403, "denied", None),
            (1, 403, "denied", None),
            (1, 206, "", Some(vec![0, 255, 3])),
            (1, 200, "", Some(vec![0])),
            (2, 200, r#"{"code":0,"data":{"url":"unusable"}}"#, None),
            (2, 200, r#"{"data":{"code":"CODE-A"}}"#, None),
            (2, 200, r#"{"code":0,"data":{"code":" "}}"#, None),
            (2, 403, "denied", None),
            (4, 503, "upload failed", None),
        ] {
            let transport = attachment_transport();
            {
                let mut responses = transport.responses.lock().unwrap_or_else(|e| panic!("{e}"));
                responses[index] = OutboundResponse {
                    status,
                    body: body.to_string(),
                    bytes,
                    headers: BTreeMap::new(),
                };
            }
            let backfill = RecordingBackfill::default();
            let widgets = [attachment_widget()];
            assert!(matches!(
                dispatch_one(
                    &transport,
                    &NoSleep,
                    &fake_tokens(),
                    &backfill,
                    &input(&attachment_cells(), &widgets)
                )
                .await,
                DispatchResult::Retryable { .. }
            ));
            assert_eq!(transport.requests().len(), index + 1);
            assert!(backfill.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn attachment_validation_precedes_download_and_empty_optional_is_omitted() {
        for value in [
            Value::Null,
            json!([]),
            json!("raw-token"),
            json!([{"file_token":"TokenA","name":"ok.png","size":0}]),
        ] {
            let transport = ScriptedTransport::new(vec![(400, r#"{"code":1390003}"#)]);
            let backfill = RecordingBackfill::default();
            let widgets = [attachment_widget()];
            let mut cells = attachment_cells();
            cells.insert("fld_src".to_string(), value);
            assert!(matches!(
                dispatch_one(
                    &transport,
                    &NoSleep,
                    &fake_tokens(),
                    &backfill,
                    &input(&cells, &widgets)
                )
                .await,
                DispatchResult::Waiting { .. }
            ));
            assert_eq!(transport.requests().len(), 1);
            assert!(backfill.writes().is_empty());
        }
        let transport = ScriptedTransport::new(vec![
            (400, r#"{"code":1390003}"#),
            (200, &ok_create("INST")),
            (200, &ok_detail("INST", "SERIAL")),
        ]);
        let mut widgets = [attachment_widget()];
        widgets[0].required = false;
        let cells = cells(&[("fld_applicant", json!([{"id":"ou_test"}]))]);
        assert!(approval_attachment::transfer(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &COORDINATES,
            "rec001",
            &widgets,
            &cells
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"))
        .is_empty());
        assert!(matches!(
            dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &RecordingBackfill::default(),
                &input(&cells, &widgets)
            )
            .await,
            DispatchResult::Backfilled { .. }
        ));
        let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            bodies[1].as_ref().unwrap_or_else(|| panic!("缺请求"))["form"],
            "[]"
        );
    }

    #[tokio::test]
    async fn detail_snapshot_reaches_the_create_request_and_legacy_config_fetches_it() {
        let snapshot = r#"[{"id":"detail","type":"fieldList","required":true,"children":[{"id":"w1","type":"input","required":true}]}]"#;
        // 历史快照可空，定义 API 的 form 可为字符串或数组；三条路都不能扁平提交。
        for remote_form in [
            None,
            Some(json!(snapshot)),
            Some(serde_json::from_str::<Value>(snapshot).unwrap_or_else(|e| panic!("{e}"))),
        ] {
            let definition =
                json!({"code":0,"data":{"approval_name":"测试","form":remote_form}}).to_string();
            let mut responses = vec![
                (200, ok_create("INST-D")),
                (200, ok_detail("INST-D", "202610090001")),
            ];
            if remote_form.is_some() {
                responses.insert(0, (200, definition));
            }
            let transport = ScriptedTransport::new(
                responses
                    .iter()
                    .map(|(status, body)| (*status, body.as_str()))
                    .collect(),
            );
            let backfill = RecordingBackfill::default();
            let widgets = [widget("w1", "input")];
            let cells = cells(&[
                ("fld_applicant", json!([{"id":"ou_abc"}])),
                ("fld_title", json!("明细内容")),
            ]);
            let mut request = input(&cells, &widgets);
            request.form_snapshot = if remote_form.is_some() {
                String::new()
            } else {
                snapshot.to_string()
            };
            let result =
                dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;
            assert!(
                matches!(result, DispatchResult::Backfilled { .. }),
                "{result:?}"
            );
            let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
            let body = bodies[usize::from(remote_form.is_some())]
                .as_ref()
                .unwrap_or_else(|| panic!("创建请求缺少 body"));
            let form: Value = serde_json::from_str(
                body["form"]
                    .as_str()
                    .unwrap_or_else(|| panic!("form 必须为字符串")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                form,
                json!([{"id":"detail","type":"fieldList","value":[[{"id":"w1","type":"input","value":"明细内容"}]]}])
            );
            assert_eq!(body["open_id"], json!("ou_abc"));
            assert!(body["uuid"].as_str().is_some_and(|uuid| !uuid.is_empty()));
        }
    }

    #[tokio::test]
    async fn corrupt_snapshot_is_terminal_without_creating_an_instance() {
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id":"ou_abc"}])),
            ("fld_title", json!("测试")),
        ]);
        let mut request = input(&cells, &widgets);
        request.form_snapshot = "broken".to_string();
        let result = dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;
        assert!(
            matches!(result, DispatchResult::Terminal { .. }),
            "{result:?}"
        );
        assert!(transport.requests().is_empty());
        assert_eq!(backfill.writes().len(), 1);
    }

    #[tokio::test]
    async fn happy_path_creates_then_reads_serial_number_then_backfills() {
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-1")),
            (200, &ok_detail("INST-1", "202609280001")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "202609280001".to_string()
            }
        );
        assert_eq!(
            backfill.writes(),
            vec![(
                "rec001".to_string(),
                // `batch_update` 的 `fields` 按**列名**作键，所以域里收的是列名而不是
                // `field_id`（解析由调用方在拿到表结构后做，见 `DispatchInput` 的说明）。
                "审批编号".to_string(),
                "202609280001".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn uuid_conflict_reclaims_via_uuid_and_backfills() {
        // 核心路径：create 返回 60012 → 按 uuid 反查 → 拿到 instance_code 与
        // serial_number → 回填编号。**绝不把 60012 写成错误文案。**
        let transport = ScriptedTransport::new(vec![
            (400, &uuid_conflict()),
            (200, &ok_detail("INST-RECOVERED", "202609280042")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "202609280042".to_string()
            },
            "60012 必须走回捞成功，而不是终态"
        );
        // 反查必须用 uuid（第二个请求的 URL 里是派生的 uuid，不是 instance_code）。
        let requests = transport.requests();
        let uuid = derive_uuid("appbcbWCzen6", "tblsRc9GRRX", "4202AD96-9EC1", "rec001");
        assert!(requests[1].contains(&uuid), "反查必须按 uuid：{requests:?}");
        // 只写了一次，内容是编号（不是错误文案）。
        assert_eq!(backfill.writes().len(), 1);
        assert_eq!(backfill.writes()[0].2, "202609280042");
    }

    #[tokio::test]
    async fn uuid_conflict_then_not_found_is_retryable_not_terminal() {
        // 并发窗口：冲突成立但实例还没可见。回到可重试，**不落终态**——
        // 落终态会把可能存在的实例判死。
        let transport = ScriptedTransport::new(vec![
            (400, &uuid_conflict()),
            (400, r#"{"code":1390003,"msg":"instance code not found"}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Retryable { .. }),
            "应为可重试，实际 {result:?}"
        );
        assert!(
            backfill.writes().is_empty(),
            "可重试不得写任何字段：{:#?}",
            backfill.writes()
        );
    }

    #[tokio::test]
    async fn missing_applicant_waits_without_writing_anything() {
        // 用户先填业务字段、后填申请人的那一轮：不能写字段，否则该行被永久钉死。
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_title", json!("报销"))]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Waiting { .. }),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty(), "等待态不得写字段");
        assert!(transport.requests().is_empty(), "等待态不应打任何飞书接口");
    }

    #[tokio::test]
    async fn missing_required_widget_waits_without_writing_anything() {
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let mut required = widget("w1", "input");
        required.required = true;
        let widgets = [required];
        let cells = cells(&[("fld_applicant", json!([{"id": "ou_abc"}]))]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Waiting { .. }),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty(), "等待态不得写字段");
    }

    #[tokio::test]
    async fn unsupported_widget_is_terminal() {
        // 配置问题（不是数据问题）：不补数据就能过，反复重扫永远好不了，
        // 必须写字段让用户看见。
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "tripGroup")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("出差")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Terminal { .. }),
            "{result:?}"
        );
        assert_eq!(backfill.writes().len(), 1, "终态要写回填字段");
        assert!(
            backfill.writes()[0].2.contains("不支持"),
            "文案应说明是控件不支持：{}",
            backfill.writes()[0].2
        );
    }

    #[tokio::test]
    async fn serial_number_missing_after_create_is_retryable() {
        // 文档未承诺创建后编号立即可查。判可重试，绝不写终态——否则刚建成的
        // 实例会被写成「找不到」。
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-2")),
            (
                200,
                r#"{"code":0,"msg":"ok","data":{"instance_code":"INST-2","status":"PENDING"}}"#,
            ),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Retryable { .. }),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty(), "可重试不得写字段");
    }

    #[tokio::test]
    async fn instance_not_found_right_after_create_is_retryable() {
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-3")),
            (400, r#"{"code":1390003,"msg":"instance code not found"}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Retryable { .. }),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty());
    }

    #[tokio::test]
    async fn approval_domain_transient_error_is_retryable() {
        // 1395001 走审批域可重试表（Task 1 的扩表）。`send_with_retry` 按
        // `PULL_RETRY`（max_attempts = 3）会真的重试，所以脚本要给足响应——
        // 给一个会得到「脚本已耗尽」，那是测试脚手架的问题，不是被测行为的问题。
        let transport = ScriptedTransport::new(vec![
            (
                400,
                r#"{"code":1395001,"msg":"there have been some errors"}"#,
            ),
            (
                400,
                r#"{"code":1395001,"msg":"there have been some errors"}"#,
            ),
            (
                400,
                r#"{"code":1395001,"msg":"there have been some errors"}"#,
            ),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Retryable { .. }),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty(), "可重试不得写字段");
        assert_eq!(
            transport.requests().len(),
            3,
            "1395001 应真的重试到 max_attempts"
        );
    }

    #[tokio::test]
    async fn terminal_failure_message_is_carried_too() {
        // 1390001（表单控件参数错误）落终态，写回的文案带码可搜索。
        let transport =
            ScriptedTransport::new(vec![(400, r#"{"code":1390001,"msg":"param is invalid"}"#)]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Terminal { .. }),
            "{result:?}"
        );
        let written = &backfill.writes()[0].2;
        assert!(written.contains("1390001"), "文案带码便于搜索：{written}");
    }

    #[tokio::test]
    async fn optional_widget_without_value_still_succeeds() {
        // 可选控件无值 → 整个控件 JSON 不传，不是错误。
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-4")),
            (200, &ok_detail("INST-4", "202609280099")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!([{"id": "ou_abc"}]))]);

        let tokens = fake_tokens();
        let result = dispatch_one(
            &transport,
            &NoSleep,
            &tokens,
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "无值可选控件不应阻塞：{result:?}"
        );
    }

    #[test]
    fn applicant_accepts_object_array_and_plain_string() {
        assert_eq!(
            applicant_open_id(&cells(&[("f", json!([{"id": "ou_x"}]))]), "f").as_deref(),
            Some("ou_x")
        );
        // 历史数据可能是裸字符串。
        assert_eq!(
            applicant_open_id(&cells(&[("f", json!("ou_y"))]), "f").as_deref(),
            Some("ou_y")
        );
        // 空格串等同缺失。
        assert_eq!(applicant_open_id(&cells(&[("f", json!("  "))]), "f"), None);
        assert_eq!(applicant_open_id(&cells(&[("f", json!([]))]), "f"), None);
        assert_eq!(applicant_open_id(&cells(&[]), "f"), None);
    }

    #[test]
    fn sanitize_keeps_the_code_and_drops_the_raw_body() {
        // 多维表格的读者范围远大于运维，原样回灌可能泄漏 request_id / 租户信息。
        let raw = r#"{"code":1390001,"msg":"param is invalid","request_id":"req_secret"}"#;
        let sanitized = sanitize_terminal_message(raw, 1390001);
        assert!(sanitized.contains("1390001"));
        assert!(!sanitized.contains("req_secret"), "{sanitized}");
    }

    #[test]
    fn widget_maps_reject_unknown_converter() {
        // 配置损坏时不猜默认转换器：静默退化成 direct 会把日期或选项值原样送出。
        let rows = vec![serde_json::Map::from_iter([
            ("widget_id".to_string(), json!("w1")),
            ("widget_type".to_string(), json!("input")),
            ("bitable_field".to_string(), json!("fld")),
            ("converter".to_string(), json!("typo")),
        ])];
        assert!(widget_maps_from_rows(&rows, |_| None).is_none());
    }

    #[test]
    fn widget_maps_parse_option_map() {
        let rows = vec![serde_json::Map::from_iter([
            ("widget_id".to_string(), json!("w1")),
            ("widget_type".to_string(), json!("radioV2")),
            ("bitable_field".to_string(), json!("fld")),
            ("converter".to_string(), json!("option")),
            ("required".to_string(), json!(true)),
            ("option_map".to_string(), json!(r#"{"已批准":"va"}"#)),
        ])];
        let widgets = widget_maps_from_rows(&rows, |_| None).unwrap_or_else(|| panic!("应可解析"));
        assert_eq!(widgets.len(), 1);
        assert!(widgets[0].required);
        assert_eq!(
            widgets[0].option_map.get("已批准").map(String::as_str),
            Some("va")
        );
    }

    // ---- 分批回写与毒记录隔离（Task 8） ----

    fn rows(count: usize) -> Vec<(String, Value)> {
        (0..count)
            .map(|index| (format!("rec{index:04}"), json!(format!("SN{index:04}"))))
            .collect()
    }

    #[tokio::test]
    async fn small_batch_is_written_in_one_call() {
        let backfill = RecordingBackfill::default();
        let report = backfill_in_chunks(&backfill, "fld_backfill", &rows(50)).await;
        assert_eq!(report.written.len(), 50);
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn batch_larger_than_chunk_is_split() {
        // 250 条应拆成 3 个子批（100/100/50），绝不一次提交 500。
        let backfill = RecordingBackfill::default();
        let report = backfill_in_chunks(&backfill, "fld_backfill", &rows(250)).await;
        assert_eq!(report.written.len(), 250);
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn poison_record_is_isolated_and_the_rest_land() {
        // 全有全无：一条毒记录会让整批失败。折半拆批要能把它单独揪出来，
        // 其余记录照常落库——否则一次坏回写会丢掉整批结果，而审批实例
        // **已经创建**，等于批量制造孤儿实例。
        let backfill = RecordingBackfill::with_poison("rec0007");
        let report = backfill_in_chunks(&backfill, "fld_backfill", &rows(20)).await;

        assert_eq!(report.failed.len(), 1, "应只定位到一条毒记录：{report:?}");
        assert_eq!(report.failed[0].0, "rec0007");
        assert_eq!(report.written.len(), 19, "其余记录必须照常落库");
        assert!(
            !report.written.contains(&"rec0007".to_string()),
            "毒记录不得出现在成功列表里"
        );
    }

    #[tokio::test]
    async fn poison_record_does_not_block_later_chunks() {
        // 毒记录不得楔住整批：它落在第一个子批里时，后续子批照常处理。
        let backfill = RecordingBackfill::with_poison("rec0003");
        let report = backfill_in_chunks(&backfill, "fld_backfill", &rows(250)).await;
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "rec0003");
        assert_eq!(
            report.written.len(),
            249,
            "毒记录之后的记录仍须写入：{report:?}"
        );
    }

    #[tokio::test]
    async fn whole_batch_failure_yields_all_failed_not_panic() {
        // 整批失败（如网络断）时，每条都被逐个判定，不 panic、不静默丢记录。
        let backfill = RecordingBackfill::failing();
        let report = backfill_in_chunks(&backfill, "fld_backfill", &rows(5)).await;
        assert!(report.written.is_empty());
        assert_eq!(report.failed.len(), 5, "每条都要有归宿：{report:?}");
    }

    #[tokio::test]
    async fn empty_batch_writes_nothing() {
        let backfill = RecordingBackfill::default();
        let report = backfill_in_chunks(&backfill, "fld_backfill", &[]).await;
        assert!(report.written.is_empty());
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn chunk_size_boundary_is_exact() {
        // 恰好一个子批时不应触发拆批；多一条则应拆成两批。
        let exact = RecordingBackfill::default();
        let report = backfill_in_chunks(
            &exact,
            "fld_backfill",
            &rows(crate::addon::feishu::domain::bitable::BACKFILL_CHUNK),
        )
        .await;
        assert_eq!(
            report.written.len(),
            crate::addon::feishu::domain::bitable::BACKFILL_CHUNK
        );

        let over = RecordingBackfill::default();
        let report = backfill_in_chunks(
            &over,
            "fld_backfill",
            &rows(crate::addon::feishu::domain::bitable::BACKFILL_CHUNK + 1),
        )
        .await;
        assert_eq!(
            report.written.len(),
            crate::addon::feishu::domain::bitable::BACKFILL_CHUNK + 1
        );
    }
}
