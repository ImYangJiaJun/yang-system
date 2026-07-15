use super::dto::AccountView;
use super::entity::AccountRow;
use super::password;
use super::repository::AccountRepository;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::BaseError;

#[derive(Clone)]
pub(super) struct AccountService {
    repository: AccountRepository,
    security: Arc<SecuritySettings>,
}

impl AccountService {
    pub(super) fn new(repository: AccountRepository, security: Arc<SecuritySettings>) -> Self {
        Self {
            repository,
            security,
        }
    }

    pub(super) async fn register(
        &self,
        username: &str,
        plain_password: &str,
    ) -> Result<AccountView, BaseError> {
        let username = self.normalize_username(username)?;
        self.validate_password(plain_password)?;
        if self.repository.find_by_username(&username).await?.is_some() {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "用户名已存在".to_string(),
            ));
        }
        let password_hash = password::hash(plain_password)?;
        let account = self.repository.insert(&username, &password_hash).await?;
        Ok(AccountView::from(&account))
    }

    pub(super) async fn authenticate(
        &self,
        username: &str,
        plain_password: &str,
    ) -> Result<AccountRow, BaseError> {
        let username = self.normalize_username(username)?;
        let account = self
            .repository
            .find_by_username(&username)
            .await?
            .ok_or(BaseError::InvalidPassword)?;
        if !password::verify(plain_password, &account.password_hash)? {
            return Err(BaseError::InvalidPassword);
        }
        self.ensure_active(account)
    }

    pub(super) async fn active_by_subject(&self, subject: &str) -> Result<AccountRow, BaseError> {
        let id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let account = self
            .repository
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        self.ensure_active(account)
    }

    pub(super) async fn view_by_id(&self, id: i64) -> Result<AccountView, BaseError> {
        let account = self
            .repository
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        let account = self.ensure_active(account)?;
        Ok(AccountView::from(&account))
    }

    fn ensure_active(&self, account: AccountRow) -> Result<AccountRow, BaseError> {
        if account.status != "active" {
            return Err(BaseError::Unauthorized("账号已停用".to_string()));
        }
        Ok(account)
    }

    fn normalize_username(&self, username: &str) -> Result<String, BaseError> {
        let normalized = username.trim().to_ascii_lowercase();
        let length = normalized.len();
        if length < self.security.username_min_length || length > self.security.username_max_length
        {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                format!(
                    "长度必须在 {}..={} 之间",
                    self.security.username_min_length, self.security.username_max_length
                ),
            ));
        }
        if !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "只允许 ASCII 字母、数字、下划线和连字符".to_string(),
            ));
        }
        Ok(normalized)
    }

    fn validate_password(&self, password: &str) -> Result<(), BaseError> {
        let length = password.chars().count();
        if length < self.security.password_min_length || length > self.security.password_max_length
        {
            return Err(BaseError::ParamInvalid(
                "password".to_string(),
                format!(
                    "长度必须在 {}..={} 之间",
                    self.security.password_min_length, self.security.password_max_length
                ),
            ));
        }
        Ok(())
    }
}

impl From<&AccountRow> for AccountView {
    fn from(account: &AccountRow) -> Self {
        Self {
            id: account.id,
            username: account.username.clone(),
            status: account.status.clone(),
            created_at: account.created_at,
            updated_at: account.updated_at,
        }
    }
}
