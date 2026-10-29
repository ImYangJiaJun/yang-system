use crate::addon::account::email_delivery::{
    ChangeEmailCodeSenderHandle, LoginEmailCodeSenderHandle, NewDeviceEmailSender,
    NewDeviceEmailSenderHandle, PasswordResetEmailSender, PasswordResetEmailSenderHandle,
    RegistrationEmailSender, RegistrationEmailSenderHandle, SmtpChangeEmailCodeSender,
    SmtpEmailSender, SmtpLoginEmailCodeSender, VerificationCodeSender,
    VerificationCodeSenderHandle,
};
use crate::addon::feishu::domain::alert::{FeishuAlertSender, FeishuAlertSenderHandle};
use crate::app::{build_app, YANG_SYSTEM_METRIC_NAMES};
use crate::authorization::{AuthorizationOutboxWorker, AuthorizationVersionCache};
use crate::config::Settings;
use crate::feishu_approval_worker::{
    ApprovalDispatchHandle, ApprovalDispatchRequest, ApprovalDispatchWorker,
};
use crate::feishu_event_worker::FeishuEventWorker;
use crate::feishu_oauth_reminder::FeishuOauthReminderWorker;
use crate::feishu_pull::{FeishuPullHandle, FeishuPullWorker};
use crate::infrastructure::grant_expiry_worker::GrantExpiryWorker;
use crate::infrastructure::session_cleanup::SessionCleanupWorker;
use anyhow::Context;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use yang_base::database::DatabaseInitializer;
use yang_base::http::HttpClient;
use yang_base::tools::{Tools, ToolsBuilder};
use yang_base::transport::axum::{serve_listener_with_shutdown, AxumTransportConfig};
use yang_db::{Database, RedisClient};
use yang_runtime::observability::{LogIdentity, TelemetryRuntime};
use yang_runtime::shutdown::ShutdownBudget;

const TRIGGER_SIGNAL: &str = "signal";
const TRIGGER_OPERATION_EXIT: &str = "operation_exit";
const PHASE_HTTP_DRAIN: &str = "http_drain";
const PHASE_MANAGEMENT_DRAIN: &str = "management_drain";
const PHASE_AUTHORIZATION_OUTBOX_WORKER: &str = "authorization_outbox_worker";
const PHASE_GRANT_EXPIRY_WORKER: &str = "grant_expiry_worker";
const PHASE_SESSION_CLEANUP_WORKER: &str = "session_cleanup_worker";
const PHASE_FEISHU_PULL_WORKER: &str = "feishu_pull_worker";
const PHASE_FEISHU_APPROVAL_WORKER: &str = "feishu_approval_worker";
const PHASE_FEISHU_EVENT_WORKER: &str = "feishu_event_worker";
const PHASE_FEISHU_OAUTH_REMINDER_WORKER: &str = "feishu_oauth_reminder_worker";
const PHASE_TOOLS_CLOSE: &str = "tools_close";
const PHASE_OBSERVABILITY: &str = "observability";

/// 一次性只读冒烟不会创建 telemetry exporter、监听器或任何业务后台任务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Serve,
    ReadOnlySmoke,
}

impl StartupMode {
    pub fn from_args(args: &[std::ffi::OsString]) -> anyhow::Result<Self> {
        match args {
            [] => Ok(Self::Serve),
            [arg] if arg == "--smoke-check" => Ok(Self::ReadOnlySmoke),
            _ => anyhow::bail!("用法：yang-system [--smoke-check]"),
        }
    }
}

pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    run_with_mode(config_path, StartupMode::Serve).await
}

pub async fn run_with_mode(config_path: &Path, mode: StartupMode) -> anyhow::Result<()> {
    let settings = Settings::load(config_path)?;
    let log_identity = LogIdentity::new(
        &settings.app.name,
        env!("CARGO_PKG_VERSION"),
        settings.app.environment.as_str(),
    )
    .with_metric_names(YANG_SYSTEM_METRIC_NAMES);
    let mut telemetry = match mode {
        StartupMode::Serve => Some(TelemetryRuntime::initialize(
            &settings.observability,
            &settings.logging.filter,
            &log_identity,
        )?),
        StartupMode::ReadOnlySmoke => None,
    };
    let shutdown_budget =
        ShutdownBudget::new(Duration::from_secs(settings.shutdown.total_timeout_seconds))
            .with_metric_names(YANG_SYSTEM_METRIC_NAMES);
    tracing::info!(
        service = %log_identity.service,
        version = %log_identity.version,
        environment = %log_identity.environment,
        config = %config_path.display(),
        "开始启动系统"
    );
    let result = run_after_telemetry_initialized(
        &settings,
        log_identity,
        telemetry.as_mut(),
        shutdown_budget.clone(),
        mode,
    )
    .await;
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    let telemetry_result = match telemetry.as_mut() {
        Some(telemetry) => {
            let deadline = shutdown_budget.wait_started().await;
            shutdown_budget
                .run_phase(PHASE_OBSERVABILITY, telemetry.shutdown_before(deadline))
                .await
                .map_err(anyhow::Error::from)
        }
        None => Ok(()),
    };
    combine_operation_and_cleanup(result, telemetry_result, "可观测性运行时")
}

async fn run_after_telemetry_initialized(
    settings: &Settings,
    log_identity: LogIdentity,
    telemetry: Option<&mut TelemetryRuntime>,
    shutdown_budget: ShutdownBudget,
    mode: StartupMode,
) -> anyhow::Result<()> {
    let token_manager = settings.token.build_manager()?;

    let mysql = Database::connect_with_config(&settings.mysql.url, settings.mysql_config())
        .await
        .context("连接 MySQL 失败")?;
    let initializer_mysql = Database::from_pool(mysql.pool().clone(), settings.mysql_config())
        .context("构造 schema 初始化数据库失败")?;
    let cache = RedisClient::connect_with_config(&settings.redis.url, settings.redis_config())
        .await
        .context("连接 Redis 失败")?;
    let authorization_cache =
        AuthorizationVersionCache::new(cache.clone(), settings.authorization.deployment.clone())
            .context("构建授权版本缓存失败")?;

    let email_sender: Arc<SmtpEmailSender> = Arc::new(
        SmtpEmailSender::new(&settings.email, &settings.app.name)
            .context("构建 SMTP 邮件投递器失败")?,
    );
    let registration_sender: Arc<dyn RegistrationEmailSender> = email_sender.clone();
    let password_reset_sender: Arc<dyn PasswordResetEmailSender> = email_sender.clone();
    let verification_code_sender: Arc<dyn VerificationCodeSender> = email_sender.clone();
    // 免密登录验证码与 MFA 验证码共用 SMTP 传输但文案独立，占用独立 extension 槽。
    let login_email_code_sender = SmtpLoginEmailCodeSender::new(email_sender.as_ref().clone());
    let change_email_code_sender = SmtpChangeEmailCodeSender(email_sender.as_ref().clone());
    // 飞书拉取失败的告警邮件复用同一条 SMTP 传输。**无条件注册**：收件人列表为空
    // （默认）时它一次也不会被调用，行为与未集成飞书时一致。
    let feishu_alert_sender: Arc<dyn FeishuAlertSender> = email_sender.clone();
    let new_device_sender: Arc<dyn NewDeviceEmailSender> = email_sender;
    // 飞书出站拉取的手动触发句柄 + 排程出口。**无条件注册**：句柄必须在 Tools 定型**之前**
    // 就位，否则「立即拉取」与「下次自动拉取」两个 Action 取不到它。
    // 段未配置时这里注册的是一个永远不会被读到的空壳（那两个 Action 也只在
    // can_pull() 时才注册），行为与未集成飞书一致。
    // 审批派发的手动触发句柄。**同样无条件注册**：dispatch Action 只在
    // `can_pull()` 为真时才注册，但句柄本身必须先于 Tools 定型，否则那个
    // Action 取不到它（与飞书拉取句柄同一条理由）。
    let (approval_dispatch_handle, approval_dispatch_requests) = ApprovalDispatchHandle::new();
    let (feishu_pull_handle, feishu_pull_requests) =
        FeishuPullHandle::new(settings.feishu.as_ref().map_or_else(
            crate::config::default_feishu_pull_interval_seconds,
            |feishu| feishu.pull_interval_seconds,
        ));
    let mut tools_builder = ToolsBuilder::new()
        .mysql(mysql)
        .cache(cache)
        .token(token_manager)
        // 出站 HTTP 客户端：飞书开放平台拉取（tenant_access_token / 多维表格记录）用。
        // 只打开 `http` feature 并不会有一个可用客户端——必须在启动期显式注册，
        // 否则 `Tools::http()` 恒返回 `HttpClientNotInitialized(300006)`。
        //
        // 这里是**客户端级默认超时**（30 秒），只作兜底：单次取 token 与单页拉取
        // 各自的超时由请求级 `RequestBuilder::timeout` 决定，量级差很大。
        .http(HttpClient::new(30)?)
        .extension(authorization_cache)
        .extension(RegistrationEmailSenderHandle::from_arc(registration_sender))
        .extension(PasswordResetEmailSenderHandle::from_arc(
            password_reset_sender,
        ))
        .extension(VerificationCodeSenderHandle::from_arc(
            verification_code_sender,
        ))
        .extension(LoginEmailCodeSenderHandle::new(login_email_code_sender))
        .extension(ChangeEmailCodeSenderHandle(
            VerificationCodeSenderHandle::new(change_email_code_sender),
        ))
        .extension(NewDeviceEmailSenderHandle::from_arc(new_device_sender))
        // 飞书拉取失败的告警出口：与上面几个投递器共用 SMTP 传输，占用独立槽位。
        .extension(FeishuAlertSenderHandle::from_arc(feishu_alert_sender))
        // 运行期句柄，照 `AuthorizationVersionCache` 的先例走 extension 槽：
        // `FeishuContext` 描述的是「有哪些表与什么配置」，不该混进 worker 的运行时状态。
        .extension(feishu_pull_handle.clone())
        .extension(approval_dispatch_handle.clone())
        .config(log_identity)
        .config(settings.email.verification.engine_config())
        .config(settings.email.password_reset.link_config());
    // 邮箱换绑验证码使用独立 config 槽与 key 域；未配置时 change_email Action 不注册。
    if let Some(change) = settings.email.change.as_ref() {
        tools_builder = tools_builder.config(crate::config::ChangeEmailVerificationConfig(
            change.change_engine_config(),
        ));
    }
    // 登录 MFA 备用邮箱验证码使用独立 config 槽与 key 域；
    // 未配置时 request_mfa_email_code 返回未启用错误。
    if let Some(mfa) = settings.email.mfa.as_ref() {
        tools_builder = tools_builder.config(crate::config::MfaEmailVerificationConfig(
            mfa.mfa_engine_config(),
        ));
    }
    // 邮箱验证码免密登录使用独立 config 槽与 key 域；
    // 未配置时 request_login_email_code / login_by_email_code 返回未启用错误。
    if let Some(login) = settings.email.login.as_ref() {
        tools_builder = tools_builder.config(crate::config::LoginEmailVerificationConfig(
            login.login_engine_config(),
        ));
    }
    // TOTP 密钥域（AEAD）；未配置时 MFA Action 不注册。
    if let Some(totp) = settings.security.totp.as_ref() {
        tools_builder = tools_builder.config(totp.clone());
    }
    // 飞书集成配置；未配置时飞书 Addon 仍装配（表要参与 Schema 同步），
    // 但对外路由不注册。用 `Arc` 包一层是为了让这个槽位与其它同类型配置区分开
    // （`ToolsBuilder::config` 按具体 Rust 类型索引）。
    if let Some(feishu) = settings.feishu.as_ref() {
        tools_builder = tools_builder.config(Arc::new(feishu.clone()));
    }
    let tools = Arc::new(tools_builder.build().context("构建应用 Tools 失败")?);

    run_then_cleanup(
        run_after_tools_created(
            settings,
            initializer_mysql,
            Arc::clone(&tools),
            shutdown_budget.clone(),
            telemetry,
            feishu_pull_handle,
            feishu_pull_requests,
            approval_dispatch_requests,
            mode,
        ),
        tools.close(),
        &shutdown_budget,
    )
    .await
}

fn combine_operation_and_cleanup<T>(
    result: anyhow::Result<T>,
    cleanup_result: anyhow::Result<()>,
    cleanup_name: &str,
) -> anyhow::Result<T> {
    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Err(operation_error), Err(cleanup_error)) => {
            tracing::error!(
                cleanup = cleanup_name,
                error = %cleanup_error,
                "业务阶段失败后，清理阶段也失败"
            );
            Err(operation_error)
        }
    }
}

/// 运行 Tools 构建后的完整启动与服务阶段。
///
/// 该函数整体位于 [`run_then_cleanup`] 的 operation 边界内，因此应用构建、schema、
/// 地址解析/绑定、服务运行失败以及正常退出都会进入同一个关闭出口。
#[allow(clippy::too_many_arguments)] // 启动期资源与通道按所有权传递，不引入进程级容器。
async fn run_after_tools_created(
    settings: &Settings,
    initializer_mysql: Database,
    tools: Arc<Tools>,
    shutdown_budget: ShutdownBudget,
    telemetry: Option<&mut TelemetryRuntime>,
    feishu_pull_handle: FeishuPullHandle,
    feishu_pull_requests: mpsc::UnboundedReceiver<Option<i64>>,
    approval_dispatch_requests: mpsc::UnboundedReceiver<ApprovalDispatchRequest>,
    mode: StartupMode,
) -> anyhow::Result<()> {
    let application = build_app(Arc::clone(&tools), Arc::new(settings.security.clone()))
        .context("构建应用模块失败")?;
    // `application.runtime` 稍后会被移进 `Arc`，收敛句柄必须在那之前取出。
    let grant_expiry = application.grant_expiry_handle();

    // 旧版全局唯一索引（record_id 单列）会让复制 Base 后的同 record_id 无法在另一
    // 配置下派发，而 schema 同步只增不删——所以在同步/预检之前 fail-closed。
    // 只读冒烟同样 fail-fast：它要证明的正是「这个库能安全跑新版本」。
    crate::schema::ensure_legacy_task_record_index_absent(initializer_mysql.pool())
        .await
        .context("启动前检查旧版审批任务唯一索引失败")?;
    let initializer = DatabaseInitializer::new(initializer_mysql);
    let definitions =
        crate::schema::definitions(&application.runtime).context("构建完整数据库 schema 失败")?;
    let schema = definitions.iter().collect::<Vec<_>>();
    if mode == StartupMode::ReadOnlySmoke {
        let report = initializer
            .preflight_table_definitions(&schema)
            .await
            .context("只读 schema 预检失败")?;
        anyhow::ensure!(report.is_safe(), "只读 schema 预检发现旧数据冲突");
        let health = tools.health_check().await;
        anyhow::ensure!(health.is_healthy(), "只读冒烟依赖检查失败");
        println!(
            "{}",
            serde_json::json!({
                "interface": "yang-system-smoke-v1",
                "mode": "read_only",
                "status": "ok",
                "schema_changes": report.plan.changes.len(),
            })
        );
        return Ok(());
    }
    let telemetry = telemetry.context("正常启动缺少 telemetry 运行时")?;
    let report = initializer
        .sync_table_definitions(&schema)
        .await
        .context("启动期同步数据库 schema 失败；请按错误中的表名、约束名和主键修复旧数据后重启")?;
    tracing::info!(
        tables = ?report.tables,
        changes = report.changes.len(),
        "数据库 schema 同步完成"
    );
    crate::audit::validate_schema(tools.mysql()?.pool())
        .await
        .context("启动期校验高权限审计表失败")?;
    drop(initializer);

    let bind = settings.bind_addr()?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("绑定 HTTP 地址失败: {bind}"))?;
    telemetry
        .start_management_server(
            settings.observability.metrics_bind_addr()?,
            Arc::clone(&tools),
        )
        .await?;
    let readiness_gate = telemetry.readiness_gate();
    let outbox_worker = AuthorizationOutboxWorker::start(&tools, settings.authorization.clone())
        .await
        .context("启动授权 Outbox Worker 失败")?;
    // 过期直授收敛：无条件启动。过期行若只靠签发/刷新过滤，已签发的 Access Token
    // 最长还能再活一个 access TTL；后台每 60 秒递增版本并写 Outbox 把它收敛掉。
    let grant_expiry_worker = GrantExpiryWorker::start(Arc::clone(&tools), grant_expiry)
        .context("启动过期直授收敛 Worker 失败")?;
    // 会话保留期清理：无条件启动。不清理会让 IP/UA 与逐设备会话行无界累积；
    // 保留期与 refresh token 硬上限对齐，删除的只可能是已无法续期的会话。
    let session_cleanup_worker = SessionCleanupWorker::start(Arc::clone(&tools))
        .context("启动会话保留期清理 Worker 失败")?;
    // 飞书出站拉取 Worker：**只在 can_pull() 为真时起**。凭证还是占位值时起来，
    // 只会拿占位凭证按间隔反复出网、每轮都失败并告警——这正是 can_pull() 要挡的状态。
    // 段未配置、或已配置但凭证是占位值时，这里安静地不起，行为与未集成飞书一致。
    let feishu_pull_worker = match settings.feishu.as_ref().filter(|feishu| feishu.can_pull()) {
        Some(feishu) => {
            let feishu_context = crate::addon::feishu::build_context(
                Arc::new(tools.mysql()?.pool().clone()),
                Arc::new(settings.security.clone()),
                Some(Arc::new(feishu.clone())),
            )
            .context("构建飞书出站上下文的上下文失败")?;
            Some(
                FeishuPullWorker::start(
                    Arc::clone(&tools),
                    feishu,
                    Arc::clone(&feishu_context),
                    feishu_pull_handle,
                    feishu_pull_requests,
                )
                .context("启动飞书出站拉取 Worker 失败")?,
            )
        }
        None => None,
    };
    // 审批派发 Worker：与拉取 Worker 同一条准入（`can_pull()`），共用同一份
    // `FeishuContext`。两者**都跑**是设计的一部分——拉取只写数据源，派发只写
    // 审批任务，互不触碰对方的表。
    let approval_worker = match settings.feishu.as_ref().filter(|feishu| feishu.can_pull()) {
        Some(feishu) => {
            let feishu_context = crate::addon::feishu::build_context(
                Arc::new(tools.mysql()?.pool().clone()),
                Arc::new(settings.security.clone()),
                Some(Arc::new(feishu.clone())),
            )
            .context("构建审批派发上下文失败")?;
            Some(
                ApprovalDispatchWorker::start(
                    Arc::clone(&tools),
                    feishu,
                    feishu_context,
                    approval_dispatch_requests,
                )
                .context("启动审批派发 Worker 失败")?,
            )
        }
        None => None,
    };
    // 飞书用户身份授权的到期提醒：同一条准入（`can_pull()`）。独立起停而不是挂进
    // 拉取 worker——它按**天**跑（保活 + 350 天后的每日催办），与拉取间隔无关。
    let oauth_reminder_worker = match settings.feishu.as_ref().filter(|feishu| feishu.can_pull()) {
        Some(feishu) => {
            let feishu_context = crate::addon::feishu::build_context(
                Arc::new(tools.mysql()?.pool().clone()),
                Arc::new(settings.security.clone()),
                Some(Arc::new(feishu.clone())),
            )
            .context("构建飞书用户身份上下文失败")?;
            Some(
                FeishuOauthReminderWorker::start(Arc::clone(&tools), feishu, feishu_context)
                    .context("启动飞书用户授权提醒 Worker 失败")?,
            )
        }
        None => None,
    };
    // 事件处理 worker：与审批派发同一条准入（`can_pull()`），共用同一份出站与数据库。
    let event_worker = match settings.feishu.as_ref().filter(|feishu| feishu.can_pull()) {
        Some(feishu) => {
            let feishu_context = crate::addon::feishu::build_context(
                Arc::new(tools.mysql()?.pool().clone()),
                Arc::new(settings.security.clone()),
                Some(Arc::new(feishu.clone())),
            )
            .context("构建事件处理上下文失败")?;
            Some(
                FeishuEventWorker::start(Arc::clone(&tools), feishu, feishu_context)
                    .context("启动事件处理 Worker 失败")?,
            )
        }
        None => None,
    };
    let runtime = Arc::new(application.runtime);
    let signal_budget = shutdown_budget.clone();
    let signal_readiness = readiness_gate.clone();
    readiness_gate.mark_ready();
    let shutdown_signal = async move {
        yang_base::lifecycle::wait_for_shutdown_signal().await;
        signal_readiness.mark_not_ready();
        signal_budget.begin(TRIGGER_SIGNAL).await;
    };
    let transport_config = AxumTransportConfig {
        max_body_bytes: settings.http.max_body_bytes,
        max_json_body_bytes: Some(settings.http.max_json_body_bytes),
        request_timeout: Some(Duration::from_secs(settings.http.request_timeout_seconds)),
        max_concurrency: Some(settings.http.max_concurrency),
        ..AxumTransportConfig::default()
    };
    let serve = async move {
        serve_listener_with_shutdown(listener, runtime, transport_config, shutdown_signal)
            .await
            .context("HTTP 服务运行失败")
    };
    tokio::pin!(serve);
    let budget_started = shutdown_budget.wait_started();
    tokio::pin!(budget_started);

    // 首个退出事件只负责触发关闭；此处不先等待 HTTP，所有 worker 同时收到信号。
    //
    // 运行期监督（D7-3）：worker 的 JoinHandle 不进任何 select 时，panic/退出
    // 只留下句柄完成，readiness 与进程都继续绿。这里每 5 秒看一眼审批 worker，
    // 一旦结束就按「业务阶段失败」走同一条关闭路径。监督 future 放在块里，
    // 借用在 select 结束时释放，后面的关闭阶段才能按值取走 worker。
    let serve_result = {
        let worker_guard = async {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                if approval_worker
                    .as_ref()
                    .is_some_and(ApprovalDispatchWorker::has_exited)
                {
                    return "feishu_approval_worker";
                }
            }
        };
        tokio::pin!(worker_guard);
        tokio::select! {
            result = &mut serve => Some(result),
            _ = &mut budget_started => None,
            exited = &mut worker_guard => {
                tracing::error!(worker = exited, "关键后台 Worker 意外退出，触发进程关闭");
                Some(Err(anyhow::anyhow!("{exited} 意外退出")))
            }
        }
    };
    readiness_gate.mark_not_ready();
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    outbox_worker.request_shutdown();
    grant_expiry_worker.request_shutdown();
    session_cleanup_worker.request_shutdown();
    if let Some(worker) = &feishu_pull_worker {
        worker.request_shutdown();
    }
    if let Some(worker) = &approval_worker {
        worker.request_shutdown();
    }
    if let Some(worker) = &event_worker {
        worker.request_shutdown();
    }
    if let Some(worker) = &oauth_reminder_worker {
        worker.request_shutdown();
    }
    telemetry.request_management_shutdown();

    // HTTP、管理面和七类 worker 共用截止时间，并给资源关闭 / trace 留出总预算的
    // 10%（最多 1 秒）。超时取消关闭 future 时 OwnedTask 仍持有句柄并请求 abort。
    let reserve = (Duration::from_secs(settings.shutdown.total_timeout_seconds) / 10)
        .min(Duration::from_secs(1));
    let (http, outbox, grants, sessions, pull, approval, event, oauth, management) = tokio::join!(
        shutdown_budget.run_phase_with_reserve(PHASE_HTTP_DRAIN, reserve, async {
            match serve_result {
                Some(result) => result,
                None => (&mut serve).await,
            }
        }),
        shutdown_budget.run_phase_with_reserve(
            PHASE_AUTHORIZATION_OUTBOX_WORKER,
            reserve,
            outbox_worker.shutdown()
        ),
        shutdown_budget.run_phase_with_reserve(
            PHASE_GRANT_EXPIRY_WORKER,
            reserve,
            grant_expiry_worker.shutdown()
        ),
        shutdown_budget.run_phase_with_reserve(
            PHASE_SESSION_CLEANUP_WORKER,
            reserve,
            session_cleanup_worker.shutdown()
        ),
        shutdown_budget.run_phase_with_reserve(PHASE_FEISHU_PULL_WORKER, reserve, async {
            match feishu_pull_worker {
                Some(worker) => worker.shutdown().await,
                None => Ok(()),
            }
        }),
        shutdown_budget.run_phase_with_reserve(PHASE_FEISHU_APPROVAL_WORKER, reserve, async {
            match approval_worker {
                Some(worker) => worker.shutdown().await,
                None => Ok(()),
            }
        }),
        shutdown_budget.run_phase_with_reserve(PHASE_FEISHU_EVENT_WORKER, reserve, async {
            match event_worker {
                Some(worker) => worker.shutdown().await,
                None => Ok(()),
            }
        }),
        shutdown_budget.run_phase_with_reserve(
            PHASE_FEISHU_OAUTH_REMINDER_WORKER,
            reserve,
            async {
                match oauth_reminder_worker {
                    Some(worker) => worker.shutdown().await,
                    None => Ok(()),
                }
            }
        ),
        shutdown_budget.run_phase_with_reserve(
            PHASE_MANAGEMENT_DRAIN,
            reserve,
            telemetry.shutdown_management()
        ),
    );
    let serve_result = http.map_err(anyhow::Error::from);
    let shutdown_result = outbox
        .and(grants)
        .and(sessions)
        .and(pull)
        .and(approval)
        .and(event)
        .and(oauth)
        .and(management)
        .map_err(anyhow::Error::from);
    match (serve_result, shutdown_result) {
        (Err(serve_error), Err(worker_error)) => {
            tracing::error!(
                error = %worker_error,
                "HTTP 服务退出失败后，授权 Outbox Worker 关闭也失败"
            );
            Err(serve_error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// 等待 operation 完成后无条件执行且仅执行一次 cleanup。
///
/// cleanup 与 operation 内部的关闭阶段共享同一截止时间；若两者都失败，保留最早的
/// operation 错误，同时记录 cleanup 错误。
async fn run_then_cleanup<T>(
    operation: impl Future<Output = anyhow::Result<T>>,
    cleanup: impl Future<Output = ()>,
    shutdown_budget: &ShutdownBudget,
) -> anyhow::Result<T> {
    let result = operation.await;
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    let cleanup_result = shutdown_budget
        .run_phase(PHASE_TOOLS_CLOSE, async {
            cleanup.await;
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from);

    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Err(operation_error), Err(cleanup_error)) => {
            tracing::error!(
                error = %cleanup_error,
                "业务阶段失败后，Tools 资源关闭也失败"
            );
            Err(operation_error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yang_base::tools::ToolsState;

    #[test]
    fn startup_mode_is_explicit_and_rejects_unknown_arguments() -> anyhow::Result<()> {
        assert_eq!(StartupMode::from_args(&[])?, StartupMode::Serve);
        assert_eq!(
            StartupMode::from_args(&["--smoke-check".into()])?,
            StartupMode::ReadOnlySmoke
        );
        for args in [
            vec!["--unknown".into()],
            vec!["--smoke-check".into(), "extra".into()],
        ] {
            assert!(StartupMode::from_args(&args).is_err());
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要本机 YANG_SYSTEM_TEST_DATABASE_URL，可创建随机 scratch 测试库"]
    async fn smoke_mode_preserves_empty_schema_and_does_not_bind_http() -> anyhow::Result<()> {
        use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
        use std::str::FromStr;
        use yang_base::token::TokenManager;
        use yang_db::DatabaseConfig;

        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let options = MySqlConnectOptions::from_str(&url)?;
        anyhow::ensure!(
            matches!(options.get_host(), "127.0.0.1" | "localhost" | "::1")
                && options
                    .get_database()
                    .is_some_and(|name| name.ends_with("_test")),
            "只读冒烟回归仅允许从 loopback 的 _test 数据库派生 scratch 库"
        );
        let admin = MySqlPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let current: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(&admin)
            .await?;
        anyhow::ensure!(current.ends_with("_test"), "拒绝非测试数据库");
        let scratch = format!("yang_smoke_{}_test", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE `{scratch}`"))
            .execute(&admin)
            .await?;
        let outcome = async {
            let pool = MySqlPoolOptions::new()
                .max_connections(2)
                .connect_with(options.database(&scratch))
                .await?;
            let mysql = Database::from_pool(pool.clone(), DatabaseConfig::default())?;
            let initializer = Database::from_pool(pool.clone(), DatabaseConfig::default())?;
            let token = TokenManager::new_symmetric(
                "smoke-test-token-key-0123456789abcdef",
                jsonwebtoken::Algorithm::HS256,
                "smoke-test".into(),
                "smoke-test".into(),
                60,
                120,
            )?;
            let tools = Arc::new(ToolsBuilder::new().mysql(mysql).token(token).build()?);
            // 仅反序列化仓内无真实凭据模板；不加载宿主机 config、env 或 secret 目录。
            let mut settings: Settings = toml::from_str(include_str!("../config.example.toml"))?;
            settings.security = crate::config::SecuritySettings::default();
            let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            settings.http.bind = occupied.local_addr()?.to_string();
            let (pull, pulls) = FeishuPullHandle::new(60);
            let (_, approvals) = ApprovalDispatchHandle::new();
            let result = run_after_tools_created(
                &settings,
                initializer,
                Arc::clone(&tools),
                ShutdownBudget::new(Duration::from_secs(1)),
                None,
                pull,
                pulls,
                approvals,
                StartupMode::ReadOnlySmoke,
            )
            .await;
            let tables: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
            )
            .fetch_one(&pool)
            .await?;
            tools.close().await;
            result?;
            anyhow::ensure!(tables == 0, "只读冒烟不得创建表，实际 {tables} 张");
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let cleanup = sqlx::query(&format!("DROP DATABASE `{scratch}`"))
            .execute(&admin)
            .await;
        admin.close().await;
        outcome?;
        cleanup?;
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_runs_exactly_once_for_success_and_every_post_tools_failure() {
        for failed_stage in [
            None,
            Some("build"),
            Some("schema"),
            Some("bind"),
            Some("serve"),
        ] {
            let cleanup_calls = AtomicUsize::new(0);
            let shutdown_budget = ShutdownBudget::new(Duration::from_secs(1));
            let operation = async move {
                match failed_stage {
                    Some(stage) => Err(anyhow::anyhow!("{stage} failed")),
                    None => Ok("completed"),
                }
            };

            let result = run_then_cleanup(
                operation,
                async {
                    cleanup_calls.fetch_add(1, Ordering::SeqCst);
                },
                &shutdown_budget,
            )
            .await;

            assert_eq!(
                cleanup_calls.load(Ordering::SeqCst),
                1,
                "退出场景 {failed_stage:?} 必须恰好 cleanup 一次"
            );
            match (failed_stage, result) {
                (Some(stage), Err(error)) => {
                    assert_eq!(error.to_string(), format!("{stage} failed"));
                }
                (None, Ok(value)) => assert_eq!(value, "completed"),
                (stage, unexpected) => {
                    panic!("退出场景 {stage:?} 返回意外结果: {unexpected:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn cleanup_boundary_reaches_tools_close() {
        let tools = match ToolsBuilder::new().build() {
            Ok(tools) => tools,
            Err(error) => panic!("测试 Tools 应构建成功: {error}"),
        };
        let shutdown_budget = ShutdownBudget::new(Duration::from_secs(1));
        let result: anyhow::Result<()> = run_then_cleanup(
            async { Err(anyhow::anyhow!("schema failed")) },
            tools.close(),
            &shutdown_budget,
        )
        .await;

        match result {
            Err(error) => assert_eq!(error.to_string(), "schema failed"),
            Ok(()) => panic!("业务失败必须保留"),
        }
        assert_eq!(tools.state(), ToolsState::Closed);
    }

    #[tokio::test]
    async fn cleanup_timeout_is_reported_for_success_but_does_not_hide_operation_failure() {
        let success_budget = ShutdownBudget::new(Duration::from_millis(20));
        let success_result = run_then_cleanup(
            async { Ok("completed") },
            tokio::time::sleep(Duration::from_millis(100)),
            &success_budget,
        )
        .await;
        let success_error = success_result
            .err()
            .unwrap_or_else(|| panic!("成功业务后的 cleanup 超时必须返回错误"));
        assert!(success_error.to_string().contains("tools_close"));

        let failure_budget = ShutdownBudget::new(Duration::from_millis(20));
        let failure_result: anyhow::Result<()> = run_then_cleanup(
            async { Err(anyhow::anyhow!("original failure")) },
            tokio::time::sleep(Duration::from_millis(100)),
            &failure_budget,
        )
        .await;
        let failure_error = failure_result
            .err()
            .unwrap_or_else(|| panic!("业务错误必须保留"));
        assert_eq!(failure_error.to_string(), "original failure");
    }
}
