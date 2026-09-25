//! 管理员等价权限的显式清单与判定（G2）。
//!
//! # 为什么是代码侧清单，而不是从 Catalog 投影
//!
//! 权限目录（决策 D3）只记录「有哪些权限、谁声明的」。而「哪条权限的危害面是管理员级」
//! 是**代码侧的知识**：它取决于 Action 的实现语义（拿到凭据、冒充主体），不是 Catalog
//! 能推出来的。因此这里维护一份显式、可评审、有测试钉住的清单——形式参照
//! `docs/architecture/authorization-writers.md` 的 allowlist：写成代码、逐条附理由、
//! 由测试钉住它与冻结 Catalog 的一致性。
//!
//! # 判据（写死；每条理由都必须落回它）
//!
//! 持有它的人能够**获得或夺取其他主体的凭据/身份**，或能够**绕过其他一切授权检查**。
//!
//! # 为什么「能授予权限」本身不算管理员等价
//!
//! `access.grants.write` / `access.groups.write` 能改动授权事实，但**改不了本清单**：
//! 授予侧的闸门（见下）让它们无论如何都授不出管理员等价权限，因此持它们的人仍然
//! 拿不到任何凭据、也冒充不了任何主体。它们不是管理员等价，而是「危害面被本闸门
//! 封顶的委派权限」——把委派者一并列进来只会把正常运营彻底锁死。
//!
//! 同理 `account.users.manage` 不在清单里：它能停用/启用账号，但停用与启用全权组成员
//! 另有「只有全权组成员能修改全权组成员」守卫（spec §8.1 附加规则）挡着，且它不签发
//! 任何凭据，夺不走任何人的身份。
//!
//! # 清单与运行期的两处落点
//!
//! 1. **目录标记**：`permission_catalog::project_permissions` 对每条已声明权限查本清单，
//!    把结果写进 `PermissionEntry::admin_equivalent`，前端据此把危害面显示出来——
//!    「可配置的前提是每个权限的危害面可见」。
//! 2. **授予闸门**：`groups::admin` 的闸门在读到这里命中时，要求调用者是全权组成员，
//!    否则 403。三条路径分别是直接授予、把该权限加进组、把用户加进持有该权限的组。
//!
//! # 清单不会随权限改名静默失效
//!
//! 本文件单测 `every_listed_permission_is_declared_by_the_catalog` 用**真实冻结 Catalog**
//! 断言清单里每条都已被声明；集成测试 `the_permission_catalog_marks_exactly_the_admin_equivalent_permissions`
//! 再从目录读接口整体比对一次。权限一旦改名，两处都会变红。

/// 一条管理员等价权限：权限字符串 + 为什么它是管理员等价的（中文理由）。
///
/// 理由不是注释而是数据：拒绝授予时它会被拼进错误信息，让调用者读懂「为什么这条权限
/// 不能由我来授」，因此它在生产路径上真的被读取。
pub(crate) struct AdminEquivalentPermission {
    pub(crate) permission: &'static str,
    pub(crate) reason: &'static str,
}

/// 管理员等价权限的显式清单（唯一事实来源）。
pub(crate) const ADMIN_EQUIVALENT_PERMISSIONS: &[AdminEquivalentPermission] = &[
    AdminEquivalentPermission {
        permission: "account.users.reset_credentials",
        reason: "对任意账号签发密码重置凭证：凭此重置其口令并登录成他，即夺取该账号的身份与全部权限",
    },
    AdminEquivalentPermission {
        permission: "feishu.datasource.secret",
        reason: "回显数据源封存的 Token 明文；该 Token 是数据源主体的入站凭据，拿到即可冒充该数据源调用本系统，不再经任何授权检查",
    },
    AdminEquivalentPermission {
        permission: "feishu.datasource.write",
        reason: "创建与轮换数据源都会把新凭据的明文返回给调用者（create_datasource_table / rotate_token），签发即持有，与直接读取凭据等价",
    },
];

/// 查清单里的完整条目（含中文理由）；不是管理员等价时返回 `None`。
pub(crate) fn admin_equivalent_entry(
    permission: &str,
) -> Option<&'static AdminEquivalentPermission> {
    ADMIN_EQUIVALENT_PERMISSIONS
        .iter()
        .find(|entry| entry.permission == permission)
}

/// 该权限是否为管理员等价权限。
pub(crate) fn is_admin_equivalent(permission: &str) -> bool {
    admin_equivalent_entry(permission).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::project_permissions;
    use crate::config::SecuritySettings;
    use jsonwebtoken::Algorithm;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use yang_base::action::StepUpManager;
    use yang_base::token::TokenManager;
    use yang_base::tools::{Tools, ToolsBuilder};
    use yang_db::{Database, DatabaseConfig};

    #[test]
    fn the_known_admin_equivalent_permissions_are_recognized() {
        assert!(is_admin_equivalent("account.users.reset_credentials"));
        assert!(is_admin_equivalent("feishu.datasource.secret"));
        assert!(is_admin_equivalent("feishu.datasource.write"));
    }

    #[test]
    fn delegating_and_ordinary_permissions_are_not_admin_equivalent() {
        // 能与授权事实交互、却授不出管理员等价权限的委派权限，不得被误列——
        // 误列会把正常运营彻底锁死。
        for permission in [
            "access.grants.write",
            "access.groups.write",
            "account.users.manage",
            "demo.notes.read",
            "feishu.datasource.read",
        ] {
            assert!(
                !is_admin_equivalent(permission),
                "{permission} 不是管理员等价权限"
            );
            assert!(admin_equivalent_entry(permission).is_none());
        }
    }

    #[test]
    fn every_entry_carries_a_non_empty_reason() {
        // 理由会拼进 403 的错误信息：空理由等于把「为什么」那一半丢掉。
        assert!(!ADMIN_EQUIVALENT_PERMISSIONS.is_empty());
        for entry in ADMIN_EQUIVALENT_PERMISSIONS {
            assert!(!entry.permission.is_empty(), "清单条目的权限字符串不能为空");
            assert!(
                entry.reason.chars().count() >= 10,
                "{} 的理由必须说清危害面，不能是一句话敷衍",
                entry.permission
            );
        }
    }

    /// 清单里每条都必须是当前 Catalog 已声明的权限。
    ///
    /// 必须用**真实冻结 Catalog** 才有意义：权限改名后清单会静默失效，只有拿真目录
    /// 比对才能把它钉红。这里经组合根构建 schema-only 应用（`connect_lazy` 不发起连接，
    /// 与 `src/app.rs` 的既有单测同例），不依赖任何外部服务。
    #[tokio::test]
    async fn every_listed_permission_is_declared_by_the_catalog() {
        let app = crate::app::build_schema_app(offline_tools(), offline_security())
            .unwrap_or_else(|error| panic!("schema-only 应用应构建成功: {error:#}"));
        let declared: Vec<String> = project_permissions(app.runtime.catalog().addons())
            .into_iter()
            .map(|entry| entry.permission().to_string())
            .collect();
        assert!(
            !declared.is_empty(),
            "冻结 Catalog 必须声明权限，否则本测试无从判定"
        );
        for entry in ADMIN_EQUIVALENT_PERMISSIONS {
            assert!(
                declared
                    .iter()
                    .any(|permission| permission == entry.permission),
                "清单里的 {} 已不在当前 Catalog 声明——权限可能被改名，清单正静默失效",
                entry.permission
            );
        }
    }

    fn offline_tools() -> Arc<Tools> {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool, DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .token(
                    TokenManager::new_symmetric(
                        "01234567890123456789012345678901",
                        Algorithm::HS256,
                        "test".to_string(),
                        "test-api".to_string(),
                        60,
                        120,
                    )
                    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}")),
                )
                .extension(Arc::new(
                    StepUpManager::new(
                        "admin-equivalent-test-secret-0123456789abcdef",
                        "test-step-up",
                        "test-sensitive-actions",
                    )
                    .unwrap_or_else(|error| panic!("测试 Step-up manager 应有效: {error}")),
                ))
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        )
    }

    fn offline_security() -> Arc<SecuritySettings> {
        Arc::new(SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 30,
            auth_rate_limit_username_attempts: 30,
            password_reset_ttl_seconds: 900,
            issue_refresh_credential_version: true,
            trusted_proxy_cidrs: Vec::new(),
            totp: None,
        })
    }
}
