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

use async_trait::async_trait;
use serde_json::{Map, Value};
use yang_base::action::{ActionContext, ApiResponse, Request};
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
        emit(&ctx);
        next.run(ctx).await
    }
}

/// 落一条请求参数日志。
///
/// 成败不在这里判定：请求的结局由同一 `request_id` 的 `Action 执行完成` 规范事件承载，
/// 两行按 `request_id` 关联（关联键的约定见 `docs/operations/LOG_SHIPPING.md`）。
fn emit(ctx: &ActionContext) {
    let operation = ctx
        .dispatch_target()
        .map(|(module, action)| format!("{module}.{action}"))
        .unwrap_or_else(|| "unknown.unknown".to_string());
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

/// 一次请求的可落日志快照。各成员都已序列化成字符串，可直接进结构化日志。
pub(crate) struct RequestSnapshot {
    method: String,
    path_params: String,
    query: String,
    headers: String,
    body: String,
    body_bytes: usize,
    body_truncated: bool,
}

/// 把请求摊平成可落日志的快照。
///
/// 抽成只吃 [`Request`] 与 method 的纯函数，是为了让**截断**与**序列化**这两件容易
/// 悄悄坏掉的事能在单测里覆盖：它们坏掉时不报错，只是日志里少一段或格式变样。
pub(crate) fn snapshot(request: &Request, method: Option<&str>) -> RequestSnapshot {
    let body = request.body.to_string();
    let body_bytes = body.len();
    let (body, body_truncated) = truncate(&body);
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
        assert_eq!(snapshot.body_bytes, snapshot.body.len());
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
