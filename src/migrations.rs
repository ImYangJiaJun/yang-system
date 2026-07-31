//! 生产发布使用的版本化、前向数据库迁移作业。
//! raw-sql-boundary: schema-validator migration-preflight

use crate::app::build_schema_app;
use crate::config::{MigrationSettings, SecuritySettings};
use anyhow::{ensure, Context};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use yang_base::database::{
    DatabaseInitializer, Migration, MigrationCheckConstraint, MigrationColumnCheck,
    MigrationForeignKeyCheck, MigrationManifest, MigrationPlan, MigrationPlanStatus,
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
    completion_check: Option<CompletionDescriptor>,
}

#[derive(Debug, Clone, Copy)]
enum CompletionDescriptor {
    Column(ColumnCompletionDescriptor),
    CheckConstraint(CheckConstraintCompletionDescriptor),
    ForeignKey(ForeignKeyCompletionDescriptor),
}

#[derive(Debug, Clone, Copy)]
struct ColumnCompletionDescriptor {
    table: &'static str,
    column: &'static str,
    column_type: &'static str,
    nullable: bool,
    default: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
struct CheckConstraintCompletionDescriptor {
    table: &'static str,
    constraint: &'static str,
    expression: &'static str,
    enforced: bool,
}

#[derive(Debug, Clone, Copy)]
struct ForeignKeyCompletionDescriptor {
    table: &'static str,
    constraint: &'static str,
    column: &'static str,
    referenced_table: &'static str,
    referenced_column: &'static str,
    update_rule: &'static str,
    delete_rule: &'static str,
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
            Some(CompletionDescriptor::Column(check)) => {
                migration.with_completion_check(MigrationColumnCheck::new(
                    check.table,
                    check.column,
                    check.column_type,
                    check.nullable,
                    check.default,
                ))
            }
            Some(CompletionDescriptor::CheckConstraint(check)) => {
                migration.with_completion_check(MigrationCheckConstraint::new(
                    check.table,
                    check.constraint,
                    check.expression,
                    check.enforced,
                ))
            }
            Some(CompletionDescriptor::ForeignKey(check)) => {
                migration.with_completion_check(MigrationForeignKeyCheck::new(
                    check.table,
                    check.constraint,
                    check.column,
                    check.referenced_table,
                    check.referenced_column,
                    check.update_rule,
                    check.delete_rule,
                ))
            }
            None => migration,
        }
    }
}

const MIGRATIONS: [MigrationDescriptor; 16] = [
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
        completion_check: Some(CompletionDescriptor::Column(ColumnCompletionDescriptor {
            table: "users",
            column: "authz_version",
            column_type: "bigint",
            nullable: false,
            default: Some("1"),
        })),
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
    MigrationDescriptor {
        version: "20260731_0010_add_user_credential_version",
        sql: include_str!("../migrations/20260731_0010_add_user_credential_version.sql"),
        description: "为用户增加独立的凭据与全量会话单调版本",
        prerequisite: "20260731_0009_create_work_task 已完成；先部署兼容读取版本，再开启新字段签发",
        recovery: "列完成探针精确核对 bigint、NOT NULL 与默认值 0；原子 DDL 已提交时只恢复迁移状态",
        completion_check: Some(CompletionDescriptor::Column(ColumnCompletionDescriptor {
            table: "users",
            column: "credential_version",
            column_type: "bigint",
            nullable: false,
            default: Some("0"),
        })),
    },
    MigrationDescriptor {
        version: "20260731_0011_create_password_reset_token",
        sql: include_str!("../migrations/20260731_0011_create_password_reset_token.sql"),
        description: "建立短期、单次消费且只存摘要的密码重置凭证表",
        prerequisite:
            "20260731_0010_add_user_credential_version 已完成；凭据写 Action 仅在协议开关开启后注册",
        recovery:
            "DDL 可重入；失败时核对摘要唯一键、活动凭证索引、时间约束和两个用户外键后原版本重跑",
        completion_check: None,
    },
    MigrationDescriptor {
        version: "20260731_0012_add_users_status_check",
        sql: include_str!("../migrations/20260731_0012_add_users_status_check.sql"),
        description: "把用户状态的 active/disabled 领域集合固化为数据库强制 CHECK",
        prerequisite: "20260731_0011_create_password_reset_token 已完成；发布前核对 MySQL VERSION()/VERSION_COMMENT() 支持并强制执行 CHECK，且按 status 分组计数仅含 active/disabled；在与生产行数和索引规模相当的 staging 表记录 ALTER 耗时和元数据锁等待，超过发布窗口则改用在线 DDL 或 expand-contract",
        recovery: "前向恢复；精确完成探针核对 chk_users_status 名称、表达式与 ENFORCED；脏数据或同名异义约束必须先人工修复，禁止修改已发布 SQL 或回滚到无约束状态",
        completion_check: Some(CompletionDescriptor::CheckConstraint(
            CheckConstraintCompletionDescriptor {
                table: "users",
                constraint: "chk_users_status",
                expression: "status IN ('active', 'disabled')",
                enforced: true,
            },
        )),
    },
    MigrationDescriptor {
        version: "20260731_0013_add_admin_user_user_fk",
        sql: include_str!("../migrations/20260731_0013_add_admin_user_user_fk.sql"),
        description: "用 RESTRICT 外键固化平台授权关系必须引用真实用户",
        prerequisite: "20260731_0012_add_users_status_check 已完成；孤儿预检为零；在与生产 admin_user 行数相当的 staging 记录 ALTER 耗时与元数据锁等待",
        recovery: "前向恢复；外键完成探针精确核对本地列、users.id 与双 RESTRICT；DDL 已提交时只恢复迁移状态，同名异义约束须人工修复",
        completion_check: Some(CompletionDescriptor::ForeignKey(
            ForeignKeyCompletionDescriptor {
                table: "admin_user",
                constraint: "fk_admin_user_user_user",
                column: "user_user",
                referenced_table: "users",
                referenced_column: "id",
                update_rule: "RESTRICT",
                delete_rule: "RESTRICT",
            },
        )),
    },
    MigrationDescriptor {
        version: "20260731_0014_add_org_user_user_fk",
        sql: include_str!("../migrations/20260731_0014_add_org_user_user_fk.sql"),
        description: "用 RESTRICT 外键固化企业成员必须引用真实用户",
        prerequisite: "0013 已完成；org_user.user_user 孤儿预检为零；在生产等量 staging 演练 ALTER 锁预算",
        recovery: "前向恢复；精确完成探针核对 org_user.user_user 到 users.id 与双 RESTRICT；同名异义约束须人工修复",
        completion_check: Some(CompletionDescriptor::ForeignKey(
            ForeignKeyCompletionDescriptor {
                table: "org_user",
                constraint: "fk_org_user_user_user",
                column: "user_user",
                referenced_table: "users",
                referenced_column: "id",
                update_rule: "RESTRICT",
                delete_rule: "RESTRICT",
            },
        )),
    },
    MigrationDescriptor {
        version: "20260731_0015_add_org_user_org_fk",
        sql: include_str!("../migrations/20260731_0015_add_org_user_org_fk.sql"),
        description: "用 RESTRICT 外键固化企业成员必须引用真实企业",
        prerequisite: "0014 已完成；org_user.org_org 孤儿预检为零；在生产等量 staging 演练 ALTER 锁预算",
        recovery: "前向恢复；精确完成探针核对 org_user.org_org 到 org_org.id 与双 RESTRICT；同名异义约束须人工修复",
        completion_check: Some(CompletionDescriptor::ForeignKey(
            ForeignKeyCompletionDescriptor {
                table: "org_user",
                constraint: "fk_org_user_org_org",
                column: "org_org",
                referenced_table: "org_org",
                referenced_column: "id",
                update_rule: "RESTRICT",
                delete_rule: "RESTRICT",
            },
        )),
    },
    MigrationDescriptor {
        version: "20260731_0016_add_admin_bootstrap_key_check",
        sql: include_str!("../migrations/20260731_0016_add_admin_bootstrap_key_check.sql"),
        description: "把平台初始化占位值固化为 NULL 或唯一 initial-admin",
        prerequisite: "0015 已完成；发布前核对 MySQL 8.0.16+ 且 admin_user.bootstrap_key 非空值只含 initial-admin；在生产等量 staging 记录 ALTER 耗时与元数据锁等待",
        recovery: "前向恢复；精确完成探针核对 chk_admin_user_bootstrap_key 名称、表达式与 ENFORCED；脏数据或同名异义约束须先人工修复",
        completion_check: Some(CompletionDescriptor::CheckConstraint(
            CheckConstraintCompletionDescriptor {
                table: "admin_user",
                constraint: "chk_admin_user_bootstrap_key",
                expression: "(bootstrap_key IS NULL) OR (bootstrap_key = 'initial-admin')",
                enforced: true,
            },
        )),
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

    preflight_users_status_check(database.pool()).await?;
    preflight_admin_bootstrap_key_check(database.pool()).await?;
    preflight_authorization_foreign_keys(database.pool()).await?;

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
            build_schema_app(Arc::clone(&tools), security).context("构建 Schema 定义失败")?;
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

async fn preflight_users_status_check(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
    let (version, version_comment): (String, String) =
        sqlx::query_as("SELECT CAST(VERSION() AS CHAR), CAST(@@version_comment AS CHAR)")
            .fetch_one(pool)
            .await
            .context("用户状态 CHECK 发布预检无法读取 MySQL 版本")?;
    ensure!(
        supports_enforced_check(&version, &version_comment),
        "用户状态 CHECK 要求 MySQL 8.0.16 及以上且不能是未纳入验证的兼容实现"
    );

    let users_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = 'users' AND table_type = 'BASE TABLE'",
    )
    .fetch_one(pool)
    .await
    .context("用户状态 CHECK 发布预检无法确认 users 表")?;
    if users_exists == 0 {
        return Ok(());
    }

    let dirty_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE status IS NULL OR status NOT IN ('active', 'disabled')",
    )
    .fetch_one(pool)
    .await
    .context("用户状态 CHECK 发布预检无法统计脏数据")?;
    ensure!(
        dirty_rows == 0,
        "用户状态 CHECK 发布预检发现 {dirty_rows} 行领域集合之外的数据；必须先清洗并复核 status 分组计数"
    );
    Ok(())
}

async fn preflight_admin_bootstrap_key_check(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
    let (version, version_comment): (String, String) =
        sqlx::query_as("SELECT CAST(VERSION() AS CHAR), CAST(@@version_comment AS CHAR)")
            .fetch_one(pool)
            .await
            .context("bootstrap_key CHECK 发布预检无法读取 MySQL 版本")?;
    ensure!(
        supports_enforced_check(&version, &version_comment),
        "bootstrap_key CHECK 要求 MySQL 8.0.16 及以上且不能是未纳入验证的兼容实现"
    );

    let table_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = 'admin_user' AND table_type = 'BASE TABLE'",
    )
    .fetch_one(pool)
    .await
    .context("bootstrap_key CHECK 发布预检无法确认 admin_user 表")?;
    if table_exists == 0 {
        return Ok(());
    }

    let dirty_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_user \
         WHERE bootstrap_key IS NOT NULL AND bootstrap_key <> 'initial-admin'",
    )
    .fetch_one(pool)
    .await
    .context("bootstrap_key CHECK 发布预检无法统计脏数据")?;
    ensure!(
        dirty_rows == 0,
        "bootstrap_key CHECK 发布预检发现 {dirty_rows} 行非法非空占位值；必须先清洗并核对 bootstrap_key 分组计数"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ForeignKeyPreflight {
    name: &'static str,
    child_table: &'static str,
    child_column: &'static str,
    parent_table: &'static str,
    parent_column: &'static str,
}

const AUTHORIZATION_FOREIGN_KEYS: [ForeignKeyPreflight; 3] = [
    ForeignKeyPreflight {
        name: "fk_admin_user_user_user",
        child_table: "admin_user",
        child_column: "user_user",
        parent_table: "users",
        parent_column: "id",
    },
    ForeignKeyPreflight {
        name: "fk_org_user_user_user",
        child_table: "org_user",
        child_column: "user_user",
        parent_table: "users",
        parent_column: "id",
    },
    ForeignKeyPreflight {
        name: "fk_org_user_org_org",
        child_table: "org_user",
        child_column: "org_org",
        parent_table: "org_org",
        parent_column: "id",
    },
];

async fn preflight_authorization_foreign_keys(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
    for relation in AUTHORIZATION_FOREIGN_KEYS {
        let child_exists = table_exists(pool, relation.child_table).await?;
        if !child_exists {
            continue;
        }
        ensure_innodb(pool, relation.child_table).await?;

        let parent_exists = table_exists(pool, relation.parent_table).await?;
        let orphan_rows = if parent_exists {
            ensure_innodb(pool, relation.parent_table).await?;
            ensure_compatible_column_types(pool, relation).await?;
            authorization_orphan_count(pool, relation.name).await?
        } else {
            authorization_child_count(pool, relation.name).await?
        };
        ensure!(
            orphan_rows == 0,
            "外键 {} 发布预检发现 {} 行孤儿授权事实；必须先修复并复核后再执行 DDL",
            relation.name,
            orphan_rows
        );
    }
    Ok(())
}

async fn authorization_orphan_count(
    pool: &sqlx::MySqlPool,
    constraint: &str,
) -> anyhow::Result<i64> {
    let result = match constraint {
        "fk_admin_user_user_user" => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM admin_user AS child \
             LEFT JOIN users AS parent ON parent.id = child.user_user \
             WHERE parent.id IS NULL",
            )
            .fetch_one(pool)
            .await
        }
        "fk_org_user_user_user" => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM org_user AS child \
             LEFT JOIN users AS parent ON parent.id = child.user_user \
             WHERE parent.id IS NULL",
            )
            .fetch_one(pool)
            .await
        }
        "fk_org_user_org_org" => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM org_user AS child \
             LEFT JOIN org_org AS parent ON parent.id = child.org_org \
             WHERE parent.id IS NULL",
            )
            .fetch_one(pool)
            .await
        }
        _ => anyhow::bail!("未知授权外键预检: {constraint}"),
    };
    result.with_context(|| format!("外键 {constraint} 发布预检无法统计孤儿行"))
}

async fn authorization_child_count(
    pool: &sqlx::MySqlPool,
    constraint: &str,
) -> anyhow::Result<i64> {
    let result = match constraint {
        "fk_admin_user_user_user" => {
            sqlx::query_scalar("SELECT COUNT(*) FROM admin_user")
                .fetch_one(pool)
                .await
        }
        "fk_org_user_user_user" | "fk_org_user_org_org" => {
            sqlx::query_scalar("SELECT COUNT(*) FROM org_user")
                .fetch_one(pool)
                .await
        }
        _ => anyhow::bail!("未知授权外键预检: {constraint}"),
    };
    result.with_context(|| format!("外键 {constraint} 发布预检无法统计子表行"))
}

async fn table_exists(pool: &sqlx::MySqlPool, table: &str) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = ? AND table_type = 'BASE TABLE'",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .with_context(|| format!("外键发布预检无法确认表 {table}"))?;
    Ok(count == 1)
}

async fn ensure_innodb(pool: &sqlx::MySqlPool, table: &str) -> anyhow::Result<()> {
    let engine: String = sqlx::query_scalar(
        "SELECT CAST(engine AS CHAR) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = ? AND table_type = 'BASE TABLE'",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .with_context(|| format!("外键发布预检无法读取表 {table} 的存储引擎"))?;
    ensure!(
        engine.eq_ignore_ascii_case("InnoDB"),
        "外键发布预检要求表 {table} 使用 InnoDB，实际为 {engine}"
    );
    Ok(())
}

async fn ensure_compatible_column_types(
    pool: &sqlx::MySqlPool,
    relation: ForeignKeyPreflight,
) -> anyhow::Result<()> {
    let child_type: String = sqlx::query_scalar(
        "SELECT CAST(column_type AS CHAR) FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?",
    )
    .bind(relation.child_table)
    .bind(relation.child_column)
    .fetch_one(pool)
    .await
    .with_context(|| format!("外键 {} 发布预检缺少子列", relation.name))?;
    let parent_type: String = sqlx::query_scalar(
        "SELECT CAST(column_type AS CHAR) FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?",
    )
    .bind(relation.parent_table)
    .bind(relation.parent_column)
    .fetch_one(pool)
    .await
    .with_context(|| format!("外键 {} 发布预检缺少父列", relation.name))?;
    ensure!(
        child_type.eq_ignore_ascii_case(&parent_type),
        "外键 {} 发布预检发现列类型不兼容: {}.{}={}，{}.{}={}",
        relation.name,
        relation.child_table,
        relation.child_column,
        child_type,
        relation.parent_table,
        relation.parent_column,
        parent_type
    );
    Ok(())
}

fn supports_enforced_check(version: &str, version_comment: &str) -> bool {
    if version.to_ascii_lowercase().contains("mariadb")
        || version_comment.to_ascii_lowercase().contains("mariadb")
    {
        return false;
    }
    let numeric = version
        .split_once('-')
        .map_or(version, |(numeric, _)| numeric);
    let mut parts = numeric.split('.').map(str::parse::<u64>);
    let Some(Ok(major)) = parts.next() else {
        return false;
    };
    let Some(Ok(minor)) = parts.next() else {
        return false;
    };
    let Some(Ok(patch)) = parts.next() else {
        return false;
    };
    (major, minor, patch) >= (8, 0, 16)
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

#[cfg(test)]
mod tests {
    use super::{descriptors, supports_enforced_check, CompletionDescriptor};

    #[test]
    fn check_preflight_accepts_only_verified_mysql_version_floor() {
        assert!(supports_enforced_check(
            "8.0.16",
            "MySQL Community Server - GPL"
        ));
        assert!(supports_enforced_check(
            "8.4.5",
            "MySQL Community Server - GPL"
        ));
        assert!(!supports_enforced_check(
            "8.0.15",
            "MySQL Community Server - GPL"
        ));
        assert!(!supports_enforced_check("8.0.36-MariaDB", "MariaDB Server"));
        for invalid in ["", "8", "8.0", "not-a-version"] {
            assert!(!supports_enforced_check(invalid, "MySQL"));
        }
    }

    #[test]
    fn bootstrap_key_check_is_the_last_forward_only_migration() {
        let descriptor = descriptors()
            .last()
            .unwrap_or_else(|| panic!("迁移清单不得为空"));
        assert_eq!(
            descriptor.version,
            "20260731_0016_add_admin_bootstrap_key_check"
        );
        assert!(matches!(
            descriptor.completion_check,
            Some(CompletionDescriptor::CheckConstraint(check))
                if check.table == "admin_user"
                    && check.constraint == "chk_admin_user_bootstrap_key"
                    && check.expression
                        == "(bootstrap_key IS NULL) OR (bootstrap_key = 'initial-admin')"
                    && check.enforced
        ));
    }
}
