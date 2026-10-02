//! 创建一个权限组。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::resolution::is_reserved_group_key;
use crate::addon::access::groups::table::{GROUP_KEY_MAX_LENGTH, GROUP_KEY_PATTERN};
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) CreateGroupInput {
        group_key: Str::new()
            .title("组标识")
            .require(true)
            .max_length(GROUP_KEY_MAX_LENGTH)
            .pattern(GROUP_KEY_PATTERN),
        title: Str::new()
            .title("展示名")
            .require(true)
            .min_length(1)
            .max_length(128),
        description: Str::new()
            .title("描述")
            .require(false)
            .max_length(255),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct CreateGroupResult {
    id: i64,
    group_key: String,
    title: String,
    description: Option<String>,
}

/// 拒绝解析期保留的 `group_key`。
///
/// 保留 key 的语义在解析期被特殊解释（`resolution::resolve_group_permissions` 对
/// `system_admin` 直接返回整个权限目录），因此它们必须是**创建路径上的硬拒绝**，
/// 而不是靠别处的守卫兜住。见 [`crate::addon::access::domain::groups::resolution::RESERVED_GROUP_KEYS`]。
///
/// 唯一允许产生全权组的路径是首账号引导（`ensure_system_admin_group_in_tx` 在引导事务内
/// 幂等创建），那条路径不经本 Action。
fn reject_reserved_group_key(group_key: &str) -> Result<(), BaseError> {
    if is_reserved_group_key(group_key) {
        return Err(BaseError::ParamInvalid(
            "group_key".to_string(),
            format!(
                "组标识「{group_key}」为解析期保留值（内置全权组的身份依据），不可通过接口创建"
            ),
        ));
    }
    Ok(())
}

/// 框架缺陷的临时折算：`TableQuery::insert_returning_id_in_tx`（yang-base
/// `src/table/table_query/write.rs:146`）用 `.map_err(BaseError::DatabaseExecuteFailed)`
/// 直接构造错误，绕过 `From<DbError>` 的「唯一键冲突→ParamInvalid」特判（该特判在
/// yang-base `src/error/mod.rs` 的 From 实现里，对所有 `?` 传播路径生效）。
/// 因此 AUTHZ_GRANTS.md §错误码表承诺的「group_key 撞唯一键→400」在本路径落不到，
/// 实测泄漏成 DatabaseExecuteFailed→500。这里按报文点名冲突索引
/// `uk_permission_group_key` 识别并折算成既有的参数错误语义；框架侧修好
/// （write.rs 改用 `?` 传播）后本折算可整体删除。
///
/// 只认「撞 `uk_permission_group_key` 的唯一键冲突」：外键/非空/CHECK 等其它约束
/// 错误必须原样放行——那是真实数据故障，不该伪装成输入错误。
fn fold_duplicate_group_key(error: BaseError) -> BaseError {
    if let BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(message)) = &error {
        if message.contains("Duplicate entry") && message.contains("uk_permission_group_key") {
            return BaseError::ParamInvalid(
                "group_key".to_string(),
                "该组标识已被占用".to_string(),
            );
        }
    }
    error
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateGroupInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    // 保留 key 先于事务拒绝：它是纯输入判定，不需要任何库事实，也就不会为一次注定
    // 被拒的请求开事务（更不会留下任何写入的中间态）。
    reject_reserved_group_key(&input.group_key)?;
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 组本身不是权限：建组既不需要过目录校验，也不改变任何人的有效权限，
        // 因此这里没有扇出失效。`group_key` 撞唯一键由 `fold_duplicate_group_key`
        // 折算成既有的参数错误语义（400），原样上抛。
        let group_id = access
            .groups()
            .insert_group_in_tx(
                &ctx,
                &mut transaction,
                &input.group_key,
                &input.title,
                input.description.as_deref(),
                operator_id,
            )
            .await
            .map_err(fold_duplicate_group_key)?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group_id)?,
            None,
            Some(audit::summary([("group_key", json!(input.group_key))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(group_id)
    }
    .await;
    let group_id = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        CreateGroupResult {
            id: group_id,
            group_key: input.group_key,
            title: input.title,
            description: input.description,
        },
        "权限组已创建",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_group"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups")
        .display_name("创建权限组")
        .description("创建一个权限组")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
    use crate::addon::access::domain::groups::resolution::RESERVED_GROUP_KEYS;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_key_and_display_contract() {
        let injected = serde_json::from_value::<CreateGroupInput>(serde_json::json!({
            "group_key": "ops",
            "title": "运维",
            "created_by": 7
        }));
        assert!(injected.is_err(), "客户端不能注入 created_by 等内部字段");

        let params = <CreateGroupInput as ParamInput>::params();
        let param = |name: &str| {
            params
                .as_slice()
                .iter()
                .find(|param| param.name.as_str() == name)
                .unwrap_or_else(|| panic!("应声明 {name} 参数"))
        };
        let group_key = param("group_key");
        assert!(group_key.required);
        assert_eq!(
            group_key.validation.pattern.as_deref(),
            Some(GROUP_KEY_PATTERN)
        );
        assert_eq!(group_key.validation.max_length, Some(GROUP_KEY_MAX_LENGTH));
        // 展示字段的上限必须与表声明同宽：比列窄只是提前拒绝，比列宽会被数据库
        // 以「数据过长」打回成 500。
        assert_eq!(param("title").validation.max_length, Some(128));
        assert_eq!(param("description").validation.max_length, Some(255));
        assert!(
            !param("description").required,
            "描述是可选字段，缺省必须表示成 None 而不是空串"
        );
    }

    /// 保留 key 集合的「完整性 + 拒绝力」双钉：集合非空、成员清单与预期一致、且**每个**
    /// 成员都真的被建组路径拒绝。
    ///
    /// 拆开看的三条断言各挡一种回归：
    /// 1. 集合为空 —— 创建期防线整体消失（保留 key 全线放行）；
    /// 2. 成员清单不符 —— 有人从集合里删掉一项（防线静默打开）或加了语义不明的
    ///    key。期望值刻意在测试侧**独立复述**，不复用生产常量本身；
    /// 3. 逐个拒绝 —— 集合只是数据，真正拦人的是 [`reject_reserved_group_key`]；
    ///    两者一旦脱钩（比如改判据忘了改调用），这条会红。
    #[test]
    fn every_reserved_group_key_is_rejected_by_the_create_path() {
        // 期望值独立复述：目前解析期只有内置全权组一个特殊语义 key。
        const EXPECTED_RESERVED_GROUP_KEYS: [&str; 1] = [SYSTEM_ADMIN_GROUP_KEY];

        assert!(
            !RESERVED_GROUP_KEYS.is_empty(),
            "保留 key 集合为空等于建组路径全线放行"
        );
        let mut actual: Vec<&str> = RESERVED_GROUP_KEYS.to_vec();
        actual.sort_unstable();
        let mut expected: Vec<&str> = EXPECTED_RESERVED_GROUP_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "保留 key 集合必须与独立的期望值一致：少一项说明创建期防线被静默打开"
        );

        for key in RESERVED_GROUP_KEYS {
            let error = match reject_reserved_group_key(key) {
                Ok(()) => panic!("保留 key {key} 必须被建组路径拒绝，实际放行"),
                Err(error) => error,
            };
            assert!(
                matches!(&error, BaseError::ParamInvalid(field, _) if field.as_str() == "group_key"),
                "保留 key 的拒绝必须是 ParamInvalid(\"group_key\")→400，实际 {error:?}"
            );
            assert!(
                error.to_string().contains("保留"),
                "拒绝信息必须说明该 key 被保留，实际 {error}"
            );
        }

        // 反向对照：普通 key、以及「以前缀相同但不等」的 key 都必须放行，避免拒绝面
        // 过宽把正常建组一并锁死。
        assert!(reject_reserved_group_key("ops").is_ok());
        assert!(
            reject_reserved_group_key("system_admin_ops").is_ok(),
            "判据必须是完整相等，不能退化成前缀匹配"
        );
    }

    /// 框架缺陷的临时折算（根因：yang-base `write.rs` 的 `insert_returning_id_in_tx`
    /// 用 `.map_err(BaseError::DatabaseExecuteFailed)` 直包，绕过 `From<DbError>` 的
    /// 唯一键特判）钉住应用侧兜底：撞 `uk_permission_group_key` 的唯一键冲突必须
    /// 折算成 `ParamInvalid("group_key")`→400；外键/非空等其它约束错误必须原样放行。
    #[test]
    fn duplicate_group_key_folds_to_param_invalid_but_other_constraints_pass_through() {
        let duplicate = fold_duplicate_group_key(BaseError::DatabaseExecuteFailed(
            yang_db::DbError::ConstraintError(
                "Duplicate entry 'ops' for key 'permission_group.uk_permission_group_key'"
                    .to_string(),
            ),
        ));
        match duplicate {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "group_key", "必须点名 group_key");
                assert_eq!(message, "该组标识已被占用");
            }
            other => panic!("唯一键冲突必须折算成 ParamInvalid，实际 {other:?}"),
        }

        for message in [
            "Cannot add or update a child row: a foreign key constraint fails",
            "Column 'title' cannot be null",
        ] {
            let passthrough = fold_duplicate_group_key(BaseError::DatabaseExecuteFailed(
                yang_db::DbError::ConstraintError(message.to_string()),
            ));
            assert!(
                matches!(
                    passthrough,
                    BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))
                ),
                "其它约束错误必须原样放行（保持 500 语义），实际 {passthrough:?}"
            );
        }
    }
}
