//! 工作台用户查找：按用户名或邮箱包含匹配分页（authenticated-only）。

use crate::addon::account::domain::status::UserStatus;
use crate::addon::account::user::table::{EMAIL, STATUS, USERNAME, USER_ID};
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec, Str};
use yang_base::table::Record;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) LookupUsersInput {
        #[param(source = query)]
        q: Str::new()
            .title("关键词")
            .require(false),
        #[param(source = query)]
        page: Int::new()
            .title("页码")
            .require(false)
            .default(1_i64),
        #[param(source = query)]
        page_size: Int::new()
            .title("每页条数")
            .require(false)
            .default(20_i64),
    }
}

/// 查找结果里的一个用户：响应契约只暴露这四个字段，绝不包含
/// password_hash/totp/版本字段（投影来源 `USER_VIEW_FIELDS`）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct LookupUser {
    id: i64,
    username: String,
    email: Option<String>,
    status: UserStatus,
}

impl TryFrom<&Record> for LookupUser {
    type Error = BaseError;

    fn try_from(user: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: user.require(USER_ID)?,
            username: user.require(USERNAME)?,
            email: user.optional(EMAIL)?,
            status: UserStatus::from_storage(&user.require::<String>(STATUS)?)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct LookupUsersResult {
    users: Vec<LookupUser>,
}

/// 校验并转换分页参数：`page_size` 上限 50（防 DoS，超限 ParamInvalid）。
fn resolve_page(input: &LookupUsersInput) -> Result<(usize, usize), BaseError> {
    let page = usize::try_from(input.page.unwrap_or(1))
        .map_err(|_| BaseError::ParamInvalid("page".to_string(), "页码无效".to_string()))?;
    let page_size = usize::try_from(input.page_size.unwrap_or(20)).map_err(|_| {
        BaseError::ParamInvalid("page_size".to_string(), "每页条数无效".to_string())
    })?;
    if page_size > 50 {
        return Err(BaseError::ParamInvalid(
            "page_size".to_string(),
            "每页条数不能超过 50".to_string(),
        ));
    }
    Ok((page, page_size))
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: LookupUsersInput,
    account: Arc<Account>,
) -> Result<LookupUsersResult, BaseError> {
    let (page, page_size) = resolve_page(&input)?;
    let records = account
        .users()
        .search(&ctx, input.q.as_deref(), page, page_size)
        .await?;
    let users = records
        .iter()
        .map(LookupUser::try_from)
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(LookupUsersResult { users })
}

/// 自包含注册：路由/认证声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // auth: authenticated-only 工作台查找——不声明 permissions（工作台访问者
    // 不一定持有 account.users.read），登录态即是唯一门槛。
    module
        .action_fn(yang_base::action_name!("lookup"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Get, "/api/v1/users/lookup")
        .display_name("用户查找")
        .description("按用户名或邮箱包含匹配分页查找用户（登录用户可用）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_has_keyword_and_bounded_pagination_from_query_only() {
        let params = <LookupUsersInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["q", "page", "page_size"]);
        // GET 无请求体：全部参数必须声明为 query 来源，否则 `?q=...`
        // 会被静默忽略（回归守护）。
        assert!(params
            .as_slice()
            .iter()
            .all(|param| param.source == yang_base::definition::ParamSource::Query));
        let injected = serde_json::from_value::<LookupUsersInput>(serde_json::json!({
            "user_id": 99
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn page_size_cap_rejects_over_50_and_applies_defaults() {
        let defaults = LookupUsersInput {
            q: None,
            page: None,
            page_size: None,
        };
        assert_eq!(
            resolve_page(&defaults).unwrap_or_else(|error| panic!("默认分页应有效: {error}")),
            (1, 20)
        );
        let capped = LookupUsersInput {
            q: None,
            page: None,
            page_size: Some(50),
        };
        assert_eq!(
            resolve_page(&capped).unwrap_or_else(|error| panic!("50 应在上限内: {error}")),
            (1, 50)
        );
        for over in [51, 100, i64::MAX] {
            let input = LookupUsersInput {
                q: None,
                page: None,
                page_size: Some(over),
            };
            assert!(matches!(
                resolve_page(&input),
                Err(BaseError::ParamInvalid(field, _)) if field == "page_size"
            ));
        }
    }

    #[test]
    fn lookup_user_projection_exposes_only_contract_fields() {
        let record = Record::new()
            .set(USER_ID, 7)
            .set(USERNAME, "alice")
            .set(EMAIL, "alice@example.com")
            .set(
                crate::addon::account::user::table::PASSWORD_HASH,
                "secret-hash",
            )
            .set(
                crate::addon::account::user::table::TOTP_SECRET,
                "aead-ciphertext",
            )
            .set(crate::addon::account::user::table::AUTHZ_VERSION, 2_i64)
            .set(
                crate::addon::account::user::table::CREDENTIAL_VERSION,
                3_i64,
            )
            .set(crate::addon::account::user::table::STATUS, "active")
            .set(crate::addon::account::user::table::CREATED_AT, 10)
            .set(crate::addon::account::user::table::UPDATED_AT, 11);
        let user = LookupUser::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为查找视图: {error}"));
        let value = serde_json::to_value(user)
            .unwrap_or_else(|error| panic!("查找视图应可序列化: {error}"));
        let keys: Vec<&str> = value
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(keys, ["email", "id", "status", "username"]);
        assert!(value.get("password_hash").is_none());
        assert!(value.get("totp_secret").is_none());
        assert!(value.get("authz_version").is_none());
        assert!(value.get("credential_version").is_none());
        assert!(value.get("created_at").is_none());
        assert!(value.get("updated_at").is_none());
    }

    #[test]
    fn lookup_user_projects_null_email_and_storage_status() {
        let record = Record::new()
            .set(USER_ID, 8)
            .set(USERNAME, "ghost")
            .set(crate::addon::account::user::table::STATUS, "deleted");
        let user = LookupUser::try_from(&record)
            .unwrap_or_else(|error| panic!("匿名化用户应可转换为查找视图: {error}"));
        let value = serde_json::to_value(user)
            .unwrap_or_else(|error| panic!("查找视图应可序列化: {error}"));
        assert_eq!(value["id"], serde_json::json!(8));
        assert_eq!(value["username"], serde_json::json!("ghost"));
        assert_eq!(value["email"], serde_json::Value::Null);
        assert_eq!(value["status"], serde_json::json!("deleted"));
    }
}
