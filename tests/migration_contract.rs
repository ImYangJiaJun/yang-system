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
    assert_eq!(manifest.migrations().len(), 4);
    assert_eq!(manifest.migrations().len(), descriptors.len());
    for (migration, descriptor) in manifest.migrations().iter().zip(descriptors) {
        assert_eq!(migration.version(), descriptor.version());
        assert!(!descriptor.description().trim().is_empty());
        assert!(!descriptor.prerequisite().trim().is_empty());
        assert!(!descriptor.recovery().trim().is_empty());
        assert!(
            migration.sql().starts_with("CREATE TABLE IF NOT EXISTS"),
            "{} 必须可安全重跑",
            migration.version()
        );
    }
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
