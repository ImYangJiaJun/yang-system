//! 生产发布使用的版本化、前向数据库迁移作业。

use crate::app::build_app;
use crate::config::{MigrationSettings, SecuritySettings};
use anyhow::{ensure, Context};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use yang_base::database::{
    DatabaseInitializer, Migration, MigrationColumnCheck, MigrationManifest, MigrationPlan,
    MigrationPlanStatus,
};
use yang_base::tools::ToolsBuilder;
use yang_db::{Database, DatabaseConfig};

const MIGRATION_MODULE: &str = "yang-system";

pub const USAGE: &str =
    "用法: cargo run --bin yang-migrate --locked -- <plan|apply> [--config <path>]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCommand {
    Plan,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStage {
    Migrate,
    Validate,
    Ready,
}

pub const PRODUCTION_RELEASE_ORDER: [ReleaseStage; 3] = [
    ReleaseStage::Migrate,
    ReleaseStage::Validate,
    ReleaseStage::Ready,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationCli {
    command: MigrationCommand,
    config_path: PathBuf,
}

impl MigrationCli {
    pub fn command(&self) -> MigrationCommand {
        self.command
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MigrationDescriptor {
    version: &'static str,
    sql: &'static str,
    description: &'static str,
    prerequisite: &'static str,
    recovery: &'static str,
    completion_check: Option<ColumnCompletionDescriptor>,
}

#[derive(Debug, Clone, Copy)]
struct ColumnCompletionDescriptor {
    table: &'static str,
    column: &'static str,
    column_type: &'static str,
    nullable: bool,
    default: Option<&'static str>,
}

impl MigrationDescriptor {
    pub fn version(&self) -> &'static str {
        self.version
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn prerequisite(&self) -> &'static str {
        self.prerequisite
    }

    pub fn recovery(&self) -> &'static str {
        self.recovery
    }

    fn migration(&self) -> Migration {
        let migration = Migration::new(self.version, self.sql);
        match self.completion_check {
            Some(check) => migration.with_completion_check(MigrationColumnCheck::new(
                check.table,
                check.column,
                check.column_type,
                check.nullable,
                check.default,
            )),
            None => migration,
        }
    }
}

const MIGRATIONS: [MigrationDescriptor; 9] = [
    MigrationDescriptor {
        version: "20260726_0001_create_users",
        sql: include_str!("../migrations/20260726_0001_create_users.sql"),
        description: "建立账号、密码摘要与状态的用户主表",
        prerequisite: "目标数据库存在；已有 users 表必须与当前应用定义兼容",
        recovery: "DDL 可重入；失败时修复 users 结构差异后原版本重跑，禁止修改已发布 SQL",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260726_0002_create_admin_user",
        sql: include_str!("../migrations/20260726_0002_create_admin_user.sql"),
        description: "建立平台账号与唯一初始化占位约束",
        prerequisite: "20260726_0001_create_users 已完成",
        recovery: "DDL 可重入；失败时修复 admin_user 结构差异后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260726_0003_create_org_org",
        sql: include_str!("../migrations/20260726_0003_create_org_org.sql"),
        description: "建立企业主数据与唯一企业编号约束",
        prerequisite: "20260726_0002_create_admin_user 已完成",
        recovery: "DDL 可重入；失败时修复 org_org 结构差异后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260726_0004_create_org_user",
        sql: include_str!("../migrations/20260726_0004_create_org_user.sql"),
        description: "建立企业成员、租户键与成员身份索引",
        prerequisite: "users 与 org_org 表已完成",
        recovery: "DDL 可重入；失败时修复 org_user 结构或索引差异后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260726_0005_add_user_authz_version",
        sql: include_str!("../migrations/20260726_0005_add_user_authz_version.sql"),
        description: "为用户增加单调授权版本，作为长生命周期 Token 的失效依据",
        prerequisite: "20260726_0001_create_users 已完成；应用仍兼容默认版本 1",
        recovery: "列完成探针精确核对 bigint、NOT NULL 与默认值 1；原子 DDL 已提交时只恢复迁移状态",
        completion_check: Some(ColumnCompletionDescriptor {
            table: "users",
            column: "authz_version",
            column_type: "bigint",
            nullable: false,
            default: Some("1"),
        }),
    },
    MigrationDescriptor {
        version: "20260726_0006_create_authorization_outbox",
        sql: include_str!("../migrations/20260726_0006_create_authorization_outbox.sql"),
        description: "建立授权版本事务 Outbox，支持至少一次 Redis 失效传播",
        prerequisite: "20260726_0005_add_user_authz_version 已完成；MySQL 8 支持 SKIP LOCKED",
        recovery: "DDL 可重入；失败时修复 authorization_outbox 结构或索引差异后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260726_0007_create_audit_event",
        sql: include_str!("../migrations/20260726_0007_create_audit_event.sql"),
        description: "建立高权限业务不可变审计事实表与检索/保留索引",
        prerequisite: "MySQL 8 支持已执行 CHECK 约束；生产运行账号与迁移账号已分离",
        recovery: "DDL 可重入；失败时修复 audit_event 列、约束或索引差异后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260731_0008_create_work_project",
        sql: include_str!("../migrations/20260731_0008_create_work_project.sql"),
        description: "建立按个人工作区隔离的项目组合与关系选择索引",
        prerequisite: "20260726_0001_create_users 已完成；个人工作区以 users.id 为可信租户键",
        recovery: "DDL 可重入；失败时核对 owner 外键与三个声明索引后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260731_0009_create_work_task",
        sql: include_str!("../migrations/20260731_0009_create_work_task.sql"),
        description: "建立任务树、项目关系、分页筛选与批量操作索引",
        prerequisite: "20260731_0008_create_work_project 已完成；MySQL 8 支持递归 CTE",
        recovery: "DDL 可重入；失败时核对 owner/project/parent 复合外键与声明索引后原版本重跑",
        completion_check: None,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRunReport {
    pub plan: MigrationPlan,
    pub validated_tables: Vec<String>,
}

pub fn descriptors() -> &'static [MigrationDescriptor] {
    &MIGRATIONS
}

pub fn manifest() -> anyhow::Result<MigrationManifest> {
    MigrationManifest::new(
        MIGRATION_MODULE,
        MIGRATIONS.iter().map(MigrationDescriptor::migration),
    )
    .context("构建 yang-system 迁移清单失败")
}

pub fn parse_cli<I>(args: I) -> Result<Option<MigrationCli>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err(format!("缺少迁移命令\n{USAGE}"));
    };
    if matches!(command.as_str(), "--help" | "-h") {
        return Ok(None);
    }
    let command = match command.as_str() {
        "plan" => MigrationCommand::Plan,
        "apply" => MigrationCommand::Apply,
        value => return Err(format!("未知迁移命令: {value}\n{USAGE}")),
    };
    let mut config_path = PathBuf::from("config.toml");
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--config 缺少路径\n{USAGE}"))?;
                if value.trim().is_empty() {
                    return Err(format!("--config 路径不能为空\n{USAGE}"));
                }
                config_path = PathBuf::from(value);
            }
            "--help" | "-h" => return Ok(None),
            value => return Err(format!("未知参数: {value}\n{USAGE}")),
        }
    }
    Ok(Some(MigrationCli {
        command,
        config_path,
    }))
}

pub async fn run(cli: MigrationCli) -> anyhow::Result<MigrationRunReport> {
    let settings = MigrationSettings::load(cli.config_path())?;
    ensure!(
        cli.command() != MigrationCommand::Apply || settings.mysql.max_connections >= 2,
        "迁移 apply 要求 mysql.max_connections 至少为 2，以持有 advisory lock 并执行 SQL"
    );
    let database = Database::connect_with_config(&settings.mysql.url, settings.mysql_config())
        .await
        .context("迁移作业连接 MySQL 失败")?;
    execute_with_database(
        cli.command(),
        database,
        settings.mysql_config(),
        Arc::new(settings.security),
    )
    .await
}

pub async fn execute_with_database(
    command: MigrationCommand,
    database: Database,
    database_config: DatabaseConfig,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<MigrationRunReport> {
    let initializer_database = Database::from_pool(database.pool().clone(), database_config)
        .context("构造迁移初始化数据库失败")?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    let manifest = manifest()?;

    if command == MigrationCommand::Plan {
        return Ok(MigrationRunReport {
            plan: initializer
                .plan_manifest(&manifest)
                .await
                .context("生成只读迁移计划失败")?,
            validated_tables: Vec::new(),
        });
    }

    initializer
        .apply_manifest(&manifest)
        .await
        .context("执行版本化迁移失败")?;
    let plan = initializer
        .plan_manifest(&manifest)
        .await
        .context("迁移后复核执行记录失败")?;
    ensure!(
        plan.entries
            .iter()
            .all(|entry| entry.status == MigrationPlanStatus::Applied),
        "迁移执行记录未全部进入 applied 状态"
    );

    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(database)
            .build()
            .context("构建 Schema 校验 Tools 失败")?,
    );
    let validation = async {
        let application =
            build_app(Arc::clone(&tools), security).context("构建 Schema 定义失败")?;
        let definitions = application
            .runtime
            .table_definitions()
            .iter()
            .collect::<Vec<_>>();
        let report = initializer
            .plan_table_definitions(&definitions)
            .await
            .context("迁移后校验数据库 Schema 失败")?;
        ensure!(
            report.is_noop(),
            "迁移后数据库 Schema 未对齐，存在 {} 项差异: {:?}",
            report.changes.len(),
            report.changes
        );
        crate::audit::validate_schema(tools.mysql()?.pool())
            .await
            .context("迁移后校验高权限审计表失败")?;
        Ok::<_, anyhow::Error>(report.tables)
    }
    .await;
    tools.close().await;
    let validated_tables = validation?;

    Ok(MigrationRunReport {
        plan,
        validated_tables,
    })
}

pub fn print_report(command: MigrationCommand, report: &MigrationRunReport) {
    for entry in &report.plan.entries {
        let descriptor = MIGRATIONS
            .iter()
            .find(|descriptor| descriptor.version == entry.version);
        let (description, prerequisite, recovery) =
            descriptor.map_or(("<missing>", "<missing>", "<missing>"), |descriptor| {
                (
                    descriptor.description,
                    descriptor.prerequisite,
                    descriptor.recovery,
                )
            });
        println!(
            "version={} checksum={} status={} description={description}",
            entry.version,
            entry.checksum,
            status_name(entry.status)
        );
        println!("  prerequisite={prerequisite}");
        println!("  recovery={recovery}");
    }
    if command == MigrationCommand::Apply {
        println!("stage=migrate status=completed");
        println!(
            "stage=validate status=completed tables={}",
            report.validated_tables.join(",")
        );
        println!("next_stage=ready");
    }
}

fn status_name(status: MigrationPlanStatus) -> &'static str {
    match status {
        MigrationPlanStatus::Pending => "pending",
        MigrationPlanStatus::Applied => "applied",
        MigrationPlanStatus::ChecksumMismatch => "checksum_mismatch",
        MigrationPlanStatus::InProgress => "in_progress",
        _ => "unknown",
    }
}
