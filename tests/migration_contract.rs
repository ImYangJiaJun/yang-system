use std::path::Path;
use yang_base::database::MigrationPlanStatus;
use yang_system::migrations::{
    descriptors, manifest, parse_cli, MigrationCommand, ReleaseStage, PRODUCTION_RELEASE_ORDER,
};

#[test]
fn manifest_and_operational_metadata_are_ordered_and_one_to_one() {
    let manifest = manifest().unwrap_or_else(|error| panic!("迁移清单应合法: {error}"));
    let descriptors = descriptors();

    assert_eq!(manifest.module(), "yang-system");
    assert_eq!(manifest.migrations().len(), 16);
    assert_eq!(manifest.migrations().len(), descriptors.len());
    for (migration, descriptor) in manifest.migrations().iter().zip(descriptors) {
        assert_eq!(migration.version(), descriptor.version());
        assert!(!descriptor.description().trim().is_empty());
        assert!(!descriptor.prerequisite().trim().is_empty());
        assert!(!descriptor.recovery().trim().is_empty());
        let idempotent_create = migration.sql().starts_with("CREATE TABLE IF NOT EXISTS");
        assert!(
            idempotent_create || migration.completion_check().is_some(),
            "{} 必须可重入或声明精确完成探针",
            migration.version()
        );
    }
    let authz_version = manifest
        .migrations()
        .get(4)
        .unwrap_or_else(|| panic!("应存在授权版本迁移"));
    assert_eq!(
        authz_version.version(),
        "20260726_0005_add_user_authz_version"
    );
    assert!(authz_version.completion_check().is_some());
    let authorization_outbox = manifest
        .migrations()
        .get(5)
        .unwrap_or_else(|| panic!("应存在授权事务 Outbox 迁移"));
    assert_eq!(
        authorization_outbox.version(),
        "20260726_0006_create_authorization_outbox"
    );
    assert!(
        authorization_outbox.sql().contains(
            "UNIQUE KEY `uk_authorization_outbox_user_version` (`user_id`, `authz_version`)"
        ),
        "Outbox 必须以用户与授权版本作为幂等键"
    );
    assert!(
        authorization_outbox
            .sql()
            .contains("KEY `idx_authorization_outbox_dispatch` (`state`, `available_at`, `id`)"),
        "Outbox 必须支持按状态、可用时间与稳定 ID 批量 claim"
    );
    let audit_event = manifest
        .migrations()
        .get(6)
        .unwrap_or_else(|| panic!("应存在高权限审计迁移"));
    assert_eq!(audit_event.version(), "20260726_0007_create_audit_event");
    for required in [
        "UNIQUE KEY `uk_audit_event_event_id` (`event_id`)",
        "KEY `idx_audit_event_actor` (`actor_type`, `actor_id`, `occurred_at`, `id`)",
        "KEY `idx_audit_event_subject` (`subject_type`, `subject_id`, `occurred_at`, `id`)",
        "KEY `idx_audit_event_target` (`target_type`, `target_id`, `occurred_at`, `id`)",
        "KEY `idx_audit_event_tenant` (`tenant_id`, `occurred_at`, `id`)",
        "KEY `idx_audit_event_request` (`request_id`, `id`)",
        "KEY `idx_audit_event_retention` (`occurred_at`, `id`)",
        "CONSTRAINT `chk_audit_event_subject_pair`",
    ] {
        assert!(
            audit_event.sql().contains(required),
            "审计迁移缺少不可变事件检索或约束契约: {required}"
        );
    }
    let work_project = manifest
        .migrations()
        .get(7)
        .unwrap_or_else(|| panic!("应存在个人项目迁移"));
    assert_eq!(work_project.version(), "20260731_0008_create_work_project");
    for required in [
        "UNIQUE KEY `uk_work_project_owner_name` (`owner_user`, `name`)",
        "UNIQUE KEY `uk_work_project_id_owner` (`id`, `owner_user`)",
        "CONSTRAINT `fk_work_project_owner`",
    ] {
        assert!(
            work_project.sql().contains(required),
            "项目迁移缺少租户唯一性或外键契约: {required}"
        );
    }
    let work_task = manifest
        .migrations()
        .get(8)
        .unwrap_or_else(|| panic!("应存在任务树迁移"));
    assert_eq!(work_task.version(), "20260731_0009_create_work_task");
    for required in [
        "KEY `idx_work_task_owner_project_status` (`owner_user`, `project_project`, `status`)",
        "CONSTRAINT `fk_work_task_project_owner`",
        "CONSTRAINT `fk_work_task_parent_project_owner`",
    ] {
        assert!(
            work_task.sql().contains(required),
            "任务迁移缺少规模索引或同租户关系约束: {required}"
        );
    }
    let credential_version = manifest
        .migrations()
        .get(9)
        .unwrap_or_else(|| panic!("应存在凭据版本迁移"));
    assert_eq!(
        credential_version.version(),
        "20260731_0010_add_user_credential_version"
    );
    assert!(credential_version.completion_check().is_some());
    assert!(credential_version
        .sql()
        .contains("ADD COLUMN `credential_version` BIGINT NOT NULL DEFAULT 0"));
    let password_reset = manifest
        .migrations()
        .get(10)
        .unwrap_or_else(|| panic!("应存在密码重置凭证迁移"));
    assert_eq!(
        password_reset.version(),
        "20260731_0011_create_password_reset_token"
    );
    for required in [
        "UNIQUE KEY `uk_password_reset_token_digest` (`token_digest`)",
        "KEY `idx_password_reset_token_user_active` (`user_user`, `consumed_at`, `invalidated_at`, `expires_at`, `id`)",
        "CONSTRAINT `fk_password_reset_token_user`",
        "CONSTRAINT `fk_password_reset_token_requested_by`",
    ] {
        assert!(
            password_reset.sql().contains(required),
            "密码重置迁移缺少单次消费、清理索引或外键契约: {required}"
        );
    }
    let user_status_check = manifest
        .migrations()
        .get(11)
        .unwrap_or_else(|| panic!("应存在用户状态约束迁移"));
    assert_eq!(
        user_status_check.version(),
        "20260731_0012_add_users_status_check"
    );
    assert_eq!(
        user_status_check.sql().trim(),
        "ALTER TABLE `users` ADD CONSTRAINT `chk_users_status` CHECK (`status` IN ('active', 'disabled'))"
    );
    assert!(user_status_check.completion_check().is_some());

    for (index, version, constraint, column, parent) in [
        (
            12,
            "20260731_0013_add_admin_user_user_fk",
            "fk_admin_user_user_user",
            "user_user",
            "users",
        ),
        (
            13,
            "20260731_0014_add_org_user_user_fk",
            "fk_org_user_user_user",
            "user_user",
            "users",
        ),
        (
            14,
            "20260731_0015_add_org_user_org_fk",
            "fk_org_user_org_org",
            "org_org",
            "org_org",
        ),
    ] {
        let migration = manifest
            .migrations()
            .get(index)
            .unwrap_or_else(|| panic!("应存在授权关系外键迁移 {version}"));
        assert_eq!(migration.version(), version);
        for required in [
            format!("CONSTRAINT `{constraint}`"),
            format!("FOREIGN KEY (`{column}`)"),
            format!("REFERENCES `{parent}` (`id`)"),
            "ON UPDATE RESTRICT ON DELETE RESTRICT".to_string(),
        ] {
            assert!(
                migration.sql().contains(&required),
                "外键迁移 {version} 缺少契约: {required}"
            );
        }
        assert!(migration.completion_check().is_some());
    }

    let bootstrap_key_check = manifest
        .migrations()
        .get(15)
        .unwrap_or_else(|| panic!("应存在平台初始化占位约束迁移"));
    assert_eq!(
        bootstrap_key_check.version(),
        "20260731_0016_add_admin_bootstrap_key_check"
    );
    assert_eq!(
        bootstrap_key_check.sql().trim(),
        "ALTER TABLE `admin_user` ADD CONSTRAINT `chk_admin_user_bootstrap_key` CHECK (`bootstrap_key` IS NULL OR `bootstrap_key` = 'initial-admin')"
    );
    assert!(bootstrap_key_check.completion_check().is_some());
}

#[test]
fn production_release_order_is_migrate_validate_ready() {
    assert_eq!(
        PRODUCTION_RELEASE_ORDER,
        [
            ReleaseStage::Migrate,
            ReleaseStage::Validate,
            ReleaseStage::Ready
        ]
    );
}

#[test]
fn cli_requires_explicit_command_and_supports_config_path() {
    let plan = parse_cli(["plan".to_string()])
        .unwrap_or_else(|error| panic!("plan 应可解析: {error}"))
        .unwrap_or_else(|| panic!("plan 不应被解析为 help"));
    assert_eq!(plan.command(), MigrationCommand::Plan);
    assert_eq!(plan.config_path(), Path::new("config.toml"));

    let apply = parse_cli([
        "apply".to_string(),
        "--config".to_string(),
        "deploy.toml".to_string(),
    ])
    .unwrap_or_else(|error| panic!("apply 应可解析: {error}"))
    .unwrap_or_else(|| panic!("apply 不应被解析为 help"));
    assert_eq!(apply.command(), MigrationCommand::Apply);
    assert_eq!(apply.config_path(), Path::new("deploy.toml"));

    assert!(parse_cli(Vec::<String>::new()).is_err());
    assert!(parse_cli(["apply".to_string(), "--unknown".to_string()]).is_err());
    assert!(parse_cli(["plan".to_string(), "--config".to_string()]).is_err());
    assert!(parse_cli(["--help".to_string()])
        .unwrap_or_else(|error| panic!("help 应可解析: {error}"))
        .is_none());
}

#[test]
fn migration_status_contract_remains_fail_closed() {
    assert_ne!(
        MigrationPlanStatus::ChecksumMismatch,
        MigrationPlanStatus::Applied
    );
    assert_ne!(
        MigrationPlanStatus::InProgress,
        MigrationPlanStatus::Applied
    );
}
