//! 飞书机器入口的请求参数日志中间件。
//!
//! # 它为什么存在
//!
//! 飞书回传的报文里有若干**项目从未观测过**的字段形状——最典型的是
//! `linkage_params` 的 key 与 value 形态（见 `docs/architecture/feishu-option-ingest.md`
//! 的 V4、`docs/architecture/feishu-datasource-table-config.md` §11.2）。
//! 联调时必须把真实报文抓下来，本中间件就是那个抓取点。
//!
//! # 它开了可观测性契约的一个例外，用三件事把代价收窄
//!
//! `docs/contracts/OBSERVABILITY.md` 的「结构化日志」章节禁止日志记录**请求体**、
//! Authorization/Cookie header 与 Token。本中间件在开启时**会**记录它们——抓报文
//! 的用途恰恰需要原样看到 token 有没有带对。三条收窄措施各自针对一个具体风险：
//!
//! 1. **默认关闭**（`[feishu].log_inbound_requests`）：关闭时不注册中间件，除
//!    「配了却没关」之外不存在额外的暴露面；
//! 2. **只覆盖三个机器入口**（取选项 + 多维表格的两条写入）：控制台 Action 一律不碰，
//!    那里的请求体里带着**轮换中的新凭据**；
//! 3. **请求体有字节上限**：超出即截断，并在同一行里给出原始长度。一次批量写入能产生
//!    比一次取选项大几个数量级的体，而日志采集端故障时不允许拖慢应用。
//!
//! 例外条款的正式记录在 `OBSERVABILITY.md`，采集与保留侧的配套在
//! `docs/operations/LOG_SHIPPING.md`。
//!
//! # 请求与响应是**两行**，按 `request_id` 关联
//!
//! `飞书机器入口请求参数` 在**派发前**落，`飞书机器入口响应参数` 在**派发后**落。
//! 不合并成一行是刻意的：被管理 Token 中间件拒掉的请求根本走不到响应那一行，而
//! 「token 配错了」正是最需要从日志里认出来的情形——它必须留下请求那一行。
//!
//! 响应体这一行不能省。框架的 `Action 执行完成` 只有 `result` / `error_code` /
//! `duration_ms`，**没有响应体**；而 `code` 对了不等于体对了——取选项接口返回的是
//! `ResponseBody::raw`，整条响应都不走框架包络，只有这里能看到飞书实际收到了什么。

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value};
use yang_base::action::{ActionContext, ApiResponse, Request, ResponseAttachment};
use yang_base::definition::ActionRef;
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;
use yang_runtime::observability::LogIdentity;

/// 请求体落日志的字节上限。
///
/// 64 KiB 比任何一次取选项请求（契约只有七个字段）高三个数量级，又远低于「一次批量
/// 写入把单行日志撑到采集器背不动」的量级。
const MAX_BODY_BYTES: usize = 64 * 1024;

/// 把指定 Action 的完整请求参数写进日志。
///
/// 与 [`super::middleware::ManagementTokenMiddleware`] 一样按 `target_action()` 精确
/// 限定，因此**每个 Action 一个实例**。
pub(crate) struct MachineRequestLogMiddleware {
    target: ActionRef,
}

impl MachineRequestLogMiddleware {
    /// 绑定要记录的 Action。
    pub(crate) fn new(target: ActionRef) -> Self {
        Self { target }
    }
}

#[async_trait]
impl Middleware for MachineRequestLogMiddleware {
    fn target_action(&self) -> Option<&ActionRef> {
        Some(&self.target)
    }

    async fn handle(&self, ctx: ActionContext, next: Next<'_>) -> Result<ApiResponse, BaseError> {
        // 先记后派发：被管理 Token 中间件拒掉的请求也必须留痕——「token 配错了」
        // 正是最需要从日志里认出来的情形，而它恰恰是走不到 Handler 的那一类。
        //
        // 关联字段必须在 `next.run` **之前**摘出来：`ctx` 会被移进下游，之后
        // `request_id` 与部署身份都拿不到，而它们正是把两行拼成一次调用的键。
        let meta = ResponseMeta::capture(&ctx);
        emit(&ctx);

        let started = Instant::now();
        let outcome = next.run(ctx).await;
        emit_response(&meta, started.elapsed(), &outcome);

        outcome
    }
}

/// 落一条请求参数日志。
///
/// 成败不在这里判定：请求的结局由同一 `request_id` 的 `Action 执行完成` 规范事件承载，
/// 两行按 `request_id` 关联（关联键的约定见 `docs/operations/LOG_SHIPPING.md`）。
fn emit(ctx: &ActionContext) {
    let operation = operation_of(ctx);
    // 与 `Action 执行完成` 规范事件共用同一份部署身份：采集侧按 `service` + `environment`
    // 划分索引流（见 `docs/operations/LOG_SHIPPING.md`），缺了这三个字段的事件会落到
    // 另一条流里，排障时要跨流检索。
    let identity = LogIdentity::from_tools(ctx.tools());
    let snapshot = snapshot(&ctx.request, ctx.request_meta.method.as_deref());
    tracing::info!(
        service = %identity.service,
        version = %identity.version,
        environment = %identity.environment,
        operation = %operation,
        request_id = %ctx.request_id(),
        method = %snapshot.method,
        path_params = %snapshot.path_params,
        query = %snapshot.query,
        headers = %snapshot.headers,
        body = %snapshot.body,
        body_bytes = snapshot.body_bytes,
        body_truncated = snapshot.body_truncated,
        "飞书机器入口请求参数"
    );
}

/// 本次调用的 `module.action`。未经 `Registry::dispatch` 时落兜底值。
fn operation_of(ctx: &ActionContext) -> String {
    ctx.dispatch_target()
        .map(|(module, action)| format!("{module}.{action}"))
        .unwrap_or_else(|| "unknown.unknown".to_string())
}

/// 响应日志要用的关联字段。
///
/// 与请求那一行共用同一份部署身份与同一个 `request_id`，两者缺一采集侧就串不起来
/// （约定见 `docs/operations/LOG_SHIPPING.md`）。
struct ResponseMeta {
    identity: LogIdentity,
    operation: String,
    request_id: String,
}

impl ResponseMeta {
    fn capture(ctx: &ActionContext) -> Self {
        Self {
            identity: LogIdentity::from_tools(ctx.tools()),
            operation: operation_of(ctx),
            request_id: ctx.request_id().to_string(),
        }
    }
}

/// 落一条响应参数日志。
///
/// `duration_ms` 与框架的 `Action 执行完成` 重复了一次：这是为了让响应这行**能单独读**
/// ——排查时是 `grep <request_id>` 抓出两行，而不是再去跨事件类型拼一次。
fn emit_response(meta: &ResponseMeta, elapsed: Duration, outcome: &Result<ApiResponse, BaseError>) {
    let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let (code, message, body) = match outcome {
        Ok(response) => (
            response.code,
            response.message.clone(),
            ResponseSnapshot::of(response),
        ),
        // 派发失败时框架随后会把它折成错误响应，这里先把**原始错误**留下：
        // `code` 是框架错误码，`message` 是给运维看的排查线索。
        Err(error) => (
            error.code(),
            error.to_string(),
            ResponseSnapshot::none("error"),
        ),
    };

    // 成败用两条不同级别：`code == 0` 是正常一次调用，其余都该在 `warn` 上被看见。
    // 字段列表两臂一致，是为了让 `grep <request_id>` 出来的两行**同形**。
    if code == 0 {
        tracing::info!(
            service = %meta.identity.service,
            version = %meta.identity.version,
            environment = %meta.identity.environment,
            operation = %meta.operation,
            request_id = %meta.request_id,
            code,
            message = %message,
            body_shape = %body.shape,
            body = %body.body,
            body_bytes = body.body_bytes,
            body_truncated = body.body_truncated,
            duration_ms,
            "飞书机器入口响应参数"
        );
    } else {
        tracing::warn!(
            service = %meta.identity.service,
            version = %meta.identity.version,
            environment = %meta.identity.environment,
            operation = %meta.operation,
            request_id = %meta.request_id,
            code,
            message = %message,
            body_shape = %body.shape,
            body = %body.body,
            body_bytes = body.body_bytes,
            body_truncated = body.body_truncated,
            duration_ms,
            "飞书机器入口响应参数"
        );
    }
}

/// 响应体快照。与请求侧共用同一个字节上限与截断规则。
struct ResponseSnapshot {
    /// `raw` 是**不套框架包络**的原始体（`ResponseBody::raw`）；`envelope` 是 `data`
    /// 字段；`empty` 是没有响应体；`error` 是派发失败。四者形态不同，混在一起看会
    /// 把「体是空的」误读成「体丢了」。
    shape: &'static str,
    body: String,
    body_bytes: usize,
    body_truncated: bool,
}

impl ResponseSnapshot {
    fn of(response: &ApiResponse) -> Self {
        // 先看原始体：取选项接口走的就是这条路，而它正是本中间件最需要看清的响应
        // ——整条体都不在 `data` 里，只看 `data` 会得到一个空字符串。
        if let Some(ResponseAttachment::Raw { body, .. }) = &response.attachment {
            return Self::from_text("raw", body);
        }
        match &response.data {
            Some(data) => Self::from_text("envelope", &data.to_string()),
            None => Self::from_text("empty", ""),
        }
    }

    fn none(shape: &'static str) -> Self {
        Self::from_text(shape, "")
    }

    fn from_text(shape: &'static str, text: &str) -> Self {
        // 与请求侧同一套约定：先量线上字节，再重排展示。
        let body_bytes = text.len();
        let display = pretty_json(text);
        let (body, body_truncated) = truncate(&display);
        Self {
            shape,
            body: body.to_string(),
            body_bytes,
            body_truncated,
        }
    }
}

/// 一次请求的可落日志快照。各成员都已序列化成字符串，可直接进结构化日志。
pub(crate) struct RequestSnapshot {
    method: String,
    path_params: String,
    query: String,
    headers: String,
    /// **展示形态**：是 JSON 就带缩进（人读格式下会摊成多行）。
    body: String,
    /// **线上原始**字节数（重排与截断之前）。
    body_bytes: usize,
    /// **展示文本**是否被截断——与 `body_bytes` 口径不同，见 [`pretty_json`]。
    body_truncated: bool,
}

/// 把请求摊平成可落日志的快照。
///
/// 抽成只吃 [`Request`] 与 method 的纯函数，是为了让**截断**与**序列化**这两件容易
/// 悄悄坏掉的事能在单测里覆盖：它们坏掉时不报错，只是日志里少一段或格式变样。
pub(crate) fn snapshot(request: &Request, method: Option<&str>) -> RequestSnapshot {
    let raw = request.body.to_string();
    // **先量字节，再重排**：`body_bytes` 是线上原始大小，重排只改展示。
    let body_bytes = raw.len();
    let display = pretty_json(&raw);
    let (body, body_truncated) = truncate(&display);
    RequestSnapshot {
        method: method.unwrap_or("UNKNOWN").to_string(),
        path_params: json_of(&request.path_params),
        query: json_of(&request.query),
        headers: json_of(&request.headers),
        body: body.to_string(),
        body_bytes,
        body_truncated,
    }
}

/// 把 JSON 文本按缩进重排，供**人读**；不是 JSON 就原样返回。
///
/// # 为什么要重排
///
/// 标准输出是人读格式，它**原样打印**值里的换行：缩进后的报文于是**真的换行**，
/// 一次调用里的长报文摊开成多行。不重排的话，一个 3 KB 的响应体会挤成一行，
/// 「看得见响应体」这件事就只剩名义上的。
///
/// 重排只影响**展示**：字段名与字段集一个没变，`LOG_SHIPPING.md` 列的那批顶层字段
/// 仍在（该文档顶部有「接采集器前先改回单行 JSON」的前置条件，改这里之前先读它）。
///
/// # 它不改变任何计数
///
/// `body_bytes` 记的始终是**线上原始**字节数（重排前量的），`body` 才是展示形态。
/// 两者不相等是刻意的：报文的字节数要用来说「对方到底发了多少」。
fn pretty_json(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

/// 把字符串映射序列化成 JSON 对象文本。
///
/// 刻意不走 `serde_json::to_string`：它返回 `Result`，而生产代码禁 `unwrap`。这两类
/// 值都是 `String`，经 `Value` 的 `Display` 是**不会失败**的路径。
fn json_of(values: &std::collections::HashMap<String, String>) -> String {
    let object: Map<String, Value> = values
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    Value::Object(object).to_string()
}

/// 按字节上限截断，且截断点**必须落在字符边界上**。
///
/// 请求体是 UTF-8：按字节硬切会切碎多字节字符，而按字节索引切片越界还会直接 panic。
/// 截断后的文本不再是合法 JSON，这是刻意的——它是给人看的日志片段，而
/// `body_bytes` / `body_truncated` 已经把「这里不完整」说清楚了。
fn truncate(body: &str) -> (&str, bool) {
    if body.len() <= MAX_BODY_BYTES {
        return (body, false);
    }
    let mut end = MAX_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    (&body[..end], true)
}

// 这两行事件各自的**内容**都在下面测到了，但 `handle` 对它们的**接线**没有：
// `Next` 的字段全是 `pub(crate)`，`yang-system` 侧造不出一个可用的 `Next`，
// 于是「派发前后各调一次」这条只能靠读代码保证。请求那一行同样是这个状态
// ——不是漏了，是这一层在框架外测不到。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use yang_base::definition::{ActionName, ModuleName};
    use yang_base::router::MiddlewareScope;

    /// 把 tracing 的 JSON 输出收进内存，供断言读取。
    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl LogBuffer {
        fn text(&self) -> String {
            let guard = self
                .0
                .lock()
                .unwrap_or_else(|error| panic!("日志缓冲锁被毒化: {error}"));
            String::from_utf8_lossy(&guard).into_owned()
        }
    }

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut guard = self
                .0
                .lock()
                .unwrap_or_else(|error| panic!("日志缓冲锁被毒化: {error}"));
            guard.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogBuffer;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn target(action: &str) -> ActionRef {
        let module = ModuleName::new("feishu.option")
            .unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        let action =
            ActionName::new(action).unwrap_or_else(|error| panic!("Action 名应有效: {error}"));
        ActionRef::new(module, action)
    }

    /// 三个机器入口都是 `public`。scope 默认就是 `AllActions`，这条把它钉住：一旦有人
    /// 改成 `ProtectedActions`，日志会对**所有真实请求**静默消失——`Next::run` 对
    /// `ProtectedActions` 的判据是 `!policy.is_public`，而机器入口全是 public。
    #[test]
    fn middleware_covers_public_machine_endpoints() {
        let reference = target("approval_options");
        let middleware = MachineRequestLogMiddleware::new(reference.clone());

        assert_eq!(middleware.scope(), MiddlewareScope::AllActions);
        assert_eq!(middleware.target_action(), Some(&reference));
    }

    /// 每个实例恰好限定一个 Action，因此不会串到控制台 Action 上。
    #[test]
    fn middleware_is_pinned_to_the_declared_action() {
        let middleware = MachineRequestLogMiddleware::new(target("upsert_options"));
        let pinned = middleware
            .target_action()
            .unwrap_or_else(|| panic!("必须限定目标 Action"));

        assert_eq!(pinned.module().as_str(), "feishu.option");
        assert_eq!(pinned.action().as_str(), "upsert_options");
    }

    /// 小报文原样落日志：`linkage_params` 的 key 与 value 一个字符都不能少，
    /// 那正是这个中间件存在的理由。
    #[test]
    fn small_bodies_are_kept_whole() {
        let request = Request::new(json!({
            "token": "1e8e999f580e7a202dbe1e5103c5e4c58ecc757e",
            "linkage_params": { "widget17796881173030001": "@i18n@currency:CNY" },
        }));

        let snapshot = snapshot(&request, Some("POST"));

        assert!(!snapshot.body_truncated);
        // 两个口径必须分开断言：`body_bytes` 是**线上原始**长度，`body` 是**缩进后**的
        // 展示形态。只断言「两者都非零」会让「重排没生效」或「字节数被改成展示长度」
        // 这两类错都溜过去——而后者会让「对方到底发了多少」这句话失去依据。
        assert_eq!(
            snapshot.body_bytes,
            request.body.to_string().len(),
            "body_bytes 必须是重排前的线上长度"
        );
        // 用 `lines()` 而不是找换行字符：断言的是「摊成了多行」这件事本身，
        // 顺带避开在 Rust 源码里写转义字符。
        assert!(
            snapshot.body.lines().count() > 1,
            "JSON 体应当被缩进成多行，实际: {}",
            snapshot.body
        );
        assert!(
            snapshot.body.len() > snapshot.body_bytes,
            "缩进后的展示文本应当比线上原文长"
        );
        assert!(snapshot.body.contains("widget17796881173030001"));
        assert!(snapshot.body.contains("@i18n@currency:CNY"));
    }

    /// 超限报文被截断，且原始大小仍然可读——「截了多少」比「截了什么」更难事后还原。
    #[test]
    fn oversized_bodies_are_truncated_with_the_original_size_kept() {
        let request = Request::new(json!({ "blob": "x".repeat(MAX_BODY_BYTES * 2) }));

        let snapshot = snapshot(&request, Some("POST"));

        assert!(snapshot.body_truncated);
        assert!(snapshot.body.len() <= MAX_BODY_BYTES);
        assert!(
            snapshot.body_bytes > MAX_BODY_BYTES,
            "body_bytes 必须是截断前的原始字节数，实际 {}",
            snapshot.body_bytes
        );
    }

    /// 截断点必须落在字符边界上：多字节字符按字节硬切会切出半个字符，按字节索引
    /// 切片越界还会 panic。
    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        // 全 3 字节字符：MAX_BODY_BYTES 不是 3 的倍数，硬切一定落在字符中间
        let body = "中".repeat(MAX_BODY_BYTES);

        let (head, truncated) = truncate(&body);

        assert!(truncated);
        assert!(body.is_char_boundary(head.len()), "截断点不在字符边界上");
        assert!(head.ends_with('中'), "末尾字符被切碎了");
        assert!(body.starts_with(head));
    }

    /// 端到端地证明「一次请求确实被打成一行结构化 JSON，且参数一个不少」。
    ///
    /// 这是本中间件**唯一**的对外行为，而它坏掉时不报错：那行日志只是不见了，或字段
    /// 空了——而那时你正等着报文定位问题。所以它值得一条真的把事件捕获下来的测试，
    /// 而不是只测快照。
    #[test]
    fn emitted_event_carries_the_full_request_parameters() {
        let mut request = Request::new(json!({
            "token": "1e8e999f580e7a202dbe1e5103c5e4c58ecc757e",
            "linkage_params": { "widget17796881173030001": "@i18n@currency:CNY" },
        }));
        request
            .path_params
            .insert("source_key".to_string(), "rate".to_string());
        request
            .query
            .insert("locale".to_string(), "zh_cn".to_string());
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        let tools = yang_base::tools::ToolsBuilder::new()
            .build()
            .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}"));
        // 派发目标（`module` / `action`）只由 `Registry::dispatch` 注入，且刻意没有
        // 公开 setter（yang-base 的文件头写着「外部构造的上下文不能伪造此字段」）。
        // 因此这里必然落到 `operation` 的兜底值——生产路径上它恒由 Registry 填好。
        let ctx = ActionContext::new(request, Arc::new(tools));

        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || emit(&ctx));

        let line = buffer.text();
        assert!(
            line.contains("飞书机器入口请求参数"),
            "事件没落日志: {line}"
        );
        assert!(
            line.contains("unknown.unknown"),
            "未经 Registry 派发时应落到 operation 兜底值: {line}"
        );
        assert!(
            line.contains("\"service\"") && line.contains("\"environment\""),
            "必须带部署身份——采集侧按 service + environment 划分索引流: {line}"
        );
        assert!(
            line.contains("1e8e999f580e7a202dbe1e5103c5e4c58ecc757e"),
            "token 未落日志: {line}"
        );
        assert!(
            line.contains("@i18n@currency:CNY"),
            "linkage_params 未落日志: {line}"
        );
        assert!(
            line.contains("widget17796881173030001"),
            "联动键丢失: {line}"
        );
        assert!(line.contains("source_key"), "路径参数丢失: {line}");
        // method / query / headers / 截断标记都必须**按值**断言：只查字段名的话，把这
        // 三行 emit 字段整行删掉、或把 `body_truncated` 取反，测试照样全绿——而那正是
        // 「日志静默缺一段」这种坏法。快照层的对应断言在别的测试里，覆盖不到 emit。
        assert!(
            line.contains("\"method\":\"UNKNOWN\""),
            "method 未落日志: {line}"
        );
        assert!(line.contains("zh_cn"), "query 未落日志: {line}");
        assert!(line.contains("content-type"), "headers 未落日志: {line}");
        assert!(
            line.contains("\"body_truncated\":false"),
            "截断标记的取值不对: {line}"
        );
        assert!(line.contains("\"body_bytes\""), "字节数缺失: {line}");
    }

    /// 在捕获式 subscriber 下跑一段代码，返回落下来的日志文本。
    fn capture(run: impl FnOnce()) -> String {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        buffer.text()
    }

    fn response_meta() -> ResponseMeta {
        ResponseMeta {
            identity: LogIdentity::new("yang-system", "0.1.0", "test"),
            operation: "feishu.option.approval_options".to_string(),
            request_id: "req-1".to_string(),
        }
    }

    /// 响应体这一行存在的**唯一**理由：`code` 对了不等于体对了。取选项接口返回的是
    /// `ResponseBody::raw`，整条响应都不走框架包络——只看 `data` 会得到一个空串，
    /// 于是「响应是空的」和「响应没被记下来」在日志里长得一模一样。
    #[test]
    fn response_event_carries_the_raw_wire_body() {
        let mut response = ApiResponse::success_value(json!({ "ignored": true }), "ok");
        response.attachment = Some(ResponseAttachment::Raw {
            body: r#"{"code":0,"msg":"success!","data":{"result":{"options":[{"id":"fldeysrdna:d22258711825"}]}}}"#.to_string(),
            content_type: "application/json".to_string(),
        });

        let line =
            capture(|| emit_response(&response_meta(), Duration::from_millis(7), &Ok(response)));

        assert!(
            line.contains("飞书机器入口响应参数"),
            "事件没落日志: {line}"
        );
        assert!(
            line.contains(r#""body_shape":"raw""#),
            "原始体必须被识别为 raw，否则读日志的人会把包络与原始体混为一谈: {line}"
        );
        assert!(
            line.contains("fldeysrdna:d22258711825"),
            "原始响应体的内容没进日志——这正是加这一行的目的: {line}"
        );
        assert!(
            !line.contains("ignored"),
            "有原始体时不该再退回 data——那会让两处内容互相矛盾: {line}"
        );
        assert!(line.contains(r#""code":0"#), "业务码未落日志: {line}");
        assert!(line.contains(r#""duration_ms":7"#), "耗时未落日志: {line}");
        assert!(
            line.contains(r#""request_id":"req-1""#),
            "关联键未落日志: {line}"
        );
        assert!(
            line.contains(r#""body_truncated":false"#),
            "截断标记取值不对: {line}"
        );
    }

    /// 没有原始体时退回框架包络的 `data`——控制台 Action 走的是这条路。
    #[test]
    fn response_event_falls_back_to_the_envelope_data() {
        let response = ApiResponse::success_value(json!({ "upserted": 3 }), "ok");

        let line =
            capture(|| emit_response(&response_meta(), Duration::from_millis(1), &Ok(response)));

        assert!(
            line.contains(r#""body_shape":"envelope""#),
            "应识别为包络体: {line}"
        );
        assert!(line.contains("upserted"), "data 内容没进日志: {line}");
    }

    /// 完全没有响应体时 `shape` 是 `empty` 而不是 `raw`——否则「空体」与「原始体为空」
    /// 分不开，而这两件事的成因完全不同。
    #[test]
    fn empty_response_body_is_labelled_empty() {
        let line = capture(|| {
            emit_response(
                &response_meta(),
                Duration::from_millis(1),
                &Ok(ApiResponse::fail(40401, "数据源不存在")),
            )
        });

        assert!(
            line.contains(r#""body_shape":"empty""#),
            "应标记为空体: {line}"
        );
        assert!(
            line.contains(r#""body_bytes":0"#),
            "空体的字节数应为 0: {line}"
        );
    }

    /// 派发失败要留下**框架错误码**与原始错误文本：响应那一行此时不存在，运维只能靠这里。
    #[test]
    fn failed_dispatch_is_logged_with_the_framework_error_code() {
        let line = capture(|| {
            emit_response(
                &response_meta(),
                Duration::from_millis(2),
                &Err(BaseError::ConfigError("下游配置缺失".to_string())),
            )
        });

        assert!(
            line.contains(r#""body_shape":"error""#),
            "应标记为派发失败: {line}"
        );
        assert!(
            line.contains("下游配置缺失"),
            "原始错误文本没进日志: {line}"
        );
        assert!(
            line.contains(&format!(
                r#""code":{}"#,
                BaseError::ConfigError(String::new()).code()
            )),
            "框架错误码未落日志: {line}"
        );
    }

    /// 超大响应体同样按字节上限截断，且**原始大小**要留着——「截了多少」事后无法还原。
    #[test]
    fn oversized_response_bodies_are_truncated_with_the_original_size_kept() {
        let huge = "x".repeat(MAX_BODY_BYTES + 1024);
        let mut response = ApiResponse::success_value(json!({}), "ok");
        response.attachment = Some(ResponseAttachment::Raw {
            body: huge.clone(),
            content_type: "application/json".to_string(),
        });

        let line =
            capture(|| emit_response(&response_meta(), Duration::from_millis(1), &Ok(response)));

        assert!(line.contains(r#""body_truncated":true"#), "未截断: {line}");
        assert!(
            line.contains(&format!(r#""body_bytes":{}"#, huge.len())),
            "body_bytes 必须是截断前的原始长度: {line}"
        );
    }

    /// 未接入传输层时 method 缺省，快照仍要可落日志而不是丢弃整条记录。
    #[test]
    fn missing_method_and_map_fields_still_produce_a_snapshot() {
        let mut request = Request::new(json!({ "token": "t" }));
        request
            .path_params
            .insert("source_key".to_string(), "rate".to_string());
        request.query.insert("page".to_string(), "1".to_string());
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());

        let snapshot = snapshot(&request, None);

        assert_eq!(snapshot.method, "UNKNOWN");
        assert_eq!(snapshot.path_params, r#"{"source_key":"rate"}"#);
        assert_eq!(snapshot.query, r#"{"page":"1"}"#);
        assert_eq!(snapshot.headers, r#"{"content-type":"application/json"}"#);
    }
}
