//! 审批实例派发的单条编排。
//!
//! 把「一条多维表格记录」变成「一个审批实例 + 回填的编号」的全过程。
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

use serde_json::{Map, Value};

use super::approval::{
    create_instance, get_approval_definition, get_instance, resolve_department_name, CreateOutcome,
    GetOutcome, InstanceDetail,
};
use super::approval_attachment;
use super::approval_convert::{
    build_form, build_group_form, connection_codes, is_empty, relation_record_ids, Converter,
    FixedOffset, FormInput, GroupFormInput, GroupFormRow, WidgetMap,
};
use super::approval_uuid::derive_uuid;
use super::bitable::{get_records_by_ids, rekey_cells_by_field_id, BitableCoordinates, FieldItem};
use super::outbound::{FailureKind, OutboundFailure, OutboundTransport, Sleeper};
use super::tenant_token::TenantTokenProvider;
use super::widget_rules;

/// 单条处理的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchResult {
    /// 创建并回填成功。
    Backfilled {
        serial_number: String,
        /// 实例 code。worker 用它落 `feishu_approval_task.instance_code`（§12 缺口 10），
        /// 二期事件追踪也靠它反查回填行。
        instance_code: String,
    },
    /// 数据不完整，本轮跳过。
    ///
    /// **不写任何多维表格字段**——用户先填业务字段、后填「申请人」的那一轮若被
    /// 写成终态错误，该行会因字段非空而永久不再被扫到。详见设计 §6.3。
    Waiting { reason: String },
    /// 终态失败；文本列写原因，链接列仅由任务/请求日志保存。
    Terminal { message: String },
    /// 实例已确认存在，编号或回填尚未完成；保留事实与重试资格。
    CreatedPendingBackfill {
        message: String,
        instance_code: String,
        serial_number: Option<String>,
    },
    /// 可重试失败，未写任何字段，等下一轮或退避后再来。
    Retryable { message: String },
}

impl DispatchResult {
    pub(crate) fn feedback_status(&self) -> super::approval_feedback::FeedbackStatus {
        use super::approval_feedback::FeedbackStatus;
        match self {
            Self::Backfilled { .. } => FeedbackStatus::Succeeded,
            Self::Waiting { .. } => FeedbackStatus::Waiting,
            Self::CreatedPendingBackfill { .. } => FeedbackStatus::CreatedPendingBackfill,
            Self::Retryable { .. } => FeedbackStatus::Retryable,
            Self::Terminal { .. } => FeedbackStatus::Failed,
        }
    }

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
            Self::Backfilled {
                serial_number,
                instance_code: _,
            } => (
                true,
                format!("已创建审批实例，编号 {serial_number}"),
                Some(serial_number.clone()),
            ),
            Self::Waiting { reason } => (true, format!("本轮未处理：{reason}"), None),
            Self::Terminal { message } => (false, message.clone(), None),
            Self::Retryable { message } => (false, format!("暂时失败，可重试：{message}"), None),
            Self::CreatedPendingBackfill { message, .. } => (false, message.clone(), None),
        }
    }
}

/// 任务没有发起人身份时的等待文案。
///
/// worker（认领后零出站早退）与编排层（`dispatch_one` 第 2 步）共用同一句：
/// 两处各写一份必然漂移，而用户看到的就是这句话。
pub(crate) const MISSING_INITIATOR_REASON: &str =
    "该任务没有发起人身份（发起人只能来自点击按钮的人）。请在表格里重新点击按钮发起。";

/// 申请人列与点击人的冲突判定。
///
/// 返回 `None` 表示**匹配**：单元格为空、不是人员形态（普通文本），或人员列表
/// 里包含点击人。返回 `Some(名字)` 表示列里明确写着别人——继续派发就是替他人
/// 发起审批，调用方必须拒绝（单条终态；批量只跳过该条）。
///
/// 先剥掉 `{type,value}` 包装：`records/batch_get` 对公式/查找字段会多包一层
/// （与 `approval_convert::unwrap_typed_cell` 同一实测形态）。不剥壳时对象会
/// 落进 `_` 兜底，被误判成「未填」，守卫就成了静默放行。
///
/// 裸 `ou_` 文本按人员 id 处理：飞书人员字段的标准形态是对象数组，裸字符串
/// 只可能来自早期数据、手工文本或被改型的列；宁可收紧（拒绝）也不把可能
/// 指向别人的 id 当成备注放行。普通文本（不以 `ou_` 开头）仍按「未填」处理。
pub(crate) fn applicant_field_conflict(
    cell: Option<&Value>,
    clicker_open_id: &str,
) -> Option<Vec<String>> {
    fn push_person(
        map: &serde_json::Map<String, Value>,
        people: &mut Vec<(String, Option<String>)>,
    ) {
        if let Some(id) = map
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            let name = map.get("name").and_then(Value::as_str).map(str::to_string);
            people.push((id.to_string(), name));
        }
    }

    let cell = super::approval_convert::unwrap_typed_cell(cell?);
    let mut people: Vec<(String, Option<String>)> = Vec::new();
    match cell {
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(map) => push_person(map, &mut people),
                    Value::String(id) if id.starts_with("ou_") => {
                        people.push((id.clone(), None));
                    }
                    _ => {}
                }
            }
        }
        // 单人对象不是 batch_get 的标准形态，但被改型/手工写入时可能出现；按人员
        // 对象处理，避免落进 `_` 兜底被误判成「未填」。
        Value::Object(map) => push_person(map, &mut people),
        Value::String(text) => {
            for part in text.split([',', '，']) {
                let part = part.trim();
                if part.starts_with("ou_") {
                    people.push((part.to_string(), None));
                }
            }
        }
        _ => {}
    }
    if people.is_empty() || people.iter().any(|(id, _)| id == clicker_open_id) {
        return None;
    }
    Some(
        people
            .into_iter()
            .map(|(id, name)| name.unwrap_or(id))
            .collect(),
    )
}

/// 单条点击**受理前**提前守卫的拒绝文案（Action 层调用）。
///
/// 单条点击的人**看得见这一行**（他点的就是它），所以可以列出列里的名字；
/// 批量扫描会扫到点击人无权查看的行，那里的通知只报条数、不报姓名。
/// `dispatch_one` 的兜底守卫可能处理批量播种的任务，用「只报条数」的另一份文案。
pub(crate) fn applicant_mismatch_message(names: &[String]) -> String {
    format!(
        "「申请人」列填写的是 {}，与本次点击人不是同一人；为避免替他人提交，本次未创建审批。请由申请人本人点击提交，或清空该列后重试。",
        names.join("、")
    )
}

/// 一条记录的处理输入。
pub(crate) struct DispatchInput<'a> {
    pub(crate) fields: &'a [FieldItem],
    pub(crate) context: Option<&'a super::context::FeishuContext>,
    pub(crate) execution: Option<&'a super::approval_execution::TaskClaim>,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) record_id: &'a str,
    /// 该记录的原始单元格（键是字段 id）。
    pub(crate) cells: &'a serde_json::Map<String, Value>,
    /// 申请人的人员字段 id —— **写目标**：创建成功后系统把点击人 open_id 写进该列。
    ///
    /// 它不再是发起人的**来源**（2026-10-31 起）：发起人只能是请求里的点击人，
    /// 读该列等于允许「谁填的列就是谁发起」，即替别人发起审批。
    pub(crate) applicant_field: &'a str,
    /// 请求传入的发起人（工作流触发人）**open_id**，派发与写回的唯一身份来源。
    ///
    /// # 上层契约（别在这里做姓名解析）
    ///
    /// 真机 `$.step_btn.user` 渲染出来的是**姓名**（设计 M11 定论），所以
    /// 「姓名 → open_id」的 contact 解析是**调用方**（dispatch Action / worker）的
    /// 职责；本字段拿到的**已是 open_id**。`None` 只可能出现在**旧任务行**上
    /// （身份落库之前的存量），编排层据此落等待态、零出站。
    pub(crate) applicant: Option<String>,
    /// 回填字段 id。
    /// 回填列的**当前列名**（不是 `field_id`）。
    ///
    /// `batch_update` 的 `records[].fields` 是**按列名**作键的 map（《数据结构概述》：
    /// 「key 是多维表格数据表中的字段名称」，官方每个写示例也都是列名），而配置里
    /// 存的是 `field_id`（刻意如此——改列名不该让配置失效）。两条轴的差由**调用方**
    /// 在拿到表结构后现场补上，与读侧的 `rekey_cells_by_field_id` 是同一件事的两个
    /// 方向。这里收列名而不是 id，正是为了让「写到哪一列」在调用处一眼可见。
    pub(crate) backfill_field_name: &'a str,
    /// 六个可选回填目标（配置里存的 `field_id`；`None` = 不回填，现状不变）。
    ///
    /// 这里收 `field_id` 而不是列名：写时由 `BitableBackfill` 现场解析列名
    /// （与 `backfill_field_name` 是同一条轴的两个方向，设计 §5.1/§5.2）。
    pub(crate) status_field: Option<&'a str>,
    pub(crate) flow_field: Option<&'a str>,
    pub(crate) start_time_field: Option<&'a str>,
    pub(crate) end_time_field: Option<&'a str>,
    pub(crate) department_field: Option<&'a str>,
    pub(crate) handler_field: Option<&'a str>,
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

    /// 一次写**一条记录的多个字段**（`(field_id, value)` 列表）。
    ///
    /// 存在的理由：扩展字段逐字段写是 N 次串行 API 调用（每次约 1.2s），
    /// 合并为一次 `batch_update` 后只剩 1 次——实测 6 个字段从约 7s 降到约 1.2s。
    /// 值语义与 [`write`](Self::write) 逐字段一致（共用同一套类型转换）。
    ///
    /// 任一配置字段不可写即返回错误，不能把缺少必要字段的回写记为完成。
    async fn write_fields(
        &self,
        record_id: &str,
        fields: &[(&str, Value)],
    ) -> Result<(), OutboundFailure>;
}

/// 真实实现：经 `bitable::batch_update_records` 写单元格。
pub(crate) struct BitableBackfill<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) fields: &'a [FieldItem],
}

impl BitableBackfill<'_> {
    /// 按 `field_id`（或列名）找目标列。
    fn find_field(&self, field_id: &str) -> Result<&FieldItem, OutboundFailure> {
        self.fields
            .iter()
            .find(|field| field.field_id == field_id || field.field_name == field_id)
            .ok_or_else(|| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: format!("找不到回填列：{field_id}"),
            })
    }

    /// 列的写接口用名（`batch_update` 的 `records[].fields` 按**列名**作键）。
    fn resolve_name(&self, field: &FieldItem) -> Result<String, OutboundFailure> {
        super::bitable::resolve_field_name(self.fields, &field.field_id).map_err(|error| {
            OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            }
        })
    }

    /// 「值 → 单元格」转换（按目标列类型）。`None` = 该值不该写（链接列不写错误文本）。
    ///
    /// `write_many` 与 `write_fields` **共用**——两条路径的类型语义必须逐字一致，
    /// 分开维护必然漂移。真机探针的结论（单选裸字符串等）都钉在这里。
    fn cell_value(field: &FieldItem, value: &Value) -> Result<Option<Value>, OutboundFailure> {
        let cell = match field.field_type {
            Some(15) if value.is_object() => value.clone(),
            // 错误信息没有审批实例；链接列保持空白，原因由任务/请求日志保存。
            Some(15) => return Ok(None),
            // 单选：**裸字符串**（真机探针定论——数组形态被 1254062
            // `SingleSelectFieldConvFail` 拒，响应明文「must be a string」；
            // 选项不存在时自动创建）。`{"text": …}` 对象也收，取其 text。
            Some(3) => {
                let text = value.get("text").unwrap_or(value);
                match text.as_str() {
                    Some(text) => Value::String(text.to_string()),
                    None => {
                        return Err(OutboundFailure {
                            kind: FailureKind::Fatal { code: 0 },
                            message: "单选字段的值必须是字符串".to_string(),
                        });
                    }
                }
            }
            // 日期：number 毫秒；字符串数字先转换（详情接口的 start_time 是字符串）。
            Some(5) => match value {
                Value::Number(_) => value.clone(),
                Value::String(text) => match text.trim().parse::<i64>() {
                    Ok(millis) => Value::from(millis),
                    Err(_) => {
                        return Err(OutboundFailure {
                            kind: FailureKind::Fatal { code: 0 },
                            message: format!("日期字段的值不是毫秒时间戳：{text}"),
                        });
                    }
                },
                _ => {
                    return Err(OutboundFailure {
                        kind: FailureKind::Fatal { code: 0 },
                        message: "日期字段的值必须是毫秒时间戳".to_string(),
                    });
                }
            },
            // 人员：`[{"id":"ou_.."}]`（官方：记录写接口仅支持传入 id）。
            Some(11) if value.is_array() => value.clone(),
            Some(11) => {
                return Err(OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: "人员字段的值必须是 [{\"id\":\"ou_..\"}] 数组".to_string(),
                });
            }
            Some(1) | None => value.get("text").unwrap_or(value).clone(),
            _ => {
                return Err(OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: "回填列必须是文本/链接/单选/日期/人员字段".to_string(),
                });
            }
        };
        Ok(Some(cell))
    }
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
        // 字段解析在批前做一次：列不存在时即使空批也报错（与逐条时代的语义一致）。
        let field = self.find_field(field_id)?;
        let field_name = self.resolve_name(field)?;
        let mut records = Vec::with_capacity(rows.len());
        for (record_id, value) in rows {
            let Some(cell) = Self::cell_value(field, value)? else {
                continue;
            };
            records.push((
                record_id.clone(),
                BTreeMap::from([(field_name.clone(), cell)]),
            ));
        }
        if records.is_empty() {
            tracing::info!(stage = "backfill", field_id = %field.field_id, input_rows = rows.len(),
                reason = "链接列不写错误文本", "审批回填没有可写单元格");
            return Ok(());
        }
        tracing::debug!(stage = "backfill", field_id = %field.field_id, field_type = field.field_type,
            row_count = records.len(), "审批回填开始批量更新多维表格记录");
        super::bitable::batch_update_records(
            self.transport,
            self.sleeper,
            self.tokens,
            self.coordinates,
            &records,
        )
        .await
    }

    async fn write_fields(
        &self,
        record_id: &str,
        fields: &[(&str, Value)],
    ) -> Result<(), OutboundFailure> {
        let mut cells = BTreeMap::new();
        for (field_id, value) in fields {
            let field = self.find_field(field_id)?;
            let name = self.resolve_name(field)?;
            if let Some(cell) = Self::cell_value(field, value)? {
                cells.insert(name, cell);
            }
        }
        if cells.is_empty() {
            return Ok(());
        }
        tracing::info!(
            stage = "backfill",
            record_id,
            field_count = cells.len(),
            "审批回填开始合并更新多维表格记录"
        );
        super::bitable::batch_update_records(
            self.transport,
            self.sleeper,
            self.tokens,
            self.coordinates,
            &[(record_id.to_string(), cells)],
        )
        .await
    }
}

/// 任一映射控件需要走附件/图片搬运（下载多维表格文件并上传审批）。
fn requires_media_transfer(widgets: &[WidgetMap]) -> bool {
    widgets
        .iter()
        .any(|widget| widget_rules::is_media(&widget.widget_type))
}

/// 处理一条记录。
///
/// 本函数**不做落库**（任务行的写入由调用方在 claim 阶段完成，因为那需要事务，
/// 而事务的生命周期属于 Action/worker）。它只负责「创建 + 取编号 + 回写」这一段。
#[tracing::instrument(name = "approval_record", skip_all, fields(
    table_id = %input.coordinates.table_id, record_id = input.record_id,
    approval_code = input.approval_code, uuid = tracing::field::Empty
))]
pub(crate) async fn dispatch_one(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    backfill: &dyn Backfill,
    input: &DispatchInput<'_>,
) -> DispatchResult {
    // 已确认实例先恢复回填，不再受源字段或映射后续变化影响。
    if let (Some(context), Some(claim)) = (input.context, input.execution) {
        match super::approval_execution::facts(context, claim).await {
            Ok((_, Some(code))) => {
                return match get_instance(transport, sleeper, tokens, &code).await {
                    Ok(GetOutcome::Found(detail)) => {
                        let mut detail = *detail;
                        fill_department_name(
                            transport,
                            sleeper,
                            tokens,
                            input.department_field,
                            &mut detail,
                        )
                        .await;
                        finish_backfill(backfill, input, &code, detail).await
                    }
                    _ => DispatchResult::CreatedPendingBackfill {
                        message: "已创建实例暂不可查询，保留事实等待核查".into(),
                        instance_code: code,
                        serial_number: None,
                    },
                };
            }
            Ok((phase, None)) if phase == "ready" => {}
            _ => {
                return DispatchResult::Retryable {
                    message: "任务执行权失效或外部结果未知，禁止重新创建".into(),
                };
            }
        }
    }
    // ---- 1. uuid ----
    let uuid = derive_uuid(
        &input.coordinates.app_token,
        &input.coordinates.table_id,
        input.approval_code,
        input.record_id,
    );
    tracing::Span::current().record("uuid", uuid.as_str());
    tracing::debug!(
        stage = "prepare",
        widget_count = input.widgets.len(),
        "审批记录开始派发"
    );

    // 先恢复已创建实例；源附件/图片或关联审批权限变化也不能阻断编号回填。
    if input.widgets.iter().any(|w| w.widget_type == "connect")
        || requires_media_transfer(input.widgets)
    {
        tracing::info!(stage = "recover", "查询已有审批实例，优先恢复回填");
        match get_instance(transport, sleeper, tokens, &uuid).await {
            Ok(GetOutcome::Found(detail)) => {
                tracing::info!(stage = "recover", instance_code = %detail.instance_code, "已恢复审批实例");
                let mut detail = *detail;
                fill_department_name(
                    transport,
                    sleeper,
                    tokens,
                    input.department_field,
                    &mut detail,
                )
                .await;
                return finish_backfill(backfill, input, &uuid, detail).await;
            }
            Ok(GetOutcome::NotFound) => {
                tracing::info!(stage = "recover", "未找到已有审批实例，继续校验数据");
            }
            Err(failure) => {
                tracing::warn!(stage = "recover", kind = ?failure.kind, "查询已有审批实例失败，等待下一轮重试");
                return DispatchResult::Retryable {
                    message: super::approval_feedback::outbound_reason(&failure),
                };
            }
        }
    }

    // ---- 2. 发起人 ----
    //
    // **唯一来源是请求参数**（上层已把点击人姓名解析成 open_id）。系统不读多维
    // 表格的申请人列：那一列是「填表人认为该谁发起」，而审批必须由点按钮的人
    // 发起——否则任何人填一列别人的名字就能替别人提单。传参路径下
    // `applicant_field` 的语义只剩**写目标**：回填成功时把 open_id 覆盖进该列
    // （设计 §6.4，见 `write_extended`）。
    //
    // 没有身份只有一种来源：上线前的旧任务行（身份落库机制之前建的行）。它对
    // 应「等待」，且**零出站**——不读表结构、不读记录、不打飞书，等有人再点一次
    // 该行把身份补上。
    let Some(open_id) = input
        .applicant
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        tracing::info!(
            stage = "initiator",
            record_id = input.record_id,
            outcome = "waiting",
            reason = "任务没有发起人身份",
            "审批派发等待补齐发起人"
        );
        return DispatchResult::Waiting {
            reason: MISSING_INITIATOR_REASON.to_string(),
        };
    };
    let open_id = open_id.to_string();
    tracing::debug!(
        stage = "initiator",
        applicant_field = input.applicant_field,
        "发起人校验通过"
    );

    // ---- 2b. 申请人列必须就是点击人 ----
    //
    // 申请人是「填表人认为该谁发起」，而审批必须由点按钮的人发起。列里明确写着
    // 别人时继续就是替他人提单；空列/非人员值按「未填」处理，成功创建后会被
    // `write_extended` 覆盖成点击人。批量播种在扫表时已按同一规则过滤（跳过不是
    // 点击人的记录），这里是单条点击与存量任务行的兜底。
    if let Some(names) = applicant_field_conflict(input.cells.get(input.applicant_field), &open_id)
    {
        // 这条兜底也可能落在**批量播种**出来的任务上（记录不是点击人本人的）：
        // 姓名属于跨权限信息，只报条数，与批量过滤同一口径。
        let message = format!(
            "「申请人」列填写的不是本次点击人（{} 人）；为避免替他人提交，本次未创建审批。请由申请人本人点击提交，或清空该列后重试。",
            names.len()
        );
        tracing::warn!(
            stage = "initiator",
            record_id = input.record_id,
            applicant_field = input.applicant_field,
            "申请人列与点击人不一致，拒绝派发"
        );
        return DispatchResult::Terminal { message };
    }

    // ---- 3. 组装 form ----
    let form_snapshot = if input.form_snapshot.trim().is_empty() {
        tracing::info!(stage = "definition", "配置没有定义快照，从飞书读取审批定义");
        match get_approval_definition(transport, sleeper, tokens, input.approval_code).await {
            Ok(definition) => match definition.form {
                Some(Value::String(form)) => form,
                form => form.unwrap_or(Value::Null).to_string(),
            },
            Err(failure) => return classify_failure(failure, input, backfill, uuid.as_str()).await,
        }
    } else {
        tracing::info!(stage = "definition", "复用审批定义快照");
        input.form_snapshot.clone()
    };
    let mut widgets = input.widgets.to_vec();
    if let Some(context) = input.context {
        tracing::debug!(stage = "external_options", "开始解析审批外部选项");
        if let Err(error) = super::approval_provision::resolve_external_options(
            context,
            &form_snapshot,
            &mut widgets,
            input.cells,
            input.fields,
        )
        .await
        {
            return match error {
                super::approval_provision::ProvisionError::Invalid(reasons) => {
                    tracing::info!(
                        stage = "external_options",
                        outcome = "waiting",
                        issue_count = reasons.len(),
                        reason = "外部选项绑定或文案匹配不可用，详情见任务或派发记录",
                        "审批派发等待外部选项修正"
                    );
                    DispatchResult::Waiting {
                        reason: reasons.join("；"),
                    }
                }
                _error => {
                    tracing::warn!(
                        stage = "external_options",
                        outcome = "retryable",
                        "审批外部选项解析失败，等待下一轮重试"
                    );
                    DispatchResult::Retryable {
                        message: "读取外部选项绑定失败，请管理员查看派发记录。".into(),
                    }
                }
            };
        }
        tracing::debug!(stage = "external_options", "审批外部选项解析完成");
    }
    // ---- 3b. 关联列 → 实例 Code ----
    //
    // 单向/双向关联列存的是 record_id，审批控件要的是实例 Code。解析结果同时供
    // preflight、两次 build_form 与附件搬运使用——第二次 build_form 若退回原始
    // 单元格，会重新撞上「关联审批单元格需要 link 或 text」。
    let resolved_cells = match resolve_relation_connect_cells(
        transport,
        sleeper,
        tokens,
        input.coordinates,
        input.fields,
        &widgets,
        input.cells,
        input.backfill_field_name,
    )
    .await
    {
        Ok(cells) => cells,
        Err(RelationProblem::Data(reason)) => {
            tracing::info!(
                stage = "connections",
                outcome = "waiting",
                reason = "关联列解析未通过，详情见任务或派发记录",
                "审批派发等待关联审批修正"
            );
            return DispatchResult::Waiting { reason };
        }
        Err(RelationProblem::Outbound(failure)) => {
            tracing::warn!(
                stage = "connections",
                kind = ?failure.kind,
                "关联列读取失败，等待下一轮重试"
            );
            return match failure.kind {
                FailureKind::Fatal { .. } => DispatchResult::Waiting {
                    reason: format!("读取关联审批记录失败：{}", failure.message),
                },
                _ => DispatchResult::Retryable {
                    message: super::approval_feedback::outbound_reason(&failure),
                },
            };
        }
    };
    let cells = resolved_cells.as_ref().unwrap_or(input.cells);
    tracing::debug!(stage = "form", "开始校验附件/图片并转换审批表单");
    let preflight = approval_attachment::preflight(&widgets, cells);
    let form = match preflight.and_then(|codes| {
        build_form(&FormInput {
            widgets: &widgets,
            form_snapshot: &form_snapshot,
            cells,
            attachment_codes: &codes,
            timezone_offset: input.timezone_offset,
        })
    }) {
        Ok(form) => {
            tracing::debug!(stage = "form", "审批表单转换通过");
            form
        }
        // 组装失败分两类：**数据不完整**（必填缺失、值形态不对）是等待态，
        // **配置不对**（不支持的控件、转换器不匹配、选项没配映射）是终态——
        // 后者不补齐字段就能过，反复重扫永远好不了。
        Err(error) => {
            log_conversion_error(&error, &widgets);
            let message = super::approval_feedback::conversion_reason(
                &error,
                &form_snapshot,
                &widgets,
                input.fields,
            );
            return if is_data_problem(&error) {
                DispatchResult::Waiting { reason: message }
            } else {
                // 配置问题（不支持的控件、转换器不匹配、选项没配映射）**必须写字段**：
                // 它是终态，而终态的意义就是「该行退出扫描集，等人工处理」。不写的话
                // 该行每轮都被重新捞出、每轮都失败一次，用户却看不到任何线索。
                let message = sanitize_terminal_message(&message, 0);
                if let Err(failure) = backfill
                    .write(
                        input.record_id,
                        input.backfill_field_name,
                        &Value::String(message.clone()),
                    )
                    .await
                {
                    tracing::warn!(stage = "error_backfill", kind = ?failure.kind, "审批错误原因回填失败");
                }
                DispatchResult::Terminal { message }
            };
        }
    };

    tracing::debug!(stage = "connections", "开始校验关联审批实例");
    if let Err(result) =
        validate_connections(transport, sleeper, tokens, &form_snapshot, &form).await
    {
        tracing::warn!(
            stage = "connections",
            "关联审批实例校验未通过，详情见任务或派发记录"
        );
        return result;
    }
    tracing::debug!(stage = "connections", "关联审批实例校验通过");

    let form = if requires_media_transfer(input.widgets) {
        tracing::debug!(stage = "attachments", "开始搬运审批附件/图片");
        let codes = match approval_attachment::transfer(
            transport,
            sleeper,
            tokens,
            input.coordinates,
            input.record_id,
            &widgets,
            cells,
        )
        .await
        {
            Ok(codes) => codes,
            Err(failure) => {
                tracing::warn!(stage = "attachments", kind = ?failure.kind, "审批附件/图片搬运失败，等待下一轮重试");
                return DispatchResult::Retryable {
                    message: super::approval_feedback::outbound_reason(&failure),
                };
            }
        };
        tracing::debug!(
            stage = "attachments",
            file_count = codes.values().map(Vec::len).sum::<usize>(),
            "审批附件/图片搬运完成"
        );
        match build_form(&FormInput {
            widgets: &widgets,
            form_snapshot: &form_snapshot,
            cells,
            attachment_codes: &codes,
            timezone_offset: input.timezone_offset,
        }) {
            Ok(form) => form,
            Err(error) => {
                log_conversion_error(&error, &widgets);
                return DispatchResult::Retryable {
                    message: super::approval_feedback::conversion_reason(
                        &error,
                        &form_snapshot,
                        &widgets,
                        input.fields,
                    ),
                };
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
    tracing::debug!(stage = "create", "开始创建审批实例");
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
            tracing::debug!(stage = "create", instance_code = %instance_code, "审批实例创建成功，等待取编号和回填");
            reclaimed = None;
            instance_code
        }
        Ok(CreateOutcome::UuidConflict) => {
            tracing::info!(stage = "recover", "uuid 冲突，按 uuid 恢复审批实例");
            // ---- 回捞：uuid 冲突 = 实例此前已建成功，响应丢了 ----
            match get_instance(transport, sleeper, tokens, &uuid).await {
                Ok(GetOutcome::Found(detail)) => {
                    tracing::info!(stage = "recover", instance_code = %detail.instance_code, "已恢复审批实例");
                    let code = detail.instance_code.clone();
                    reclaimed = Some(*detail);
                    code
                }
                // 实例不存在：并发窗口（冲突成立但实例还没可见）或前一次其实没建成。
                // **不落终态**——那会把可能存在的实例判死。
                Ok(GetOutcome::NotFound) => {
                    tracing::warn!(stage = "recover", "uuid 冲突但实例尚不可见，等待下一轮重试");
                    return DispatchResult::Retryable {
                        message: format!("uuid 冲突但按 uuid 查不到实例（uuid={uuid}），稍后重试"),
                    };
                }
                Err(failure) => {
                    return classify_failure(failure, input, backfill, uuid.as_str()).await;
                }
            }
        }
        Err(failure) => return classify_failure(failure, input, backfill, uuid.as_str()).await,
    };

    if let (Some(context), Some(claim)) = (input.context, input.execution) {
        if super::approval_execution::confirm_effect(
            context,
            claim,
            "created",
            Some(&instance_code),
        )
        .await
        .is_err()
        {
            return DispatchResult::CreatedPendingBackfill {
                message: "审批实例已存在，但本地确认失败，已暂停后续写入".into(),
                instance_code,
                serial_number: None,
            };
        }
    }

    // ---- 5. 取 serial_number ----
    //
    // 回捞路径已经拿到了详情，只有正常创建路径需要再查一次。
    let mut detail = match reclaimed {
        Some(detail) => detail,
        None => {
            tracing::debug!(stage = "detail", instance_code = %instance_code, "开始查询审批实例编号");
            match get_instance(transport, sleeper, tokens, &instance_code).await {
                Ok(GetOutcome::Found(detail)) => *detail,
                // 刚创建的实例查不到：判**可重试**，绝不写终态——否则刚建成的实例会被
                // 写成「找不到」，反而制造第二条不可恢复路径。
                Ok(GetOutcome::NotFound) => {
                    tracing::warn!(stage = "detail", instance_code = %instance_code, "刚创建的审批实例详情尚不可见，等待下一轮重试");
                    return DispatchResult::CreatedPendingBackfill {
                        message: "审批实例已确认，详情暂不可见，请稍后重试。".into(),
                        instance_code,
                        serial_number: None,
                    };
                }
                Err(failure) => {
                    return DispatchResult::CreatedPendingBackfill {
                        message: super::approval_feedback::outbound_reason(&failure),
                        instance_code,
                        serial_number: None,
                    };
                }
            }
        }
    };

    // 部门名解析（本次派发唯一一次额外出站；未配部门列时零调用）。
    fill_department_name(
        transport,
        sleeper,
        tokens,
        input.department_field,
        &mut detail,
    )
    .await;
    finish_backfill(backfill, input, &uuid, detail).await
}

// ---------------------------------------------------------------------------
// 聚合组派发：一次 create 组装 N 行明细，同编号回填全部成员
// ---------------------------------------------------------------------------

// 组派发的入口由 worker 的组处理在 Task 9 接入；Task 8 先交付编排与单测，
// 与 `approval_aggregation` / `approval_execution` 的组原语同一条惯例。
#[allow(dead_code)]
/// 组内一条成员记录：原始单元格 + 已搬运好的附件 code。
///
/// 附件由调用方（worker 的组处理）逐记录预检/搬运后传入——组只有一次 `create`，
/// 搬运必须在组装表单之前完成（设计 §4.6 第 4 步）。空 map 表示该成员没有媒体控件。
pub(crate) struct GroupMemberInput<'a> {
    pub(crate) record_id: &'a str,
    pub(crate) cells: &'a Map<String, Value>,
    pub(crate) attachment_codes: BTreeMap<String, Vec<String>>,
}

/// 一个聚合组的派发输入。
///
/// 组身份（`group_uuid` / `instance_code`）由组行提供，**不从 leader 任务行取**：
/// leader 永远不持有组实例码（设计 §4.1）。
#[allow(dead_code)]
pub(crate) struct GroupDispatchInput<'a> {
    pub(crate) fields: &'a [FieldItem],
    /// 组行阶段与实例码的落库入口：`60012` 回捞未命中时把阶段写回 `ready`，
    /// 命中时把实例码与 `created` 写回组行（设计 §4.6 第 3/7 步）。
    pub(crate) context: Option<&'a super::context::FeishuContext>,
    pub(crate) execution: Option<&'a super::approval_execution::TaskClaim>,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) group_key: &'a str,
    /// 组行派生出的幂等键；`create` 必须用它（成员行的 `uuid` 只服务单条点击）。
    pub(crate) group_uuid: &'a str,
    /// 组行已有的实例码。非空 = 已建过实例：恢复回填，跳过创建与签名复核。
    pub(crate) instance_code: Option<&'a str>,
    pub(crate) backfill_field_name: &'a str,
    pub(crate) status_field: Option<&'a str>,
    pub(crate) flow_field: Option<&'a str>,
    pub(crate) start_time_field: Option<&'a str>,
    pub(crate) end_time_field: Option<&'a str>,
    pub(crate) department_field: Option<&'a str>,
    pub(crate) handler_field: Option<&'a str>,
    pub(crate) approval_code: &'a str,
    pub(crate) widgets: &'a [WidgetMap],
    pub(crate) form_snapshot: &'a str,
    pub(crate) timezone_offset: FixedOffset,
    pub(crate) applicant_field: &'a str,
    /// 批量点击人（open_id）。组缺少身份时整组暂停，不走单条 `Waiting`。
    pub(crate) applicant_open_id: Option<&'a str>,
    /// 组内全部成员，**按 `group_seq` 升序**（下标即明细行序）。
    pub(crate) members: &'a [GroupMemberInput<'a>],
}

/// 组失败的处置。只有两种：整组暂停，或保持 `pending` 等下一轮。
///
/// 与单条 [`DispatchResult`] 分开的理由是「**绝不写回填列**」：单条终态要写原因，
/// 而组暂停只进 `last_error` 与通知——写进任何成员的编号列都会让该行退出扫描集，
/// 整组就再也修不回来了（设计 §5）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum GroupOutcome {
    /// 数据/配置/远端明确拒绝：整组暂停，等人工重新批量点击。
    Paused { message: String },
    /// 频控/瞬时失败/凭证失效：保持 `pending`，按既有退避重试。
    Retryable { message: String },
}

/// 组失败分类：`Fatal` → 整组暂停，其余 → 可重试。
///
/// **不调用 `backfill.write`**，也不写任何成员的编号列（设计 §4.6）。
#[allow(dead_code)]
pub(crate) fn classify_group_failure(failure: OutboundFailure) -> GroupOutcome {
    tracing::warn!(kind = ?failure.kind, "聚合组出站失败，按错误类别决定暂停或重试");
    let reason = super::approval_feedback::outbound_reason(&failure);
    match failure.kind {
        FailureKind::Retry { .. }
        | FailureKind::TokenExpired
        | FailureKind::UuidConflict
        | FailureKind::InstanceNotFound => GroupOutcome::Retryable { message: reason },
        // 终态文案要裁剪：多维表格的读者范围远大于运维。
        FailureKind::Fatal { code } => GroupOutcome::Paused {
            message: sanitize_terminal_message(&reason, code),
        },
    }
}

/// 组处置 → 编排结论。
///
/// 组暂停回 [`DispatchResult::Terminal`]：worker 的组分支据此写 `state='paused'`
/// （不是 `terminal`），并**跳过**单条的「终态写回填列」动作。
#[allow(dead_code)]
fn group_result(outcome: GroupOutcome) -> DispatchResult {
    match outcome {
        GroupOutcome::Paused { message } => DispatchResult::Terminal { message },
        GroupOutcome::Retryable { message } => DispatchResult::Retryable { message },
    }
}

/// 派发一个聚合组：一次 `create`、按组 uuid 回捞、同编号回填全部成员。
///
/// 与 [`dispatch_one`] 的差别只在「一条实例对多条记录」：
///
/// - `create` 的幂等键是**组 uuid**（成员行 uuid 只服务单条点击）；
/// - 组实例码由 transport 的组专用确认写进组行，leader 任务行的 `instance_code`
///   保持为 NULL（设计 §4.1）；
/// - `60012` 按组 uuid 回捞；未命中把组行阶段写回 `ready` 再退避重发；
/// - 回填是**批量**的：`BackfillReport.failed` 非空即整组重试，全部成功后才逐成员
///   写扩展字段与发起人列。
#[allow(dead_code)]
pub(crate) async fn dispatch_group(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    backfill: &dyn Backfill,
    input: &GroupDispatchInput<'_>,
) -> DispatchResult {
    if input.members.is_empty() {
        // 空组不该出现（播种至少一行）；真出现了也只能暂停，不能建一个空单。
        return DispatchResult::Terminal {
            message: "聚合组没有成员记录，已暂停".into(),
        };
    }

    // ---- 1. 已建实例：恢复回填，不再创建、不再复核签名 ----
    if let Some(code) = input
        .instance_code
        .map(str::trim)
        .filter(|code| !code.is_empty())
    {
        tracing::info!(stage = "recover", instance_code = %code, "按组实例码恢复回填");
        return match get_instance(transport, sleeper, tokens, code).await {
            Ok(GetOutcome::Found(detail)) => {
                let mut detail = *detail;
                fill_department_name(
                    transport,
                    sleeper,
                    tokens,
                    input.department_field,
                    &mut detail,
                )
                .await;
                finish_group_backfill(backfill, input, detail).await
            }
            // 实例码已在组行落实（外部副作用已发生），详情查不到只是暂时不可见：
            // 保留事实，下一轮再来，绝不重新创建。
            Ok(GetOutcome::NotFound) | Err(_) => DispatchResult::CreatedPendingBackfill {
                message: "聚合组实例已确认但详情暂不可见，保留事实等待核查".into(),
                instance_code: code.to_string(),
                serial_number: None,
            },
        };
    }

    // ---- 2. 发起人：组缺身份 = 整组暂停 ----
    let Some(open_id) = input
        .applicant_open_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        tracing::info!(
            stage = "initiator",
            group_key = input.group_key,
            outcome = "paused",
            "聚合组没有发起人身份，整组暂停"
        );
        return DispatchResult::Terminal {
            message: MISSING_INITIATOR_REASON.to_string(),
        };
    };
    let open_id = open_id.to_string();

    // ---- 2b. 每个成员的申请人列都必须包含点击人 ----
    //
    // 组成员是批量扫描出来的，播种期已按同一规则整批拦截；这里兜住「行是旧版本
    // 播下的 / 点击后有人改了申请人列」。一个成员不匹配就整组暂停，绝不替他人提单
    // （组路径不写任何成员的编号列，设计 §5）。
    //
    // 通知只报**条数**，不列姓名：批量扫描会扫到点击人在高级权限下看不到的行，
    // 姓名属于跨权限信息；「有 N 条不是您」已经足够行动。
    let mismatch_count = input
        .members
        .iter()
        .filter(|member| {
            applicant_field_conflict(member.cells.get(input.applicant_field), &open_id).is_some()
        })
        .count();
    if mismatch_count > 0 {
        let message = format!(
            "聚合组内有 {mismatch_count} 条记录的「申请人」不是本次点击人；为避免替他人提交，整组已暂停，未创建审批。请由申请人本人重新点击批量提交，或清空该列后重试。"
        );
        tracing::warn!(
            stage = "initiator",
            group_key = input.group_key,
            mismatch_count,
            "聚合组成员申请人与点击人不一致，整组暂停"
        );
        return DispatchResult::Terminal { message };
    }

    // 关联列 → 实例 Code（逐成员，与单条同一套解析）。组路径不写回填列，
    // 所以数据问题只能整组暂停，等用户修正后重新批量点击。
    let mut resolved: Vec<Option<Map<String, Value>>> = Vec::with_capacity(input.members.len());
    for member in input.members {
        match resolve_relation_connect_cells(
            transport,
            sleeper,
            tokens,
            input.coordinates,
            input.fields,
            input.widgets,
            member.cells,
            input.backfill_field_name,
        )
        .await
        {
            Ok(cells) => resolved.push(cells),
            Err(RelationProblem::Data(reason)) => {
                return DispatchResult::Terminal {
                    message: format!("关联审批列解析失败：{reason}"),
                }
            }
            Err(RelationProblem::Outbound(failure)) => {
                return group_result(classify_group_failure(failure))
            }
        }
    }

    // ---- 3. 组装多行明细表单 ----
    let rows: Vec<GroupFormRow<'_>> = input
        .members
        .iter()
        .zip(&resolved)
        .map(|(member, resolved)| GroupFormRow {
            record_id: member.record_id,
            cells: resolved.as_ref().unwrap_or(member.cells),
            attachment_codes: &member.attachment_codes,
        })
        .collect();
    tracing::debug!(
        stage = "form",
        member_count = rows.len(),
        "开始组装聚合组表单"
    );
    let form = match build_group_form(&GroupFormInput {
        widgets: input.widgets,
        form_snapshot: input.form_snapshot,
        rows: &rows,
        timezone_offset: input.timezone_offset,
    }) {
        Ok(form) => form,
        Err(error) => {
            log_conversion_error(&error, input.widgets);
            // 组路径一律暂停：空明细/缺必填/选项冲突都是「修好后重新点击」的事，
            // 轮询不会自愈（设计 §5）。
            return DispatchResult::Terminal {
                message: super::approval_feedback::conversion_reason(
                    &error,
                    input.form_snapshot,
                    input.widgets,
                    input.fields,
                ),
            };
        }
    };
    tracing::debug!(stage = "form", "聚合组表单组装完成");

    // ---- 4. 创建（幂等键 = 组 uuid） ----
    tracing::debug!(
        stage = "create",
        group_uuid = input.group_uuid,
        "开始创建聚合组实例"
    );
    let reclaimed: Option<InstanceDetail>;
    let instance_code = match create_instance(
        transport,
        sleeper,
        tokens,
        input.approval_code,
        &form,
        &open_id,
        input.group_uuid,
    )
    .await
    {
        Ok(CreateOutcome::Created { instance_code }) => {
            reclaimed = None;
            instance_code
        }
        Ok(CreateOutcome::UuidConflict) => {
            tracing::info!(stage = "recover", "组 uuid 冲突，按组 uuid 恢复审批实例");
            match get_instance(transport, sleeper, tokens, input.group_uuid).await {
                Ok(GetOutcome::Found(detail)) => {
                    let code = detail.instance_code.clone();
                    reclaimed = Some(*detail);
                    code
                }
                Ok(GetOutcome::NotFound) => {
                    // 组行停在 `lookup` 等不到实例：写回 `ready` 才能重新创建
                    // （组行阶段是唯一事实，重试不会自己复位，设计 §4.6 第 3 步）。
                    if let (Some(context), Some(claim)) = (input.context, input.execution) {
                        if let Err(error) = super::approval_execution::confirm_group_effect(
                            context,
                            claim,
                            input.group_key,
                            "ready",
                            None,
                        )
                        .await
                        {
                            tracing::warn!(stage = "recover", error = %error, "组阶段复位失败");
                        }
                    }
                    return DispatchResult::Retryable {
                        message: format!(
                            "组 uuid 冲突但按组 uuid 查不到实例（uuid={}），稍后重试",
                            input.group_uuid
                        ),
                    };
                }
                Err(failure) => return group_result(classify_group_failure(failure)),
            }
        }
        Err(failure) => return group_result(classify_group_failure(failure)),
    };

    // 实例码必须落回**组行**（leader 行永远不持有组实例码，设计 §4.1）。
    //
    // `create` 成功时 `GroupExecutionTransport` 已经写过一次；但 `60012` 回捞
    // 那条路只把阶段确认成 `lookup`，**实例码还在详情里没落库**——不回写的话
    // 组行 `instance_code` 永久为 NULL：`maintain_groups` 以它判定「已提交组」，
    // 成员编号被清空后就不会再 reopen 补回填（设计 §4.6 第 3/7 步）。
    if let (Some(context), Some(claim)) = (input.context, input.execution) {
        if super::approval_execution::confirm_group_effect(
            context,
            claim,
            input.group_key,
            "created",
            Some(&instance_code),
        )
        .await
        .is_err()
        {
            return DispatchResult::CreatedPendingBackfill {
                message: "聚合组实例已存在，但组行确认失败，已暂停后续写入".into(),
                instance_code,
                serial_number: None,
            };
        }
    }

    // ---- 5. 详情（回捞路径已带回编号，不再多打一次） ----
    let mut detail = match reclaimed {
        Some(detail) => detail,
        None => {
            tracing::debug!(stage = "detail", instance_code = %instance_code, "开始查询组实例编号");
            match get_instance(transport, sleeper, tokens, &instance_code).await {
                Ok(GetOutcome::Found(detail)) => *detail,
                Ok(GetOutcome::NotFound) => {
                    return DispatchResult::CreatedPendingBackfill {
                        message: "聚合组实例已确认，详情暂不可见，请稍后重试。".into(),
                        instance_code,
                        serial_number: None,
                    };
                }
                Err(failure) => {
                    return DispatchResult::CreatedPendingBackfill {
                        message: super::approval_feedback::outbound_reason(&failure),
                        instance_code,
                        serial_number: None,
                    };
                }
            }
        }
    };
    fill_department_name(
        transport,
        sleeper,
        tokens,
        input.department_field,
        &mut detail,
    )
    .await;
    finish_group_backfill(backfill, input, detail).await
}

/// 组回填：先把同一编号批量写进**全部成员**，全部落库后才逐成员写扩展字段。
///
/// 部分失败返回 [`DispatchResult::CreatedPendingBackfill`] 让整组重试——重复写同一
/// 编号是幂等的；把部分成功当成功会让一部分成员永久缺编号（设计 §4.6 第 8 步）。
#[allow(dead_code)]
async fn finish_group_backfill(
    backfill: &dyn Backfill,
    input: &GroupDispatchInput<'_>,
    detail: InstanceDetail,
) -> DispatchResult {
    let Some(serial_number) = serial_number_of(&detail) else {
        tracing::info!(stage = "detail", instance_code = %detail.instance_code, "聚合组实例已创建但编号尚未生成，等待下一轮重试");
        return DispatchResult::CreatedPendingBackfill {
            message: "聚合组实例已确认，编号尚未生成，请稍后重试。".into(),
            instance_code: detail.instance_code.clone(),
            serial_number: None,
        };
    };
    let link = match super::approval::instance_applink(&detail.instance_code) {
        Ok(link) => link,
        Err(error) => {
            tracing::warn!(stage = "backfill", "聚合组实例链接生成失败，等待下一轮重试");
            return DispatchResult::CreatedPendingBackfill {
                message: format!("审批编号：{serial_number}；实例链接暂不可用：{error}"),
                instance_code: detail.instance_code.clone(),
                serial_number: Some(serial_number),
            };
        }
    };
    let value = serde_json::json!({"text": serial_number, "link": link});
    let rows: Vec<(String, Value)> = input
        .members
        .iter()
        .map(|member| (member.record_id.to_string(), value.clone()))
        .collect();
    tracing::debug!(
        stage = "backfill",
        instance_code = %detail.instance_code,
        member_count = rows.len(),
        "开始批量回填聚合组编号"
    );
    let report = backfill_in_chunks(backfill, input.backfill_field_name, &rows).await;
    if !report.failed.is_empty() {
        let failed_ids: Vec<&str> = report
            .failed
            .iter()
            .take(5)
            .map(|(record_id, _)| record_id.as_str())
            .collect();
        tracing::warn!(
            stage = "backfill",
            failed = report.failed.len(),
            written = report.written.len(),
            "聚合组编号回填部分失败，整组重试"
        );
        return DispatchResult::CreatedPendingBackfill {
            message: format!(
                "审批编号：{serial_number}；{} 条成员回填失败（{}），整组重试",
                report.failed.len(),
                failed_ids.join("、")
            ),
            instance_code: detail.instance_code.clone(),
            serial_number: Some(serial_number),
        };
    }
    if let Err(failure) = write_group_extended(backfill, input, &detail).await {
        tracing::warn!(stage = "backfill", kind = ?failure.kind, "聚合组扩展字段回填失败");
        return DispatchResult::CreatedPendingBackfill {
            message: format!(
                "审批编号：{serial_number}；扩展字段回填失败：{}",
                super::approval_feedback::outbound_reason(&failure)
            ),
            instance_code: detail.instance_code.clone(),
            serial_number: Some(serial_number),
        };
    }
    DispatchResult::Backfilled {
        serial_number,
        instance_code: detail.instance_code.clone(),
    }
}

/// 逐成员写扩展字段与发起人列（与单条路径同一份 [`write_extended`]）。
///
/// 顶层控件值相同，所以每个成员的写入内容只差 `record_id` 与自己的单元格。发起人
/// 列只在空列或写着点击人时写入；恢复已建组的实例时，明确写着别人的非空列保持原样
/// （见 [`write_extended`] 的保护规则）。
#[allow(dead_code)]
async fn write_group_extended(
    backfill: &dyn Backfill,
    input: &GroupDispatchInput<'_>,
    detail: &InstanceDetail,
) -> Result<(), OutboundFailure> {
    for member in input.members {
        let single = DispatchInput {
            fields: input.fields,
            context: input.context,
            execution: input.execution,
            coordinates: input.coordinates,
            record_id: member.record_id,
            cells: member.cells,
            applicant_field: input.applicant_field,
            applicant: input.applicant_open_id.map(str::to_string),
            backfill_field_name: input.backfill_field_name,
            status_field: input.status_field,
            flow_field: input.flow_field,
            start_time_field: input.start_time_field,
            end_time_field: input.end_time_field,
            department_field: input.department_field,
            handler_field: input.handler_field,
            approval_code: input.approval_code,
            widgets: input.widgets,
            form_snapshot: input.form_snapshot.to_string(),
            timezone_offset: input.timezone_offset,
        };
        write_extended(backfill, &single, detail).await?;
    }
    Ok(())
}

/// 关联列预解析失败的分类。
#[derive(Debug)]
enum RelationProblem {
    /// 数据问题：关联行不存在、申请编号还没回填、跨表关联等。等数据补齐。
    Data(String),
    /// 出站失败：按类别决定重试还是等待。
    Outbound(OutboundFailure),
}

/// 关联列（单向 `18` / 双向 `21`）→ 实例 Code。
///
/// 「关联审批」控件只认实例 Code，而关联列单元格里存的是 record_id——Code 在那些
/// 被关联行的**回填列**（「申请编号」的超链接）里。所以必须先把被关联行读回来。
/// 单次最多 100 条（`records/batch_get` 的官方上限）。返回 `None` = 没有任何
/// 关联单元格需要解析。
#[allow(clippy::too_many_arguments)] // 派发路径的要素逐个平铺，与既有编排函数一致。
async fn resolve_relation_connect_cells(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    fields: &[FieldItem],
    widgets: &[WidgetMap],
    cells: &Map<String, Value>,
    backfill_field_name: &str,
) -> Result<Option<Map<String, Value>>, RelationProblem> {
    let mut targets: Vec<(&WidgetMap, Vec<String>)> = Vec::new();
    for widget in widgets.iter().filter(|w| w.widget_type == "connect") {
        let field = fields
            .iter()
            .find(|field| field.field_id == widget.bitable_field);
        if let Some(field) = field {
            // 只认关联列：文本格子里恰好写着「rec…」不该被当成 record_id。
            // 字段信息缺失时（旧快照/裁剪响应）退回按单元格形态判断。
            if !matches!(field.field_type, Some(18 | 21) | None) {
                continue;
            }
            if let Some(table_id) = field
                .property
                .as_ref()
                .and_then(|property| property.table_id.as_deref())
            {
                if table_id != coordinates.table_id {
                    return Err(RelationProblem::Data(
                        "关联审批列指向其它数据表，暂不支持".into(),
                    ));
                }
            }
        }
        let Some(ids) = cells
            .get(&widget.bitable_field)
            .and_then(relation_record_ids)
        else {
            continue;
        };
        targets.push((widget, ids));
    }
    if targets.is_empty() {
        return Ok(None);
    }

    let backfill_field = fields
        .iter()
        .find(|field| {
            field.field_name.trim() == backfill_field_name.trim()
                || field.field_id == backfill_field_name
        })
        .ok_or_else(|| RelationProblem::Data("回填列不可用".into()))?;

    // 保序去重后统一批量读：同一被关联行可能被多个控件引用。
    let mut wanted: Vec<String> = Vec::new();
    for (_, ids) in &targets {
        for id in ids {
            if !wanted.contains(id) {
                wanted.push(id.clone());
            }
        }
    }
    let mut records: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for chunk in wanted.chunks(100) {
        let items = get_records_by_ids(transport, sleeper, tokens, coordinates, chunk)
            .await
            .map_err(|failure| match failure.kind {
                FailureKind::Fatal { .. } => RelationProblem::Data(failure.message),
                _ => RelationProblem::Outbound(failure),
            })?;
        for item in items {
            let cells = rekey_cells_by_field_id(fields, &item.fields)
                .map_err(|error| RelationProblem::Data(error.to_string()))?;
            records.insert(item.record_id, cells);
        }
    }
    if wanted.iter().any(|id| !records.contains_key(id)) {
        return Err(RelationProblem::Data(
            "关联审批列引用的记录不存在（可能已被删除）".into(),
        ));
    }

    let mut resolved = cells.clone();
    for (widget, ids) in &targets {
        let mut codes: Vec<String> = Vec::new();
        for id in ids {
            let record = records.get(id).ok_or_else(|| {
                RelationProblem::Data("关联审批列引用的记录不存在（可能已被删除）".into())
            })?;
            let cell = record.get(&backfill_field.field_id).ok_or_else(|| {
                RelationProblem::Data("关联审批列引用的记录还没有申请编号".into())
            })?;
            if is_empty(cell) {
                return Err(RelationProblem::Data(
                    "关联审批列引用的记录还没有申请编号".into(),
                ));
            }
            let found = connection_codes(cell).map_err(|reason| {
                RelationProblem::Data(format!("关联审批列引用的记录，其申请编号不可用：{reason}"))
            })?;
            for code in found {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
        resolved.insert(
            widget.bitable_field.clone(),
            Value::Array(codes.into_iter().map(Value::String).collect()),
        );
    }
    Ok(Some(resolved))
}

/// 在上传附件/图片和创建审批之前校验关联实例；同一实例在一单中只查询一次。
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
                    Ok(GetOutcome::Found(detail)) => *detail,
                    Ok(GetOutcome::NotFound) => {
                        tracing::info!(stage = "connections", widget_id = %node.id, instance_code = code, reason = "关联实例不存在", "审批派发等待关联审批修正");
                        return Err(DispatchResult::Waiting {
                            reason: format!(
                                "关联审批控件「{}」（{}）：实例 {code} 不存在",
                                node.name, node.id
                            ),
                        });
                    }
                    Err(failure) => {
                        tracing::warn!(stage = "connections", widget_id = %node.id, instance_code = code, kind = ?failure.kind, "关联审批实例查询失败");
                        return Err(DispatchResult::Retryable {
                            message: format!(
                                "关联审批控件「{}」查询失败：{}（控件定位：{}）",
                                node.name,
                                super::approval_feedback::outbound_reason(&failure),
                                node.id
                            ),
                        });
                    }
                };
                if !detail.instance_code.eq_ignore_ascii_case(code)
                    || detail
                        .approval_code
                        .as_deref()
                        .map_or(true, |code| code.trim().is_empty())
                {
                    tracing::warn!(stage = "connections", widget_id = %node.id, "关联审批详情不完整或实例 Code 不一致");
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
                tracing::info!(stage = "connections", widget_id = %node.id, instance_code = code, reason = "关联实例不属于允许的审批定义", "审批派发等待关联审批修正");
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

/// 给详情补上发起人部门名（回填列优先写名字，解析失败退化为写 ID）。
///
/// **只在配置了部门回填列时才出站**——这是 `write_extended` 那句「值全部取自
/// 已经拿到的那一份详情，零额外 API 调用」的**唯一例外**，且只在显式配置了
/// 部门列时发生；未配该列的存量配置行为零变化（零额外调用）。
///
/// 解析失败不冒泡：`resolve_department_name` 内部 warn 后返回 `None`，
/// `write_extended` 自然退化写 ID。
pub(crate) async fn fill_department_name(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    department_field: Option<&str>,
    detail: &mut InstanceDetail,
) {
    if department_field.is_none() {
        return;
    }
    // 编号还没出现时 `finish_backfill` 会直接返回 `CreatedPendingBackfill`
    // （`write_extended` 根本不执行，解析结果必被丢弃），而任务落回 pending
    // 后下一轮还会再来——不拦的话就是每轮一次**白跑**的 contact 调用。
    if serial_number_of(detail).is_none() {
        return;
    }
    let Some(department_id) = detail.department_id.as_deref() else {
        return;
    };
    detail.department_name =
        resolve_department_name(transport, sleeper, tokens, department_id).await;
}

async fn finish_backfill(
    backfill: &dyn Backfill,
    input: &DispatchInput<'_>,
    _uuid: &str,
    detail: InstanceDetail,
) -> DispatchResult {
    let Some(serial_number) = serial_number_of(&detail) else {
        // 实例存在但编号还没生成：同样是可重试，不是失败。
        tracing::info!(stage = "detail", instance_code = %detail.instance_code, "审批实例已创建但编号尚未生成，等待下一轮重试");
        return DispatchResult::CreatedPendingBackfill {
            message: "审批实例已确认，编号尚未生成，请稍后重试。".into(),
            instance_code: detail.instance_code.clone(),
            serial_number: None,
        };
    };

    let link = match super::approval::instance_applink(&detail.instance_code) {
        Ok(link) => link,
        Err(error) => {
            tracing::warn!(stage = "backfill", "审批实例链接生成失败，等待下一轮重试");
            return DispatchResult::CreatedPendingBackfill {
                message: format!("审批编号：{serial_number}；实例链接暂不可用：{error}"),
                instance_code: detail.instance_code.clone(),
                serial_number: Some(serial_number),
            };
        }
    };
    let value = serde_json::json!({"text": serial_number, "link": link});
    // ---- 6. 回写 ----
    tracing::debug!(stage = "backfill", instance_code = %detail.instance_code, serial_number = %serial_number, "开始回填审批编号");
    match backfill
        .write(input.record_id, input.backfill_field_name, &value)
        .await
    {
        Ok(()) => {
            tracing::debug!(stage = "backfill", instance_code = %detail.instance_code, serial_number = %serial_number, "审批编号回填成功");
            if let Err(failure) = write_extended(backfill, input, &detail).await {
                return DispatchResult::CreatedPendingBackfill {
                    message: format!(
                        "审批编号：{serial_number}；扩展字段回填失败：{}",
                        super::approval_feedback::outbound_reason(&failure)
                    ),
                    instance_code: detail.instance_code.clone(),
                    serial_number: Some(serial_number),
                };
            }
            DispatchResult::Backfilled {
                serial_number,
                instance_code: detail.instance_code.clone(),
            }
        }
        Err(failure) => {
            tracing::warn!(stage = "backfill", instance_code = %detail.instance_code, kind = ?failure.kind, "审批实例已存在，编号回填失败");
            DispatchResult::CreatedPendingBackfill {
                message: format!(
                    "审批编号：{serial_number}；编号回填失败：{}",
                    super::approval_feedback::outbound_reason(&failure)
                ),
                instance_code: detail.instance_code.clone(),
                serial_number: Some(serial_number),
            }
        }
    }
}

/// 编号回填成功后的可选扩展字段（设计 §5.2）。
///
/// 每个字段只在 `input` 有对应目标列时写；全部字段**合并为一次**
/// `batch_update`——逐字段写是 N 次串行 API 调用（每次约 1.2s），合并后
/// 只剩 1 次（实测 6 个字段从约 7s 降到约 1.2s）。值语义不变：类型转换在
/// [`Backfill::write_fields`] 的实现里与逐条路径**共用**同一套逻辑。
///
/// 任一配置字段回填失败必须传给调用方，保留已创建实例并等待恢复，不能提前确认完成。
/// 值全部取自**已经拿到的**那一份
/// 详情，零额外 API 调用——唯一例外是部门名（`fill_department_name` 在调用方
/// 预先解析，且只在配置了部门列时发生）。
pub(crate) async fn write_extended(
    backfill: &dyn Backfill,
    input: &DispatchInput<'_>,
    detail: &InstanceDetail,
) -> Result<(), OutboundFailure> {
    // 先把全部目标字段收集成 (field_id, value) 列表，一次合并写。
    let mut fields: Vec<(&str, Value)> = Vec::new();

    // 发起时间 / 完成时间：字符串毫秒 → number（`"0"` 与缺失同义，不写，
    // 否则日期列会显示 1970，设计 D6）。
    if let Some(field) = input.start_time_field {
        if let Some(millis) = millis_of(detail.start_time.as_deref()) {
            fields.push((field, Value::from(millis)));
        }
    }
    if let Some(field) = input.end_time_field {
        if let Some(millis) = millis_of(detail.end_time.as_deref()) {
            fields.push((field, Value::from(millis)));
        }
    }
    // 申请状态：中文映射 + 「(已撤销)」后缀。文本列收字符串，单选列也收裸字符串
    // （`write_many` 的 `Some(3)` 臂）。
    if let Some(field) = input.status_field {
        if let Some(status) = status_text(detail) {
            fields.push((field, Value::String(status)));
        }
    }
    // 当前处理人：`task_list` 里 `status == "PENDING"` 的 open_id 去重；
    // 已终态（无 PENDING）时写空数组清列（设计 §5.2）。
    if let Some(field) = input.handler_field {
        if let Some(task_list) = detail.task_list.as_ref() {
            fields.push((field, Value::Array(pending_handlers(task_list))));
        }
    }
    // 审批流程：已到达节点的 `node_name✓/✗/⏳` 序列（设计 §5.2）。
    if let Some(field) = input.flow_field {
        if let Some(flow) = flow_text(detail) {
            fields.push((field, Value::String(flow)));
        }
    }
    // 发起人列写回（§6.4）：把点击人 open_id **覆盖**进 `applicant_field` 列。
    // 这是该列唯一的写入方——用户手填的值在下一次成功创建时被改写成真正的发起人。
    // 与六个扩展列同一口径：列不在表结构里就是整批失败（`write_fields` 全有全无），
    // 任务留在 `CreatedPendingBackfill` 等人修，而不是悄悄只写一半。
    //
    // **但恢复路径不能覆盖明确写着别人的非空人员列**：实例已创建时发起人已在飞书
    // 固定，恢复/重试只补编号，不能把人工改过的申请人列静默改回（组恢复逐成员调用
    // 这里，同样受这条保护）。新建路径在 `dispatch_one`/`dispatch_group` 已保证
    // 单元格为空或包含点击人，所以这里的跳过只会在恢复或校验后的并发改动中发生。
    let applicant = input.applicant.as_deref().unwrap_or_default().trim();
    let preserve_foreign = !input.applicant_field.trim().is_empty()
        && !applicant.is_empty()
        && applicant_field_conflict(input.cells.get(input.applicant_field), applicant).is_some();
    if !input.applicant_field.trim().is_empty() && !applicant.is_empty() && !preserve_foreign {
        fields.push((
            input.applicant_field,
            Value::Array(vec![serde_json::json!({"id": applicant})]),
        ));
    }
    // 发起人部门：**优先写解析出的部门名**（`fill_department_name` 在拿详情后
    // 补上），解析失败/未配时退化为写 `department_id` 原值——信息不丢，用户
    // 至少能看出「这一列的 ID 是没解析成功的」。
    //
    // 判空必须逐项做（`or_else` 而不是 `or`）：`or` 会被一个空白名字短路，
    // 随后的 filter 再把整条链判空——该列一个字都不写，连 ID 兜底都丢掉。
    if let Some(field) = input.department_field {
        let non_empty = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        if let Some(department) = non_empty(detail.department_name.as_deref())
            .or_else(|| non_empty(detail.department_id.as_deref()))
        {
            fields.push((field, Value::String(department)));
        }
    }

    if fields.is_empty() {
        tracing::info!(
            record_id = input.record_id,
            "扩展回填无可写字段，跳过本次回写"
        );
        return Ok(());
    }
    backfill.write_fields(input.record_id, &fields).await
}

/// 字符串毫秒时间戳 → `i64`。`"0"`（未完成）、空串、缺失、非数字一律 `None`。
fn millis_of(raw: Option<&str>) -> Option<i64> {
    let millis = raw?.trim().parse::<i64>().ok()?;
    (millis != 0).then_some(millis)
}

/// 实例状态 → 中文。未知枚举返回 `None`（不写，不猜）并记 warn。
///
/// `reverted` 加「(已撤销)」后缀——官方语义是「单据是否被撤销」；「已退回」
/// 在飞书是 task 级 `ROLLBACK` 状态，与此无关，不要混用。
///
/// 事件文档还列有 `REVERTED` / `OVERTIME_CLOSE` / `OVERTIME_RECOVER` 三个
/// 实例状态（get 接口文档的 5 值枚举里没有）——遇到时记 warn 不猜，靠日志发现。
fn status_text(detail: &InstanceDetail) -> Option<String> {
    let text = match detail.status.as_deref()?.trim() {
        "PENDING" => "审批中",
        "APPROVED" => "已通过",
        "REJECTED" => "已拒绝",
        "CANCELED" => "已取消",
        "DELETED" => "已删除",
        other => {
            tracing::warn!(status = other, "未知的审批实例状态枚举，跳过状态列回填");
            return None;
        }
    };
    Some(if detail.reverted == Some(true) {
        format!("{text}(已撤销)")
    } else {
        text.to_string()
    })
}

/// 当前处理人：`status == "PENDING"` 的节点的 `open_id`，按出现序去重。
fn pending_handlers(task_list: &[super::approval::TaskNode]) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut handlers = Vec::new();
    for node in task_list {
        if node.status.as_deref() != Some("PENDING") {
            continue;
        }
        let Some(open_id) = node
            .open_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        if seen.iter().any(|seen| seen == open_id) {
            continue;
        }
        seen.push(open_id.to_string());
        handlers.push(serde_json::json!({"id": open_id}));
    }
    handlers
}

/// 审批流程概要：已到达节点按 `task_list` 顺序拼 `node_name✓/✗/⏳`
/// （`DONE`/`APPROVED` → ✓、`PENDING` → ⏳、其余 → ✗），如「部门审批✓ → 财务审批⏳」。
/// 没有可拼的节点时返回 `None`（不写，不覆盖已有内容）。
fn flow_text(detail: &InstanceDetail) -> Option<String> {
    let parts: Vec<String> = detail
        .task_list
        .as_deref()?
        .iter()
        .filter_map(|node| {
            let name = node.node_name.as_deref()?.trim();
            if name.is_empty() {
                return None;
            }
            // task 级状态：get 接口文档明写 task_list[].status 可为 APPROVED
            // （通过）与 DONE（完成）——不把 APPROVED 映射成 ✓ 会把已通过的
            // 节点渲染成 ✗。
            let mark = match node.status.as_deref() {
                Some("DONE") | Some("APPROVED") => "✓",
                Some("PENDING") => "⏳",
                _ => "✗",
            };
            Some(format!("{name}{mark}"))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" → "))
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
    tracing::warn!(kind = ?failure.kind, "审批出站失败，按错误类别决定重试或终止");
    let reason = super::approval_feedback::outbound_reason(&failure);
    match failure.kind {
        // 可重试：不写字段。
        FailureKind::Retry { .. } | FailureKind::TokenExpired => {
            DispatchResult::Retryable { message: reason }
        }
        // uuid 冲突出现在这里说明它来自取值阶段（创建之后），此时按 uuid 反查
        // 才是正解；但调用点已经在创建分支处理过冲突，走到这里只能是别处冒出来的
        // 冲突——一律判可重试，把决定权交给下一轮的创建分支。
        FailureKind::UuidConflict => DispatchResult::Retryable {
            message: format!("uuid 冲突需反查实例（uuid={uuid}）：{reason}"),
        },
        FailureKind::InstanceNotFound => DispatchResult::Retryable { message: reason },
        // 终态：写回填字段。文案要**裁剪**——多维表格的读者范围远大于运维，
        // 原样回灌可能含内部 request_id、租户信息。
        FailureKind::Fatal { code } => {
            let message = sanitize_terminal_message(&reason, code);
            // 回写失败也不改变结论：终态的判据是「这个记录本身有问题」，
            // 而错误文案写不进去只是让用户少了一个排查线索。
            if let Err(failure) = backfill
                .write(
                    input.record_id,
                    input.backfill_field_name,
                    &Value::String(message.clone()),
                )
                .await
            {
                tracing::warn!(stage = "error_backfill", kind = ?failure.kind, "审批错误原因回填失败");
            }
            DispatchResult::Terminal { message }
        }
    }
}

/// 转换错误含原始单元格、电话或选项文案，日志只保留控件定位与错误类别。
fn log_conversion_error(error: &super::approval_convert::ConvertError, widgets: &[WidgetMap]) {
    use super::approval_convert::ConvertError;
    let (widget_id, reason) = match error {
        ConvertError::MissingRequired { widget_id } => (widget_id.as_str(), "缺少必填值"),
        ConvertError::BadValue { widget_id, .. } => {
            (widget_id.as_str(), "字段值形态不符合控件要求")
        }
        ConvertError::UnmappedOption { widget_id, .. } => (widget_id.as_str(), "选项没有配置映射"),
        ConvertError::Unsupported { widget_id, .. } => (widget_id.as_str(), "控件不支持 API 提单"),
        ConvertError::ConverterMismatch { widget_id, .. } => {
            (widget_id.as_str(), "转换器与控件类型不匹配")
        }
        ConvertError::InvalidDefinition(_) => ("", "审批定义快照与映射不一致"),
        ConvertError::UnknownConverter(_) => ("", "转换器标识非法"),
    };
    let widget = widgets.iter().find(|widget| widget.widget_id == widget_id);
    tracing::warn!(
        stage = "form",
        widget_id,
        reason,
        bitable_field = widget.map(|widget| widget.bitable_field.as_str()),
        widget_type = widget.map(|widget| widget.widget_type.as_str()),
        "审批表单转换失败"
    );
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
        // code=0 仅用于本地安全说明；未知飞书码不回灌响应原文。
        None if code == 0 => super::approval_feedback::truncate(raw, 950),
        None => "飞书拒绝本次请求，请管理员核对配置和权限。".into(),
    };
    if code == 0 {
        head
    } else {
        format!("[飞书 {code}] {head}")
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
            option_source_key: row
                .get("option_source_key")
                .and_then(Value::as_str)
                .map(str::to_string),
            interval_end_field: row
                .get("interval_end_field")
                .and_then(Value::as_str)
                .map(str::to_string),
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
    tracing::debug!(
        stage = "backfill_batch",
        row_count = rows.len(),
        chunk_size = super::bitable::BACKFILL_CHUNK,
        "审批编号开始分批回填"
    );
    let mut report = BackfillReport::default();
    for chunk in rows.chunks(super::bitable::BACKFILL_CHUNK) {
        write_chunk(backfill, field_id, chunk, &mut report).await;
    }
    tracing::debug!(
        stage = "backfill_batch",
        written = report.written.len(),
        failed = report.failed.len(),
        "审批编号分批回填结束"
    );
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
                tracing::warn!(stage = "backfill_batch", record_id, kind = ?failure.kind, "审批编号单条回填失败");
                report
                    .failed
                    .push((record_id.clone(), failure.message.clone()));
                return;
            }
            tracing::warn!(stage = "backfill_batch", row_count = chunk.len(), kind = ?failure.kind, "审批回填子批失败，拆分隔离失败记录");
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
    #[test]
    fn feedback_status_and_legacy_response_preserve_each_outcome() {
        use super::super::approval_feedback::FeedbackStatus;
        for (result, status, accepted, number) in [
            (
                DispatchResult::Backfilled {
                    instance_code: "INST".into(),
                    serial_number: "SN".into(),
                },
                FeedbackStatus::Succeeded,
                true,
                Some("SN"),
            ),
            (
                DispatchResult::Waiting {
                    reason: "补齐数据".into(),
                },
                FeedbackStatus::Waiting,
                true,
                None,
            ),
            (
                DispatchResult::CreatedPendingBackfill {
                    message: "审批编号：SN；待回填".into(),
                    instance_code: "INST".into(),
                    serial_number: Some("SN".into()),
                },
                FeedbackStatus::CreatedPendingBackfill,
                false,
                None,
            ),
            (
                DispatchResult::Retryable {
                    message: "稍后重试".into(),
                },
                FeedbackStatus::Retryable,
                false,
                None,
            ),
            (
                DispatchResult::Terminal {
                    message: "配置错误".into(),
                },
                FeedbackStatus::Failed,
                false,
                None,
            ),
        ] {
            assert_eq!(result.feedback_status(), status);
            let (actual, message, serial) = result.response_parts();
            assert_eq!(actual, accepted);
            assert_eq!(serial.as_deref(), number, "serial_number 只表示编号已回填");
            assert!(!message.is_empty());
        }
    }

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
            interval_end_field: None,
            converter: Converter::Direct,
            option_map: BTreeMap::new(),
            option_source_key: None,
            currency: None,
        }
    }

    fn cells(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn media_transfer_covers_attachments_and_images() {
        for kind in ["attachmentV2", "image", "imageV2"] {
            assert!(
                requires_media_transfer(&[widget("w", kind)]),
                "{kind} 应触发上传"
            );
        }
        assert!(!requires_media_transfer(&[widget("w", "input")]));
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
        assert_eq!(
            link,
            "https://applink.feishu.cn/client/mini_program/open?appId=cli_9cb844403dbb9108&mode=appCenter&path_pc=pc%2Fpages%2Fin-process%2Findex%3FinstanceId%3DINST-A%26source%3Dbitable&path=pages%2Fdetail%2Findex%3FinstanceId%3DINST-A%26source%3Dbitable"
        );
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
                    is_primary: false,
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
            is_primary: false,
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
            is_primary: false,
            field_id: "fldResult".into(),
            field_name: "审批编号".into(),
            field_type: Some(15),
            ui_type: None,
            property: None,
        }];
        // 本用例只验编号回填：发起人列写回会在成功创建后额外写一次（见
        // `write_extended`），所以在夹具里关掉它，让 `bodies[0]` 仍是编号。
        let cells = cells(&[]);
        let mut request = input(&cells, &[]);
        request.applicant = None;
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
                coordinates: request.coordinates,
                fields: &fields,
            };
            let result = finish_backfill(
                &writer,
                &request,
                "source-uuid",
                InstanceDetail {
                    instance_code: code.into(),
                    approval_code: None,
                    serial_number: serial.map(str::to_string),
                    status: None,
                    start_time: None,
                    end_time: None,
                    open_id: None,
                    user_id: None,
                    department_id: None,
                    department_name: None,
                    uuid: None,
                    reverted: None,
                    task_list: None,
                    timeline: None,
                    form: None,
                },
            )
            .await;
            assert_eq!(matches!(result, DispatchResult::Backfilled { .. }), success);
            if !success {
                assert!(
                    matches!(&result, DispatchResult::CreatedPendingBackfill { instance_code, serial_number, .. }
                    if instance_code == code && serial_number.as_deref() == serial)
                );
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
        assert_eq!(
            transport.requests(),
            vec![
                "Post https://open.feishu.cn/open-apis/bitable/v1/apps/appTest/tables/tblTest/records/batch_get"
            ]
        );
    }

    /// 关联列解析用的字段表：关联列 + 回填列。
    fn relation_fields(kind: i32, table_id: Option<&str>) -> Vec<FieldItem> {
        vec![
            FieldItem {
                is_primary: false,
                field_id: "fldRel".into(),
                field_name: "关联审批/Related Approval Form".into(),
                field_type: Some(kind),
                ui_type: Some("SingleLink".into()),
                property: Some(super::super::bitable::FieldProperty {
                    options: Vec::new(),
                    table_id: table_id.map(str::to_string),
                }),
            },
            FieldItem {
                is_primary: false,
                field_id: "fldBack".into(),
                field_name: "申请编号".into(),
                field_type: Some(15),
                ui_type: Some("Url".into()),
                property: None,
            },
        ]
    }

    fn connect_on_relation() -> WidgetMap {
        let mut widget = widget("w1", "connect");
        widget.bitable_field = "fldRel".into();
        widget
    }

    #[tokio::test]
    async fn relation_connect_cells_resolve_linked_records_to_instance_codes() {
        let code = "19EAC829-F1CB-527F-BE2A-1330422E60C0";
        let link = super::super::approval::instance_applink(code).unwrap_or_else(|e| panic!("{e}"));
        let body = json!({"code":0,"data":{"records":[{
            "record_id":"recLinked",
            "fields":{"申请编号":{"text":"202601010001","link":link}}
        }]}})
        .to_string();
        let transport = ScriptedTransport::new(vec![(200, body.as_str())]);
        let resolved = resolve_relation_connect_cells(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &coordinates(),
            &relation_fields(18, Some("tblsRc9GRRX")),
            &[connect_on_relation()],
            &cells(&[("fldRel", json!({"link_record_ids":["recLinked"]}))]),
            "申请编号",
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("应解析出实例 Code"));
        assert_eq!(resolved["fldRel"], json!([code]));
        assert_eq!(
            transport.requests(),
            vec![
                "Post https://open.feishu.cn/open-apis/bitable/v1/apps/appbcbWCzen6/tables/tblsRc9GRRX/records/batch_get"
            ]
        );
    }

    #[tokio::test]
    async fn relation_connect_cells_report_missing_serial_and_foreign_table() {
        // 被关联行还没有申请编号：数据问题，等它先派发完。
        let body = json!({"code":0,"data":{"records":[
            {"record_id":"recLinked","fields":{"申请编号":null}}
        ]}})
        .to_string();
        let transport = ScriptedTransport::new(vec![(200, body.as_str())]);
        let problem = resolve_relation_connect_cells(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &coordinates(),
            &relation_fields(18, Some("tblsRc9GRRX")),
            &[connect_on_relation()],
            &cells(&[("fldRel", json!({"link_record_ids":["recLinked"]}))]),
            "申请编号",
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("缺申请编号应判数据问题"));
        assert!(
            matches!(problem, RelationProblem::Data(ref reason) if reason.contains("还没有申请编号")),
            "{problem:?}"
        );

        // 跨表关联列：直接说清楚，且不打出站请求。
        let transport = ScriptedTransport::new(vec![]);
        let problem = resolve_relation_connect_cells(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &coordinates(),
            &relation_fields(18, Some("tblOther")),
            &[connect_on_relation()],
            &cells(&[("fldRel", json!({"link_record_ids":["recLinked"]}))]),
            "申请编号",
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("跨表关联应判数据问题"));
        assert!(
            matches!(problem, RelationProblem::Data(ref reason) if reason.contains("其它数据表")),
            "{problem:?}"
        );
        assert!(transport.requests().is_empty());

        // 已经是超链接单元格（历史写法）：原样交给原有解析，不发起读取。
        let transport = ScriptedTransport::new(vec![]);
        let none = resolve_relation_connect_cells(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &coordinates(),
            &relation_fields(18, Some("tblsRc9GRRX")),
            &[connect_on_relation()],
            &cells(&[(
                "fldRel",
                json!({"link":"https://example.com","text":"申请编号"}),
            )]),
            "申请编号",
        )
        .await
        .ok()
        .flatten();
        assert!(none.is_none());
        assert!(transport.requests().is_empty());
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
        writes: Mutex<Vec<(String, String, Value)>>,
        /// `write_fields` 的调用形态（record_id, 字段列表）——钉住「合并为一次」。
        batches: Mutex<Vec<(String, Vec<String>)>>,
        fail: bool,
        fail_fields: bool,
        /// 毒记录名单：这批里出现任一即让整批失败（复刻全有全无语义）。
        poison: Mutex<Vec<String>>,
    }

    impl RecordingBackfill {
        fn failing() -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
                batches: Mutex::new(Vec::new()),
                fail: true,
                fail_fields: false,
                poison: Mutex::new(Vec::new()),
            }
        }

        fn with_poison(record_id: &str) -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
                batches: Mutex::new(Vec::new()),
                fail: false,
                fail_fields: false,
                poison: Mutex::new(vec![record_id.to_string()]),
            }
        }

        /// 写入调用原文（记录 id、字段、**原值**）。
        fn raw_writes(&self) -> Vec<(String, String, Value)> {
            self.writes
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }

        /// 回填写入折成文本（`{"text": …}` 取 text，其余原样 `as_str`）。
        ///
        /// **不含 `fld_applicant`**：发起人列写回是每次成功创建都会发生的固定动作
        /// （见 [`write_extended`]），把它算进来会让每条「编号回填了一次」的断言都
        /// 变成 2。发起人写回单独用 [`Self::raw_writes`] 断言。
        fn writes(&self) -> Vec<(String, String, String)> {
            self.raw_writes()
                .into_iter()
                .filter(|(_, field, _)| field != "fld_applicant")
                .map(|(record_id, field, value)| {
                    (
                        record_id,
                        field,
                        value
                            .get("text")
                            .unwrap_or(&value)
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        }

        /// `write_fields` 的调用形态（record_id, 字段列表）——钉住「合并为一次」。
        fn batches(&self) -> Vec<(String, Vec<String>)> {
            self.batches
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
                    writes.push((record_id.clone(), field_id.to_string(), value.clone()));
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

        async fn write_fields(
            &self,
            record_id: &str,
            fields: &[(&str, Value)],
        ) -> Result<(), OutboundFailure> {
            if let Ok(mut writes) = self.writes.lock() {
                for (field_id, value) in fields {
                    writes.push((record_id.to_string(), field_id.to_string(), value.clone()));
                }
            }
            if let Ok(mut batches) = self.batches.lock() {
                batches.push((
                    record_id.to_string(),
                    fields
                        .iter()
                        .map(|(field_id, _)| field_id.to_string())
                        .collect(),
                ));
            }
            if self.fail || self.fail_fields {
                return Err(OutboundFailure {
                    kind: FailureKind::Retry {
                        retry_after_seconds: None,
                    },
                    message: "回写失败".to_string(),
                });
            }
            Ok(())
        }
    }

    fn input<'a>(
        cells: &'a serde_json::Map<String, Value>,
        widgets: &'a [WidgetMap],
    ) -> DispatchInput<'a> {
        DispatchInput {
            fields: &[],
            context: None,
            execution: None,
            coordinates: &COORDINATES,
            record_id: "rec001",
            cells,
            applicant_field: "fld_applicant",
            // 发起人只来自请求（点击人）。夹具统一给一个，用 `ou_abc` 与各用例里
            // 「申请人列」的值区分开——两者相同会让「到底读了谁」验不出来。
            applicant: Some("ou_abc".to_string()),
            backfill_field_name: "审批编号",
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
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

    /// 带 `department_id` 的详情（bot 形态：非 `od-` 前缀）。
    fn ok_detail_with_department(instance_code: &str, serial: &str, department: &str) -> String {
        format!(
            r#"{{"code":0,"msg":"ok","data":{{"instance_code":"{instance_code}","serial_number":"{serial}","status":"PENDING","department_id":"{department}"}}}}"#
        )
    }

    /// 部门查询的成功响应（`data.department.name`）。
    fn department_body(name: &str) -> String {
        format!(r#"{{"code":0,"msg":"ok","data":{{"department":{{"name":"{name}"}}}}}}"#)
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
                ("fld_applicant", json!([{"id":"ou_abc"}])),
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
                ("fld_applicant", json!([{"id":"ou_abc"}])),
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
            ("fld_applicant", json!([{"id":"ou_abc"}])),
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
                    serial_number: "SERIAL-A".to_string(),
                    instance_code: "INST-A".to_string()
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
                serial_number: "SERIAL".to_string(),
                instance_code: "EXISTING".to_string()
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
        let cells = cells(&[("fld_applicant", json!([{"id":"ou_abc"}]))]);
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
                serial_number: "202609280001".to_string(),
                instance_code: "INST-1".to_string()
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

    /// 部门名**接线**：`dispatch_one` 必须在回填前真的解析部门名。
    ///
    /// 这条用例存在的理由：取值链（`department_cell_prefers_*`）与解析函数
    /// （`department_name_*`）各自有单测，但「`dispatch_one` 把两者接起来」此前
    /// **零覆盖**——夹具的详情没有 `department_id`，解析在守卫处就返回了，
    /// 于是删掉调用点、或把守卫写反，其余用例照样全绿。真机上那种断线的表现
    /// 恰好是「悄悄写回 ID」，最难发现。
    #[tokio::test]
    async fn dispatch_one_resolves_department_name_before_backfilling() {
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-1")),
            (
                200,
                &ok_detail_with_department("INST-1", "202609280001", "db59a45c851464gf"),
            ),
            (200, &department_body("研发部")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);
        let mut request = input(&cells, &widgets);
        request.department_field = Some("fld_department");

        let result = dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;

        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "{result:?}"
        );
        let department = backfill
            .raw_writes()
            .into_iter()
            .find(|(_, field, _)| field == "fld_department")
            .map(|(_, _, value)| value);
        assert_eq!(
            department,
            Some(json!("研发部")),
            "接线断了：部门列写回的不是解析出的名字"
        );
        assert!(
            transport
                .requests()
                .iter()
                .any(|url| url.contains("/contact/v3/departments/db59a45c851464gf")),
            "应发生一次部门查询：{:?}",
            transport.requests()
        );
    }

    /// 未配部门列 → **零** contact 调用（脚本只喂了 create + detail 两条响应，
    /// 多打一次请求就会耗尽脚本并把结论打成 Retryable，断言同时钉住两件事）。
    #[tokio::test]
    async fn dispatch_one_skips_department_lookup_without_the_field() {
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-1")),
            (
                200,
                &ok_detail_with_department("INST-1", "202609280001", "db59a45c851464gf"),
            ),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_abc"}])),
            ("fld_title", json!("报销")),
        ]);

        let result = dispatch_one(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &input(&cells, &widgets),
        )
        .await;

        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "未配部门列不得多打请求：{result:?}"
        );
        assert!(
            !transport
                .requests()
                .iter()
                .any(|url| url.contains("/contact/v3/departments/")),
            "未配部门列时零 contact 调用：{:?}",
            transport.requests()
        );
    }

    /// 恢复路径（带 connect/attachment 控件时先按 uuid 反查实例）**也**要接上
    /// 部门名解析——它是 `dispatch_one` 里提前 `return` 的另一条分支，主路径的
    /// 接线测试覆盖不到它。
    #[tokio::test]
    async fn recover_path_also_resolves_department_name() {
        let transport = ScriptedTransport::new(vec![
            (
                200,
                &ok_detail_with_department("INST-R", "202609280009", "db59a45c851464gf"),
            ),
            (200, &department_body("研发部")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "connect")];
        let cells = cells(&[("fld_applicant", json!([{"id": "ou_abc"}]))]);
        let mut request = input(&cells, &widgets);
        request.department_field = Some("fld_department");

        let result = dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;

        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "{result:?}"
        );
        let department = backfill
            .raw_writes()
            .into_iter()
            .find(|(_, field, _)| field == "fld_department")
            .map(|(_, _, value)| value);
        assert_eq!(department, Some(json!("研发部")), "恢复路径漏了部门名接线");
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
                serial_number: "202609280042".to_string(),
                instance_code: "INST-RECOVERED".to_string()
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
    async fn missing_initiator_waits_without_any_outbound() {
        // 没有发起人身份的任务（上线前的存量行 / 受理与落库之间的窄窗口）：
        // 等待、**零出站**，且不写任何字段。表格申请人列填着人也不再被读——
        // 读它就等于允许替别人发起。
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[
            ("fld_applicant", json!([{"id": "ou_someone_else"}])),
            ("fld_title", json!("报销")),
        ]);
        let mut request = input(&cells, &widgets);
        request.applicant = None;

        let result = dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;

        assert!(
            matches!(&result, DispatchResult::Waiting { reason } if reason.contains("发起人身份")),
            "{result:?}"
        );
        assert!(transport.requests().is_empty(), "等待态不得发出站请求");
        assert!(backfill.writes().is_empty(), "等待态不得写字段");
    }

    #[test]
    fn applicant_field_conflict_only_flags_known_other_people() {
        let clicker = "ou_clicker";
        let matching: [Option<Value>; 9] = [
            None,
            Some(json!(null)),
            Some(json!([])),
            Some(json!("")),
            Some(json!("张三")),
            Some(json!([{"id": "ou_clicker"}])),
            Some(json!([{"id": "ou_clicker", "name": "我"}, {"id": "ou_other"}])),
            // v3 batch_get 对公式/查找字段多包一层；剥壳后必须仍能匹配。
            Some(json!({"type": 11, "value": [{"id": "ou_clicker"}]})),
            // 单人对象：非标准但可能来自改型/手工数据，不能静默放行。
            Some(json!({"id": "ou_clicker", "name": "我"})),
        ];
        for cell in &matching {
            assert_eq!(
                applicant_field_conflict(cell.as_ref(), clicker),
                None,
                "{cell:?} 应视为匹配"
            );
        }
        assert_eq!(
            applicant_field_conflict(Some(&json!([{"id": "ou_other", "name": "张三"}])), clicker),
            Some(vec!["张三".to_string()])
        );
        // `{type,value}` 包装内的对象数组/裸字符串都要剥壳后再判定。
        assert_eq!(
            applicant_field_conflict(
                Some(&json!({"type": 11, "value": [{"id": "ou_other", "name": "张三"}]})),
                clicker
            ),
            Some(vec!["张三".to_string()])
        );
        assert_eq!(
            applicant_field_conflict(Some(&json!({"type": 2, "value": "ou_a"})), clicker),
            Some(vec!["ou_a".to_string()])
        );
        assert_eq!(
            applicant_field_conflict(Some(&json!({"id": "ou_other", "name": "张三"})), clicker),
            Some(vec!["张三".to_string()])
        );
        // 裸 `ou_` 文本按人员 id 收紧处理；没有名字时用 id 兜底。
        assert_eq!(
            applicant_field_conflict(Some(&json!("ou_a,ou_b")), clicker),
            Some(vec!["ou_a".to_string(), "ou_b".to_string()])
        );
    }

    #[tokio::test]
    async fn applicant_mismatch_terminates_without_any_outbound() {
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!([{"id": "ou_other", "name": "张三"}]))]);
        let request = input(&cells, &widgets);

        let result = dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await;

        assert!(
            matches!(&result, DispatchResult::Terminal { message }
                if !message.contains("张三")
                    && message.contains("1 人")
                    && message.contains("申请人")),
            "兜底文案只报条数、不列姓名（可能来自批量播种的跨权限记录）: {result:?}"
        );
        assert!(transport.requests().is_empty(), "申请人冲突不得发出站请求");
        assert!(backfill.writes().is_empty(), "申请人冲突不得写回填列");
    }

    #[tokio::test]
    async fn applicant_mismatch_empty_cell_still_dispatches() {
        // 空列/非人员值按「未填」处理：成功创建后由 `write_extended` 覆盖成点击人。
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-E")),
            (200, &ok_detail("INST-E", "SERIAL-E")),
        ]);
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!("张三（手填）"))]);

        assert!(matches!(
            dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &backfill,
                &input(&cells, &widgets)
            )
            .await,
            DispatchResult::Backfilled { .. }
        ));
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
    async fn serial_number_missing_after_create_preserves_created_fact() {
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
            matches!(&result, DispatchResult::CreatedPendingBackfill { instance_code, serial_number: None, .. } if instance_code == "INST-2"),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty(), "可重试不得写字段");
    }

    #[tokio::test]
    async fn instance_not_found_right_after_create_preserves_created_fact() {
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
            matches!(&result, DispatchResult::CreatedPendingBackfill { instance_code, serial_number: None, .. } if instance_code == "INST-3"),
            "{result:?}"
        );
        assert!(backfill.writes().is_empty());
    }

    #[tokio::test]
    async fn detail_failure_after_create_preserves_instance_without_terminal_write() {
        for (status, error, attempts) in [
            (400, r#"{"code":999999,"msg":"private-detail-error"}"#, 1),
            (503, r#"{"code":1395001,"msg":"private-detail-error"}"#, 3),
        ] {
            let create = ok_create("INST-4");
            let mut responses = vec![(200, create.as_str())];
            responses.extend(std::iter::repeat((status, error)).take(attempts));
            let transport = ScriptedTransport::new(responses);
            let backfill = RecordingBackfill::default();
            let widgets = [widget("w1", "input")];
            let cells = cells(&[
                ("fld_applicant", json!([{"id":"ou_abc"}])),
                ("fld_title", json!("付款")),
            ]);
            let result = dispatch_one(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &backfill,
                &input(&cells, &widgets),
            )
            .await;
            assert!(
                matches!(&result,
                DispatchResult::CreatedPendingBackfill { instance_code, serial_number: None, .. }
                if instance_code == "INST-4"),
                "{result:?}"
            );
            assert!(
                backfill.writes().is_empty(),
                "确认实例后不得把详情错误写成终态"
            );
            assert_eq!(transport.requests().len(), attempts + 1);
            assert!(!result.response_parts().1.contains("private-detail-error"));
        }
    }

    #[tokio::test]
    async fn classified_failures_mask_raw_body_and_keep_retry_semantics() {
        let cells = cells(&[]);
        let input = input(&cells, &[]);
        let raw =
            r#"{"msg":"private-outbound","request_id":"req_secret","access_token":"token-secret"}"#;
        for (kind, hint, terminal) in [
            (
                FailureKind::Retry {
                    retry_after_seconds: None,
                },
                "稍后重试",
                false,
            ),
            (FailureKind::TokenExpired, "凭证", false),
            (FailureKind::UuidConflict, "UUID", false),
            (FailureKind::InstanceNotFound, "审批详情", false),
            (FailureKind::Fatal { code: 1390001 }, "1390001", true),
            (FailureKind::Fatal { code: 999999 }, "999999", true),
            (FailureKind::Fatal { code: 0 }, "飞书", true),
        ] {
            let backfill = RecordingBackfill::default();
            let result = classify_failure(
                OutboundFailure {
                    kind,
                    message: raw.into(),
                },
                &input,
                &backfill,
                "source-uuid",
            )
            .await;
            assert_eq!(matches!(&result, DispatchResult::Terminal { .. }), terminal);
            assert_eq!(
                matches!(&result, DispatchResult::Retryable { .. }),
                !terminal
            );
            let message = result.response_parts().1;
            assert!(message.contains(hint), "{kind:?}: {message}");
            for marker in ["private-outbound", "req_secret", "token-secret", "{", "}"] {
                assert!(!message.contains(marker), "{kind:?}: {message}");
            }
            let writes = backfill.writes();
            assert_eq!(writes.len(), usize::from(terminal));
            if terminal {
                assert_eq!(writes[0].2, message);
            }
        }
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
        assert!(widgets[0].option_source_key.is_none(), "旧行保持兼容");
        let mut saved_rows = rows.clone();
        saved_rows[0].insert("option_source_key".into(), json!("fldq7lcb6y"));
        saved_rows[0].insert("interval_end_field".into(), json!("fld_end"));
        let saved =
            widget_maps_from_rows(&saved_rows, |_| None).unwrap_or_else(|| panic!("应可解析"));
        assert_eq!(saved[0].option_source_key.as_deref(), Some("fldq7lcb6y"));
        assert!(widgets[0].interval_end_field.is_none(), "旧行保持兼容");
        assert_eq!(saved[0].interval_end_field.as_deref(), Some("fld_end"));
        assert_eq!(widgets.len(), 1);
        assert!(widgets[0].required);
        assert_eq!(
            widgets[0].option_map.get("已批准").map(String::as_str),
            Some("va")
        );
    }

    // ---- 可选回填扩展（设计 §5） ----

    /// 单选 / 日期 / 人员三臂的写入形态（真机探针与文档定论）。
    #[tokio::test]
    async fn select_date_and_person_arms_write_native_shapes() {
        for (kind, value, expected) in [
            // 单选：**裸字符串**（真机探针定论：数组被 1254062 拒，字符串可且选项
            // 自动创建）。`{"text": …}` 对象取 text。
            (3, json!({"text":"审批中"}), json!("审批中")),
            (3, json!("审批中"), json!("审批中")),
            // 日期：number 毫秒；详情接口的 start_time 是**字符串**，先转换。
            (5, json!("1790750409857"), json!(1790750409857i64)),
            (5, json!(1790750409857i64), json!(1790750409857i64)),
            // 人员：`[{"id":"ou_.."}]` 数组原样。
            (11, json!([{"id":"ou_x"}]), json!([{"id":"ou_x"}])),
        ] {
            let transport = ScriptedTransport::new(vec![(200, r#"{"code":0,"data":{}}"#)]);
            let fields = [FieldItem {
                is_primary: false,
                field_id: "fldT".into(),
                field_name: "回填列".into(),
                field_type: Some(kind),
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
            assert!(
                writer.write("recOne", "fldT", &value).await.is_ok(),
                "kind={kind} value={value}"
            );
            let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                bodies[0]
                    .as_ref()
                    .map(|body| body["records"][0]["fields"]["回填列"].clone()),
                Some(expected),
                "kind={kind} value={value}"
            );
        }
        // 形态不符 → 终态错误，且**不发请求**（别把坏值送出去）。
        for (kind, bad) in [
            (3, json!(123)),
            (5, json!("不是数字")),
            (5, json!(null)),
            (11, json!("ou_x")),
            // 其余类型（如数字列 2）一律拒绝。
            (2, json!("x")),
        ] {
            let transport = ScriptedTransport::new(vec![]);
            let fields = [FieldItem {
                is_primary: false,
                field_id: "fldT".into(),
                field_name: "回填列".into(),
                field_type: Some(kind),
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
            assert!(
                writer.write("recOne", "fldT", &bad).await.is_err(),
                "kind={kind} value={bad}"
            );
            assert!(transport.requests().is_empty(), "kind={kind}");
        }
    }

    #[tokio::test]
    async fn required_extended_backfill_failure_keeps_confirmed_instance_pending() {
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!([{"id":"ou_abc"}]))]);
        let mut request = input(&cells, &widgets);
        request.status_field = Some("fld_status");
        let detail = InstanceDetail {
            status: Some("PENDING".into()),
            ..base_detail("INST-EXT", Some("SERIAL"))
        };
        let backfill = RecordingBackfill {
            fail_fields: true,
            ..Default::default()
        };
        let result = finish_backfill(&backfill, &request, "source-uuid", detail).await;
        assert!(
            matches!(result, DispatchResult::CreatedPendingBackfill { instance_code, serial_number: Some(serial), .. } if instance_code == "INST-EXT" && serial == "SERIAL")
        );
        assert_eq!(backfill.batches().len(), 1);
        assert_eq!(
            backfill.raw_writes()[0].1,
            "审批编号",
            "编号成功后扩展写失败仍不能确认完成"
        );
    }

    #[tokio::test]
    async fn required_extended_field_validation_cannot_silently_skip_a_column() {
        for (field_id, value) in [
            ("fldMissing", json!("x")),
            ("fldStatus", json!({"invalid":true})),
        ] {
            let transport = ScriptedTransport::new(vec![]);
            let fields = [FieldItem {
                is_primary: false,
                field_id: "fldStatus".into(),
                field_name: "状态".into(),
                field_type: Some(3),
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
                .write_fields("recOne", &[(field_id, value)])
                .await
                .is_err());
            assert!(transport.requests().is_empty(), "校验失败不能发部分更新");
        }
    }

    /// 详情 → 扩展字段的取值与写入（顺序、跳过规则、清列）。
    #[tokio::test]
    async fn finish_backfill_writes_extended_fields_after_the_serial_number() {
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!([{"id":"ou_abc"}]))]);
        let mut request = input(&cells, &widgets);
        request.start_time_field = Some("fld_start");
        request.end_time_field = Some("fld_end");
        request.status_field = Some("fld_status");
        request.handler_field = Some("fld_handler");
        request.flow_field = Some("fld_flow");
        request.department_field = Some("fld_department");
        let detail = InstanceDetail {
            status: Some("PENDING".to_string()),
            reverted: Some(true),
            start_time: Some("1790750409857".to_string()),
            // 未完成 = 字符串 "0"：**不写**（否则日期列显示 1970）。
            end_time: Some("0".to_string()),
            department_id: Some("db59a45c851464gf".to_string()),
            task_list: Some(vec![
                task("部门审批", "DONE", "ou_a"),
                task("财务审批", "PENDING", "ou_b"),
                // 同一处理人重复出现 → 去重；流程按节点逐条拼。
                task("法务", "PENDING", "ou_b"),
            ]),
            ..base_detail("INST-A", Some("SERIAL"))
        };
        let backfill = RecordingBackfill::default();
        assert_eq!(
            finish_backfill(&backfill, &request, "source-uuid", detail).await,
            DispatchResult::Backfilled {
                serial_number: "SERIAL".to_string(),
                instance_code: "INST-A".to_string()
            }
        );
        let writes = backfill.raw_writes();
        let fields: Vec<&str> = writes.iter().map(|(_, field, _)| field.as_str()).collect();
        assert_eq!(
            fields,
            vec![
                "审批编号",
                "fld_start",
                "fld_status",
                "fld_handler",
                "fld_flow",
                // 发起人列由系统覆盖成点击人（`input()` 夹具的 `ou_abc`）。
                "fld_applicant",
                "fld_department",
            ],
            "完成时间 \"0\" 必须跳过：{writes:?}"
        );
        assert_eq!(writes[1].2, json!(1790750409857i64));
        assert_eq!(writes[2].2, json!("审批中(已撤销)"));
        assert_eq!(writes[3].2, json!([{"id":"ou_b"}]));
        assert_eq!(writes[4].2, json!("部门审批✓ → 财务审批⏳ → 法务⏳"));
        assert_eq!(
            writes[5].2,
            json!([{"id":"ou_abc"}]),
            "发起人列被点击人覆盖"
        );
        assert_eq!(writes[6].2, json!("db59a45c851464gf"));
        // 扩展字段必须**合并为一次**批量写（2026-10-15 优化：逐条 N 次串行
        // API 调用 → 1 次，实测 6 个字段省约 5s）；一次覆盖全部已收集字段。
        assert_eq!(
            backfill.batches(),
            vec![(
                "rec001".to_string(),
                vec![
                    "fld_start".to_string(),
                    "fld_status".to_string(),
                    "fld_handler".to_string(),
                    "fld_flow".to_string(),
                    "fld_applicant".to_string(),
                    "fld_department".to_string(),
                ],
            )],
            "扩展字段必须合并为一次 write_fields 调用"
        );

        // 已终态（无 PENDING）→ 处理人写空数组**清列**；未配的列一律不写。
        let mut request = input(&cells, &widgets);
        request.handler_field = Some("fld_handler");
        let detail = InstanceDetail {
            status: Some("APPROVED".to_string()),
            task_list: Some(vec![task("部门审批", "DONE", "ou_a")]),
            ..base_detail("INST-A", Some("SERIAL"))
        };
        let backfill = RecordingBackfill::default();
        assert!(matches!(
            finish_backfill(&backfill, &request, "source-uuid", detail).await,
            DispatchResult::Backfilled { .. }
        ));
        assert_eq!(
            backfill.raw_writes(),
            vec![
                (
                    "rec001".to_string(),
                    "审批编号".to_string(),
                    json!({"text":"SERIAL","link":super::super::approval::instance_applink("INST-A").unwrap_or_else(|e| panic!("{e}"))})
                ),
                ("rec001".to_string(), "fld_handler".to_string(), json!([])),
                // 发起人列随成功创建一起被覆盖成点击人（`input()` 夹具的 `ou_abc`）。
                (
                    "rec001".to_string(),
                    "fld_applicant".to_string(),
                    json!([{"id":"ou_abc"}])
                ),
            ]
        );
    }

    /// 恢复/重试路径：申请人列明确写着别人时**不覆盖**（实例已创建，发起人已固定；
    /// 列是人工修改，不能静默改回）。新建路径的空列/含点击人仍照常写回。
    #[tokio::test]
    async fn write_extended_preserves_a_foreign_applicant_cell() {
        let backfill = RecordingBackfill::default();
        let widgets = [widget("w1", "input")];
        let cells = cells(&[(
            "fld_applicant",
            json!({"type": 11, "value": [{"id": "ou_other", "name": "张三"}]}),
        )]);
        let mut request = input(&cells, &widgets);
        request.status_field = Some("fld_status");
        let detail = InstanceDetail {
            status: Some("PENDING".to_string()),
            ..base_detail("INST-A", Some("SERIAL"))
        };

        write_extended(&backfill, &request, &detail)
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(backfill.batches().len(), 1, "扩展字段仍合并为一次写入");
        let fields = &backfill.batches()[0].1;
        assert!(fields.contains(&"fld_status".to_string()), "{fields:?}");
        assert!(
            !fields.contains(&"fld_applicant".to_string()),
            "不得覆盖明确写着别人的申请人列: {fields:?}"
        );
    }

    /// 部门列：**优先写解析出的名称**；未解析（`department_name` 为 `None`，
    /// 无 scope / 部门不在可见范围时）退化为写原始 ID——信息不丢，用户至少
    /// 能看出这一列没解析成功。
    #[tokio::test]
    async fn department_cell_prefers_the_resolved_name_over_the_raw_id() {
        let widgets = [widget("w1", "input")];
        let cells = cells(&[("fld_applicant", json!([{"id":"ou_abc"}]))]);
        let mut request = input(&cells, &widgets);
        request.department_field = Some("fld_department");

        async fn department_cell(
            request: &DispatchInput<'_>,
            detail: InstanceDetail,
        ) -> Option<Value> {
            let backfill = RecordingBackfill::default();
            assert!(matches!(
                finish_backfill(&backfill, request, "source-uuid", detail).await,
                DispatchResult::Backfilled { .. }
            ));
            backfill
                .raw_writes()
                .into_iter()
                .find(|(_, field, _)| field == "fld_department")
                .map(|(_, _, value)| value)
        }

        let resolved = department_cell(
            &request,
            InstanceDetail {
                department_id: Some("db59a45c851464gf".to_string()),
                department_name: Some("研发部".to_string()),
                ..base_detail("INST-A", Some("SERIAL"))
            },
        )
        .await;
        assert_eq!(resolved, Some(json!("研发部")), "解析出名称时必须写名称");

        let fallback = department_cell(
            &request,
            InstanceDetail {
                department_id: Some("db59a45c851464gf".to_string()),
                ..base_detail("INST-A", Some("SERIAL"))
            },
        )
        .await;
        assert_eq!(
            fallback,
            Some(json!("db59a45c851464gf")),
            "未解析时退化为写 ID"
        );
    }

    #[test]
    fn flow_text_renders_approved_task_as_done() {
        // get 接口文档明写 task_list[].status 可为 APPROVED（通过）——它必须
        // 渲染成 ✓，否则「已通过的任务」会显示成 ✗（2026-10-14 文档核对发现）。
        let detail = InstanceDetail {
            task_list: Some(vec![
                task("部门审批", "APPROVED", "ou_a"),
                task("财务审批", "PENDING", "ou_b"),
            ]),
            ..base_detail("INST-FLOW", None)
        };
        assert_eq!(
            flow_text(&detail).as_deref(),
            Some("部门审批✓ → 财务审批⏳")
        );
    }

    /// 发起人只来自请求参数：空白/缺失时等待（**不**回读表格申请人列）。
    #[tokio::test]
    async fn applicant_param_drives_the_create_request_and_is_written_back() {
        let widgets = [widget("w1", "input")];
        // cells 里**没有**申请人也能创建，create 请求体的 open_id 是传参值（trim 后）。
        // 上层已把姓名解析成 open_id，这里不该再做任何解析。
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-P")),
            (200, &ok_detail("INST-P", "SERIAL-P")),
        ]);
        let backfill = RecordingBackfill::default();
        let cell_map = cells(&[("fld_title", json!("报销"))]);
        let mut request = input(&cell_map, &widgets);
        request.applicant = Some("  ou_from_param  ".to_string());
        assert_eq!(
            dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await,
            DispatchResult::Backfilled {
                serial_number: "SERIAL-P".to_string(),
                instance_code: "INST-P".to_string()
            }
        );
        {
            let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                bodies[0].as_ref().map(|body| body["open_id"].clone()),
                Some(json!("ou_from_param"))
            );
        }
        // §6.4：发起人列被**覆盖**成点击人。
        {
            let writes = backfill.raw_writes();
            assert_eq!(
                writes
                    .iter()
                    .find(|(_, field, _)| field == "fld_applicant")
                    .map(|(_, _, value)| value.clone()),
                Some(json!([{"id": "ou_from_param"}])),
                "创建成功后必须覆盖发起人列"
            );
        }

        // 空白串等同缺失 → 等待、零出站；表格列的 `ou_cell` **不被采纳**。
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();
        let cell_map = cells(&[("fld_applicant", json!([{"id":"ou_cell"}]))]);
        let mut request = input(&cell_map, &widgets);
        request.applicant = Some("   ".to_string());
        assert!(matches!(
            dispatch_one(&transport, &NoSleep, &fake_tokens(), &backfill, &request).await,
            DispatchResult::Waiting { .. }
        ));
        assert!(
            transport.requests().is_empty(),
            "没传发起人不得读表/建单：{:#?}",
            transport.requests()
        );
        assert!(backfill.writes().is_empty());
    }

    /// 最小可用详情；扩展字段按需用结构体更新语法覆盖。
    fn base_detail(instance_code: &str, serial: Option<&str>) -> InstanceDetail {
        InstanceDetail {
            instance_code: instance_code.to_string(),
            approval_code: None,
            serial_number: serial.map(str::to_string),
            status: None,
            start_time: None,
            end_time: None,
            open_id: None,
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted: None,
            task_list: None,
            timeline: None,
            form: None,
        }
    }

    fn task(node_name: &str, status: &str, open_id: &str) -> super::super::approval::TaskNode {
        super::super::approval::TaskNode {
            id: None,
            node_id: None,
            node_name: Some(node_name.to_string()),
            user_id: None,
            open_id: Some(open_id.to_string()),
            status: Some(status.to_string()),
            r#type: None,
            start_time: None,
            end_time: None,
        }
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

    // ---- 聚合组派发（Task 8） ----

    /// 组夹具：第一条是 leader（`group_seq=0`），其余是成员。
    struct GroupFixture {
        member_cells: Vec<serde_json::Map<String, Value>>,
        widgets: Vec<WidgetMap>,
        group_key: String,
        group_uuid: String,
    }

    impl GroupFixture {
        fn new() -> Self {
            let mut top = widget("top", "input");
            top.bitable_field = "fld_top".to_string();
            let mut desc = widget("desc", "textarea");
            desc.bitable_field = "fld_desc".to_string();
            Self {
                member_cells: vec![
                    cells(&[("fld_top", json!("共同")), ("fld_desc", json!("明细 A"))]),
                    cells(&[("fld_top", json!("共同")), ("fld_desc", json!("明细 B"))]),
                ],
                widgets: vec![top, desc],
                group_key: "gk00000000000000000000000000test".to_string(),
                group_uuid: "0f7a65c4-2c56-4a05-9bd3-57e0f1f75b3b".to_string(),
            }
        }

        fn members(&self) -> Vec<GroupMemberInput<'_>> {
            self.member_cells
                .iter()
                .enumerate()
                .map(|(index, cells)| GroupMemberInput {
                    record_id: if index == 0 { "recA" } else { "recB" },
                    cells,
                    attachment_codes: BTreeMap::new(),
                })
                .collect()
        }
    }

    /// 组装组输入；`members` 由调用方提供（生命周期跟着夹具走）。
    fn group_input<'a>(
        fixture: &'a GroupFixture,
        members: &'a [GroupMemberInput<'a>],
    ) -> GroupDispatchInput<'a> {
        GroupDispatchInput {
            fields: &[],
            context: None,
            execution: None,
            coordinates: &COORDINATES,
            group_key: &fixture.group_key,
            group_uuid: &fixture.group_uuid,
            instance_code: None,
            backfill_field_name: "审批编号",
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
            approval_code: "4202AD96-9EC1",
            widgets: &fixture.widgets,
            form_snapshot: r#"[{"id":"top","type":"input"},{"id":"detail","type":"fieldList","children":[{"id":"desc","type":"textarea"}]}]"#,
            timezone_offset: FixedOffset::from_seconds(8 * 3600)
                .unwrap_or_else(|error| panic!("{error}")),
            applicant_field: "fld_applicant",
            applicant_open_id: Some("ou_clicker"),
            members,
        }
    }

    #[tokio::test]
    async fn group_dispatch_sends_one_create_with_two_detail_rows() {
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-GROUP")),
            (200, &ok_detail("INST-GROUP", "SERIAL-G")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let result = dispatch_group(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &group_input(&fixture, &members),
        )
        .await;
        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "SERIAL-G".to_string(),
                instance_code: "INST-GROUP".to_string(),
            }
        );
        let requests = transport
            .full_requests
            .lock()
            .unwrap_or_else(|e| panic!("{e}"));
        let creates: Vec<_> = requests
            .iter()
            .filter(|request| request.url.ends_with("/approval/v4/instances"))
            .collect();
        assert_eq!(creates.len(), 1, "一次 create 覆盖整组");
        assert_eq!(
            creates[0].json_body.as_ref().map(|body| &body["uuid"]),
            Some(&json!(fixture.group_uuid)),
            "create 的幂等键必须是组 uuid"
        );
        let form: Value = serde_json::from_str(
            creates[0]
                .json_body
                .as_ref()
                .unwrap_or_else(|| panic!("缺创建请求"))["form"]
                .as_str()
                .unwrap_or_default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            form[1]["value"],
            json!([
                [{"id":"desc","type":"textarea","value":"明细 A"}],
                [{"id":"desc","type":"textarea","value":"明细 B"}]
            ]),
            "明细必须一成员一行"
        );
    }

    #[tokio::test]
    async fn group_dispatch_never_writes_instance_code_to_leader() {
        // leader 任务行永远不持有组实例码：组确认由 GroupExecutionTransport 写组行。
        // 这里钉住编排层**不把实例码塞进任何任务行字段**——返回的 instance_code 只
        // 供 worker 写组行，成员/leader 行不出现在回填写入里。
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-ONLY-GROUP")),
            (200, &ok_detail("INST-ONLY-GROUP", "SERIAL-G")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        assert!(matches!(
            dispatch_group(
                &transport,
                &NoSleep,
                &fake_tokens(),
                &backfill,
                &group_input(&fixture, &members)
            )
            .await,
            DispatchResult::Backfilled { .. }
        ));
        assert!(
            backfill.raw_writes().iter().all(|(_, field, value)| {
                field != "instance_code" && value.as_str() != Some("INST-ONLY-GROUP")
            }),
            "组实例码不得作为任何单元格的值：{:?}",
            backfill.raw_writes()
        );
    }

    #[tokio::test]
    async fn group_dispatch_recovers_by_group_uuid_on_60012() {
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (400, &uuid_conflict()),
            (200, &ok_detail("INST-RECOVERED", "SERIAL-R")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let result = dispatch_group(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &group_input(&fixture, &members),
        )
        .await;
        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "SERIAL-R".to_string(),
                instance_code: "INST-RECOVERED".to_string(),
            }
        );
        let requests = transport.requests();
        assert!(
            requests[1].contains(&fixture.group_uuid),
            "60012 必须按**组 uuid** 回捞：{requests:?}"
        );
        assert_eq!(
            backfill.writes().len(),
            2,
            "两名成员都写同一编号：{:?}",
            backfill.writes()
        );
        assert!(backfill
            .writes()
            .iter()
            .all(|(_, _, serial)| serial == "SERIAL-R"));
    }

    /// 真实库上下文：组派发的实例码回写只碰 MySQL，出站仍由脚本传输接管。
    async fn group_dispatch_context(
    ) -> anyhow::Result<Arc<crate::addon::feishu::domain::context::FeishuContext>> {
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
        let database = yang_db::Database::connect(&url).await?;
        Ok(crate::addon::feishu::build_context(
            Arc::new(database.pool().clone()),
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?)
    }

    /// `60012` 回捞命中必须把实例码写回**组行**（设计 §4.6 第 3/7 步）。
    ///
    /// 复现：上一轮 `create` 已成功但响应丢失——组行停在 `lookup`、`instance_code`
    /// 为 NULL；下一次批量点击重发 `create` 得 60012 → 按组 uuid 回捞命中。
    /// 只取详情不回写会让组行 `instance_code` 永久为 NULL，而 `maintain_groups`
    /// 以它判定「已提交组」：成员编号被清空后再也不会被 reopen 补回填。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn group_dispatch_60012_recovery_persists_instance_code_to_group_row(
    ) -> anyhow::Result<()> {
        let context = group_dispatch_context().await?;
        let namespace = uuid::Uuid::new_v4().simple().to_string();
        let config_id: i64 = sqlx::query("INSERT INTO feishu_approval_config (title,base_token,table_id,approval_code,applicant_field,backfill_field,base_timezone,enabled,created_at,updated_at) VALUES ('组回捞落码测试','appGroupRecover',?,'DEF-GROUP','fldPerson','fldNumber','Asia/Shanghai',true,NOW(),NOW())")
            .bind(format!("tbl{namespace}"))
            .execute(context.approval_configs().pool())
            .await?
            .last_insert_id() as i64;
        let fixture = {
            let mut fixture = GroupFixture::new();
            // 组 uuid 有全局唯一索引：每次运行都要换一个，否则重跑必撞。
            fixture.group_uuid = uuid::Uuid::new_v4().to_string();
            fixture
        };
        // 组行停在 `lookup` 且无实例码：上一轮 create 成功、本地确认前中断。
        sqlx::query("INSERT INTO feishu_approval_group (config_id,group_key,uuid,signature_hash,effect_phase,instance_code,created_at,updated_at) VALUES (?,?,?,'sig','lookup',NULL,NOW(),NOW())")
            .bind(config_id)
            .bind(&fixture.group_key)
            .bind(&fixture.group_uuid)
            .execute(context.approval_groups().pool())
            .await?;
        let leader_id = sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,group_id,group_seq,created_at,updated_at) VALUES (?,?,?,'pending',?,0,NOW(),NOW())")
            .bind(config_id)
            .bind(format!("{namespace}-leader"))
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&fixture.group_key)
            .execute(context.approval_tasks().pool())
            .await?
            .last_insert_id() as i64;
        let claim = crate::addon::feishu::domain::approval_execution::claim_ready(
            &context,
            0,
            Some(leader_id),
            1,
        )
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("组 leader 未被认领"))?;

        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (400, &uuid_conflict()),
            (200, &ok_detail("INST-RECOVERED", "SERIAL-R")),
        ]);
        let backfill = RecordingBackfill::default();
        let mut input = group_input(&fixture, &members);
        input.context = Some(context.as_ref());
        input.execution = Some(&claim);
        let result = dispatch_group(&transport, &NoSleep, &fake_tokens(), &backfill, &input).await;
        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "{result:?}"
        );

        let (phase, instance): (String, Option<String>) = sqlx::query_as(
            "SELECT effect_phase,instance_code FROM feishu_approval_group WHERE config_id=? AND group_key=?",
        )
        .bind(config_id)
        .bind(&fixture.group_key)
        .fetch_one(context.approval_groups().pool())
        .await?;
        assert_eq!(phase, "created", "回捞命中后组行阶段必须落实为 created");
        assert_eq!(
            instance.as_deref(),
            Some("INST-RECOVERED"),
            "回捞到的实例码必须写回组行"
        );
        Ok(())
    }

    #[tokio::test]
    async fn group_dispatch_returns_created_pending_backfill_on_partial_failure() {
        // 一条成员回填失败 = 整组重试，绝不把部分成功当完成。
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-PARTIAL")),
            (200, &ok_detail("INST-PARTIAL", "SERIAL-P")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::with_poison("recB");
        let result = dispatch_group(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &group_input(&fixture, &members),
        )
        .await;
        assert!(
            matches!(
                &result,
                DispatchResult::CreatedPendingBackfill { instance_code, serial_number, message }
                    if instance_code == "INST-PARTIAL"
                        && serial_number.as_deref() == Some("SERIAL-P")
                        && message.contains("回填失败")
            ),
            "{result:?}"
        );
        // 失败的成员不得被写成扩展字段：只写了成功那一条的编号。
        assert!(
            backfill
                .batches()
                .iter()
                .all(|(record_id, _)| record_id != "recB"),
            "部分失败时不得为失败成员写扩展字段：{:?}",
            backfill.batches()
        );
    }

    #[tokio::test]
    async fn group_dispatch_fatal_writes_no_backfill_cells() {
        // 飞书明确拒绝（1390013 = 不支持自定义审批流程）：整组暂停，回填列一个字都不写。
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![(
            400,
            r#"{"code":1390013,"msg":"unsupported custom workflow"}"#,
        )]);
        let backfill = RecordingBackfill::default();
        let result = dispatch_group(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &group_input(&fixture, &members),
        )
        .await;
        assert!(
            matches!(result, DispatchResult::Terminal { .. }),
            "Fatal 必须判整组暂停：{result:?}"
        );
        assert!(
            backfill.raw_writes().is_empty(),
            "Fatal 不得写任何回填单元格：{:?}",
            backfill.raw_writes()
        );
    }

    #[tokio::test]
    async fn group_dispatch_pauses_when_any_member_applicant_differs() {
        let fixture = GroupFixture::new();
        let mut member_cells = fixture.member_cells.clone();
        member_cells[1].insert(
            "fld_applicant".to_string(),
            json!([{"id": "ou_other", "name": "李四"}]),
        );
        let members: Vec<GroupMemberInput<'_>> = member_cells
            .iter()
            .enumerate()
            .map(|(index, cells)| GroupMemberInput {
                record_id: if index == 0 { "recA" } else { "recB" },
                cells,
                attachment_codes: BTreeMap::new(),
            })
            .collect();
        let transport = ScriptedTransport::new(vec![]);
        let backfill = RecordingBackfill::default();

        let result = dispatch_group(
            &transport,
            &NoSleep,
            &fake_tokens(),
            &backfill,
            &group_input(&fixture, &members),
        )
        .await;

        assert!(
            matches!(&result, DispatchResult::Terminal { message }
                if message.contains("1 条") && !message.contains("李四")),
            "组通知只报条数、不列姓名：{result:?}"
        );
        assert!(transport.requests().is_empty(), "整组暂停不得发出站请求");
        assert!(
            backfill.raw_writes().is_empty(),
            "整组暂停不得写任何单元格：{:?}",
            backfill.raw_writes()
        );
    }

    #[tokio::test]
    async fn group_dispatch_writes_extended_fields_and_applicant_per_member() {
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_create("INST-EXT")),
            (200, &ok_detail("INST-EXT", "SERIAL-E")),
            (200, r#"{"code":0,"data":{}}"#),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let mut input = group_input(&fixture, &members);
        input.status_field = Some("fld_status");
        input.start_time_field = Some("fld_start");
        let result = dispatch_group(&transport, &NoSleep, &fake_tokens(), &backfill, &input).await;
        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "{result:?}"
        );
        let batches = backfill.batches();
        assert_eq!(batches.len(), 2, "每个成员各一次合并写：{batches:?}");
        for (record_id, fields) in &batches {
            assert!(
                fields.contains(&"fld_applicant".to_string()),
                "发起人列必须逐成员写：{record_id} {fields:?}"
            );
            assert!(
                fields.contains(&"fld_status".to_string()),
                "六个扩展字段同样逐成员写：{record_id} {fields:?}"
            );
        }
        let applicants: Vec<_> = backfill
            .raw_writes()
            .into_iter()
            .filter(|(_, field, _)| field == "fld_applicant")
            .collect();
        assert_eq!(applicants.len(), 2, "两条成员记录各写一次发起人");
        assert!(applicants
            .iter()
            .all(|(_, _, value)| value == &json!([{"id": "ou_clicker"}])));
    }

    #[tokio::test]
    async fn group_dispatch_recovers_existing_instance_without_creating() {
        // 组行已有实例码（上一轮创建成功但本地确认前后崩溃）：只恢复详情并回填，
        // **不再 create**——否则同一组会多出一个审批单。
        let fixture = GroupFixture::new();
        let members = fixture.members();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_detail("INST-EXISTING", "SERIAL-X")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let mut input = group_input(&fixture, &members);
        input.instance_code = Some("INST-EXISTING");
        let result = dispatch_group(&transport, &NoSleep, &fake_tokens(), &backfill, &input).await;
        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "SERIAL-X".to_string(),
                instance_code: "INST-EXISTING".to_string(),
            }
        );
        let created = transport
            .full_requests
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .filter(|request| {
                request.method == crate::addon::feishu::domain::outbound::OutboundMethod::Post
                    && request.url.ends_with("/approval/v4/instances")
            })
            .count();
        assert_eq!(created, 0, "已有实例码时不得再次创建");
        assert_eq!(backfill.writes().len(), 2, "全部成员仍要回填同一编号");
    }

    /// 已建实例的组在恢复回填时：空申请人列补写组身份，**明确写着别人的列不覆盖**
    /// （实例发起人已固定，人工修改不能被静默改回）。
    #[tokio::test]
    async fn group_recovery_preserves_a_foreign_applicant_cell() {
        let fixture = GroupFixture::new();
        let member_cells = [
            fixture.member_cells[0].clone(),
            cells(&[
                ("fld_top", json!("共同")),
                ("fld_desc", json!("明细 B")),
                ("fld_applicant", json!([{"id": "ou_other", "name": "张三"}])),
            ]),
        ];
        let members: Vec<GroupMemberInput<'_>> = member_cells
            .iter()
            .enumerate()
            .map(|(index, cell)| GroupMemberInput {
                record_id: if index == 0 { "recA" } else { "recB" },
                cells: cell,
                attachment_codes: BTreeMap::new(),
            })
            .collect();
        let transport = ScriptedTransport::new(vec![
            (200, &ok_detail("INST-EXISTING", "SERIAL-X")),
            (200, r#"{"code":0,"data":{}}"#),
        ]);
        let backfill = RecordingBackfill::default();
        let mut input = group_input(&fixture, &members);
        input.instance_code = Some("INST-EXISTING");
        let result = dispatch_group(&transport, &NoSleep, &fake_tokens(), &backfill, &input).await;
        assert!(
            matches!(result, DispatchResult::Backfilled { .. }),
            "{result:?}"
        );
        let applicants: Vec<_> = backfill
            .raw_writes()
            .into_iter()
            .filter(|(_, field, _)| field == "fld_applicant")
            .collect();
        assert_eq!(
            applicants,
            vec![(
                "recA".to_string(),
                "fld_applicant".to_string(),
                json!([{"id": "ou_clicker"}])
            )],
            "只补写空申请人列，不能覆盖写着别人的成员"
        );
    }

    #[test]
    fn group_failure_classification_pauses_only_on_fatal() {
        for (kind, paused) in [
            (FailureKind::Fatal { code: 1390015 }, true),
            (
                FailureKind::Retry {
                    retry_after_seconds: None,
                },
                false,
            ),
            (FailureKind::TokenExpired, false),
            (FailureKind::InstanceNotFound, false),
            (FailureKind::UuidConflict, false),
        ] {
            let outcome = classify_group_failure(OutboundFailure {
                kind,
                message: "原文".to_string(),
            });
            assert_eq!(
                matches!(outcome, GroupOutcome::Paused { .. }),
                paused,
                "{kind:?}"
            );
        }
    }
}
